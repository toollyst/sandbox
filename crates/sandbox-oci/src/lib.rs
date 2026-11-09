//! OCI 이미지 소스. `sandbox-image`의 [`ImageSource`]를 OCI Distribution Spec과 Image Spec으로 구현한다.
//!
//! OCI는 입력 형식이다. OCI 자료형은 이 크레이트 밖으로 나가지 않고, 여기서 내부 모델
//! (`ImageConfig`, 레이어 목록)로 변환된다. 그래서 적용 범위를 넓힐 때는 이 크레이트만 고친다.
//!
//! - 무엇을 받고 무엇을 받지 않는지: [`scope`]
//! - 이미지 참조 해석: [`reference`](mod@reference)
//! - 문서 해석: [`manifest`]
//! - 레지스트리 통신: [`registry`]
//!
//! 해석 순서 ([`ImageSource::resolve`]):
//!
//! 1. 참조를 해석한다. digest가 있으면 digest로, 없으면 태그로 manifest를 받는다.
//! 2. index면 linux/호스트 아키텍처 manifest를 고르고 다시 받는다.
//! 3. 받은 문서는 모두 직접 sha256을 계산해 기대한 digest와 대조한다. 이미지 ID는 플랫폼 manifest의 digest다.
//! 4. config를 받아 digest를 대조하고, os와 아키텍처가 맞는지, `diff_ids` 개수가 레이어 개수와 같은지 확인한다.
//!
//! 레이어 blob의 digest 대조는 받는 쪽(`sandbox-manage`)이 한다.
//!
//! - 의존: `sandbox-model`, `sandbox-image`

use std::io::Write;

use sha2::{Digest as _, Sha256};

use sandbox_image::{ImageError, ImageSource, Platform, ResolvedImage};
use sandbox_model::{BoxFuture, Digest, ImageId, Layer};

pub mod manifest;
pub mod reference;
pub mod registry;
pub mod scope;

pub use reference::Reference;

use manifest::Document;
use registry::{MAX_DOCUMENT_BYTES, Registry, read_limited};

fn sha256(bytes: &[u8]) -> Digest {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Digest::parse(&format!("sha256:{hex}")).unwrap_or_else(|_| unreachable!("valid sha256 hex"))
}

fn verify(what: &str, bytes: &[u8], expected: &Digest) -> Result<(), ImageError> {
    let actual = sha256(bytes);
    if &actual == expected {
        Ok(())
    } else {
        Err(ImageError::Integrity(format!(
            "{what}: expected {expected}, got {actual}"
        )))
    }
}

/// OCI 레지스트리 이미지 소스.
#[derive(Debug)]
pub struct OciImageSource {
    registry: Registry,
}

impl OciImageSource {
    /// 만든다.
    pub fn new() -> Result<Self, ImageError> {
        Ok(Self {
            registry: Registry::new()?,
        })
    }

    async fn manifest(
        &self,
        r: &Reference,
        target: &str,
    ) -> Result<(Vec<u8>, Option<String>), ImageError> {
        let resp = self
            .registry
            .get(
                r.api_host(),
                &r.repository,
                &format!("manifests/{target}"),
                scope::MANIFEST_MEDIA_TYPES,
            )
            .await?;
        read_limited(resp, MAX_DOCUMENT_BYTES).await
    }

