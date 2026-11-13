//! `.dockerignore` 맞추기(`sbx-dockerfile`의 `DockerIgnore`). 빌드 컨텍스트와 함께 오는 파일이다.
//!
//! 입력은 텍스트다. 첫 NUL 앞이 `.dockerignore` 내용이고, 뒤는 줄마다 컨텍스트 안 경로 하나다.
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다. 같은 경로는 같은 답이다. 오류의 줄 번호는 파일 안에 있다.
//! - 끝에 `**`를 더하면 모든 경로가 빠지고, 이어서 `!**`를 더하면 아무것도 빠지지 않는다(마지막 패턴이 이긴다).
//! - 앞에 주석 줄을 더해도 답이 같다.
//! - 예외 패턴이 없으면, 빠진 디렉터리 아래 경로도 빠진다(`has_exceptions`가 거짓이면 디렉터리를 건너뛰어도 된다).

#![no_main]

use libfuzzer_sys::fuzz_target;
use sbx_dockerfile::DockerIgnore;

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let (text, paths) = input.split_once('\0').unwrap_or((input, ""));
    let paths: Vec<&str> = paths.split('\n').take(64).collect();
    let ig = match DockerIgnore::parse(text) {
        Ok(ig) => ig,
        Err(e) => {
            let lines = text.lines().count();
            let line = e.line.expect("pattern errors have a line");
            assert!((1..=lines).contains(&line), "line {line} of {lines}");
            return;
        }
    };
    let mut base = text.to_owned();
    if !base.is_empty() && !base.ends_with('\n') {
        base.push('\n');
    }
    let all = DockerIgnore::parse(&format!("{base}**\n")).expect("adding ** keeps it valid");
    let none = DockerIgnore::parse(&format!("{base}**\n!**\n")).expect("adding !** keeps it valid");
    // BOM은 첫 줄에서만 버리므로 주석 뒤로 옮기지 않는다.
    let body = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let commented =
        DockerIgnore::parse(&format!("#sbx-fuzz\n{body}")).expect("a comment keeps it valid");
    for p in paths {
        let x = ig.is_excluded(p);
        assert_eq!(x, ig.is_excluded(p));
        assert!(all.is_excluded(p), "{p:?} not excluded by **");
        assert!(!none.is_excluded(p), "{p:?} excluded after !**");
        assert_eq!(x, commented.is_excluded(p), "{p:?}");
        // 정리된 경로(`.`, `..`, 빈 조각이 없는 것)만 본다. `a/..`의 위 디렉터리는 `a`가 아니다.
        let clean = p.split('/').all(|s| !s.is_empty() && s != "." && s != "..");
        if clean && !ig.has_exceptions() {
            let mut dir = p;
            while let Some(i) = dir.rfind('/') {
                dir = &dir[..i];
                if ig.is_excluded(dir) {
                    assert!(x, "{p:?} kept although {dir:?} is excluded");
                }
            }
        }
    }
});
