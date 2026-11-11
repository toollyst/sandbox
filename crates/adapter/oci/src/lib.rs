//! OCI 이미지 소스. `sbx-image`의 [`ImageSource`]를 OCI Distribution Spec과 Image Spec으로 구현한다.
//!
//! OCI는 입력 형식이다. OCI 자료형은 이 크레이트 밖으로 나가지 않고, 여기서 내부 모델
//! (`ImageConfig`, 레이어 목록)로 변환된다. 그래서 적용 범위를 넓힐 때는 이 크레이트만 고친다.
//!
//! - 무엇을 받고 무엇을 받지 않는지: [`scope`]
//! - 이미지 참조 해석: [`reference`](mod@reference)
//! - 문서 해석: [`manifest`]
//! - 레지스트리 통신: [`registry`]
//! - OCI image layout(`sbx load`, 디렉터리나 tar): [`layout`]
//! - 레지스트리 자격 증명(Docker config.json, containers-auth.json, credential helper): [`auth`]
//! - 미러, 이름 바꾸기, 차단(containers registries.conf): [`registries`]
//!
//! 해석 순서 ([`ImageSource::resolve`]):
//!
//! 1. 참조를 해석한다. `registries.conf`가 있으면 받을 곳(미러들, 그다음 주 위치)을 정하고([`registries`]) 앞에서부터
//!    시도한다. 한 곳이 실패하면(없음, 연결, 거부, 검증 실패) 다음 곳으로 가고, 모두 실패하면 주 위치의 오류다.
//!    레이어는 문서를 받은 곳에서 받는다. digest가 있으면 digest로, 없으면 태그로 manifest를 받는다.
//! 2. index면 linux/호스트 아키텍처 manifest를 고르고 다시 받는다.
//! 3. 받은 문서는 모두 직접 sha256을 계산해 기대한 digest와 대조한다. 이미지 ID는 플랫폼 manifest의 digest다.
//! 4. config를 받아 digest를 대조하고, os와 아키텍처가 맞는지, `diff_ids` 개수가 레이어 개수와 같은지 확인한다.
//!
//! 레이어 blob의 digest 대조는 받는 쪽(`sbx-manage`)이 한다.
//!
//! - 의존: `sbx-model`, `sbx-image`

use std::io::Write;

use sha2::{Digest as _, Sha256};

use sbx_image::{ImageError, ImageSource, Platform, ResolvedImage};
use sbx_model::{BoxFuture, Digest, ImageId, Layer};

pub mod auth;
pub mod layout;
pub mod manifest;
pub mod reference;
pub mod registries;
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

/// 플랫폼 manifest와 그 config에서 [`ResolvedImage`]를 만든다. config의 digest를 대조하고, OS·아키텍처와
/// 레이어 수를 확인한다. 레지스트리와 OCI layout(`sbx load`)이 함께 쓴다.
fn assemble(
    r: &Reference,
    source: String,
    platform: Platform,
    manifest_bytes: Vec<u8>,
    manifest: manifest::Manifest,
    config_bytes: Vec<u8>,
) -> Result<ResolvedImage, ImageError> {
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
        id: ImageId::new(sha256(&manifest_bytes)),
        reference: r.to_string(),
        source,
        platform,
        config: parsed.config,
        layers,
        ignored_fields: parsed.ignored_fields,
        documents: Some(sbx_image::Documents {
            manifest: manifest_bytes,
            manifest_media_type: manifest.media_type,
            config: config_bytes,
            config_digest: manifest.config,
        }),
    })
}

/// OCI 레지스트리 이미지 소스.
///
/// 자격 증명 파일과 `registries.conf`는 레지스트리에 처음 갈 때 읽는다. 그래서 레지스트리를 쓰지 않는 일
/// (`images`, `rmi`, `save` 등)은 그 파일이 틀려도 막히지 않는다.
#[derive(Debug, Default)]
pub struct OciImageSource {
    auth: Option<auth::AuthFiles>,
    conf: Option<registries::ConfFiles>,
    state: std::sync::OnceLock<State>,
}

#[derive(Debug)]
struct State {
    registry: Registry,
    registries: registries::Registries,
}

impl OciImageSource {
    /// 만든다. 익명으로 받고 `registries.conf`를 읽지 않는다.
    pub fn new() -> Result<Self, ImageError> {
        Ok(Self::default())
    }

