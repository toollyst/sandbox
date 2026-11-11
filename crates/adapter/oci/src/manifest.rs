//! manifest, index, image config 해석. 네트워크 없이 바이트만 다룬다.
//!
//! 레지스트리가 보낸 문서는 믿지 않는다. 형식이 [`scope`]에 없으면 거부하고,
//! digest는 호출하는 쪽이 직접 계산해 대조한다.

use serde::Deserialize;
use serde_json::{Map, Value};

use sbx_image::ImageError;
use sbx_model::{Arch, Compression, Digest, ImageConfig};

use crate::scope;

const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";

#[derive(Deserialize)]
struct RawPlatform {
    architecture: String,
    os: String,
    #[serde(default)]
    variant: Option<String>,
}

#[derive(Deserialize)]
struct RawDescriptor {
    #[serde(rename = "mediaType", default)]
    media_type: String,
    digest: String,
    size: u64,
    #[serde(default)]
    platform: Option<RawPlatform>,
}

#[derive(Deserialize)]
struct RawDocument {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    #[serde(rename = "mediaType", default)]
    media_type: Option<String>,
    #[serde(default)]
    manifests: Option<Vec<RawDescriptor>>,
    #[serde(default)]
    config: Option<RawDescriptor>,
    #[serde(default)]
    layers: Option<Vec<RawDescriptor>>,
}

/// index 안의 manifest 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// manifest digest.
    pub digest: Digest,
    /// manifest 형식.
    pub media_type: String,
    /// `platform.os`.
    pub os: Option<String>,
    /// `platform.architecture`.
    pub arch: Option<String>,
    /// `platform.variant`.
    pub variant: Option<String>,
}

/// 레이어 하나의 기술.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRef {
    /// 압축된 blob digest.
    pub digest: Digest,
    /// 압축된 크기.
    pub size: u64,
    /// 압축 형식.
    pub compression: Compression,
}

/// 플랫폼 하나의 manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// config blob digest.
    pub config: Digest,
    /// config blob 크기.
    pub config_size: u64,
    /// 아래부터 순서대로.
    pub layers: Vec<LayerRef>,
    /// 미디어 형식(OCI 또는 Docker v2 manifest).
    pub media_type: String,
}

/// 해석한 문서.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Document {
    /// 여러 플랫폼의 목록.
    Index(Vec<IndexEntry>),
    /// 플랫폼 하나.
    Manifest(Manifest),
}

fn digest(s: &str) -> Result<Digest, ImageError> {
    Digest::parse(s).map_err(|_| ImageError::Invalid(format!("bad digest {s:?}")))
}

