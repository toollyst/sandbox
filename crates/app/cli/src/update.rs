//! 새 버전 알림. `sbx`를 쓰다가 더 높은 버전이 나왔으면 `sbx 0.2.1 (0.2.2 available)`처럼 알린다.
//! 방향은 `docs/plan/upgrade.md`, 색인 형식과 서명은 [`sbx_release_index`].
//!
//! 명시적 동작:
//!
//! - **언제 확인하나.** 릴리스 빌드(빌드 때 `SBX_RELEASE_BUILD=1`, release.yml이 준다)이고, `SBX_NO_UPDATE_CHECK`와 `CI`가
//!   비어 있고, `--format json`이 아니고, stderr가 터미널이고, 명령이 `mcp`·`completion`·`tui`·`upgrade`가 아닐 때.
//!   마지막 확인에서 [`CHECK_EVERY`](실패했으면 [`RETRY_AFTER`])가 지났을 때만 네트워크에 닿는다.
//! - **어떻게.** 명령과 함께 비동기 작업 하나가 색인을 받는다(연결 [`CONNECT_TIMEOUT`]). 명령이 먼저 끝나면 시작에서
//!   [`WAIT`]가 될 때까지만 기다리고 버린다. 보내는 것은 요청과 `User-Agent: sbx/<버전> (<os>; <arch>)`뿐이다.
//! - **무엇을 믿나.** 색인 키([`INDEX_KEYS`])의 서명, 기한, 본 serial(`<data_dir>/release-index.seen`)이 맞는 색인만.
//!   틀리면 알리지 않는다(가짜 "보안 고침"을 막는다).
//! - **기록.** `<cache_dir>/update-check.json`: 확인한 시각과 이 플랫폼의 릴리스 목록(버전, 보안 고침, 거둠, 알림). 캐시라
//!   지워도 된다. 쓰지 못해도 오류가 아니다.
//! - **알림.** 명령 출력 뒤 stderr에 한 줄. 보통은 하루 한 번, 보안 고침이 있거나 지금 버전을 거뒀으면 명령마다.
//!   [`FRESH_FOR`]보다 오래된 기록으로는 알리지 않는다.
//! - **다른 곳.** `sbx --version`(stdout이 터미널일 때), `sbx info`(첫 줄, JSON `update`)는 기록만 본다.
//! - **패키지 빌드.** 빌드 때 `SBX_UPGRADE_COMMAND`(예: `brew upgrade sbx`)를 주면 알림이 그 명령을 권한다.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sbx_release_index::{Asset, Index, Release, Source, Status, Version};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::cli::{Cli, Command, Format};

/// `releases.json`을 서명하는 색인 키(minisign 공개키, 한 줄 base64). 비어 있으면 알림이 없다.
pub const INDEX_KEYS: &[&str] = &[];

/// 릴리스 묶음을 서명하는 묶음 키. 비어 있으면 `sbx upgrade`가 아무것도 설치하지 않는다.
pub const ARCHIVE_KEYS: &[&str] = &[];

/// 성공한 확인 사이의 간격(초).
pub const CHECK_EVERY: u64 = 24 * 3600;
/// 실패한 뒤 다시 확인하기까지(초).
pub const RETRY_AFTER: u64 = 3600;
/// 이보다 오래된 기록으로는 알리지 않는다(초). 색인 기한(30일)과 같다.
pub const FRESH_FOR: u64 = 30 * 24 * 3600;
/// 명령이 끝난 뒤 확인을 기다리는 한도(시작에서부터).
pub const WAIT: Duration = Duration::from_secs(3);
/// 연결을 맺는 데 기다리는 한도.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

const CACHE_FILE: &str = "update-check.json";
const SEEN_FILE: &str = "release-index.seen";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// 이 바이너리의 버전.
pub fn current() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the workspace version is a release version")
}

/// 릴리스 빌드인가(빌드 때 `SBX_RELEASE_BUILD=1`).
pub fn release_build() -> bool {
    option_env!("SBX_RELEASE_BUILD") == Some("1")
}

