//! 줄 나누기: 파서 지시어, 주석, 줄 이음. BuildKit의 `parser.Parse`와 같은 규칙이다.
//!
//! - 줄은 `\n`으로 나눈다. 줄 끝의 `\r`은 버린다(CRLF). 첫 줄 앞의 UTF-8 BOM은 버린다.
//! - **파서 지시어**는 파일 맨 위에서만 읽는다. `# 이름=값` 꼴(공백 허용, 이름은 대소문자 무관)이고, 아는 이름은
//!   `escape`, `syntax`, `check`다. 지시어가 아닌 줄(빈 줄, 다른 주석, 모르는 지시어, 명령)을 만나면 더 읽지 않는다.
//!   같은 지시어가 두 번이면 오류다.
//! - `escape`: 이스케이프 문자. `\`(기본)나 `` ` ``만 받는다. 줄 이음과 변수 풀기에 쓴다.
//! - **주석**: 앞 공백을 빼고 `#`으로 시작하는 줄. 명령 중간(`RUN echo # x`)의 `#`은 주석이 아니다.
//! - **줄 이음**: 줄이 이스케이프 문자(뒤에 공백과 탭만 올 수 있다)로 끝나면 다음 줄을 잇는다. 이스케이프 문자와
//!   줄바꿈은 빼고 그대로 붙인다(공백을 넣지 않는다). 이어지는 줄 사이의 주석 줄과 빈 줄은 건너뛴다. 빈 줄은 Docker처럼
//!   경고한다.
//! - 명령의 줄 번호는 시작하는 줄이다.

use sbx_model::build::PlanError;

/// 파서 지시어.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directives {
    /// 이스케이프 문자.
    pub escape: char,
    /// `# syntax=`(줄, 값).
    pub syntax: Option<(usize, String)>,
    /// `# check=`(줄, 값).
    pub check: Option<(usize, String)>,
}

/// 이은 명령 줄 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logical {
    /// 시작 줄(1부터).
    pub line: usize,
    /// 이은 내용. 앞 공백은 뺐다.
    pub text: String,
    /// 이어지는 줄 가운데 빈 줄이 있었다.
    pub empty_continuation: bool,
}

/// 지시어와 명령 줄들로 나눈다.
pub fn split(text: &str) -> Result<(Directives, Vec<Logical>), PlanError> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let mut raw: Vec<&str> = text.split('\n').collect();
    // 끝의 줄바꿈 뒤에는 줄이 없다(Go의 bufio.ScanLines와 같다).
    if raw.last() == Some(&"") {
        raw.pop();
    }

    let mut d = Directives {
        escape: '\\',
        syntax: None,
        check: None,
    };
    let mut directives_done = false;
    let mut seen_escape = false;
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let lineno = i + 1;
        let t = trim_newline(raw[i]).trim_start();
        i += 1;
        if !directives_done {
            match directive(t) {
                Some((name, value)) => {
                    let seen = match name.as_str() {
                        "escape" => std::mem::replace(&mut seen_escape, true),
                        "syntax" => d.syntax.is_some(),
                        _ => d.check.is_some(),
                    };
                    if seen {
                        return Err(PlanError::at(
                            lineno,
                            format!("only one {name} parser directive can be used"),
                        ));
                    }
                    match name.as_str() {
                        "escape" => {
                            d.escape = match value.as_str() {
                                "\\" => '\\',
                                "`" => '`',
                                _ => {
                                    return Err(PlanError::at(
                                        lineno,
                                        format!(
                                            "invalid escape token '{value}' does not match ` or \\"
                                        ),
                                    ));
                                }
                            }
                        }
                        "syntax" => d.syntax = Some((lineno, value)),
                        _ => d.check = Some((lineno, value)),
                    }
                }
                None => directives_done = true,
            }
        }
        if is_comment(t) {
            continue;
        }
        let (first, mut end) = trim_continuation(t, d.escape);
        if end && first.is_empty() {
            continue;
        }
        let mut line = first.to_owned();
        let mut empty_continuation = false;
        while !end && i < raw.len() {
            let t = trim_newline(raw[i]);
            i += 1;
            if is_comment(t.trim_start()) {
                continue;
            }
            if t.trim_start().is_empty() {
                empty_continuation = true;
                continue;
            }
            let (more, e) = trim_continuation(t, d.escape);
            line.push_str(more);
            end = e;
        }
        out.push(Logical {
            line: lineno,
            text: line,
            empty_continuation,
        });
    }
    Ok((d, out))
}

