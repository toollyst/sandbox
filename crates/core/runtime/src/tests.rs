//! 실행 코어 테스트. 같은 프로세스 안의 fake 실행기가 실제 게스트 처리를 호스트에서 돌린다.
//! 그래서 "게스트 경로"는 호스트의 임시 경로이고, 실행 사용자는 지금 사용자다.
// 게스트를 돌리는 테스트는 unix에서만 있다. 다른 곳에서는 그 도우미들을 쓰지 않는다.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use super::*;

use std::collections::BTreeMap;
use std::time::Instant;

use sbx_executor_fake::FakeExecutor;
use sbx_model::{Digest, ImageId, Mount, NetworkPolicy, Resources, RootfsFormat};

use crate::chan::block_on;

struct Env {
    root: PathBuf,
    rt: Runtime,
    uid: u32,
    gid: u32,
}

impl Env {
    fn new(name: &str) -> Self {
        // Unix 소켓 길이 제한 때문에 짧은 경로를 쓴다.
        let root = PathBuf::from(format!("/tmp/sbx-rt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("initramfs"), b"guest").unwrap();
        let rt = Runtime::new(
            RuntimeConfig {
                data_dir: root.join("d"),
                cache_dir: root.join("c"),
                guest_init: root.join("initramfs"),
                socket_dir: root.join("s"),
            },
            Arc::new(FakeExecutor::basic()),
        )
        .unwrap();
        // fake 게스트는 지금 사용자로 돈다. 디렉터리 소유 그룹은 부모를 따를 수 있으므로(macOS) id로 구한다.
        let id = |flag: &str| -> u32 {
            std::process::Command::new("id")
                .arg(flag)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0)
        };
        let (uid, gid) = (id("-u"), id("-g"));
        Self { root, rt, uid, gid }
    }

    fn image() -> ImageId {
        ImageId::new(Digest::parse(&format!("sha256:{}", "ef".repeat(32))).unwrap())
    }

    /// 이미지 기록 (`image.json`) 내용.
    fn record(id: &ImageId) -> Vec<u8> {
        serde_json::to_vec(&sbx_store::ImageRecord {
            id: id.clone(),
            reference: "docker.io/library/t:latest".into(),
            source: "test".into(),
            arch: sbx_model::Arch::host().unwrap_or(sbx_model::Arch::X86_64),
            config: Default::default(),
            layers: vec![],
            ignored_fields: vec![],
        })
        .unwrap()
    }

    /// 관리 쪽이 해 두었을 일: 이미지 기록, rootfs, 잠금 파일.
    fn prepare_image(&self) {
        let layout = self.rt.inner.store.layout();
        let id = Self::image();
        std::fs::create_dir_all(layout.image_dir(&id)).unwrap();
        std::fs::write(layout.image_record(&id), Env::record(&id)).unwrap();
        std::fs::write(layout.rootfs(&id, RootfsFormat::Erofs), b"fs").unwrap();
        std::fs::write(layout.image_lock(&id), b"").unwrap();
    }

    fn spec(&self, args: &[&str]) -> SandboxSpec {
        SandboxSpec {
            image: Self::image(),
            rootfs_format: RootfsFormat::Erofs,
            process: ProcessDefaults {
                args: args.iter().map(|s| s.to_string()).collect(),
                env: vec![("PATH".into(), "/usr/bin:/bin".into())],
                cwd: "/".into(),
                user: UserSpec {
                    user: Ident::Id(self.uid),
                    group: Some(Ident::Id(self.gid)),
                },
            },
            resources: Resources {
                vcpus: 1,
                memory_mib: 128,
                scratch_disk_mib: 4,
            },
            mounts: vec![],
            network: NetworkPolicy::Disabled,
            hostname: None,
            labels: BTreeMap::new(),
            stop: Default::default(),
            open_stdin: false,
            tty: false,
            name: None,
            lifetime_secs: None,
            ports: Vec::new(),
            egress_limit_bytes: None,
        }
    }

    fn sandbox(&self, args: &[&str]) -> Sandbox {
        self.prepare_image();
        block_on(self.rt.create(self.spec(args))).unwrap()
    }

    fn state(&self, s: &Sandbox) -> SandboxInfo {
        block_on(self.rt.get(s.id())).unwrap().info().clone()
    }

    fn wait_state(&self, s: &Sandbox, state: State) -> SandboxInfo {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let info = self.state(s);
            if info.state == state {
                return info;
            }
            assert!(Instant::now() < deadline, "still {:?}", info.state);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn exec(args: &[&str]) -> ExecSpec {
    ExecSpec {
        args: args.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        cwd: None,
        user: None,
        tty: false,
    }
}

/// 끝까지 읽는다. (stdout, stderr, 종료 코드)
fn collect(mut p: Process) -> (String, String, Option<i32>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while let Some(o) = block_on(p.next()) {
        match o {
            Output::Stdout(b) => out.extend(b),
            Output::Stderr(b) => err.extend(b),
        }
    }
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        p.exit_code(),
    )
}

#[test]
fn create_does_not_pull_missing_image() {
    let e = Env::new("nopull");
    assert!(matches!(
        block_on(e.rt.create(e.spec(&["sh"]))),
        Err(RuntimeError::ImageNotPulled(_))
    ));
    // 캐시를 만들지도 않는다.
    assert!(!e.root.join("c").exists());
}

#[test]
fn create_does_not_prepare_missing_rootfs() {
    let e = Env::new("noprep");
    let layout = e.rt.inner.store.layout();
    let id = Env::image();
    std::fs::create_dir_all(layout.image_dir(&id)).unwrap();
    std::fs::write(layout.image_record(&id), Env::record(&id)).unwrap();
    assert!(matches!(
        block_on(e.rt.create(e.spec(&["sh"]))),
        Err(RuntimeError::RootfsNotPrepared { .. })
    ));
}

#[test]
fn create_writes_records_and_sparse_scratch() {
    let e = Env::new("create");
    let s = e.sandbox(&["sh"]);
    assert_eq!(s.state(), State::Created);
    assert_eq!(s.id().as_str().len(), 64);
    let paths = e.rt.paths(s.id());
    assert_eq!(std::fs::metadata(paths.scratch()).unwrap().len(), 4 << 20);
    let list = block_on(e.rt.list()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].spec, e.spec(&["sh"]));
}

#[cfg(unix)]
#[test]
fn main_process_exit_stops_the_sandbox_with_its_code() {
    let e = Env::new("mainexit");
    let s = e.sandbox(&["sh", "-c", "echo hi; exit 3"]);
    let main = block_on(s.start(IoMode::default())).unwrap();
    assert_eq!(collect(main), ("hi\n".into(), "".into(), Some(3)));
    let info = e.wait_state(&s, State::Stopped);
    assert_eq!(info.exit_code, Some(3));
    assert_eq!(info.error, None);
    // 멈춘 뒤에는 다시 시작할 수 있다(Docker와 같다).
    let main = block_on(s.start(IoMode::default())).unwrap();
    assert_eq!(block_on(main.wait()).unwrap(), 3);
}

#[cfg(unix)]
#[test]
fn exec_while_running_then_stop() {
    let e = Env::new("exec");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let main = block_on(s.start(IoMode::default())).unwrap();
    assert_eq!(e.state(&s).state, State::Running);

    let mut x = exec(&["sh", "-c", "echo $FOO; echo err >&2; exit 4"]);
    x.env = vec![("FOO".into(), "bar".into())];
    let p = block_on(s.exec(x)).unwrap();
    assert_eq!(collect(p), ("bar\n".into(), "err\n".into(), Some(4)));

    // 표준 입력.
    let p = block_on(s.exec_with(
        exec(&["cat"]),
        IoMode {
            stdin: true,
            tty: None,
        },
    ))
    .unwrap();
    p.write_stdin(b"piped");
    p.close_stdin();
    assert_eq!(collect(p).0, "piped");

    // stop은 기본 프로세스에 SIGTERM을 먼저 보낸다. 그것으로 끝나면 128+15다(Docker와 같다).
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
    let info = e.state(&s);
    assert_eq!((info.state, info.exit_code), (State::Stopped, Some(143)));
    assert_eq!(block_on(main.wait()).unwrap(), 143);
    assert!(matches!(
        block_on(s.exec(exec(&["true"]))),
        Err(RuntimeError::InvalidState { .. })
    ));
    block_on(s.clone().delete()).unwrap();
    assert!(matches!(
        block_on(e.rt.get(s.id())),
        Err(RuntimeError::NotFound(_))
    ));
}

#[cfg(unix)]
#[test]
fn exec_failure_kinds_come_from_the_guest() {
    let e = Env::new("execfail");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let _main = block_on(s.start(IoMode::default())).unwrap();
    match block_on(s.exec(exec(&["no-such-binary-xyz"]))) {
        Err(RuntimeError::CommandFailed(g)) => {
            assert_eq!(g.kind, sbx_guest_proto::ErrorKind::NotFound)
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        block_on(s.exec(exec(&[]))),
        Err(RuntimeError::NoCommand)
    ));
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
}

#[cfg(unix)]
#[test]
fn start_guards() {
    let e = Env::new("guards");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    std::fs::remove_file(e.root.join("initramfs")).unwrap();
    assert!(matches!(
        block_on(s.start(IoMode::default())),
        Err(RuntimeError::GuestInitMissing(_))
    ));
    std::fs::write(e.root.join("initramfs"), b"guest").unwrap();
    let _main = block_on(s.start(IoMode::default())).unwrap();
    assert!(matches!(
        block_on(s.start(IoMode::default())),
        Err(RuntimeError::InvalidState { .. })
    ));
    assert!(matches!(
        block_on(s.clone().delete()),
        Err(RuntimeError::InvalidState { .. })
    ));
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
}

#[test]
fn running_record_without_owner_is_reported_stopped() {
    let e = Env::new("owner");
    let s = e.sandbox(&["sh"]);
    let mut st = StateRecord::new(State::Running);
    st.pid = Some(42);
    records::write_state(&e.rt.paths(s.id()), &st).unwrap();
    let info = e.state(&s);
    assert_eq!(info.state, State::Stopped);
    assert_eq!(info.error.as_deref(), Some(OWNER_GONE));
}

#[test]
fn corrupt_records_are_errors() {
    let e = Env::new("corrupt");
    let s = e.sandbox(&["sh"]);
    std::fs::write(e.rt.paths(s.id()).state(), b"{").unwrap();
    assert!(matches!(
        block_on(e.rt.list()),
        Err(RuntimeError::Corrupt { .. })
    ));
}

#[cfg(unix)]
#[test]
fn copy_follows_docker_cp_rules() {
    let e = Env::new("cp");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let _main = block_on(s.start(IoMode::default())).unwrap();
    // 호스트 원본.
    let src = e.root.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), "A").unwrap();
    std::fs::write(src.join("sub/b.txt"), "B").unwrap();
    // "게스트" 쪽(fake라 호스트 경로).
    let g = e.root.join("g");
    std::fs::create_dir_all(&g).unwrap();
    let gs = |p: &str| format!("{}/{p}", g.display());
    let cp_in = |host: &Path, guest: &str| block_on(s.copy_in(host, guest));

    // 파일 → 없는 경로: 그 이름으로.
    cp_in(&src.join("a.txt"), &gs("renamed.txt")).unwrap();
    assert_eq!(std::fs::read_to_string(g.join("renamed.txt")).unwrap(), "A");
    // 파일 → 디렉터리: 그 안에 원본 이름으로.
    cp_in(&src.join("a.txt"), &gs("")).unwrap();
    assert_eq!(std::fs::read_to_string(g.join("a.txt")).unwrap(), "A");
    // 파일 → 없는 디렉터리/: 오류.
    assert!(matches!(
        cp_in(&src.join("a.txt"), &gs("missing/")),
        Err(RuntimeError::Invalid(_))
    ));
    // 디렉터리 → 없는 경로: 그 경로로.
    cp_in(&src, &gs("tree")).unwrap();
    assert_eq!(
        std::fs::read_to_string(g.join("tree/sub/b.txt")).unwrap(),
        "B"
    );
    // 디렉터리 → 있는 디렉터리: 그 안에 원본 이름으로.
    cp_in(&src, &gs("tree")).unwrap();
    assert!(g.join("tree/src/a.txt").is_file());
    // 디렉터리/. → 있는 디렉터리: 내용만.
    std::fs::create_dir(g.join("flat")).unwrap();
    cp_in(&src.join("."), &gs("flat")).unwrap();
    assert!(g.join("flat/a.txt").is_file() && g.join("flat/sub/b.txt").is_file());
    // 디렉터리 → 파일: 오류.
    assert!(matches!(
        cp_in(&src, &gs("a.txt")),
        Err(RuntimeError::Invalid(_))
    ));

    // 게스트 → 호스트.
    let out = e.root.join("out");
    std::fs::create_dir(&out).unwrap();
    block_on(s.copy_out(&gs("tree"), &out.join("t2"))).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("t2/a.txt")).unwrap(), "A");
    block_on(s.copy_out(&gs("tree"), &out)).unwrap();
    assert!(out.join("tree/sub/b.txt").is_file());
    block_on(s.copy_out(&format!("{}/.", gs("flat")), &out.join("t2"))).unwrap();
    assert!(out.join("t2/sub/b.txt").is_file());
    block_on(s.copy_out(&gs("a.txt"), &out.join("copy.txt"))).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("copy.txt")).unwrap(), "A");
    match block_on(s.copy_out(&gs("nope"), &out)) {
        Err(RuntimeError::Guest(g)) => assert_eq!(g.kind, sbx_guest_proto::ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        block_on(s.copy_in(&src, "relative")),
        Err(RuntimeError::Invalid(_))
    ));
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
}