/// 패키지 관리자 빌드가 권하는 올리기 명령(빌드 때 `SBX_UPGRADE_COMMAND`).
pub fn packaged_upgrade() -> Option<&'static str> {
    option_env!("SBX_UPGRADE_COMMAND").filter(|c| !c.is_empty())
}

/// 시험용 색인 위치와 키. 디버그 빌드에서만 환경 변수로 바꾼다(배포하는 릴리스 빌드에서는 바꿀 수 없다).
fn test_override(var: &str) -> Option<String> {
    if cfg!(debug_assertions) {
        std::env::var(var).ok().filter(|v| !v.is_empty())
    } else {
        None
    }
}

/// 색인이 있는 곳과 믿는 색인 키.
pub fn source() -> Source {
    let url = test_override("SBX_TEST_RELEASES_URL")
        .and_then(|u| Url::parse(&u).ok())
        .unwrap_or_else(|| Url::parse(sbx_release_index::OFFICIAL_URL).expect("official URL"));
    let keys = match test_override("SBX_TEST_RELEASE_INDEX_KEY") {
        Some(k) => vec![k],
        None => INDEX_KEYS.iter().map(|k| (*k).to_owned()).collect(),
    };
    Source { url, keys }
}

/// 묶음을 믿는 키.
pub fn archive_keys() -> Vec<String> {
    match test_override("SBX_TEST_RELEASE_ARCHIVE_KEY") {
        Some(k) => vec![k],
        None => ARCHIVE_KEYS.iter().map(|k| (*k).to_owned()).collect(),
    }
}

/// 짧게 기다리는 HTTP 클라이언트(`User-Agent: sbx/<버전> (<os>; <arch>)`).
pub fn fetcher(read: Duration) -> Result<sbx_executor_repo::Fetcher, String> {
    let ua = format!(
        "sbx/{} ({}; {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    sbx_executor_repo::Fetcher::with_options(&ua, CONNECT_TIMEOUT, read).map_err(|e| e.to_string())
}

/// 본 serial을 적는 곳.
pub fn seen(data_dir: &Path) -> sbx_executor_repo::Seen {
    sbx_executor_repo::Seen::new(data_dir.join(SEEN_FILE))
}

/// 색인을 받는다(서명·기한·본 serial 확인).
pub async fn fetch_index(data_dir: &Path, read: Duration) -> Result<Index, String> {
    let f = fetcher(read)?;
    let seen = seen(data_dir);
    sbx_release_index::fetch(&f, &source(), Some(&seen))
        .await
        .map_err(|e| e.to_string())
}

/// 기록한 릴리스 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CachedRelease {
    version: String,
    #[serde(default)]
    security: bool,
    #[serde(default)]
    yanked: bool,
    #[serde(default)]
    advisory: Option<String>,
    /// 이 플랫폼 묶음이 있다.
    #[serde(default)]
    build: bool,
}

/// `<cache_dir>/update-check.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Cache {
    /// 마지막으로 확인한 시각(성공이든 실패든).
    #[serde(default)]
    checked_at: u64,
    /// 마지막 확인이 실패했다.
    #[serde(default)]
    failed: bool,
    /// 마지막으로 성공한 시각.
    #[serde(default)]
    fetched_at: u64,
    #[serde(default)]
    releases: Vec<CachedRelease>,
    /// 마지막으로 알린 시각과 그때의 새 버전.
    #[serde(default)]
    notified_at: u64,
    #[serde(default)]
    notified_for: Option<String>,
}

