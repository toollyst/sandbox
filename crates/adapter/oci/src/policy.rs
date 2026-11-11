//! 이미지 서명 정책: podman, skopeo, buildah가 함께 쓰는 containers
//! [`policy.json`](https://github.com/containers/image/blob/main/docs/containers-policy.json.5.md)을 그대로 따른다.
//! sbx만의 정책 파일은 없다. 파일이 없으면(정하는 것은 호출자) Docker처럼 모두 받는다.
//!
//! | 무엇 | 규칙(containers/image와 같다) |
//! |---|---|
//! | 고르기 | `transports.docker`에서 가장 구체적인 scope 하나: `저장소:태그`(또는 `저장소@digest`), 저장소, 그 위 이름공간들, 호스트(포트 포함), `*.도메인`(포트 없는 호스트만), `""`, 마지막에 `default`. 이미지 이름은 미러가 아니라 요청한 것 |
//! | 목록 | 고른 목록의 요구를 **모두** 만족해야 한다. 빈 목록은 정책 파일 오류다 |
//! | `insecureAcceptAnything` | 받는다 |
//! | `reject` | 받지 않는다 |
//! | `sigstoreSigned` + `keyPath`·`keyPaths`·`keyData`·`keyDatas` | 아래 서명 중 하나가 그 키들 중 하나로 검증되고, 서명한 digest가 이미지(최상위 manifest)의 digest이고, 서명한 이름이 `signedIdentity`에 맞아야 한다 |
//! | `signedIdentity` | `matchRepoDigestOrExact`(기본), `matchExact`, `matchRepository`, `exactReference`, `exactRepository`, `remapIdentity` |
//! | 할 수 없는 요구 | `signedBy`(GPG), `sigstoreSigned`의 `fulcio`·`pki`(keyless)·`rekorPublicKey*`. 파일은 읽히고, 그 요구가 걸린 이미지만 **거절**한다(받아들이는 쪽으로 넘기지 않는다) |
//!
//! 서명은 cosign이 두는 자리에서 찾는다: 이미지를 받은 저장소의 태그 `sha256-<hex>.sig`, 레이어
//! `application/vnd.dev.cosign.simplesigning.v1+json`, 서명은 주석 `dev.cosignproject.cosign/signature`(base64).
//! containers는 `registries.d`의 `use-sigstore-attachments`가 켜져 있어야 찾지만, sbx는 정책이 서명을 요구할
//! 때만, 그리고 항상 찾는다. 키는 ECDSA(P-256, P-384, SHA-256), RSA(PKCS#1 v1.5, SHA-256), Ed25519이고 PEM
//! (`PUBLIC KEY`)이다. cosign의 서명 이름에는 태그가 없으므로 태그로 받는 이미지에는 보통
//! `"signedIdentity": {"type": "matchRepository"}`가 필요하다(containers와 같다).
//!
//! `sbx load`는 `oci-archive`(tar)나 `oci`(디렉터리) 전송의 정책을 본다([`Policy::check_archive`]). archive의
//! 서명은 확인할 수 없으므로 서명을 요구하면 거절한다.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use aws_lc_rs::signature::{self, ParsedPublicKey, VerificationAlgorithm};
use serde_json::Value;

use sbx_image::ImageError;
use sbx_model::Digest;

use crate::reference::Reference;

/// cosign 서명 레이어의 미디어 타입.
pub const SIGNATURE_MEDIA_TYPE: &str = "application/vnd.dev.cosign.simplesigning.v1+json";
/// 서명(base64)을 담은 주석.
pub const SIGNATURE_ANNOTATION: &str = "dev.cosignproject.cosign/signature";
const PAYLOAD_TYPE: &str = "cosign container image signature";
/// 서명 payload의 최대 크기.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// 한 이미지에서 보는 서명의 최대 개수.
pub const MAX_SIGNATURES: usize = 64;

fn bad(path: &Path, why: impl std::fmt::Display) -> ImageError {
    ImageError::Invalid(format!("{}: {why}", path.display()))
}

/// 이름(태그·digest는 있을 수도 없을 수도). containers의 `ParseNormalizedNamed`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Named {
    /// `레지스트리/저장소`.
    repo: String,
    tag: Option<String>,
    digest: Option<Digest>,
}

