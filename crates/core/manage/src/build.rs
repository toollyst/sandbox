//! `sbx build`의 저장소 쪽: 층 저장, 빌드 캐시 기록, 빌드 VM의 뿌리 기록, 만든 이미지 기록.
//!
//! - **층**([`Manager::add_layer`]): 빌드가 낸 압축하지 않은 층 tar를 읽어(믿지 않는 입력이다. 게스트가 보낸 것일 수
//!   있다) diff_id를 직접 계산하고 층 디스크(`layers/<diff_id>/layer.<형식>`)로 둔다. 압축 blob은 만들지 않는다.
//!   혼자 설 수 없는 층(`sbx_rootfs::lower`)만 tar를 그대로 blob(압축 없음)으로 두고 `needs-lower` 표시를 남긴다.
//!   합칠 때 그 blob을 읽는다. 항목이 없는 층은 [`sbx_store::EMPTY_LAYER`]이고 아무것도 두지 않는다.
//! - **캐시 기록**: `build/<키>.json`. 가리키는 층이 남아 있을 때만 쓴다([`Manager::build_cache`]).
//! - **쥐기**: 빌드가 쓰는 층은 끝날 때까지 [`Manager::claim_layers`]로 쥔다. 그동안 정리가 지우지 않는다.
//! - **뿌리 기록**([`BUILD_ROOT_SOURCE`]): 빌드 VM은 이미지 하나로 뜨므로 그때까지의 층으로 이미지 기록을 하나 쓴다.
//!   목록(`images`)에 보이지 않고 이벤트도 없다. 빌드가 끝나면 지우고, 남은 것은 `system prune`이 지운다.
//! - **만든 이미지**([`BUILT_SOURCE`]): 이미지 ID는 config 문서의 sha256이다. config 문서(`config.json`)는 두고 manifest는
//!   두지 않는다. 그래서 `save`·`push`는 [`ManageError::NotExportable`]이다(층 tar를 다시 만드는 단계의 자리).
//!   이벤트 `build`(속성 `reference`, 태그마다)를 남긴다.
//! - **정리**: 캐시 기록이 이긴다. 만든 이미지를 지워도 캐시가 가리키는 층은 남는다. [`Manager::prune_build_cache`]가
//!   기록을 지우고 나면 다음 정리가 층을 지운다.

use std::fs::File;
use std::io::{self, BufReader};
use std::path::Path;

use sbx_model::{Arch, Compression, Digest, ImageConfig, ImageId, Layer, RootfsFormat};
use sbx_rootfs::LayerEntry;
use sbx_store::digest::{HashingReader, sha256};
use sbx_store::write::{self, BlobClaim, BlobWriter, WorkDir};
use sbx_store::{BuildCacheRecord, ImageRecord, LayerState};

use crate::built::{BUILD_ROOT_SOURCE, BUILT_SOURCE, BuildCacheUsage, LayerContents};
use crate::{EVENT_KIND, ManageError, Manager, layer};

fn empty_layer() -> Layer {
    let d = Digest::parse(sbx_store::EMPTY_LAYER).expect("a valid digest");
    Layer {
        digest: d.clone(),
        diff_id: d,
        size: 1024,
        compression: Compression::None,
    }
}

impl Manager {
    /// 빌드가 쓰는 층(diff_id와 blob digest)을 쥔다. 돌려받은 것을 버릴 때까지 정리가 지우지 않는다.
    pub fn claim_layers(&self, layers: &[Layer]) -> Result<BlobClaim, ManageError> {
        let mut ds: Vec<Digest> = Vec::new();
        for l in layers {
            ds.push(l.digest.clone());
            if l.diff_id != l.digest {
                ds.push(l.diff_id.clone());
            }
        }
        Ok(write::claim_blobs(self.store.layout(), &ds)?)
    }

