//! `check-links`: 저장소의 Markdown 문서에서 상대 링크와 앵커가 가리키는 곳이 있는지 본다.
//!
//! ```text
//! cargo xtask check-links
//! ```
//!
//! - 대상: 저장소 뿌리 아래의 `*.md`. `target`, `fuzz/corpus`·`fuzz/artifacts`, `.github` 말고 `.`으로 시작하는
//!   디렉터리(`.git`, 다른 작업 트리가 있는 `.claude`)는 보지 않는다.
//! - 링크: `[글](곳)`, `![글](곳)`, `[이름]: 곳`, HTML의 `href="곳"`·`src="곳"`. 코드 블록(```` ``` ````, `~~~`)과
//!   `` `코드` `` 안은 링크가 아니다. 스킴이 있는 주소(`https:`, `mailto:`)와 `//`로 시작하는 주소는 보지 않는다.
//! - 경로는 그 문서의 디렉터리에서 푼다. 파일이나 디렉터리가 있어야 하고, 이름의 대소문자까지 같아야 한다(macOS의
//!   파일 시스템은 가리지 않지만 GitHub는 가린다).
//! - 앵커(`#…`)는 대상이 Markdown일 때만 본다: 그 문서의 제목에서 GitHub가 만드는 앵커이거나 `id="…"`·`name="…"`여야
//!   한다. 제목의 앵커는 GitHub와 같이 만든다([`slug`]): 보이는 글자만 남기고(코드 표시, 링크 주소, 강조, HTML 태그를
//!   뺀다) 소문자로, 글자·숫자·`-`·`_`·공백 말고는 지우고, 공백은 `-`로. 같은 앵커가 또 나오면 `-1`, `-2`를 붙인다.
//! - 소스 파일의 앵커(`lib.rs#L10`)는 보지 않는다.
//!
//! `cargo test -p xtask links`가 저장소 전체를 본다. 그래서 CI의 `cargo test`가 끊긴 링크를 잡는다.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// 보지 않는 디렉터리(뿌리에서의 상대 경로).
const SKIP: &[&str] = &["target", "fuzz/corpus", "fuzz/artifacts", "fuzz/target"];

/// 저장소의 Markdown 문서를 모두 보고, 본 링크 수를 돌려준다. 끊긴 것이 있으면 하나씩 적어 돌려준다.
pub fn check(root: &Path) -> Result<usize, Vec<String>> {
    let mut files = Vec::new();
    collect(root, root, &mut files).map_err(|e| vec![e])?;
    files.sort();
    let mut anchors: BTreeMap<PathBuf, BTreeSet<String>> = BTreeMap::new();
    let mut problems = Vec::new();
    let mut n = 0;
    for file in &files {
        let text =
            std::fs::read_to_string(file).map_err(|e| vec![format!("{}: {e}", file.display())])?;
        let rel = file.strip_prefix(root).unwrap_or(file);
        for (line, dest) in links(&text) {
            n += 1;
            let (path, anchor) = match dest.split_once('#') {
                Some((p, a)) => (p, Some(a)),
                None => (dest.as_str(), None),
            };
            let path = path.replace("%20", " ");
            let target = if path.is_empty() {
                file.clone()
            } else {
                match resolve(root, file, &path) {
                    Ok(t) => t,
                    Err(e) => {
                        problems.push(format!("{}:{line}: {dest}: {e}", rel.display()));
                        continue;
                    }
                }
            };
            let Some(anchor) = anchor.filter(|a| !a.is_empty()) else {
                continue;
            };
            if target.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if !anchors.contains_key(&target) {
                let t = std::fs::read_to_string(&target).unwrap_or_default();
                anchors.insert(target.clone(), anchors_of(&t));
            }
            if !anchors[&target].contains(anchor) {
                problems.push(format!(
                    "{}:{line}: {dest}: no heading or id makes the anchor #{anchor}",
                    rel.display()
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(n)
    } else {
        Err(problems)
    }
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let kind = entry
            .file_type()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if kind.is_dir() {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let rel = rel.to_string_lossy().replace('\\', "/");
            if (name.starts_with('.') && name != ".github") || SKIP.contains(&rel.as_str()) {
                continue;
            }
            collect(root, &path, out)?;
        } else if kind.is_file() && name.ends_with(".md") {
            out.push(path);
        }
    }
    Ok(())
}

/// `file`에서 쓴 상대 경로 `link`가 가리키는 곳. 저장소 밖으로 나가거나, 없거나, 대소문자가 다르면 오류.
fn resolve(root: &Path, file: &Path, link: &str) -> Result<PathBuf, String> {
    let base = if link.starts_with('/') {
        root.to_path_buf()
    } else {
        file.parent().unwrap_or(root).to_path_buf()
    };
    let mut parts: Vec<String> = base
        .strip_prefix(root)
        .unwrap_or(Path::new(""))
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    for part in link.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err("leaves the repository".into());
                }
            }
            p => parts.push(p.to_owned()),
        }
    }
    let mut at = root.to_path_buf();
    for part in &parts {
        let names: Vec<String> = std::fs::read_dir(&at)
            .map_err(|_| "no such file or directory".to_owned())?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        if !names.iter().any(|n| n == part) {
            return Err(match names.iter().find(|n| n.eq_ignore_ascii_case(part)) {
                Some(n) => {
                    format!("no such file or directory (there is {n}; GitHub tells case apart)")
                }
                None => "no such file or directory".into(),
            });
        }
        at.push(part);
    }
    Ok(at)
}

