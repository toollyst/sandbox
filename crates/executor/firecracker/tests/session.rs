//! `sbx-executor-firecracker` 세션 테스트. 진짜 firecracker 대신 같은 명령줄을 받는 셸 스크립트를 쓴다.
//! 스크립트는 `--config-file`의 `uds_path`에 파일을 만들고(`FAKE_FC_MODE`에 따라) 기다리거나 끝난다.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use sbx_executor_proto::{
    Capabilities, DiskRole, DiskSpec, Event, Request, ShareSpec, VERSION, VmSpec, read_line,
    write_line,
};

const FAKE: &str = r#"#!/bin/sh
# 불린 인자를 한 줄씩 남긴다(어댑터가 upstream이 모르는 인자를 주지 않는지 본다).
echo "$*" >> "$FAKE_FC_DIR/args"
cfg=""
while [ $# -gt 0 ]; do
  case "$1" in --config-file) cfg="$2"; shift;; esac
  shift
done
echo $$ > "$FAKE_FC_DIR/pid"
cp "$cfg" "$FAKE_FC_DIR/config.json"
echo "fake firecracker console"
if [ "$FAKE_FC_MODE" = fail ]; then echo boom >&2; exit 1; fi
uds=$(sed -n 's/.*"uds_path":"\([^"]*\)".*/\1/p' "$cfg")
: > "$uds"
if [ "$FAKE_FC_MODE" = poweroff ]; then sleep 0.3; exit 0; fi
exec sleep 600
"#;

static N: AtomicUsize = AtomicUsize::new(0);

struct Env {
    dir: PathBuf,
    socket: PathBuf,
}

