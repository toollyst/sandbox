//! 외부 실행기 어댑터. `sbx-executor-proto` 규약을 말하는 실행 파일을 띄우고 다룬다.
//!
//! sbx 쪽 어댑터는 이것 하나다. VMM마다 다른 것은 실행기 실행 파일(예: wick의 firecracker를 쓰는 sbx-executor-firecracker)이 맡는다.
//!
//! - [`ProcessExecutor::new`]는 `<실행기> capabilities`를 한 번 실행해 capability를 받아 둔다.
//! - `launch`는 `<실행기> run`을 띄우고 `Launch`를 보낸 뒤 `Started`를 기다린다. 실행기의 표준 오류는 [`VmConfig::log`]에 붙인다.
//!   표준 오류에는 게스트 콘솔이 섞일 수 있으므로(Firecracker의 직렬 출력) 띄울 때마다 [`MAX_LOG_BYTES`]까지만 쓰고
//!   나머지는 읽어서 버린다(게스트가 호스트 디스크를 채우지 못하게, 그리고 실행기가 쓰다 막히지 않게).
//! - [`VmHandle`]이 살아 있는 동안 실행기의 표준 입력을 쥐고 있다. 버려지면 `Kill`을 보내고, 끝나지 않으면 프로세스를 죽인다.
//!   sbx 프로세스가 죽어도 표준 입력이 닫히므로 실행기가 VM을 끝낸다(규약 5).
//! - `connect`는 채널의 Unix 소켓에 vsock 방식으로 연결한다. Unix에서만 된다.
//! - [`ConnectOnly`]는 실행 파일 없이 연결만 한다. VM을 띄우지 않는 일에 쓴다.
//! - 모든 블로킹 입출력은 별도 스레드에서 한다. 호출자의 비동기 스레드를 막지 않는다.
//!
//! - 의존: `sbx-model`, `sbx-executor`, `sbx-executor-proto`

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sbx_executor::{
    Capabilities, ChannelSpec, DiskRole, Executor, ExecutorError, GuestConn, VmConfig, VmHandle,
};
use sbx_executor_proto as proto;
use sbx_model::{Arch, BoxFuture, RootfsFormat, blocking};

/// `Kill`을 보낸 뒤 실행기가 스스로 끝나기를 기다리는 시간. 넘으면 프로세스를 죽인다.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// 외부 실행기.
#[derive(Debug, Clone)]
pub struct ProcessExecutor {
    binary: PathBuf,
    /// 실행기에 더 주는 환경 변수 (예: [`proto::VMM_DIR_ENV`]).
    env: Vec<(OsString, OsString)>,
    caps: Capabilities,
    /// 이 실행기와 쓸 규약 버전.
    version: u32,
}

fn invalid(m: impl Into<String>) -> ExecutorError {
    ExecutorError::Invalid(m.into())
}

fn range(min: u32, max: u32) -> String {
    if min == max {
        min.to_string()
    } else {
        format!("{min} to {max}")
    }
}

fn version_mismatch(name: &str, min: u32, max: u32) -> ExecutorError {
    ExecutorError::Unsupported(format!(
        "executor {name} speaks protocol {}, sbx speaks {}",
        range(min, max),
        range(proto::MIN_VERSION, proto::VERSION)
    ))
}

/// 실행기가 알린 범위로 쓸 규약 버전([`proto::negotiate`]).
fn negotiated(name: &str, min: Option<u32>, max: u32) -> Result<u32, ExecutorError> {
    let min = min.unwrap_or(max);
    proto::negotiate(min, max).ok_or_else(|| version_mismatch(name, min, max))
}

/// 규약의 capability를 실행기 타입으로. 모르는 rootfs 형식 이름은 버린다.
pub fn convert_capabilities(c: proto::Capabilities) -> Result<Capabilities, ExecutorError> {
    negotiated(&c.name, c.min_version, c.version)?;
    let name: &'static str = Box::leak(c.name.into_boxed_str());
    Ok(Capabilities {
        name,
        arch: match c.arch {
            proto::Arch::Amd64 => Arch::X86_64,
            proto::Arch::Arm64 => Arch::Aarch64,
        },
        rootfs_formats: c
            .rootfs_formats
            .iter()
            .filter_map(|f| f.parse::<RootfsFormat>().ok())
            .collect(),
        live_share: c.live_share,
        custom_kernel: c.custom_kernel,
        snapshot: c.snapshot,
        max_disks: c.max_disks as usize,
    })
}

