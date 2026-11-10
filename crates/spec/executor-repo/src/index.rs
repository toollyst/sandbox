//! 색인(`index.json`) 형식과 고르기. 형식은 크레이트 문서에 있다.

use std::collections::BTreeMap;

use sbx_executor_install::{Spec, version_cmp};
use serde::Deserialize;
use url::Url;

use crate::location;
use crate::platform::Platform;

/// 이 클라이언트가 쓰는(가장 새) 색인 `schema`.
pub const SCHEMA: u32 = 2;

/// 이 클라이언트가 읽는 색인 `schema`들.
pub const SCHEMAS: &[u32] = &[1, 2];

/// 색인의 크기 한도 (바이트).
pub const MAX_INDEX_BYTES: u64 = 16 << 20;

/// 묶음 하나 (플랫폼 하나의 압축 파일).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// 이 묶음이 도는 플랫폼.
    pub platform: Platform,
    /// 압축 파일의 URL (색인 URL 기준으로 풀어 둔 것).
    pub url: Url,
    /// 압축 파일의 SHA-256 (소문자 16진수 64자).
    pub sha256: String,
    /// 압축 파일의 바이트 수.
    pub size: u64,
}

/// 실행기나 어댑터 버전 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// 버전.
    pub version: String,
    /// 이 묶음이 말하는 실행기 규약(`sbx-executor-proto`) 버전. 자체 완결 실행기와 어댑터에 있다.
    pub protocol: Option<u32>,
    /// VMM 묶음이면 그것을 돌리는 어댑터 이름 (schema 2).
    pub adapter: Option<String>,
    /// 플랫폼별 묶음.
    pub artifacts: Vec<Artifact>,
}

/// 실행기 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executor {
    /// 이름.
    pub name: String,
    /// 한 줄 설명.
    pub description: Option<String>,
    /// 버전들 (낮은 것부터, [`version_cmp`] 순서).
    pub versions: Vec<Version>,
}

/// 읽은 색인.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// 실행기들 (이름 순).
    pub executors: Vec<Executor>,
    /// 어댑터들 (이름 순, schema 2). 모양은 실행기와 같고 버전마다 `protocol`이 있다.
    pub adapters: Vec<Executor>,
}

#[derive(Deserialize)]
struct RawSchema {
    schema: u32,
}

#[derive(Deserialize)]
struct RawIndex {
    #[serde(default)]
    executors: BTreeMap<String, RawExecutor>,
    #[serde(default)]
    adapters: BTreeMap<String, RawExecutor>,
}

#[derive(Deserialize)]
struct RawExecutor {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    versions: BTreeMap<String, RawVersion>,
}

#[derive(Deserialize)]
struct RawVersion {
    #[serde(default)]
    protocol: Option<u32>,
    #[serde(default)]
    adapter: Option<String>,
    #[serde(default)]
    artifacts: Vec<RawArtifact>,
}

/// 목록 하나(실행기 또는 어댑터)의 버전 규칙.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// schema 1의 실행기: `protocol`이 있다(`adapter`는 모르는 필드로 본다).
    ExecutorV1,
    /// schema 2의 실행기: `protocol`과 `adapter` 중 정확히 하나.
    ExecutorV2,
    /// 어댑터: `protocol`이 있고 `adapter`는 없다.
    Adapter,
}

#[derive(Deserialize)]
struct RawArtifact {
    os: String,
    architecture: String,
    url: String,
    sha256: String,
    size: u64,
}

impl Index {
    /// 색인을 읽는다. `url`은 이 색인의 URL이다(상대 URL을 이것 기준으로 푼다). 규약에 맞지 않는 곳이 하나라도
    /// 있으면 색인 전체를 거부한다(저장소를 만든 쪽이 잘못을 바로 보게).
    pub fn parse(bytes: &[u8], url: &Url) -> Result<Self, String> {
        let schema: RawSchema =
            serde_json::from_slice(bytes).map_err(|e| format!("not a repository index: {e}"))?;
        if !SCHEMAS.contains(&schema.schema) {
            return Err(format!(
                "schema {} is not supported (this sbx reads schema {}){}",
                schema.schema,
                SCHEMAS
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" and "),
                if schema.schema > SCHEMA {
                    "; a newer sbx is needed"
                } else {
                    ""
                }
            ));
        }
        let raw: RawIndex = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        let (executor_kind, adapters) = if schema.schema == 1 {
            // schema 1에는 어댑터가 없다. 같은 이름의 필드가 있어도 모르는 필드로 본다.
            (Kind::ExecutorV1, BTreeMap::new())
        } else {
            (Kind::ExecutorV2, raw.adapters)
        };
        Ok(Self {
            executors: entries(raw.executors, executor_kind, url)?,
            adapters: entries(adapters, Kind::Adapter, url)?,
        })
    }

    /// 이름으로 찾는다.
    pub fn executor(&self, name: &str) -> Option<&Executor> {
        self.executors.iter().find(|e| e.name == name)
    }

    /// 어댑터를 이름으로 찾는다.
    pub fn adapter(&self, name: &str) -> Option<&Executor> {
        self.adapters.iter().find(|e| e.name == name)
    }
}

