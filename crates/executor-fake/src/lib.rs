//! 테스트용 가짜 실행기.
//!
//! VM을 띄우지 않고, 받은 설정을 기록만 한다. capability는 만들 때 정한다.
//! 실행 코어의 규칙을 모든 OS에서 실제 VM 없이 검증하는 데 쓴다.
//! 실제 어댑터와 같은 적합성 테스트를 통과해야 한다.
//!
//! - 의존: `sandbox-model`, `sandbox-executor`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_executor::{Capabilities, Executor, ExecutorError, VmConfig, VmHandle};
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
                l.push(vm);
            }
            Ok(Box::new(FakeVm { running: true }) as Box<dyn VmHandle>)
        })
    }
}

/// 가짜 VM.
#[derive(Debug)]
pub struct FakeVm {
    running: bool,
}

impl FakeVm {
    /// 아직 실행 중인지.
    pub fn is_running(&self) -> bool {
        self.running
    }
}

impl VmHandle for FakeVm {
    fn pid(&self) -> Option<u32> {
        None
    }

    fn shutdown(&mut self, _timeout: Duration) -> BoxFuture<'_, Result<(), ExecutorError>> {
        self.running = false;
        Box::pin(async { Ok(()) })
    }

    fn kill(&mut self) -> Result<(), ExecutorError> {
        self.running = false;
        Ok(())
    }
}
