//! 실행기 추상 인터페이스.
//!
//! 실행 코어(`sandbox-runtime`)가 실행기에 요구하는 최소한만 정한다.
//! 특정 실행기의 API 모양을 따르지 않는다. 실행기마다 다른 기능은 숨기지 않고
//! [`Capabilities`]로 드러낸다.
//!
//! 실행기 하나는 VM 하나당 별도 프로세스 하나를 띄운다. 앱 프로세스 안에서 VM을 돌리지 않는다.
//!
//! - 의존: `sandbox-model`
//! - 구현: `executor-fake`, `executor-firecracker`

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use sandbox_model::{Arch, BoxFuture, RootfsFormat};

/// 실행기가 제공하는 기능.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// 실행기 이름. 오류 메시지와 `inspect`에 쓴다.
    pub name: &'static str,
    /// 게스트 아키텍처. 호스트와 같아야 한다.
    pub arch: Arch,
    /// 이 실행기와 게스트 커널 조합이 읽을 수 있는 rootfs 형식.
    pub rootfs_formats: Vec<RootfsFormat>,
    /// 실시간 폴더 공유(`MountKind::Bind`) 지원 여부.
    pub live_share: bool,
    /// 스냅샷 지원 여부.
    pub snapshot: bool,
    /// VM 하나에 붙일 수 있는 디스크 개수.
    pub max_disks: usize,
}

impl Capabilities {
    /// 이 형식의 rootfs를 받는지.
    pub fn supports_format(&self, format: RootfsFormat) -> bool {
        self.rootfs_formats.contains(&format)
    }
}

/// 디스크 용도.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskRole {
    /// 이미지 rootfs. 항상 읽기 전용.
    Rootfs,
    /// 샌드박스별 쓰기 가능 디스크.
    Scratch,
}

/// VM에 붙일 디스크.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    /// 호스트의 디스크 이미지 파일.
    pub path: PathBuf,
    /// 용도.
    pub role: DiskRole,
    /// 읽기 전용 여부.
    pub readonly: bool,
}

/// 실시간 폴더 공유 하나. `Capabilities::live_share`일 때만 쓴다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// 호스트 디렉터리.
    pub host_path: PathBuf,
    /// 게스트가 이 공유를 찾을 때 쓰는 이름.
    pub tag: String,
    /// 읽기 전용 여부.
    pub readonly: bool,
}

/// 호스트-게스트 통신 채널의 호스트 쪽 접속점. 게스트 쪽은 실행기가 정한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSpec {
    /// 호스트에서 접속할 경로(Unix 소켓 또는 named pipe).
    pub host_endpoint: PathBuf,
}

/// VM 하나를 띄우는 데 필요한 전부.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfig {
    /// 게스트 커널.
    pub kernel: PathBuf,
    /// 게스트 initrd.
    pub initrd: Option<PathBuf>,
    /// 디스크.
    pub disks: Vec<Disk>,
    /// vCPU 개수.
    pub vcpus: u8,
    /// 메모리(MiB).
    pub memory_mib: u32,
    /// 실시간 폴더 공유.
    pub shares: Vec<Share>,
    /// 제어 채널.
    pub control_channel: ChannelSpec,
}

impl VmConfig {
    /// 실행기 기능으로 받을 수 있는 설정인지 검사한다. 받을 수 없으면 다른 방식으로 바꾸지 않고 오류를 낸다.
    pub fn check(&self, caps: &Capabilities) -> Result<(), ExecutorError> {
        if self.vcpus == 0 {
            return Err(ExecutorError::Invalid("vcpus must be at least 1".into()));
        }
        if self.memory_mib == 0 {
            return Err(ExecutorError::Invalid(
                "memory_mib must be at least 1".into(),
            ));
        }
        if self.disks.len() > caps.max_disks {
            return Err(ExecutorError::Unsupported(format!(
                "{} disks requested, {} supports at most {}",
                self.disks.len(),
                caps.name,
                caps.max_disks
            )));
        }
        if !self.shares.is_empty() && !caps.live_share {
            return Err(ExecutorError::Unsupported(format!(
                "{} does not support live sharing",
                caps.name
            )));
        }
        Ok(())
    }
}

