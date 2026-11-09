//! 테스트용 가짜 실행기. 두 가지로 쓴다.
//!
//! - [`FakeExecutor`]: 같은 프로세스 안의 `Executor` 구현. 실행 코어 단위 테스트용.
//! - `sbx-fake-executor` 실행 파일([`process::main`]): `executor-proto` 규약을 그대로 말하는 외부 실행기.
//!   `executor-process` 어댑터와 CLI가 실제 실행기와 같은 경로로 동작하는지 검증한다.
//!
//! 둘 다 VM을 띄우지 않는다. 대신 채널 위치(제어 소켓)에 Unix 소켓을 열고, 규약의 vsock 방식
//! (`CONNECT <포트>`)으로 연결을 받은 뒤 `sandbox-guest`의 연결 처리(`server::serve`)를 그대로 돌린다.
//! 그래서 실행 코어는 실제 게스트와 같은 메시지로 exec, 파일 복사, 종료 요청을 주고받는다.
//!
//! 실제 VM과 다른 점 (테스트를 쓸 때 알아야 할 것)
//! - 프로세스는 호스트에서, 지금 사용자 권한으로 돈다. rootfs와 scratch 디스크는 쓰지 않는다.
//!   그래서 실행 사용자는 지금 사용자여야 하고, 경로는 호스트 경로다.
//! - 사용자 이름은 호스트의 `/etc/passwd`로 해석한다.
//! - VM을 끝내면 열린 게스트 연결을 모두 닫는다. 그러면 게스트 쪽 처리가 그 연결의 프로세스를 SIGKILL로 끝낸다
//!   (실제 VM에서 전원이 꺼지는 것과 같은 결과). 그 프로세스가 띄운 다른 호스트 프로세스는 남을 수 있다.
//! - 게스트 포트([`sandbox_proto::GUEST_PORT`]) 외의 포트로 연결하면 닫는다(듣는 쪽이 없는 것과 같다).
//! - Unix에서만 VM을 "띄운다". 다른 OS에서는 `launch`가 [`ExecutorError::Unsupported`]다.
//!
//! 모듈 층: `lib`, `process` → `guest` → `vm`.
//!
//! - 의존: `sandbox-model`, `sandbox-executor`, `executor-proto`, `sandbox-proto`, `sandbox-guest`(Unix)

use std::io;
use std::sync::{Arc, Mutex};

use sandbox_executor::{
    Capabilities, ChannelSpec, Executor, ExecutorError, GuestConn, VmConfig, VmHandle,
};
use sandbox_model::{Arch, BoxFuture, RootfsFormat};

mod guest;
#[cfg(unix)]
pub mod process;
mod vm;

pub use vm::{FakeVm, REASON_GUEST_SHUTDOWN, REASON_KILLED};

/// 가짜 실행기 (같은 프로세스 안).
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
            guest::start(&vm.control_channel.host_endpoint)
                .map(|v| Box::new(v) as Box<dyn VmHandle>)
        })
    }

    fn connect(&self, channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        guest::connect(channel)
    }
}

#[cfg(all(test, unix))]
mod tests;