fn trim_newline(s: &str) -> &str {
    s.trim_end_matches(['\r', '\n'])
}

fn is_comment(s: &str) -> bool {
    s.starts_with('#')
}

/// 줄 이음 문자로 끝나면 그것과 뒤 공백·탭을 뺀 것과 `false`, 아니면 그대로와 `true`.
fn trim_continuation(s: &str, escape: char) -> (&str, bool) {
    let t = s.trim_end_matches([' ', '\t']);
    match t.strip_suffix(escape) {
        Some(rest) => (rest, false),
        None => (s, true),
    }
}

/// `# 이름=값`(아는 이름만). 이름은 소문자로.
fn directive(s: &str) -> Option<(String, String)> {
    let rest = s.strip_prefix('#')?;
    let rest = rest.trim_start_matches(is_re_space);
    let name_len = rest
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())))
        .map_or(rest.len(), |(i, _)| i);
    if name_len == 0 {
        return None;
    }
    let name = rest[..name_len].to_ascii_lowercase();
    let rest = rest[name_len..]
        .trim_start_matches(is_re_space)
        .strip_prefix('=')?;
    let value = rest.trim_matches(is_re_space);
    if value.is_empty() || !matches!(name.as_str(), "escape" | "syntax" | "check") {
        return None;
    }
    Some((name, value.to_owned()))
}

/// Go 정규식의 `\s`(`[\t\n\f\r ]`).
fn is_re_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<(usize, String)> {
        split(text)
            .unwrap()
            .1
            .into_iter()
            .map(|l| (l.line, l.text))
            .collect()
    }

    #[test]
    fn comments_blank_lines_and_crlf() {
        let t = "\u{FEFF}# c\n\nFROM a\r\n  # indented comment\n\tRUN echo # not a comment\r\n";
        assert_eq!(
            lines(t),
            [(3, "FROM a".into()), (5, "RUN echo # not a comment".into())]
        );
    }

    #[test]
    fn continuations_join_without_spaces_and_skip_comments() {
        let t = "RUN a \\\n  && b \\  \n# c\n\n    && c\nFROM x\\";
        let (_, ls) = split(t).unwrap();
        assert_eq!(ls[0].line, 1);
        assert_eq!(ls[0].text, "RUN a   && b     && c");
        assert!(ls[0].empty_continuation);
        // 마지막 줄의 줄 이음은 그냥 끝난다.
        assert_eq!((ls[1].line, ls[1].text.as_str()), (6, "FROM x"));
        assert!(!ls[1].empty_continuation);
    }

    #[test]
    fn escape_directive_changes_continuation() {
        let t = "# escape=`\nFROM a\nRUN dir c:\\ `\n  x";
        let (d, ls) = split(t).unwrap();
        assert_eq!(d.escape, '`');
        assert_eq!(ls[1].text, "RUN dir c:\\   x");
    }

    #[test]
    fn directives_only_at_the_top() {
        let (d, _) = split("#  SYNTAX = docker/dockerfile:1 \n# escape=\\\nFROM a").unwrap();
        assert_eq!(d.syntax, Some((1, "docker/dockerfile:1".into())));
        let (d, _) = split("# hello\n# syntax=x\nFROM a").unwrap();
        assert_eq!(d.syntax, None);
        let (d, _) = split("\n# escape=`\nFROM a").unwrap();
        assert_eq!(d.escape, '\\');
        let (d, _) = split("# unknown=1\n# escape=`\nFROM a").unwrap();
        assert_eq!(d.escape, '\\');
        let (d, _) = split("  # check=skip=all\nFROM a").unwrap();
        assert_eq!(d.check, Some((1, "skip=all".into())));
    }

    #[test]
    fn bad_directives() {
        let e = split("# escape=x\nFROM a").unwrap_err();
        assert_eq!(e.line, Some(1));
        assert!(e.message.contains("invalid escape token"));
        let e = split("# syntax=a\n# syntax=b\nFROM a").unwrap_err();
        assert_eq!(
            e,
            PlanError::at(2, "only one syntax parser directive can be used")
        );
    }
}
