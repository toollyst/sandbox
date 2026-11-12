//! 색인 읽기. 형식은 크레이트 문서에 있다.

use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;
use url::Url;

/// 읽는 형식 버전.
pub const SCHEMA: u64 = 1;
/// 색인의 크기 한도 (바이트).
pub const MAX_INDEX_BYTES: u64 = 1 << 20;
/// 릴리스 수의 한도.
pub const MAX_RELEASES: usize = 2000;
/// 묶음 하나의 크기 한도 (바이트).
pub const MAX_ASSET_BYTES: u64 = 256 << 20;
/// 플랫폼 이름의 길이 한도.
const MAX_PLATFORM_CHARS: usize = 32;

/// 정식 릴리스 버전 `MAJOR.MINOR.PATCH`. 각 자리는 앞에 0을 붙이지 않은 수다. `-rc`, `+build` 같은 꼬리는 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    /// 주 버전.
    pub major: u64,
    /// 부 버전.
    pub minor: u64,
    /// 패치.
    pub patch: u64,
}

impl Version {
    /// `0.2.1`이나 `v0.2.1`을 읽는다.
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = || format!("{s:?} is not a release version like 0.2.1");
        let body = s.strip_prefix('v').unwrap_or(s);
        let mut parts = body.split('.');
        let mut next = || -> Result<u64, String> {
            let p = parts.next().ok_or_else(bad)?;
            if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad());
            }
            if p.len() > 1 && p.starts_with('0') {
                return Err(bad());
            }
            p.parse().map_err(|_| bad())
        };
        let v = Self {
            major: next()?,
            minor: next()?,
            patch: next()?,
        };
        if parts.next().is_some() {
            return Err(bad());
        }
        Ok(v)
    }

    /// 주·부 버전이 같은가(같은 줄의 패치끼리). 0.x에서는 부 버전이 바뀌면 깨질 수 있다.
    pub fn same_line(&self, other: &Self) -> bool {
        self.major == other.major && self.minor == other.minor
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// 묶음 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// 압축 파일 URL(색인 기준으로 풀어 둔 것).
    pub url: Url,
    /// SHA-256 (소문자 16진수 64자).
    pub sha256: String,
    /// 바이트 수.
    pub size: u64,
}

/// 릴리스 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// 버전.
    pub version: Version,
    /// 보안 고침을 담는가.
    pub security: bool,
    /// 거뒀는가.
    pub yanked: bool,
    /// 알림 한 줄(다듬은 것).
    pub advisory: Option<String>,
    /// 데이터·캐시 디렉터리 배치 버전.
    pub layout_version: Option<u32>,
    /// 실행기 규약 범위 `(min, max)`.
    pub protocol: Option<(u32, u32)>,
    /// 플랫폼 → 묶음.
    pub assets: BTreeMap<String, Asset>,
}

/// 읽은 색인. 릴리스는 버전 순(낮은 것부터)이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    /// 일련번호.
    pub serial: u64,
    /// 기한(Unix 초).
    pub expires: u64,
    /// 릴리스들.
    pub releases: Vec<Release>,
}

#[derive(Deserialize)]
struct RawIndex {
    schema: u64,
    serial: Option<u64>,
    expires: Option<u64>,
    #[serde(default)]
    releases: Vec<RawRelease>,
}

#[derive(Deserialize)]
struct RawRelease {
    version: String,
    #[serde(default)]
    security: bool,
    #[serde(default)]
    yanked: bool,
    advisory: Option<String>,
    layout_version: Option<u32>,
    protocol: Option<RawProtocol>,
    #[serde(default)]
    assets: BTreeMap<String, RawAsset>,
}

#[derive(Deserialize)]
struct RawProtocol {
    min: u32,
    max: u32,
}

#[derive(Deserialize)]
struct RawAsset {
    url: String,
    sha256: String,
    size: u64,
}

#[derive(Deserialize)]
struct Schema {
    schema: u64,
}

fn platform_ok(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= MAX_PLATFORM_CHARS
        && p.contains('-')
        && p.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

impl Index {
    /// `url`에서 받은 색인을 읽는다. 상대 URL은 `url` 기준으로 푼다.
    pub fn parse(bytes: &[u8], url: &Url) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_INDEX_BYTES {
            return Err(format!("larger than {MAX_INDEX_BYTES} bytes"));
        }
        let schema: Schema = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if schema.schema != SCHEMA {
            return Err(format!(
                "schema {} is not supported (this sbx reads schema {SCHEMA})",
                schema.schema
            ));
        }
        let raw: RawIndex = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        debug_assert_eq!(raw.schema, SCHEMA);
        let (Some(serial), Some(expires)) = (raw.serial, raw.expires) else {
            return Err("serial and expires are required".into());
        };
        if raw.releases.len() > MAX_RELEASES {
            return Err(format!("more than {MAX_RELEASES} releases"));
        }
        let mut releases = Vec::with_capacity(raw.releases.len());
        for r in raw.releases {
            let version = Version::parse(&r.version)?;
            let at = |m: String| format!("release {version}: {m}");
            let protocol = match r.protocol {
                Some(p) if p.min > p.max => {
                    return Err(at(format!("protocol min {} is above max {}", p.min, p.max)));
                }
                Some(p) => Some((p.min, p.max)),
                None => None,
            };
            let mut assets = BTreeMap::new();
            for (platform, a) in r.assets {
                if !platform_ok(&platform) {
                    return Err(at(format!(
                        "{platform:?} is not a platform like linux-x86_64"
                    )));
                }
                let asset_url = url
                    .join(&a.url)
                    .map_err(|e| at(format!("{platform}: url {:?}: {e}", a.url)))?;
                sbx_executor_repo::location::check_artifact(url, &asset_url)
                    .map_err(|e| at(format!("{platform}: {e}")))?;
                if a.sha256.len() != 64
                    || !a
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(at(format!(
                        "{platform}: sha256 must be 64 lowercase hex digits"
                    )));
                }
                if a.size == 0 || a.size > MAX_ASSET_BYTES {
                    return Err(at(format!(
                        "{platform}: size must be 1 to {MAX_ASSET_BYTES} bytes"
                    )));
                }
                assets.insert(
                    platform,
                    Asset {
                        url: asset_url,
                        sha256: a.sha256,
                        size: a.size,
                    },
                );
            }
            releases.push(Release {
                version,
                security: r.security,
                yanked: r.yanked,
                advisory: r
                    .advisory
                    .as_deref()
                    .and_then(sbx_executor_repo::index::advisory_text),
                layout_version: r.layout_version,
                protocol,
                assets,
            });
        }
        releases.sort_by_key(|r| r.version);
        if let Some(w) = releases.windows(2).find(|w| w[0].version == w[1].version) {
            return Err(format!("release {} is listed twice", w[0].version));
        }
        Ok(Self {
            serial,
            expires,
            releases,
        })
    }

    /// 그 버전의 릴리스.
    pub fn get(&self, v: &Version) -> Option<&Release> {
        self.releases.iter().find(|r| r.version == *v)
    }
}