impl Named {
    fn parse(s: &str) -> Option<Self> {
        let (name_tag, has_digest) = match s.split_once('@') {
            Some((n, _)) => (n, true),
            None => (s, false),
        };
        let has_tag = name_tag[name_tag.rfind('/').map_or(0, |i| i + 1)..].contains(':');
        let r = Reference::parse(s).ok()?;
        Some(Self {
            repo: format!("{}/{}", r.registry, r.repository),
            tag: if has_tag { r.tag } else { None },
            digest: if has_digest { r.digest } else { None },
        })
    }

    fn of(r: &Reference) -> Self {
        Self {
            repo: format!("{}/{}", r.registry, r.repository),
            tag: r.tag.clone(),
            digest: r.digest.clone(),
        }
    }

    fn name_only(&self) -> bool {
        self.tag.is_none() && self.digest.is_none()
    }

    fn to_string_full(&self) -> String {
        let mut s = self.repo.clone();
        if let Some(t) = &self.tag {
            s.push(':');
            s.push_str(t);
        }
        if let Some(d) = &self.digest {
            s.push('@');
            s.push_str(d.as_str());
        }
        s
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Identity {
    MatchExact,
    MatchRepoDigestOrExact,
    MatchRepository,
    ExactReference(Named),
    ExactRepository(String),
    Remap {
        prefix: String,
        signed_prefix: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeySource {
    Path(PathBuf),
    /// base64로 적힌 PEM.
    Data(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Requirement {
    Accept,
    Reject,
    Sigstore {
        keys: Vec<KeySource>,
        identity: Identity,
    },
    /// sbx가 확인할 수 없는 요구. 걸리면 거절한다. 값은 무엇인지.
    Unsupported(String),
}

/// 읽은 정책. 비어 있으면(파일 없음) 모두 받는다.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    /// 파일. 없으면 `None`이고 모두 받는다.
    file: Option<PathBuf>,
    default: Vec<Requirement>,
    transports: BTreeMap<String, BTreeMap<String, Vec<Requirement>>>,
}

/// 서명 하나(cosign 레이어 하나).
#[derive(Debug, Clone)]
pub struct Signature {
    /// payload 원본(서명한 바이트).
    pub payload: Vec<u8>,
    /// 서명(base64 주석을 푼 것).
    pub signature: Vec<u8>,
}

fn object<'a>(
    path: &Path,
    v: &'a Value,
    what: &str,
    allowed: &[&str],
) -> Result<&'a serde_json::Map<String, Value>, ImageError> {
    let m = v
        .as_object()
        .ok_or_else(|| bad(path, format!("{what} must be an object")))?;
    if let Some(k) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(bad(path, format!("{what}: unknown field {k:?}")));
    }
    Ok(m)
}

fn string<'a>(
    path: &Path,
    m: &'a serde_json::Map<String, Value>,
    k: &str,
) -> Result<Option<&'a str>, ImageError> {
    match m.get(k) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(path, format!("{k} must be a string"))),
    }
}

fn strings(
    path: &Path,
    m: &serde_json::Map<String, Value>,
    k: &str,
) -> Result<Option<Vec<String>>, ImageError> {
    match m.get(k) {
        None => Ok(None),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| bad(path, format!("{k} must be a list of strings")))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(bad(path, format!("{k} must be a list of strings"))),
    }
}

/// `host[:port]`, `host/ns`, `host/ns/repo` 모양인가(태그·digest 없음).
fn valid_prefix(s: &str) -> bool {
    !s.is_empty()
        && !s.contains('@')
        && !s.ends_with('/')
        && (!s.contains('/') || Named::parse(s).is_some_and(|n| n.name_only() && n.repo == s))
}

