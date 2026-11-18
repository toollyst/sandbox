//! cache_dir의 레이아웃, 잠금, 읽기.
//!
//! - 읽기 API는 누구나 쓴다.
//! - 쓰기 API([`write`](mod@write))는 `write` feature 뒤에 있다. 관리(`sbx-manage`)만 이 feature를 켠다.
//!   실행(`sbx-runtime`)은 켜지 않으므로 캐시를 바꿀 수 없다. `cargo xtask check-deps`가 검사한다.
//!
//! cache_dir은 설정으로 받는다. 여러 프로세스가 같은 경로를 쓰면 캐시가 공유된다.
//! 그래서 모든 쓰기는 임시 파일에 쓴 뒤 rename하고, 이미지마다 잠금 파일을 둔다.
//!
//! ```text
//! <cache_dir>/
//!   blobs/sha256/<hex>          레이어 blob (내용 주소, digest 검증 후에만 놓인다). save와 prepare가 읽는다
//!   blobs/.lock                 정리(blob, 층, 합친 것)와 pull 표시 생성, prepare 시작의 상호 배제
//!   layers/<hex>/layer.<형식>   층 하나의 디스크 (diff_id, 그 층만으로 만든 overlay 아래층, [`stack`])
//!   layers/<hex>/needs-lower    층이 혼자 설 수 없다는 표시. 내용은 그 이유 (층 디스크 대신)
//!   layers/<hex>/.lock          그 층을 만드는 동안의 배타 잠금 (같은 층을 한 번만 만든다)
//!   merged/<hex>/rootfs.<형식>  아래 층들을 합친 디스크 (chain ID, 지워도 되는 캐시, [`stack`])
//!   merged/<hex>/.lock          그것을 만드는 동안의 배타 잠금
//!   images/<hex>/image.json     이미지 기록 ([`ImageRecord`]). 층 목록이 곧 준비한 상태다
//!   images/<hex>/kernel         이미지가 담은 커널 (`ImageConfig::KERNEL_LABEL`이 있는 이미지만, prepare가 꺼낸다)
//!   images/<hex>/.lock          사용 중 보호용 잠금 파일
//!   guest/<version>/<arch>/initramfs  게스트 init. version은 `sha256-<hex>`(sbx에 내장된 것을 관리 코어가
//!                               꺼낸 것, 내용 digest) 또는 게스트 프로토콜 버전(사용자가 직접 둔 것)
//!   pulls/<이름>                진행 중인 pull이 쓸 레이어 목록 (pull이 공유 잠금을 쥔다). 빌드도 쓴다(쥔 층)
//!   build/<키>.json             빌드 캐시 기록: 캐시 키 → 층([`BuildCacheRecord`], `sbx build`)
//!   refs.json                   정규화된 참조 → 이미지 ID
//!   tmp/                        prepare 작업 공간 (끝나면 지운다)
//!   .lock                       refs.json 갱신용 잠금
//!   events/images/              이미지 이벤트 기록 (`sbx-events`, 관리 코어가 쓴다)
//! ```
//!
//! 배치 1(0.2)은 이미지마다 모든 층을 합친 `images/<hex>/rootfs.<형식>` 하나를 두었다([`CacheLayout::legacy_rootfs`]).
//! 배치 2는 그것을 쓰지 않는다. 옮기는 쪽(CLI의 `layout`)이 지우고, 다음 prepare가 층으로 다시 만든다.
//!
//! 파일 이름에는 digest의 hex만 쓴다. `:`는 Windows 파일 이름에 쓸 수 없다.
//!
//! **사용 중 보호**: 샌드박스가 실행 중인 동안 실행기 프로세스를 붙잡고 있는 쪽이
//! [`Store::lease`]로 이미지 잠금 파일에 공유 잠금을 건다. 이미지를 지우는 쪽은 배타 잠금을
//! 잡아야 지울 수 있다. 잠금이 cache_dir에 걸리므로 다른 data_dir의 샌드박스도 보호된다.
//! 층과 합친 것은 따로 잠그지 않는다. 정리는 어떤 이미지 기록도 가리키지 않는 것만 지우고, lease를 쥔 이미지의 기록은
//! 지워지지 않으므로 그 이미지가 쓰는 층도 남는다.
//!
//! **blob·층 보호**: blob, 층, 합친 것은 이미지 기록, 빌드 캐시 기록, 진행 중인 pull 표시가 가리키는 동안 지워지지
//! 않는다. 빌드 캐시 기록이 이긴다: 만든 이미지를 지워도(`rmi`) 캐시가 가리키는 층은 남고, 캐시 기록을 지우는 것
//! (`system prune`)이 그 층을 놓는다.
//! 규칙과 순서는 [`write::claim_blobs`], [`write::collect`], [`write::wait_for_collect`]에 있다.
//!
//! - 의존: `sbx-model`