/// 실행기 타입의 VM 설정을 규약으로.
pub fn convert_vm(vm: &VmConfig) -> proto::VmSpec {
    proto::VmSpec {
        guest_init: vm.guest_init.clone(),
        kernel: vm.kernel.clone(),
        disks: vm
            .disks
            .iter()
            .map(|d| proto::DiskSpec {
                path: d.path.clone(),
                role: match d.role {
                    DiskRole::Rootfs => proto::DiskRole::Rootfs,
                    DiskRole::Scratch => proto::DiskRole::Scratch,
                },
                readonly: d.readonly,
            })
            .collect(),
        vcpus: vm.vcpus,
        memory_mib: vm.memory_mib,
        shares: vm
            .shares
            .iter()
            .map(|s| proto::ShareSpec {
                host_path: s.host_path.clone(),
                tag: s.tag.clone(),
                readonly: s.readonly,
            })
            .collect(),
        control_socket: vm.control_channel.host_endpoint.clone(),
        hostname: vm.hostname.clone(),
    }
}

impl ProcessExecutor {
    /// 실행 파일로 만든다. `<실행기> capabilities`를 실행해 본다(블로킹, 길어야 [`CAPABILITIES_TIMEOUT`]).
    pub fn new(binary: impl Into<PathBuf>) -> Result<Self, ExecutorError> {
        Self::with_env(binary, Vec::new())
    }

    /// 환경 변수 `env`를 더해 띄우는 실행기. 어댑터에 VMM 묶음을 알릴 때 쓴다([`proto::VMM_DIR_ENV`]).
    pub fn with_env(
        binary: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Result<Self, ExecutorError> {
        Self::with_timeout(binary.into(), env, CAPABILITIES_TIMEOUT)
    }

    fn with_timeout(
        binary: PathBuf,
        env: Vec<(OsString, OsString)>,
        timeout: Duration,
    ) -> Result<Self, ExecutorError> {
        let mut cmd = Command::new(&binary);
        cmd.envs(env.iter().map(|(k, v)| (k, v)))
            .arg("capabilities")
            .stdin(Stdio::null());
        let out = bounded_output(&mut cmd, timeout).map_err(|e| {
            ExecutorError::Launch(format!("cannot run executor {}: {e}", binary.display()))
        })?;
        if !out.status.success() {
            return Err(ExecutorError::Launch(format!(
                "{} capabilities failed ({}): {}",
                binary.display(),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let bad = |e: io::Error| invalid(format!("{} capabilities: {e}", binary.display()));
        // 버전을 먼저 본다. 버전이 다르면 모양도 다를 수 있다.
        let header: proto::CapabilitiesHeader = proto::read_line(&mut out.stdout.as_slice())
            .map_err(bad)?
            .ok_or_else(|| invalid(format!("{} capabilities printed nothing", binary.display())))?;
        let version = negotiated(&header.name, header.min_version, header.version)?;
        let caps: proto::Capabilities = proto::read_line(&mut out.stdout.as_slice())
            .map_err(bad)?
            .ok_or_else(|| invalid(format!("{} capabilities printed nothing", binary.display())))?;
        Ok(Self {
            caps: convert_capabilities(caps)?,
            version,
            binary,
            env,
        })
    }

    /// 실행 파일 경로.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl Executor for ProcessExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn launch(&self, vm: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>> {
        let caps = self.caps.clone();
        let (binary, env, version) = (self.binary.clone(), self.env.clone(), self.version);
        Box::pin(async move {
            vm.check(&caps)?;
            let r = blocking(move || launch_blocking(&binary, &env, version, &vm)).await?;
            Ok(Box::new(r) as Box<dyn VmHandle>)
        })
    }

    fn connect(&self, channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        connect_channel(channel)
    }
}

/// 채널의 Unix 소켓에 vsock 방식으로 연결한다.
fn connect_channel(channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
    #[cfg(unix)]
    {
        Ok(Box::new(proto::vsock::connect(
            &channel.host_endpoint,
            channel.guest_port,
        )?))
    }
    #[cfg(not(unix))]
    {
        let _ = channel;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "guest connections need Unix sockets",
        ))
    }
}

/// 이미 떠 있는 VM의 게스트에 연결만 하는 실행기. 실행기 실행 파일을 실행하지 않는다.
///
/// 연결은 규약(제어 소켓 + vsock 방식 handshake)만으로 되므로 실행기가 필요 없다.
/// VM을 띄우지 않는 일(목록, 정지, 삭제, exec, cp)에 쓴다.
/// capability는 비어 있고(아무 형식도 받지 않는다) `launch`는 언제나 오류다.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConnectOnly;

/// [`ConnectOnly`]의 이름.
pub const CONNECT_ONLY: &str = "connect-only";

impl Executor for ConnectOnly {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: CONNECT_ONLY,
            arch: Arch::host().unwrap_or(Arch::X86_64),
            rootfs_formats: Vec::new(),
            live_share: false,
            custom_kernel: false,
            snapshot: false,
            max_disks: 0,
        }
    }

