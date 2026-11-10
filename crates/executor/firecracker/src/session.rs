//! `capabilities`, `run` 세션, 그리고 firecracker를 감시하는 `vmm` 모드. 규칙은 크레이트 문서에 있다.

use std::ffi::OsString;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use sbx_executor_proto::{
    Arch, Capabilities, Event, Request, VERSION, VmSpec, read_line, write_line,
};

use crate::config::firecracker_config;
use crate::settings::Settings;

/// 감시 프로세스 모드의 첫 인자 (내부용).
pub const VMM: &str = "vmm";

/// firecracker가 vsock 소켓을 만들기를 기다리는 시간.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// 프로세스 상태를 다시 보는 간격.
const POLL: Duration = Duration::from_millis(2);

/// 이 실행기가 알리는 기능.
pub fn capabilities_value() -> Capabilities {
    Capabilities {
        version: VERSION,
        min_version: None,
        name: "firecracker".into(),
        arch: if cfg!(target_arch = "aarch64") {
            Arch::Arm64
        } else {
            Arch::Amd64
        },
        rootfs_formats: vec!["erofs".into()],
        live_share: false,
        custom_kernel: true,
        snapshot: false,
        max_disks: 8,
    }
}

/// `capabilities`: 실행 파일과 커널을 찾을 수 있을 때만 성공한다.
pub fn capabilities() -> ExitCode {
    if let Err(e) = Settings::locate() {
        eprintln!("sbx-executor-firecracker: {e}");
        return ExitCode::FAILURE;
    }
    match write_line(&mut io::stdout(), &capabilities_value()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sbx-executor-firecracker: {e}");
            ExitCode::FAILURE
        }
    }
}

fn send(e: &Event) {
    let _ = write_line(&mut io::stdout(), e);
}

fn fail(message: String) -> ExitCode {
    eprintln!("sbx-executor-firecracker: {message}");
    send(&Event::Error { message });
    ExitCode::FAILURE
}

/// 세션을 멈추게 하는 입력.
enum Stop {
    /// `Kill`을 받았다.
    Kill,
    /// 입력이 끝났다(sbx가 끝났거나 놓았다).
    Closed,
    /// 규약 위반.
    Bad(String),
}

/// `run`: 제어 세션.
pub fn run() -> ExitCode {
    let mut input = io::BufReader::new(io::stdin());
    let vm: VmSpec = match read_line(&mut input) {
        Ok(Some(Request::Launch { version, vm })) if version == VERSION => vm,
        Ok(Some(Request::Launch { version, .. })) => {
            return fail(format!(
                "protocol version {version} is not supported (this executor speaks {VERSION})"
            ));
        }
        Ok(Some(other)) => return fail(format!("expected launch, got {other:?}")),
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => return fail(format!("bad request: {e}")),
    };
    let settings = match Settings::locate() {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    #[cfg(unix)]
    if let Some(jailer) = settings.jailer.clone() {
        let result = jailed(input, &vm, &settings, &jailer);
        let _ = std::fs::remove_file(&vm.control_socket);
        return result;
    }
    let config = match firecracker_config(&vm, &settings.kernel, cfg!(target_arch = "x86_64")) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    let config_path = config_path(&vm.control_socket);
    let result = match prepare(&vm.control_socket).and_then(|()| {
        write_config(&config_path, &config.to_string())
            .map_err(|e| format!("{}: {e}", config_path.display()))
    }) {
        Ok(()) => {
            let args = vec![
                "--no-api".into(),
                "--config-file".into(),
                config_path.clone().into(),
            ];
            launch_and_watch(input, &vm.control_socket, &settings.firecracker, args)
        }
        Err(e) => fail(e),
    };
    let _ = std::fs::remove_file(&config_path);
    let _ = std::fs::remove_file(&vm.control_socket);
    result
}

/// Firecracker는 소켓 파일이 이미 있으면 VM을 만들지 못한다.
fn prepare(control_socket: &Path) -> Result<(), String> {
    match std::fs::remove_file(control_socket) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", control_socket.display())),
    }
}

