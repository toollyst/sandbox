//! `sbx tui`의 키 목록. 정본이다: `?` 도움말([`super::view`])과 `sbx tui --help`([`crate::cli`])가 여기서 나오고,
//! 키가 하는 일은 [`super::model`]이 이 표대로 한다(테스트가 확인한다).
//!
//! - 의존: 없음. `cli`가 도움말에 쓰므로 위 모듈(`output` 등)을 부르지 않는다.

/// 키 하나의 설명.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyHelp {
    /// 키.
    pub keys: &'static str,
    /// 어느 탭에서.
    pub tabs: &'static str,
    /// 하는 일(부르는 `sbx` 명령).
    pub does: &'static str,
}

const fn k(keys: &'static str, tabs: &'static str, does: &'static str) -> KeyHelp {
    KeyHelp { keys, tabs, does }
}

/// 키 목록. 정본이다.
pub const KEYS: &[KeyHelp] = &[
    k("Tab, 1 2 3 4", "all", "switch tab"),
    k("Up Down j k, PgUp PgDn, g G", "all", "move"),
    k("Enter", "all", "details (as JSON)"),
    k("/", "all", "filter rows by text (Esc clears)"),
    k(
        ":",
        "all",
        "run any sbx command, e.g. `:run -it alpine sh` (hands over the terminal)",
    ),
    k("s", "sandboxes", "start"),
    k("x", "sandboxes", "stop"),
    k("r", "sandboxes", "restart"),
    k("K", "sandboxes", "kill (asks first)"),
    k("D", "sandboxes", "rm -f (asks first)"),
    k("D", "images", "rmi (asks first)"),
    k(
        "P",
        "images",
        "system prune -a: stopped sandboxes, unused images and layers (asks first)",
    ),
    k("U", "images", "system df: disk usage (hands over the terminal)"),
    k("e", "sandboxes", "exec -it ID sh (hands over the terminal)"),
    k("a", "sandboxes", "attach (hands over the terminal)"),
    k(
        "l",
        "sandboxes",
        "logs -f (hands over the terminal; Ctrl+C stops following)",
    ),
    k(
        "u",
        "executors",
        "executor use (the default for new sandboxes)",
    ),
    k(
        "i",
        "executors",
        "executor install (fills in the command to edit; hands over the terminal)",
    ),
    k(
        "S",
        "executors",
        "executor search (hands over the terminal)",
    ),
    k(
        "D",
        "executors",
        "executor rm, or executor adapter rm (asks first)",
    ),
    k("P", "executors", "executor prune (asks first)"),
    k("?", "all", "this help"),
    k("q, Ctrl+C", "all", "quit"),
];

/// `sbx tui --help`에 붙는 키 목록.
pub fn help_text() -> String {
    let mut s = String::from(
        "Every action runs an sbx command and shows its command line. Commands that need the terminal\n\
         (exec, attach, logs -f, executor install and search, `:`) take it over and give it back when they exit.\n\nKeys:\n",
    );
    for h in KEYS {
        s.push_str(&format!("  {:<28} {:<10} {}\n", h.keys, h.tabs, h.does));
    }
    s
}
