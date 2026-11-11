//! `--filter KEY=VALUE` (`ps`, `images`, `events`). 규칙은 Docker와 같다.
//!
//! - 다른 키끼리는 모두 맞아야 하고(AND), 같은 키를 여러 번 주면 그중 하나만 맞으면 된다(OR).
//! - 명령마다 받는 키는 [`PS`], [`IMAGES`], [`EVENTS`]에 있다. 모르는 키나 잘못된 값이면 아무것도 하지 않고 오류다.
//! - 키마다 맞추는 방법은 표의 설명(`help`)이 정본이고, 공용 규칙은 [`label`], [`glob`], [`image`]다.
//!   Docker와 다른 점(이름은 정규식이 아니라 부분 문자열 등)은 `behavior::DOCKER_DIFFERENCES`에 있다.
//!
//! 이 모듈은 입출력이 없다. 무엇과 맞출지는 부르는 쪽(`app`)이 값을 넘긴다.

use std::collections::BTreeMap;

/// 키가 받는 값.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Value {
    /// 아무 문자열.
    Any,
    /// 이 중 하나.
    OneOf(&'static [&'static str]),
    /// 정수.
    Int,
    /// `true` 또는 `false`.
    Bool,
}

/// 받는 키 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    /// 이름.
    pub name: &'static str,
    /// 값.
    pub value: Value,
    /// 어떻게 맞추는지(사용자에게 보이는 설명).
    pub help: &'static str,
}

const fn key(name: &'static str, value: Value, help: &'static str) -> Key {
    Key { name, value, help }
}

/// 샌드박스 상태 이름(OCI).
pub const STATES: &[&str] = &["creating", "created", "running", "stopped"];
/// 이벤트 동작 이름.
pub const ACTIONS: &[&str] = &[
    "create",
    "start",
    "stop",
    "remove",
    "exec_start",
    "exec_die",
    "pull",
    "prepare",
    "untag",
    "delete",
    "connect",
    "refuse",
    "close",
];

/// `ps --filter`. `status`나 `exited`가 있으면 `-a`가 없어도 멈춘 것까지 본다(Docker와 같다).
pub const PS: &[Key] = &[
    key("id", Value::Any, "sandbox ID starts with VALUE"),
    key("name", Value::Any, "name contains VALUE"),
    key("label", Value::Any, "has label KEY, or KEY=VALUE exactly"),
    key("status", Value::OneOf(STATES), "state is VALUE"),
    key("exited", Value::Int, "stopped with exit code VALUE"),
    key(
        "ancestor",
        Value::Any,
        "runs image VALUE (reference, full ID, or ID prefix of 12+ hex digits)",
    ),
];

/// `images --filter`.
pub const IMAGES: &[Key] = &[
    key(
        "reference",
        Value::Any,
        "a reference matches the pattern VALUE (`*` any run, `?` one character), short or full form",
    ),
    key(
        "dangling",
        Value::Bool,
        "true: no reference points to it; false: one does",
    ),
    key(
        "label",
        Value::Any,
        "image config has label KEY, or KEY=VALUE exactly",
    ),
];

/// `events --filter`.
pub const EVENTS: &[Key] = &[
    key(
        "type",
        Value::OneOf(&["sandbox", "image", "network"]),
        "event type is VALUE",
    ),
    key("event", Value::OneOf(ACTIONS), "action is VALUE"),
    key(
        "sandbox",
        Value::Any,
        "sandbox event whose sandbox ID starts with VALUE or whose name is VALUE",
    ),
    key(
        "image",
        Value::Any,
        "image event for image VALUE, or sandbox event whose sandbox runs it (as for ps ancestor; \
         the reference is the event's own, else the one in the cache, so a deleted image matches by ID only)",
    ),
];

