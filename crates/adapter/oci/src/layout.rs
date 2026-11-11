//! OCI image layout 읽기 (`sbx load`). 레지스트리 없이 디렉터리나 tar에서 이미지를 들인다
//! ([Image Layout spec](https://github.com/opencontainers/image-spec/blob/main/image-layout.md)).
//!
//! - 받는 것: `oci-layout`(`imageLayoutVersion` 1.0.0), `index.json`, `blobs/sha256/<hex>`. Docker 25 이후의
//!   `docker save`, `podman save --format oci-archive`, `skopeo copy ... oci-archive:`가 이 모양이다.
//! - tar는 [`unpack`]이 임시 디렉터리에 푼다(gzip이면 풀어서). 위 세 가지의 일반 파일만 꺼내고 나머지(Docker의
//!   `manifest.json`·`repositories`, 링크, 장치)는 건너뛴다. 경로는 정해진 이름과만 맞추므로 밖으로 나갈 수 없다.
//! - 레지스트리와 똑같이 믿지 않는다: 모든 문서와 blob의 digest를 대조하고(blob은 받는 쪽 `sbx-manage`가),
//!   문서는 [`MAX_DOCUMENT_BYTES`]까지 읽는다.
//! - `index.json`에 이미지가 여럿이면 이름으로 고른다: 항목의 `io.containerd.image.name`, 또는 완전한 참조인
//!   `org.opencontainers.image.ref.name`([`OciLayoutSource::names`]).

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use sbx_image::{ImageError, ImageSource, Platform, ResolvedImage};
use sbx_model::{BoxFuture, Digest};

use crate::manifest::{self, Document};
use crate::registry::MAX_DOCUMENT_BYTES;
use crate::{Reference, assemble, verify};

/// `docker save`가 붙이는 전체 이름.
const CONTAINERD_NAME: &str = "io.containerd.image.name";
/// OCI의 이름(보통 태그만, podman은 전체 참조).
const REF_NAME: &str = "org.opencontainers.image.ref.name";

#[derive(Deserialize)]
struct LayoutFile {
    #[serde(rename = "imageLayoutVersion")]
    version: String,
}

#[derive(Deserialize)]
struct Index {
    #[serde(default)]
    manifests: Vec<Entry>,
}

#[derive(Deserialize, Clone)]
struct Entry {
    #[serde(rename = "mediaType", default)]
    media_type: Option<String>,
    digest: String,
    #[serde(default)]
    annotations: std::collections::BTreeMap<String, String>,
}

impl Entry {
    /// 정규화한 이름.
    fn name(&self) -> Option<String> {
        [CONTAINERD_NAME, REF_NAME]
            .iter()
            .filter_map(|k| self.annotations.get(*k))
            // 태그만 있는 ref.name("3.20")은 저장소를 모른다.
            .filter(|n| n.contains('/') || n.contains(':') && !n.starts_with(':'))
            .find_map(|n| Reference::parse(n).ok())
            .map(|r| r.to_string())
    }
}

/// 디렉터리 하나의 OCI image layout.
#[derive(Debug, Clone)]
pub struct OciLayoutSource {
    dir: PathBuf,
}

fn read_limited(path: &Path) -> Result<Vec<u8>, ImageError> {
    let f =
        File::open(path).map_err(|e| ImageError::Invalid(format!("{}: {e}", path.display())))?;
    let mut b = Vec::new();
    f.take(MAX_DOCUMENT_BYTES as u64 + 1).read_to_end(&mut b)?;
    if b.len() > MAX_DOCUMENT_BYTES {
        return Err(ImageError::Invalid(format!(
            "{}: larger than {MAX_DOCUMENT_BYTES} bytes",
            path.display()
        )));
    }
    Ok(b)
}

impl OciLayoutSource {
    /// 연다. `oci-layout`이 없거나 버전이 1.x가 아니면 오류다.
    pub fn open(dir: &Path) -> Result<Self, ImageError> {
        let raw = read_limited(&dir.join("oci-layout")).map_err(|_| {
            ImageError::Unsupported(format!(
                "{} is not an OCI image layout (no oci-layout file); export it with `docker save` (Docker 25 or later), \
                 `podman save --format oci-archive` or `skopeo copy ... oci-archive:`",
                dir.display()
            ))
        })?;
        let l: LayoutFile = serde_json::from_slice(&raw)
            .map_err(|e| ImageError::Invalid(format!("oci-layout: {e}")))?;
        if !l.version.starts_with("1.") {
            return Err(ImageError::Unsupported(format!(
                "image layout version {}",
                l.version
            )));
        }
        Ok(Self {
            dir: dir.to_owned(),
        })
    }

    fn index(&self) -> Result<Vec<Entry>, ImageError> {
        let raw = read_limited(&self.dir.join("index.json"))?;
        let i: Index = serde_json::from_slice(&raw)
            .map_err(|e| ImageError::Invalid(format!("index.json: {e}")))?;
        Ok(i.manifests)
    }

    /// 이름이 붙은 이미지들(정규화한 참조), `index.json` 순서.
    pub fn names(&self) -> Result<Vec<String>, ImageError> {
        Ok(self.index()?.iter().filter_map(Entry::name).collect())
    }

    fn blob_path(&self, d: &Digest) -> PathBuf {
        // Digest는 `sha256:` + 소문자 hex 64자만 받는다: 경로가 된다.
        self.dir.join("blobs").join("sha256").join(d.hex())
    }

    fn document(&self, d: &Digest, what: &str) -> Result<Vec<u8>, ImageError> {
        let b = read_limited(&self.blob_path(d))?;
        verify(what, &b, d)?;
        Ok(b)
    }

