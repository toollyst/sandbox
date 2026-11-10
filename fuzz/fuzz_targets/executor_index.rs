//! 실행기 저장소 색인 (`sbx-executor-repo`의 `Index::parse`)과 그 색인에서 고르기(`available`, `pick`).
//! 색인은 저장소 서버가 준 JSON이다. 저장소는 믿는 쪽이지만, 깨진 색인 때문에 `sbx`가 죽거나 엉뚱한 것을 골라서는
//! 안 된다.
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - 받아들인 색인의 모든 이름과 버전은 `NAME@VERSION`으로 다시 읽어 같은 이름과 그 버전이 된다(설치할 수 있는
//!   이름이다. `latest`처럼 "가장 높은 것"으로 읽히는 버전은 없다).
//! - 버전은 `version_cmp` 순서다. 묶음 URL은 받는 scheme이고, sha256은 소문자 16진수 64자이며, 플랫폼은 겹치지 않는다.
//!   어댑터에는 `protocol`이 있고 `adapter`가 없다. 실행기에는 둘 중 하나가 있다.
//! - `pick`은 그 이름과 플랫폼에서 버전이 가장 높은 것을 고르고, 버전을 주면 그 버전으로 시작하는 것만 고른다.

#![no_main]

use std::cmp::Ordering;

use libfuzzer_sys::fuzz_target;
use sbx_executor_install::{Spec, version_cmp, version_matches};
use sbx_executor_repo::index::{self, Executor};
use sbx_executor_repo::{Index, location};
use url::Url;

fn check_list(list: &[Executor], adapters: bool) {
    for e in list {
        let mut last: Option<&str> = None;
        for v in &e.versions {
            let spec = format!("{}@{}", e.name, v.version);
            assert_eq!(
                spec.parse::<Spec>().ok(),
                Some(Spec {
                    name: e.name.clone(),
                    version: Some(v.version.clone()),
                }),
                "{spec:?} is in the index but does not name that version"
            );
            if let Some(prev) = last {
                assert_ne!(version_cmp(prev, &v.version), Ordering::Greater);
            }
            last = Some(&v.version);
            if adapters {
                assert!(v.protocol.is_some() && v.adapter.is_none());
            } else {
                assert!(v.protocol.is_some() || v.adapter.is_some());
            }
            for (i, a) in v.artifacts.iter().enumerate() {
                assert!(location::check(&a.url).is_ok(), "{}", a.url);
                assert_eq!(a.sha256.len(), 64);
                assert!(
                    a.sha256
                        .bytes()
                        .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                );
                assert!(v.artifacts[..i].iter().all(|b| b.platform != a.platform));
            }
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let base = Url::parse("https://repo.example/executors/v2/index.json").unwrap();
    let Ok(idx) = Index::parse(data, &base) else {
        return;
    };
    check_list(&idx.executors, false);
    check_list(&idx.adapters, true);

    let indexes = vec![
        ("first".to_owned(), idx.clone()),
        ("second".to_owned(), idx),
    ];
    let (_, first) = &indexes[0];
    for e in first.executors.iter().chain(&first.adapters) {
        for v in &e.versions {
            for a in &v.artifacts {
                for protocol in [v.protocol.unwrap_or(1), 1] {
                    let all = index::available(&indexes, Some(&e.name), &a.platform, protocol);
                    let adapters =
                        index::available_adapters(&indexes, Some(&e.name), &a.platform, protocol);
                    for list in [&all, &adapters] {
                        for w in list.windows(2) {
                            assert_ne!(version_cmp(&w[0].version, &w[1].version), Ordering::Less);
                        }
                    }
                    let want = [None, Some(v.version.clone())];
                    for version in want {
                        let spec = Spec {
                            name: e.name.clone(),
                            version: version.clone(),
                        };
                        for (picked, list) in [
                            (index::pick(&indexes, &spec, &a.platform, protocol), &all),
                            (
                                index::pick_adapter(&indexes, &spec, &a.platform, protocol),
                                &adapters,
                            ),
                        ] {
                            let Ok(c) = picked else {
                                continue;
                            };
                            assert_eq!(c.name, e.name);
                            assert_eq!(c.artifact.platform, a.platform);
                            assert_eq!(
                                c.repository, "first",
                                "same version must come from the first repository"
                            );
                            if let Some(want) = &version {
                                assert!(version_matches(want, &c.version));
                            }
                            for other in list.iter() {
                                if version
                                    .as_deref()
                                    .is_none_or(|w| version_matches(w, &other.version))
                                {
                                    assert_ne!(
                                        version_cmp(&other.version, &c.version),
                                        Ordering::Greater,
                                        "picked {} over {}",
                                        c.version,
                                        other.version
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }
});
