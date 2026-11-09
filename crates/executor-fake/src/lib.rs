//! 테스트용 가짜 실행기.
//!
//! VM을 띄우지 않는다. 대신 채널 위치([`ChannelSpec::host_endpoint`])에 Unix 소켓을 열고,
//! 연결마다 `sandbox-guest`의 연결 처리(`server::serve`)를 그대로 돌린다.
//! 그래서 실행 코어는 실제 게스트와 같은 메시지로 exec, 파일 복사, 종료 요청을 주고받는다.
//!
//! 실제 VM과 다른 점 (테스트를 쓸 때 알아야 할 것)
//! - 프로세스는 호스트에서, 지금 사용자 권한으로 돈다. rootfs와 scratch 디스크는 쓰지 않는다.
//!   그래서 실행 사용자는 지금 사용자여야 하고, 경로는 호스트 경로다.
//! - 사용자 이름은 호스트의 `/etc/passwd`로 해석한다.
//! - VM을 끝내도(`kill`, 종료 요청) 이미 실행 중인 프로세스는 남는다. 연결이 닫히면 게스트 쪽 처리가 끝낸다.
//! - Unix에서만 VM을 "띄운다". 다른 OS에서는 `launch`가 [`ExecutorError::Unsupported`]다.
//!
//! capability는 만들 때 정한다. 받은 설정은 [`FakeExecutor::launches`]로 볼 수 있다.
//!
//! - 의존: `sandbox-model`, `sandbox-executor`, `sandbox-guest`(Unix)

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sandbox_executor::{
    Capabilities, ChannelSpec, Executor, ExecutorError, GuestConn, VmConfig, VmHandle,
};
use sandbox_model::{Arch, BoxFuture, RootfsFormat};

/// 가짜 실행기.
#[derive(Debug, Clone)]
pub struct FakeExecutor {
    caps: Capabilities,
    launches: Arc<Mutex<Vec<VmConfig>>>,
}

impl FakeExecutor {
    /// capability를 정해서 만든다.
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            launches: Arc::default(),
        }
    }

    /// 흔한 기본값: 호스트 아키텍처, 모든 rootfs 형식, 실시간 공유 없음, 디스크 2개.
    pub fn basic() -> Self {
        Self::new(Capabilities {
            name: "fake",
            arch: Arch::host().unwrap_or(Arch::X86_64),
            rootfs_formats: RootfsFormat::ALL.to_vec(),
            live_share: false,
            snapshot: false,
            max_disks: 2,
        })
    }

    /// 지금까지 받은 VM 설정.
    pub fn launches(&self) -> Vec<VmConfig> {
        self.launches.lock().map(|l| l.clone()).unwrap_or_default()
    }
}

impl Executor for FakeExecutor {
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }

    fn launch(&self, vm: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>> {
        Box::pin(async move {
            vm.check(&self.caps)?;
            if let Ok(mut l) = self.launches.lock() {
                l.push(vm.clone());
            }
            guest::start(&vm.control_channel).map(|v| Box::new(v) as Box<dyn VmHandle>)
        })
    }

    fn connect(&self, channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        guest::connect(channel)
    }
}

/// VM 하나의 상태. 끝났는지를 여러 스레드가 본다.
#[derive(Debug, Default)]
struct Life {
    exited: Mutex<bool>,
    changed: Condvar,
}

impl Life {
    fn is_exited(&self) -> bool {
        *self.exited.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 처음 끝낼 때만 `true`.
    fn end(&self) -> bool {
        let mut e = self.exited.lock().unwrap_or_else(|e| e.into_inner());
        let first = !*e;
        *e = true;
        self.changed.notify_all();
        first
    }

    fn wait(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut e = self.exited.lock().unwrap_or_else(|e| e.into_inner());
        while !*e {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            e = self
                .changed
                .wait_timeout(e, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        true
    }
}

/// 가짜 VM.
pub struct FakeVm {
    life: Arc<Life>,
    stop: Arc<dyn Fn() + Send + Sync>,
}

impl std::fmt::Debug for FakeVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeVm")
            .field("running", &self.is_running())
            .finish()
    }
}

impl FakeVm {
    /// 아직 실행 중인지.
    pub fn is_running(&self) -> bool {
        !self.life.is_exited()
    }
}

impl VmHandle for FakeVm {
    fn pid(&self) -> Option<u32> {
        None
    }

    /// 블로킹으로 기다린다.
    fn wait(&mut self, timeout: Duration) -> BoxFuture<'_, Result<bool, ExecutorError>> {
        let life = self.life.clone();
        Box::pin(async move { Ok(life.wait(timeout)) })
    }

    fn kill(&mut self) -> Result<(), ExecutorError> {
        (self.stop)();
        Ok(())
    }
}

