//! sbx 릴리스 색인(`sbx-release-index`의 `Index::parse`)과 그 색인에서 고르기(`status`, `target`).
//! 색인은 서명을 확인한 뒤에 읽지만, 서명한 색인이 깨졌어도 `sbx`가 죽거나 엉뚱한 버전을 골라서는 안 된다(새 버전
//! 알림은 명령마다 돈다).
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - 받아들인 색인의 버전은 겹치지 않고 낮은 것부터다. 묶음 URL은 `https`이고, sha256은 소문자 16진수 64자, 크기는
//!   1 이상 한도 이하, 알림에는 제어 문자가 없다.
//! - `status`의 `latest`는 지금보다 높고 거두지 않았고 그 플랫폼 묶음이 있는 것 중 가장 높은 것이다. `target`을 버전
//!   없이 부르면 같은 것을 고른다. 낮은 버전은 `allow_downgrade` 없이는 거부한다.

#![no_main]

use std::collections::BTreeSet;

use libfuzzer_sys::fuzz_target;
use sbx_release_index::index::MAX_ASSET_BYTES;
use sbx_release_index::{Index, Version, status, target};
use url::Url;

fuzz_target!(|data: &[u8]| {
    let base = Url::parse("https://toollyst.github.io/sandbox/releases.json").unwrap();
    let Ok(idx) = Index::parse(data, &base) else {
        return;
    };
    for w in idx.releases.windows(2) {
        assert!(w[0].version < w[1].version, "sorted and unique");
    }
    let mut platforms = BTreeSet::new();
    for r in &idx.releases {
        if let Some(a) = &r.advisory {
            assert!(!a.chars().any(char::is_control), "{a:?}");
        }
        for (p, a) in &r.assets {
            platforms.insert(p.clone());
            assert_eq!(a.url.scheme(), "https", "{}", a.url);
            assert_eq!(a.sha256.len(), 64);
            assert!(a.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
            assert!(a.size >= 1 && a.size <= MAX_ASSET_BYTES);
        }
    }
    platforms.insert("linux-x86_64".into());
    let mut currents: Vec<Version> = idx.releases.iter().map(|r| r.version).collect();
    currents.push(Version::parse("0.0.0").unwrap());
    for cur in currents {
        for p in &platforms {
            let s = status(cur, &idx, p);
            let best = idx
                .releases
                .iter()
                .filter(|r| r.version > cur && !r.yanked && r.assets.contains_key(p))
                .map(|r| r.version)
                .max();
            assert_eq!(s.latest.map(|r| r.version), best);
            assert_eq!(
                target(cur, &idx, p, None, false).unwrap().map(|r| r.version),
                best
            );
            for r in &idx.releases {
                let t = target(cur, &idx, p, Some(r.version), false);
                if !r.assets.contains_key(p) || r.version < cur {
                    assert!(t.is_err());
                } else if r.version == cur {
                    assert_eq!(t, Ok(None));
                } else {
                    assert_eq!(t.unwrap().map(|x| x.version), Some(r.version));
                }
            }
        }
    }
});