    fn launch(&self, _vm: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>> {
        Box::pin(async {
            Err(ExecutorError::Unsupported(
                "this executor only connects to running VMs".into(),
            ))
        })
    }

    fn connect(&self, channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        connect_channel(channel)
    }
}

/// 실행 파일이 막 쓰여 다른 프로세스가 아직 쓰기로 쥐고 있으면(Linux `ETXTBSY`, 예: 방금 내려받은 실행기,
/// 다른 스레드가 fork한 자식이 잠깐 물려받은 파일) 실행이 실패한다. 잠깐씩 기다리며 몇 번 다시 해 본다.
/// `capabilities`를 기다리는 가장 긴 시간. 그동안 답이 없으면 실행기를 죽이고 실패한다. 설치는 잠금을 쥔 채
/// 이것을 부르므로, 멈춘 실행기가 다른 설치를 끝없이 막지 못한다.
pub const CAPABILITIES_TIMEOUT: Duration = Duration::from_secs(30);

/// `cmd`를 실행해 출력을 모은다. 표준 출력은 두 줄([`proto::MAX_LINE`] 둘)까지, 표준 오류는 64KiB까지만 읽고,
/// `timeout`이 지나면 죽인다. 실행기가 띄운 자식이 파이프를 쥐고 있어도 기다림은 `timeout`을 넘지 않는다.
fn bounded_output(cmd: &mut Command, timeout: Duration) -> io::Result<std::process::Output> {
    use std::io::Read;
    const STDOUT_LIMIT: u64 = 2 * proto::MAX_LINE as u64 + 2;
    const STDERR_LIMIT: u64 = 64 * 1024;
    let deadline = Instant::now() + timeout;
    let mut child = retry_busy(|| cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn())?;
    let read = |pipe: Option<Box<dyn Read + Send>>, limit: u64| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(p) = pipe {
                let _ = p.take(limit).read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        STDOUT_LIMIT,
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
        STDERR_LIMIT,
    );
    let timed_out = || {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "capabilities did not finish within {}s",
                timeout.as_secs_f32()
            ),
        )
    };
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(timed_out());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let left = || deadline.saturating_duration_since(Instant::now());
    let stdout = stdout.recv_timeout(left()).map_err(|_| timed_out())?;
    if stdout.len() as u64 >= STDOUT_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("capabilities printed more than {STDOUT_LIMIT} bytes"),
        ));
    }
    let stderr = stderr.recv_timeout(left()).unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn retry_busy<T>(mut f: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut wait = std::time::Duration::from_millis(5);
    for _ in 0..8 {
        match f() {
            Err(e) if e.kind() == io::ErrorKind::ExecutableFileBusy => {
                std::thread::sleep(wait);
                wait *= 2;
            }
            r => return r,
        }
    }
    f()
}

/// 실행기 한 번(VM 하나)이 로그에 쓸 수 있는 바이트.
pub const MAX_LOG_BYTES: u64 = 16 << 20;

/// `from`을 끝까지 읽어 `limit` 바이트까지 `to`에 쓴다. 넘으면 한 줄로 알리고 나머지는 버린다.
fn copy_capped(mut from: impl Read, mut to: impl Write, limit: u64) {
    let mut buf = [0u8; 8192];
    let mut left = limit;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        };
        if left == 0 {
            continue;
        }
        let k = (n as u64).min(left) as usize;
        let _ = to.write_all(&buf[..k]);
        left -= k as u64;
        if left == 0 {
            let _ = writeln!(
                to,
                "\nsbx: this log reached {} MiB; the rest of the executor's output is dropped",
                limit >> 20
            );
        }
    }
}

