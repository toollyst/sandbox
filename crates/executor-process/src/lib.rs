//! 외부 실행기 어댑터. `executor-proto` 규약을 말하는 실행 파일을 띄우고 다룬다.
//!
//! sbx 쪽 어댑터는 이것 하나다. VMM마다 다른 것은 실행기 실행 파일(예: fc-fork)이 맡는다.
//!
//! - [`ProcessExecutor::new`]는 `<실행기> capabilities`를 한 번 실행해 capability를 받아 둔다.
//! - `launch`는 `<실행기> run`을 띄우고 `Launch`를 보낸 뒤 `Started`를 기다린다. 실행기의 표준 오류는 [`VmConfig::log`]에 붙인다.
//! - [`VmHandle`]이 살아 있는 동안 실행기의 표준 입력을 쥐고 있다. 버려지면 `Kill`을 보내고, 끝나지 않으면 프로세스를 죽인다.
//!   sbx 프로세스가 죽어도 표준 입력이 닫히므로 실행기가 VM을 끝낸다(규약 5).
//! - `connect`는 채널의 Unix 소켓에 vsock 방식으로 연결한다. Unix에서만 된다.
//! - 모든 블로킹 입출력은 별도 스레드에서 한다. 호출자의 비동기 스레드를 막지 않는다.
//!
//! - 의존: `sandbox-model`, `sandbox-executor`, `executor-proto`

use std::fs::OpenOptions;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use executor_proto as proto;
use sandbox_executor::{
    Capabilities, ChannelSpec, DiskRole, Executor, ExecutorError, GuestConn, VmConfig, VmHandle,
};
use sandbox_model::{Arch, BoxFuture, RootfsFormat, blocking};

/// `Kill`을 보낸 뒤 실행기가 스스로 끝나기를 기다리는 시간. 넘으면 프로세스를 죽인다.
pub const KILL_GRACE: Duration = Duration::from_secs(5);

/// 외부 실행기.
#[derive(Debug, Clone)]
pub struct ProcessExecutor {
    binary: PathBuf,
    caps: Capabilities,
}

fn invalid(m: impl Into<String>) -> ExecutorError {
    ExecutorError::Invalid(m.into())
}

/// 규약의 capability를 실행기 타입으로. 모르는 rootfs 형식 이름은 버린다.
pub fn convert_capabilities(c: proto::Capabilities) -> Result<Capabilities, ExecutorError> {
    if c.version != proto::VERSION {
        return Err(ExecutorError::Unsupported(format!(
            "executor {} speaks protocol {}, sbx speaks {}",
            c.name,
            c.version,
            proto::VERSION
        )));
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
        snapshot: c.snapshot,
        max_disks: c.max_disks as usize,
    })
}

/// 실행기 타입의 VM 설정을 규약으로.
pub fn convert_vm(vm: &VmConfig) -> proto::VmSpec {
    proto::VmSpec {
        guest_init: vm.guest_init.clone(),
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
        let binary = binary.into();
        let out = Command::new(&binary)
            .arg("capabilities")
            .stdin(Stdio::null())
            .output()
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
        let caps: proto::Capabilities = proto::read_line(&mut out.stdout.as_slice())
            .map_err(|e| invalid(format!("{} capabilities: {e}", binary.display())))?
            .ok_or_else(|| invalid(format!("{} capabilities printed nothing", binary.display())))?;
        Ok(Self {
            caps: convert_capabilities(caps)?,
            binary,
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
        let binary = self.binary.clone();
        Box::pin(async move {
            vm.check(&caps)?;
            let r = blocking(move || launch_blocking(&binary, &vm)).await?;
            Ok(Box::new(r) as Box<dyn VmHandle>)
        })
    }

    fn connect(&self, channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
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
}

fn launch_blocking(binary: &Path, vm: &VmConfig) -> Result<ProcessVm, ExecutorError> {
    if let Some(dir) = vm.log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = OpenOptions::new().create(true).append(true).open(&vm.log)?;
    let mut child = Command::new(binary)
        .arg("run")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
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
