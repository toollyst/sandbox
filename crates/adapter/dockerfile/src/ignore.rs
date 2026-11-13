//! `.dockerignore`: 컨텍스트에서 뺄 경로. Docker(`moby/patternmatcher`의 `ignorefile.ReadAll`과
//! `MatchesOrParentMatches`)와 같은 규칙이다. 파일을 읽거나 디렉터리를 돌지 않는다. 경로 문자열만 본다.
//!
//! 파일 읽기:
//!
//! - 줄마다 패턴 하나다. 첫 줄의 UTF-8 BOM은 버린다. **맨 앞 글자가** `#`이면 주석이다(앞에 공백이 있으면 주석이
//!   아니다). 앞뒤 공백을 빼고 빈 줄은 건너뛴다.
//! - `!`로 시작하면 예외(다시 넣기)다. `!` 뒤의 공백도 뺀다. `!`만 있으면 오류다.
//! - 패턴은 경로처럼 정리한다(`./a` → `a`, `a/` → `a`, `a//b` → `a/b`, `a/../b` → `b`). 맨 앞 `/`는 뺀다
//!   (컨텍스트 기준이다).
//!
//! 맞추기 (경로는 컨텍스트 기준 상대 경로, `/`로 나눈다):
//!
//! - `*`는 `/`가 아닌 글자 여럿, `?`는 `/`가 아닌 글자 하나, `[...]`는 글자 묶음(`[^...]`은 반대, `a-z` 범위,
//!   `\`로 이스케이프)이다. 묶음은 Go `filepath.Match`의 규칙으로 검사한다: 비었거나, 닫히지 않았거나, `-`·`]`로
//!   시작하는 범위, 거꾸로 된 범위(`z-a`)는 오류다.
//! - `**`는 디렉터리 여럿(0개 포함)이다: `**/a`는 `a`, `x/a`, `x/y/a`에 맞는다. 패턴 끝의 `**`는 무엇이든이다.
//!   패턴 맨 앞의 `**` 뒤에 글자만 오면 Docker의 빠른 길과 같이 끝 맞추기다(`**.go`는 `a.go`, `x/b.go`에 맞는다).
//! - `\`는 다음 글자를 그대로 쓴다. 패턴 끝의 `\`는 오류다.
//! - 패턴은 경로 전체에 맞아야 한다. 경로가 맞지 않아도 그 위 디렉터리(`a/b/c`면 `a`, `a/b`)가 맞으면 맞는 것이다.
//!   그래서 `node_modules`는 그 아래 모든 것을 뺀다.
//! - 패턴을 차례로 보고 마지막으로 맞은 것이 이긴다: 빼는 패턴이 맞으면 빠지고, 예외 패턴이 맞으면 다시 들어간다.
//!
//! Docker와 다른 점: Docker는 패턴을 정규식으로 바꾸므로 묶음 안의 `*`(`[*]`)나 POSIX 묶음(`[[:alpha:]]`), 묶음
//! 밖의 `^`가 정규식 뜻을 갖는다. 여기서는 `filepath.Match`의 뜻(글자 그대로)이다.

use sbx_model::build::PlanError;

