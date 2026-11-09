//! 관리 코어: 이미지와 캐시.
//!
//! 이미지를 받고(pull), rootfs를 만들고(prepare), 목록을 보여주고, 지운다(remove, untag).
//! cache_dir에 쓰는 것은 이 크레이트뿐이다. data_dir과 샌드박스 실행은 모른다.
//!
//! **명시적 동작**
//!
//! - `pull`은 받기만 한다. rootfs는 만들지 않는다. 이미 있는 레이어는 다시 받지 않는다.
//!   레이어는 digest를 직접 계산해 맞을 때만 저장한다. 모든 레이어가 저장된 뒤에야 이미지 기록과 참조를 쓴다.
//! - `prepare`는 이미 그 형식의 rootfs가 있으면 아무것도 하지 않는다. 레이어의 diff_id도 직접 계산해 대조한다.
//!   만드는 동안 이미지에 공유 잠금을 쥐므로 그사이 이미지가 지워지지 않는다.
//! - 자동 정리는 없다. 무엇을 언제 지울지는 관리하는 쪽(임베드한 앱이나 사람)이 정하고 `remove`를 부른다.
//! - `remove`는 실행 중인 샌드박스가 쓰는 이미지를 지우지 않는다. 강제할 수도 없다.
//!   실행 중인지는 cache_dir의 잠금으로 알 수 있으므로 어느 data_dir의 샌드박스든 보호된다.
//! - 정지된 샌드박스가 쓰는 이미지를 지울지는 이 크레이트가 판단하지 않는다. data_dir을 모르기 때문이다.
//!   표현 계층이 실행 코어에서 샌드박스 목록을 보고 판단한다.
//!
//! - 의존: `sandbox-model`, `sandbox-image`(trait), `sandbox-rootfs`(trait), `sandbox-store`(`write`)

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::PathBuf;
use std::sync::Arc;

use flate2::read::GzDecoder;

use sandbox_image::{ImageError, ImageSource, Platform};
use sandbox_model::{Compression, ImageId, RootfsFormat};
use sandbox_rootfs::{MergedTree, RootfsBuilder, RootfsError};
use sandbox_store::digest::HashingReader;
use sandbox_store::write::{self, BlobWriter, WorkDir};
use sandbox_store::{ImageRecord, LeaseError, Store, StoreError};

mod layer;

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

/// 받아 둔 이미지 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// 이미지 기록.
    pub record: ImageRecord,
    /// 이 이미지를 가리키는 참조들.
    pub refs: Vec<String>,
    /// 준비된 rootfs 형식.
    pub prepared: Vec<RootfsFormat>,
}

/// `remove`와 `untag`의 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// 지운 참조.
    pub untagged: Vec<String>,
    /// 이미지 자체를 지웠으면 그 ID.
    pub deleted: Option<ImageId>,
}

/// 관리 코어.
pub struct Manager {
    store: Store,
    source: Arc<dyn ImageSource>,
    builders: Vec<Arc<dyn RootfsBuilder>>,
}