use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use sbx_model::{Arch, Digest, ImageConfig, ImageId, Layer, RootfsFormat};

pub mod digest;
pub mod stack;
#[cfg(feature = "write")]
pub mod write;

pub use stack::{EMPTY_LAYER, LayerState, MAX_LAYER_DISKS, Part, Plan};

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
    /// `sbx build`가 저장한 이미지면 어디서 만들었는지(다시 빌드하려고). 받거나 불러온 이미지, 이 필드가 없던 때의
    /// 기록은 `None`이다. 더하기만 한 선택 필드라 배치 버전을 올리지 않는다: 옛 `sbx`(같은 배치 2)는 모르는 필드를
    /// 읽지 않고 넘기고, 새 `sbx`는 없으면 `None`으로 읽는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<BuildRecord>,
}

/// 만든 이미지를 어디서 만들었는지([`ImageRecord::build`]). 같은 자리에서 다시 빌드하는 데 필요한 것만 둔다:
/// 빌드 인자(이름도 값도), `--allow` 목록, 태그는 두지 않는다.
///
/// 호스트 경로는 그 사람의 것이다. 이 기록은 캐시 디렉터리의 `image.json`에만 있고, 이미지 config, 라벨, history,
/// 그리고 내보내는 것(`save`, `push`, `build -o type=oci|docker`)에는 들어가지 않는다.
///
/// 이미지 ID 하나에 기록 하나다. 그 ID를 저장한 마지막 빌드가 이긴다: 같은 이미지를 다른 디렉터리에서 다시 만들면
/// 그 디렉터리가 남는다. 태그가 여럿이어도 기록은 이미지의 것이라 하나다. 빌드가 아닌 것(`pull`, `load`)이 같은 ID의
/// 기록을 다시 쓰면 빌드 기록은 없어진다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecord {
    /// 빌드 컨텍스트 디렉터리의 절대 경로(`sbx build`는 로컬 디렉터리만 받는다).
    pub context: String,
    /// Dockerfile의 절대 경로. 표준 입력에서 읽었으면(`-f -`) `None`.
    pub file: Option<String>,
    /// `--target`. 주지 않았으면 `None`.
    pub target: Option<String>,
}

impl ImageRecord {
    /// 레이어 압축 크기의 합.
    pub fn compressed_size(&self) -> u64 {
        self.layers.iter().map(|l| l.size).sum()
    }
}

/// 빌드 캐시 기록 하나([`CacheLayout::build_cache_record`]): 그 키의 단계가 만든 층.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCacheRecord {
    /// 층.
    pub layer: Layer,
    /// 층을 만든 시각(RFC 3339).
    pub created: String,
}