impl Cache {
    fn load(cache_dir: &Path) -> Self {
        std::fs::read(cache_dir.join(CACHE_FILE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    fn save(&self, cache_dir: &Path) {
        let Ok(body) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if std::fs::create_dir_all(cache_dir).is_err() {
            return;
        }
        let tmp = cache_dir.join(format!(".{CACHE_FILE}.{}", std::process::id()));
        if std::fs::write(&tmp, body).is_ok()
            && std::fs::rename(&tmp, cache_dir.join(CACHE_FILE)).is_err()
        {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    fn due(&self, now: u64) -> bool {
        let wait = if self.failed {
            RETRY_AFTER
        } else {
            CHECK_EVERY
        };
        // 한 번도 안 했거나 시계가 뒤로 갔으면 다시 확인한다.
        self.checked_at == 0 || self.checked_at > now || now - self.checked_at >= wait
    }

    fn record(&mut self, got: Result<&Index, ()>, now: u64, platform: &str) {
        self.checked_at = now;
        match got {
            Ok(index) => {
                self.failed = false;
                self.fetched_at = now;
                self.releases = index
                    .releases
                    .iter()
                    .map(|r| CachedRelease {
                        version: r.version.to_string(),
                        security: r.security,
                        yanked: r.yanked,
                        advisory: r.advisory.clone(),
                        build: r.assets.contains_key(platform),
                    })
                    .collect();
            }
            Err(()) => self.failed = true,
        }
    }

    /// 기록을 색인 꼴로(묶음은 이 플랫폼에 있다는 표시만). 오래됐거나 없으면 `None`.
    fn index(&self, now: u64, platform: &str) -> Option<Index> {
        if self.fetched_at == 0 || self.fetched_at > now || now - self.fetched_at > FRESH_FOR {
            return None;
        }
        let marker = Asset {
            url: Url::parse("https://invalid/").expect("static URL"),
            sha256: String::new(),
            size: 0,
        };
        let releases = self
            .releases
            .iter()
            .filter_map(|r| {
                Some(Release {
                    version: Version::parse(&r.version).ok()?,
                    security: r.security,
                    yanked: r.yanked,
                    advisory: r.advisory.clone(),
                    layout_version: None,
                    protocol: None,
                    assets: if r.build {
                        [(platform.to_owned(), marker.clone())].into()
                    } else {
                        Default::default()
                    },
                })
            })
            .collect();
        Some(Index {
            serial: 0,
            expires: 0,
            releases,
        })
    }
}

/// 받은 색인을 알림 기록에 남긴다(`sbx upgrade`가 받은 것도 쓴다). 쓰지 못하면 그냥 둔다.
pub fn record(cache_dir: &Path, index: &Index) {
    let mut c = Cache::load(cache_dir);
    c.record(Ok(index), now(), &sbx_release_index::host_platform());
    c.save(cache_dir);
}

/// 알림 한 줄(앞의 `sbx 0.2.1` 포함). 알릴 것이 없으면 `None`.
pub fn summary(s: &Status<'_>) -> Option<String> {
    if !s.notable() {
        return None;
    }
    let mut parts = Vec::new();
    if s.yanked() {
        match s.advisory() {
            Some(a) => parts.push(format!("yanked: {a}")),
            None => parts.push("yanked".to_owned()),
        }
    } else if let Some(a) = s.advisory() {
        parts.push(format!("advisory: {a}"));
    }
    if let Some(l) = s.latest {
        if s.security {
            parts.push(format!("{} available, security fix", l.version));
        } else {
            parts.push(format!("{} available", l.version));
        }
    }
    Some(format!("sbx {} ({})", s.current, parts.join("; ")))
}

/// 무엇으로 올리라고 권하나.
pub fn upgrade_hint() -> String {
    packaged_upgrade().unwrap_or("sbx upgrade").to_owned()
}

/// 기록으로 본 지금 상태의 한 줄(`sbx --version`, `sbx info`). 기록이 없거나 알릴 것이 없으면 `None`.
pub fn cached_summary(cache_dir: &Path) -> Option<String> {
    let platform = sbx_release_index::host_platform();
    let index = Cache::load(cache_dir).index(now(), &platform)?;
    summary(&sbx_release_index::status(current(), &index, &platform))
}

/// `sbx info --format json`의 `update`. 기록이 없으면 `None`.
pub fn cached_info(cache_dir: &Path) -> Option<crate::output::UpdateInfo> {
    let platform = sbx_release_index::host_platform();
    let cache = Cache::load(cache_dir);
    let index = cache.index(now(), &platform)?;
    let s = sbx_release_index::status(current(), &index, &platform);
    Some(crate::output::UpdateInfo {
        latest: s.latest.map(|r| r.version.to_string()),
        security: s.latest.is_some() && s.security,
        yanked: s.yanked(),
        advisory: s.advisory().map(str::to_owned),
        checked_at: cache.fetched_at,
    })
}

/// `--version`의 글: stdout이 터미널이고 알릴 것이 있으면 `0.2.1 (0.2.2 available)`, 아니면 버전만.
pub fn version_text(cache_dir: Option<&Path>) -> String {
    let plain = env!("CARGO_PKG_VERSION").to_owned();
    if !std::io::stdout().is_terminal() {
        return plain;
    }
    cache_dir
        .and_then(cached_summary)
        .and_then(|s| s.strip_prefix("sbx ").map(str::to_owned))
        .unwrap_or(plain)
}

/// `sbx tui` 머리줄의 괄호 안(`0.2.2`, `0.2.2, security fix`, `yanked`)과 급한지. 알릴 것이 없으면 `None`.
pub fn short(cache_dir: &Path) -> Option<(String, bool)> {
    let platform = sbx_release_index::host_platform();
    let index = Cache::load(cache_dir).index(now(), &platform)?;
    let s = sbx_release_index::status(current(), &index, &platform);
    let text = match (s.latest, s.yanked()) {
        (Some(l), _) if s.security => format!("{}, security fix", l.version),
        (Some(l), true) => format!("{}; this one is yanked", l.version),
        (Some(l), false) => l.version.to_string(),
        (None, true) => "yanked".to_owned(),
        (None, false) => return None,
    };
    Some((text, s.urgent()))
}

/// 할 때가 됐으면 색인을 받아 기록한다(`sbx tui`가 뒤에서 부른다). 받지 못하면 실패로 기록한다.
pub async fn refresh(cache_dir: &Path, data_dir: &Path) {
    let mut cache = Cache::load(cache_dir);
    let now = now();
    if !checks_enabled() || !cache.due(now) {
        return;
    }
    let platform = sbx_release_index::host_platform();
    match fetch_index(data_dir, WAIT).await {
        Ok(index) => cache.record(Ok(&index), now, &platform),
        Err(_) => cache.record(Err(()), now, &platform),
    }
    cache.save(cache_dir);
}

/// 명령과 함께 도는 확인.
pub struct Check {
    cache_dir: PathBuf,
    started: Instant,
    task: Option<tokio::task::JoinHandle<Result<Index, String>>>,
}

/// 확인을 할 명령인가.
fn wanted(cli: &Cli) -> bool {
    let command_ok = !matches!(
        cli.command,
        Command::Mcp { .. } | Command::Completion { .. } | Command::Tui | Command::Upgrade(_)
    );
    command_ok
        && cli.format != Format::Json
        && checks_enabled()
        // 시험(디버그 빌드의 시험용 색인)에서는 터미널이 아니어도 본다.
        && (std::io::stderr().is_terminal() || test_override("SBX_TEST_RELEASES_URL").is_some())
}

/// 이 빌드와 환경에서 확인을 하는가(명령과 터미널은 빼고).
pub fn checks_enabled() -> bool {
    (release_build() || test_override("SBX_TEST_RELEASES_URL").is_some())
        && !env_set("SBX_NO_UPDATE_CHECK")
        && !env_set("CI")
}

/// 명령을 시작할 때 부른다. 할 때가 됐으면 색인 받기를 시작한다. 확인하지 않는 명령이면 `None`.
pub fn start(cli: &Cli) -> Option<Check> {
    if !wanted(cli) {
        return None;
    }
    let cache_dir = cli.cache_dir.clone().or_else(crate::dirs::cache_dir)?;
    let data_dir = cli.data_dir.clone().or_else(crate::dirs::data_dir)?;
    let task = Cache::load(&cache_dir)
        .due(now())
        .then(|| tokio::spawn(async move { fetch_index(&data_dir, WAIT).await }));
    Some(Check {
        cache_dir,
        started: Instant::now(),
        task,
    })
}

/// 명령이 끝난 뒤 부른다. 확인이 [`WAIT`] 안에 끝났으면 기록하고, 알릴 것이 있으면 stderr에 한 줄 쓴다.
pub async fn finish(check: Option<Check>) {
    let Some(c) = check else { return };
    let platform = sbx_release_index::host_platform();
    let mut cache = Cache::load(&c.cache_dir);
    let now = now();
    if let Some(task) = c.task {
        let left = WAIT.saturating_sub(c.started.elapsed());
        match tokio::time::timeout(left, task).await {
            Ok(Ok(Ok(index))) => cache.record(Ok(&index), now, &platform),
            Ok(_) => cache.record(Err(()), now, &platform),
            // 늦었다: 다음 명령이 다시 해 본다(기록하지 않는다).
            Err(_) => {}
        }
    }
    if let Some(line) = notice(&mut cache, now, &platform) {
        eprintln!("{line}");
    }
    cache.save(&c.cache_dir);
}

/// 지금 알릴 한 줄. 알리면 알린 시각을 적는다.
fn notice(cache: &mut Cache, now: u64, platform: &str) -> Option<String> {
    let index = cache.index(now, platform)?;
    let s = sbx_release_index::status(current(), &index, platform);
    let line = summary(&s)?;
    let latest = s.latest.map(|r| r.version.to_string());
    let quiet_since = cache.notified_at <= now && now - cache.notified_at < CHECK_EVERY;
    if !s.urgent() && quiet_since && cache.notified_for == latest {
        return None;
    }
    cache.notified_at = now;
    cache.notified_for = latest;
    Some(match s.latest {
        Some(_) => format!("{line}: run '{}'", upgrade_hint()),
        None => line,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(v: &str) -> CachedRelease {
        CachedRelease {
            version: v.into(),
            security: false,
            yanked: false,
            advisory: None,
            build: true,
        }
    }

    fn cache(releases: Vec<CachedRelease>, now: u64) -> Cache {
        Cache {
            checked_at: now,
            fetched_at: now,
            releases,
            ..Cache::default()
        }
    }

    const P: &str = "linux-x86_64";

    fn newer(v: &Version) -> String {
        Version {
            patch: v.patch + 1,
            ..*v
        }
        .to_string()
    }

    #[test]
    fn notice_is_daily_unless_urgent() {
        let cur = current();
        let t = 1_000_000;
        let mut c = cache(vec![rel(&cur.to_string()), rel(&newer(&cur))], t);
        let line = notice(&mut c, t, P).unwrap();
        assert_eq!(
            line,
            format!("sbx {cur} ({} available): run 'sbx upgrade'", newer(&cur))
        );
        assert!(notice(&mut c, t + 60, P).is_none(), "once a day");
        assert!(notice(&mut c, t + CHECK_EVERY, P).is_some(), "next day");

        let mut s = rel(&newer(&cur));
        s.security = true;
        let mut c = cache(vec![rel(&cur.to_string()), s], t);
        assert!(notice(&mut c, t, P).unwrap().contains("security fix"));
        assert!(
            notice(&mut c, t + 1, P).is_some(),
            "security: every command"
        );

        let mut y = rel(&cur.to_string());
        y.yanked = true;
        y.advisory = Some("data loss on stop".into());
        let mut c = cache(vec![y], t);
        assert_eq!(
            notice(&mut c, t, P).unwrap(),
            format!("sbx {cur} (yanked: data loss on stop)")
        );
    }

    #[test]
    fn nothing_without_a_fresh_record_or_a_build_for_this_platform() {
        let cur = current();
        let t = 1_000_000;
        let mut c = cache(vec![rel(&newer(&cur))], t);
        assert!(notice(&mut c, t + FRESH_FOR + 1, P).is_none(), "stale");
        let mut other = rel(&newer(&cur));
        other.build = false;
        let mut c = cache(vec![other], t);
        assert!(notice(&mut c, t, P).is_none(), "no build here");
        let mut c = cache(vec![rel(&cur.to_string())], t);
        assert!(notice(&mut c, t, P).is_none(), "up to date");
    }

    #[test]
    fn checks_are_spaced() {
        let mut c = Cache::default();
        assert!(c.due(100));
        c.checked_at = 100;
        assert!(!c.due(100 + CHECK_EVERY - 1));
        assert!(c.due(100 + CHECK_EVERY));
        c.failed = true;
        assert!(c.due(100 + RETRY_AFTER));
        assert!(c.due(50), "clock went back");
    }
}
