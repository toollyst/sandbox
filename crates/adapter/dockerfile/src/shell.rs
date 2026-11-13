//! 변수 풀기. BuildKit의 셸 낱말 처리(`shell/lex.go`)와 같은 규칙이다.
//!
//! 한 낱말을 읽으며 따옴표를 벗기고, 이스케이프 문자를 처리하고, `$`를 푼다.
//!
//! - `$NAME`, `${NAME}`: 값. 없으면 빈 문자열이다. 이름은 글자, 숫자, `_`다. 숫자로 시작하면 숫자만 이름이고(`$1`),
//!   `$@ $* $# $? $- $$ $!`는 한 글자 이름이다(Dockerfile에는 값이 없으므로 빈 문자열이 된다).
//!   `$` 뒤에 이름이 없으면 `$` 그대로다.
//! - `${NAME:-말}`: 없거나 비었으면 `말`. `${NAME-말}`: 없을 때만 `말`.
//! - `${NAME:+말}`: 있고 비지 않았으면 `말`, 아니면 빈 문자열. `${NAME+말}`: 있으면 `말`.
//! - `${NAME:?말}`, `${NAME?말}`: 없으면(`:`이면 비어도) `NAME: 말`로 오류다.
//! - `말` 안에서도 따옴표, 이스케이프, `$`를 똑같이 푼다(중첩은 [`MAX_NESTING`]까지).
//! - `${NAME#패턴}`, `${NAME%패턴}`, `${NAME/패턴/바꿀 것}`은 아직 받지 않는다. 오류다(BuildKit 1.7부터 있는 기능).
//! - 작은따옴표 안은 그대로다(이스케이프도 없다). 큰따옴표 안에서는 `$`를 풀고, 이스케이프 문자는 `"`, `$`,
//!   이스케이프 문자 앞에서만 이스케이프이고 다른 곳에서는 글자 그대로 남는다.
//! - 따옴표 밖의 이스케이프 문자는 다음 글자를 그대로 넣는다. 맨 끝의 이스케이프 문자는 버린다.
//! - 닫히지 않은 따옴표와 `}`는 오류다.
//!
//! [`word`]는 공백을 그대로 둔 한 문자열을, [`words`]는 따옴표 밖 공백으로 나눈 낱말들을 낸다. 따옴표 밖에서 푼
//! 값의 공백도 나눈다(`EXPOSE $PORTS`).

/// `${...}` 중첩 상한. 넘으면 오류다(스택을 지킨다). Docker에는 상한이 없다.
pub const MAX_NESTING: usize = 64;

/// 변수 값 찾기. 없으면 `None`(빈 값과 다르다).
pub type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;

/// 한 낱말로 푼다(공백은 그대로). 결과가 `limit` 바이트를 넘으면 오류다.
pub fn word(text: &str, escape: char, env: Lookup<'_>, limit: usize) -> Result<String, String> {
    Lex::new(text, escape, env, limit).run().map(|(s, _)| s)
}

/// 낱말들로 나눠 푼다. 결과가 `limit` 바이트를 넘으면 오류다.
pub fn words(
    text: &str,
    escape: char,
    env: Lookup<'_>,
    limit: usize,
) -> Result<Vec<String>, String> {
    Lex::new(text, escape, env, limit).run().map(|(_, w)| w)
}

#[derive(Default)]
struct Words {
    words: Vec<String>,
    word: String,
    in_word: bool,
}

impl Words {
    fn add_char(&mut self, c: char) {
        if c.is_whitespace() {
            if self.in_word && !self.word.is_empty() {
                self.words.push(std::mem::take(&mut self.word));
                self.in_word = false;
            }
        } else {
            self.add_raw_char(c);
        }
    }

    fn add_raw_char(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }

    fn add_string(&mut self, s: &str) {
        for c in s.chars() {
            self.add_char(c);
        }
    }

    fn add_raw_string(&mut self, s: &str) {
        self.word.push_str(s);
        self.in_word = true;
    }

    fn finish(mut self) -> Vec<String> {
        if !self.word.is_empty() {
            self.words.push(self.word);
        }
        self.words
    }
}