#[cfg(unix)]
#[test]
fn copy_mounts_are_copied_before_the_main_process() {
    let e = Env::new("mount");
    let src = e.root.join("work-src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("input"), "data").unwrap();
    let dest = e.root.join("work");
    e.prepare_image();
    let mut spec = e.spec(&["sh", "-c", &format!("cat {}/input", dest.display())]);
    spec.mounts = vec![Mount {
        source: src,
        destination: dest.to_str().unwrap().into(),
        kind: MountKind::Copy,
        readonly: false,
    }];
    let s = block_on(e.rt.create(spec)).unwrap();
    let main = block_on(s.start(IoMode::default())).unwrap();
    assert_eq!(collect(main), ("data".into(), "".into(), Some(0)));
}

#[cfg(unix)]
#[test]
fn main_command_that_cannot_start_leaves_the_sandbox_created() {
    let e = Env::new("mainfail");
    let s = e.sandbox(&["no-such-binary-xyz"]);
    match block_on(s.start(IoMode::default())) {
        Err(RuntimeError::CommandFailed(g)) => {
            assert_eq!(g.kind, sbx_guest_proto::ErrorKind::NotFound)
        }
        other => panic!("{other:?}"),
    }
    let info = e.state(&s);
    assert_eq!(info.state, State::Created);
    assert!(info.error.is_some());
}

#[cfg(unix)]
#[test]
fn main_exit_is_reported_after_the_sandbox_is_stopped() {
    let e = Env::new("mainorder");
    let s = e.sandbox(&["sh", "-c", "exit 3"]);
    let main = block_on(s.start(IoMode::default())).unwrap();
    assert_eq!(block_on(main.wait()).unwrap(), 3);
    // 기다리지 않고 곧바로: 이미 stopped이고 지울 수 있다.
    let info = e.state(&s);
    assert_eq!(info.state, State::Stopped);
    assert_eq!(info.exit_code, Some(3));
    block_on(block_on(e.rt.get(s.id())).unwrap().delete()).unwrap();
}

#[cfg(unix)]
#[test]
fn tty_merges_output_and_uses_the_requested_size() {
    let e = Env::new("tty");
    let s = e.sandbox(&["sh", "-c", "stty size; echo err >&2"]);
    let tty = IoMode {
        stdin: false,
        tty: Some(TtySize { rows: 12, cols: 34 }),
    };
    let mut main = block_on(s.start(tty)).unwrap();
    let mut out = Vec::new();
    while let Some(o) = block_on(main.next()) {
        match o {
            Output::Stdout(b) => out.extend(b),
            Output::Stderr(b) => panic!("stderr with a tty: {b:?}"),
        }
    }
    assert_eq!(main.exit_code(), Some(0));
    assert_eq!(String::from_utf8(out).unwrap(), "12 34\r\nerr\r\n");
}

#[cfg(unix)]
#[test]
fn bind_mount_is_shared_live() {
    let e = Env::new("bind");
    let host = e.root.join("host");
    std::fs::create_dir(&host).unwrap();
    std::fs::write(host.join("in.txt"), b"from host").unwrap();
    // fake 게스트의 경로는 호스트 경로다.
    let target = e.root.join("guest").join("data");
    e.prepare_image();
    let mut spec = e.spec(&[
        "sh",
        "-c",
        &format!(
            "cat {t}/in.txt; echo from guest > {t}/out.txt",
            t = target.display()
        ),
    ]);
    spec.mounts = vec![Mount {
        source: host.clone(),
        destination: target.display().to_string(),
        kind: MountKind::Bind,
        readonly: false,
    }];
    let s = block_on(e.rt.create(spec)).unwrap();
    let mut main = block_on(s.start(IoMode::default())).unwrap();
    let mut out = Vec::new();
    while let Some(o) = block_on(main.next()) {
        if let Output::Stdout(b) = o {
            out.extend(b);
        }
    }
    assert_eq!(main.exit_code(), Some(0));
    assert_eq!(out, b"from host");
    // 실시간 공유: 게스트가 쓴 것이 호스트에 있다.
    assert_eq!(
        std::fs::read(host.join("out.txt")).unwrap(),
        b"from guest\n"
    );
}

#[cfg(unix)]
#[test]
fn network_allow_list_is_enforced_through_the_guest_proxy() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    // 목적지: 받은 첫 줄을 그대로 돌려주는 서버.
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let allowed = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in server.incoming().flatten() {
            let mut line = String::new();
            BufReader::new(c.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let _ = c.write_all(format!("echo:{line}").as_bytes());
        }
    });

    // 목적지 2: 클라이언트가 보내기를 끝낼 때(반쯤 닫기)까지 읽고 받은 바이트 수를 답한다.
    let counter = TcpListener::bind("127.0.0.1:0").unwrap();
    let counted = counter.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in counter.incoming().flatten() {
            let mut all = Vec::new();
            c.read_to_end(&mut all).unwrap();
            let _ = c.write_all(format!("got {}", all.len()).as_bytes());
        }
    });

    let e = Env::new("egress");
    e.prepare_image();
    let mut spec = e.spec(&["sh", "-c", "sleep 30"]);
    spec.network = NetworkPolicy::Allow(vec![
        format!("127.0.0.1:{allowed}").parse().unwrap(),
        format!("127.0.0.1:{counted}").parse().unwrap(),
    ]);
    let s = block_on(e.rt.create(spec)).unwrap();
    let _main = block_on(s.start(IoMode::default())).unwrap();
    let port = e.state(&s).egress_port.expect("egress port");

    // 프로세스는 프록시 환경 변수를 받는다.
    let p = block_on(s.exec(exec(&["sh", "-c", "printf %s \"$HTTPS_PROXY $NO_PROXY\""]))).unwrap();
    let mut p = p;
    let mut out = Vec::new();
    while let Some(o) = block_on(p.next()) {
        if let Output::Stdout(b) = o {
            out.extend(b);
        }
    }
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("http://127.0.0.1:{port} localhost,127.0.0.1,::1")
    );

    // fake 게스트는 호스트에서 돌므로 테스트가 곧 게스트 안의 클라이언트다.
    let proxy = || TcpStream::connect(("127.0.0.1", port)).unwrap();
    let head = |c: &mut TcpStream| {
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut status = String::new();
        r.read_line(&mut status).unwrap();
        loop {
            let mut l = String::new();
            r.read_line(&mut l).unwrap();
            if l == "\r\n" || l.is_empty() {
                break;
            }
        }
        (status, r)
    };

    // CONNECT: 허용.
    let mut c = proxy();
    write!(c, "CONNECT 127.0.0.1:{allowed} HTTP/1.1\r\n\r\n").unwrap();
    let (status, mut r) = head(&mut c);
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    c.write_all(b"ping\n").unwrap();
    let mut reply = String::new();
    r.read_line(&mut reply).unwrap();
    assert_eq!(reply, "echo:ping\n");

    // 반쯤 닫기는 터널을 지나 목적지에 닿고, 그 뒤에도 답을 받는다. 터널 조각보다 큰 데이터.
    let mut c = proxy();
    write!(c, "CONNECT 127.0.0.1:{counted} HTTP/1.1\r\n\r\n").unwrap();
    let (status, mut r) = head(&mut c);
    assert!(status.starts_with("HTTP/1.1 200"), "{status}");
    c.write_all(&vec![7u8; 3 * 1024 * 1024 + 5]).unwrap();
    c.shutdown(std::net::Shutdown::Write).unwrap();
    let mut reply = String::new();
    r.read_to_string(&mut reply).unwrap();
    assert_eq!(reply, format!("got {}", 3 * 1024 * 1024 + 5));

    // 목록에 없는 포트: 403과 이유.
    let mut c = proxy();
    write!(
        c,
        "CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n",
        (1..)
            .map(|d| allowed.wrapping_add(d))
            .find(|p| *p != counted)
            .unwrap()
    )
    .unwrap();
    let (status, mut r) = head(&mut c);
    assert!(status.starts_with("HTTP/1.1 403"), "{status}");
    let mut body = String::new();
    r.read_to_string(&mut body).unwrap();
    assert!(body.contains("not allowed"), "{body}");

    // 평문 HTTP 절대 형식: 요청을 그대로 목적지로 보낸다.
    let mut c = proxy();
    write!(
        c,
        "GET http://127.0.0.1:{allowed}/x HTTP/1.1\r\nHost: a\r\n\r\n"
    )
    .unwrap();
    let mut reply = String::new();
    BufReader::new(c).read_line(&mut reply).unwrap();
    assert_eq!(
        reply,
        format!("echo:GET http://127.0.0.1:{allowed}/x HTTP/1.1\r\n")
    );

    // 연결을 여러 번 써도 맡긴 연결이 다시 채워진다.
    for _ in 0..(egress::POOL * 2) {
        let mut c = proxy();
        write!(c, "CONNECT 127.0.0.1:{allowed} HTTP/1.1\r\n\r\n").unwrap();
        assert!(head(&mut c).0.starts_with("HTTP/1.1 200"));
    }
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
}