fn identity(path: &Path, v: Option<&Value>) -> Result<Identity, ImageError> {
    let Some(v) = v else {
        return Ok(Identity::MatchRepoDigestOrExact);
    };
    let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
    Ok(match ty {
        "matchExact" => {
            object(path, v, "signedIdentity", &["type"])?;
            Identity::MatchExact
        }
        "matchRepoDigestOrExact" => {
            object(path, v, "signedIdentity", &["type"])?;
            Identity::MatchRepoDigestOrExact
        }
        "matchRepository" => {
            object(path, v, "signedIdentity", &["type"])?;
            Identity::MatchRepository
        }
        "exactReference" => {
            let m = object(path, v, "signedIdentity", &["type", "dockerReference"])?;
            let r = string(path, m, "dockerReference")?.unwrap_or_default();
            let n = Named::parse(r).filter(|n| !n.name_only()).ok_or_else(|| {
                bad(
                    path,
                    format!("exactReference {r:?} needs a tag or a digest"),
                )
            })?;
            Identity::ExactReference(n)
        }
        "exactRepository" => {
            let m = object(path, v, "signedIdentity", &["type", "dockerRepository"])?;
            let r = string(path, m, "dockerRepository")?.unwrap_or_default();
            let n = Named::parse(r).filter(Named::name_only).ok_or_else(|| {
                bad(
                    path,
                    format!("exactRepository {r:?} must be a repository without a tag"),
                )
            })?;
            Identity::ExactRepository(n.repo)
        }
        "remapIdentity" => {
            let m = object(
                path,
                v,
                "signedIdentity",
                &["type", "prefix", "signedPrefix"],
            )?;
            let prefix = string(path, m, "prefix")?.unwrap_or_default().to_owned();
            let signed_prefix = string(path, m, "signedPrefix")?
                .unwrap_or_default()
                .to_owned();
            if !valid_prefix(&prefix) || !valid_prefix(&signed_prefix) {
                return Err(bad(
                    path,
                    "remapIdentity needs a prefix and signedPrefix like host, host/ns or host/ns/repo",
                ));
            }
            Identity::Remap {
                prefix,
                signed_prefix,
            }
        }
        other => return Err(bad(path, format!("unknown signedIdentity type {other:?}"))),
    })
}

fn requirement(path: &Path, v: &Value) -> Result<Requirement, ImageError> {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
    Ok(match ty {
        "insecureAcceptAnything" => {
            object(path, v, ty, &["type"])?;
            Requirement::Accept
        }
        "reject" => {
            object(path, v, ty, &["type"])?;
            Requirement::Reject
        }
        "signedBy" => {
            object(
                path,
                v,
                ty,
                &[
                    "type",
                    "keyType",
                    "keyPath",
                    "keyPaths",
                    "keyData",
                    "signedIdentity",
                ],
            )?;
            Requirement::Unsupported("GPG signatures (signedBy)".into())
        }
        "sigstoreSigned" => {
            let m = object(
                path,
                v,
                ty,
                &[
                    "type",
                    "keyPath",
                    "keyPaths",
                    "keyData",
                    "keyDatas",
                    "fulcio",
                    "pki",
                    "rekorPublicKeyPath",
                    "rekorPublicKeyPaths",
                    "rekorPublicKeyData",
                    "rekorPublicKeyDatas",
                    "signedIdentity",
                ],
            )?;
            let sources = [
                "keyPath", "keyPaths", "keyData", "keyDatas", "fulcio", "pki",
            ]
            .iter()
            .filter(|k| m.contains_key(**k))
            .count();
            if sources != 1 {
                return Err(bad(
                    path,
                    "sigstoreSigned needs exactly one of keyPath, keyPaths, keyData, keyDatas, fulcio and pki",
                ));
            }
            let id = identity(path, m.get("signedIdentity"))?;
            if m.contains_key("fulcio") || m.contains_key("pki") {
                Requirement::Unsupported("keyless sigstore signatures (fulcio, pki)".into())
            } else if m.keys().any(|k| k.starts_with("rekorPublicKey")) {
                Requirement::Unsupported("Rekor transparency log checks (rekorPublicKey*)".into())
            } else {
                let mut keys = Vec::new();
                if let Some(p) = string(path, m, "keyPath")? {
                    keys.push(KeySource::Path(p.into()));
                }
                for p in strings(path, m, "keyPaths")?.unwrap_or_default() {
                    keys.push(KeySource::Path(p.into()));
                }
                if let Some(d) = string(path, m, "keyData")? {
                    keys.push(KeySource::Data(d.to_owned()));
                }
                for d in strings(path, m, "keyDatas")?.unwrap_or_default() {
                    keys.push(KeySource::Data(d));
                }
                if keys.is_empty() {
                    return Err(bad(path, "sigstoreSigned lists no keys"));
                }
                Requirement::Sigstore { keys, identity: id }
            }
        }
        other => return Err(bad(path, format!("unknown requirement type {other:?}"))),
    })
}

fn requirements(path: &Path, v: &Value, what: &str) -> Result<Vec<Requirement>, ImageError> {
    let a = v
        .as_array()
        .ok_or_else(|| bad(path, format!("{what} must be a list")))?;
    if a.is_empty() {
        return Err(bad(
            path,
            format!(
                "{what} is empty (use [{{\"type\": \"insecureAcceptAnything\"}}] to accept anything)"
            ),
        ));
    }
    a.iter().map(|r| requirement(path, r)).collect()
}