/// 받은 문서에 대한 것([`CacheLayout::image_documents`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentsInfo {
    /// 플랫폼 manifest의 미디어 형식.
    pub manifest_media_type: String,
    /// image config의 digest.
    pub config_digest: Digest,
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
        self.blobs_dir().join(digest.hex())
    }

    /// 이미지 디렉터리.
    pub fn image_dir(&self, id: &ImageId) -> PathBuf {
        self.root.join("images").join(id.digest().hex())
    }

    /// 이미지 기록.
    pub fn image_record(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join("image.json")
    }

    /// 받은 플랫폼 manifest 그대로(`sbx save`). 0.2.1 전에 받은 이미지에는 없다.
    pub fn image_manifest(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join("manifest.json")
    }

    /// 받은 image config 그대로(`sbx save`).
    pub fn image_config(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join("config.json")
    }

    /// [`Self::image_manifest`]의 미디어 형식과 config의 digest([`DocumentsInfo`]).
    pub fn image_documents(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join("documents.json")
    }

    /// 배치 1의 이미지별 rootfs(모든 층을 합친 것). 지금은 쓰지 않는다. 옮길 때 지운다.
    pub fn legacy_rootfs(&self, id: &ImageId, format: RootfsFormat) -> PathBuf {
        self.image_dir(id)
            .join(format!("rootfs.{}", format.as_str()))
    }

    /// 층 디렉터리들의 부모.
    pub fn layers_dir(&self) -> PathBuf {
        self.root.join("layers")
    }

    /// 층 하나(diff_id)의 디렉터리.
    pub fn layer_dir(&self, diff_id: &Digest) -> PathBuf {
        self.layers_dir().join(diff_id.hex())
    }

    /// 층 하나의 디스크.
    pub fn layer_disk(&self, diff_id: &Digest, format: RootfsFormat) -> PathBuf {
        self.layer_dir(diff_id)
            .join(format!("layer.{}", format.as_str()))
    }

    /// 층이 혼자 설 수 없다는 표시.
    pub fn layer_needs_lower(&self, diff_id: &Digest) -> PathBuf {
        self.layer_dir(diff_id).join("needs-lower")
    }

    /// 합친 것들의 부모.
    pub fn merged_dir(&self) -> PathBuf {
        self.root.join("merged")
    }

    /// 아래 층들을 합친 것(chain ID)의 디렉터리.
    pub fn merged_for(&self, chain: &Digest) -> PathBuf {
        self.merged_dir().join(chain.hex())
    }

    /// 아래 층들을 합친 디스크.
    pub fn merged_disk(&self, chain: &Digest, format: RootfsFormat) -> PathBuf {
        self.merged_for(chain)
            .join(format!("rootfs.{}", format.as_str()))
    }

    /// 층 디스크 하나의 파일.
    pub fn part_disk(&self, part: &Part, format: RootfsFormat) -> PathBuf {
        match part {
            Part::Layer(d) => self.layer_disk(d, format),
            Part::Merged { chain, .. } => self.merged_disk(chain, format),
        }
    }

    /// 층이나 합친 것의 디렉터리 안 잠금 파일(만드는 동안 배타).
    pub fn build_lock(dir: &Path) -> PathBuf {
        dir.join(".lock")
    }

    /// 이미지에서 꺼낸 커널 (`ImageConfig::KERNEL_LABEL`이 있는 이미지만).
    pub fn kernel(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join("kernel")
    }

    /// 이미지 잠금 파일.
    pub fn image_lock(&self, id: &ImageId) -> PathBuf {
        self.image_dir(id).join(".lock")
    }

    /// 게스트 기반 이미지 디렉터리.
    pub fn guest_dir(&self, version: &str, arch: Arch) -> PathBuf {
        self.root.join("guest").join(version).join(arch.oci_name())
    }

    /// 게스트 init 파일.
    pub fn guest_init(&self, version: &str, arch: Arch) -> PathBuf {
        self.guest_dir(version, arch).join("initramfs")
    }

    /// 태그 기록.
    pub fn refs(&self) -> PathBuf {
        self.root.join("refs.json")
    }

    /// 이미지 이벤트 기록 디렉터리(`sbx-events`).
    pub fn image_events(&self) -> PathBuf {
        self.root.join("events").join("images")
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

    /// blob 디렉터리.
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }

    /// 정리(blob, 층, 합친 것)와 pull 표시 생성, prepare 시작을 서로 배제하는 잠금 파일.
    pub fn blobs_lock(&self) -> PathBuf {
        self.root.join("blobs").join(".lock")
    }

    /// 진행 중인 pull 표시들.
    pub fn pulls_dir(&self) -> PathBuf {
        self.root.join("pulls")
    }

    /// 빌드 캐시 기록들.
    pub fn build_cache_dir(&self) -> PathBuf {
        self.root.join("build")
    }

    /// 빌드 캐시 기록 하나. 키는 16진수 sha256이어야 한다(아니면 `None`).
    pub fn build_cache_record(&self, key: &str) -> Option<PathBuf> {
        (key.len() == 64
            && key
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
        .then(|| self.build_cache_dir().join(format!("{key}.json")))
    }
}