fn launch_blocking(
    binary: &Path,
    env: &[(OsString, OsString)],
    version: u32,
    vm: &VmConfig,
) -> Result<ProcessVm, ExecutorError> {
    if let Some(dir) = vm.log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = OpenOptions::new().create(true).append(true).open(&vm.log)?;
    let mut child = retry_busy(|| {
        Command::new(binary)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .arg("run")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })
    .map_err(|e| ExecutorError::Launch(format!("cannot run {}: {e}", binary.display())))?;
    let err = child.stderr.take().expect("piped");
    let logger = std::sync::Mutex::new(Some(std::thread::spawn(move || {
        copy_capped(err, log, MAX_LOG_BYTES)
    })));
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));

    let fail = |child: &mut Child, msg: String| {
        let _ = child.kill();
        let _ = child.wait();
        // 실행기가 남긴 이유가 로그에 다 쓰인 뒤 돌아간다(호출자가 로그를 읽는다).
        if let Some(h) = logger.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        ExecutorError::Launch(format!("{msg} (see {})", vm.log.display()))
    };
    if let Err(e) = proto::write_line(
        &mut stdin,
        &proto::Request::Launch {
            version,
            vm: convert_vm(vm),
        },
    ) {
        return Err(fail(&mut child, format!("cannot send launch: {e}")));
    }
    let pid = match proto::read_line::<proto::Event>(&mut stdout) {
        Ok(Some(proto::Event::Started { pid })) => pid,
        Ok(Some(proto::Event::Error { message })) => return Err(fail(&mut child, message)),
        Ok(Some(other)) => {
            return Err(fail(&mut child, format!("expected started, got {other:?}")));
        }
        Ok(None) => {
            return Err(fail(
                &mut child,
                "executor exited before starting the vm".into(),
            ));
        }
        Err(e) => return Err(fail(&mut child, format!("bad message from executor: {e}"))),
    };

    let life = Arc::new(Life::default());
    {
        let life = life.clone();
        std::thread::spawn(move || {
            let reason = match proto::read_line::<proto::Event>(&mut stdout) {
                Ok(Some(proto::Event::Exited { reason })) => reason,
                Ok(Some(proto::Event::Error { message })) => format!("executor error: {message}"),
                Ok(Some(other)) => format!("unexpected message {other:?}"),
                Ok(None) => "executor exited without reporting".into(),
                Err(e) => format!("bad message from executor: {e}"),
            };
            life.end(reason);
        });
    }
    Ok(ProcessVm {
        child: Arc::new(Mutex::new(child)),
        stdin: Arc::new(Mutex::new(Some(stdin))),
        life,
        pid,
    })
}

#[derive(Debug, Default)]
struct Life {
    reason: Mutex<Option<String>>,
    changed: Condvar,
}

impl Life {
    fn end(&self, reason: String) {
        let mut r = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        if r.is_none() {
            *r = Some(reason);
        }
        self.changed.notify_all();
    }

