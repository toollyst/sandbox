//! 관리 코어: 이미지와 캐시.
//!
//! 이미지를 받고(pull), rootfs를 만들고(prepare), 목록을 보여주고, 이름을 붙이고(tag), 올리고(push), 지운다
//! (remove, untag). cache_dir에 쓰는 것은 이 크레이트뿐이다. data_dir과 샌드박스 실행은 모른다.
//!
//! **명시적 동작**
//!
//! - `pull`은 받기만 한다. rootfs는 만들지 않는다. 이미 있는 레이어는 다시 받지 않는다.
//!   레이어는 digest를 직접 계산해 맞을 때만 저장한다. 모든 레이어가 저장된 뒤에야 이미지 기록과 참조를 쓴다.
//! - `prepare`는 층 단위로 만든다(층마다 EROFS 하나, 같은 층은 이미지끼리 한 번만, `sbx_store::stack`). 실행기가 붙일
//!   디스크 수를 받아 그 안에 들게 하고, 모자라면 아래 층들을 합친다. 이미 다 있으면 아무것도 하지 않는다.
//!   레이어의 diff_id도 직접 계산해 대조한다. 압축 blob은 지우지 않는다(`save`가 그대로 쓴다).
//!   **S1 phase B 자리**: 층 디스크를 만든 뒤 받은 blob을 지우고(`--keep-blobs`로 남긴다), `save`·push가 층 디스크에서
//!   층 tar를 다시 만드는 일은 [`Manager::prepare`]의 끝과 [`Manager::save`]에 붙는다. 그때 `collect`의 blob 규칙과
//!   `stored_parts`의 `Blob`도 바뀐다.
//!   이미지가 자기 커널을 담고 있으면(`ImageConfig::KERNEL_LABEL`) 그 커널도 `images/<hex>/kernel`로 꺼낸다.
//!   커널을 꺼낼 수 없으면(라벨의 경로가 없거나 일반 파일이 아니면) rootfs도 만들지 않는다.
//!   만드는 동안 이미지에 공유 잠금을 쥐므로 그사이 이미지가 지워지지 않는다.
//! - 자동 정리는 없다. 무엇을 언제 지울지는 관리하는 쪽(임베드한 앱이나 사람)이 정하고 `remove`를 부른다.
//! - `remove`는 이미지를 지운 뒤 다른 이미지도, 진행 중인 pull도 쓰지 않는 레이어 blob, 층 디스크, 합친 디스크를
//!   지운다(Docker `rmi`와 같다).
//!   동시에 같은 레이어를 쓰는 이미지를 받고 있어도 그 blob은 남는다([`write::claim_blobs`]).
//! - `tag`는 참조 하나를 이미지에 붙인다. 그 참조가 다른 이미지를 가리키고 있었으면 옮긴다(Docker `tag`와 같다).
//!   참조의 정규화는 부르는 쪽이 한다(OCI 이름 규칙은 이 크레이트가 모른다).
//! - `push`는 받은 문서(manifest, config)와 레이어 blob을 저장된 바이트 그대로 올린다. 그래서 받은 이미지를 다른 곳에
//!   올려도 digest(이미지 ID)가 같다. 순서는 레이어(같은 것은 한 번), config, 마지막에 manifest다. blob 하나라도
//!   실패하면 manifest를 올리지 않으므로 올릴 곳의 태그는 바뀌지 않는다. 올리는 동안 이미지를 쥔다. 보내기 전에
//!   manifest의 sha256이 이미지 ID인지, blob 파일의 크기가 manifest의 것인지 본다. 올릴 곳은 부르는 쪽이
//!   [`ImageDestination`]으로 준다.
//! - `remove`는 실행 중인 샌드박스가 쓰는 이미지를 지우지 않는다. 강제할 수도 없다.
//!   실행 중인지는 cache_dir의 잠금으로 알 수 있으므로 어느 data_dir의 샌드박스든 보호된다.
//! - 정지된 샌드박스가 쓰는 이미지를 지울지는 이 크레이트가 판단하지 않는다. data_dir을 모르기 때문이다.
//!   표현 계층이 실행 코어에서 샌드박스 목록을 보고 판단한다.
//!
//! - **이벤트**: 캐시를 바꾼 뒤 `<cache_dir>/events/images`([`events_log`])에 덧붙인다. 종류는 [`EVENT_KIND`],
//!   동작은 `pull`(속성 `reference`), `prepare`(무언가 만들었을 때만, `rootfs_format`), `tag`(`reference`),
//!   `untag`(`reference`), `push`(`reference`, 올릴 곳의 참조), `delete`(이미지를 지웠을 때)이다. ID는 이미지 ID다.
//!   기록하지 못해도 동작은 계속한다(알림이다).
//!
//! - 의존: `sbx-model`, `sbx-image`(trait), `sbx-rootfs`(trait), `sbx-store`(`write`), `sbx-events`

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::GzDecoder;