/// `system prune --filter`(Docker와 같은 키). 샌드박스에 적용한다.
pub const PRUNE: &[Key] = &[
    key(
        "until",
        Value::Any,
        "created before VALUE (RFC 3339 time or a duration ago like 24h, as for events --since)",
    ),
    key("label", Value::Any, "has label KEY, or KEY=VALUE exactly"),
];

/// 해석한 필터. 키마다 받은 값들.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    values: BTreeMap<&'static str, Vec<String>>,
}

impl Filters {
    /// `KEY=VALUE`들을 `keys`대로 해석한다. 값 안의 `=`는 값의 일부다(`label=a=b`).
    pub fn parse(given: &[String], keys: &'static [Key]) -> Result<Self, String> {
        let mut values: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
        for g in given {
            let Some((k, v)) = g.split_once('=') else {
                return Err(format!("--filter {g:?}: expected KEY=VALUE"));
            };
            let Some(spec) = keys.iter().find(|s| s.name == k) else {
                let names: Vec<&str> = keys.iter().map(|s| s.name).collect();
                return Err(format!(
                    "--filter {g:?}: unknown key {k:?} (this command takes {})",
                    names.join(", ")
                ));
            };
            let ok = match spec.value {
                Value::Any => !v.is_empty(),
                Value::OneOf(allowed) => allowed.contains(&v),
                Value::Int => v.parse::<i64>().is_ok(),
                Value::Bool => matches!(v, "true" | "false"),
            };
            if !ok {
                let want = match spec.value {
                    Value::Any => "a non-empty value".to_owned(),
                    Value::OneOf(a) => format!("one of {}", a.join(", ")),
                    Value::Int => "a whole number".to_owned(),
                    Value::Bool => "true or false".to_owned(),
                };
                return Err(format!("--filter {g:?}: {k} takes {want}"));
            }
            values.entry(spec.name).or_default().push(v.to_owned());
        }
        Ok(Self { values })
    }

    /// 그 키의 값들(없으면 비어 있다).
    pub fn values(&self, key: &str) -> impl Iterator<Item = &str> {
        self.values
            .get(key)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }

    /// 그 키가 있는지.
    pub fn has(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }

    /// 키가 없으면 `true`. 있으면 값 중 하나라도 `matches`를 만족할 때 `true`(같은 키는 OR).
    pub fn allows(&self, key: &str, matches: impl Fn(&str) -> bool) -> bool {
        self.values
            .get(key)
            .is_none_or(|vs| vs.iter().any(|v| matches(v)))
    }
}

/// `label=KEY`(그 라벨이 있다) 또는 `label=KEY=VALUE`(값까지 같다).
pub fn label(labels: &BTreeMap<String, String>, value: &str) -> bool {
    match value.split_once('=') {
        Some((k, v)) => labels.get(k).is_some_and(|have| have == v),
        None => labels.contains_key(value),
    }
}