/// root: jailer로 가둔 firecracker로 돌린다(크레이트 문서의 "jailer" 절). 제어 소켓 자리에는 jail 안 vsock 소켓을
/// 가리키는 링크를 둔다.
#[cfg(unix)]
fn jailed(
    input: impl BufRead + Send + 'static,
    vm: &VmSpec,
    settings: &Settings,
    jailer: &Path,
) -> ExitCode {
    use crate::jail;
    let base = std::env::var_os(jail::ENV_CHROOT_BASE)
        .map(PathBuf::from)
        .unwrap_or_else(|| jail::DEFAULT_CHROOT_BASE.into());
    let id_base = match std::env::var(jail::ENV_ID_BASE) {
        Err(_) => jail::DEFAULT_ID_BASE,
        Ok(v) => match v.parse::<u32>() {
            Ok(n) if n > 0 && n.checked_add(jail::SLOTS + 1).is_some() => n,
            _ => return fail(format!("{}={v}: not a usable uid", jail::ENV_ID_BASE)),
        },
    };
    if let Err(e) = jail::check_base(&base) {
        return fail(e);
    }
    let slot = match jail::take_slot(&base) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let kernel = vm.kernel.as_deref().unwrap_or(&settings.kernel);
    let plan = jail::Plan::new(vm, kernel, &base, id_base, slot.n);
    let config = match firecracker_config(&plan.spec, &plan.kernel, cfg!(target_arch = "x86_64")) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    if let Err(e) = prepare(&vm.control_socket) {
        return fail(e);
    }
    let owners = match jail::stage(&plan) {
        Ok(o) => o,
        Err(e) => {
            jail::finish(&plan, &[]);
            return fail(e);
        }
    };
    let result = match jail::write_config(&plan, &config.to_string()).and_then(|()| {
        std::os::unix::fs::symlink(plan.socket(), &vm.control_socket)
            .map_err(|e| format!("{}: {e}", vm.control_socket.display()))
    }) {
        Ok(()) => {
            let cgroups = if std::env::var(jail::ENV_CGROUPS).as_deref() == Ok("off") {
                Vec::new()
            } else {
                std::fs::read_to_string(Path::new(jail::CGROUP_ROOT).join("cgroup.controllers"))
                    .map(|c| jail::cgroup_limits(vm, &c))
                    .unwrap_or_default()
            };
            launch_and_watch(
                input,
                &vm.control_socket,
                jailer,
                plan.args(&settings.firecracker, &cgroups),
            )
        }
        Err(e) => fail(e),
    };
    let _ = std::fs::remove_file(&vm.control_socket);
    jail::finish(&plan, &owners);
    drop(slot);
    result
}

/// Firecracker 설정 파일 자리: 제어 소켓 옆. 그 디렉터리는 sbx가 만든, 이 사용자만 쓸 수 있는 곳이다
/// (공용 임시 디렉터리에 두면 다른 사용자가 이름을 짐작해 미리 만들거나 링크를 걸 수 있다).
fn config_path(control_socket: &Path) -> PathBuf {
    let mut name = control_socket
        .file_name()
        .unwrap_or_default()
        .to_os_string();
    name.push(".firecracker.json");
    control_socket.with_file_name(name)
}

/// 새 파일로만 쓴다(있던 것은 지우고, 링크를 따라가지 않는다). 이 사용자만 읽는다.
fn write_config(path: &Path, text: &str) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    io::Write::write_all(&mut o.open(path)?, text.as_bytes())
}