    /// 압축하지 않은 층 tar를 층으로 둔다(규칙은 모듈 문서). 돌려받은 쥠을 캐시 기록이나 이미지 기록을 쓸 때까지 쥔다.
    pub fn add_layer(&self, tar: &Path) -> Result<(Layer, BlobClaim), ManageError> {
        let layout = self.store.layout();
        let size = std::fs::metadata(tar)?.len();
        let work = WorkDir::create(layout, "build-layer")?;
        let mut hashing = HashingReader::new(BufReader::new(File::open(tar)?));
        let entries = layer::read(0, &mut hashing, work.path())?;
        io::copy(&mut hashing, &mut io::sink())?;
        let diff_id = hashing.digest();
        if entries.is_empty() {
            let l = empty_layer();
            let claim = self.claim_layers(std::slice::from_ref(&l))?;
            return Ok((l, claim));
        }
        let layer = Layer {
            digest: diff_id.clone(),
            diff_id: diff_id.clone(),
            size,
            compression: Compression::None,
        };
        let claim = self.claim_layers(std::slice::from_ref(&layer))?;
        let _lock = write::lock_build(&layout.layer_dir(&diff_id))?;
        let missing: Vec<_> = self
            .builders
            .iter()
            .filter(|b| self.store.layer_state(&diff_id, b.format()) == LayerState::Unknown)
            .collect();
        if missing.is_empty() {
            return Ok((layer, claim));
        }
        let lower = sbx_rootfs::lower::lower_layer(entries)?;
        match lower.needs_lower {
            Some(why) => {
                if !layout.blob(&diff_id).is_file() {
                    let mut blob = BlobWriter::create(layout)?;
                    io::copy(&mut File::open(tar)?, &mut blob)?;
                    blob.commit(layout, &diff_id)?;
                }
                write::write_needs_lower(layout, &diff_id, &why)?;
            }
            None => {
                for b in missing {
                    Self::write_disk(
                        b.as_ref(),
                        &lower.tree,
                        &layer::Spool::new(work.path()),
                        &layout.layer_disk(&diff_id, b.format()),
                    )?;
                }
            }
        }
        Ok((layer, claim))
    }

    /// 층 하나의 항목과 그 내용. `index`는 `ContentRef::layer`에 쓰는 번호다.
    pub fn layer_entries(
        &self,
        l: &Layer,
        index: usize,
    ) -> Result<(Vec<LayerEntry>, LayerContents), ManageError> {
        let work = WorkDir::create(self.store.layout(), "build-read")?;
        let mut disks = sbx_rootfs::erofs::read::DiskContents::new();
        let entries = self.read_layer(index, l, work.path(), &mut disks)?;
        let contents = layer::Contents::new(work.path(), disks);
        Ok((
            entries,
            LayerContents {
                _work: work,
                contents,
            },
        ))
    }

    /// 층을 쓸 수 있는지: 빈 층이거나, blob이 있거나, 층 디스크가 있다.
    fn layer_usable(&self, l: &Layer) -> bool {
        let layout = self.store.layout();
        sbx_store::stack::is_empty_layer(&l.diff_id)
            || layout.blob(&l.digest).is_file()
            || RootfsFormat::ALL
                .iter()
                .any(|f| layout.layer_disk(&l.diff_id, *f).is_file())
    }

    /// 캐시 기록 하나. 없거나 그 층이 더는 없으면 `None`.
    pub fn build_cache(&self, key: &str) -> Result<Option<BuildCacheRecord>, ManageError> {
        Ok(self
            .store
            .build_cache(key)?
            .filter(|r| self.layer_usable(&r.layer)))
    }

    /// 캐시 기록을 쓴다.
    pub fn put_build_cache(&self, key: &str, record: &BuildCacheRecord) -> Result<(), ManageError> {
        Ok(write::write_build_cache(self.store.layout(), key, record)?)
    }

    /// 빌드 VM의 뿌리 기록을 쓴다(규칙은 모듈 문서). 같은 층이면 같은 ID다.
    pub fn put_build_root(&self, layers: &[Layer], arch: Arch) -> Result<ImageId, ManageError> {
        let mut text = String::from("sbx-build-root\n");
        for l in layers {
            text.push_str(&format!("{} {}\n", l.digest, l.diff_id));
        }
        let id = ImageId::new(sha256(text.as_bytes()));
        let layout = self.store.layout();
        if !self.store.has_image(&id) {
            write::create_image_dir(layout, &id)?;
            write::write_image_record(
                layout,
                &ImageRecord {
                    id: id.clone(),
                    reference: String::new(),
                    source: BUILD_ROOT_SOURCE.into(),
                    arch,
                    config: ImageConfig::default(),
                    layers: layers.to_vec(),
                    ignored_fields: Vec::new(),
                },
            )?;
        }
        Ok(id)
    }

