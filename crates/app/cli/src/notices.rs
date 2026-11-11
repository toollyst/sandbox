//! 저장소가 거두거나(`yanked`) 알림(`advisory`)을 붙인 실행기·어댑터 버전의 기록.
//!
//! `executor ls`와 tui는 저장소에 닿지 않는다. 그래서 색인을 받을 때마다(`search`, `install`, `adapter install`)
//! 거둔 것과 알림이 있는 버전만 `<cache_dir>/executor-notices.json`에 남기고, 설치한 것을 보일 때 이것과 맞춘다.
//! 받은 저장소의 줄만 새로 쓰고 다른 저장소의 줄은 둔다. 캐시라서 지워도 된다(다음에 받을 때 다시 생긴다).
//!
//! 설치한 것과는 이름, 버전, 받은 저장소(`.sbx-origin`)가 모두 같아야 맞는다. 다른 저장소의 같은 이름은 다른 것이다.
//! 쓰지 못해도 오류가 아니다(읽기 전용 캐시).

use std::fs;
use std::path::{Path, PathBuf};

use sbx_executor_repo::index::Index;
use serde::{Deserialize, Serialize};

/// 실행기인가 어댑터인가.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// 실행기(VMM 묶음 포함).
    Executor,
    /// 어댑터.
    Adapter,
}

/// 버전 하나의 알림.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// 저장소 이름.
    pub repository: String,
    /// 실행기나 어댑터.
    pub kind: Kind,
    /// 이름.
    pub name: String,
    /// 버전.
    pub version: String,
    /// 거뒀는가.
    pub yanked: bool,
    /// 알림 한 줄(저장소가 준 것을 다듬은 것).
    pub advisory: Option<String>,
}

fn file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("executor-notices.json")
}

/// 남긴 알림. 없거나 읽지 못하면 빈 목록이다.
pub fn load(cache_dir: &Path) -> Vec<Notice> {
    fs::read(file(cache_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// 받은 색인들의 알림으로 그 저장소들의 줄을 바꾼다.
pub fn record(cache_dir: &Path, indexes: &[(String, Index)]) {
    let mut all: Vec<Notice> = load(cache_dir)
        .into_iter()
        .filter(|n| !indexes.iter().any(|(r, _)| *r == n.repository))
        .collect();
    for (repo, idx) in indexes {
        for (kind, list) in [
            (Kind::Executor, &idx.executors),
            (Kind::Adapter, &idx.adapters),
        ] {
            for e in list {
                for v in &e.versions {
                    if v.yanked || v.advisory.is_some() {
                        all.push(Notice {
                            repository: repo.clone(),
                            kind,
                            name: e.name.clone(),
                            version: v.version.clone(),
                            yanked: v.yanked,
                            advisory: v.advisory.clone(),
                        });
                    }
                }
            }
        }
    }
    let Ok(bytes) = serde_json::to_vec(&all) else {
        return;
    };
    let path = file(cache_dir);
    if fs::create_dir_all(cache_dir).is_err() {
        return;
    }
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    if fs::write(&tmp, bytes).is_err() || fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
}

/// 설치한 것 하나의 알림.
pub fn find<'a>(
    notices: &'a [Notice],
    kind: Kind,
    name: &str,
    version: &str,
    origin: Option<&str>,
) -> Option<&'a Notice> {
    let origin = origin?;
    notices.iter().find(|n| {
        n.kind == kind && n.name == name && n.version == version && n.repository == origin
    })
}

/// 사람에게 보일 한 줄: `wick@1.18.0 is yanked by official: <알림>`.
pub fn describe(n: &Notice) -> String {
    let what = if n.yanked {
        format!("{}@{} is yanked by {}", n.name, n.version, n.repository)
    } else {
        format!(
            "{}@{} has an advisory from {}",
            n.name, n.version, n.repository
        )
    };
    match &n.advisory {
        Some(a) => format!("{what}: {a}"),
        None => what,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(yanked: bool, advisory: Option<&str>) -> Index {
        let mut v = serde_json::json!({
            "schema": 2,
            "executors": {"wick": {"versions": {"1.0.0": {"adapter": "fc", "yanked": yanked, "artifacts": []},
                                                 "2.0.0": {"adapter": "fc", "artifacts": []}}}},
            "adapters": {"fc": {"versions": {"0.1.0": {"protocol": 2, "artifacts": []}}}}
        });
        if let Some(a) = advisory {
            v["adapters"]["fc"]["versions"]["0.1.0"]["advisory"] = a.into();
        }
        let repo = sbx_executor_repo::location::repository("https://example.com/").unwrap();
        let url = sbx_executor_repo::location::index_v2(&repo);
        Index::parse(v.to_string().as_bytes(), &url).unwrap()
    }

    #[test]
    fn notices_are_recorded_per_repository_and_matched_by_origin() {
        let d = std::env::temp_dir().join(format!("sbx-notices-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        record(&d, &[("a".into(), index(true, Some("CVE-1")))]);
        record(&d, &[("b".into(), index(false, None))]);
        let n = load(&d);
        assert_eq!(n.len(), 2, "{n:?}");
        let y = find(&n, Kind::Executor, "wick", "1.0.0", Some("a")).unwrap();
        assert!(y.yanked);
        assert_eq!(describe(y), "wick@1.0.0 is yanked by a");
        assert!(find(&n, Kind::Executor, "wick", "1.0.0", Some("b")).is_none());
        assert!(find(&n, Kind::Executor, "wick", "1.0.0", None).is_none());
        let adv = find(&n, Kind::Adapter, "fc", "0.1.0", Some("a")).unwrap();
        assert_eq!(describe(adv), "fc@0.1.0 has an advisory from a: CVE-1");
        // 저장소 a를 다시 받으면 a의 줄만 바뀐다.
        record(&d, &[("a".into(), index(false, None))]);
        assert!(load(&d).is_empty());
        let _ = fs::remove_dir_all(&d);
    }
}