/// 문서의 링크 대상들(줄 번호, 대상). 코드 블록과 코드 표시 안은 빼고, 스킴이 있는 주소도 뺀다.
fn links(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for (i, raw) in text.lines().enumerate() {
        let trimmed = raw.trim_start();
        if let Some(f) = fence {
            if trimmed.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let line = strip_code(raw);
        let mut found = Vec::new();
        // [글](곳), ![글](곳)
        let b = line.as_bytes();
        let mut k = 0;
        while let Some(off) = line[k..].find("](") {
            let start = k + off + 2;
            let mut depth = 1;
            let mut end = start;
            while end < b.len() {
                match b[end] {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                end += 1;
            }
            if end >= b.len() {
                break;
            }
            let inner = line[start..end].trim();
            let dest = if let Some(rest) = inner.strip_prefix('<') {
                rest.split('>').next().unwrap_or("")
            } else {
                inner.split_whitespace().next().unwrap_or("")
            };
            found.push(dest.to_owned());
            k = end;
        }
        // [이름]: 곳
        if let Some(rest) = trimmed.strip_prefix('[')
            && let Some((label, dest)) = rest.split_once("]:")
            && !label.is_empty()
            && !label.contains(']')
            && let Some(d) = dest.split_whitespace().next()
        {
            found.push(d.to_owned());
        }
        // href="곳", src="곳"
        for attr in ["href=\"", "src=\""] {
            let mut k = 0;
            while let Some(off) = line[k..].find(attr) {
                let start = k + off + attr.len();
                let Some(len) = line[start..].find('"') else {
                    break;
                };
                found.push(line[start..start + len].to_owned());
                k = start + len;
            }
        }
        for d in found {
            if d.is_empty() || d.starts_with("//") || has_scheme(&d) {
                continue;
            }
            out.push((i + 1, d));
        }
    }
    out
}

/// `https:`, `mailto:`처럼 스킴으로 시작하는지.
fn has_scheme(dest: &str) -> bool {
    match dest.split_once(':') {
        Some((s, _)) => {
            !s.is_empty()
                && s.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                && !s.contains('/')
        }
        None => false,
    }
}

/// 코드 표시(`` `…` ``)를 같은 길이의 빈칸으로 바꾼다(그 안의 `](`는 링크가 아니다).
fn strip_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '`' {
            out.push(c);
            continue;
        }
        let mut ticks = 1;
        while chars.peek() == Some(&'`') {
            chars.next();
            ticks += 1;
        }
        let rest: String = chars.clone().collect();
        let close = "`".repeat(ticks);
        match rest.find(&close) {
            Some(pos) => {
                out.push_str(&" ".repeat(ticks * 2 + rest[..pos].chars().count()));
                for _ in 0..rest[..pos].chars().count() + ticks {
                    chars.next();
                }
            }
            None => out.push_str(&close),
        }
    }
    out
}

