//! 관리 코어 오류. 크레이트의 가장 아래 모듈이다. 다른 모듈은 여기에 의존하고, 여기는 아무 모듈에도 의존하지 않는다.

use std::fmt;
use std::io;

use sandbox_image::ImageError;
use sandbox_model::{ImageId, RootfsFormat};
use sandbox_rootfs::RootfsError;
use sandbox_store::StoreError;

/// 관리 코어 오류.
#[derive(Debug)]
pub enum ManageError {
    /// 이미지 소스 오류.
    Source(ImageError),
    /// 그 형식의 writer가 조립되지 않음.
    BuilderMissing(RootfsFormat),
    /// 이미지가 없음.
    ImageNotFound(ImageId),
    /// 참조가 없음.
    RefNotFound(String),
    /// 실행 중인 샌드박스가 쓰고 있음.
    InUse(ImageId),
    /// 받은 데이터의 digest가 맞지 않음.
    Integrity(String),
    /// 레이어 tar를 읽을 수 없음.
    Layer {
        /// 레이어 번호(아래부터 0).
        index: usize,
        /// 이유.
        reason: String,
    },
    /// rootfs 생성 오류.
    Rootfs(RootfsError),
    /// 저장소 오류.
    Store(StoreError),
    /// 입출력 오류.
    Io(io::Error),
}

impl fmt::Display for ManageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManageError::Source(e) => e.fmt(f),
            ManageError::BuilderMissing(format) => write!(f, "no {format} rootfs builder"),
            ManageError::ImageNotFound(id) => write!(f, "image {id} not found"),
            ManageError::RefNotFound(r) => write!(f, "no image tagged {r}"),
            ManageError::InUse(id) => write!(f, "image {id} is used by a running sandbox"),
            ManageError::Integrity(m) => write!(f, "digest mismatch: {m}"),
            ManageError::Layer { index, reason } => write!(f, "layer {index}: {reason}"),
            ManageError::Rootfs(e) => e.fmt(f),
            ManageError::Store(e) => e.fmt(f),
            ManageError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for ManageError {}

impl From<io::Error> for ManageError {
    fn from(e: io::Error) -> Self {
        ManageError::Io(e)
    }
}

impl From<StoreError> for ManageError {
    fn from(e: StoreError) -> Self {
        match e {
            StoreError::ImageNotFound(id) => ManageError::ImageNotFound(id),
            e => ManageError::Store(e),
        }
    }
}

impl From<RootfsError> for ManageError {
    fn from(e: RootfsError) -> Self {
        ManageError::Rootfs(e)
    }
}