/// 잠금을 잡지 못한 이유.
#[derive(Debug)]
pub enum LeaseError {
    /// 이미지가 없음 (기다리는 동안 지워진 경우 포함).
    ImageNotFound,
    /// 이미지는 있지만 그 형식의 rootfs가 없음.
    RootfsNotFound,
    /// 이미지가 자기 커널을 담고 있다고 하는데 꺼내 둔 커널이 없음 (prepare를 다시 해야 한다).
    KernelNotFound,
    /// 입출력 오류.
    Io(io::Error),
}

impl fmt::Display for LeaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LeaseError::ImageNotFound => f.write_str("image not found"),
            LeaseError::RootfsNotFound => f.write_str("rootfs not prepared"),
            LeaseError::KernelNotFound => f.write_str("image kernel not prepared"),
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

/// 살아 있는 동안 이미지 삭제를 막는 공유 잠금. 이미지 기록이 남으므로 그 이미지가 쓰는 층도 지워지지 않는다.
#[derive(Debug)]
pub struct RootfsLease {
    _lock: File,
    disks: Vec<PathBuf>,
    kernel: Option<PathBuf>,
}

impl RootfsLease {
    /// 붙일 층 디스크들, 아래부터([`stack`]). 첫째가 맨 아래 층이다.
    pub fn disks(&self) -> &[PathBuf] {
        &self.disks
    }

