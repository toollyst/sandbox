//! 상세 창(Enter)의 내용: 고른 줄의 공개 출력 자료형을 사람이 읽는 `이름  값` 줄로 편다(YAML과 비슷하다).
//!
//! 자료형마다 손으로 쓰지 않는다. `--format json`과 같은 직렬화를 거쳐 펴므로 CLI가 내는 것과 어긋나지 않고,
//! 이름은 JSON의 필드 이름 그대로다(`--jq`로 꺼낼 때 그 이름을 쓴다). 순서도 JSON 그대로다: serde_json의 `Value`는
//! 이름순으로 다시 늘어놓으므로, 순서를 지키는 jaq의 값으로 읽는다.
//!
//! - 비었거나(`null`, `[]`, `{}`) 없는 것은 줄을 두지 않는다.
//! - 값이 짧은 목록은 한 줄(`a, b`), 길면 줄마다 `- `.
//! - 객체 목록은 항목마다 `- `로 시작한다.

use jaq_json::Val;
use serde::Serialize;

/// 짧은 목록을 한 줄에 둘 최대 글자 수.
const INLINE_LIST_MAX: usize = 60;

/// 상세 창의 줄 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailLine {
    /// 들여쓰기 단계.
    pub depth: u16,
    /// 목록 항목의 첫 줄이면 `- `를 붙인다.
    pub bullet: bool,
    /// 필드 이름. 목록 항목의 값만 있는 줄이면 비었다.
    pub key: String,
    /// 값. 아래 줄에 더 있으면(객체, 긴 목록) 비었다.
    pub value: String,
    /// 같은 단계 형제들의 이름 칸 너비(값을 한 칸에 맞춘다).
    pub key_width: usize,
}

