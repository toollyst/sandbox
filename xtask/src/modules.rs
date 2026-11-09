//! 크레이트 안 모듈 사이의 의존 방향 검사.
//!
//! 규칙: 모듈 의존에 순환이 없어야 한다. 그러면 모듈은 위아래로 줄 세울 수 있고,
//! 아래 모듈은 위 모듈을 모른다. 함께 쓰는 것은 둘 다보다 아래 모듈로 내린다.
//!
//! 의존으로 세는 것: 주석이 아닌 코드의 `crate::경로`, `super::경로`, 그리고 하위 모듈 이름으로 바로 쓰는 `x::경로`.
//! `mod x;` 선언은 세지 않는다(담는 것이지 쓰는 것이 아니다). `pub use x::...`는 센다.
//! 파일 안에 바로 쓴 `mod 이름 { ... }`도 하위 모듈로 본다(중괄호 짝으로 범위를 정한다).
//! 경로는 알려진 모듈 중 가장 긴 접두사로 정한다. 모듈이 아니면 크레이트 루트에 있는 항목이다.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// `src` 기준 상대 경로를 모듈 경로로. 루트는 빈 문자열.
fn module_of(rel: &Path) -> String {
    let mut parts: Vec<String> = rel
        .with_extension("")
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if matches!(parts.last().map(String::as_str), Some("mod")) {
        parts.pop();
    }
    if parts.len() == 1 && matches!(parts[0].as_str(), "lib" | "main") {
        parts.clear();
    }
    parts.join("::")
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_owned(),
        (_, true) => a.to_owned(),
        _ => format!("{a}::{b}"),
    }
}

fn parent(m: &str) -> String {
    m.rsplit_once("::")
        .map(|(p, _)| p.to_owned())
        .unwrap_or_default()
}

/// 주석, 문자열, 문자 리터럴의 내용을 공백으로 바꾼다. 줄바꿈과 따옴표는 남긴다.
/// 그래야 문자열 안의 `crate::`나 `'{'` 같은 것을 코드로 읽지 않는다.
fn strip_comments(src: &str) -> String {
    let c: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let blank = |out: &mut String, ch: char| out.push(if ch == '\n' { '\n' } else { ' ' });
    let mut i = 0;
    while i < c.len() {
        let next = c.get(i + 1).copied().unwrap_or('\0');
        if c[i] == '/' && next == '/' {
            while i < c.len() && c[i] != '\n' {
                out.push(' ');
                i += 1;
            }
        } else if c[i] == '/' && next == '*' {
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    blank(&mut out, c[i]);
                    i += 1;
                }
            }
        } else if c[i] == 'r' && (next == '"' || next == '#') && (i == 0 || !is_ident(c[i - 1])) {
            // 원시 문자열 r"...", r#"..."#
            let mut j = i + 1;
            let mut hashes = 0;
            while c.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            if c.get(j) != Some(&'"') {
                out.push(c[i]);
                i += 1;
                continue;
            }
            for _ in i..=j {
                out.push(' ');
            }
            i = j + 1;
            while i < c.len() {
                if c[i] == '"' && (0..hashes).all(|k| c.get(i + 1 + k) == Some(&'#')) {
                    for _ in 0..=hashes {
                        out.push(' ');
                    }
                    i += 1 + hashes;
                    break;
                }
                blank(&mut out, c[i]);
                i += 1;
            }
        } else if c[i] == '"' {
            out.push('"');
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    out.push(' ');
                    i += 1;
                }
                if i < c.len() {
                    blank(&mut out, c[i]);
                    i += 1;
                }
            }
            out.push('"');
            i += 1;
        } else if c[i] == '\'' && (next == '\\' || c.get(i + 2) == Some(&'\'')) {
            // 문자 리터럴 'x', '\n', '\u{..}'. 수명('a)은 닫는 따옴표가 바로 오지 않으므로 여기 오지 않는다.
            out.push('\'');
            i += 1;
            if c.get(i) == Some(&'\\') {
                out.push(' ');
                i += 1;
            }
            while i < c.len() && c[i] != '\'' {
                out.push(' ');
                i += 1;
            }
            out.push('\'');
            i += 1;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `s`의 앞에서 `a::b::c` 경로를 읽는다. `{`를 만나면 묶음 안의 경로들을 모두 읽는다.
fn read_paths(s: &str, prefix: &str, out: &mut Vec<String>) {
    let mut path = prefix.to_owned();
    let mut rest = s;
    loop {
        let end = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
        if end == 0 {
            if let Some(group) = rest.strip_prefix('{') {
                // 묶음: 깊이 0의 쉼표로 나눈다.
                let mut depth = 0;
                let mut start = 0;
                for (i, c) in group.char_indices() {
                    match c {
                        '{' => depth += 1,
                        '}' if depth == 0 => {
                            read_paths(group[start..i].trim(), &path, out);
                            return;
                        }
                        '}' => depth -= 1,
                        ',' if depth == 0 => {
                            read_paths(group[start..i].trim(), &path, out);
                            start = i + 1;
                        }
                        _ => {}
                    }
                }
            }
            break;
        }
        path = join(&path, &rest[..end]);
        rest = &rest[end..];
        match rest.strip_prefix("::") {
            Some(r) => rest = r,
            None => break,
        }
    }
    out.push(path);
}

/// 파일 안의 `mod 이름 { ... }` 영역들: (시작, 끝, 모듈 경로). 안쪽 모듈도 포함한다.
fn inline_modules(text: &str, file_module: &str) -> Vec<(usize, usize, String)> {
    let bytes = text.as_bytes();
    let mut out: Vec<(usize, usize, String)> = Vec::new();
    let mut from = 0;
    while let Some(i) = text[from..].find("mod ") {
        let at = from + i;
        from = at + 4;
        if at > 0 && is_ident(bytes[at - 1] as char) {
            continue;
        }
        let rest = &text[at + 4..];
        let name_len = rest.find(|c: char| !is_ident(c)).unwrap_or(rest.len());
        if name_len == 0 {
            continue;
        }
        let after = rest[name_len..].trim_start();
        if !after.starts_with('{') {
            continue;
        }
        let open = at + 4 + name_len + (rest[name_len..].len() - after.len());
        let mut depth = 0usize;
        let mut close = text.len();
        for (j, c) in text[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = open + j;
                        break;
                    }
                }
                _ => {}
            }
        }
        let parent = out
            .iter()
            .filter(|(s, e, _)| *s <= at && at < *e)
            .max_by_key(|(s, _, _)| *s)
            .map(|r| r.2.clone())
            .unwrap_or_else(|| file_module.to_owned());
        out.push((open, close, join(&parent, &rest[..name_len])));
    }
    out
}