    fn resolve_inner(
        &self,
        reference: &str,
        platform: Platform,
    ) -> Result<ResolvedImage, ImageError> {
        let r = Reference::parse(reference).map_err(ImageError::Reference)?;
        let want = r.to_string();
        let entries = self.index()?;
        let entry = match entries.as_slice() {
            [] => return Err(ImageError::Invalid("index.json lists no images".into())),
            [only] => only.clone(),
            many => many
                .iter()
                .find(|e| e.name().as_deref() == Some(want.as_str()))
                .cloned()
                .ok_or_else(|| {
                    let names: Vec<String> = many.iter().filter_map(Entry::name).collect();
                    ImageError::NotFound(format!(
                        "{want} in this archive (it has {})",
                        if names.is_empty() {
                            "unnamed images only".into()
                        } else {
                            names.join(", ")
                        }
                    ))
                })?,
        };
        let digest = Digest::parse(&entry.digest)
            .map_err(|e| ImageError::Invalid(format!("index.json digest: {e}")))?;
        let bytes = self.document(&digest, "manifest")?;
        let (manifest_bytes, manifest) =
            match manifest::parse_document(entry.media_type.as_deref(), &bytes)? {
                Document::Manifest(m) => (bytes, m),
                Document::Index(list) => {
                    let e = manifest::select(&list, platform.arch).ok_or_else(|| {
                        ImageError::PlatformNotFound {
                            reference: want.clone(),
                            arch: platform.arch,
                        }
                    })?;
                    let b = self.document(&e.digest, "platform manifest")?;
                    match manifest::parse_document(Some(&e.media_type), &b)? {
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
        let config = read_limited(&self.blob_path(&manifest.config))?;
        assemble(
            &r,
            format!("oci-layout:{}", self.dir.display()),
            platform,
            manifest_bytes,
            manifest,
            config,
        )
    }
}

impl ImageSource for OciLayoutSource {
    fn resolve<'a>(
        &'a self,
        reference: &'a str,
        platform: Platform,
    ) -> BoxFuture<'a, Result<ResolvedImage, ImageError>> {
        Box::pin(async move { self.resolve_inner(reference, platform) })
    }

    fn fetch_blob<'a>(
        &'a self,
        _source: &'a str,
        digest: &'a Digest,
        sink: &'a mut (dyn Write + Send),
    ) -> BoxFuture<'a, Result<u64, ImageError>> {
        Box::pin(async move {
            let mut f = File::open(self.blob_path(digest)).map_err(|e| {
                ImageError::Invalid(format!("the archive has no blob {digest}: {e}"))
            })?;
            Ok(io::copy(&mut f, sink)?)
        })
    }
}

/// tar 안의 경로가 꺼낼 것이면 그 상대 경로.
fn wanted(path: &str) -> Option<String> {
    let p = path.trim_start_matches("./");
    match p {
        "oci-layout" | "index.json" => Some(p.to_owned()),
        _ => {
            let hex = p.strip_prefix("blobs/sha256/")?;
            (hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
            .then(|| p.to_owned())
        }
    }
}

/// OCI layout tar(gzip이어도 된다)를 `into`(비어 있어야 한다)에 푼다. 규칙은 모듈 문서에 있다.
pub fn unpack(archive: &mut dyn Read, into: &Path) -> Result<(), ImageError> {
    let mut head = [0u8; 2];
    let mut first = Vec::new();
    let n = archive.read(&mut head)?;
    first.extend_from_slice(&head[..n]);
    let chained = io::Cursor::new(first).chain(archive);
    let reader: Box<dyn Read> = if head[..n] == [0x1f, 0x8b] {
        Box::new(flate2::read::GzDecoder::new(chained))
    } else {
        Box::new(chained)
    };
    std::fs::create_dir_all(into.join("blobs/sha256"))?;
    let mut tar = tar::Archive::new(reader);
    for e in tar.entries()? {
        let mut e = e?;
        if e.header().entry_type() != tar::EntryType::Regular {
            continue;
        }
        let path = String::from_utf8_lossy(&e.path_bytes()).into_owned();
        let Some(rel) = wanted(&path) else { continue };
        let target = into.join(&rel);
        // 같은 이름이 두 번 오면 처음 것만(뒤의 것으로 바꿔치지 않는다).
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        };
        io::copy(&mut e, &mut f)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_layout_files_are_unpacked() {
        assert_eq!(wanted("./index.json").as_deref(), Some("index.json"));
        assert_eq!(wanted("oci-layout").as_deref(), Some("oci-layout"));
        let hex = "a".repeat(64);
        assert_eq!(
            wanted(&format!("blobs/sha256/{hex}")),
            Some(format!("blobs/sha256/{hex}"))
        );
        for bad in [
            "manifest.json",
            "../index.json",
            "blobs/sha256/../../x",
            "blobs/sha512/aa",
            &format!("blobs/sha256/{}", "A".repeat(64)),
            &format!("blobs/sha256/{hex}/x"),
            "/index.json",
        ] {
            assert_eq!(wanted(bad), None, "{bad}");
        }
    }

    #[test]
    fn names_come_from_full_references_only() {
        let e = |k: &str, v: &str| Entry {
            media_type: None,
            digest: String::new(),
            annotations: [(k.to_owned(), v.to_owned())].into(),
        };
        assert_eq!(
            e(CONTAINERD_NAME, "alpine:3.20").name().as_deref(),
            Some("docker.io/library/alpine:3.20")
        );
        assert_eq!(
            e(REF_NAME, "ghcr.io/a/b:1").name().as_deref(),
            Some("ghcr.io/a/b:1")
        );
        assert_eq!(e(REF_NAME, "3.20").name(), None);
    }
}