/// 문서에서 쓸 수 있는 앵커: 제목에서 만든 것과 `id="…"`·`name="…"`.
fn anchors_of(text: &str) -> BTreeSet<String> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = BTreeSet::new();
    let mut fence: Option<&str> = None;
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if let Some(f) = fence {
            if trimmed.starts_with(f) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        for attr in ["id=\"", "name=\""] {
            let mut k = 0;
            while let Some(off) = raw[k..].find(attr) {
                let start = k + off + attr.len();
                let Some(len) = raw[start..].find('"') else {
                    break;
                };
                out.insert(raw[start..start + len].to_owned());
                k = start + len;
            }
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if !(1..=6).contains(&hashes) || raw.len() - trimmed.len() > 3 {
            continue;
        }
        let rest = &trimmed[hashes..];
        if !(rest.is_empty() || rest.starts_with(' ')) {
            continue;
        }
        let title = rest.trim().trim_end_matches('#').trim_end();
        let base = slug(title);
        let count = seen.entry(base.clone()).or_insert(0);
        let anchor = if *count == 0 {
            base.clone()
        } else {
            format!("{base}-{count}")
        };
        *count += 1;
        out.insert(anchor);
    }
    out
}

/// GitHub가 제목에서 만드는 앵커.
fn slug(title: &str) -> String {
    // 보이는 글자만: 링크는 글로, 코드 표시와 강조와 HTML 태그는 뺀다.
    let mut visible = String::new();
    let mut rest = title;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('[')
            && let Some(close) = after.find("](")
            && let Some(end) = after[close..].find(')')
        {
            visible.push_str(&after[..close]);
            rest = &after[close + end + 1..];
            continue;
        }
        if rest.starts_with('<')
            && let Some(end) = rest.find('>')
        {
            rest = &rest[end + 1..];
            continue;
        }
        let c = rest.chars().next().expect("not empty");
        if c != '`' && c != '*' {
            visible.push(c);
        }
        rest = &rest[c.len_utf8()..];
    }
    visible
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_' || *c == ' ')
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_follow_github() {
        assert_eq!(slug("v0.2.1"), "v021");
        assert_eq!(slug("`sbx build`"), "sbx-build");
        assert_eq!(
            slug("Sandbox → sandbox (`--connect`)"),
            "sandbox--sandbox---connect"
        );
        assert_eq!(
            slug("[Use it](usage.md) like **Docker**"),
            "use-it-like-docker"
        );
        assert_eq!(slug("compose.yaml"), "composeyaml");
        assert_eq!(
            slug("M9. 개발 흐름 (0.3, 2026-11-13~)"),
            "m9-개발-흐름-03-2026-11-13"
        );
        let a = anchors_of(
            "# A\n## A\n```\n# B\n```\n<a id=\"x\"></a>\n    # not a heading\n#hashtag\n",
        );
        assert_eq!(
            a.into_iter().collect::<Vec<_>>(),
            ["a", "a-1", "x"].map(String::from)
        );
    }

    #[test]
    fn links_skip_code_and_urls() {
        let text = "See [a](a.md#x) and `[b](b.md)` or [c](https://x.y/(z)).\n\
                    ```\n[d](d.md)\n```\n\
                    [ref]: ref.md\n<img src=\"assets/x.svg\"> <a href=\"#top\">t</a> [e](<e f.md> \"t\")\n";
        let got: Vec<String> = links(text).into_iter().map(|(_, d)| d).collect();
        assert_eq!(got, ["a.md#x", "ref.md", "e f.md", "#top", "assets/x.svg"]);
        assert!(has_scheme("mailto:x@y") && !has_scheme("a/b:c") && !has_scheme("x.md"));
    }

    /// 저장소의 문서에 끊긴 상대 링크나 앵커가 없다.
    #[test]
    fn repository_links_resolve() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives inside the workspace");
        if let Err(problems) = check(root) {
            panic!("broken links:\n{}", problems.join("\n"));
        }
    }
}