    async fn resolve_inner(
        &self,
        input: &str,
        platform: Platform,
    ) -> Result<ResolvedImage, ImageError> {
        let r = Reference::parse(input).map_err(ImageError::Reference)?;
        let target = match (&r.digest, &r.tag) {
            (Some(d), _) => d.to_string(),
            (None, Some(t)) => t.clone(),
            (None, None) => unreachable!("Reference always has a tag or a digest"),
        };
        let (bytes, content_type) = self.manifest(&r, &target).await?;
        if let Some(d) = &r.digest {
            verify("manifest", &bytes, d)?;
        }

        let (manifest_bytes, manifest) =
            match manifest::parse_document(content_type.as_deref(), &bytes)? {
                Document::Manifest(m) => (bytes, m),
                Document::Index(entries) => {
                    let entry = manifest::select(&entries, platform.arch).ok_or_else(|| {
                        ImageError::PlatformNotFound {
                            reference: r.to_string(),
                            arch: platform.arch,
                        }
                    })?;
                    let (b, ct) = self.manifest(&r, entry.digest.as_str()).await?;
                    verify("platform manifest", &b, &entry.digest)?;
                    match manifest::parse_document(ct.as_deref(), &b)? {
                        Document::Manifest(m) => (b, m),
                        Document::Index(_) => {
                            return Err(ImageError::Unsupported("nested image index".into()));
                        }
                    }
                }
            };
        let id = ImageId::new(sha256(&manifest_bytes));

        if manifest.config_size > MAX_DOCUMENT_BYTES as u64 {
            return Err(ImageError::Invalid("image config too large".into()));
        }
        let resp = self
            .registry
            .get(
                r.api_host(),
                &r.repository,
                &format!("blobs/{}", manifest.config),
                &[],
            )
            .await?;
        let (config_bytes, _) = read_limited(resp, MAX_DOCUMENT_BYTES).await?;
        verify("image config", &config_bytes, &manifest.config)?;
        let parsed = manifest::parse_config(&config_bytes)?;

        if parsed.os != "linux" || parsed.arch != platform.arch.oci_name() {
            return Err(ImageError::PlatformNotFound {
                reference: r.to_string(),
                arch: platform.arch,
            });
        }
        if parsed.diff_ids.len() != manifest.layers.len() {
            return Err(ImageError::Invalid(format!(
                "{} layers but {} diff_ids",
                manifest.layers.len(),
                parsed.diff_ids.len()
            )));
        }
        let layers = manifest
            .layers
            .into_iter()
            .zip(parsed.diff_ids)
            .map(|(l, diff_id)| Layer {
                digest: l.digest,
                diff_id,
                size: l.size,
                compression: l.compression,
            })
            .collect();

        Ok(ResolvedImage {
            id,
            reference: r.to_string(),
            source: format!("{}/{}", r.registry, r.repository),
            platform,
            config: parsed.config,
            layers,
            ignored_fields: parsed.ignored_fields,
        })
    }

    async fn fetch_blob_inner(
        &self,
        source: &str,
        digest: &Digest,
        sink: &mut (dyn Write + Send),
    ) -> Result<u64, ImageError> {
        let r = Reference::parse(source).map_err(ImageError::Reference)?;
        let mut resp = self
            .registry
            .get(r.api_host(), &r.repository, &format!("blobs/{digest}"), &[])
            .await?;
        let mut written = 0u64;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| ImageError::Transport(e.to_string()))?
        {
            sink.write_all(&chunk)?;
            written += chunk.len() as u64;
        }
        Ok(written)
    }
}

impl ImageSource for OciImageSource {
    fn resolve<'a>(
        &'a self,
        reference: &'a str,
        platform: Platform,
    ) -> BoxFuture<'a, Result<ResolvedImage, ImageError>> {
        Box::pin(self.resolve_inner(reference, platform))
    }

    fn fetch_blob<'a>(
        &'a self,
        source: &'a str,
        digest: &'a Digest,
        sink: &'a mut (dyn Write + Send),
    ) -> BoxFuture<'a, Result<u64, ImageError>> {
        Box::pin(self.fetch_blob_inner(source, digest, sink))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_model::Arch;

    /// Docker Hub에 실제로 접속한다. `cargo test -p sandbox-oci -- --ignored`로 돌린다.
    #[tokio::test]
    #[ignore = "network"]
    async fn resolves_alpine_from_docker_hub() {
        let arch = Arch::host().unwrap();
        let src = OciImageSource::new().unwrap();
        let img = src.resolve("alpine:3.20", Platform { arch }).await.unwrap();
        assert_eq!(img.reference, "docker.io/library/alpine:3.20");
        assert_eq!(img.source, "docker.io/library/alpine");
        assert_eq!(img.layers.len(), 1);
        assert_eq!(img.config.cmd, vec!["/bin/sh"]);

        let mut buf = Vec::new();
        let n = src
            .fetch_blob(&img.source, &img.layers[0].digest, &mut buf)
            .await
            .unwrap();
        assert_eq!(n, img.layers[0].size);
        assert_eq!(sha256(&buf), img.layers[0].digest);
    }
}
