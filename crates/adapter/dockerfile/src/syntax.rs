//! 명령 한 줄 안의 문법: 명령 이름, 플래그(`--from=x`), 낱말 나누기, `이름=값` 목록, JSON 또는 공백 목록.
//! BuildKit의 `parser`(`splitCommand`, `extractBuilderFlags`, `parseWords`, `parseNameVal`)와 같은 규칙이다.
//!
//! 여기서는 변수를 풀지 않는다. 따옴표와 이스케이프도 그대로 남긴다(풀기는 [`crate::shell`]이 한다).
//! 예외는 플래그다: Docker처럼 플래그 값의 따옴표는 여기서 벗기고, 플래그 안의 이스케이프 문자는 언제나 `\`다.

use crate::json::{self, Json};

/// 명령 이름(소문자)과 나머지. 나머지는 앞뒤 공백을 뺐다.
pub fn split_command(line: &str) -> (String, &str) {
    let line = line.trim();
    match line.find(char::is_whitespace) {
        Some(i) => (line[..i].to_ascii_lowercase(), line[i..].trim_start()),
        None => (line.to_ascii_lowercase(), ""),
    }
}

/// 앞의 `--` 낱말들(플래그)을 떼어 낸다. 플래그(`--`를 뺀 `이름=값`)들과 나머지를 돌려준다.
///
/// 낱말 `--` 하나는 플래그의 끝이다.
pub fn extract_flags(line: &str) -> (Vec<String>, &str) {
    let (words, rest) = flag_words(line);
    let words = words
        .into_iter()
        .map(|w| w.strip_prefix("--").map(str::to_owned).unwrap_or(w))
        .collect();
    (words, rest)
}

/// BuildKit `extractBuilderFlags`: `--`로 시작하는 낱말들(따옴표를 벗긴다)과 나머지.
fn flag_words(line: &str) -> (Vec<String>, &str) {
    #[derive(PartialEq)]
    enum Phase {
        Spaces,
        Word,
        Quote(char),
    }
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut words = Vec::new();
    let mut phase = Phase::Spaces;
    let mut word = String::new();
    let mut blank_ok = false;
    let mut k = 0;
    while k <= chars.len() {
        let at_end = k == chars.len();
        let (pos, ch) = chars.get(k).copied().unwrap_or((line.len(), '\0'));
        if phase == Phase::Spaces {
            if at_end {
                break;
            }
            if ch.is_whitespace() {
                k += 1;
                continue;
            }
            if !line[pos..].starts_with("--") {
                return (words, &line[pos..]);
            }
            phase = Phase::Word;
        }
        if at_end {
            if word != "--" && (blank_ok || !word.is_empty()) {
                words.push(word);
            }
            break;
        }
        match phase {
            Phase::Word => {
                if ch.is_whitespace() {
                    phase = Phase::Spaces;
                    if word == "--" {
                        return (words, &line[pos..]);
                    }
                    if blank_ok || !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    blank_ok = false;
                    k += 1;
                    continue;
                }
                if ch == '\'' || ch == '"' {
                    phase = Phase::Quote(ch);
                    blank_ok = true;
                    k += 1;
                    continue;
                }
                let mut c = ch;
                if c == '\\' {
                    if k + 1 == chars.len() {
                        k += 1;
                        continue;
                    }
                    k += 1;
                    c = chars[k].1;
                }
                word.push(c);
            }
            Phase::Quote(q) => {
                if ch == q {
                    phase = Phase::Word;
                    k += 1;
                    continue;
                }
                let mut c = ch;
                if c == '\\' {
                    if k + 1 == chars.len() {
                        phase = Phase::Word;
                        k += 1;
                        continue;
                    }
                    k += 1;
                    c = chars[k].1;
                }
                word.push(c);
            }
            Phase::Spaces => unreachable!(),
        }
        k += 1;
    }
    (words, "")
}