impl fmt::Debug for Manager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Manager")
            .field("cache_dir", &self.store.layout().root())
            .field(
                "builders",
                &self.builders.iter().map(|b| b.format()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Manager {
    /// 만든다. cache_dir이 없으면 만든다.
    pub fn new(
        cache_dir: impl Into<PathBuf>,
        source: Arc<dyn ImageSource>,
        builders: Vec<Arc<dyn RootfsBuilder>>,
    ) -> Result<Self, ManageError> {
        let cache_dir = cache_dir.into();
        std::fs::create_dir_all(&cache_dir)?;
        Ok(Self {
            store: Store::open(cache_dir),
            source,
            builders,
        })
    }

    /// 읽기용 저장소.
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// 이미지를 받는다. 받기만 한다.
    pub async fn pull(
        &self,
        reference: &str,
        platform: Platform,
    ) -> Result<ImageInfo, ManageError> {
        let resolved = self
            .source
            .resolve(reference, platform)
            .await
            .map_err(ManageError::Source)?;
        let layout = self.store.layout();
        for layer in &resolved.layers {
            if layout.blob(&layer.digest).is_file() {
                continue;
            }
            let mut blob = BlobWriter::create(layout)?;
            self.source
                .fetch_blob(&resolved.source, &layer.digest, &mut blob)
                .await
                .map_err(ManageError::Source)?;
            blob.commit(layout, &layer.digest).map_err(|e| {
                if e.kind() == io::ErrorKind::InvalidData {
                    ManageError::Integrity(e.to_string())
                } else {
                    ManageError::Io(e)
                }
            })?;
        }
        let record = ImageRecord {
            id: resolved.id.clone(),
            reference: resolved.reference.clone(),
            source: resolved.source,
            arch: resolved.platform.arch,
            config: resolved.config,
            layers: resolved.layers,
            ignored_fields: resolved.ignored_fields,
        };
        write::write_image_record(layout, &record)?;
        write::set_ref(layout, &resolved.reference, &resolved.id)?;
        self.info(record)
    }

    /// 그 형식의 rootfs를 만든다. 만들었으면 `true`, 이미 있었으면 `false`.
    pub async fn prepare(
        &self,
        image: &ImageId,
        format: RootfsFormat,
    ) -> Result<bool, ManageError> {
        let builder = self
            .builders
            .iter()
            .find(|b| b.format() == format)
            .ok_or(ManageError::BuilderMissing(format))?;
        let _guard = self.store.guard(image).map_err(|e| match e {
            LeaseError::Io(e) => ManageError::Io(e),
            _ => ManageError::ImageNotFound(image.clone()),
        })?;
        if self.store.has_rootfs(image, format) {
            return Ok(false);
        }
        let record = self.store.image(image)?;
        let layout = self.store.layout();
        let work = WorkDir::create(layout, "prepare")?;

        let mut tree = MergedTree::new();
        for (index, l) in record.layers.iter().enumerate() {
            let file = File::open(layout.blob(&l.digest)).map_err(|e| ManageError::Layer {
                index,
                reason: format!("blob {}: {e}", l.digest),
            })?;
            let decoded: Box<dyn Read> = match l.compression {
                Compression::None => Box::new(BufReader::new(file)),
                Compression::Gzip => Box::new(GzDecoder::new(BufReader::new(file))),
            };
            let mut hashing = HashingReader::new(decoded);
            let entries = layer::read(index, &mut hashing, work.path())?;
            // tar 끝 표시 뒤에 남은 바이트까지 읽어야 diff_id가 맞는다.
            io::copy(&mut hashing, &mut io::sink())?;
            let actual = hashing.digest();
            if actual != l.diff_id {
                return Err(ManageError::Integrity(format!(
                    "layer {index} diff_id: expected {}, got {actual}",
                    l.diff_id
                )));
            }
            tree.apply_layer(entries)?;
        }

        let target = layout.rootfs(image, format);
        let temp = write::temp_path(&layout.image_dir(image), &format!("rootfs.{format}"));
        let result = (|| -> Result<(), ManageError> {
            let mut out = io::BufWriter::new(File::create(&temp)?);
            builder.build(&tree, &layer::Spool::new(work.path()), &mut out)?;
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            std::fs::rename(&temp, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.map(|()| true)
    }

    /// 받아 둔 이미지 목록. ID 순서.
    pub fn images(&self) -> Result<Vec<ImageInfo>, ManageError> {
        self.store
            .images()?
            .into_iter()
            .map(|r| self.info(r))
            .collect()
    }

    fn info(&self, record: ImageRecord) -> Result<ImageInfo, ManageError> {
        let refs = self
            .store
            .refs()?
            .into_iter()
            .filter(|(_, id)| *id == record.id)
            .map(|(r, _)| r)
            .collect();
        let prepared = RootfsFormat::ALL
            .iter()
            .copied()
            .filter(|f| self.store.has_rootfs(&record.id, *f))
            .collect();
        Ok(ImageInfo {
            record,
            refs,
            prepared,
        })
    }

    /// 참조 하나를 지운다. 이미지는 지우지 않는다(Docker `rmi`로 태그만 지울 때와 같다).
    pub fn untag(&self, reference: &str) -> Result<Removal, ManageError> {
        match write::remove_ref(self.store.layout(), reference)? {
            Some(_) => Ok(Removal {
                untagged: vec![reference.to_owned()],
                deleted: None,
            }),
            None => Err(ManageError::RefNotFound(reference.to_owned())),
        }
    }

    /// 이미지와 그 이미지를 가리키는 참조를 모두 지운다.
    /// 실행 중인 샌드박스가 쓰고 있으면 [`ManageError::InUse`]다.
    pub fn remove(&self, image: &ImageId) -> Result<Removal, ManageError> {
        let layout = self.store.layout();
        if !layout.image_dir(image).is_dir() {
            return Err(ManageError::ImageNotFound(image.clone()));
        }
        // 잠금 파일이 없는 이미지 디렉터리는 만드는 도중이거나 깨진 것이다. 잠금 파일을 만들고 진행한다.
        write::create_image_dir(layout, image)?;
        let lock = write::try_lock_image(layout, image)?
            .ok_or_else(|| ManageError::InUse(image.clone()))?;
        let untagged = write::remove_refs_to(layout, image)?;
        write::remove_image(layout, image, lock)?;
        Ok(Removal {
            untagged,
            deleted: Some(image.clone()),
        })
    }
}

#[cfg(test)]
mod tests;
