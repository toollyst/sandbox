//! 이미지 소스와 올릴 곳의 추상 인터페이스.
//!
//! - [`ImageSource`]: "이미지 참조를 받아 config와 레이어 목록을 내놓고, 레이어 내용을 내려받는다".
//! - [`ImageDestination`]: "blob을 올리고(이미 있으면 건너뛰고), 마지막에 manifest를 이름에 붙인다".
//!   무엇을 어떤 순서로 올릴지는 쓰는 쪽(`sbx-manage`)이 정한다. 구현체는 blob 하나, manifest 하나의 규약만 안다.
//!
//! OCI 자료형은 여기에 없다. OCI 해석과 레지스트리 규약은 구현체(`sbx-oci`) 안에만 있다.
//! 그래서 OCI 적용 범위가 바뀌어도 이 trait을 쓰는 쪽(`sbx-manage`)은 바뀌지 않는다.
//! manifest와 config는 바이트 그대로 오간다([`Documents`]): 해석하지 않고 그대로 올리므로 digest가 바뀌지 않는다.
//!
//! - 의존: `sbx-model`
//! - 구현: `sbx-oci`

use std::fmt;
use std::io::{self, Read, Write};

use sbx_model::{Arch, BoxFuture, Digest, ImageConfig, ImageId, Layer, ParseError};

pub use sbx_model::Compression;

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
    /// 받은 문서 그대로(digest를 대조한 것). `sbx save`가 같은 이미지 ID로 다시 내보내는 데 쓴다.
    pub documents: Option<Documents>,
}

/// 플랫폼 manifest와 image config의 원래 바이트.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Documents {
    /// 플랫폼 manifest. 이 바이트의 sha256이 이미지 ID다.
    pub manifest: Vec<u8>,
    /// manifest의 미디어 형식(OCI 또는 Docker v2).
    pub manifest_media_type: String,
    /// image config.
    pub config: Vec<u8>,
    /// image config의 digest(manifest에 적힌 것, 대조함).
    pub config_digest: Digest,
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
    /// digest 검증은 받는 쪽(`sbx-manage`)이 한다. 소스를 믿지 않기 때문이다.
    fn fetch_blob<'a>(
        &'a self,
        source: &'a str,
        digest: &'a Digest,
        sink: &'a mut (dyn Write + Send),
    ) -> BoxFuture<'a, Result<u64, ImageError>>;
}

/// 올릴 곳. [`ImageDestination::target`]이 만든다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushTarget {
    /// 정규화된 참조(태그 포함, 예: `docker.io/user/app:1`). manifest가 이 태그로 붙는다.
    pub reference: String,
    /// blob을 같은 레지스트리의 이 저장소(`레지스트리/저장소`)에서 끌어올 수 있다(cross-repository mount).
    /// 구현체가 정한다: 같은 레지스트리의 다른 저장소일 때만 있다.
    pub mount_from: Option<String>,
}

/// blob 하나를 어떻게 올렸는지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobPush {
    /// 레지스트리에 이미 있었다. 아무것도 보내지 않았다.
    Exists,
    /// 같은 레지스트리의 다른 저장소(`레지스트리/저장소`)에서 끌어왔다. 내용은 보내지 않았다.
    Mounted(String),
    /// 내용을 보냈다.
    Uploaded,
}

/// 올릴 blob의 내용. 처음부터든 중간부터든 다시 읽을 수 있어야 한다(인증 뒤 다시 보내기, 나눠 보내기).
pub trait BlobContent: Send + Sync {
    /// 바이트 수.
    fn size(&self) -> u64;

    /// `offset`부터 끝까지 읽는다.
    fn open(&self, offset: u64) -> io::Result<Box<dyn Read + Send + Sync>>;
}

/// 메모리의 바이트(config 같은 작은 문서).
impl BlobContent for Vec<u8> {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn open(&self, offset: u64) -> io::Result<Box<dyn Read + Send + Sync>> {
        let start = usize::try_from(offset)
            .ok()
            .filter(|o| *o <= self.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset past the end"))?;
        Ok(Box::new(io::Cursor::new(self[start..].to_vec())))
    }
}

/// 이미지를 올릴 곳.
///
/// 순서는 쓰는 쪽이 지킨다: 모든 blob(레이어, config)을 [`push_blob`](Self::push_blob)으로 올린 뒤에야
/// [`push_manifest`](Self::push_manifest)를 부른다. 그래서 중간에 실패하면 이름(태그)은 바뀌지 않는다.
pub trait ImageDestination: Send + Sync {
    /// 참조를 해석해 올릴 곳을 정한다. 레지스트리에 가지 않는다. 받지 않는 이름(digest만 있는 참조, 차단된
    /// 레지스트리 등)은 여기서 오류다. `blobs_from`은 blob을 받아 온 곳(`레지스트리/저장소`)이고 mount의 단서다.
    fn target(&self, reference: &str, blobs_from: Option<&str>) -> Result<PushTarget, ImageError>;

    /// blob 하나를 올린다. 이미 있으면 보내지 않고, 끌어올 수 있으면 끌어오고, 아니면 내용을 보낸다.
    /// `progress`에는 보낸 바이트 수(이 blob 안에서)를 알린다.
    fn push_blob<'a>(
        &'a self,
        target: &'a PushTarget,
        digest: &'a Digest,
        content: &'a dyn BlobContent,
        progress: &'a (dyn Fn(u64) + Sync),
    ) -> BoxFuture<'a, Result<BlobPush, ImageError>>;

    /// manifest를 바이트 그대로 태그에 붙인다. `media_type`은 manifest의 미디어 형식이다.
    fn push_manifest<'a>(
        &'a self,
        target: &'a PushTarget,
        media_type: &'a str,
        manifest: &'a [u8],
    ) -> BoxFuture<'a, Result<(), ImageError>>;
}