#[test]
fn list_never_fails_while_sandboxes_are_removed() {
    let e = Env::new("listrm");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let rt = e.rt.clone();
    let flag = stop.clone();
    let reader = std::thread::spawn(move || {
        let mut n = 0;
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            block_on(rt.list()).unwrap();
            n += 1;
        }
        n
    });
    for _ in 0..200 {
        let s = e.sandbox(&["true"]);
        block_on(s.delete()).unwrap();
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
    assert!(block_on(e.rt.list()).unwrap().is_empty());
    // 지운 디렉터리가 남지 않는다.
    let left: Vec<_> = std::fs::read_dir(e.root.join("d").join("sandboxes"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn sandbox_names_reads_records_without_touching_locks() {
    let e = Env::new("namelist");
    let data = e.root.join("d");
    assert!(sandbox_names(&e.root.join("none")).unwrap().is_empty());
    let mut spec = e.spec(&["true"]);
    spec.name = Some("web".into());
    e.prepare_image();
    let named = block_on(e.rt.create(spec)).unwrap();
    let plain = e.sandbox(&["true"]);
    let lock = Paths::new(&data, plain.id()).owner_lock();
    let _ = std::fs::remove_file(&lock);
    let mut want = vec![
        (named.id().clone(), Some("web".to_owned())),
        (plain.id().clone(), None),
    ];
    want.sort();
    assert_eq!(sandbox_names(&data).unwrap(), want);
    // 소유 잠금 파일을 만들지 않는다(Runtime::list는 만든다).
    assert!(!lock.exists());
    // 만드는 중인 것(상태 기록이 아직 없는 것)은 건너뛴다.
    std::fs::remove_file(Paths::new(&data, plain.id()).state()).unwrap();
    assert_eq!(sandbox_names(&data).unwrap().len(), 1);
}

/// 시그널을 받으면 `code`로 끝나고, 다른 시그널(TERM 포함)은 무시하는 기본 프로세스.
#[cfg(unix)]
fn trapping(sig: &str, code: i32) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!("trap '' TERM; trap 'exit {code}' {sig}; echo ready; while :; do sleep 0.02; done"),
    ]
}

/// 기본 프로세스가 트랩을 걸 때까지 기다린다.
#[cfg(unix)]
fn until_ready(main: &mut Process) {
    match block_on(main.next()) {
        Some(Output::Stdout(b)) => assert_eq!(b, b"ready\n"),
        other => panic!("{other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn stop_falls_back_to_power_off_after_the_timeout() {
    let e = Env::new("stopto");
    let args = trapping("USR2", 1);
    let s = e.sandbox(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let mut main = block_on(s.start(IoMode::default())).unwrap();
    until_ready(&mut main);
    let t = Instant::now();
    block_on(s.stop(None, Duration::from_millis(300))).unwrap();
    assert!(
        t.elapsed() >= Duration::from_millis(300),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(e.state(&s).exit_code, Some(EXIT_VM_ENDED));
    assert_eq!(block_on(main.wait()).unwrap(), EXIT_VM_ENDED);
}

#[cfg(unix)]
#[test]
fn stop_uses_the_given_or_recorded_signal() {
    let e = Env::new("stopsig");
    // 기록한 시그널 (이미지 StopSignal이나 --stop-signal).
    e.prepare_image();
    let mut spec = e.spec(&[]);
    spec.process.args = trapping("INT", 9);
    spec.stop.signal = "SIGINT".parse().unwrap();
    let s = block_on(e.rt.create(spec)).unwrap();
    let mut main = block_on(s.start(IoMode::default())).unwrap();
    until_ready(&mut main);
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
    assert_eq!(e.state(&s).exit_code, Some(9));
    // 호출자가 준 시그널이 이긴다 (`stop -s`).
    let mut main = block_on(s.start(IoMode::default())).unwrap();
    until_ready(&mut main);
    let usr1 = "USR1".parse().unwrap();
    assert!(matches!(
        block_on(s.stop(Some(usr1), Duration::from_millis(200))),
        Ok(())
    ));
    // USR1은 트랩이 없어 기본 동작(끝남)으로 끝난다: 128+10 (Linux 번호).
    assert_eq!(e.state(&s).exit_code, Some(128 + 10));
}

#[cfg(unix)]
#[test]
fn kill_signals_the_main_process_and_wait_reports_its_code() {
    let e = Env::new("killwait");
    let args = trapping("USR1", 42);
    let s = e.sandbox(&args.iter().map(String::as_str).collect::<Vec<_>>());
    let mut main = block_on(s.start(IoMode::default())).unwrap();
    until_ready(&mut main);
    let waiter = {
        let s = s.clone();
        std::thread::spawn(move || block_on(s.wait()))
    };
    // TERM은 무시하므로 멈추지 않는다.
    block_on(s.kill(Signal::TERM)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(e.state(&s).state, State::Running);
    block_on(s.kill("USR1".parse().unwrap())).unwrap();
    assert_eq!(waiter.join().unwrap().unwrap(), 42);
    assert_eq!(block_on(main.wait()).unwrap(), 42);
    // 멈춘 뒤: kill은 오류, wait은 곧바로 기록된 코드.
    assert!(matches!(
        block_on(s.kill(Signal::KILL)),
        Err(RuntimeError::InvalidState { .. })
    ));
    assert_eq!(block_on(s.wait()).unwrap(), 42);
    // 지운 뒤에는 NotFound.
    let gone = s.clone();
    block_on(s.delete()).unwrap();
    assert!(matches!(
        block_on(gone.wait()),
        Err(RuntimeError::NotFound(_))
    ));
}

#[test]
fn ids_resolve_by_unique_prefix() {
    let e = Env::new("resolve");
    let spec = e.spec(&["true"]);
    for id in ["abc1", "abc2", "abd"] {
        records::create(&e.root.join("d"), &SandboxId::new(id).unwrap(), &spec, 1).unwrap();
    }
    let r = |q: &str| block_on(e.rt.resolve(q));
    assert_eq!(r("abd").unwrap().as_str(), "abd");
    assert_eq!(r("abc1").unwrap().as_str(), "abc1");
    assert_eq!(r("abc2").unwrap().as_str(), "abc2");
    assert!(matches!(
        r("abc"),
        Err(RuntimeError::Ambiguous { matches: 2, .. })
    ));
    assert!(matches!(
        r("ab"),
        Err(RuntimeError::Ambiguous { matches: 3, .. })
    ));
    assert!(matches!(r("x"), Err(RuntimeError::NoMatch(_))));
    assert!(matches!(r(""), Err(RuntimeError::NoMatch(_))));
    assert!(matches!(r("ABC"), Err(RuntimeError::NoMatch(_))));
    // 정확히 같은 ID는 다른 ID의 앞부분이어도 그것이다.
    records::create(&e.root.join("d"), &SandboxId::new("ab").unwrap(), &spec, 1).unwrap();
    assert_eq!(r("ab").unwrap().as_str(), "ab");
    assert_eq!(block_on(e.rt.find("abd")).unwrap().id().as_str(), "abd");
}

#[test]
fn names_are_unique_resolve_and_free_on_delete() {
    let e = Env::new("names");
    e.prepare_image();
    let named = |name: &str| {
        let mut spec = e.spec(&["true"]);
        spec.name = Some(name.into());
        block_on(e.rt.create(spec))
    };
    let a = named("web").unwrap();
    assert_eq!(a.info().spec.name.as_deref(), Some("web"));
    let r = |q: &str| block_on(e.rt.resolve(q));
    assert_eq!(r("web").unwrap(), *a.id());

    // 같은 이름은 거절되고, 만들던 것은 남지 않는다.
    let before = block_on(e.rt.list()).unwrap().len();
    match named("web") {
        Err(RuntimeError::NameInUse { name, id }) => {
            assert_eq!(name, "web");
            assert_eq!(id, *a.id());
        }
        other => panic!("{:?}", other.map(|s| s.id().clone())),
    }
    assert_eq!(block_on(e.rt.list()).unwrap().len(), before);
    assert!(matches!(named("-x"), Err(RuntimeError::InvalidName(_))));

    // 이름은 ID 앞부분보다 먼저 본다(Docker와 같다).
    let prefix = &a.id().as_str()[..4];
    let b = named(prefix).unwrap();
    assert_eq!(r(prefix).unwrap(), *b.id());

    // 지우면 이름이 풀리고 다시 쓸 수 있다.
    block_on(a.delete()).unwrap();
    assert!(matches!(r("web"), Err(RuntimeError::NoMatch(_))));
    let c = named("web").unwrap();
    assert_eq!(r("web").unwrap(), *c.id());

    // 없는 샌드박스를 가리키는 남은 이름 파일은 넘겨받는다.
    let names = e.root.join("d").join("names");
    std::fs::write(names.join("stale"), "0123456789ab").unwrap();
    assert!(matches!(r("stale"), Err(RuntimeError::NoMatch(_))));
    let d = named("stale").unwrap();
    assert_eq!(r("stale").unwrap(), *d.id());
}

#[test]
fn hostile_signal_numbers_do_not_overflow_the_exit_code() {
    use sbx_guest_proto::ExitStatus;
    assert_eq!(crate::exit_code(ExitStatus::Signal(15)), 143);
    assert_eq!(crate::exit_code(ExitStatus::Signal(i32::MAX)), i32::MAX);
    assert_eq!(crate::exit_code(ExitStatus::Code(-1)), -1);
}

#[cfg(unix)]
#[test]
fn a_planted_socket_directory_link_is_refused() {
    use std::os::unix::fs::PermissionsExt;
    let root = PathBuf::from(format!("/tmp/sbx-rt-sockdir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("real")).unwrap();
    std::fs::set_permissions(root.join("real"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    let e = crate::prepare_socket_dir(&root.join("link")).unwrap_err();
    assert!(e.to_string().contains("not a directory owned"), "{e}");
    // 링크가 가리킨 디렉터리의 권한은 바꾸지 않는다.
    let mode = std::fs::metadata(root.join("real"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
    crate::prepare_socket_dir(&root.join("own")).unwrap();
    let mode = std::fs::metadata(root.join("own"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700);
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(unix)]
#[test]
fn state_changes_are_recorded_as_events() {
    let e = Env::new("events");
    let mut r = events_log(&e.root.join("d"))
        .reader(sbx_events::Start::Beginning)
        .unwrap();
    let mut spec = {
        e.prepare_image();
        e.spec(&["sh", "-c", "exit 3"])
    };
    spec.name = Some("ev1".into());
    let s = block_on(e.rt.create(spec)).unwrap();
    let main = block_on(s.start(IoMode::default())).unwrap();
    assert_eq!(block_on(main.wait()).unwrap(), 3);
    e.wait_state(&s, State::Stopped);
    // 멈춤은 기록을 쓴 뒤 덧붙인다. 기록이 보인 뒤 잠깐 기다린다.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = Vec::new();
    while got.len() < 3 && Instant::now() < deadline {
        got.extend(std::iter::from_fn(|| r.next_event().unwrap()));
        std::thread::sleep(Duration::from_millis(10));
    }
    block_on(block_on(e.rt.get(s.id())).unwrap().delete()).unwrap();
    got.extend(std::iter::from_fn(|| r.next_event().unwrap()));

    let actions: Vec<&str> = got.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["create", "start", "stop", "remove"]);
    for ev in &got {
        assert_eq!(ev.kind, EVENT_KIND);
        assert_eq!(ev.id, s.id().as_str());
        assert_eq!(ev.attributes["name"], "ev1");
        assert_eq!(ev.attributes["image"], Env::image().to_string());
    }
    assert_eq!(got[2].attributes["exit_code"], "3");
    assert!(!got[2].attributes.contains_key("error"));
}

#[cfg(unix)]
#[test]
fn exec_is_recorded_with_its_argv_and_exit_code() {
    let e = Env::new("evexec");
    e.prepare_image();
    let s = block_on(e.rt.create(e.spec(&["sleep", "30"]))).unwrap();
    let _main = block_on(s.start(IoMode::default())).unwrap();
    let s = block_on(e.rt.get(s.id())).unwrap();
    let mut r = events_log(&e.root.join("d"))
        .reader(sbx_events::Start::End)
        .unwrap();
    let p = block_on(s.exec(exec(&["sh", "-c", "exit 5"]))).unwrap();
    assert_eq!(block_on(p.wait()).unwrap(), 5);
    let pid = block_on(s.exec_detached(exec(&["true"]))).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got = Vec::new();
    while got.len() < 3 && Instant::now() < deadline {
        got.extend(std::iter::from_fn(|| r.next_event().unwrap()));
        std::thread::sleep(Duration::from_millis(10));
    }
    block_on(s.stop(None, Duration::ZERO)).unwrap();
    let actions: Vec<&str> = got.iter().map(|e| e.action.as_str()).collect();
    assert_eq!(actions, ["exec_start", "exec_die", "exec_start"]);
    assert_eq!(got[0].attributes["args"], r#"["sh","-c","exit 5"]"#);
    assert_eq!(got[1].attributes["exit_code"], "5");
    assert_eq!(got[0].attributes["pid"], got[1].attributes["pid"]);
    assert_eq!(got[2].attributes["detached"], "true");
    assert_eq!(got[2].attributes["pid"], pid.to_string());
}

#[test]
fn a_failed_create_records_nothing() {
    let e = Env::new("evfail");
    // 이미지가 없어서 실패한다.
    assert!(block_on(e.rt.create(e.spec(&["sh"]))).is_err());
    let mut r = events_log(&e.root.join("d"))
        .reader(sbx_events::Start::Beginning)
        .unwrap();
    assert!(r.next_event().unwrap().is_none());
}

/// 시험 환경 이름은 겹치면 안 된다: 같은 프로세스에서 나란히 돌며 서로의 디렉터리를 지운다.
#[test]
fn test_environment_names_are_unique() {
    let src = include_str!("tests.rs");
    let mut names: Vec<&str> = src
        .split("Env::new(\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "an Env::new name is used twice");
}

#[cfg(unix)]
#[test]
fn sandbox_records_are_private_to_their_user() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("perm");
    let s = env.sandbox(&["true"]);
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let dir = env.root.join("d/sandboxes").join(s.id().as_str());
    assert_eq!(mode(&env.root.join("d/sandboxes")), 0o700);
    assert_eq!(mode(&dir), 0o700);
    for f in ["spec.json", "state.json", "scratch.img", "owner.lock"] {
        assert_eq!(mode(&dir.join(f)), 0o600, "{f}");
    }
    // 0.2.0이 만든 열린 디렉터리는 좁힌다.
    let old = env.root.join("old");
    std::fs::create_dir(&old).unwrap();
    std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    records::private_dir(&old).unwrap();
    assert_eq!(mode(&old), 0o700);
    let _ = std::fs::remove_dir_all(&env.root);
}

#[cfg(unix)]
#[test]
fn a_sandbox_stops_when_its_lifetime_runs_out() {
    let e = Env::new("life");
    e.prepare_image();
    let mut spec = e.spec(&["sleep", "30"]);
    spec.lifetime_secs = Some(1);
    spec.stop.timeout_secs = Some(1);
    let s = block_on(e.rt.create(spec)).unwrap();
    let started = std::time::Instant::now();
    let main = block_on(s.start(IoMode::default())).unwrap();
    // sleep은 TERM으로 끝난다(128+15).
    let code = block_on(main.wait()).unwrap();
    assert_eq!(code, 143);
    assert!(started.elapsed() < Duration::from_secs(20));
    let info = e.wait_state(&s, State::Stopped);
    assert_eq!(info.exit_code, Some(143));
    let _ = std::fs::remove_dir_all(&e.root);
}

#[cfg(unix)]
#[test]
fn published_ports_reach_a_service_inside_and_close_on_stop() {
    use std::io::{Read, Write};
    let e = Env::new("ports");
    e.prepare_image();
    // 가짜 실행기의 게스트는 호스트에서 돈다: 게스트 안 서비스 흉내를 호스트 포트에 띄운다.
    let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let guest_port = service.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in service.incoming() {
            let Ok(mut s) = s else { return };
            std::thread::spawn(move || {
                let mut b = Vec::new();
                let _ = s.read_to_end(&mut b);
                let _ = s.write_all(&b.to_ascii_uppercase());
            });
        }
    });
    let mut spec = e.spec(&["sleep", "30"]);
    spec.ports = vec![format!("127.0.0.1::{guest_port}").parse().unwrap()];
    let s = block_on(e.rt.create(spec)).unwrap();
    let _main = block_on(s.start(IoMode::default())).unwrap();
    let info = block_on(e.rt.get(s.id())).unwrap().info().clone();
    assert_eq!(info.published.len(), 1);
    let (addr, g) = info.published[0];
    assert_eq!(g, guest_port);
    assert_ne!(addr.port(), 0, "the picked port is recorded");
    for _ in 0..2 {
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        c.write_all(b"hello").unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).unwrap();
        assert_eq!(got, b"HELLO");
    }
    block_on(
        block_on(e.rt.get(s.id()))
            .unwrap()
            .stop(None, Duration::ZERO),
    )
    .unwrap();
    let info = e.wait_state(&s, State::Stopped);
    assert!(info.published.is_empty());
    // 멈추면 듣지 않는다.
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::net::TcpStream::connect(addr).is_ok() {
        assert!(Instant::now() < deadline, "{addr} still accepts after stop");
        std::thread::sleep(Duration::from_millis(20));
    }

    // 이미 쓰는 포트면 VM 없이 시작이 실패한다.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut spec = e.spec(&["sleep", "30"]);
    spec.ports = vec![
        format!("{}:{guest_port}", taken.local_addr().unwrap().port())
            .parse()
            .unwrap(),
    ];
    let s = block_on(e.rt.create(spec)).unwrap();
    let err = block_on(s.start(IoMode::default()))
        .unwrap_err()
        .to_string();
    assert!(err.contains("can't listen"), "{err}");
    let _ = std::fs::remove_dir_all(&e.root);
}