/// 공개키 PEM(`-----BEGIN PUBLIC KEY-----`)의 DER.
fn pem_der(pem: &str) -> Option<Vec<u8>> {
    let body = pem
        .trim()
        .strip_prefix("-----BEGIN PUBLIC KEY-----")?
        .trim_end()
        .strip_suffix("-----END PUBLIC KEY-----")?;
    let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
    crate::auth::base64_decode(&b64)
}

/// cosign·sigstore가 SHA-256으로 쓰는 알고리즘들.
const ALGORITHMS: &[&'static dyn VerificationAlgorithm] = &[
    &signature::ECDSA_P256_SHA256_ASN1,
    &signature::ECDSA_P384_SHA256_ASN1,
    &signature::RSA_PKCS1_2048_8192_SHA256,
    &signature::ED25519,
];

fn parse_key(der: &[u8]) -> Option<ParsedPublicKey> {
    ALGORITHMS
        .iter()
        .find_map(|alg| ParsedPublicKey::new(*alg, der).ok())
}

fn load_key(policy: &Path, k: &KeySource) -> Result<ParsedPublicKey, ImageError> {
    let (pem, name) = match k {
        KeySource::Path(p) => (
            std::fs::read_to_string(p)
                .map_err(|e| bad(policy, format!("key {}: {e}", p.display())))?,
            p.display().to_string(),
        ),
        KeySource::Data(d) => (
            crate::auth::base64_decode(d)
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or_else(|| bad(policy, "keyData is not base64 of a PEM key"))?,
            "keyData".to_owned(),
        ),
    };
    pem_der(&pem)
        .and_then(|der| parse_key(&der))
        .ok_or_else(|| {
            bad(
                policy,
                format!(
                    "{name}: not a PEM public key sbx can use (ECDSA P-256/P-384, RSA, Ed25519)"
                ),
            )
        })
}

/// payload를 엄격히 읽는다(containers와 같이 모르는 필드는 거절). 서명한 digest와 이름.
fn parse_payload(b: &[u8]) -> Result<(Digest, String), String> {
    let v: Value = serde_json::from_slice(b).map_err(|e| format!("payload: {e}"))?;
    let exact =
        |v: &Value, keys: &[&str], what: &str| -> Result<serde_json::Map<String, Value>, String> {
            let m = v
                .as_object()
                .ok_or(format!("payload {what} is not an object"))?;
            if m.len() != keys.len() || keys.iter().any(|k| !m.contains_key(*k)) {
                return Err(format!(
                    "payload {what} must have exactly {}",
                    keys.join(", ")
                ));
            }
            Ok(m.clone())
        };
    let top = exact(&v, &["critical", "optional"], "")?;
    let critical = exact(&top["critical"], &["type", "image", "identity"], "critical")?;
    if critical["type"].as_str() != Some(PAYLOAD_TYPE) {
        return Err(format!("payload type is not {PAYLOAD_TYPE:?}"));
    }
    let image = exact(
        &critical["image"],
        &["docker-manifest-digest"],
        "critical.image",
    )?;
    let identity = exact(
        &critical["identity"],
        &["docker-reference"],
        "critical.identity",
    )?;
    match &top["optional"] {
        Value::Null => {}
        Value::Object(o) => {
            if let Some(t) = o.get("timestamp")
                && !(t.is_i64() || t.is_u64())
            {
                return Err("payload optional.timestamp is not an integer".into());
            }
        }
        _ => return Err("payload optional is not an object".into()),
    }
    let digest = image["docker-manifest-digest"]
        .as_str()
        .and_then(|d| Digest::parse(d).ok())
        .ok_or("payload digest is not a sha256 digest")?;
    let reference = identity["docker-reference"]
        .as_str()
        .ok_or("payload docker-reference is not a string")?
        .to_owned();
    Ok((digest, reference))
}

/// `reference`가 `prefix`로 시작하고 그 뒤가 끝이나 구분자인가. containers의 remapIdentity와 같다.
fn strip_scope_prefix<'a>(reference: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = reference.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with(['/', ':', '@'])).then_some(rest)
}

fn repo_digest_or_exact(image: &Named, signed: &Named) -> bool {
    if image.digest.is_some() {
        image.repo == signed.repo
    } else {
        image == signed
    }
}