impl Drop for FakeVm {
    fn drop(&mut self) {
        (self.stop)();
    }
}

#[cfg(unix)]
mod guest {
    use super::*;

    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;
    use std::thread;

    use sandbox_guest::server::{self, Ctx};

    pub(super) fn connect(channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Ok(Box::new(UnixStream::connect(&channel.host_endpoint)?))
    }

    pub(super) fn start(channel: &ChannelSpec) -> Result<FakeVm, ExecutorError> {
        let path: PathBuf = channel.host_endpoint.clone();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let life = Arc::new(Life::default());

        let stop: Arc<dyn Fn() + Send + Sync> = {
            let (life, path) = (life.clone(), path.clone());
            Arc::new(move || {
                if life.end() {
                    // 기다리는 accept를 깨운다.
                    let _ = UnixStream::connect(&path);
                    let _ = std::fs::remove_file(&path);
                }
            })
        };
        let ctx = Arc::new(Ctx {
            root: "/".into(),
            on_shutdown: {
                let stop = stop.clone();
                Box::new(move || stop())
            },
        });
        {
            let life = life.clone();
            thread::spawn(move || {
                for conn in listener.incoming() {
                    if life.is_exited() {
                        break;
                    }
                    let Ok(conn) = conn else { continue };
                    let ctx = ctx.clone();
                    thread::spawn(move || server::serve(conn, &ctx));
                }
            });
        }
        Ok(FakeVm { life, stop })
    }
}

#[cfg(not(unix))]
mod guest {
    use super::*;

    pub(super) fn connect(_channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the fake guest needs Unix sockets",
        ))
    }

    pub(super) fn start(_channel: &ChannelSpec) -> Result<FakeVm, ExecutorError> {
        Err(ExecutorError::Unsupported(
            "the fake guest needs Unix sockets".into(),
        ))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use sandbox_executor::{Disk, DiskRole};
    use sandbox_model::BoxFuture;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    fn ready<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("future did not complete immediately"),
        }
    }

    fn vm(sock: &str) -> VmConfig {
        VmConfig {
            kernel: "k".into(),
            initrd: None,
            disks: vec![Disk {
                path: "r".into(),
                role: DiskRole::Rootfs,
                readonly: true,
            }],
            vcpus: 1,
            memory_mib: 64,
            shares: vec![],
            control_channel: ChannelSpec {
                host_endpoint: format!("/tmp/sbx-fake-{sock}-{}.sock", std::process::id()).into(),
            },
            hostname: "h".into(),
        }
    }

    #[test]
    fn launch_serves_guest_protocol_until_killed() {
        use sandbox_proto::{GuestMsg, HostMsg, PROTOCOL_VERSION};
        let fake = FakeExecutor::basic();
        let cfg = vm("serve");
        let mut handle = ready(fake.launch(cfg.clone())).unwrap();
        assert_eq!(fake.launches().len(), 1);

        let mut c = fake.connect(&cfg.control_channel).unwrap();
        HostMsg::Hello {
            version: PROTOCOL_VERSION,
        }
        .write_to(&mut c)
        .unwrap();
        assert!(matches!(
            GuestMsg::read_from(&mut c).unwrap(),
            GuestMsg::HelloAck { .. }
        ));
        drop(c);

        assert!(!ready(handle.wait(Duration::from_millis(10))).unwrap());
        handle.kill().unwrap();
        assert!(ready(handle.wait(Duration::from_secs(1))).unwrap());
        assert!(fake.connect(&cfg.control_channel).is_err());
    }

    #[test]
    fn guest_shutdown_request_ends_the_vm() {
        use sandbox_proto::{GuestMsg, HostMsg, PROTOCOL_VERSION};
        let fake = FakeExecutor::basic();
        let cfg = vm("shutdown");
        let mut handle = ready(fake.launch(cfg.clone())).unwrap();
        let mut c = fake.connect(&cfg.control_channel).unwrap();
        HostMsg::Hello {
            version: PROTOCOL_VERSION,
        }
        .write_to(&mut c)
        .unwrap();
        GuestMsg::read_from(&mut c).unwrap();
        HostMsg::Shutdown.write_to(&mut c).unwrap();
        assert_eq!(GuestMsg::read_from(&mut c).unwrap(), GuestMsg::Done);
        assert!(ready(handle.wait(Duration::from_secs(5))).unwrap());
    }

    #[test]
    fn launch_rejects_unsupported_config() {
        let fake = FakeExecutor::basic();
        let mut cfg = vm("reject");
        cfg.disks = vec![cfg.disks[0].clone(); 3];
        let r: BoxFuture<'_, _> = fake.launch(cfg);
        assert!(matches!(ready(r), Err(ExecutorError::Unsupported(_))));
    }
}