/// 크레이트 하나의 모듈 순환을 찾는다.
pub fn check(crate_name: &str, src: &Path) -> Vec<String> {
    let mut files = Vec::new();
    rs_files(src, &mut files);
    let has_lib = src.join("lib.rs").is_file();
    let files: Vec<_> = files
        .into_iter()
        .filter(|f| !(has_lib && f == &src.join("main.rs")))
        .collect();

    let mut modules: BTreeSet<String> = files
        .iter()
        .map(|f| module_of(f.strip_prefix(src).unwrap_or(f)))
        .collect();
    let mut texts = Vec::new();
    for f in &files {
        let m = module_of(f.strip_prefix(src).unwrap_or(f));
        let text = strip_comments(&std::fs::read_to_string(f).unwrap_or_default());
        let regions = inline_modules(&text, &m);
        modules.extend(regions.iter().map(|r| r.2.clone()));
        texts.push((m, text, regions));
    }

    let resolve = |path: &str| -> String {
        let segs: Vec<&str> = path.split("::").collect();
        (0..=segs.len())
            .rev()
            .map(|n| segs[..n].join("::"))
            .find(|p| modules.contains(p))
            .unwrap_or_default()
    };

    let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (m, text, regions) in &texts {
        let module_at = |pos: usize| -> String {
            regions
                .iter()
                .filter(|(s, e, _)| *s <= pos && pos < *e)
                .max_by_key(|(s, _, _)| *s)
                .map(|r| r.2.clone())
                .unwrap_or_else(|| m.clone())
        };
        for (kw, is_super) in [("crate::", false), ("super::", true)] {
            let mut from = 0;
            while let Some(i) = text[from..].find(kw) {
                let at = from + i;
                from = at + kw.len();
                if at > 0 && is_ident(text[..at].chars().last().unwrap_or(' ')) {
                    continue;
                }
                let here = module_at(at);
                let mut start = if is_super {
                    parent(&here)
                } else {
                    String::new()
                };
                let mut rest = &text[from..];
                if is_super {
                    while let Some(r) = rest.strip_prefix("super::") {
                        start = parent(&start);
                        rest = r;
                    }
                }
                let mut paths = Vec::new();
                read_paths(rest, &start, &mut paths);
                for p in paths {
                    let target = resolve(&p);
                    if target != here {
                        edges.entry(here.clone()).or_default().insert(target);
                    }
                }
            }
        }
        // 하위 모듈을 이름으로 바로 쓰는 경우 (`layer::read`, `pub use msg::*`).
        let bytes = text.as_bytes();
        let mut from = 0;
        while let Some(i) = text[from..].find("::") {
            let at = from + i;
            from = at + 2;
            let mut s = at;
            while s > 0 && is_ident(bytes[s - 1] as char) {
                s -= 1;
            }
            if s == at || (s > 0 && bytes[s - 1] == b':') {
                continue;
            }
            let here = module_at(s);
            let child = join(&here, &text[s..at]);
            if modules.contains(&child) && child != here {
                edges.entry(here).or_default().insert(child);
            }
        }
    }

    let mut problems = Vec::new();
    let mut done = BTreeSet::new();
    for m in &modules {
        let mut stack = Vec::new();
        find_cycle(m, &edges, &mut stack, &mut done, &mut problems, crate_name);
    }
    problems
}