/// 문서를 해석한다. 형식은 `Content-Type` 헤더, 없으면 문서의 `mediaType`으로 정한다.
pub fn parse_document(content_type: Option<&str>, bytes: &[u8]) -> Result<Document, ImageError> {
    let raw: RawDocument = serde_json::from_slice(bytes)
        .map_err(|e| ImageError::Invalid(format!("manifest json: {e}")))?;
    if raw.schema_version != 2 {
        return Err(ImageError::Unsupported(format!(
            "manifest schemaVersion {}",
            raw.schema_version
        )));
    }
    let header = content_type
        .map(|c| c.split(';').next().unwrap_or("").trim().to_owned())
        .filter(|c| !c.is_empty() && c != "application/json");
    let media_type = header.or(raw.media_type).unwrap_or_else(|| {
        if raw.manifests.is_some() {
            OCI_INDEX.to_owned()
        } else {
            "application/vnd.oci.image.manifest.v1+json".to_owned()
        }
    });
    if scope::REJECTED_MANIFEST_MEDIA_TYPES.contains(&media_type.as_str())
        || !scope::MANIFEST_MEDIA_TYPES.contains(&media_type.as_str())
    {
        return Err(ImageError::Unsupported(format!(
            "manifest media type {media_type}"
        )));
    }

    if media_type == OCI_INDEX || media_type == DOCKER_LIST {
        let entries = raw
            .manifests
            .ok_or_else(|| ImageError::Invalid("index without manifests".into()))?
            .into_iter()
            .map(|d| {
                Ok(IndexEntry {
                    digest: digest(&d.digest)?,
                    media_type: d.media_type,
                    os: d.platform.as_ref().map(|p| p.os.clone()),
                    arch: d.platform.as_ref().map(|p| p.architecture.clone()),
                    variant: d.platform.and_then(|p| p.variant),
                })
            })
            .collect::<Result<_, ImageError>>()?;
        return Ok(Document::Index(entries));
    }

    let config = raw
        .config
        .ok_or_else(|| ImageError::Invalid("manifest without config".into()))?;
    let layers = raw
        .layers
        .ok_or_else(|| ImageError::Invalid("manifest without layers".into()))?
        .into_iter()
        .map(|l| {
            let compression = scope::layer_compression(&l.media_type).ok_or_else(|| {
                ImageError::Unsupported(format!("layer media type {}", l.media_type))
            })?;
            Ok(LayerRef {
                digest: digest(&l.digest)?,
                size: l.size,
                compression,
            })
        })
        .collect::<Result<_, ImageError>>()?;
    Ok(Document::Manifest(Manifest {
        config: digest(&config.digest)?,
        config_size: config.size,
        layers,
        media_type,
    }))
}

/// index에서 linux/`arch` manifest를 고른다.
///
/// variant가 없거나 arm64의 `v8`인 것을 먼저 고르고, 같으면 index 순서를 따른다.
/// 플랫폼이 없는 항목(서명, attestation 등)은 고르지 않는다.
pub fn select(entries: &[IndexEntry], arch: Arch) -> Option<&IndexEntry> {
    entries
        .iter()
        .filter(|e| {
            e.os.as_deref() == Some("linux")
                && e.arch.as_deref() == Some(arch.oci_name())
                && scope::MANIFEST_MEDIA_TYPES.contains(&e.media_type.as_str())
                && e.media_type != OCI_INDEX
                && e.media_type != DOCKER_LIST
        })
        .min_by_key(|e| match e.variant.as_deref() {
            None | Some("v8") => 0,
            Some(_) => 1,
        })
}

/// 해석한 image config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedConfig {
    /// `os`.
    pub os: String,
    /// `architecture`.
    pub arch: String,
    /// 실행 기본값.
    pub config: ImageConfig,
    /// `rootfs.diff_ids`.
    pub diff_ids: Vec<Digest>,
    /// 값이 있었지만 적용하지 않은 `config.*` 필드.
    pub ignored_fields: Vec<String>,
}

#[derive(Deserialize)]
struct RawRootfs {
    #[serde(rename = "type")]
    kind: String,
    diff_ids: Vec<String>,
}

#[derive(Deserialize)]
struct RawConfigFile {
    architecture: String,
    os: String,
    #[serde(default)]
    config: Option<Map<String, Value>>,
    rootfs: RawRootfs,
}

fn strings(key: &str, v: &Value) -> Result<Vec<String>, ImageError> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| ImageError::Invalid(format!("config.{key} must be strings")))
            })
            .collect(),
        _ => Err(ImageError::Invalid(format!(
            "config.{key} must be an array"
        ))),
    }
}

fn string(key: &str, v: &Value) -> Result<Option<String>, ImageError> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) if s.is_empty() => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(ImageError::Invalid(format!(
            "config.{key} must be a string"
        ))),
    }
}

fn labels(v: &Value) -> Result<std::collections::BTreeMap<String, String>, ImageError> {
    match v {
        Value::Null => Ok(Default::default()),
        Value::Object(m) => m
            .iter()
            .map(|(k, v)| match v {
                Value::String(s) => Ok((k.clone(), s.clone())),
                _ => Err(ImageError::Invalid(
                    "config.Labels values must be strings".into(),
                )),
            })
            .collect(),
        _ => Err(ImageError::Invalid(
            "config.Labels must be an object".into(),
        )),
    }
}