/// 공백으로 나누되 따옴표 안의 공백은 나누지 않는다. 따옴표와 이스케이프 문자는 남긴다(BuildKit `parseWords`).
pub fn parse_words(rest: &str, escape: char) -> Vec<String> {
    #[derive(PartialEq)]
    enum Phase {
        Spaces,
        Word,
        Quote(char),
    }
    let chars: Vec<char> = rest.chars().collect();
    let mut words = Vec::new();
    let mut phase = Phase::Spaces;
    let mut word = String::new();
    let mut blank_ok = false;
    let mut k = 0;
    while k <= chars.len() {
        let at_end = k == chars.len();
        let ch = chars.get(k).copied().unwrap_or('\0');
        if phase == Phase::Spaces {
            if at_end {
                break;
            }
            if ch.is_whitespace() {
                k += 1;
                continue;
            }
            phase = Phase::Word;
        }
        if at_end {
            if blank_ok || !word.is_empty() {
                words.push(word);
            }
            break;
        }
        match phase {
            Phase::Word => {
                if ch.is_whitespace() {
                    phase = Phase::Spaces;
                    if blank_ok || !word.is_empty() {
                        words.push(std::mem::take(&mut word));
                    }
                    blank_ok = false;
                    k += 1;
                    continue;
                }
                if ch == '\'' || ch == '"' {
                    phase = Phase::Quote(ch);
                    blank_ok = true;
                } else if ch == escape {
                    if k + 1 == chars.len() {
                        k += 1;
                        continue;
                    }
                    // 따옴표 밖의 이스케이프는 다음 글자와 함께 남긴다. 다음 글자가 따옴표여도 따옴표를 열지 않는다.
                    word.push(ch);
                    word.push(chars[k + 1]);
                    k += 2;
                    continue;
                }
                word.push(ch);
            }
            Phase::Quote(q) => {
                if ch == q {
                    phase = Phase::Word;
                }
                if ch == escape && q != '\'' {
                    if k + 1 == chars.len() {
                        phase = Phase::Word;
                        k += 1;
                        continue;
                    }
                    word.push(ch);
                    k += 1;
                    word.push(chars[k]);
                    k += 1;
                    continue;
                }
                word.push(ch);
            }
            Phase::Spaces => unreachable!(),
        }
        k += 1;
    }
    words
}

/// `이름=값` 목록(`ENV`, `LABEL`). 첫 낱말에 `=`가 없으면 옛 형식(`ENV 이름 값`)이다: 첫 공백까지가 이름,
/// 나머지 전부가 값이다. 값과 이름은 풀기 전 원문이다.
pub fn parse_name_val(
    rest: &str,
    key: &str,
    escape: char,
) -> Result<Vec<(String, String)>, String> {
    let words = parse_words(rest, escape);
    let Some(first) = words.first() else {
        return Err(format!("{key} requires at least one argument"));
    };
    if !first.contains('=') {
        let Some(i) = rest.find(char::is_whitespace) else {
            return Err(format!("{key} must have two arguments"));
        };
        let value = rest[i..].trim_start();
        if value.is_empty() {
            return Err(format!("{key} must have two arguments"));
        }
        return Ok(vec![(rest[..i].to_owned(), value.to_owned())]);
    }
    let mut out = Vec::with_capacity(words.len());
    for w in words {
        let Some((k, v)) = w.split_once('=') else {
            return Err(format!(
                "Syntax error - can't find = in {w:?}. Must be of the form: name=value"
            ));
        };
        if k.is_empty() {
            return Err(format!("{key} names can not be blank"));
        }
        out.push((k.to_owned(), v.to_owned()));
    }
    Ok(out)
}

/// 명령의 인자 형식.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    /// JSON 배열(exec 형식).
    Json(Vec<String>),
    /// 그 밖(셸 형식). 원문 그대로.
    Text(String),
}

/// JSON 배열이면 그것, 아니면 원문(`RUN`, `CMD`, `ENTRYPOINT`, `SHELL`).
pub fn maybe_json(rest: &str) -> Result<Form, String> {
    if rest.starts_with('[') {
        match json::parse_array(rest) {
            Json::Strings(v) => return Ok(Form::Json(v)),
            Json::NotStrings => {
                return Err("Dockerfile JSON arrays must contain only strings".into());
            }
            Json::Other => {}
        }
    }
    Ok(Form::Text(rest.to_owned()))
}

/// JSON 배열이면 그것, 아니면 공백으로 나눈 것(`COPY`, `ADD`, `VOLUME`). 두 번째 값은 JSON이었는지다.
pub fn maybe_json_list(rest: &str) -> Result<(Vec<String>, bool), String> {
    match maybe_json(rest)? {
        Form::Json(v) => Ok((v, true)),
        Form::Text(t) => Ok((fields(&t), false)),
    }
}