    /// 잠금으로 보호되는, 이미지가 담은 커널. 없으면 실행기의 기본 커널로 부팅한다.
    pub fn kernel(&self) -> Option<&Path> {
        self.kernel.as_deref()
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

    /// 층 하나를 살펴본 결과. 그 형식의 층 디스크가 있으면 `Ready`, 혼자 설 수 없다는 표시가 있으면 `NeedsLower`.
    pub fn layer_state(&self, diff_id: &Digest, format: RootfsFormat) -> LayerState {
        if self.layout.layer_disk(diff_id, format).is_file() {
            LayerState::Ready
        } else if self.layout.layer_needs_lower(diff_id).is_file() {
            LayerState::NeedsLower
        } else {
            LayerState::Unknown
        }
    }

    /// 디스크 `max`개 안의 층 구성([`stack::plan`]). 지금 캐시에 있는 것으로 정한다.
    pub fn plan(&self, record: &ImageRecord, format: RootfsFormat, max: usize) -> Plan {
        stack::plan(&record.layers, max, |d| self.layer_state(d, format))
    }

    /// 디스크 `max`개로 띄울 수 있게 준비되어 있으면 붙일 파일들(아래부터). 아니면 `None`(prepare할 것이 있다).
    pub fn stack(
        &self,
        record: &ImageRecord,
        format: RootfsFormat,
        max: usize,
    ) -> Option<Vec<PathBuf>> {
        let Plan::Parts(parts) = self.plan(record, format, max) else {
            return None;
        };
        let disks: Vec<PathBuf> = parts
            .iter()
            .map(|p| self.layout.part_disk(p, format))
            .collect();
        disks.iter().all(|d| d.is_file()).then_some(disks)
    }

    /// 디스크 `max`개로 띄울 준비가 됐는지: 층 구성이 모두 있고, 이미지가 자기 커널을 담고 있으면 그 커널도 꺼내 두었다.
    pub fn is_prepared(&self, id: &ImageId, format: RootfsFormat, max: usize) -> bool {
        self.image(id).is_ok_and(|r| {
            self.stack(&r, format, max).is_some()
                && (r.config.kernel().is_none() || self.layout.kernel(id).is_file())
        })
    }

    /// 어떤 디스크 수로든 띄울 수 있게 준비되어 있는지(`sbx images`가 보인다).
    pub fn is_prepared_for_some(&self, record: &ImageRecord, format: RootfsFormat) -> bool {
        (1..=MAX_LAYER_DISKS).any(|max| self.stack(record, format, max).is_some())
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

    /// 빌드 캐시 기록 하나. 없으면 `None`.
    pub fn build_cache(&self, key: &str) -> Result<Option<BuildCacheRecord>, StoreError> {
        match self.layout.build_cache_record(key) {
            Some(p) if p.is_file() => read_json(&p).map(Some),
            _ => Ok(None),
        }
    }

    /// 모든 빌드 캐시 기록(키, 기록). 키 순서. 읽지 못하는 기록이 있으면 오류다.
    pub fn build_caches(&self) -> Result<Vec<(String, BuildCacheRecord)>, StoreError> {
        let entries = match std::fs::read_dir(self.layout.build_cache_dir()) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(key) = name.strip_suffix(".json") else {
                continue;
            };
            if self.layout.build_cache_record(key).is_none() {
                continue;
            }
            out.push((key.to_owned(), read_json(&entry.path())?));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
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

    /// 디스크 `max`개로 띄울 층 디스크들에 공유 잠금을 건다. 이미지를 지우는 쪽이 배타 잠금을 쥐고 있으면 풀릴 때까지
    /// 기다린다.
    ///
    /// 잠금을 잡은 뒤에 존재를 다시 확인한다. 기다리는 동안 지워졌으면 `ImageNotFound`다. 층 구성([`Self::stack`])이
    /// 준비되어 있지 않으면 `RootfsNotFound`다.
    /// 이미지가 자기 커널을 담고 있으면(`ImageConfig::KERNEL_LABEL`) 꺼내 둔 커널도 있어야 한다.
    pub fn lease(
        &self,
        id: &ImageId,
        format: RootfsFormat,
        max: usize,
    ) -> Result<RootfsLease, LeaseError> {
        let lock = self.lock_shared(id)?;
        let record = self.image(id).map_err(|e| match e {
            StoreError::ImageNotFound(_) => LeaseError::ImageNotFound,
            StoreError::Io(e) => LeaseError::Io(e),
            e => LeaseError::Io(io::Error::other(e.to_string())),
        })?;
        let disks = self
            .stack(&record, format, max)
            .ok_or(LeaseError::RootfsNotFound)?;
        let kernel = match record.config.kernel() {
            None => None,
            Some(_) => {
                let k = self.layout.kernel(id);
                if !k.is_file() {
                    return Err(LeaseError::KernelNotFound);
                }
                Some(k)
            }
        };
        Ok(RootfsLease {
            _lock: lock,
            disks,
            kernel,
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
            l.layer_disk(id.digest(), RootfsFormat::Erofs),
            Path::new("/c/layers").join(&hex).join("layer.erofs")
        );
        assert_eq!(
            l.merged_disk(id.digest(), RootfsFormat::Erofs),
            Path::new("/c/merged").join(&hex).join("rootfs.erofs")
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
            store.lease(&image(), RootfsFormat::Erofs, 1),
            Err(LeaseError::ImageNotFound)
        ));
    }
}