/// `ExposedPorts`, `Volumes`: 키만 쓰는 객체(값은 `{}`).
fn keys(key: &str, v: &Value) -> Result<Vec<String>, ImageError> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Object(m) => Ok(m.keys().cloned().collect()),
        _ => Err(ImageError::Invalid(format!(
            "config.{key} must be an object"
        ))),
    }
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// image config를 해석한다. 적용하는 필드는 [`scope::APPLIED_CONFIG_FIELDS`]다.
pub fn parse_config(bytes: &[u8]) -> Result<ParsedConfig, ImageError> {
    let raw: RawConfigFile = serde_json::from_slice(bytes)
        .map_err(|e| ImageError::Invalid(format!("image config json: {e}")))?;
    if raw.rootfs.kind != "layers" {
        return Err(ImageError::Unsupported(format!(
            "rootfs type {}",
            raw.rootfs.kind
        )));
    }
    let mut config = ImageConfig::default();
    let mut ignored = Vec::new();
    for (key, value) in raw.config.unwrap_or_default() {
        match key.as_str() {
            "Entrypoint" => config.entrypoint = strings(&key, &value)?,
            "Cmd" => config.cmd = strings(&key, &value)?,
            "Env" => {
                config.env = strings(&key, &value)?
                    .into_iter()
                    .map(|kv| match kv.split_once('=') {
                        Some((k, v)) => (k.to_owned(), v.to_owned()),
                        None => (kv, String::new()),
                    })
                    .collect();
            }
            "WorkingDir" => config.working_dir = string(&key, &value)?,
            "User" => config.user = string(&key, &value)?,
            "StopSignal" => config.stop_signal = string(&key, &value)?,
            "Labels" => config.labels = labels(&value)?,
            "ExposedPorts" => config.exposed_ports = keys(&key, &value)?,
            "Volumes" => config.volumes = keys(&key, &value)?,
            _ if is_empty(&value) => {}
            _ => ignored.push(format!("config.{key}")),
        }
    }
    let diff_ids = raw
        .rootfs
        .diff_ids
        .iter()
        .map(|d| digest(d))
        .collect::<Result<_, _>>()?;
    Ok(ParsedConfig {
        os: raw.os,
        arch: raw.architecture,
        config,
        diff_ids,
        ignored_fields: ignored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(c: char) -> String {
        format!("sha256:{}", c.to_string().repeat(64))
    }

    fn index_json() -> String {
        format!(
            r#"{{"schemaVersion":2,"mediaType":"{OCI_INDEX}","manifests":[
              {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"amd64","os":"linux"}}}},
              {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"arm","os":"linux","variant":"v7"}}}},
              {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"arm64","os":"linux","variant":"v8"}}}},
              {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"unknown","os":"unknown"}}}}
            ]}}"#,
            d('1'),
            d('2'),
            d('3'),
            d('4')
        )
    }

    #[test]
    fn index_and_platform_selection() {
        let Document::Index(entries) = parse_document(None, index_json().as_bytes()).unwrap()
        else {
            panic!("expected index");
        };
        assert_eq!(entries.len(), 4);
        assert_eq!(
            select(&entries, Arch::X86_64).unwrap().digest.as_str(),
            d('1')
        );
        assert_eq!(
            select(&entries, Arch::Aarch64).unwrap().digest.as_str(),
            d('3')
        );
    }

    #[test]
    fn platform_missing() {
        let Document::Index(entries) = parse_document(None, index_json().as_bytes()).unwrap()
        else {
            panic!("expected index");
        };
        let only_arm: Vec<_> = entries
            .into_iter()
            .filter(|e| e.arch.as_deref() == Some("arm"))
            .collect();
        assert!(select(&only_arm, Arch::Aarch64).is_none());
    }

    #[test]
    fn docker_v2_manifest() {
        let json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json",
              "config":{{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"{}","size":10}},
              "layers":[{{"mediaType":"application/vnd.docker.image.rootfs.diff.tar.gzip","digest":"{}","size":20}}]}}"#,
            d('a'),
            d('b')
        );
        let Document::Manifest(m) = parse_document(None, json.as_bytes()).unwrap() else {
            panic!("expected manifest");
        };
        assert_eq!(m.config.as_str(), d('a'));
        assert_eq!(m.layers[0].compression, Compression::Gzip);
        assert_eq!(m.layers[0].size, 20);
    }

    #[test]
    fn content_type_header_wins_and_params_are_ignored() {
        let json = format!(
            r#"{{"schemaVersion":2,"config":{{"digest":"{}","size":1}},"layers":[]}}"#,
            d('a')
        );
        let doc = parse_document(
            Some("application/vnd.oci.image.manifest.v1+json; charset=utf-8"),
            json.as_bytes(),
        )
        .unwrap();
        assert!(matches!(doc, Document::Manifest(_)));
    }

    #[test]
    fn unsupported_documents() {
        let schema1 = br#"{"schemaVersion":1,"name":"x"}"#;
        assert!(matches!(
            parse_document(None, schema1),
            Err(ImageError::Unsupported(_))
        ));
        let lz4 = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",
              "config":{{"digest":"{}","size":1}},
              "layers":[{{"mediaType":"application/vnd.oci.image.layer.v1.tar+lz4","digest":"{}","size":1}}]}}"#,
            d('a'),
            d('b')
        );
        assert!(matches!(
            parse_document(None, lz4.as_bytes()),
            Err(ImageError::Unsupported(m)) if m.contains("lz4")
        ));
        assert!(matches!(
            parse_document(None, b"not json"),
            Err(ImageError::Invalid(_))
        ));
    }

    #[test]
    fn config_fields() {
        let json = format!(
            r#"{{"architecture":"arm64","os":"linux",
              "config":{{"Env":["PATH=/usr/bin","EMPTY=","BARE"],"Cmd":["/bin/sh"],"Entrypoint":null,
                        "WorkingDir":"","User":"app","Labels":{{"a":"b"}},"StopSignal":"SIGTERM","Volumes":{{"/data":{{}}}},
                        "ExposedPorts":{{"80/tcp":{{}},"53/udp":{{}}}},"Healthcheck":{{"Test":["CMD","true"]}}}},
              "rootfs":{{"type":"layers","diff_ids":["{}"]}},
              "history":[{{"created_by":"x"}}]}}"#,
            d('c')
        );
        let c = parse_config(json.as_bytes()).unwrap();
        assert_eq!((c.os.as_str(), c.arch.as_str()), ("linux", "arm64"));
        assert_eq!(c.config.cmd, vec!["/bin/sh"]);
        assert!(c.config.entrypoint.is_empty());
        assert_eq!(c.config.working_dir, None);
        assert_eq!(c.config.user.as_deref(), Some("app"));
        assert_eq!(
            c.config.env,
            vec![
                ("PATH".into(), "/usr/bin".into()),
                ("EMPTY".into(), "".into()),
                ("BARE".into(), "".into())
            ]
        );
        assert_eq!(c.ignored_fields, ["config.Healthcheck"]);
        assert_eq!(c.config.volumes, ["/data"]);
        assert_eq!(c.config.exposed_ports, ["53/udp", "80/tcp"]);
        assert_eq!(c.config.labels.get("a").map(String::as_str), Some("b"));
        assert_eq!(c.config.stop_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(c.diff_ids[0].as_str(), d('c'));
    }

    #[test]
    fn config_type_errors() {
        let json = format!(
            r#"{{"architecture":"amd64","os":"linux","config":{{"Cmd":"sh"}},"rootfs":{{"type":"layers","diff_ids":["{}"]}}}}"#,
            d('c')
        );
        assert!(matches!(
            parse_config(json.as_bytes()),
            Err(ImageError::Invalid(_))
        ));
    }
}