fn entries(
    raw: BTreeMap<String, RawExecutor>,
    kind: Kind,
    url: &Url,
) -> Result<Vec<Executor>, String> {
    let mut out = Vec::new();
    for (name, e) in raw {
        let mut versions = Vec::new();
        for (version, v) in e.versions {
            let spec = format!("{name}@{version}");
            spec.parse::<Spec>().map_err(|_| {
                format!(
                    "invalid {} name or version {spec:?}",
                    if kind == Kind::Adapter {
                        "adapter"
                    } else {
                        "executor"
                    }
                )
            })?;
            let adapter = if kind == Kind::ExecutorV1 {
                None
            } else {
                v.adapter
            };
            match (kind, v.protocol, &adapter) {
                (Kind::ExecutorV1 | Kind::Adapter, None, _) => {
                    return Err(format!("{spec}: missing \"protocol\""));
                }
                (Kind::Adapter, _, Some(_)) => {
                    return Err(format!("{spec}: an adapter has no \"adapter\""));
                }
                (Kind::ExecutorV2, Some(_), Some(_)) | (Kind::ExecutorV2, None, None) => {
                    return Err(format!(
                        "{spec}: give exactly one of \"protocol\" (a self-contained executor) or \"adapter\" (a VMM package)"
                    ));
                }
                _ => {}
            }
            if let Some(a) = &adapter {
                a.parse::<Spec>()
                    .ok()
                    .filter(|s| s.version.is_none())
                    .ok_or_else(|| format!("{spec}: invalid adapter name {a:?}"))?;
            }
            let mut artifacts: Vec<Artifact> = Vec::new();
            for a in v.artifacts {
                let platform = Platform {
                    os: a.os,
                    architecture: a.architecture,
                };
                if artifacts.iter().any(|x| x.platform == platform) {
                    return Err(format!("{spec}: two artifacts for {platform}"));
                }
                let artifact_url = url
                    .join(&a.url)
                    .map_err(|e| format!("{spec}: artifact URL {:?}: {e}", a.url))?;
                location::check(&artifact_url).map_err(|e| format!("{spec}: {e}"))?;
                if a.sha256.len() != 64
                    || !a
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(format!(
                        "{spec}: sha256 must be 64 lowercase hex digits, not {:?}",
                        a.sha256
                    ));
                }
                artifacts.push(Artifact {
                    platform,
                    url: artifact_url,
                    sha256: a.sha256,
                    size: a.size,
                });
            }
            versions.push(Version {
                version,
                protocol: v.protocol,
                adapter,
                artifacts,
            });
        }
        versions.sort_by(|a, b| version_cmp(&a.version, &b.version));
        out.push(Executor {
            name,
            description: e.description,
            versions,
        });
    }
    Ok(out)
}

impl Version {
    /// 그 플랫폼의 묶음.
    pub fn artifact(&self, platform: &Platform) -> Option<&Artifact> {
        self.artifacts.iter().find(|a| &a.platform == platform)
    }
}

/// 설치할 수 있는 것 하나: 어느 저장소의 어느 실행기 버전의 어느 묶음.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 저장소 이름.
    pub repository: String,
    /// 실행기 이름.
    pub name: String,
    /// 버전.
    pub version: String,
    /// 실행기 규약 버전 (자체 완결 실행기와 어댑터).
    pub protocol: Option<u32>,
    /// VMM 묶음이면 그것을 돌리는 어댑터 이름.
    pub adapter: Option<String>,
    /// 이 플랫폼의 묶음.
    pub artifact: Artifact,
}

impl Candidate {
    /// `<이름>@<버전>`.
    pub fn spec(&self) -> Spec {
        Spec {
            name: self.name.clone(),
            version: Some(self.version.clone()),
        }
    }
}

/// 저장소들(우선순위 순서)에서 이 플랫폼과 규약 버전으로 설치할 수 있는 실행기를 모두 고른다.
/// `name`을 주면 그 실행기만. 이름 순, 같은 이름은 높은 버전부터, 같은 버전은 앞 저장소부터.
/// VMM 묶음(어댑터가 있는 것)은 규약 버전과 상관없이 나온다. 맞는 어댑터는 [`pick_adapter`]로 따로 고른다.
pub fn available(
    indexes: &[(String, Index)],
    name: Option<&str>,
    platform: &Platform,
    protocol: u32,
) -> Vec<Candidate> {
    candidates(indexes, |i| &i.executors, name, platform, protocol)
}