/// `*`(아무 길이)와 `?`(글자 하나)만 있는 패턴이 `text` 전체와 맞는지.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    // 마지막 `*`의 위치로 되돌아가는 방식(선형에 가깝다).
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// 이미지 `id`(`sha256:<hex>`)이고 받을 때 참조가 `reference`인 이미지가 필터 값 `value`와 맞는지:
/// 전체 ID, `sha256:`이 있든 없든 12자리 이상의 hex 앞부분, 또는 참조. `normalized`는 `value`를 참조로 정규화한 것
/// (`alpine` → `docker.io/library/alpine:latest`, 참조가 아니면 `None`)이고 부르는 쪽이 넘긴다.
pub fn image(id: &str, reference: Option<&str>, value: &str, normalized: Option<&str>) -> bool {
    if value == id {
        return true;
    }
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    let prefix = value.strip_prefix("sha256:").unwrap_or(value);
    if prefix.len() >= 12
        && prefix.bytes().all(|b| b.is_ascii_hexdigit())
        && hex.starts_with(prefix)
    {
        return true;
    }
    matches!((reference, normalized), (Some(r), Some(n)) if r == n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(given: &[&str], keys: &'static [Key]) -> Result<Filters, String> {
        Filters::parse(
            &given.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            keys,
        )
    }

    #[test]
    fn different_keys_and_same_key_or() {
        let f = parse(&["status=running", "status=stopped", "name=web"], PS).unwrap();
        let ok = |state: &str, name: &str| {
            f.allows("status", |v| v == state) && f.allows("name", |v| name.contains(v))
        };
        assert!(ok("running", "my-web"));
        assert!(ok("stopped", "web"));
        assert!(!ok("created", "web"), "status is OR, but must match one");
        assert!(!ok("running", "db"), "different keys are AND");
        // 없는 키는 모두 통과한다.
        assert!(f.allows("label", |_| false));
        assert!(Filters::default().allows("status", |_| false));
        assert!(f.has("status") && !f.has("exited"));
    }

    #[test]
    fn bad_filters_are_errors_that_say_what_is_wanted() {
        let err = |g: &[&str], keys| parse(g, keys).unwrap_err();
        assert!(err(&["status"], PS).contains("KEY=VALUE"));
        assert!(err(&["color=red"], PS).contains("id, name, label, status, exited, ancestor"));
        assert!(err(&["status=exited"], PS).contains("creating, created, running, stopped"));
        assert!(err(&["exited=x"], PS).contains("whole number"));
        assert!(err(&["dangling=yes"], IMAGES).contains("true or false"));
        assert!(err(&["name="], PS).contains("non-empty"));
        assert!(err(&["event=die"], EVENTS).contains("create, start"));
        assert!(err(&["ancestor=alpine"], EVENTS).contains("unknown key"));
        // 값 안의 `=`는 값이다.
        let f = parse(&["label=a=b=c"], PS).unwrap();
        assert!(f.allows("label", |v| v == "a=b=c"));
    }

    #[test]
    fn labels_by_key_or_exact_value() {
        let labels: BTreeMap<String, String> = [
            ("owner".to_owned(), "mcp".to_owned()),
            ("e".to_owned(), "".to_owned()),
        ]
        .into();
        assert!(label(&labels, "owner"));
        assert!(label(&labels, "owner=mcp"));
        assert!(!label(&labels, "owner=cli"));
        assert!(!label(&labels, "own"));
        assert!(label(&labels, "e="));
        assert!(!label(&labels, "missing"));
    }

    #[test]
    fn globs_match_the_whole_text() {
        for (p, t, want) in [
            ("alpine", "alpine", true),
            ("alpine", "alpine:3.20", false),
            ("alpine*", "alpine:3.20", true),
            ("*:3.20", "alpine:3.20", true),
            ("al?ine:*", "alpine:latest", true),
            ("*", "", true),
            ("?", "", false),
            ("a*b*c", "aXbYbZc", true),
            ("a*b*c", "aXbYbZ", false),
            ("**x", "abcx", true),
            ("ghcr.io/*", "docker.io/library/x", false),
        ] {
            assert_eq!(glob(p, t), want, "{p:?} {t:?}");
        }
    }

    #[test]
    fn images_by_id_prefix_or_reference() {
        let id = format!("sha256:{}", "ab12".repeat(16));
        let r = Some("docker.io/library/alpine:3.20");
        assert!(image(&id, r, &id, None));
        assert!(image(&id, r, "ab12ab12ab12", None));
        assert!(image(&id, r, "sha256:ab12ab12ab12ab", None));
        assert!(
            !image(&id, r, "ab12ab12ab1", None),
            "fewer than 12 hex digits"
        );
        assert!(image(
            &id,
            r,
            "alpine:3.20",
            Some("docker.io/library/alpine:3.20")
        ));
        assert!(!image(
            &id,
            r,
            "alpine",
            Some("docker.io/library/alpine:latest")
        ));
        assert!(!image(
            &id,
            None,
            "alpine:3.20",
            Some("docker.io/library/alpine:3.20")
        ));
    }
}
