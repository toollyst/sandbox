//! cache_dir의 레이아웃, 잠금, 읽기.
//!
//! - 읽기 API는 누구나 쓴다.
//! - 쓰기 API([`write`](mod@write))는 `write` feature 뒤에 있다. 관리(`sandbox-manage`)만 이 feature를 켠다.
//!   실행(`sandbox-runtime`)은 켜지 않으므로 캐시를 바꿀 수 없다. `cargo xtask check-deps`가 검사한다.
//!
//! cache_dir은 설정으로 받는다. 여러 프로세스가 같은 경로를 쓰면 캐시가 공유된다.
//! 그래서 모든 쓰기는 임시 파일에 쓴 뒤 rename하고, 이미지마다 잠금 파일을 둔다.
//!
//! ```text
//! <cache_dir>/
//!   blobs/sha256/<hex>          레이어 (내용 주소, digest 검증 후에만 놓인다)
//!   images/<hex>/image.json     이미지 기록 ([`ImageRecord`])
//!   images/<hex>/rootfs.<형식>  형식별 읽기 전용 rootfs (prepare한 것만)
//!   images/<hex>/.lock          사용 중 보호용 잠금 파일
//!   guest/<version>/<arch>/     게스트 기반 이미지 (커널, 게스트 init)
//!   refs.json                   정규화된 참조 → 이미지 ID
//!   tmp/                        prepare 작업 공간 (끝나면 지운다)
//!   .lock                       refs.json 갱신용 잠금
//! ```
//!
//! 파일 이름에는 digest의 hex만 쓴다. `:`는 Windows 파일 이름에 쓸 수 없다.
//!
//! **사용 중 보호**: 샌드박스가 실행 중인 동안 실행기 프로세스를 붙잡고 있는 쪽이
//! [`Store::lease`]로 이미지 잠금 파일에 공유 잠금을 건다. 이미지를 지우는 쪽은 배타 잠금을
//! 잡아야 지울 수 있다. 잠금이 cache_dir에 걸리므로 다른 data_dir의 샌드박스도 보호된다.
//!
//! - 의존: `sandbox-model`

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sandbox_model::{Arch, Digest, ImageConfig, ImageId, Layer, RootfsFormat};

pub mod digest;
#[cfg(feature = "write")]
pub mod write;

/// `image.json`에 저장하는 이미지 기록.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRecord {
    /// 이미지 ID.
    pub id: ImageId,
    /// 받을 때의 정규화된 참조.
    pub reference: String,
    /// 레이어를 내려받은 저장소 위치.
    pub source: String,
    /// 아키텍처.
    pub arch: Arch,
    /// 실행 기본값.
    pub config: ImageConfig,
    /// 아래 레이어부터 순서대로.
    pub layers: Vec<Layer>,
    /// 이미지에 있었지만 적용하지 않은 필드.
    pub ignored_fields: Vec<String>,
}

impl ImageRecord {
    /// 레이어 압축 크기의 합.
    pub fn compressed_size(&self) -> u64 {
        self.layers.iter().map(|l| l.size).sum()
    }
}

/// 저장소 읽기 오류.
#[derive(Debug)]
pub enum StoreError {
    /// 이미지가 없음.
    ImageNotFound(ImageId),
    /// 기록 파일이 깨짐.
    Corrupt {
        /// 파일.
        path: PathBuf,
        /// 이유.
        reason: String,
    },
    /// 입출력 오류.
    Io(io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::ImageNotFound(id) => write!(f, "image {id} not found"),
            StoreError::Corrupt { path, reason } => {
                write!(f, "corrupt record {}: {reason}", path.display())
            }
            StoreError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        StoreError::Io(e)
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, StoreError> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| StoreError::Corrupt {
        path: path.to_owned(),
        reason: e.to_string(),
    })
}

/// cache_dir 안의 경로 규칙.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheLayout {
    root: PathBuf,
}

impl CacheLayout {
    /// cache_dir을 받는다.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// cache_dir.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// blob 하나.
    pub fn blob(&self, digest: &Digest) -> PathBuf {
        self.root.join("blobs").join("sha256").join(digest.hex())
    }

    /// 이미지 디렉터리.
    pub fn image_dir(&self, id: &ImageId) -> PathBuf {
        self.root.join("images").join(id.digest().hex())
    }

    /// 이미지 기록.
    pub fn image_record(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join("image.json")
    }

    /// 형식별 rootfs.
    pub fn rootfs(&self, id: &ImageId, format: RootfsFormat) -> PathBuf {
        self.image_dir(id)
            .join(format!("rootfs.{}", format.as_str()))
    }

    /// 이미지 잠금 파일.
    pub fn image_lock(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join(".lock")
    }

    /// 게스트 기반 이미지 디렉터리.
    pub fn guest_dir(&self, version: &str, arch: Arch) -> PathBuf {
        self.root.join("guest").join(version).join(arch.oci_name())
    }

    /// 태그 기록.
    pub fn refs(&self) -> PathBuf {
        self.root.join("refs.json")
    }

    /// 이미지 디렉터리들의 부모.
    pub fn images_dir(&self) -> PathBuf {
        self.root.join("images")
    }

    /// 작업 공간.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// refs.json 갱신용 잠금 파일.
    pub fn cache_lock(&self) -> PathBuf {
        self.root.join(".lock")
    }
}