/// `x`를 상세 창의 줄로 편다. 직렬화하지 못하면 빈 목록이다(공개 출력 자료형은 늘 된다).
pub fn readable<T: Serialize>(x: &T) -> Vec<DetailLine> {
    let Ok(bytes) = serde_json::to_vec(x) else {
        return Vec::new();
    };
    let Ok(v) = jaq_json::read::parse_single(&bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match &v {
        Val::Obj(_) => fields(&v, 0, &mut out),
        other => out.push(line(0, "", text(other))),
    }
    out
}

/// 객체의 필드를 `depth` 단계에 편다.
fn fields(v: &Val, depth: u16, out: &mut Vec<DetailLine>) {
    let Val::Obj(map) = v else { return };
    let mut siblings = Vec::new();
    for (k, x) in map.iter() {
        if blank(x) {
            continue;
        }
        siblings.push(out.len());
        field(&text(k), x, depth, out);
    }
    let width = siblings
        .iter()
        .filter(|&&i| !out[i].value.is_empty())
        .map(|&i| out[i].key.chars().count())
        .max()
        .unwrap_or(0);
    for i in siblings {
        out[i].key_width = width;
    }
}

fn field(key: &str, x: &Val, depth: u16, out: &mut Vec<DetailLine>) {
    match x {
        Val::Obj(_) => {
            out.push(line(depth, key, String::new()));
            fields(x, depth + 1, out);
        }
        // 명령과 인자는 셸에 붙여 넣을 수 있게(`sh -c 'a, b'`).
        Val::Arr(items) if key == "args" && items.iter().all(scalar) => {
            let words: Vec<String> = items.iter().map(|i| quote(&text(i))).collect();
            out.push(line(depth, key, words.join(" ")));
        }
        Val::Arr(items) if items.iter().all(scalar) => {
            let joined = items.iter().map(text).collect::<Vec<_>>().join(", ");
            if joined.chars().count() <= INLINE_LIST_MAX {
                out.push(line(depth, key, joined));
            } else {
                out.push(line(depth, key, String::new()));
                for i in items.iter() {
                    let mut l = line(depth + 1, "", text(i));
                    l.bullet = true;
                    out.push(l);
                }
            }
        }
        Val::Arr(items) => {
            out.push(line(depth, key, String::new()));
            for item in items.iter() {
                let first = out.len();
                match item {
                    Val::Obj(_) => fields(item, depth + 1, out),
                    other => out.push(line(depth + 1, "", text(other))),
                }
                if let Some(l) = out.get_mut(first) {
                    l.bullet = true;
                }
            }
        }
        other => out.push(line(depth, key, text(other))),
    }
}

/// 셸에서 한 낱말이 되게: 그대로 써도 되면 그대로, 아니면 작은따옴표로.
pub fn quote(word: &str) -> String {
    let plain = !word.is_empty()
        && word.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@' | ',')
        });
    if plain {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

fn line(depth: u16, key: &str, value: String) -> DetailLine {
    DetailLine {
        depth,
        bullet: false,
        key: key.to_owned(),
        value,
        key_width: 0,
    }
}

fn blank(v: &Val) -> bool {
    match v {
        Val::Null => true,
        Val::Arr(a) => a.is_empty(),
        Val::Obj(m) => m.is_empty(),
        _ => false,
    }
}

fn scalar(v: &Val) -> bool {
    !matches!(v, Val::Arr(_) | Val::Obj(_))
}

/// 값 하나를 글로: 문자열은 따옴표 없이, 나머지는 JSON 그대로.
fn text(v: &Val) -> String {
    match v {
        Val::TStr(b) | Val::BStr(b) => String::from_utf8_lossy(b).into_owned(),
        other => other.to_string(),
    }
}

/// 줄을 글로(테스트와 `sbx tui` 밖에서 볼 때): 단계마다 두 칸, 항목은 `- `, 이름 칸을 맞춘다.
pub fn plain(lines: &[DetailLine]) -> String {
    lines
        .iter()
        .map(|l| {
            let mut s = "  ".repeat(l.depth as usize);
            if l.bullet {
                s.replace_range(s.len().saturating_sub(2).., "- ");
            }
            if !l.key.is_empty() {
                s.push_str(&l.key);
                if !l.value.is_empty() {
                    let pad = l.key_width.saturating_sub(l.key.chars().count()) + 2;
                    s.push_str(&" ".repeat(pad));
                }
            }
            s.push_str(&l.value);
            s
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_field_order_skips_empty_and_aligns_values() {
        let s = crate::tui::model::tests::sandbox(Some("web"), '1', "running");
        let text = plain(&readable(&s));
        // JSON과 같은 순서(이름순이 아니다): id가 name보다, name이 created보다 먼저.
        let at = |k: &str| {
            text.lines()
                .position(|l| l.trim_start().starts_with(&format!("{k} ")) || l.trim() == k)
                .unwrap_or_else(|| panic!("{k}\n{text}"))
        };
        assert!(
            at("id") < at("name") && at("name") < at("created"),
            "{text}"
        );
        // null(exit_code, error, hostname)과 빈 목록·객체(mounts, labels)는 없다.
        for gone in ["exit_code", "error", "hostname", "mounts", "labels", "null"] {
            assert!(!text.contains(gone), "{gone}\n{text}");
        }
        // 객체는 들여 쓰고, 짧은 목록은 한 줄이다. 형제끼리 값 칸이 맞는다.
        assert!(text.contains("\nimage\n  id         sha256:"), "{text}");
        assert!(
            text.contains("\n  reference  docker.io/library/alpine:3.20"),
            "{text}"
        );
        assert!(text.contains("\n  args  sleep 1d"), "{text}");
        assert!(!text.contains('{') && !text.contains('"'), "{text}");
    }

    #[test]
    fn long_lists_and_lists_of_objects_get_bullets() {
        let v = serde_json::json!({
            "env": ["A=".to_owned() + &"x".repeat(40), "B=".to_owned() + &"y".repeat(40)],
            "mounts": [{"type": "bind", "target": "/w"}, {"type": "copy", "target": "/c"}],
            "n": 3,
            "ok": true,
        });
        let text = plain(&readable(&v));
        assert!(text.contains("env\n- A=xxx"), "{text}");
        // (json!은 이름순으로 둔다. 자료형을 거치면 선언 순서다.)
        assert!(
            text.contains("mounts\n- target  /w\n  type    bind\n- target  /c"),
            "{text}"
        );
        // 값이 있는 형제(n, ok)끼리 맞춘다. 아래에 더 있는 것(env, mounts)은 세지 않는다.
        assert!(text.contains("\nn   3\nok  true"), "{text}");
    }
}