    /// 뿌리 기록을 지운다. 다른 빌드의 VM이 쓰고 있으면 두고 `false`.
    pub fn remove_build_root(&self, id: &ImageId) -> Result<bool, ManageError> {
        let layout = self.store.layout();
        match self.store.image(id) {
            Ok(r) if r.source == BUILD_ROOT_SOURCE => {}
            _ => return Ok(false),
        }
        match write::try_lock_image(layout, id)? {
            Some(lock) => {
                write::remove_image(layout, id, lock)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// 만든 이미지를 둔다(규칙은 모듈 문서). `tags`는 정규화한 참조들이다.
    pub fn commit_built(
        &self,
        config_json: &[u8],
        config: ImageConfig,
        layers: Vec<Layer>,
        arch: Arch,
        tags: &[String],
    ) -> Result<ImageId, ManageError> {
        let id = ImageId::new(sha256(config_json));
        let layout = self.store.layout();
        write::create_image_dir(layout, &id)?;
        write::atomic_write(&layout.image_config(&id), config_json)?;
        write::write_image_record(
            layout,
            &ImageRecord {
                id: id.clone(),
                reference: tags.first().cloned().unwrap_or_default(),
                source: BUILT_SOURCE.into(),
                arch,
                config,
                layers,
                ignored_fields: Vec::new(),
            },
        )?;
        for t in tags {
            write::set_ref(layout, t, &id)?;
        }
        let mut e = sbx_events::Event::new(EVENT_KIND, "build", id.to_string());
        if let Some(t) = tags.first() {
            e = e.with("reference", t.clone());
        }
        self.emit(e);
        Ok(id)
    }

    /// 만든 이미지나 받은 이미지의 config 문서(OCI image config JSON). 0.2.1 전에 받은 이미지는 `None`.
    pub fn config_document(&self, id: &ImageId) -> Result<Option<Vec<u8>>, ManageError> {
        match std::fs::read(self.store.layout().image_config(id)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// 빌드 캐시 기록을 모두 지우고, 쓰는 VM이 없는 뿌리 기록도 지운다(`system prune`). 지운 캐시 기록 수.
    /// 층은 지우지 않는다. 다음 [`Manager::collect`]가 지운다.
    pub fn prune_build_cache(&self) -> Result<usize, ManageError> {
        let layout = self.store.layout();
        let n = write::remove_build_caches(layout)?;
        for r in self.store.images()? {
            if r.source == BUILD_ROOT_SOURCE {
                self.remove_build_root(&r.id)?;
            }
        }
        Ok(n)
    }

    /// 빌드 캐시가 차지하는 것(`system df`).
    pub fn build_cache_usage(&self) -> Result<BuildCacheUsage, ManageError> {
        let layout = self.store.layout();
        let records = self.store.build_caches()?;
        let mut used = std::collections::BTreeSet::new();
        for r in self.store.images()? {
            if r.source == BUILD_ROOT_SOURCE {
                continue;
            }
            for l in r.layers {
                used.insert(l.diff_id);
            }
        }
        let mut out = BuildCacheUsage {
            records: records.len(),
            ..Default::default()
        };
        let mut seen = std::collections::BTreeSet::new();
        for (_, rec) in records {
            if !seen.insert(rec.layer.diff_id.clone()) {
                continue;
            }
            let paths = [
                layout.layer_dir(&rec.layer.diff_id),
                layout.blob(&rec.layer.digest),
            ];
            for p in paths.into_iter().filter(|p| p.exists()) {
                if used.contains(&rec.layer.diff_id) {
                    out.shared.push(p);
                } else {
                    out.only_cache.push(p);
                }
            }
        }
        Ok(out)
    }
}