/// 잠금을 잡지 못한 이유.
#[derive(Debug)]
pub enum LeaseError {
    /// 이미지가 없음 (기다리는 동안 지워진 경우 포함).
    ImageNotFound,
    /// 이미지는 있지만 그 형식의 rootfs가 없음.
    RootfsNotFound,
    /// 입출력 오류.
    Io(io::Error),
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseError::ImageNotFound => f.write_str("image not found"),
            LeaseError::RootfsNotFound => f.write_str("rootfs not prepared"),
            LeaseError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// 살아 있는 동안 이미지 삭제를 막는 공유 잠금. rootfs 존재는 보장하지 않는다.
#[derive(Debug)]
pub struct ImageGuard {
    _lock: File,
}

/// 살아 있는 동안 이미지 삭제를 막는 공유 잠금.
#[derive(Debug)]
pub struct RootfsLease {
    _lock: File,
    rootfs: PathBuf,
}

impl RootfsLease {
    /// 잠금으로 보호되는 rootfs 파일.
    pub fn rootfs(&self) -> &Path {
        &self.rootfs
    }
}

/// cache_dir 읽기.
#[derive(Debug, Clone)]
pub struct Store {
    layout: CacheLayout,
}

impl Store {
    /// cache_dir을 연다. 디렉터리를 만들지 않는다.
    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self {
            layout: CacheLayout::new(root),
        }
    }

    /// 경로 규칙.
    pub fn layout(&self) -> &CacheLayout {
        &self.layout
    }

    /// 이미지를 받아 두었는지.
    pub fn has_image(&self, id: &ImageId) -> bool {
        self.layout.image_record(id).is_file()
    }

    /// 그 형식의 rootfs를 준비해 두었는지.
    pub fn has_rootfs(&self, id: &ImageId, format: RootfsFormat) -> bool {
        self.layout.rootfs(id, format).is_file()
    }

    /// 이미지 기록을 읽는다.
    pub fn image(&self, id: &ImageId) -> Result<ImageRecord, StoreError> {
        let path = self.layout.image_record(id);
        if !path.is_file() {
            return Err(StoreError::ImageNotFound(id.clone()));
        }
        read_json(&path)
    }

    /// 받아 둔 모든 이미지. ID 순서.
    ///
    /// 기록이 없는 디렉터리(받는 중이거나 지우는 중)는 건너뛴다.
    pub fn images(&self) -> Result<Vec<ImageRecord>, StoreError> {
        let dir = self.layout.images_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let path = entry?.path().join("image.json");
            if path.is_file() {
                out.push(read_json::<ImageRecord>(&path)?);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// 정규화된 참조 → 이미지 ID.
    pub fn refs(&self) -> Result<BTreeMap<String, ImageId>, StoreError> {
        let path = self.layout.refs();
        if !path.is_file() {
            return Ok(BTreeMap::new());
        }
        read_json(&path)
    }

    /// 이미지에 공유 잠금을 건다. rootfs 존재는 확인하지 않는다.
    ///
    /// rootfs를 만드는 동안 이미지가 지워지지 않게 할 때 쓴다.
    pub fn guard(&self, id: &ImageId) -> Result<ImageGuard, LeaseError> {
        let lock = self.lock_shared(id)?;
        Ok(ImageGuard { _lock: lock })
    }

    fn lock_shared(&self, id: &ImageId) -> Result<File, LeaseError> {
        let lock = match File::open(self.layout.image_lock(id)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(LeaseError::ImageNotFound);
            }
            Err(e) => return Err(LeaseError::Io(e)),
        };
        lock.lock_shared().map_err(LeaseError::Io)?;
        if !self.has_image(id) {
            return Err(LeaseError::ImageNotFound);
        }
        Ok(lock)
    }

    /// rootfs에 공유 잠금을 건다. 이미지를 지우는 쪽이 배타 잠금을 쥐고 있으면 풀릴 때까지 기다린다.
    ///
    /// 잠금을 잡은 뒤에 존재를 다시 확인한다. 기다리는 동안 지워졌으면 `ImageNotFound`다.
    pub fn lease(&self, id: &ImageId, format: RootfsFormat) -> Result<RootfsLease, LeaseError> {
        let lock = self.lock_shared(id)?;
        let rootfs = self.layout.rootfs(id, format);
        if !rootfs.is_file() {
            return Err(LeaseError::RootfsNotFound);
        }
        Ok(RootfsLease {
            _lock: lock,
            rootfs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> ImageId {
        ImageId::new(Digest::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap())
    }

    #[test]
    fn layout_uses_hex_only_file_names() {
        let l = CacheLayout::new("/c");
        let id = image();
        let hex = id.digest().hex().to_owned();
        assert_eq!(
            l.rootfs(&id, RootfsFormat::Erofs),
            Path::new("/c/images").join(&hex).join("rootfs.erofs")
        );
        assert_eq!(l.blob(id.digest()), Path::new("/c/blobs/sha256").join(&hex));
        let dir_name = l.image_dir(&id);
        assert!(
            !dir_name
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(':')
        );
    }

    #[test]
    fn lease_on_missing_image() {
        let store = Store::open(std::env::temp_dir().join("sandbox-store-missing"));
        assert!(matches!(
            store.lease(&image(), RootfsFormat::Erofs),
            Err(LeaseError::ImageNotFound)
        ));
    }
}
