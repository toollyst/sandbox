//! 실행기 이름과 버전 (`sbx-executor-install`의 `Spec`, `version_cmp`, `version_matches`, `version_line`)과 저장소 URL
//! (`sbx-executor-repo`의 `location::repository`). `Spec`은 명령줄, 색인, 그리고 지금 폴더나 그 위의 `.sbx-executor`
//! 파일(남이 만든 저장소를 받으면 그 안에 있다)에서 온다.
//!
//! 입력은 텍스트 세 줄(버전 셋)과 나머지다. 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - `Spec`은 표시한 문자열을 다시 읽으면 같다. 이름과 버전은 `[A-Za-z0-9._+-]`이고 `.`으로 시작하지 않는다
//!   (설치 디렉터리 이름이 되므로 경로 구분자나 `..`이 없다).
//! - `version_cmp`는 전순서다: 반사, 반대칭, 추이.
//! - `version_matches(a, b)`면 `b`는 `a`로 시작하고, `version_line(v)`는 `v`의 앞부분이며 `-`, `+`가 없다.
//! - 받아들인 저장소 URL은 디렉터리 URL(`/`로 끝남)이고 받는 scheme이며 query와 fragment가 없다.

#![no_main]

use std::cmp::Ordering;

use libfuzzer_sys::fuzz_target;
use sbx_executor_install::{Spec, version_cmp, version_line, version_matches};
use sbx_executor_repo::location;

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut lines = text.splitn(4, '\n');
    let a = lines.next().unwrap_or_default();
    let b = lines.next().unwrap_or_default();
    let c = lines.next().unwrap_or_default();
    let rest = lines.next().unwrap_or_default();

    for s in [a, b, c, rest, text] {
        if let Ok(spec) = s.parse::<Spec>() {
            assert!(valid_part(&spec.name));
            if let Some(v) = &spec.version {
                assert!(valid_part(v) && v != "latest");
            }
            let shown = spec.to_string();
            assert_eq!(shown.parse::<Spec>().ok(), Some(spec), "{s:?} → {shown:?}");
        }
        if let Ok(url) = location::repository(s) {
            assert!(url.path().ends_with('/'));
            assert!(url.query().is_none() && url.fragment().is_none());
            assert!(location::check(&url).is_ok());
            assert!(location::index(&url).as_str().starts_with(url.as_str()));
            assert!(location::index_v2(&url).as_str().starts_with(url.as_str()));
        }
    }

    for v in [a, b, c] {
        assert_eq!(version_cmp(v, v), Ordering::Equal, "{v:?}");
        let line = version_line(v);
        assert!(v.starts_with(line) && !line.contains(['-', '+']));
        assert!(version_matches(v, v));
    }
    for (x, y) in [(a, b), (b, c), (a, c)] {
        assert_eq!(
            version_cmp(x, y),
            version_cmp(y, x).reverse(),
            "{x:?} {y:?}"
        );
        if version_matches(x, y) {
            assert!(y.starts_with(x));
        }
    }
    let (ab, bc, ac) = (version_cmp(a, b), version_cmp(b, c), version_cmp(a, c));
    if ab != Ordering::Greater && bc != Ordering::Greater {
        assert_ne!(
            ac,
            Ordering::Greater,
            "{a:?} <= {b:?} <= {c:?} but {a:?} > {c:?}"
        );
    }
    if ab != Ordering::Less && bc != Ordering::Less {
        assert_ne!(
            ac,
            Ordering::Less,
            "{a:?} >= {b:?} >= {c:?} but {a:?} < {c:?}"
        );
    }
});