/// `program args...`(firecracker나 jailer)를 감시 프로세스 아래에서 띄우고, `socket`이 생기면 `Started`를 보낸 뒤
/// VM이 끝날 때까지 지켜본다.
fn launch_and_watch(
    mut input: impl BufRead + Send + 'static,
    socket: &Path,
    program: &Path,
    args: Vec<OsString>,
) -> ExitCode {
    let mut vmm = match spawn_vmm(program, args) {
        Ok(c) => c,
        Err(e) => return fail(e),
    };
    // 준비: 소켓 파일이 생기면.
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        if socket.exists() {
            break;
        }
        if let Ok(Some(status)) = vmm.try_wait() {
            return fail(format!(
                "firecracker exited before the VM started ({status}); see the executor log"
            ));
        }
        if Instant::now() >= deadline {
            stop_vmm(&mut vmm);
            return fail(format!(
                "firecracker did not create {} within {READY_TIMEOUT:?}",
                socket.display()
            ));
        }
        thread::sleep(POLL);
    }
    send(&Event::Started {
        pid: Some(vmm.id()),
    });

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let stop = match read_line::<Request>(&mut input) {
            Ok(Some(Request::Kill {})) => Stop::Kill,
            Ok(Some(other)) => Stop::Bad(format!("unexpected request {other:?}")),
            Ok(None) => Stop::Closed,
            Err(e) => Stop::Bad(format!("bad request: {e}")),
        };
        let _ = tx.send(stop);
    });
    loop {
        if let Ok(Some(status)) = vmm.try_wait() {
            send(&Event::Exited {
                reason: exit_reason(status),
            });
            return ExitCode::SUCCESS;
        }
        match rx.try_recv() {
            Ok(Stop::Kill) | Ok(Stop::Closed) => {
                stop_vmm(&mut vmm);
                send(&Event::Exited {
                    reason: "killed".into(),
                });
                return ExitCode::SUCCESS;
            }
            Ok(Stop::Bad(m)) => {
                stop_vmm(&mut vmm);
                return fail(m);
            }
            Err(_) => thread::sleep(POLL),
        }
    }
}

fn exit_reason(s: ExitStatus) -> String {
    if s.success() {
        "guest powered off".into()
    } else {
        format!("firecracker exited with {s}")
    }
}

/// 감시 프로세스를 띄운다. 그 표준 입력을 쥐고 있는 동안만 VM이 산다.
fn spawn_vmm(program: &Path, args: Vec<OsString>) -> Result<Child, String> {
    let me = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    Command::new(me)
        .arg(VMM)
        .arg(program)
        .args(args)
        .stdin(Stdio::piped())
        // firecracker의 출력(게스트 콘솔)은 실행기 로그로. 표준 출력은 규약 메시지만 쓴다.
        .stdout(Stdio::from(io::stderr()))
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", program.display()))
}

/// 감시 프로세스의 입력을 닫아 VM을 끝내고 기다린다.
fn stop_vmm(vmm: &mut Child) {
    drop(vmm.stdin.take());
    let _ = vmm.wait();
}

/// `vmm <firecracker> <인자...>`: firecracker를 자식으로 띄우고, 표준 입력이 닫히면 죽인다.
/// firecracker가 끝나면 같은 성공 여부로 끝난다.
pub fn vmm(args: &[String]) -> ExitCode {
    let Some((program, rest)) = args.split_first() else {
        eprintln!("usage: sbx-executor-firecracker {VMM} <firecracker> <args...>");
        return ExitCode::from(2);
    };
    let mut child = match Command::new(PathBuf::from(program))
        .args(rest)
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sbx-executor-firecracker: cannot start {program}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tx, rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let _ = io::copy(&mut io::stdin().lock().by_ref(), &mut io::sink());
        let _ = tx.send(());
    });
    loop {
        match child.try_wait() {
            Ok(Some(s)) if s.success() => return ExitCode::SUCCESS,
            Ok(Some(_)) | Err(_) => return ExitCode::FAILURE,
            Ok(None) => {}
        }
        if rx.try_recv().is_ok() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = io::stderr().flush();
            return ExitCode::FAILURE;
        }
        thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_sits_beside_the_socket_and_never_follows_links() {
        let dir = std::env::temp_dir().join(format!("sbx-fc-config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = config_path(&dir.join("control.sock"));
        assert_eq!(p, dir.join("control.sock.firecracker.json"));
        write_config(&p, "{}").unwrap();
        write_config(&p, "{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":1}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            // 링크가 있으면 링크를 지우고 새로 만든다. 가리키던 파일은 그대로다.
            let target = dir.join("victim");
            std::fs::write(&target, "keep").unwrap();
            std::fs::remove_file(&p).unwrap();
            std::os::unix::fs::symlink(&target, &p).unwrap();
            write_config(&p, "{}").unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
            assert!(
                !std::fs::symlink_metadata(&p)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
