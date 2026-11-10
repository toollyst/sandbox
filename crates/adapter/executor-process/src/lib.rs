//! 외부 실행기 어댑터. `sbx-executor-proto` 규약을 말하는 실행 파일을 띄우고 다룬다.
//!
//! sbx 쪽 어댑터는 이것 하나다. VMM마다 다른 것은 실행기 실행 파일(예: wick의 firecracker를 쓰는 sbx-executor-firecracker)이 맡는다.
//!
//! - [`ProcessExecutor::new`]는 `<실행기> capabilities`를 한 번 실행해 capability를 받아 둔다.
//! - `launch`는 `<실행기> run`을 띄우고 `Launch`를 보낸 뒤 `Started`를 기다린다. 실행기의 표준 오류는 [`VmConfig::log`]에 붙인다.
//! - [`VmHandle`]이 살아 있는 동안 실행기의 표준 입력을 쥐고 있다. 버려지면 `Kill`을 보내고, 끝나지 않으면 프로세스를 죽인다.
//!   sbx 프로세스가 죽어도 표준 입력이 닫히므로 실행기가 VM을 끝낸다(규약 5).
//! - `connect`는 채널의 Unix 소켓에 vsock 방식으로 연결한다. Unix에서만 된다.
//! - [`ConnectOnly`]는 실행 파일 없이 연결만 한다. VM을 띄우지 않는 일에 쓴다.
//! - 모든 블로킹 입출력은 별도 스레드에서 한다. 호출자의 비동기 스레드를 막지 않는다.
//!
//! - 의존: `sbx-model`, `sbx-executor`, `sbx-executor-proto`

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, BufReader};
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
}

fn invalid(m: impl Into<String>) -> ExecutorError {
    ExecutorError::Invalid(m.into())
}

fn version_mismatch(name: &str, version: u32) -> ExecutorError {
    ExecutorError::Unsupported(format!(
        "executor {name} speaks protocol {version}, sbx speaks {}",
        proto::VERSION
    ))
}

/// 규약의 capability를 실행기 타입으로. 모르는 rootfs 형식 이름은 버린다.
pub fn convert_capabilities(c: proto::Capabilities) -> Result<Capabilities, ExecutorError> {
    if c.version != proto::VERSION {
        return Err(version_mismatch(&c.name, c.version));
    }
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
    /// 실행 파일로 만든다. `<실행기> capabilities`를 실행해 본다(블로킹).
    pub fn new(binary: impl Into<PathBuf>) -> Result<Self, ExecutorError> {
        Self::with_env(binary, Vec::new())
    }

    /// 환경 변수 `env`를 더해 띄우는 실행기. 어댑터에 VMM 묶음을 알릴 때 쓴다([`proto::VMM_DIR_ENV`]).
    pub fn with_env(
        binary: impl Into<PathBuf>,
        env: Vec<(OsString, OsString)>,
    ) -> Result<Self, ExecutorError> {
        let binary = binary.into();
        let out = retry_busy(|| {
            Command::new(&binary)
                .envs(env.iter().map(|(k, v)| (k, v)))
                .arg("capabilities")
                .stdin(Stdio::null())
                .output()
        })
        .map_err(|e| {
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
        if header.version != proto::VERSION {
            return Err(version_mismatch(&header.name, header.version));
        }
        let caps: proto::Capabilities = proto::read_line(&mut out.stdout.as_slice())
            .map_err(bad)?
            .ok_or_else(|| invalid(format!("{} capabilities printed nothing", binary.display())))?;
        Ok(Self {
            caps: convert_capabilities(caps)?,
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
        let (binary, env) = (self.binary.clone(), self.env.clone());
        Box::pin(async move {
            vm.check(&caps)?;
            let r = blocking(move || launch_blocking(&binary, &env, &vm)).await?;
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

fn launch_blocking(
    binary: &Path,
    env: &[(OsString, OsString)],
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
            .stderr(log.try_clone()?)
            .spawn()
    })
    .map_err(|e| ExecutorError::Launch(format!("cannot run {}: {e}", binary.display())))?;
    let mut stdin = child.stdin.take().expect("piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("piped"));

    let fail = |child: &mut Child, msg: String| {
        let _ = child.kill();
        let _ = child.wait();
        ExecutorError::Launch(format!("{msg} (see {})", vm.log.display()))
    };
    if let Err(e) = proto::write_line(
        &mut stdin,
        &proto::Request::Launch {
            version: proto::VERSION,
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