use sbx_image::{ImageSource, Platform};

pub use sbx_image::{BlobPush, ImageDestination};
use sbx_model::{Arch, Compression, Digest, ImageId, RootfsFormat};
use sbx_rootfs::{ContentSource, MergedTree, NodeKind, RootfsBuilder};
use sbx_store::digest::HashingReader;
use sbx_store::write::{self, BlobWriter, WorkDir};
use sbx_store::{ImageRecord, LayerState, LeaseError, Part, Plan, Store};

pub use sbx_store::write::{BlobClaim, Collected};

mod build;
mod built;
mod error;
mod layer;
mod push;
mod tarlimit;
mod zstd;
pub use built::{BUILD_ROOT_SOURCE, BUILT_SOURCE, BuildCacheUsage, LayerContents};
pub use error::ManageError;

/// 퍼징(`fuzz/`)만 쓰는 입구: 레이어 tar 읽기와 zstd 풀기. `fuzzing` feature를 켜야 있다.
/// 배포하는 바이너리는 이 feature를 켜지 않는다(`cargo xtask check-deps`가 확인한다).
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing {
    pub use crate::layer::{Spool, read as read_layer};
    pub use crate::zstd::ZstdReader;
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
    /// 함께 지운 레이어 blob. 다른 이미지가 쓰는 레이어는 남는다.
    pub deleted_layers: Vec<Digest>,
    /// 함께 지운 층 디스크(diff_id). 다른 이미지가 쓰는 층은 남는다.
    pub deleted_disks: Vec<Digest>,
}

/// 캐시에서 이미지가 차지하는 것의 종류([`StoredPart`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartKind {
    /// 이미지 디렉터리(기록, 받은 문서, 꺼낸 커널).
    Image,
    /// 압축한 층 blob(`save`, prepare가 읽는다).
    Blob,
    /// 층 하나의 디스크(디렉터리).
    Layer,
    /// 아래 층들을 합친 디스크(디렉터리, 지워도 되는 캐시).
    Merged,
}

/// 캐시에서 이미지들이 차지하는 것 하나와 그것을 쓰는 이미지들([`Manager::stored_parts`]). 같은 층을 쓰는 이미지가
/// 여럿이면 하나로 센다. 크기는 부르는 쪽이 `path`로 잰다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPart {
    /// 종류.
    pub kind: PartKind,
    /// 파일이나 디렉터리.
    pub path: PathBuf,
    /// 쓰는 이미지들. ID 순서.
    pub users: Vec<ImageId>,
}

/// 관리 코어.
pub struct Manager {
    store: Store,
    source: Arc<dyn ImageSource>,
    builders: Vec<Arc<dyn RootfsBuilder>>,
    /// 받을 이미지의 레이어 합계(압축된 크기) 상한. 받기 전에 본다.
    max_image_bytes: Option<u64>,
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

/// 받기 진행. 이미 있는 레이어는 세지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullProgress {
    /// 받을 레이어 수.
    pub layers: usize,
    /// 받은 레이어 수.
    pub layers_done: usize,
    /// 받을 바이트 (압축된 크기의 합).
    pub bytes: u64,
    /// 받은 바이트.
    pub bytes_done: u64,
}

/// 올리기 진행. blob(레이어와 config) 단위다. 이미 있거나 끌어온 blob도 끝난 것으로 센다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushProgress {
    /// 올릴 blob 수(같은 레이어는 한 번).
    pub blobs: usize,
    /// 끝난 blob 수.
    pub blobs_done: usize,
    /// 모든 blob의 바이트 합.
    pub bytes: u64,
    /// 끝났거나 보낸 바이트.
    pub bytes_done: u64,
}

/// 올린 blob 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushedBlob {
    /// digest.
    pub digest: Digest,
    /// 바이트 수.
    pub size: u64,
    /// 어떻게 올렸는지.
    pub how: BlobPush,
}