    /// 401이면 `files`에서 레지스트리 자격 증명을 찾는다.
    pub fn with_auth(files: auth::AuthFiles) -> Result<Self, ImageError> {
        Ok(Self {
            auth: Some(files),
            ..Self::default()
        })
    }

    /// 미러, 이름 바꾸기, 차단, `insecure`를 이 `registries.conf` 파일들대로([`registries`]).
    pub fn with_registries(mut self, files: registries::ConfFiles) -> Self {
        self.conf = Some(files);
        self
    }

    fn state(&self) -> Result<&State, ImageError> {
        if let Some(s) = self.state.get() {
            return Ok(s);
        }
        let registries = match &self.conf {
            Some(f) => registries::Registries::load(f)?,
            None => registries::Registries::default(),
        };
        let mut registry = Registry::new()?.with_insecure(registries.insecure_hosts())?;
        if let Some(a) = &self.auth {
            registry = registry.with_auth(a.clone());
        }
        // 동시에 처음 부른 쪽이 여럿이면 하나만 남는다(어느 것이든 같다).
        let _ = self.state.set(State {
            registry,
            registries,
        });
        self.state
            .get()
            .ok_or_else(|| ImageError::Transport("registry client".into()))
    }

    async fn manifest(
        &self,
        r: &Reference,
        target: &str,
    ) -> Result<(Vec<u8>, Option<String>), ImageError> {
        let resp = self
            .state()?
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
        let wanted = Reference::parse(input).map_err(ImageError::Reference)?;
        let mut mirror_errors = Vec::new();
        for e in self.state()?.registries.endpoints(&wanted)? {
            match self.resolve_at(&wanted, &e.reference, platform).await {
                Ok(img) => return Ok(img),
                Err(err) if e.mirror => {
                    mirror_errors.push(format!("mirror {}: {err}", e.reference))
                }
                Err(err) if mirror_errors.is_empty() => return Err(err),
                Err(err) => {
                    return Err(ImageError::Transport(format!(
                        "{err} (after {})",
                        mirror_errors.join("; ")
                    )));
                }
            }
        }
        unreachable!("endpoints always ends with the registry itself")
    }

    /// `at`(바꾼 참조)에서 받는다. 결과의 참조는 `wanted`, 출처는 `at`이다.
    async fn resolve_at(
        &self,
        wanted: &Reference,
        r: &Reference,
        platform: Platform,
    ) -> Result<ResolvedImage, ImageError> {
        let target = match (&r.digest, &r.tag) {
            (Some(d), _) => d.to_string(),
            (None, Some(t)) => t.clone(),
            (None, None) => unreachable!("Reference always has a tag or a digest"),
        };
        let (bytes, content_type) = self.manifest(r, &target).await?;
        if let Some(d) = &r.digest {
            verify("manifest", &bytes, d)?;
        }

        let (manifest_bytes, manifest) =
            match manifest::parse_document(content_type.as_deref(), &bytes)? {
                Document::Manifest(m) => (bytes, m),
                Document::Index(entries) => {
                    let entry = manifest::select(&entries, platform.arch).ok_or_else(|| {
                        ImageError::PlatformNotFound {
                            reference: wanted.to_string(),
                            arch: platform.arch,
                        }
                    })?;
                    let (b, ct) = self.manifest(r, entry.digest.as_str()).await?;
                    verify("platform manifest", &b, &entry.digest)?;
                    match manifest::parse_document(ct.as_deref(), &b)? {
                        Document::Manifest(m) => (b, m),
                        Document::Index(_) => {
                            return Err(ImageError::Unsupported("nested image index".into()));
                        }
                    }
                }
            };
        if manifest.config_size > MAX_DOCUMENT_BYTES as u64 {
            return Err(ImageError::Invalid("image config too large".into()));
        }
        let resp = self
            .state()?
            .registry
            .get(
                r.api_host(),
                &r.repository,
                &format!("blobs/{}", manifest.config),
                &[],
            )
            .await?;
        let (config_bytes, _) = read_limited(resp, MAX_DOCUMENT_BYTES).await?;
        assemble(
            wanted,
            format!("{}/{}", r.registry, r.repository),
            platform,
            manifest_bytes,
            manifest,
            config_bytes,
        )
    }

