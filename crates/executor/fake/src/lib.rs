//! 테스트용 가짜 실행기. 두 가지로 쓴다.
//!
//! - [`FakeExecutor`]: 같은 프로세스 안의 `Executor` 구현. 실행 코어 단위 테스트용.
//! - `sbx-executor-fake` 실행 파일(unix에서 `process::main`): `sbx-executor-proto` 규약을 그대로 말하는 외부 실행기.
//!   `sbx-executor-process` 어댑터와 CLI가 실제 실행기와 같은 경로로 동작하는지 검증한다.
//!
//! 둘 다 VM을 띄우지 않는다. 대신 채널 위치(제어 소켓)에 Unix 소켓을 열고, 규약의 vsock 방식
//! (`CONNECT <포트>`)으로 연결을 받은 뒤 `sbx-guest`의 연결 처리(`server::serve`)를 그대로 돌린다.
//! 그래서 실행 코어는 실제 게스트와 같은 메시지로 exec, 파일 복사, 종료 요청을 주고받는다.
//!
//! 실제 VM과 다른 점 (테스트를 쓸 때 알아야 할 것)
//! - 프로세스는 호스트에서, 지금 사용자 권한으로 돈다. rootfs(층 디스크들)와 scratch 디스크는 쓰지 않는다.
//!   디스크는 [`MAX_DISKS`]개까지 받는다(실행 파일은 `SBX_FAKE_MAX_DISKS`로 바꾼다).
//!   그래서 실행 사용자는 지금 사용자여야 하고, 경로는 호스트 경로다.
//! - 사용자 이름은 호스트의 `/etc/passwd`로 해석한다.
//! - 게스트 처리가 띄우는 프로세스는 저마다 자기 프로세스 그룹에서 돈다. VM을 끝내면(`Kill`, 표준 입력 닫힘, 게스트의
//!   종료 요청, 핸들 버리기) 끝났다고 알리기 **전에** 그 그룹들을 모두 SIGKILL로 끝내고 열린 게스트 연결을 닫는다
//!   (실제 VM에서 전원이 꺼지는 것과 같은 결과). 그래서 실행기 프로세스가 곧바로 끝나도 호스트에 고아가 남지 않는다.
//!   그룹을 떠난 프로세스(`setsid`로 떼어 낸 데몬)는 남을 수 있다.
//!   [`FakeExecutor::leaked`]와 실행 파일의 `SBX_FAKE_PROCS`로 테스트가 남은 프로세스를 찾는다.
//! - 게스트 포트([`sbx_guest_proto::GUEST_PORT`]) 외의 포트로 연결하면 닫는다(듣는 쪽이 없는 것과 같다).
//! - 공유 폴더(`Share`)는 마운트하지 않고 대상 경로에 호스트 경로를 가리키는 심볼릭 링크를 둔다
//!   (대상이 빈 디렉터리면 바꾼다). 읽기 전용은 강제하지 않는다.
//! - Unix에서만 VM을 "띄운다". 다른 OS에서는 `launch`가 [`ExecutorError::Unsupported`]다.
//! - 빌드 층(`OpenLayer`, `ExportLayer`)은 overlay 대신 scratch 디스크 옆 디렉터리 하나([`build_root`])를 두 번 훑어
//!   견준다. 규칙과 실제 overlay와 다른 점은 `layers` 모듈에 있다. 첫 층을 열 때 그 디렉터리가 없으면 받은 층
//!   디스크(EROFS)를 펼치고, 그 뒤로는 연결이 보내는 절대 경로를 그 디렉터리 아래로 옮긴다(`rootdir` 모듈).
//! - 루프백 DNS는 53번 대신 `127.0.0.1`의 빈 포트에서 듣고, 투명 TCP(`Transparent`)는 경로 표를 바꾸지 않는다.
//!   가짜 주소에서 듣는 대신 `127.0.0.1`의 빈 포트에서 듣는다. 어디서 듣는지는 [`net_dir`]의 파일에 적는다.
//!   호스트에서 도는 프로그램은 그 DNS와 가짜 주소를 모르므로, 테스트가 그 파일을 읽고 직접 묻고 붙는다.
//! - 볼륨 디스크(`Volume`)는 포맷하지 않고 디스크 파일 옆 디렉터리([`volume_content`])로 흉내 낸다. 대상 경로에 그
//!   디렉터리를 가리키는 링크를 둔다. 빈 볼륨이면 그 전에 대상 경로의 내용(이미지 내용 노릇)으로 채우고 대상을
//!   지운다. 읽기 전용은 강제하지 않는다(디스크를 읽기 전용으로 받았는지만 본다).
//! - 게스트 시계(`ClockSync`)는 호스트 시계를 건드리지 않고 scratch 디스크 옆 파일 하나([`clock_file`])의 "호스트와의
//!   차이"를 바꾼다. 테스트는 그 파일로 게스트 시계를 틀고 맞았는지 본다. 규칙은 [`clock`] 모듈에 있다.
//!
//! 모듈 층: `lib`, `process` → `guest`, `probe`, `leftover` → `layers`, `vm`, `netdir`, `clock` → `rootdir`.
//! `probe`와 `leftover`는 테스트가 쓰는 도구다.
//!
//! - 의존: `sbx-model`, `sbx-executor`, `sbx-executor-proto`, `sbx-guest-proto`, `sbx-guest`(Unix),
//!   `sbx-rootfs`(Unix, 층 디스크 읽기)

use std::io;
use std::sync::{Arc, Mutex};