/// [`available`]과 같고 어댑터를 고른다. 규약 버전이 맞는 것만.
pub fn available_adapters(
    indexes: &[(String, Index)],
    name: Option<&str>,
    platform: &Platform,
    protocol: u32,
) -> Vec<Candidate> {
    candidates(indexes, |i| &i.adapters, name, platform, protocol)
}

fn candidates(
    indexes: &[(String, Index)],
    list: fn(&Index) -> &Vec<Executor>,
    name: Option<&str>,
    platform: &Platform,
    protocol: u32,
) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (repo, index) in indexes {
        for e in list(index) {
            if name.is_some_and(|n| n != e.name) {
                continue;
            }
            for v in &e.versions {
                if v.adapter.is_none() && v.protocol != Some(protocol) {
                    continue;
                }
                if let Some(a) = v.artifact(platform) {
                    out.push(Candidate {
                        repository: repo.clone(),
                        name: e.name.clone(),
                        version: v.version.clone(),
                        protocol: v.protocol,
                        adapter: v.adapter.clone(),
                        artifact: a.clone(),
                    });
                }
            }
        }
    }
    // 안정 정렬이라 같은 버전은 저장소 순서를 지킨다.
    out.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| version_cmp(&b.version, &a.version))
    });
    out
}

/// 하나를 고른다. 버전을 주면 그 버전, 아니면 가장 높은 버전. 같은 버전이 여러 저장소에 있으면 앞 저장소.
/// 없으면 왜 없는지(이름이 없다, 버전이 없다, 이 플랫폼 빌드가 없다, 규약 버전이 다르다) 알려 준다.
pub fn pick(
    indexes: &[(String, Index)],
    spec: &Spec,
    platform: &Platform,
    protocol: u32,
) -> Result<Candidate, String> {
    pick_from(
        indexes,
        Index::executor,
        "executor",
        spec,
        platform,
        protocol,
    )
}

/// [`pick`]과 같고 어댑터를 고른다. VMM 묶음이 이름으로 가리킨 어댑터를 이렇게 고른다(버전 없이: 가장 높은 것).
pub fn pick_adapter(
    indexes: &[(String, Index)],
    spec: &Spec,
    platform: &Platform,
    protocol: u32,
) -> Result<Candidate, String> {
    pick_from(indexes, Index::adapter, "adapter", spec, platform, protocol)
}

fn pick_from(
    indexes: &[(String, Index)],
    entry: for<'a> fn(&'a Index, &str) -> Option<&'a Executor>,
    kind: &str,
    spec: &Spec,
    platform: &Platform,
    protocol: u32,
) -> Result<Candidate, String> {
    let list = if kind == "adapter" {
        available_adapters
    } else {
        available
    };
    let found = list(indexes, Some(&spec.name), platform, protocol)
        .into_iter()
        .find(|c| spec.version.as_deref().is_none_or(|v| v == c.version));
    if let Some(c) = found {
        return Ok(c);
    }
    // 이유를 찾는다.
    let versions: Vec<&Version> = indexes
        .iter()
        .filter_map(|(_, i)| entry(i, &spec.name))
        .flat_map(|e| &e.versions)
        .filter(|v| spec.version.as_deref().is_none_or(|x| x == v.version))
        .collect();
    let what = match &spec.version {
        Some(v) => format!("{}@{v}", spec.name),
        None => spec.name.clone(),
    };
    if versions.is_empty() {
        return Err(
            if indexes.iter().any(|(_, i)| entry(i, &spec.name).is_some()) {
                format!("{what} is not in any repository")
            } else {
                format!("no {kind} named {:?} in any repository", spec.name)
            },
        );
    }
    let for_platform: Vec<&&Version> = versions
        .iter()
        .filter(|v| v.artifact(platform).is_some())
        .collect();
    if for_platform.is_empty() {
        let mut have: Vec<String> = versions
            .iter()
            .flat_map(|v| &v.artifacts)
            .map(|a| a.platform.to_string())
            .collect();
        have.sort();
        have.dedup();
        return Err(format!(
            "{what} has no build for {platform} (available: {})",
            if have.is_empty() {
                "none".into()
            } else {
                have.join(", ")
            }
        ));
    }
    let mut protocols: Vec<u32> = for_platform.iter().filter_map(|v| v.protocol).collect();
    protocols.sort();
    protocols.dedup();
    let list: Vec<String> = protocols.iter().map(u32::to_string).collect();
    Err(format!(
        "{what} speaks executor protocol {}; this sbx speaks {protocol}",
        list.join(", ")
    ))
}