    async fn fetch_blob_inner(
        &self,
        source: &str,
        digest: &Digest,
        sink: &mut (dyn Write + Send),
    ) -> Result<u64, ImageError> {
        let r = Reference::parse(source).map_err(ImageError::Reference)?;
        let mut resp = self
            .state()?
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
    use sbx_model::Arch;

    /// Docker Hub에 실제로 접속한다. `cargo test -p sbx-oci -- --ignored`로 돌린다.
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

    /// 경로 → (Content-Type, 본문)을 주는 레지스트리. 없는 경로는 404. 받은 경로를 남긴다.
    fn fake_registry(
        docs: std::collections::HashMap<String, (String, Vec<u8>)>,
    ) -> (u16, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { return };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    continue;
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                }
                let path = first.split(' ').nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(path.clone());
                let head = match docs.get(&path) {
                    Some((ct, body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes()
                    .into_iter()
                    .chain(body.iter().copied())
                    .collect::<Vec<u8>>(),
                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = c.write_all(&head);
            }
        });
        (port, seen)
    }

    #[tokio::test]
    async fn mirrors_are_tried_first_and_the_name_stays_the_one_asked_for() {
        let arch = Arch::host().unwrap();
        let layer = b"layer bytes".to_vec();
        let config = format!(
            r#"{{"architecture":"{}","os":"linux","config":{{}},"rootfs":{{"type":"layers","diff_ids":["{}"]}}}}"#,
            arch.oci_name(),
            sha256(b"x")
        );
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},"layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"{}","size":{}}}]}}"#,
            sha256(config.as_bytes()),
            config.len(),
            sha256(&layer),
            layer.len()
        );
        let mut docs = std::collections::HashMap::new();
        docs.insert(
            "/v2/real/tool/manifests/1".to_owned(),
            (
                "application/vnd.oci.image.manifest.v1+json".to_owned(),
                manifest.clone().into_bytes(),
            ),
        );
        docs.insert(
            format!("/v2/real/tool/blobs/{}", sha256(config.as_bytes())),
            (
                "application/octet-stream".to_owned(),
                config.clone().into_bytes(),
            ),
        );
        docs.insert(
            format!("/v2/real/tool/blobs/{}", sha256(&layer)),
            ("application/octet-stream".to_owned(), layer.clone()),
        );
        let (primary, primary_seen) = fake_registry(docs);
        let (mirror, mirror_seen) = fake_registry(Default::default());
        let dir = std::env::temp_dir().join(format!("sbx-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("registries.conf"),
            format!(
                "[[registry]]\nprefix = \"example.invalid/team\"\nlocation = \"127.0.0.1:{primary}/real\"\n\
                 [[registry.mirror]]\nlocation = \"127.0.0.1:{mirror}/m\"\n\
                 [[registry]]\nlocation = \"blocked.invalid\"\nblocked = true\n"
            ),
        )
        .unwrap();
        let src = OciImageSource::new()
            .unwrap()
            .with_registries(registries::ConfFiles {
                main: Some(dir.join("registries.conf")),
                dropin_dirs: vec![],
            });

        let img = src
            .resolve("example.invalid/team/tool:1", Platform { arch })
            .await
            .unwrap();
        assert_eq!(img.reference, "example.invalid/team/tool:1");
        assert_eq!(img.source, format!("127.0.0.1:{primary}/real/tool"));
        assert_eq!(img.id, ImageId::new(sha256(manifest.as_bytes())));
        assert_eq!(
            mirror_seen.lock().unwrap().first().map(String::as_str),
            Some("/v2/m/tool/manifests/1")
        );
        let mut buf = Vec::new();
        src.fetch_blob(&img.source, &img.layers[0].digest, &mut buf)
            .await
            .unwrap();
        assert_eq!(buf, layer);
        assert!(primary_seen.lock().unwrap().len() >= 3);

        // 어디에도 없으면 주 위치의 오류에 미러의 것을 붙인다.
        let e = src
            .resolve("example.invalid/team/none:1", Platform { arch })
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("/real/none") && e.contains("mirror"), "{e}");
        let e = src
            .resolve("blocked.invalid/a:1", Platform { arch })
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("blocked"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