struct Lex<'a> {
    chars: Vec<char>,
    i: usize,
    escape: char,
    env: Lookup<'a>,
    depth: usize,
    limit: usize,
}

impl<'a> Lex<'a> {
    fn new(text: &str, escape: char, env: Lookup<'a>, limit: usize) -> Self {
        Self {
            chars: text.chars().collect(),
            i: 0,
            escape,
            env,
            depth: 0,
            limit,
        }
    }

    fn check(&self, len: usize) -> Result<(), String> {
        if len > self.limit {
            return Err(format!(
                "a substituted value is longer than {} bytes",
                self.limit
            ));
        }
        Ok(())
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.i).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.i += 1;
        }
        c
    }

    fn run(&mut self) -> Result<(String, Vec<String>), String> {
        self.stop_on(None)
    }

    /// `stop`(없으면 끝)까지 읽는다. `stop`은 먹는다.
    fn stop_on(&mut self, stop: Option<char>) -> Result<(String, Vec<String>), String> {
        let mut result = String::new();
        let mut words = Words::default();
        while let Some(c) = self.peek() {
            if Some(c) == stop {
                self.i += 1;
                return Ok((result, words.finish()));
            }
            match c {
                '$' => {
                    let v = self.dollar()?;
                    self.check(result.len() + v.len())?;
                    result.push_str(&v);
                    words.add_string(&v);
                }
                '\'' => {
                    let v = self.single_quote()?;
                    result.push_str(&v);
                    words.add_raw_string(&v);
                }
                '"' => {
                    let v = self.double_quote()?;
                    result.push_str(&v);
                    words.add_raw_string(&v);
                }
                _ => {
                    self.i += 1;
                    if c == self.escape {
                        let Some(n) = self.next() else {
                            break;
                        };
                        words.add_raw_char(n);
                        result.push(n);
                    } else {
                        words.add_char(c);
                        result.push(c);
                    }
                }
            }
        }
        match stop {
            Some(s) => Err(format!(
                "unexpected end of statement while looking for matching {s}"
            )),
            None => Ok((result, words.finish())),
        }
    }

    fn single_quote(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.next() {
                None => {
                    return Err(
                        "unexpected end of statement while looking for matching single-quote"
                            .into(),
                    );
                }
                Some('\'') => return Ok(out),
                Some(c) => out.push(c),
            }
        }
    }

    fn double_quote(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(
                    "unexpected end of statement while looking for matching double-quote".into(),
                );
            };
            match c {
                '"' => {
                    self.i += 1;
                    return Ok(out);
                }
                '$' => {
                    let v = self.dollar()?;
                    self.check(out.len() + v.len())?;
                    out.push_str(&v);
                }
                _ => {
                    self.i += 1;
                    if c == self.escape {
                        match self.peek() {
                            None => continue,
                            Some(n) if n == '"' || n == '$' || n == self.escape => {
                                self.i += 1;
                                out.push(n);
                                continue;
                            }
                            Some(_) => {}
                        }
                    }
                    out.push(c);
                }
            }
        }
    }

    fn name(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if name.is_empty() && c.is_ascii_digit() {
                while let Some(d) = self.peek().filter(char::is_ascii_digit) {
                    self.i += 1;
                    name.push(d);
                }
                return name;
            }
            if name.is_empty() && matches!(c, '@' | '*' | '#' | '?' | '-' | '$' | '!') {
                self.i += 1;
                name.push(c);
                return name;
            }
            if !c.is_alphanumeric() && c != '_' {
                break;
            }
            self.i += 1;
            name.push(c);
        }
        name
    }

    fn dollar(&mut self) -> Result<String, String> {
        self.i += 1;
        if self.peek() != Some('{') {
            let name = self.name();
            if name.is_empty() {
                return Ok("$".into());
            }
            return Ok((self.env)(&name).unwrap_or_default());
        }
        self.i += 1;
        match self.peek() {
            None => return Err("syntax error: missing '}'".into()),
            Some('{' | '}' | ':') => return Err("syntax error: bad substitution".into()),
            Some(_) => {}
        }
        let name = self.name();
        let (colon, modifier) = match self.next() {
            None => return Err("syntax error: missing '}'".into()),
            Some('}') => return Ok((self.env)(&name).unwrap_or_default()),
            Some(':') => (true, self.next()),
            Some(c) => (false, Some(c)),
        };
        let shown = |m: Option<char>| {
            let m = m.map(String::from).unwrap_or_default();
            if colon { format!(":{m}") } else { m }
        };
        match modifier {
            Some('-' | '+' | '?') => {}
            Some('#' | '%' | '/') if !colon => {
                return Err(format!(
                    "${{{name}{}...}} pattern substitution is not supported yet",
                    shown(modifier)
                ));
            }
            None => return Err("syntax error: missing '}'".into()),
            m => {
                return Err(format!(
                    "unsupported modifier ({}) in substitution",
                    shown(m)
                ));
            }
        }
        if self.depth >= MAX_NESTING {
            return Err(format!(
                "substitutions are nested more than {MAX_NESTING} deep"
            ));
        }
        self.depth += 1;
        let word = self.stop_on(Some('}'));
        self.depth -= 1;
        let word = match word {
            Ok((w, _)) => w,
            Err(_) if self.peek().is_none() => return Err("syntax error: missing '}'".into()),
            Err(e) => return Err(e),
        };
        let value = (self.env)(&name);
        let empty = |v: &Option<String>| v.as_deref().is_none_or(|v| colon && v.is_empty());
        Ok(match modifier {
            Some('-') => {
                if empty(&value) {
                    word
                } else {
                    value.unwrap_or_default()
                }
            }
            Some('+') => {
                if empty(&value) {
                    String::new()
                } else {
                    word
                }
            }
            _ => {
                if empty(&value) {
                    let message = if !word.is_empty() {
                        word
                    } else if value.is_none() {
                        "is not allowed to be unset".into()
                    } else {
                        "is not allowed to be empty".into()
                    };
                    return Err(format!("{name}: {message}"));
                }
                value.unwrap_or_default()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(name: &str) -> Option<String> {
        match name {
            "A" => Some("a".into()),
            "EMPTY" => Some(String::new()),
            "SP" => Some("x  y".into()),
            "PATH" => Some("/usr/bin".into()),
            "한글" => Some("ok".into()),
            _ => None,
        }
    }

    fn w(s: &str) -> Result<String, String> {
        word(s, '\\', &env, usize::MAX)
    }

    #[test]
    fn plain_and_braced_names() {
        assert_eq!(w("$A").unwrap(), "a");
        assert_eq!(w("${A}b").unwrap(), "ab");
        assert_eq!(w("$Ab").unwrap(), "");
        assert_eq!(w("$A-b").unwrap(), "a-b");
        assert_eq!(w("$NOPE").unwrap(), "");
        assert_eq!(w("$PATH:/x").unwrap(), "/usr/bin:/x");
        assert_eq!(w("a$").unwrap(), "a$");
        assert_eq!(w("$ a").unwrap(), "$ a");
        assert_eq!(w("$1x").unwrap(), "x");
        assert_eq!(w("$@x").unwrap(), "x");
        assert_eq!(w("$한글").unwrap(), "ok");
    }

    #[test]
    fn modifiers() {
        assert_eq!(w("${A:-d}").unwrap(), "a");
        assert_eq!(w("${NOPE:-d}").unwrap(), "d");
        assert_eq!(w("${EMPTY:-d}").unwrap(), "d");
        assert_eq!(w("${EMPTY-d}").unwrap(), "");
        assert_eq!(w("${NOPE-d}").unwrap(), "d");
        assert_eq!(w("${A:+alt}").unwrap(), "alt");
        assert_eq!(w("${EMPTY:+alt}").unwrap(), "");
        assert_eq!(w("${EMPTY+alt}").unwrap(), "alt");
        assert_eq!(w("${NOPE+alt}").unwrap(), "");
        assert_eq!(w("${NOPE:-$A/${NOPE:-x}}").unwrap(), "a/x");
        assert_eq!(w("${NOPE:-'}'}").unwrap(), "}");
        assert_eq!(w("${NOPE:-a\\}b}").unwrap(), "a}b");
        assert_eq!(w("${A:?}").unwrap(), "a");
        assert_eq!(
            w("${NOPE?}").unwrap_err(),
            "NOPE: is not allowed to be unset"
        );
        assert_eq!(
            w("${EMPTY:?}").unwrap_err(),
            "EMPTY: is not allowed to be empty"
        );
        assert_eq!(w("${EMPTY?}").unwrap(), "");
        assert_eq!(w("${NOPE:?set it}").unwrap_err(), "NOPE: set it");
    }

    #[test]
    fn bad_substitutions() {
        for (t, e) in [
            ("${", "syntax error: missing '}'"),
            ("${A", "syntax error: missing '}'"),
            ("${A:-x", "syntax error: missing '}'"),
            ("${}", "syntax error: bad substitution"),
            ("${:-x}", "syntax error: bad substitution"),
            ("${{A}}", "syntax error: bad substitution"),
            ("${A:=x}", "unsupported modifier (:=) in substitution"),
            ("${A x}", "unsupported modifier ( ) in substitution"),
            (
                "${A#x}",
                "${A#...} pattern substitution is not supported yet",
            ),
            (
                "${A%x}",
                "${A%...} pattern substitution is not supported yet",
            ),
            (
                "${A/x/y}",
                "${A/...} pattern substitution is not supported yet",
            ),
            ("${A:#x}", "unsupported modifier (:#) in substitution"),
        ] {
            assert_eq!(w(t).unwrap_err(), e, "{t}");
        }
        let deep = "${N:-".repeat(MAX_NESTING + 1) + &"}".repeat(MAX_NESTING + 1);
        assert!(w(&deep).unwrap_err().contains("nested"));
        let ok = "${N:-".repeat(MAX_NESTING) + "x" + &"}".repeat(MAX_NESTING);
        assert_eq!(w(&ok).unwrap(), "x");
    }

    #[test]
    fn quotes_and_escapes() {
        assert_eq!(w("'$A'").unwrap(), "$A");
        assert_eq!(w("\"$A b\"").unwrap(), "a b");
        assert_eq!(w("\\$A").unwrap(), "$A");
        assert_eq!(w("a\\ b").unwrap(), "a b");
        assert_eq!(w("a\\").unwrap(), "a");
        assert_eq!(w("\"a\\\"b\\$c\\\\d\\e\"").unwrap(), "a\"b$c\\d\\e");
        assert_eq!(w("'a\\'").unwrap(), "a\\");
        assert_eq!(w("x\"\"y").unwrap(), "xy");
        assert!(w("'a").unwrap_err().contains("single-quote"));
        assert!(w("\"a").unwrap_err().contains("double-quote"));
        // 백틱이 이스케이프 문자면 `\`는 글자다.
        assert_eq!(word("C:\\a`$A", '`', &env, usize::MAX).unwrap(), "C:\\a$A");
    }

    #[test]
    fn word_lists_split_outside_quotes() {
        let ws = |s: &str| words(s, '\\', &env, usize::MAX).unwrap();
        assert_eq!(ws("80  443/udp"), ["80", "443/udp"]);
        assert_eq!(ws("$SP"), ["x", "y"]);
        assert_eq!(ws("\"$SP\""), ["x  y"]);
        assert_eq!(ws("a\\ b c"), ["a b", "c"]);
        assert_eq!(ws("'' x"), ["x"]);
        assert!(ws("").is_empty());
        assert_eq!(w(" a  b ").unwrap(), " a  b ");
    }

    #[test]
    fn long_results_stop_early() {
        let e = word("$PATH$PATH", '\\', &env, 10).unwrap_err();
        assert!(e.contains("longer than 10 bytes"));
        assert_eq!(word("$PATH", '\\', &env, 8).unwrap(), "/usr/bin");
    }
}