    fn wait(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut r = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(reason) = &*r {
                return Some(reason.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            r = self
                .changed
                .wait_timeout(r, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// 외부 실행기가 띄운 VM.
#[derive(Debug)]
pub struct ProcessVm {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    life: Arc<Life>,
    pid: Option<u32>,
}

impl ProcessVm {
    /// VM이 끝났으면 실행기가 알린 이유.
    pub fn exit_reason(&self) -> Option<String> {
        self.life.wait(Duration::ZERO)
    }

    fn kill_blocking(
        child: &Mutex<Child>,
        stdin: &Mutex<Option<ChildStdin>>,
        life: &Life,
    ) -> Result<(), ExecutorError> {
        if life.wait(Duration::ZERO).is_none() {
            if let Some(s) = stdin.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                let _ = proto::write_line(s, &proto::Request::Kill {});
            }
            if life.wait(KILL_GRACE).is_none() {
                let mut c = child.lock().unwrap_or_else(|e| e.into_inner());
                let _ = c.kill();
                life.end("killed after the executor did not stop".into());
            }
        }
        // 표준 입력을 닫고 프로세스를 거둔다.
        stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
        child.lock().unwrap_or_else(|e| e.into_inner()).wait()?;
        Ok(())
    }
}

impl VmHandle for ProcessVm {
    fn pid(&self) -> Option<u32> {
        self.pid
    }

    fn wait(&mut self, timeout: Duration) -> BoxFuture<'_, Result<bool, ExecutorError>> {
        let life = self.life.clone();
        Box::pin(async move { Ok(blocking(move || life.wait(timeout).is_some()).await) })
    }

    fn kill(&mut self) -> Result<(), ExecutorError> {
        Self::kill_blocking(&self.child, &self.stdin, &self.life)
    }
}

impl Drop for ProcessVm {
    fn drop(&mut self) {
        let _ = Self::kill_blocking(&self.child, &self.stdin, &self.life);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// `capabilities`에 이 한 줄을 내는 실행기.
    fn executor(name: &str, line: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sbx-exproc-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("sbx-executor");
        std::fs::write(&exe, format!("#!/bin/sh\ncat <<'EOF'\n{line}\nEOF\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        exe
    }

    fn caps(min: Option<u32>, max: u32) -> String {
        let arch = if cfg!(target_arch = "aarch64") {
            "arm64"
        } else {
            "amd64"
        };
        let min = min.map_or(String::new(), |m| format!(r#""min_version":{m},"#));
        format!(
            r#"{{"version":{max},{min}"name":"x","arch":"{arch}","rootfs_formats":["erofs"],"live_share":false,"custom_kernel":false,"snapshot":false,"max_disks":2}}"#
        )
    }

    /// 멈추거나 끝없이 쓰는 실행기는 제한 시간 안에 죽이고 실패한다(설치 잠금을 쥔 채 멈추지 않는다).
    #[test]
    fn a_hung_or_flooding_executor_is_killed_and_refused() {
        let script = |name: &str, body: &str| {
            let exe = executor(name, "");
            std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
            exe
        };
        let t = Duration::from_millis(500);
        for (name, body) in [
            ("hang", "exec sleep 60"),
            // 자식이 표준 출력을 쥐고 남아도 기다림은 제한 시간을 넘지 않는다.
            ("orphan", "sleep 60 &\nexit 0"),
        ] {
            let start = Instant::now();
            let e = ProcessExecutor::with_timeout(script(name, body), Vec::new(), t)
                .unwrap_err()
                .to_string();
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "{name}: {:?}",
                start.elapsed()
            );
            assert!(e.contains("did not finish within 0.5s"), "{name}: {e}");
        }
        // 끝없이 쓰는 실행기: 상한까지만 읽고 거부한다. 시간 상한이 먼저 걸리지 않게 넉넉히 준다(바쁜 기계에서는
        // 셸이 뜨고 2 MiB를 쓰는 데 0.5초가 넘을 수 있다).
        let e = ProcessExecutor::with_timeout(
            script("flood", "exec yes"),
            Vec::new(),
            Duration::from_secs(10),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("capabilities printed more than"), "{e}");
        // 답은 했지만 너무 긴 줄: 끝까지 읽지 않고 거부한다.
        let long = format!("printf '%0{}d\\n' 0", proto::MAX_LINE + 10);
        let e = ProcessExecutor::with_timeout(
            script("long", &long),
            Vec::new(),
            Duration::from_secs(10),
        )
        .unwrap_err()
        .to_string();
        assert!(e.contains("line exceeds MAX_LINE"), "{e}");
    }

    #[test]
    fn executors_that_speak_a_range_meet_sbx_at_the_highest_common_version() {
        // 더 새 버전까지 말하지만 이 sbx의 버전도 말하는 실행기: 이 sbx의 가장 높은 버전으로 쓴다.
        let p = ProcessExecutor::new(executor(
            "newer",
            &caps(Some(proto::VERSION), proto::VERSION + 3),
        ))
        .unwrap();
        assert_eq!(p.version, proto::VERSION);
        let p = ProcessExecutor::new(executor("same", &caps(None, proto::VERSION))).unwrap();
        assert_eq!(p.version, proto::VERSION);
        // 겹치지 않으면 범위를 알린다.
        let e = ProcessExecutor::new(executor(
            "future",
            &caps(Some(proto::VERSION + 1), proto::VERSION + 2),
        ))
        .unwrap_err()
        .to_string();
        assert!(
            e.contains(&format!(
                "speaks protocol {} to {}",
                proto::VERSION + 1,
                proto::VERSION + 2
            )),
            "{e}"
        );
        let e = ProcessExecutor::new(executor("old", &caps(None, proto::MIN_VERSION - 1)))
            .unwrap_err()
            .to_string();
        assert!(e.contains("sbx speaks"), "{e}");
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    #[test]
    fn the_log_stops_at_its_limit_but_the_pipe_is_drained() {
        let input = vec![b'x'; 100_000];
        let mut out = Vec::new();
        copy_capped(&input[..], &mut out, 1000);
        assert!(out.starts_with(&[b'x'; 1000]));
        assert!(String::from_utf8_lossy(&out[1000..]).contains("dropped"));
        assert!(out.len() < 1200);
        let mut small = Vec::new();
        copy_capped(&b"hello"[..], &mut small, 1000);
        assert_eq!(small, b"hello");
    }
}