fn name(m: &str) -> &str {
    if m.is_empty() { "crate" } else { m }
}

fn find_cycle(
    m: &str,
    edges: &BTreeMap<String, BTreeSet<String>>,
    stack: &mut Vec<String>,
    done: &mut BTreeSet<String>,
    problems: &mut Vec<String>,
    crate_name: &str,
) {
    if let Some(pos) = stack.iter().position(|s| s == m) {
        let cycle: Vec<&str> = stack[pos..]
            .iter()
            .map(|s| name(s))
            .chain([name(m)])
            .collect();
        problems.push(format!(
            "{crate_name}: module cycle (lower module depends on upper): {}",
            cycle.join(" -> ")
        ));
        return;
    }
    if done.contains(m) {
        return;
    }
    stack.push(m.to_owned());
    for next in edges.get(m).into_iter().flatten() {
        find_cycle(next, edges, stack, done, problems, crate_name);
    }
    stack.pop();
    done.insert(m.to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_in_groups() {
        let mut out = Vec::new();
        read_paths("{a::{B, c::D}, e, f::G as H};", "", &mut out);
        assert_eq!(out, vec!["a::B", "a::c::D", "e", "f::G"]);
        let mut out = Vec::new();
        read_paths("digest::sha256(x)", "", &mut out);
        assert_eq!(out, vec!["digest::sha256"]);
    }

    #[test]
    fn module_names() {
        assert_eq!(module_of(Path::new("lib.rs")), "");
        assert_eq!(module_of(Path::new("a/mod.rs")), "a");
        assert_eq!(module_of(Path::new("a/b.rs")), "a::b");
    }

    #[test]
    fn comments_strings_and_chars_are_blanked() {
        for (src, kept) in [
            ("a // crate::x\n", "a"),
            ("\"crate::x\" b", "\"\"b"),
            ("'{' x '\\n' 'a", "''x'''a"),
            ("r#\"mod a {\"# z", "z"),
            ("/* crate::a */b", "b"),
            ("\"a\\\"crate::\" c", "\"\"c"),
        ] {
            // 길이는 그대로, 공백을 빼면 코드와 따옴표만 남는다.
            let out = strip_comments(src);
            assert_eq!(out.chars().count(), src.chars().count(), "{src:?}");
            let squeezed: String = out.split_whitespace().collect();
            assert_eq!(squeezed, kept, "{src:?} -> {out:?}");
        }
    }

    #[test]
    fn detects_cycle_and_accepts_layers() {
        let dir = std::env::temp_dir().join(format!("xtask-modules-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.rs"), "mod a; mod b; use crate::a::run;\n").unwrap();
        std::fs::write(dir.join("a.rs"), "use crate::b::Out;\n").unwrap();
        std::fs::write(dir.join("b.rs"), "pub struct Out;\n").unwrap();
        assert!(check("ok", &dir).is_empty());

        std::fs::write(dir.join("b.rs"), "use crate::a::run; pub struct Out;\n").unwrap();
        let p = check("bad", &dir);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("a -> b -> a"), "{p:?}");

        // 하위 모듈이 루트 항목을 쓰고 루트도 그 모듈을 쓰면 순환이다.
        std::fs::write(dir.join("b.rs"), "pub struct Out;\n").unwrap();
        std::fs::write(dir.join("a.rs"), "use crate::Error;\n").unwrap();
        assert!(!check("bad", &dir).is_empty());

        // 파일 안 모듈도 본다: 루트가 guest::start를 쓰고 guest가 super의 항목을 쓰면 순환이다.
        std::fs::write(
            dir.join("lib.rs"),
            "struct Vm; fn f() { guest::start() } mod guest { use super::Vm; pub fn start() {} }\n",
        )
        .unwrap();
        std::fs::write(dir.join("a.rs"), "").unwrap();
        let p = check("inline", &dir);
        assert!(p.iter().any(|p| p.contains("guest")), "{p:?}");
        std::fs::write(dir.join("a.rs"), "use crate::Error;\n").unwrap();

        // 루트가 `crate::` 없이 하위 모듈을 써도 센다.
        std::fs::write(dir.join("lib.rs"), "mod a; mod b; fn f() { a::run() }\n").unwrap();
        assert!(!check("bad", &dir).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