/// `push`의 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    /// 올린 곳의 정규화된 참조(태그 포함).
    pub reference: String,
    /// 이미지 ID(올린 manifest의 digest).
    pub id: ImageId,
    /// manifest의 바이트 수.
    pub manifest_size: u64,
    /// 레이어(같은 것은 한 번), 그다음 config.
    pub blobs: Vec<PushedBlob>,
}

/// 쓴 바이트를 세어 알리는 `Write`.
struct Counting<'a> {
    inner: &'a mut (dyn Write + Send),
    done: u64,
    /// manifest가 알린 크기. 소스가 이보다 많이 보내면 쓰지 않고 멈춘다(끝없는 응답으로 디스크를 채우지 못한다).
    limit: u64,
    /// `limit`을 넘겨 멈췄다.
    over: bool,
    report: &'a (dyn Fn(u64) + Sync),
}

impl Write for Counting<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.done + buf.len() as u64 > self.limit {
            self.over = true;
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the blob is larger than the {} bytes the manifest says",
                    self.limit
                ),
            ));
        }
        let n = self.inner.write(buf)?;
        self.done += n as u64;
        (self.report)(self.done);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// 이미지 이벤트의 종류.
pub const EVENT_KIND: &str = "image";

/// `cache_dir`의 이미지 이벤트 기록.
pub fn events_log(cache_dir: &Path) -> sbx_events::Log {
    sbx_events::Log::new(sbx_store::CacheLayout::new(cache_dir).image_events())
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
            max_image_bytes: None,
        })
    }

    /// 레이어 합계(압축된 크기, 같은 레이어는 한 번)가 `max`를 넘는 이미지는 받지 않는다([`ManageError::TooLarge`]).
    /// 레이어를 받기 전에 manifest로 판단한다. 이미 받은 레이어도 센다(이미지의 크기다).
    pub fn with_max_image_bytes(mut self, max: Option<u64>) -> Self {
        self.max_image_bytes = max;
        self
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
        self.pull_with(reference, platform, &|_| {}).await
    }

    /// [`pull`](Self::pull)과 같고, 받는 동안 `progress`에 진행을 알린다. 받을 레이어 목록이 정해지면 한 번,
    /// 그 뒤 바이트가 들어올 때마다 부른다. 보여주는 방식은 부르는 쪽이 정한다.
    pub async fn pull_with(
        &self,
        reference: &str,
        platform: Platform,
        progress: &(dyn Fn(PullProgress) + Sync),
    ) -> Result<ImageInfo, ManageError> {
        let resolved = self
            .source
            .resolve(reference, platform)
            .await
            .map_err(ManageError::Source)?;
        if let Some(max) = self.max_image_bytes {
            let mut seen = std::collections::HashSet::new();
            let size: u64 = resolved
                .layers
                .iter()
                .filter(|l| seen.insert(&l.digest))
                .map(|l| l.size)
                .sum();
            if size > max {
                return Err(ManageError::TooLarge {
                    reference: resolved.reference.clone(),
                    size,
                    max,
                });
            }
        }
        let layout = self.store.layout();
        let digests: Vec<Digest> = resolved.layers.iter().map(|l| l.digest.clone()).collect();
        // 이미지 기록을 쓸 때까지 쥔다. 그동안 rmi의 blob 정리가 이 레이어들을 지우지 않는다.
        let _claim = write::claim_blobs(layout, &digests)?;
        let missing: Vec<_> = resolved
            .layers
            .iter()
            .filter(|l| !layout.blob(&l.digest).is_file())
            .collect();
        let mut state = PullProgress {
            layers: missing.len(),
            layers_done: 0,
            bytes: missing.iter().map(|l| l.size).sum(),
            bytes_done: 0,
        };
        progress(state);
        for layer in missing {
            let mut blob = BlobWriter::create(layout)?;
            let start = state.bytes_done;
            let mut counting = Counting {
                inner: &mut blob,
                done: 0,
                limit: layer.size,
                over: false,
                report: &|n| {
                    progress(PullProgress {
                        bytes_done: start + n,
                        ..state
                    })
                },
            };
            let fetched = self
                .source
                .fetch_blob(&resolved.source, &layer.digest, &mut counting)
                .await;
            if let Err(e) = fetched {
                return Err(if counting.over {
                    ManageError::Integrity(format!(
                        "layer {}: the blob is larger than the {} bytes the manifest says",
                        layer.digest, layer.size
                    ))
                } else {
                    ManageError::Source(e)
                });
            }
            state.layers_done += 1;
            state.bytes_done = start + layer.size;
            progress(state);
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
        if let Some(d) = &resolved.documents {
            let info = sbx_store::DocumentsInfo {
                manifest_media_type: d.manifest_media_type.clone(),
                config_digest: d.config_digest.clone(),
            };
            write::write_image_documents(layout, &record.id, &d.manifest, &d.config, &info)?;
        }
        write::write_image_record(layout, &record)?;
        write::set_ref(layout, &resolved.reference, &resolved.id)?;
        self.emit(
            sbx_events::Event::new(EVENT_KIND, "pull", record.id.to_string())
                .with("reference", resolved.reference.clone()),
        );
        self.info(record)
    }

    /// 이미지를 OCI image layout tar로 `out`에 쓴다(`sbx save`). 받은 문서와 레이어 blob 그대로라 다시 들이면
    /// (`sbx load`, `docker load`, `podman load`, `skopeo copy oci-archive:`) 같은 이미지 ID다. `reference`가
    /// 있으면 그 이름을 index에 붙인다(`io.containerd.image.name`, 태그는 `org.opencontainers.image.ref.name`).
    /// `docker save`처럼 Docker의 `manifest.json`도 함께 쓴다: `docker load`는 그것만 읽는다. 쓰는 동안 이미지를
    /// 쥔다.
    ///
    /// S1 phase B 자리: prepare가 blob을 지우게 되면 여기서 층 디스크로부터 층 tar를 다시 만든다(지금은 blob 그대로).
    pub fn save(
        &self,
        image: &ImageId,
        reference: Option<&str>,
        out: &mut dyn io::Write,
    ) -> Result<(), ManageError> {
        let _guard = self.store.guard(image).map_err(|e| match e {
            LeaseError::Io(e) => ManageError::Io(e),
            _ => ManageError::ImageNotFound(image.clone()),
        })?;
        let record = self.store.image(image)?;
        let layout = self.store.layout();
        let (manifest, config, info) = self.documents(image)?;
        let mut annotations = serde_json::Map::new();
        if let Some(r) = reference {
            annotations.insert("io.containerd.image.name".into(), r.into());
            if let Some((_, tag)) = r.rsplit_once(':').filter(|(_, t)| !t.contains('/')) {
                annotations.insert("org.opencontainers.image.ref.name".into(), tag.into());
            }
        }
        let index = serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [{
                "mediaType": info.manifest_media_type,
                "digest": image.to_string(),
                "size": manifest.len(),
                "annotations": annotations,
            }],
        });
        let blob = |d: &Digest| format!("blobs/sha256/{}", d.hex());
        // `docker load`의 목록. 태그 참조만 RepoTags가 된다(digest 참조는 Docker가 태그로 받지 않는다).
        let repo_tags: Vec<&str> = reference.filter(|r| !r.contains('@')).into_iter().collect();
        let docker_manifest = serde_json::json!([{
            "Config": blob(&info.config_digest),
            "RepoTags": repo_tags,
            "Layers": record.layers.iter().map(|l| blob(&l.digest)).collect::<Vec<_>>(),
        }]);
        let mut tar = tar::Builder::new(out);
        let mut put = |path: &str, bytes: &[u8]| -> io::Result<()> {
            let mut h = tar::Header::new_ustar();
            h.set_size(bytes.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, bytes)
        };
        put("oci-layout", br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        put(
            "index.json",
            &serde_json::to_vec(&index).map_err(io::Error::other)?,
        )?;
        put(
            "manifest.json",
            &serde_json::to_vec(&docker_manifest).map_err(io::Error::other)?,
        )?;
        put(&blob(image.digest()), &manifest)?;
        put(&blob(&info.config_digest), &config)?;
        let mut seen = std::collections::HashSet::new();
        for l in &record.layers {
            // 같은 레이어가 두 번이면 한 번만 쓴다.
            if !seen.insert(&l.digest) {
                continue;
            }
            let mut f = std::fs::File::open(layout.blob(&l.digest))?;
            let mut h = tar::Header::new_ustar();
            h.set_size(f.metadata()?.len());
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, blob(&l.digest), &mut f)?;
        }
        tar.finish()?;
        Ok(())
    }

    /// 받은 문서: manifest, config, 그 정보. 0.2.1 전에 받은 이미지는 [`ManageError::NoDocuments`].
    fn documents(
        &self,
        image: &ImageId,
    ) -> Result<(Vec<u8>, Vec<u8>, sbx_store::DocumentsInfo), ManageError> {
        let layout = self.store.layout();
        // S1 phase B / 내보내기 자리: 층 디스크에서 층 tar를 다시 만들면 만든 이미지도 manifest를 쓰고 save·push한다.
        if self.store.image(image)?.source == BUILT_SOURCE {
            return Err(ManageError::NotExportable(image.clone()));
        }
        let read = |p: PathBuf| match std::fs::read(&p) {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err(ManageError::NoDocuments(image.clone()))
            }
            Err(e) => Err(ManageError::Io(e)),
        };
        let manifest = read(layout.image_manifest(image))?;
        let config = read(layout.image_config(image))?;
        let info: sbx_store::DocumentsInfo =
            serde_json::from_slice(&read(layout.image_documents(image))?)
                .map_err(|e| ManageError::Io(io::Error::other(e)))?;
        Ok((manifest, config, info))
    }

    /// 이미지 이벤트를 덧붙인다. 실패해도 동작은 계속한다(알림이다).
    fn emit(&self, e: sbx_events::Event) {
        let _ = sbx_events::Log::new(self.store.layout().image_events()).append(e);
    }

    /// 디스크 `max_disks`개(실행기가 붙일 수 있는 수에서 scratch 하나를 뺀 것)로 띄울 층 구성을 만든다
    /// (`sbx_store::stack`). 이미지가 자기 커널을 담고 있으면(`ImageConfig::KERNEL_LABEL`) 그 커널도 꺼낸다.
    /// 무언가 만들었으면 `true`, 모두 이미 있었으면 `false`.
    ///
    /// 위 층부터 하나씩 살펴 층 디스크(`layers/<diff_id>/layer.<형식>`)를 만든다. 같은 층은 다른 이미지가 이미 만들었으면
    /// 그대로 쓰고, 두 프로세스가 함께 만들려 하면 하나만 만든다(`write::lock_build`). 혼자 설 수 없는 층은 표시만 남긴다.
    /// 디스크가 모자라거나 혼자 설 수 없는 층이 있으면 아래 층들을 합친 것(`merged/<chain_id>`)을 만든다.
    pub async fn prepare(
        &self,
        image: &ImageId,
        format: RootfsFormat,
        max_disks: usize,
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
        let layout = self.store.layout();
        // 이 이미지 기록을 보지 못한 정리가 끝나기를 기다린다. 그 뒤에 본 층은 지워지지 않는다.
        write::wait_for_collect(layout)?;
        let record = self.store.image(image)?;
        let mut built = false;
        let parts = loop {
            match self.store.plan(&record, format, max_disks) {
                Plan::Parts(parts) => break parts,
                Plan::Inspect(index) => {
                    built |= self.inspect_layer(&record, index, format, builder.as_ref())?;
                }
            }
        };
        for part in &parts {
            if let Part::Merged { chain, layers } = part {
                built |= self.build_merged(&record, *layers, chain, format, builder.as_ref())?;
            }
        }
        // 커널은 모든 층을 합친 트리에서 찾는다(링크가 층을 넘을 수 있다). 잘못된 라벨이면 이 이미지로는 부팅할 수 없다.
        if let Some(path) = record
            .config
            .kernel()
            .filter(|_| !layout.kernel(image).is_file())
        {
            let work = WorkDir::create(layout, "prepare")?;
            let (tree, spool) = self.merge(&record.layers, work.path())?;
            let target = layout.kernel(image);
            let temp = write::temp_path(&layout.image_dir(image), "kernel");
            let result = extract_kernel(&tree, &spool, path, &temp)
                .and_then(|()| Ok(std::fs::rename(&temp, &target)?));
            if result.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            result?;
            built = true;
        }
        if built {
            self.emit(
                sbx_events::Event::new(EVENT_KIND, "prepare", image.to_string())
                    .with("rootfs_format", format.to_string()),
            );
        }
        Ok(built)
    }

    /// 층 하나의 tar를 blob에서 읽어 항목으로 바꾼다. 내용은 `work`에 둔다. diff_id를 직접 계산해 대조한다.
    ///
    /// blob이 없고 층 디스크가 있으면(빌드가 만든 층) 그 디스크를 읽는다(`erofs::read`). 내용은 `disks`로 연다.
    /// 디스크는 sbx가 쓴 것이고 diff_id 이름 아래에 있으므로 대조하지 않는다. 빈 층([`sbx_store::EMPTY_LAYER`])은
    /// blob이 없어도 항목이 없다.
    pub(crate) fn read_layer(
        &self,
        index: usize,
        l: &sbx_model::Layer,
        work: &Path,
        disks: &mut sbx_rootfs::erofs::read::DiskContents,
    ) -> Result<Vec<sbx_rootfs::LayerEntry>, ManageError> {
        let layout = self.store.layout();
        let blob = layout.blob(&l.digest);
        if !blob.is_file() {
            if sbx_store::stack::is_empty_layer(&l.diff_id) {
                return Ok(Vec::new());
            }
            let disk = layout.layer_disk(&l.diff_id, RootfsFormat::Erofs);
            if disk.is_file() {
                let mut f = BufReader::new(File::open(&disk)?);
                let read = sbx_rootfs::erofs::read::read_layer(&mut f, index).map_err(|e| {
                    ManageError::Layer {
                        index,
                        reason: format!("layer disk {}: {e}", disk.display()),
                    }
                })?;
                disks.add(index, &disk, read.offsets);
                return Ok(read.entries);
            }
        }
        let file = File::open(&blob).map_err(|e| ManageError::Layer {
            index,
            reason: if e.kind() == io::ErrorKind::NotFound {
                format!("blob {} is missing; pull the image again", l.digest)
            } else {
                format!("blob {}: {e}", l.digest)
            },
        })?;
        let decoded: Box<dyn Read> = match l.compression {
            Compression::None => Box::new(BufReader::new(file)),
            Compression::Gzip => Box::new(GzDecoder::new(BufReader::new(file))),
            Compression::Zstd => Box::new(zstd::ZstdReader::new(BufReader::new(file))),
        };
        let mut hashing = HashingReader::new(decoded);
        let entries = layer::read(index, &mut hashing, work)?;
        // tar 끝 표시 뒤에 남은 바이트까지 읽어야 diff_id가 맞는다.
        io::copy(&mut hashing, &mut io::sink())?;
        let actual = hashing.digest();
        if actual != l.diff_id {
            return Err(ManageError::Integrity(format!(
                "layer {index} diff_id: expected {}, got {actual}",
                l.diff_id
            )));
        }
        Ok(entries)
    }

    /// 층들을 아래부터 합친다. 내용은 `work`(또는 층 디스크)에 있다.
    fn merge(
        &self,
        layers: &[sbx_model::Layer],
        work: &Path,
    ) -> Result<(MergedTree, layer::Contents), ManageError> {
        let mut tree = MergedTree::new();
        let mut disks = sbx_rootfs::erofs::read::DiskContents::new();
        for (index, l) in layers.iter().enumerate() {
            tree.apply_layer(self.read_layer(index, l, work, &mut disks)?)?;
        }
        Ok((tree, layer::Contents::new(work, disks)))
    }

    /// 트리를 디스크로 써서 `target`에 놓는다(옆 임시 파일에 쓴 뒤 rename).
    fn write_disk(
        builder: &dyn RootfsBuilder,
        tree: &MergedTree,
        content: &dyn ContentSource,
        target: &Path,
    ) -> Result<(), ManageError> {
        let dir = target
            .parent()
            .ok_or_else(|| io::Error::other("disk path has no parent"))?;
        let name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = write::temp_path(dir, &name);
        let result = (|| -> Result<(), ManageError> {
            let mut out = io::BufWriter::new(File::create(&temp)?);
            builder.build(tree, content, &mut out)?;
            let file = out.into_inner().map_err(|e| e.into_error())?;
            file.sync_all()?;
            std::fs::rename(&temp, target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    /// `index`번 층을 살펴본다: 혼자 설 수 있으면 층 디스크를, 아니면 `needs-lower` 표시를 만든다. 만들었으면 `true`.
    fn inspect_layer(
        &self,
        record: &ImageRecord,
        index: usize,
        format: RootfsFormat,
        builder: &dyn RootfsBuilder,
    ) -> Result<bool, ManageError> {
        let layout = self.store.layout();
        let l = &record.layers[index];
        let _lock = write::lock_build(&layout.layer_dir(&l.diff_id))?;
        if self.store.layer_state(&l.diff_id, format) != LayerState::Unknown {
            return Ok(false);
        }
        let work = WorkDir::create(layout, "layer")?;
        let mut disks = sbx_rootfs::erofs::read::DiskContents::new();
        let lower =
            sbx_rootfs::lower::lower_layer(self.read_layer(index, l, work.path(), &mut disks)?)?;
        match lower.needs_lower {
            Some(why) => write::write_needs_lower(layout, &l.diff_id, &why)?,
            None => Self::write_disk(
                builder,
                &lower.tree,
                &layer::Spool::new(work.path()),
                &layout.layer_disk(&l.diff_id, format),
            )?,
        }
        Ok(true)
    }

    /// 아래 `count`개 층을 합친 디스크를 만든다. 이미 있으면 `false`.
    fn build_merged(
        &self,
        record: &ImageRecord,
        count: usize,
        chain: &Digest,
        format: RootfsFormat,
        builder: &dyn RootfsBuilder,
    ) -> Result<bool, ManageError> {
        let layout = self.store.layout();
        let target = layout.merged_disk(chain, format);
        if target.is_file() {
            return Ok(false);
        }
        let _lock = write::lock_build(&layout.merged_for(chain))?;
        if target.is_file() {
            return Ok(false);
        }
        let work = WorkDir::create(layout, "merge")?;
        let (tree, content) = self.merge(&record.layers[..count], work.path())?;
        Self::write_disk(builder, &tree, &content, &target)?;
        Ok(true)
    }

    /// 디스크 `max_disks`개로 띄울 준비가 됐는지: 층 구성이 모두 있고, 이미지가 자기 커널을 담고 있으면 그 커널도 꺼내
    /// 두었다. 아니면 [`Manager::prepare`]가 할 일이 있다.
    pub fn is_prepared(&self, image: &ImageId, format: RootfsFormat, max_disks: usize) -> bool {
        self.store.is_prepared(image, format, max_disks)
    }

    /// 받아 둔 이미지 목록. ID 순서.
    /// 빌드가 잠깐 쓰는 뿌리 기록([`BUILD_ROOT_SOURCE`])은 이미지가 아니므로 빠진다.
    pub fn images(&self) -> Result<Vec<ImageInfo>, ManageError> {
        self.store
            .images()?
            .into_iter()
            .filter(|r| r.source != BUILD_ROOT_SOURCE)
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
        let kernel_ok =
            record.config.kernel().is_none() || self.store.layout().kernel(&record.id).is_file();
        let prepared = RootfsFormat::ALL
            .iter()
            .copied()
            .filter(|f| kernel_ok && self.store.is_prepared_for_some(&record, *f))
            .collect();
        Ok(ImageInfo {
            record,
            refs,
            prepared,
        })
    }

    /// 참조 하나(정규화한 것)를 이미지에 붙인다. 그 참조가 다른 이미지를 가리키고 있었으면 옮기고, 그 이미지를
    /// 돌려준다(그 이미지는 지우지 않는다. Docker `tag`와 같다). 이벤트 `tag`를 남긴다.
    pub fn tag(&self, image: &ImageId, reference: &str) -> Result<Option<ImageId>, ManageError> {
        if !self.store.has_image(image) {
            return Err(ManageError::ImageNotFound(image.clone()));
        }
        let before = self.store.refs()?.remove(reference);
        write::set_ref(self.store.layout(), reference, image)?;
        self.emit(
            sbx_events::Event::new(EVENT_KIND, "tag", image.to_string())
                .with("reference", reference),
        );
        Ok(before.filter(|b| b != image))
    }

    /// 참조 하나를 지운다. 이미지는 지우지 않는다(Docker `rmi`로 태그만 지울 때와 같다).
    pub fn untag(&self, reference: &str) -> Result<Removal, ManageError> {
        match write::remove_ref(self.store.layout(), reference)? {
            Some(id) => {
                self.emit(
                    sbx_events::Event::new(EVENT_KIND, "untag", id.to_string())
                        .with("reference", reference),
                );
                Ok(Removal {
                    untagged: vec![reference.to_owned()],
                    deleted: None,
                    deleted_layers: Vec::new(),
                    deleted_disks: Vec::new(),
                })
            }
            None => Err(ManageError::RefNotFound(reference.to_owned())),
        }
    }

    /// 게스트 init 내용을 캐시에 둔다(sbx에 내장된 것을 꺼낼 때). 경로는 내용 digest로 정한다
    /// (`guest/sha256-<hex>/<arch>/initramfs`). 같은 내용이 이미 있으면 그대로 두고, 깨져 있으면 다시 쓴다.
    /// 쓰기는 임시 파일에 쓴 뒤 rename한다.
    pub fn install_guest_init(&self, arch: Arch, bytes: &[u8]) -> Result<PathBuf, ManageError> {
        let layout = self.store.layout();
        let digest = sbx_store::digest::sha256(bytes);
        let path = layout.guest_init(&format!("sha256-{}", digest.hex()), arch);
        if std::fs::read(&path).is_ok_and(|have| sbx_store::digest::sha256(&have) == digest) {
            return Ok(path);
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write::atomic_write(&path, bytes)?;
        Ok(path)
    }

    /// 어느 이미지도 쓰지 않는 레이어 blob, 층 디스크, 합친 디스크를 지운다(`system prune`).
    pub fn collect(&self) -> Result<Collected, ManageError> {
        Ok(write::collect(self.store.layout())?)
    }

    /// 캐시에서 이미지들이 차지하는 것들([`StoredPart`]). 있는 것만, 종류와 경로 순서.
    /// 이미지 기록을 하나라도 읽지 못하면 오류다.
    pub fn stored_parts(&self) -> Result<Vec<StoredPart>, ManageError> {
        use std::collections::BTreeMap;
        let layout = self.store.layout();
        let mut parts: BTreeMap<(u8, PathBuf), (PartKind, Vec<ImageId>)> = BTreeMap::new();
        let mut add = |kind: PartKind, path: PathBuf, id: &ImageId| {
            if !path.exists() {
                return;
            }
            let users = &mut parts
                .entry((kind as u8, path))
                .or_insert((kind, Vec::new()))
                .1;
            if !users.contains(id) {
                users.push(id.clone());
            }
        };
        for r in self
            .store
            .images()?
            .into_iter()
            .filter(|r| r.source != BUILD_ROOT_SOURCE)
        {
            add(PartKind::Image, layout.image_dir(&r.id), &r.id);
            for l in &r.layers {
                add(PartKind::Blob, layout.blob(&l.digest), &r.id);
                add(PartKind::Layer, layout.layer_dir(&l.diff_id), &r.id);
            }
            let mut chains = sbx_store::stack::chain_ids(&r.layers);
            if r.layers.is_empty() {
                chains.push(sbx_store::stack::empty_chain());
            }
            for c in chains {
                add(PartKind::Merged, layout.merged_for(&c), &r.id);
            }
        }
        Ok(parts
            .into_iter()
            .map(|((_, path), (kind, mut users))| {
                users.sort();
                StoredPart { kind, path, users }
            })
            .collect())
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
        for r in &untagged {
            self.emit(
                sbx_events::Event::new(EVENT_KIND, "untag", image.to_string())
                    .with("reference", r.clone()),
            );
        }
        self.emit(sbx_events::Event::new(
            EVENT_KIND,
            "delete",
            image.to_string(),
        ));
        let collected = write::collect(layout)?;
        Ok(Removal {
            untagged,
            deleted: Some(image.clone()),
            deleted_layers: collected.blobs,
            deleted_disks: collected.layers,
        })
    }
}

/// 배치 1(0.2)의 cache_dir을 배치 2로 옮긴다: 이미지마다 모든 층을 합쳐 두었던 `images/<hex>/rootfs.<형식>`을 지운다.
/// 지운 파일. 다음 prepare(`sbx run`이 부른다)가 받아 둔 blob에서 층 디스크를 만든다. blob이 없는 이미지는 그때
/// "다시 받으라"는 오류가 나고, `sbx pull`이 없는 blob만 받는다.
pub fn migrate_from_layout_1(cache_dir: &Path) -> io::Result<Vec<PathBuf>> {
    write::remove_legacy_rootfs(&sbx_store::CacheLayout::new(cache_dir))
}

/// 이미지 안 `path`(절대 경로, 링크는 이미지 안에서 따라간다)의 일반 파일을 `out`에 쓴다.
fn extract_kernel(
    tree: &MergedTree,
    spool: &layer::Contents,
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