/// 실행기 오류.
#[derive(Debug)]
pub enum ExecutorError {
    /// 실행기가 지원하지 않는 요청.
    Unsupported(String),
    /// 잘못된 설정.
    Invalid(String),
    /// VM을 띄우지 못함.
    Launch(String),
    /// 입출력 오류.
    Io(io::Error),
    /// 아직 구현하지 않은 기능.
    Unimplemented(&'static str),
}

impl fmt::Display for ExecutorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecutorError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ExecutorError::Invalid(m) => write!(f, "invalid vm config: {m}"),
            ExecutorError::Launch(m) => write!(f, "failed to launch vm: {m}"),
            ExecutorError::Io(e) => write!(f, "io: {e}"),
            ExecutorError::Unimplemented(what) => write!(f, "not implemented yet: {what}"),
        }
    }
}

impl std::error::Error for ExecutorError {}

impl From<io::Error> for ExecutorError {
    fn from(e: io::Error) -> Self {
        ExecutorError::Io(e)
    }
}

/// 실행기.
pub trait Executor: Send + Sync {
    /// 이 실행기가 제공하는 기능.
    fn capabilities(&self) -> Capabilities;

    /// VM 하나를 띄운다. 구현은 먼저 [`VmConfig::check`]를 통과시켜야 한다.
    fn launch(&self, vm: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>>;
}

/// 띄운 VM 하나.
pub trait VmHandle: Send {
    /// 실행기 프로세스 ID. 고아 프로세스 정리에 쓴다.
    fn pid(&self) -> Option<u32>;

    /// 게스트에 종료를 요청하고 `timeout`까지 기다린다. 넘으면 강제로 끝낸다.
    fn shutdown(&mut self, timeout: Duration) -> BoxFuture<'_, Result<(), ExecutorError>>;

    /// 즉시 강제로 끝낸다.
    fn kill(&mut self) -> Result<(), ExecutorError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(live_share: bool, max_disks: usize) -> Capabilities {
        Capabilities {
            name: "test",
            arch: Arch::X86_64,
            rootfs_formats: vec![RootfsFormat::Erofs],
            live_share,
            snapshot: false,
            max_disks,
        }
    }

    fn vm() -> VmConfig {
        VmConfig {
            kernel: "k".into(),
            initrd: None,
            disks: vec![Disk {
                path: "r".into(),
                role: DiskRole::Rootfs,
                readonly: true,
            }],
            vcpus: 1,
            memory_mib: 256,
            shares: vec![],
            control_channel: ChannelSpec {
                host_endpoint: "c".into(),
            },
        }
    }

    #[test]
    fn accepts_supported_config() {
        assert!(vm().check(&caps(false, 2)).is_ok());
    }

    #[test]
    fn rejects_too_many_disks() {
        let mut v = vm();
        v.disks.push(v.disks[0].clone());
        assert!(matches!(
            v.check(&caps(false, 1)),
            Err(ExecutorError::Unsupported(_))
        ));
    }

    #[test]
    fn rejects_share_without_live_share() {
        let mut v = vm();
        v.shares.push(Share {
            host_path: "h".into(),
            tag: "t".into(),
            readonly: false,
        });
        assert!(matches!(
            v.check(&caps(false, 2)),
            Err(ExecutorError::Unsupported(_))
        ));
        assert!(v.check(&caps(true, 2)).is_ok());
    }

    #[test]
    fn rejects_zero_resources() {
        let mut v = vm();
        v.vcpus = 0;
        assert!(matches!(
            v.check(&caps(false, 2)),
            Err(ExecutorError::Invalid(_))
        ));
    }
}