impl Identity {
    fn accepts(&self, image: &Named, signed: &Named) -> bool {
        match self {
            Identity::MatchExact => image == signed,
            Identity::MatchRepoDigestOrExact => repo_digest_or_exact(image, signed),
            Identity::MatchRepository => image.repo == signed.repo,
            Identity::ExactReference(r) => r == signed,
            Identity::ExactRepository(repo) => *repo == signed.repo,
            Identity::Remap {
                prefix,
                signed_prefix,
            } => {
                let full = image.to_string_full();
                let remapped = strip_scope_prefix(&full, prefix)
                    .and_then(|rest| Named::parse(&format!("{signed_prefix}{rest}")));
                repo_digest_or_exact(remapped.as_ref().unwrap_or(image), signed)
            }
        }
    }
}

/// docker 전송의 scope 후보, 구체적인 것부터.
fn docker_scopes(r: &Reference) -> Vec<String> {
    let repo = format!("{}/{}", r.registry, r.repository);
    let mut out = vec![match (&r.digest, &r.tag) {
        (Some(d), _) => format!("{repo}@{d}"),
        (None, Some(t)) => format!("{repo}:{t}"),
        (None, None) => repo.clone(),
    }];
    let mut ns = repo.as_str();
    out.push(ns.to_owned());
    while let Some(i) = ns.rfind('/') {
        ns = &ns[..i];
        out.push(ns.to_owned());
    }
    if !r.registry.contains(':') {
        let mut host = r.registry.as_str();
        while let Some(i) = host.find('.') {
            host = &host[i + 1..];
            out.push(format!("*.{host}"));
        }
    }
    out
}

/// oci·oci-archive 전송의 scope 후보: 절대 경로와 그 위 디렉터리들.
fn path_scopes(path: &Path) -> Vec<String> {
    path.ancestors()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.display().to_string())
        .collect()
}

impl Policy {
    /// 읽는다. `None`이거나 파일이 없으면 모두 받는 정책이다.
    pub fn load(file: Option<&Path>) -> Result<Self, ImageError> {
        let Some(path) = file else {
            return Ok(Self::default());
        };
        let text = match std::fs::read(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(bad(path, e)),
        };
        Self::parse(path, &text)
    }

    fn parse(path: &Path, text: &[u8]) -> Result<Self, ImageError> {
        let v: Value = serde_json::from_slice(text).map_err(|e| bad(path, e))?;
        let top = object(path, &v, "the policy", &["default", "transports"])?;
        let default = requirements(
            path,
            top.get("default")
                .ok_or_else(|| bad(path, "no default requirements"))?,
            "default",
        )?;
        let mut transports = BTreeMap::new();
        if let Some(t) = top.get("transports") {
            let t = t
                .as_object()
                .ok_or_else(|| bad(path, "transports must be an object"))?;
            for (name, scopes) in t {
                let scopes = scopes
                    .as_object()
                    .ok_or_else(|| bad(path, format!("transports.{name} must be an object")))?;
                let mut m = BTreeMap::new();
                for (scope, reqs) in scopes {
                    m.insert(
                        scope.clone(),
                        requirements(path, reqs, &format!("transports.{name}.{scope:?}"))?,
                    );
                }
                transports.insert(name.clone(), m);
            }
        }
        Ok(Self {
            file: Some(path.to_owned()),
            default,
            transports,
        })
    }

    fn file(&self) -> &Path {
        self.file.as_deref().unwrap_or(Path::new("policy.json"))
    }

    /// 가장 구체적인 scope의 요구와 그 scope 이름.
    fn select(&self, transport: &str, scopes: &[String]) -> (&[Requirement], String) {
        if let Some(t) = self.transports.get(transport) {
            for s in scopes {
                if let Some(r) = t.get(s) {
                    return (r, format!("transports.{transport}.{s:?}"));
                }
            }
            if let Some(r) = t.get("") {
                return (r, format!("transports.{transport}.\"\""));
            }
        }
        (&self.default, "default".to_owned())
    }

    /// 정책이 요구하는 것(서명이 필요한지)을 미리 본다. 서명 없이 결정되면 결과를 돌려준다.
    fn decide_without_signatures(
        &self,
        reqs: &[Requirement],
        scope: &str,
        image: &str,
    ) -> Option<Result<(), ImageError>> {
        if self.file.is_none() {
            return Some(Ok(()));
        }
        for r in reqs {
            match r {
                Requirement::Reject => {
                    return Some(Err(ImageError::Unsupported(format!(
                        "{image}: rejected by {} ({scope})",
                        self.file().display()
                    ))));
                }
                Requirement::Unsupported(what) => {
                    return Some(Err(ImageError::Unsupported(format!(
                        "{image}: {} ({scope}) requires {what}, which sbx can't check; \
                         use a sigstoreSigned key requirement or insecureAcceptAnything for it",
                        self.file().display()
                    ))));
                }
                Requirement::Accept | Requirement::Sigstore { .. } => {}
            }
        }
        (!reqs
            .iter()
            .any(|r| matches!(r, Requirement::Sigstore { .. })))
        .then_some(Ok(()))
    }