/// 읽은 `.dockerignore`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerIgnore {
    patterns: Vec<Pattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Pattern {
    exclusion: bool,
    /// 정리한 패턴(빠른 길의 문자열 비교에 쓴다).
    text: String,
    how: Match,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Match {
    /// 글자 그대로 같다.
    Exact,
    /// 끝의 `**`를 뺀 것으로 시작한다.
    Prefix,
    /// 앞의 `**`를 뺀 것으로 끝난다(`**/a`는 `a`에도 맞는다).
    Suffix,
    /// 일반 패턴.
    Glob(Vec<Tok>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Lit(char),
    /// `*`: `/`가 아닌 글자 여럿.
    Star,
    /// `?`: `/`가 아닌 글자 하나.
    One,
    /// `[...]`: 글자 하나.
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
    /// `**/`, 가운데의 `**`: 비었거나 `/`로 끝나는 무엇이든.
    Dirs,
    /// 끝의 `**`: 무엇이든.
    Any,
}

impl DockerIgnore {
    /// `.dockerignore` 내용을 읽는다. 잘못된 패턴은 그 줄의 오류다.
    pub fn parse(text: &str) -> Result<Self, PlanError> {
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let mut patterns = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let line = i + 1;
            if raw.starts_with('#') {
                continue;
            }
            let mut p = raw.trim();
            if p.is_empty() {
                continue;
            }
            let exclusion = p.starts_with('!');
            if exclusion {
                p = p[1..].trim();
            }
            if p.is_empty() {
                return Err(PlanError::at(line, "illegal exclusion pattern: \"!\""));
            }
            let mut p = clean(p);
            if p.len() > 1 && p.starts_with('/') {
                p.remove(0);
            }
            validate(&p).map_err(|e| PlanError::at(line, format!("{raw:?}: {e}")))?;
            let how = compile(&p).map_err(|e| PlanError::at(line, format!("{raw:?}: {e}")))?;
            patterns.push(Pattern {
                exclusion,
                text: p,
                how,
            });
        }
        Ok(Self { patterns })
    }

    /// 예외 패턴(`!`)이 있는지. 없으면 빠진 디렉터리 아래를 볼 필요가 없다.
    pub fn has_exceptions(&self) -> bool {
        self.patterns.iter().any(|p| p.exclusion)
    }

    /// 컨텍스트 기준 경로 `path`(`/`로 나눈다)가 빠지는지.
    pub fn is_excluded(&self, path: &str) -> bool {
        let path = clean(path.trim_start_matches('/'));
        let parent = match path.rfind('/') {
            Some(i) => &path[..i],
            None => ".",
        };
        let parents: Vec<&str> = if parent == "." {
            Vec::new()
        } else {
            parent
                .match_indices('/')
                .map(|(i, _)| &parent[..i])
                .chain(std::iter::once(parent))
                .collect()
        };
        let mut matched = false;
        for p in &self.patterns {
            if p.exclusion != matched {
                continue;
            }
            if p.matches(&path) || parents.iter().any(|d| p.matches(d)) {
                matched = !p.exclusion;
            }
        }
        matched
    }
}

impl Pattern {
    fn matches(&self, path: &str) -> bool {
        match &self.how {
            Match::Exact => path == self.text,
            Match::Prefix => path.starts_with(&self.text[..self.text.len() - 2]),
            Match::Suffix => {
                let suffix = &self.text[2..];
                path.ends_with(suffix) || suffix.strip_prefix('/') == Some(path)
            }
            Match::Glob(toks) => glob(toks, &path.chars().collect::<Vec<_>>()),
        }
    }
}

/// Go `filepath.Clean`(`/` 경로).
fn clean(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                _ if rooted => {}
                _ => parts.push(".."),
            },
            s => parts.push(s),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// Go `filepath.Match`가 패턴을 거부하는지(`ErrBadPattern`).
fn validate(p: &str) -> Result<(), String> {
    let bad = || Err("syntax error in pattern".to_owned());
    let c: Vec<char> = p.chars().collect();
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                if i + 1 == c.len() {
                    return bad();
                }
                i += 2;
            }
            '[' => {
                i += 1;
                if c.get(i) == Some(&'^') {
                    i += 1;
                }
                let mut n = 0;
                loop {
                    if c.get(i) == Some(&']') && n > 0 {
                        i += 1;
                        break;
                    }
                    let Some((lo, next)) = esc(&c, i) else {
                        return bad();
                    };
                    i = next;
                    let mut hi = lo;
                    if c.get(i) == Some(&'-') {
                        let Some((h, next)) = esc(&c, i + 1) else {
                            return bad();
                        };
                        hi = h;
                        i = next;
                    }
                    if lo > hi {
                        return Err(format!("invalid character class range {lo}-{hi}"));
                    }
                    n += 1;
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// 묶음 안의 글자 하나(`\` 이스케이프 포함)와 다음 위치. 뒤에 아무것도 없으면 Go처럼 잘못된 패턴이다.
fn esc(c: &[char], mut i: usize) -> Option<(char, usize)> {
    let first = *c.get(i)?;
    if first == '-' || first == ']' {
        return None;
    }
    if first == '\\' {
        i += 1;
    }
    let ch = *c.get(i)?;
    (i + 1 < c.len()).then_some((ch, i + 1))
}

/// 패턴을 맞추는 방법으로 바꾼다. Docker의 빠른 길(같다, 앞, 끝)을 그대로 고른다.
fn compile(p: &str) -> Result<Match, String> {
    #[derive(PartialEq)]
    enum Kind {
        Exact,
        Prefix,
        Suffix,
        Glob,
    }
    let c: Vec<char> = p.chars().collect();
    let mut toks = Vec::new();
    let mut kind = Kind::Exact;
    let mut i = 0;
    let mut first = true;
    while i < c.len() {
        let ch = c[i];
        i += 1;
        match ch {
            '*' if c.get(i) == Some(&'*') => {
                i += 1;
                if c.get(i) == Some(&'/') {
                    i += 1;
                }
                if i == c.len() {
                    if kind == Kind::Exact {
                        kind = Kind::Prefix;
                    } else {
                        kind = Kind::Glob;
                    }
                    toks.push(Tok::Any);
                } else {
                    toks.push(Tok::Dirs);
                    kind = Kind::Glob;
                }
                if first {
                    kind = Kind::Suffix;
                }
            }
            '*' => {
                toks.push(Tok::Star);
                kind = Kind::Glob;
            }
            '?' => {
                toks.push(Tok::One);
                kind = Kind::Glob;
            }
            '\\' => {
                // validate가 끝의 `\`를 이미 거부했다.
                toks.push(Tok::Lit(c[i]));
                i += 1;
                kind = Kind::Glob;
            }
            '[' => {
                let mut negated = false;
                if c.get(i) == Some(&'^') {
                    negated = true;
                    i += 1;
                }
                let mut ranges = Vec::new();
                loop {
                    if c[i] == ']' && !ranges.is_empty() {
                        i += 1;
                        break;
                    }
                    let (lo, next) = esc(&c, i).ok_or("syntax error in pattern")?;
                    i = next;
                    let mut hi = lo;
                    if c[i] == '-' {
                        let (h, next) = esc(&c, i + 1).ok_or("syntax error in pattern")?;
                        hi = h;
                        i = next;
                    }
                    ranges.push((lo, hi));
                }
                toks.push(Tok::Class { negated, ranges });
                kind = Kind::Glob;
            }
            ch => toks.push(Tok::Lit(ch)),
        }
        first = false;
    }
    Ok(match kind {
        Kind::Exact => Match::Exact,
        Kind::Prefix => Match::Prefix,
        Kind::Suffix => Match::Suffix,
        Kind::Glob => Match::Glob(toks),
    })
}

/// 토큰들이 `s` 전체에 맞는지. 위치 쌍마다 한 번만 본다(지수 시간이 들지 않는다).
fn glob(toks: &[Tok], s: &[char]) -> bool {
    // ok[t][i]: toks[t..]가 s[i..]에 맞는다. 뒤에서부터 채운다.
    let n = s.len();
    let mut next = vec![false; n + 1];
    next[n] = true;
    for tok in toks.iter().rev() {
        let mut cur = vec![false; n + 1];
        // Dirs: i 뒤(포함)에 `/`인 s[j]가 있고 next[j + 1]인지.
        let mut dirs = false;
        for i in (0..=n).rev() {
            if i < n && s[i] == '/' && next[i + 1] {
                dirs = true;
            }
            cur[i] = match tok {
                Tok::Lit(c) => i < n && s[i] == *c && next[i + 1],
                Tok::One => i < n && s[i] != '/' && next[i + 1],
                Tok::Class { negated, ranges } => {
                    i < n && {
                        let inside = ranges.iter().any(|&(lo, hi)| lo <= s[i] && s[i] <= hi);
                        inside != *negated && next[i + 1]
                    }
                }
                // 0개이거나, 글자 하나(`/` 아님)를 먹고 다시 Star.
                Tok::Star => next[i] || (i < n && s[i] != '/' && cur[i + 1]),
                Tok::Any => next[i] || (i < n && cur[i + 1]),
                // 비었거나, `/`로 끝나는 어떤 s[i..j].
                Tok::Dirs => next[i] || dirs,
            };
        }
        next = cur;
    }
    next[0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ig(text: &str) -> DockerIgnore {
        DockerIgnore::parse(text).unwrap()
    }

    #[test]
    fn plain_names_exclude_everything_below() {
        let i = ig("node_modules\n.git\n/dist/\n./build\n");
        for p in [
            "node_modules",
            "node_modules/x/y",
            ".git/HEAD",
            "dist/a.js",
            "build",
        ] {
            assert!(i.is_excluded(p), "{p}");
        }
        for p in ["src/node_modules", "gitx", "a.git", "src/dist", "distro"] {
            assert!(!i.is_excluded(p), "{p}");
        }
        assert!(!i.has_exceptions());
    }

    #[test]
    fn stars_and_double_stars() {
        let i = ig("*.md\n**/*.log\ntemp?\nsrc/**/test\ndocs/**\n");
        assert!(i.is_excluded("README.md"));
        assert!(!i.is_excluded("docs2/README.md"), "* does not cross /");
        assert!(i.is_excluded("a.log"));
        assert!(i.is_excluded("x/y/a.log"));
        assert!(i.is_excluded("temp1"));
        assert!(!i.is_excluded("temp12"));
        assert!(i.is_excluded("src/test"));
        assert!(i.is_excluded("src/a/b/test/x"));
        assert!(!i.is_excluded("src/atest"));
        assert!(i.is_excluded("docs/a/b"));
        // 끝의 `**`는 Docker의 앞 맞추기다(`docs/`로 시작).
        assert!(!i.is_excluded("docs"));
    }

    #[test]
    fn leading_double_star_is_a_suffix_match() {
        let i = ig("**.go\n");
        assert!(i.is_excluded("a.go"));
        assert!(i.is_excluded("x/b.go"));
        let i = ig("**/foo\n");
        assert!(i.is_excluded("foo"));
        assert!(i.is_excluded("a/b/foo"));
        assert!(!i.is_excluded("afoo"));
        let i = ig("**\n");
        assert!(i.is_excluded("anything/at/all"));
    }

    #[test]
    fn exceptions_put_files_back_and_the_last_match_wins() {
        let i = ig("*.md\n!README.md\n");
        assert!(i.is_excluded("CHANGES.md"));
        assert!(!i.is_excluded("README.md"));
        assert!(i.has_exceptions());
        let i = ig("*.md\n!README*.md\nREADME-secret.md\n");
        assert!(!i.is_excluded("README-public.md"));
        assert!(i.is_excluded("README-secret.md"));
        let i = ig("a\n!a/b\n");
        assert!(i.is_excluded("a/c"));
        assert!(!i.is_excluded("a/b"));
        assert!(!i.is_excluded("a/b/c"));
        // 예외는 이미 빠진 것에만 쓴다.
        let i = ig("!x\n");
        assert!(!i.is_excluded("x"));
    }

    #[test]
    fn classes_and_escapes() {
        let i = ig("[abc].txt\n[^x]y\nfile[0-9]\n\\*star\n");
        assert!(i.is_excluded("a.txt"));
        assert!(!i.is_excluded("d.txt"));
        assert!(i.is_excluded("zy"));
        assert!(!i.is_excluded("xy"));
        assert!(i.is_excluded("file7"));
        assert!(!i.is_excluded("filex"));
        assert!(i.is_excluded("*star"));
        assert!(!i.is_excluded("xstar"));
    }

    #[test]
    fn comments_whitespace_and_cleaning() {
        let i = ig("\u{FEFF}# comment\n  # not a comment\n  spaced  \n!  keep\n\na/../b\n");
        assert!(i.is_excluded("# not a comment"));
        assert!(i.is_excluded("spaced"));
        assert!(i.is_excluded("b"));
        assert!(!i.is_excluded("a"));
        // 넘겨받은 경로도 정리한다.
        assert!(i.is_excluded("./spaced/x"));
        assert!(i.is_excluded("/spaced"));
    }

    #[test]
    fn bad_patterns_fail_with_their_line() {
        for (text, line) in [
            ("ok\n!\n", 2),
            ("! \n", 1),
            ("[\n", 1),
            ("a[]b\n", 1),
            ("x\n[z-a]\n", 2),
            ("[-a]\n", 1),
            ("[a-]\n", 1),
            ("a\\\n", 1),
            ("[a\n", 1),
        ] {
            let e = DockerIgnore::parse(text).unwrap_err();
            assert_eq!(e.line, Some(line), "{text:?}: {e}");
        }
    }

    #[test]
    fn go_clean() {
        for (a, b) in [
            ("", "."),
            ("./a/", "a"),
            ("a//b/./c/..", "a/b"),
            ("../a", "../a"),
            ("/../a", "/a"),
            ("a/../..", ".."),
            ("/", "/"),
        ] {
            assert_eq!(clean(a), b, "{a}");
        }
    }
}