use sbx_executor::{
    Capabilities, ChannelSpec, Executor, ExecutorError, GuestConn, VmConfig, VmHandle,
};
use sbx_model::{Arch, BoxFuture, RootfsFormat};

pub mod clock;
mod guest;
#[cfg(unix)]
mod layers;
#[cfg(unix)]
pub mod leftover;
mod netdir;
pub mod probe;
#[cfg(unix)]
pub mod process;
#[cfg(unix)]
mod rootdir;
mod vm;

pub use clock::clock_file;
pub use netdir::{NET_DNS, NET_ROUTES, net_dir};
pub use vm::{FakeVm, REASON_GUEST_SHUTDOWN, REASON_KILLED, volume_content};

/// 기본으로 붙일 수 있는 디스크 수. Firecracker 어댑터와 같다(층 디스크 7개와 scratch).
pub const MAX_DISKS: usize = 8;

/// 빌드 층 흉내의 뿌리: scratch 디스크 파일 이름 뒤에 `.root`를 붙인 디렉터리. 첫 `OpenLayer`가 없으면 만든다.
/// 그 전에 둔 내용이 이미지 rootfs 노릇을 한다.
pub fn build_root(scratch: &std::path::Path) -> std::path::PathBuf {
    let mut s = scratch.as_os_str().to_owned();
    s.push(".root");
    s.into()
}

/// 띄운 VM 하나의 상태와 그 VM의 프로세스 그룹들([`FakeExecutor::leaked`]).
type Launched = (Arc<vm::Life>, Arc<vm::Procs>);

/// 가짜 실행기 (같은 프로세스 안).
#[derive(Debug, Clone)]
pub struct FakeExecutor {
    caps: Capabilities,
    launches: Arc<Mutex<Vec<VmConfig>>>,
    vms: Arc<Mutex<Vec<Launched>>>,
}

impl FakeExecutor {
    /// capability를 정해서 만든다.
    pub fn new(caps: Capabilities) -> Self {
        Self {
            caps,
            launches: Arc::default(),
            vms: Arc::default(),
        }
    }

    /// 흔한 기본값: 호스트 아키텍처, 모든 rootfs 형식, 실시간 공유, 디스크 [`MAX_DISKS`]개.
    pub fn basic() -> Self {
        Self::new(Capabilities {
            name: "fake",
            arch: Arch::host().unwrap_or(Arch::X86_64),
            rootfs_formats: RootfsFormat::ALL.to_vec(),
            live_share: true,
            custom_kernel: true,
            snapshot: false,
            max_disks: MAX_DISKS,
            volumes: true,
        })
    }

    /// 지금까지 받은 VM 설정.
    pub fn launches(&self) -> Vec<VmConfig> {
        self.launches.lock().map(|l| l.clone()).unwrap_or_default()
    }

    fn launched(&self) -> Vec<Launched> {
        self.vms.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// 이미 끝난 VM의 프로세스 그룹 가운데 아직 프로세스가 있는 것(좀비 포함). VM을 끝내면 그 그룹들을 SIGKILL로
    /// 끝내므로 비어 있어야 한다. 막 끝낸 VM의 프로세스는 잠깐 남아 있을 수 있으니 부르는 쪽이 조금 기다려 본다.
    pub fn leaked(&self) -> Vec<u32> {
        self.launched()
            .into_iter()
            .filter(|(life, _)| life.reason().is_some())
            .flat_map(|(_, procs)| procs.alive())
            .collect()
    }

    /// 띄운 모든 VM(끝났든 아니든)의 프로세스 그룹을 SIGKILL로 끝낸다. 프로세스가 있던 그룹들을 돌려준다.
    /// 테스트가 패닉한 뒤 정리에 쓴다.
    pub fn kill_everything(&self) -> Vec<u32> {
        self.launched()
            .into_iter()
            .flat_map(|(_, procs)| procs.kill_all())
            .collect()
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
            // 게스트 흉내는 Unix 소켓이 있어야 한다(`guest`).
            #[cfg(not(unix))]
            return Err(ExecutorError::Unsupported(
                "the fake guest needs Unix sockets".into(),
            ));
            #[cfg(unix)]
            {
                let shares = vm
                    .shares
                    .iter()
                    .map(|s| (s.tag.clone(), s.host_path.clone()))
                    .collect();
                let scratch = vm
                    .disks
                    .iter()
                    .find(|d| d.role == sbx_executor::DiskRole::Scratch);
                let dirs = scratch.map(|d| (build_root(&d.path), net_dir(&d.path)));
                let clock = scratch.map(|d| clock_file(&d.path));
                let layer_disks = vm
                    .disks
                    .iter()
                    .filter(|d| d.role == sbx_executor::DiskRole::Rootfs)
                    .map(|d| d.path.clone())
                    .collect();
                let disks = vm
                    .disks
                    .iter()
                    .map(|d| vm::FakeDisk {
                        path: d.path.clone(),
                        volume: d.role == sbx_executor::DiskRole::Volume,
                        readonly: d.readonly,
                    })
                    .collect();
                let procs = Arc::new(vm::Procs::new(None));
                let v = guest::start(
                    &vm.control_channel.host_endpoint,
                    shares,
                    dirs,
                    layer_disks,
                    clock,
                    disks,
                    procs.clone(),
                )?;
                if let Ok(mut l) = self.vms.lock() {
                    l.push((v.life.clone(), procs));
                }
                Ok(Box::new(v) as Box<dyn VmHandle>)
            }
        })
    }

    fn connect(&self, channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        guest::connect(channel)
    }
}

#[cfg(all(test, unix))]
mod tests;