    /// 레지스트리 이미지 `wanted`(요청한 이름)에 서명이 필요한가. 필요 없으면 여기서 받거나 거절한다.
    pub fn needs_signatures(&self, wanted: &Reference) -> Result<bool, ImageError> {
        let (reqs, scope) = self.select("docker", &docker_scopes(wanted));
        match self.decide_without_signatures(reqs, &scope, &wanted.to_string()) {
            Some(r) => r.map(|()| false),
            None => Ok(true),
        }
    }

    /// 레지스트리 이미지를 서명과 함께 판단한다. `digest`는 최상위 manifest의 digest다.
    pub fn check(
        &self,
        wanted: &Reference,
        digest: &Digest,
        signatures: &[Signature],
    ) -> Result<(), ImageError> {
        let (reqs, scope) = self.select("docker", &docker_scopes(wanted));
        let name = wanted.to_string();
        if let Some(r) = self.decide_without_signatures(reqs, &scope, &name) {
            return r;
        }
        let image = Named::of(wanted);
        for r in reqs {
            let Requirement::Sigstore { keys, identity } = r else {
                continue;
            };
            let keys: Vec<ParsedPublicKey> = keys
                .iter()
                .map(|k| load_key(self.file(), k))
                .collect::<Result<_, _>>()?;
            let mut why = Vec::new();
            let ok = signatures.iter().any(|s| {
                if !keys.iter().any(|k| k.verify_sig(&s.payload, &s.signature).is_ok()) {
                    why.push("not signed by a key in the policy".to_owned());
                    return false;
                }
                match parse_payload(&s.payload) {
                    Err(e) => {
                        why.push(e);
                        false
                    }
                    Ok((d, _)) if d != *digest => {
                        why.push(format!("signed {d}, not this image"));
                        false
                    }
                    Ok((_, signed)) => match Named::parse(&signed) {
                        Some(n) if identity.accepts(&image, &n) => true,
                        _ => {
                            why.push(format!("signed as {signed:?}, which {scope}'s signedIdentity doesn't accept"));
                            false
                        }
                    },
                }
            });
            if !ok {
                why.dedup();
                return Err(ImageError::Invalid(format!(
                    "{name}: no valid signature as {} ({scope}) requires: {}",
                    self.file().display(),
                    if signatures.is_empty() {
                        format!(
                            "no cosign signature found (tag sha256-{}.sig)",
                            digest.hex()
                        )
                    } else {
                        why.join("; ")
                    }
                )));
            }
        }
        Ok(())
    }

