//! exec 형식(`["npm", "ci"]`)의 JSON 배열 읽기. Docker가 Go의 `encoding/json`으로 읽는 것과 같은 답을 낸다.
//!
//! - JSON 배열이고 원소가 모두 문자열이면 그 목록이다.
//! - JSON으로는 맞지만 문자열이 아닌 원소가 있으면 오류다(Docker도 셸 형식으로 넘어가지 않는다).
//! - JSON이 아니면 셸 형식이다(`RUN [ -f x ] && ...`).
//! - 깊이가 [`MAX_DEPTH`]를 넘는 중첩은 JSON이 아닌 것으로 본다(스택을 지킨다). Go는 10000까지 받는다.

/// 중첩 깊이 상한.
pub const MAX_DEPTH: usize = 128;

/// 읽은 결과.
#[derive(Debug, PartialEq, Eq)]
pub enum Json {
    /// 문자열 배열.
    Strings(Vec<String>),
    /// JSON 배열이지만 문자열이 아닌 원소가 있다.
    NotStrings,
    /// JSON 배열이 아니다.
    Other,
}

/// `text`(앞 공백은 이미 뺀 것)를 읽는다.
pub fn parse_array(text: &str) -> Json {
    let mut p = Parser {
        s: text.as_bytes(),
        text,
        i: 0,
    };
    p.ws();
    if p.peek() != Some(b'[') {
        return Json::Other;
    }
    let Some(value) = p.value(0) else {
        return Json::Other;
    };
    p.ws();
    if p.i != p.s.len() {
        return Json::Other;
    }
    match value {
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s),
                    _ => return Json::NotStrings,
                }
            }
            Json::Strings(out)
        }
        _ => Json::Other,
    }
}

/// 문자열을 JSON 문자열로 쓴다(진행 표시와 캐시 키의 `["a", "b"]`).
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 문자열 목록을 `["a", "b"]`로 쓴다.
pub fn array(items: &[String]) -> String {
    let parts: Vec<String> = items.iter().map(|s| quote(s)).collect();
    format!("[{}]", parts.join(", "))
}

enum Value {
    Array(Vec<Value>),
    String(String),
    Other,
}

struct Parser<'a> {
    s: &'a [u8],
    text: &'a str,
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn eat(&mut self, b: u8) -> Option<()> {
        (self.peek() == Some(b)).then(|| self.i += 1)
    }

    fn value(&mut self, depth: usize) -> Option<Value> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.ws();
        match self.peek()? {
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.eat(b']').is_some() {
                    return Some(Value::Array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b']' => {
                            self.i += 1;
                            return Some(Value::Array(items));
                        }
                        _ => return None,
                    }
                }
            }
            b'{' => {
                self.i += 1;
                self.ws();
                if self.eat(b'}').is_some() {
                    return Some(Value::Other);
                }
                loop {
                    self.ws();
                    self.string()?;
                    self.ws();
                    self.eat(b':')?;
                    self.value(depth + 1)?;
                    self.ws();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b'}' => {
                            self.i += 1;
                            return Some(Value::Other);
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.string().map(Value::String),
            b't' => self.word("true"),
            b'f' => self.word("false"),
            b'n' => self.word("null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn word(&mut self, w: &str) -> Option<Value> {
        self.s[self.i..].starts_with(w.as_bytes()).then(|| {
            self.i += w.len();
            Value::Other
        })
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Option<Value> {
        self.eat(b'-');
        if self.eat(b'0').is_none() && self.digits() == 0 {
            return None;
        }
        if self.eat(b'.').is_some() && self.digits() == 0 {
            return None;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return None;
            }
        }
        Some(Value::Other)
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = self.s.get(self.i..self.i + 4)?;
        let h = std::str::from_utf8(h).ok()?;
        let v = u32::from_str_radix(h, 16).ok()?;
        // from_str_radix는 앞의 `+`를 받는다. JSON은 받지 않는다.
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        self.i += 4;
        Some(v)
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let start = self.i;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\\' || b < 0x20 {
                    break;
                }
                self.i += 1;
            }
            // 경계는 ASCII 바이트이므로 UTF-8 문자 가운데가 아니다.
            out.push_str(&self.text[start..self.i]);
            match self.peek()? {
                b'"' => {
                    self.i += 1;
                    return Some(out);
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.peek()?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            // Go처럼 짝이 없는 대리 쌍은 U+FFFD로 바꾼다.
                            let c = if (0xD800..0xDC00).contains(&hi)
                                && self.s[self.i..].starts_with(b"\\u")
                            {
                                let save = self.i;
                                self.i += 2;
                                match self.hex4() {
                                    Some(lo) if (0xDC00..0xE000).contains(&lo) => char::from_u32(
                                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00),
                                    ),
                                    _ => {
                                        self.i = save;
                                        None
                                    }
                                }
                            } else {
                                char::from_u32(hi)
                            };
                            out.push(c.unwrap_or('\u{FFFD}'));
                        }
                        _ => return None,
                    }
                }
                _ => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Json {
        Json::Strings(v.iter().map(|x| x.to_string()).collect())
    }

    #[test]
    fn string_arrays() {
        assert_eq!(parse_array(r#"["npm", "ci"]"#), s(&["npm", "ci"]));
        assert_eq!(parse_array("[]"), s(&[]));
        assert_eq!(parse_array(" [ \"a\" ] \t"), s(&["a"]));
        assert_eq!(
            parse_array(r#"["a\"b\\c\/d\n\u00e9\ud83d\ude00"]"#),
            s(&["a\"b\\c/d\né😀"])
        );
        assert_eq!(parse_array(r#"["\ud800"]"#), s(&["\u{FFFD}"]));
        assert_eq!(parse_array(r#"["한글"]"#), s(&["한글"]));
    }

    #[test]
    fn valid_json_with_other_values_is_an_error_not_shell_form() {
        for t in [
            "[1]",
            r#"["a", null]"#,
            "[true]",
            r#"[{"a": [1, 2.5e-3]}]"#,
            "[[]]",
        ] {
            assert_eq!(parse_array(t), Json::NotStrings, "{t}");
        }
    }

    #[test]
    fn anything_else_is_shell_form() {
        for t in [
            "[ -f x ] && echo",
            "[\"a\"",
            "[\"a\",]",
            "['a']",
            "[\"a\"] x",
            "[01]",
            "[\"\\x\"]",
            "[\"\t\"]",
            "[\"\\u+123\"]",
            "\"a\"",
            "",
        ] {
            assert_eq!(parse_array(t), Json::Other, "{t}");
        }
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert_eq!(parse_array(&deep), Json::Other);
    }

    #[test]
    fn quoted_arrays_read_back() {
        let items = vec![
            "a\"b".to_string(),
            "\\".into(),
            "\u{1}".into(),
            "x y".into(),
        ];
        assert_eq!(
            parse_array(&array(&items)),
            Json::Strings(items.clone()),
            "{}",
            array(&items)
        );
        assert_eq!(array(&["npm".into(), "ci".into()]), r#"["npm", "ci"]"#);
    }
}
