//! Firecracker 실행기 어댑터. 첫 번째 실제 실행기다.
//!
//! 이 크레이트는 `sandbox-executor` trait을 Firecracker 프로세스 제어로 옮기는 일만 한다.
//! Firecracker 고유의 API 모양은 이 크레이트 밖으로 나가지 않는다.
//!
//! capability는 이 어댑터가 **지금 구현한 것**을 기준으로 적는다. Firecracker가 지원하더라도
//! 어댑터가 아직 다루지 않는 기능(스냅샷 등)은 `false`다.
//!
//! - 의존: `sandbox-model`, `sandbox-executor`

use std::path::PathBuf;

use sandbox_executor::{
    Capabilities, ChannelSpec, Executor, ExecutorError, GuestConn, VmConfig, VmHandle,
};
use sandbox_model::{Arch, BoxFuture, RootfsFormat};

/// Firecracker 실행기.
#[derive(Debug, Clone)]
pub struct FirecrackerExecutor {
    binary: PathBuf,
    arch: Arch,
}

impl FirecrackerExecutor {
    /// Firecracker 바이너리 경로로 만든다. 지원하지 않는 호스트 아키텍처면 오류.
    pub fn new(binary: impl Into<PathBuf>) -> Result<Self, ExecutorError> {
        let arch = Arch::host().ok_or_else(|| {
            ExecutorError::Unsupported("host architecture is not supported".into())
        })?;
        Ok(Self {
            binary: binary.into(),
            arch,
        })
    }

    /// Firecracker 바이너리 경로.
    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }
}

impl Executor for FirecrackerExecutor {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "firecracker",
            arch: self.arch,
            // 레포에서 빌드하는 게스트 커널은 erofs를 켠다.
            rootfs_formats: vec![RootfsFormat::Erofs],
            // Firecracker에는 virtio-fs가 없다.
            live_share: false,
            snapshot: false,
            // 지금 붙이는 디스크: rootfs, scratch.
            max_disks: 2,
        }
    }

    fn launch(&self, vm: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>> {
        Box::pin(async move {
            vm.check(&self.capabilities())?;
            Err(ExecutorError::Unimplemented("firecracker launch"))
        })
    }

    fn connect(&self, _channel: &ChannelSpec) -> std::io::Result<Box<dyn GuestConn>> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "firecracker connect is not implemented yet",
        ))
    }
}