    /// `sbx load`: `oci-archive`(tar) 또는 `oci`(디렉터리) 전송의 정책. `path`가 없으면(표준 입력) 그 전송의
    /// `""`와 `default`만 본다. archive의 서명은 확인할 수 없어서 서명을 요구하면 거절한다.
    pub fn check_archive(&self, transport: &str, path: Option<&Path>) -> Result<(), ImageError> {
        let scopes = path.map(path_scopes).unwrap_or_default();
        let (reqs, scope) = self.select(transport, &scopes);
        let image = path.map_or_else(|| "standard input".to_owned(), |p| p.display().to_string());
        match self.decide_without_signatures(reqs, &scope, &image) {
            Some(r) => r,
            None => Err(ImageError::Unsupported(format!(
                "{image}: {} ({scope}) requires signatures, and sbx can't check signatures in an archive",
                self.file().display()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_lc_rs::encoding::AsDer;
    use aws_lc_rs::rand::SystemRandom;
    use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

    fn policy(json: &str) -> Policy {
        Policy::parse(Path::new("policy.json"), json.as_bytes()).unwrap()
    }

    fn r(s: &str) -> Reference {
        Reference::parse(s).unwrap()
    }

    /// cosign generate-key-pair와 같은 P-256 키: (서명 키, keyData 값).
    fn key() -> (EcdsaKeyPair, String) {
        let k = EcdsaKeyPair::generate(&ECDSA_P256_SHA256_ASN1_SIGNING).unwrap();
        let der = k.public_key().as_der().unwrap();
        let b64 = crate::auth::base64_encode(der.as_ref());
        let pem = format!("-----BEGIN PUBLIC KEY-----\n{b64}\n-----END PUBLIC KEY-----\n");
        (k, crate::auth::base64_encode(pem.as_bytes()))
    }

    fn sign(k: &EcdsaKeyPair, digest: &Digest, reference: &str) -> Signature {
        let payload = format!(
            r#"{{"critical":{{"identity":{{"docker-reference":"{reference}"}},"image":{{"docker-manifest-digest":"{digest}"}},"type":"cosign container image signature"}},"optional":{{"creator":"t","timestamp":1}}}}"#
        )
        .into_bytes();
        let sig = k.sign(&SystemRandom::new(), &payload).unwrap();
        Signature {
            payload,
            signature: sig.as_ref().to_vec(),
        }
    }

    fn d(n: u8) -> Digest {
        Digest::parse(&format!("sha256:{}", format!("{n:02x}").repeat(32))).unwrap()
    }

    #[test]
    fn the_most_specific_scope_wins() {
        let p = policy(
            r#"{"default":[{"type":"reject"}],"transports":{"docker":{
                "docker.io":[{"type":"insecureAcceptAnything"}],
                "docker.io/library/alpine:3.20":[{"type":"reject"}],
                "*.corp.example":[{"type":"insecureAcceptAnything"}]}}}"#,
        );
        assert!(!p.needs_signatures(&r("busybox")).unwrap());
        assert!(p.needs_signatures(&r("alpine:3.20")).is_err());
        assert!(!p.needs_signatures(&r("alpine:3.21")).unwrap());
        assert!(!p.needs_signatures(&r("reg.eu.corp.example/a/b:1")).unwrap());
        let e = p
            .needs_signatures(&r("ghcr.io/a/b:1"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("rejected") && e.contains("default"), "{e}");
        // 포트가 있으면 와일드카드는 맞지 않는다.
        assert!(p.needs_signatures(&r("reg.corp.example:5000/a:1")).is_err());
    }

    #[test]
    fn no_file_accepts_everything_and_bad_files_are_errors() {
        assert!(
            !Policy::default()
                .needs_signatures(&r("x.example/a:1"))
                .unwrap()
        );
        assert!(Policy::default().check_archive("oci-archive", None).is_ok());
        for bad in [
            r#"{}"#,
            r#"{"default":[]}"#,
            r#"{"default":[{"type":"nope"}]}"#,
            r#"{"default":[{"type":"reject","extra":1}]}"#,
            r#"{"default":[{"type":"sigstoreSigned"}]}"#,
            r#"{"default":[{"type":"sigstoreSigned","keyPath":"/a","keyData":"b"}]}"#,
            r#"{"default":[{"type":"sigstoreSigned","keyPath":"/a","signedIdentity":{"type":"exactReference","dockerReference":"repo"}}]}"#,
            r#"{"default":[{"type":"insecureAcceptAnything"}],"transports":{"docker":{"x.example":[]}}}"#,
        ] {
            assert!(
                Policy::parse(Path::new("p"), bad.as_bytes()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn requirements_sbx_cant_check_reject_only_their_images() {
        let p = policy(
            r#"{"default":[{"type":"insecureAcceptAnything"}],"transports":{"docker":{
                "registry.access.redhat.com":[{"type":"signedBy","keyType":"GPGKeys","keyPath":"/etc/pki/x"}],
                "ghcr.io/keyless":[{"type":"sigstoreSigned","fulcio":{"caPath":"/x","oidcIssuer":"i","subjectEmail":"e"},"rekorPublicKeyPath":"/r"}]}}}"#,
        );
        assert!(!p.needs_signatures(&r("alpine")).unwrap());
        let e = p
            .needs_signatures(&r("registry.access.redhat.com/ubi9:latest"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("GPG"), "{e}");
        let e = p
            .needs_signatures(&r("ghcr.io/keyless/x:1"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("keyless"), "{e}");
    }

    #[test]
    fn signatures_need_the_key_the_digest_and_the_name() {
        let (k, data) = key();
        let (other, _) = key();
        let p = policy(&format!(
            r#"{{"default":[{{"type":"reject"}}],"transports":{{"docker":{{
                "ghcr.io/team":[{{"type":"sigstoreSigned","keyData":"{data}","signedIdentity":{{"type":"matchRepository"}}}}],
                "ghcr.io/exact":[{{"type":"sigstoreSigned","keyData":"{data}"}}]}}}}}}"#
        ));
        let img = r("ghcr.io/team/app:1");
        assert!(p.needs_signatures(&img).unwrap());
        let good = sign(&k, &d(1), "ghcr.io/team/app");
        p.check(&img, &d(1), std::slice::from_ref(&good)).unwrap();
        // 다른 서명이 섞여 있어도 하나가 맞으면 된다.
        p.check(
            &img,
            &d(1),
            &[sign(&other, &d(1), "ghcr.io/team/app"), good.clone()],
        )
        .unwrap();
        let e = |sigs: &[Signature]| p.check(&img, &d(1), sigs).unwrap_err().to_string();
        assert!(e(&[]).contains("no cosign signature found"));
        assert!(e(&[sign(&other, &d(1), "ghcr.io/team/app")]).contains("not signed by a key"));
        assert!(e(&[sign(&k, &d(2), "ghcr.io/team/app")]).contains("not this image"));
        assert!(e(&[sign(&k, &d(1), "ghcr.io/team/other")]).contains("signedIdentity"));
        // 서명을 고치면 맞지 않는다.
        let mut tampered = good.clone();
        tampered.payload[10] ^= 1;
        assert!(e(&[tampered]).contains("not signed by a key"));
        // 기본(matchRepoDigestOrExact): 태그로 받으면 태그까지 같아야, digest로 받으면 저장소만.
        let exact = r("ghcr.io/exact/app:1");
        assert!(
            p.check(&exact, &d(1), &[sign(&k, &d(1), "ghcr.io/exact/app")])
                .is_err()
        );
        p.check(&exact, &d(1), &[sign(&k, &d(1), "ghcr.io/exact/app:1")])
            .unwrap();
        let by_digest = r(&format!("ghcr.io/exact/app@{}", d(1)));
        p.check(&by_digest, &d(1), &[sign(&k, &d(1), "ghcr.io/exact/app")])
            .unwrap();
    }