/// 공백으로 나눈다(따옴표를 보지 않는다).
pub fn fields(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

/// 플래그 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagKind {
    /// 값이 있어야 한다(`--from=x`).
    Value,
    /// 아직 받지 않는 Docker 플래그. 쓰면 이 문장으로 오류다.
    Unsupported(&'static str),
}

/// 떼어 낸 플래그를 명령의 플래그 표로 읽는다. 받는 플래그의 (이름, 값) 목록을 돌려준다.
pub fn parse_flags(
    flags: &[String],
    allowed: &[(&str, FlagKind)],
) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    for f in flags {
        let (name, value) = match f.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (f.as_str(), None),
        };
        match allowed.iter().find(|(n, _)| *n == name) {
            None => return Err(format!("unknown flag: {name}")),
            Some((_, FlagKind::Unsupported(msg))) => return Err((*msg).to_owned()),
            Some((_, FlagKind::Value)) => {
                let Some(value) = value else {
                    return Err(format!("missing a value on flag: {name}"));
                };
                if out.iter().any(|(n, _)| n == name) {
                    return Err(format!("duplicate flag specified: {name}"));
                }
                out.push((name.to_owned(), value.to_owned()));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_split_on_the_first_space() {
        assert_eq!(split_command("  RUN  echo  a "), ("run".into(), "echo  a"));
        assert_eq!(split_command("FROM"), ("from".into(), ""));
        assert_eq!(split_command("Copy\ta b"), ("copy".into(), "a b"));
    }

    #[test]
    fn flags_come_first() {
        let (f, rest) = extract_flags("--from=build --chown=\"a b\" src dst");
        assert_eq!(f, ["from=build", "chown=a b"]);
        assert_eq!(rest, "src dst");
        let (f, rest) = extract_flags("--link -- --not-a-flag x");
        assert_eq!(f, ["link"]);
        assert_eq!(rest, " --not-a-flag x");
        let (f, rest) = extract_flags("-x --y");
        assert!(f.is_empty());
        assert_eq!(rest, "-x --y");
        let (f, rest) = extract_flags("--a=1 --b='2 3'");
        assert_eq!(f, ["a=1", "b=2 3"]);
        assert_eq!(rest, "");
        let (f, _) = extract_flags("--a=x\\ y z");
        assert_eq!(f, ["a=x y"]);
        let (f, rest) = extract_flags("echo --x");
        assert!(f.is_empty());
        assert_eq!(rest, "echo --x");
    }

    #[test]
    fn words_keep_quotes_and_escapes() {
        assert_eq!(
            parse_words(r#"a="b c" d=e\ f g='h i'"#, '\\'),
            [r#"a="b c""#, r"d=e\ f", "g='h i'"]
        );
        assert_eq!(parse_words(r#"a=\"b c"#, '\\'), [r#"a=\"b"#, "c"]);
        assert_eq!(parse_words("x=\"\"", '\\'), ["x=\"\""]);
        assert_eq!(parse_words("  ", '\\'), Vec::<String>::new());
        assert_eq!(parse_words("a\\", '\\'), ["a"]);
        assert_eq!(parse_words("a=`\"b c`\"", '`'), ["a=`\"b", "c`\""]);
    }

    #[test]
    fn name_values_both_forms() {
        let nv = |s| parse_name_val(s, "ENV", '\\');
        assert_eq!(
            nv("A=1 B=\"2 3\"").unwrap(),
            [("A".into(), "1".into()), ("B".into(), "\"2 3\"".into())]
        );
        assert_eq!(
            nv("A  hello  world ").unwrap(),
            [("A".into(), "hello  world ".into())]
        );
        assert_eq!(nv("A=").unwrap(), [("A".into(), String::new())]);
        assert_eq!(nv("").unwrap_err(), "ENV requires at least one argument");
        assert_eq!(nv("A").unwrap_err(), "ENV must have two arguments");
        assert_eq!(nv("=1").unwrap_err(), "ENV names can not be blank");
        assert!(nv("A=1 B").unwrap_err().contains("can't find ="));
    }

    #[test]
    fn json_or_text() {
        assert_eq!(
            maybe_json(r#"["a"]"#).unwrap(),
            Form::Json(vec!["a".into()])
        );
        assert_eq!(
            maybe_json("[ -f x ]").unwrap(),
            Form::Text("[ -f x ]".into())
        );
        assert!(maybe_json("[1]").is_err());
        assert_eq!(
            maybe_json_list("a  b").unwrap(),
            (vec!["a".into(), "b".into()], false)
        );
    }

    #[test]
    fn flag_tables() {
        let table = [
            ("from", FlagKind::Value),
            (
                "link",
                FlagKind::Unsupported("COPY --link is not supported yet"),
            ),
        ];
        let p = |f: &[&str]| {
            let f: Vec<String> = f.iter().map(|s| s.to_string()).collect();
            parse_flags(&f, &table)
        };
        assert_eq!(p(&["from=a"]).unwrap(), [("from".into(), "a".into())]);
        assert_eq!(p(&["from"]).unwrap_err(), "missing a value on flag: from");
        assert_eq!(
            p(&["from=a", "from=b"]).unwrap_err(),
            "duplicate flag specified: from"
        );
        assert_eq!(
            p(&["link"]).unwrap_err(),
            "COPY --link is not supported yet"
        );
        assert_eq!(p(&["nope=1"]).unwrap_err(), "unknown flag: nope");
    }
}