impl Env {
    fn new() -> Self {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(format!("/tmp/sbx-fc-t{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fc = dir.join("firecracker");
        std::fs::write(&fc, FAKE).unwrap();
        std::fs::set_permissions(&fc, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("kernel"), b"k").unwrap();
        let socket = dir.join("c.sock");
        Self { dir, socket }
    }

    fn cmd(&self, mode: &str, arg: &str) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_sbx-executor-firecracker"));
        c.arg(arg)
            .env("SBX_FIRECRACKER", self.dir.join("firecracker"))
            .env("SBX_FIRECRACKER_KERNEL", self.dir.join("kernel"))
            .env("FAKE_FC_DIR", &self.dir)
            .env("FAKE_FC_MODE", mode);
        c
    }

    fn vm(&self) -> VmSpec {
        VmSpec {
            guest_init: self.dir.join("initramfs"),
            kernel: None,
            disks: vec![DiskSpec {
                path: self.dir.join("rootfs"),
                role: DiskRole::Rootfs,
                readonly: true,
            }],
            vcpus: 1,
            memory_mib: 128,
            shares: vec![],
            control_socket: self.socket.clone(),
            hostname: "h".into(),
        }
    }

    /// `run`을 띄우고 `Launch`를 보낸 뒤 첫 사건을 돌려준다.
    fn launch(&self, mode: &str) -> (Child, ChildStdin, BufReader<ChildStdout>, Event) {
        // 남은 소켓 파일은 지우고 시작해야 한다.
        std::fs::write(&self.socket, b"stale").unwrap();
        let mut child = self
            .cmd(mode, "run")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        write_line(
            &mut stdin,
            &Request::Launch {
                version: VERSION,
                vm: self.vm(),
            },
        )
        .unwrap();
        let first = next(&mut stdout);
        (child, stdin, stdout, first)
    }

    fn fake_pid(&self) -> String {
        std::fs::read_to_string(self.dir.join("pid"))
            .unwrap()
            .trim()
            .to_owned()
    }

    /// 가짜 firecracker가 끝날 때까지 기다린다.
    fn assert_fake_gone(&self) {
        let pid = self.fake_pid();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "firecracker {pid} is still running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn next(r: &mut impl BufRead) -> Event {
    read_line(r).unwrap().expect("an event")
}

#[test]
fn capabilities_need_firecracker_and_a_kernel() {
    let e = Env::new();
    let out = e.cmd("", "capabilities").output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let caps: Capabilities = read_line(&mut out.stdout.as_slice()).unwrap().unwrap();
    assert_eq!((caps.version, caps.name.as_str()), (VERSION, "firecracker"));
    assert_eq!(caps.rootfs_formats, vec!["erofs"]);
    assert!(!caps.live_share);

    std::fs::remove_file(e.dir.join("kernel")).unwrap();
    let out = e.cmd("", "capabilities").output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("SBX_FIRECRACKER_KERNEL"));
}

#[test]
fn kill_ends_the_vm() {
    let e = Env::new();
    let (mut child, mut stdin, mut stdout, first) = e.launch("wait");
    assert!(
        matches!(first, Event::Started { pid: Some(_) }),
        "{first:?}"
    );
    // 설정 파일은 Firecracker 형식이다.
    let cfg: serde_json::Value =
        serde_json::from_slice(&std::fs::read(e.dir.join("config.json")).unwrap()).unwrap();
    assert_eq!(cfg["vsock"]["uds_path"], e.socket.to_str().unwrap());
    assert_eq!(cfg["drives"][0]["is_read_only"], true);
    write_line(&mut stdin, &Request::Kill {}).unwrap();
    assert_eq!(
        next(&mut stdout),
        Event::Exited {
            reason: "killed".into()
        }
    );
    assert!(child.wait().unwrap().success());
    e.assert_fake_gone();
    assert!(!e.socket.exists());
    // firecracker의 출력은 표준 오류(실행기 로그)로 간다.
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert!(err.contains("fake firecracker console"), "{err}");
}

#[test]
fn closing_input_or_killing_the_executor_ends_the_vm() {
    let e = Env::new();
    let (mut child, stdin, _out, first) = e.launch("wait");
    assert!(matches!(first, Event::Started { .. }));
    drop(stdin);
    assert!(child.wait().unwrap().success());
    e.assert_fake_gone();

    // 실행기 프로세스가 강제로 죽어도 VM은 남지 않는다.
    let e = Env::new();
    let (mut child, _stdin, _out, first) = e.launch("wait");
    assert!(matches!(first, Event::Started { .. }));
    child.kill().unwrap();
    let _ = child.wait();
    e.assert_fake_gone();
}

/// 감시 프로세스(`vmm` 모드, `Started`의 pid)만 SIGKILL로 죽어도 firecracker는 남지 않고, 세션은 끝을 알린다.
#[test]
fn a_killed_monitor_does_not_leave_firecracker_behind() {
    let e = Env::new();
    let (mut child, _stdin, mut stdout, first) = e.launch("wait");
    let Event::Started { pid: Some(monitor) } = first else {
        panic!("{first:?}");
    };
    assert!(
        Command::new("kill")
            .args(["-9", &monitor.to_string()])
            .status()
            .unwrap()
            .success()
    );
    match next(&mut stdout) {
        Event::Exited { reason } => assert!(reason.contains("firecracker exited"), "{reason}"),
        other => panic!("{other:?}"),
    }
    e.assert_fake_gone();
    assert!(child.wait().unwrap().success());
}

#[test]
fn guest_power_off_and_early_failure() {
    let e = Env::new();
    let (mut child, _stdin, mut stdout, first) = e.launch("poweroff");
    assert!(matches!(first, Event::Started { .. }));
    assert_eq!(
        next(&mut stdout),
        Event::Exited {
            reason: "guest powered off".into()
        }
    );
    assert!(child.wait().unwrap().success());

    let e = Env::new();
    let (mut child, _stdin, _out, first) = e.launch("fail");
    match first {
        Event::Error { message } => assert!(message.contains("exited before"), "{message}"),
        other => panic!("{other:?}"),
    }
    assert!(!child.wait().unwrap().success());
}

#[test]
fn wrong_version_is_refused() {
    let e = Env::new();
    let mut child = e
        .cmd("wait", "run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    write_line(
        &mut stdin,
        &Request::Launch {
            version: VERSION + 1,
            vm: e.vm(),
        },
    )
    .unwrap();
    stdin.flush().unwrap();
    let ev = next(&mut BufReader::new(child.stdout.take().unwrap()));
    assert!(matches!(ev, Event::Error { .. }), "{ev:?}");
    assert!(!child.wait().unwrap().success());
}

/// firecracker는 VM을 띄울 때 한 번, upstream이 아는 인자(`--no-api --config-file <파일>`)로만 불린다. `capabilities`는
/// 부르지 않는다. 공유 폴더는 알리지 않으므로(`live_share: false`), 공유가 오면 firecracker를 띄우지 않고 `Error`다.
#[test]
fn firecracker_sees_only_upstream_arguments() {
    let e = Env::new();
    let args = e.dir.join("args");
    let out = e.cmd("", "capabilities").output().unwrap();
    assert!(out.status.success());
    let caps: Capabilities = read_line(&mut out.stdout.as_slice()).unwrap().unwrap();
    assert!(!caps.live_share);
    assert!(!args.exists(), "capabilities ran firecracker");

    let mut vm = e.vm();
    vm.shares.push(ShareSpec {
        host_path: e.dir.clone(),
        tag: "sbx0".into(),
        readonly: false,
    });
    let mut child = e
        .cmd("wait", "run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    write_line(
        &mut stdin,
        &Request::Launch {
            version: VERSION,
            vm,
        },
    )
    .unwrap();
    let ev = next(&mut BufReader::new(child.stdout.take().unwrap()));
    assert!(
        matches!(&ev, Event::Error { message } if message.contains("shared folders")),
        "{ev:?}"
    );
    assert!(!child.wait().unwrap().success());
    assert!(!args.exists(), "a share reached firecracker");

    let (mut child, stdin, _out, first) = e.launch("wait");
    assert!(matches!(first, Event::Started { .. }), "{first:?}");
    drop(stdin);
    assert!(child.wait().unwrap().success());
    e.assert_fake_gone();
    let config = e.dir.join("c.sock.firecracker.json");
    assert_eq!(
        std::fs::read_to_string(&args).unwrap(),
        format!("--no-api --config-file {}\n", config.display())
    );
}