    #[test]
    fn identities() {
        let n = |s: &str| Named::parse(s).unwrap();
        let img = n("docker.io/library/busybox:latest");
        assert!(Identity::MatchExact.accepts(&img, &n("busybox:latest")));
        assert!(!Identity::MatchExact.accepts(&img, &n("busybox")));
        assert!(Identity::MatchRepository.accepts(&img, &n("index.docker.io/library/busybox")));
        assert!(Identity::ExactRepository("quay.io/a/b".into()).accepts(&img, &n("quay.io/a/b:x")));
        assert!(Identity::ExactReference(n("quay.io/a/b:1")).accepts(&img, &n("quay.io/a/b:1")));
        assert!(!Identity::ExactReference(n("quay.io/a/b:1")).accepts(&img, &n("quay.io/a/b:2")));
        let remap = Identity::Remap {
            prefix: "mirror.lan/hub".into(),
            signed_prefix: "docker.io/library".into(),
        };
        assert!(remap.accepts(&n("mirror.lan/hub/busybox:1"), &n("busybox:1")));
        assert!(!remap.accepts(&n("mirror.lan/hubx/busybox:1"), &n("busybox:1")));
        assert_eq!(n("a/b").tag, None);
        assert_eq!(n("a/b:1").tag.as_deref(), Some("1"));
    }

    #[test]
    fn payloads_are_read_strictly() {
        let ok = format!(
            r#"{{"critical":{{"identity":{{"docker-reference":"a/b"}},"image":{{"docker-manifest-digest":"{}"}},"type":"cosign container image signature"}},"optional":null}}"#,
            d(1)
        );
        assert_eq!(parse_payload(ok.as_bytes()).unwrap().0, d(1));
        for bad in [
            ok.replace(r#","optional":null"#, ""),
            ok.replace("cosign container image signature", "other"),
            ok.replace(r#""type""#, r#""extra":1,"type""#),
            ok.replace("null", r#"{"timestamp":1.5}"#),
            ok.replace(&d(1).to_string(), "md5:00"),
        ] {
            assert!(parse_payload(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn archives_follow_their_transport() {
        let p = policy(
            r#"{"default":[{"type":"reject"}],"transports":{
                "oci-archive":{"/srv/images":[{"type":"insecureAcceptAnything"}]},
                "oci":{"":[{"type":"sigstoreSigned","keyPath":"/k"}]}}}"#,
        );
        p.check_archive("oci-archive", Some(Path::new("/srv/images/a.tar")))
            .unwrap();
        assert!(
            p.check_archive("oci-archive", Some(Path::new("/tmp/a.tar")))
                .is_err()
        );
        assert!(p.check_archive("oci-archive", None).is_err());
        let e = p
            .check_archive("oci", Some(Path::new("/x")))
            .unwrap_err()
            .to_string();
        assert!(e.contains("can't check signatures"), "{e}");
    }
}
