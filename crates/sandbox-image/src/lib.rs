//! 이미지 소스 추상 인터페이스.
//!
//! "이미지 참조를 받아 config와 레이어 목록을 내놓고, 레이어 내용을 내려받는다"는 것만 정한다.
//! OCI 자료형은 여기에 없다. OCI 해석은 구현체(`sandbox-oci`) 안에만 있다.
//! 그래서 OCI 적용 범위가 바뀌어도 이 trait을 쓰는 쪽(`sandbox-manage`)은 바뀌지 않는다.
//!
//! - 의존: `sandbox-model`
//! - 구현: `sandbox-oci`

use std::fmt;
use std::io::{self, Write};

use sandbox_model::{Arch, BoxFuture, Digest, ImageConfig, ImageId, Layer, ParseError};

pub use sandbox_model::Compression;

/// 받을 이미지의 플랫폼. OS는 항상 linux다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// 아키텍처.
    pub arch: Arch,
}

/// 해석이 끝난 이미지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedImage {
    /// 이미지 ID (플랫폼별 manifest digest).
    pub id: ImageId,
    /// 정규화된 참조(예: `docker.io/library/alpine:latest`). 태그 기록의 키가 된다.
    pub reference: String,
    /// 레이어를 내려받을 때 다시 쓰는 저장소 위치(예: `docker.io/library/alpine`).
    pub source: String,
    /// 플랫폼.
    pub platform: Platform,
    /// 실행 기본값.
    pub config: ImageConfig,
    /// 아래 레이어부터 순서대로.
    pub layers: Vec<Layer>,
    /// 이미지에 있었지만 적용하지 않은 필드. `image inspect`에서 보여준다.
    pub ignored_fields: Vec<String>,
}

/// 이미지 소스 오류.
#[derive(Debug)]
pub enum ImageError {
    /// 참조 형식이 잘못됨.
    Reference(ParseError),
    /// 이미지가 없음.
    NotFound(String),
    /// 요청한 플랫폼의 이미지가 없음.
    PlatformNotFound {
        /// 참조.
        reference: String,
        /// 요청한 아키텍처.
        arch: Arch,
    },
    /// 지원하지 않는 형식.
    Unsupported(String),
    /// 형식이 잘못된 문서.
    Invalid(String),
    /// 레지스트리와의 통신 오류.
    Transport(String),
    /// digest가 맞지 않음.
    Integrity(String),
    /// 입출력 오류.
    Io(io::Error),
    /// 아직 구현하지 않은 기능.
    Unimplemented(&'static str),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImageError::Reference(e) => e.fmt(f),
            ImageError::NotFound(r) => write!(f, "image not found: {r}"),
            ImageError::PlatformNotFound { reference, arch } => write!(
                f,
                "image {reference} has no linux/{} variant",
                arch.oci_name()
            ),
            ImageError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ImageError::Invalid(m) => write!(f, "invalid: {m}"),
            ImageError::Transport(m) => write!(f, "registry: {m}"),
            ImageError::Integrity(m) => write!(f, "digest mismatch: {m}"),
            ImageError::Io(e) => write!(f, "io: {e}"),
            ImageError::Unimplemented(what) => write!(f, "not implemented yet: {what}"),
        }
    }
}

impl std::error::Error for ImageError {}

impl From<io::Error> for ImageError {
    fn from(e: io::Error) -> Self {
        ImageError::Io(e)
    }
}

/// 이미지 소스.
pub trait ImageSource: Send + Sync {
    /// 참조를 해석해 플랫폼에 맞는 이미지를 고른다. 태그는 이 시점에 digest로 고정된다.
    fn resolve<'a>(
        &'a self,
        reference: &'a str,
        platform: Platform,
    ) -> BoxFuture<'a, Result<ResolvedImage, ImageError>>;

    /// blob 하나를 `sink`에 쓴다. 쓴 바이트 수를 돌려준다.
    ///
    /// digest 검증은 받는 쪽(`sandbox-manage`)이 한다. 소스를 믿지 않기 때문이다.
    fn fetch_blob<'a>(
        &'a self,
        source: &'a str,
        digest: &'a Digest,
        sink: &'a mut (dyn Write + Send),
    ) -> BoxFuture<'a, Result<u64, ImageError>>;
}
