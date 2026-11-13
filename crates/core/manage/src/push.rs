//! `push`: 저장한 이미지를 바이트 그대로 올린다. 규칙은 크레이트 문서의 "명시적 동작"에 있다.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;

use sbx_image::{BlobContent, ImageDestination};
use sbx_model::{Digest, ImageId};
use sbx_store::LeaseError;
use sbx_store::digest::sha256;

use crate::{EVENT_KIND, ManageError, Manager, PushProgress, Pushed, PushedBlob};

/// cache_dir의 blob 파일.
struct FileBlob {
    path: PathBuf,
    size: u64,
}

impl BlobContent for FileBlob {
    fn size(&self) -> u64 {
        self.size
    }

    fn open(&self, offset: u64) -> io::Result<Box<dyn Read + Send + Sync>> {
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(offset))?;
        Ok(Box::new(f))
    }
}

/// 올릴 blob 하나.
enum Blob {
    File(FileBlob),
    Bytes(Vec<u8>),
}

impl Blob {
    fn content(&self) -> &dyn BlobContent {
        match self {
            Blob::File(f) => f,
            Blob::Bytes(b) => b,
        }
    }
}

impl Manager {
    /// 이미지를 `dest`의 `reference`(정규화한 것, 태그 포함)로 올린다. 순서와 규칙은 크레이트 문서에 있다.
    /// 끝나면 이벤트 `push`를 남긴다.
    pub async fn push(
        &self,
        dest: &dyn ImageDestination,
        image: &ImageId,
        reference: &str,
    ) -> Result<Pushed, ManageError> {
        self.push_with(dest, image, reference, &|_| {}).await
    }

    /// [`push`](Self::push)와 같고, 올리는 동안 `progress`에 진행을 알린다. 올릴 blob 목록이 정해지면 한 번, 그 뒤
    /// 바이트를 보내거나 blob 하나가 끝날 때마다 부른다.
    pub async fn push_with(
        &self,
        dest: &dyn ImageDestination,
        image: &ImageId,
        reference: &str,
        progress: &(dyn Fn(PushProgress) + Sync),
    ) -> Result<Pushed, ManageError> {
        let _guard = self.store.guard(image).map_err(|e| match e {
            LeaseError::Io(e) => ManageError::Io(e),
            _ => ManageError::ImageNotFound(image.clone()),
        })?;
        let record = self.store.image(image)?;
        let (manifest, config, info) = self.documents(image)?;
        let actual = sha256(&manifest);
        if &actual != image.digest() {
            return Err(ManageError::Integrity(format!(
                "the stored manifest of {image} hashes to {actual}"
            )));
        }
        if sha256(&config) != info.config_digest {
            return Err(ManageError::Integrity(format!(
                "the stored config of {image} does not match {}",
                info.config_digest
            )));
        }
        let layout = self.store.layout();
        let mut blobs: Vec<(Digest, Blob)> = Vec::new();
        for l in &record.layers {
            if blobs.iter().any(|(d, _)| *d == l.digest) {
                continue;
            }
            let path = layout.blob(&l.digest);
            let size = std::fs::metadata(&path)
                .map_err(|e| ManageError::Layer {
                    index: blobs.len(),
                    reason: format!("blob {}: {e}", l.digest),
                })?
                .len();
            if size != l.size {
                return Err(ManageError::Integrity(format!(
                    "layer {}: the stored blob is {size} bytes, the manifest says {}",
                    l.digest, l.size
                )));
            }
            blobs.push((l.digest.clone(), Blob::File(FileBlob { path, size })));
        }
        blobs.push((info.config_digest.clone(), Blob::Bytes(config)));

        let target = dest
            .target(reference, Some(&record.source))
            .map_err(ManageError::Source)?;
        let mut state = PushProgress {
            blobs: blobs.len(),
            blobs_done: 0,
            bytes: blobs.iter().map(|(_, b)| b.content().size()).sum(),
            bytes_done: 0,
        };
        progress(state);
        let mut pushed = Vec::new();
        for (digest, blob) in &blobs {
            let content = blob.content();
            let start = state.bytes_done;
            let how = dest
                .push_blob(&target, digest, content, &|n| {
                    progress(PushProgress {
                        bytes_done: start + n.min(content.size()),
                        ..state
                    })
                })
                .await
                .map_err(ManageError::Source)?;
            state.blobs_done += 1;
            state.bytes_done = start + content.size();
            progress(state);
            pushed.push(PushedBlob {
                digest: digest.clone(),
                size: content.size(),
                how,
            });
        }
        dest.push_manifest(&target, &info.manifest_media_type, &manifest)
            .await
            .map_err(ManageError::Source)?;
        self.emit(
            sbx_events::Event::new(EVENT_KIND, "push", image.to_string())
                .with("reference", target.reference.clone()),
        );
        Ok(Pushed {
            reference: target.reference,
            id: image.clone(),
            manifest_size: manifest.len() as u64,
            blobs: pushed,
        })
    }
}
