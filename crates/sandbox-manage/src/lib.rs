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
//!   이미지가 자기 커널을 담고 있으면(`ImageConfig::KERNEL_LABEL`) 그 커널도 `images/<hex>/kernel`로 꺼낸다.
//!   커널을 꺼낼 수 없으면(라벨의 경로가 없거나 일반 파일이 아니면) rootfs도 만들지 않는다.
//!   만드는 동안 이미지에 공유 잠금을 쥐므로 그사이 이미지가 지워지지 않는다.
//! - 자동 정리는 없다. 무엇을 언제 지울지는 관리하는 쪽(임베드한 앱이나 사람)이 정하고 `remove`를 부른다.
//! - `remove`는 이미지를 지운 뒤 다른 이미지도, 진행 중인 pull도 쓰지 않는 레이어 blob을 지운다(Docker `rmi`와 같다).
//!   동시에 같은 레이어를 쓰는 이미지를 받고 있어도 그 blob은 남는다([`write::claim_blobs`]).
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

use sandbox_image::{ImageSource, Platform};
use sandbox_model::{Arch, Compression, Digest, ImageId, RootfsFormat};
use sandbox_rootfs::{ContentSource, MergedTree, NodeKind, RootfsBuilder};
use sandbox_store::digest::HashingReader;
use sandbox_store::write::{self, BlobWriter, WorkDir};
use sandbox_store::{ImageRecord, LeaseError, Store};

mod error;
mod layer;
mod zstd;
pub use error::ManageError;

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
    /// 함께 지운 레이어 blob. 다른 이미지가 쓰는 레이어는 남는다.
    pub deleted_layers: Vec<Digest>,
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
        let digests: Vec<Digest> = resolved.layers.iter().map(|l| l.digest.clone()).collect();
        // 이미지 기록을 쓸 때까지 쥔다. 그동안 rmi의 blob 정리가 이 레이어들을 지우지 않는다.
        let _claim = write::claim_blobs(layout, &digests)?;
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

    /// 그 형식의 rootfs를 만든다. 이미지가 자기 커널을 담고 있으면(`ImageConfig::KERNEL_LABEL`) 그 커널도 꺼낸다.
    /// 무언가 만들었으면 `true`, 모두 이미 있었으면 `false`.
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
        let record = self.store.image(image)?;
        let layout = self.store.layout();
        let need_rootfs = !self.store.has_rootfs(image, format);
        let need_kernel = record
            .config
            .kernel()
            .filter(|_| !layout.kernel(image).is_file());
        if !need_rootfs && need_kernel.is_none() {
            return Ok(false);
        }
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
                Compression::Zstd => Box::new(zstd::ZstdReader::new(BufReader::new(file))),
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
        let spool = layer::Spool::new(work.path());

        // 커널을 먼저 꺼낸다. 잘못된 라벨이면 rootfs도 만들지 않는다(이 이미지로는 부팅할 수 없다).
        if let Some(path) = need_kernel {
            let target = layout.kernel(image);
            let temp = write::temp_path(&layout.image_dir(image), "kernel");
            let result = extract_kernel(&tree, &spool, path, &temp)
                .and_then(|()| Ok(std::fs::rename(&temp, &target)?));
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result?;
        }
        if need_rootfs {
            let target = layout.rootfs(image, format);
            let temp = write::temp_path(&layout.image_dir(image), &format!("rootfs.{format}"));
            let result = (|| -> Result<(), ManageError> {
                let mut out = io::BufWriter::new(File::create(&temp)?);
                builder.build(&tree, &spool, &mut out)?;
                let file = out.into_inner().map_err(|e| e.into_error())?;
                file.sync_all()?;
                std::fs::rename(&temp, &target)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result?;
        }
        Ok(true)
    }

    /// 그 형식으로 띄울 준비가 됐는지: rootfs가 있고, 이미지가 자기 커널을 담고 있으면 그 커널도 꺼내 두었다.
    /// 아니면 [`Manager::prepare`]가 할 일이 있다.
    pub fn is_prepared(&self, image: &ImageId, format: RootfsFormat) -> bool {
        let layout = self.store.layout();
        self.store.has_rootfs(image, format)
            && self
                .store
                .image(image)
                .is_ok_and(|r| r.config.kernel().is_none() || layout.kernel(image).is_file())
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
                deleted_layers: Vec::new(),
            }),
            None => Err(ManageError::RefNotFound(reference.to_owned())),
        }
    }

    /// 게스트 init 내용을 캐시에 둔다(sbx에 내장된 것을 꺼낼 때). 경로는 내용 digest로 정한다
    /// (`guest/sha256-<hex>/<arch>/initramfs`). 같은 내용이 이미 있으면 그대로 두고, 깨져 있으면 다시 쓴다.
    /// 쓰기는 임시 파일에 쓴 뒤 rename한다.
    pub fn install_guest_init(&self, arch: Arch, bytes: &[u8]) -> Result<PathBuf, ManageError> {
        let layout = self.store.layout();
        let digest = sandbox_store::digest::sha256(bytes);
        let path = layout.guest_init(&format!("sha256-{}", digest.hex()), arch);
        if std::fs::read(&path).is_ok_and(|have| sandbox_store::digest::sha256(&have) == digest) {
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write::atomic_write(&path, bytes)?;
        Ok(path)
    }

    /// 이미지와 그 이미지를 가리키는 참조를 모두 지우고, 더 쓰이지 않는 레이어 blob도 지운다.
    /// 실행 중인 샌드박스가 쓰고 있으면 [`ManageError::InUse`]다.
    ///
    /// blob 정리가 실패하면 오류를 돌려준다. 그때 이미지와 참조는 이미 지워졌다.
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
        let deleted_layers = write::collect_blobs(layout)?;
        Ok(Removal {
            untagged,
            deleted: Some(image.clone()),
            deleted_layers,
        })
    }
}

/// 이미지 안 `path`(절대 경로, 링크는 이미지 안에서 따라간다)의 일반 파일을 `out`에 쓴다.
fn extract_kernel(
    tree: &MergedTree,
    spool: &layer::Spool,
    path: &str,
    out: &std::path::Path,
) -> Result<(), ManageError> {
    let bad = |reason: &str| ManageError::Kernel {
        path: path.to_owned(),
        reason: reason.to_owned(),
    };
    if !path.starts_with('/') {
        return Err(bad("must be an absolute path"));
    }
    let real = tree
        .resolve(path)
        .ok_or_else(|| bad("not found in the image"))?;
    let content = match tree.get(&real).map(|n| &n.kind) {
        Some(NodeKind::File(c)) => *c,
        _ => return Err(bad("not a regular file")),
    };
    let mut src = spool.open(&content)?;
    let mut dst = io::BufWriter::new(File::create(out)?);
    io::copy(&mut src, &mut dst)?;
    dst.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests;
