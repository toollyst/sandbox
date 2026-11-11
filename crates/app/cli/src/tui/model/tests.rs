use super::*;
use crate::tui::keys::{KEYS, help_text};

use std::collections::BTreeMap;

use ratatui::crossterm::event::KeyEvent;

/// `ps -a`가 낼 샌드박스 하나(공개 출력 자료형을 JSON에서 엄격히 읽는다).
pub(crate) fn sandbox(name: Option<&str>, id_digit: char, state: &str) -> Sandbox {
    serde_json::from_value(serde_json::json!({
        "id": id_digit.to_string().repeat(64),
        "name": name,
        "created": "2026-11-10T12:00:00Z",
        "state": state,
        "exit_code": if state == "stopped" { Some(0) } else { None },
        "error": null,
        "image": {"id": format!("sha256:{}", "ab".repeat(32)), "reference": "docker.io/library/alpine:3.20"},
        "rootfs_format": "erofs",
        "process": {"args": ["sleep", "1d"], "env": [], "cwd": "/", "user": "0:0"},
        "resources": {"vcpus": 1, "memory_mib": 512, "scratch_disk_mib": 1024},
        "network": {"mode": "none", "allow": []},
        "mounts": [],
        "hostname": null,
        "labels": {},
        "stop": {"signal": "SIGTERM", "timeout_secs": 10},
        "interactive": false,
        "tty": false,
    }))
    .unwrap()
}

pub(crate) fn image(reference: Option<&str>) -> Image {
    serde_json::from_value(serde_json::json!({
        "id": format!("sha256:{}", "cd".repeat(32)),
        "references": reference.into_iter().collect::<Vec<_>>(),
        "arch": "arm64",
        "prepared": ["erofs"],
        "compressed_size": 3_500_000,
        "ignored_fields": [],
    }))
    .unwrap()
}

pub(crate) fn event(kind: &str, action: &str, id: &str) -> Event {
    Event {
        time: "2026-11-10T13:20:14.000000001Z".to_owned(),
        kind: kind.to_owned(),
        action: action.to_owned(),
        id: id.to_owned(),
        attributes: BTreeMap::from([("name".to_owned(), "web".to_owned())]),
    }
}

/// 샌드박스 둘(web 실행 중, 이름 없는 것 멈춤)과 이미지 하나.
pub(crate) fn loaded() -> Model {
    let mut m = Model::default();
    m.apply(Msg::Sandboxes(Ok(vec![
        sandbox(Some("web"), '1', "running"),
        sandbox(None, '2', "stopped"),
    ])));
    m.apply(Msg::Images(Ok(vec![image(Some(
        "docker.io/library/alpine:3.20",
    ))])));
    m
}

/// 실행기 둘(wick 기본, fake)과 어댑터 하나, 지금 쓸 것은 wick.
pub(crate) fn executors() -> Executors {
    let ex =
        |name: &str, version: &str, default: bool, adapter: Option<&str>| -> InstalledExecutor {
            serde_json::from_value(serde_json::json!({
                "name": name, "version": version, "default": default,
                "path": format!("/d/executors/{name}/{version}"), "adapter": adapter,
                "origin": adapter.map(|_| "official"),
            }))
            .unwrap()
        };
    Executors {
        executors: vec![
            ex("fake", "1.0", false, None),
            ex("wick", "1.18.0", true, Some("firecracker")),
        ],
        adapters: vec![
            serde_json::from_value(serde_json::json!({
                "name": "firecracker", "version": "0.2.0",
                "path": "/d/adapters/firecracker/0.2.0/sbx-executor", "used_by": ["wick@1.18.0"],
            }))
            .unwrap(),
        ],
        current: serde_json::from_value(serde_json::json!({
            "name": "wick", "version": "1.18.0", "source": "default (sbx executor use)",
            "program": "/d/adapters/firecracker/0.2.0/sbx-executor", "adapter": "firecracker@0.2.0",
            "vmm_dir": "/d/executors/wick/1.18.0",
        }))
        .map_err(|e: serde_json::Error| e.to_string()),
    }
}

fn press(m: &mut Model, code: KeyCode) -> Option<Action> {
    m.key(KeyEvent::from(code))
}

fn chars(m: &mut Model, s: &str) -> Option<Action> {
    let mut last = None;
    for c in s.chars() {
        last = press(m, KeyCode::Char(c));
    }
    last
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn keys_become_sbx_command_lines_for_the_selected_sandbox() {
    let mut m = loaded();
    assert_eq!(
        press(&mut m, KeyCode::Char('x')),
        Some(Action::Run(strings(&["stop", "web"])))
    );
    // 상태 줄은 실행 중으로 그 명령 줄을 보인다.
    let s = m.status.clone().unwrap();
    assert_eq!((s.args, s.ok), (strings(&["stop", "web"]), None));
    assert_eq!(
        press(&mut m, KeyCode::Char('e')),
        Some(Action::Handover(strings(&["exec", "-it", "web", SHELL])))
    );
    assert_eq!(
        press(&mut m, KeyCode::Char('l')),
        Some(Action::Handover(strings(&["logs", "-f", "web"])))
    );
    // 이름이 없으면 전체 ID.
    press(&mut m, KeyCode::Down);
    assert_eq!(
        press(&mut m, KeyCode::Char('s')),
        Some(Action::Run(strings(&["start", &"2".repeat(64)])))
    );
}

#[test]
fn destructive_keys_ask_first() {
    let mut m = loaded();
    assert_eq!(press(&mut m, KeyCode::Char('D')), None);
    assert_eq!(
        m.popup,
        Some(Popup::Confirm {
            args: strings(&["rm", "-f", "web"])
        })
    );
    // y가 아니면 그냥 닫는다.
    assert_eq!(press(&mut m, KeyCode::Char('n')), None);
    assert_eq!(m.popup, None);
    press(&mut m, KeyCode::Char('K'));
    assert_eq!(
        press(&mut m, KeyCode::Char('y')),
        Some(Action::Run(strings(&["kill", "web"])))
    );
    // 이미지 탭의 D는 rmi(참조가 있으면 참조).
    press(&mut m, KeyCode::Char('2'));
    assert_eq!(press(&mut m, KeyCode::Char('D')), None);
    assert_eq!(
        press(&mut m, KeyCode::Char('y')),
        Some(Action::Run(strings(&[
            "rmi",
            "docker.io/library/alpine:3.20"
        ])))
    );
}

#[test]
fn every_listed_letter_key_does_something() {
    for h in KEYS.iter().filter(|h| h.tabs == "sandboxes") {
        let mut m = loaded();
        let c = h.keys.chars().next().unwrap();
        let acted = press(&mut m, KeyCode::Char(c)).is_some() || m.popup.is_some();
        assert!(acted, "{:?}", h.keys);
    }
    for h in KEYS.iter().filter(|h| h.tabs == "executors") {
        let mut m = loaded();
        m.apply(Msg::Executors(Ok(executors())));
        m.tab = Tab::Executors;
        // 기본이 아닌 실행기(fake)를 고른다.
        let c = h.keys.chars().next().unwrap();
        let acted =
            press(&mut m, KeyCode::Char(c)).is_some() || m.popup.is_some() || m.prompt.is_some();
        assert!(acted, "{:?}", h.keys);
    }
    // 고를 것이 없으면 아무것도 하지 않는다.
    let mut empty = Model::default();
    assert_eq!(press(&mut empty, KeyCode::Char('x')), None);
    assert_eq!(press(&mut empty, KeyCode::Char('D')), None);
    assert_eq!(empty.popup, None);
}

#[test]
fn quit_keys_and_tabs() {
    let mut m = loaded();
    assert_eq!(press(&mut m, KeyCode::Tab), None);
    assert_eq!(m.tab(), Tab::Images);
    press(&mut m, KeyCode::BackTab);
    press(&mut m, KeyCode::BackTab);
    assert_eq!(m.tab(), Tab::Executors);
    press(&mut m, KeyCode::BackTab);
    assert_eq!(m.tab(), Tab::Events);
    assert_eq!(press(&mut m, KeyCode::Char('q')), Some(Action::Quit));
    // 입력 중에도 Ctrl+C는 끝낸다. q는 글자다.
    chars(&mut m, "/");
    assert_eq!(press(&mut m, KeyCode::Char('q')), None);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(m.key(ctrl_c), Some(Action::Quit));
}

#[test]
fn filter_applies_as_you_type_and_esc_clears() {
    let mut m = loaded();
    chars(&mut m, "/WE");
    assert_eq!(m.filter, "WE");
    assert_eq!(m.visible_sandboxes().len(), 1, "case-insensitive");
    press(&mut m, KeyCode::Enter);
    assert_eq!(m.prompt, None);
    assert_eq!(m.filter, "WE", "Enter keeps the filter");
    // 거른 목록에서 고른 것이 대상이다.
    // `/`는 새로 친다.
    chars(&mut m, "/stopped");
    press(&mut m, KeyCode::Enter);
    assert_eq!(
        press(&mut m, KeyCode::Char('s')),
        Some(Action::Run(strings(&["start", &"2".repeat(64)])))
    );
    press(&mut m, KeyCode::Esc);
    assert_eq!(m.filter, "");
    assert_eq!(m.visible_sandboxes().len(), 2);
}

#[test]
fn command_prompt_hands_over_any_sbx_command() {
    let mut m = loaded();
    chars(&mut m, ":run -it -e 'A=b c' alpine sh");
    assert_eq!(
        press(&mut m, KeyCode::Enter),
        Some(Action::Handover(strings(&[
            "run", "-it", "-e", "A=b c", "alpine", "sh"
        ])))
    );
    // 빈 줄은 아무것도 하지 않는다. 따옴표가 안 닫히면 상태 줄에 까닭.
    chars(&mut m, ":");
    assert_eq!(press(&mut m, KeyCode::Enter), None);
    chars(&mut m, ":run \"x");
    assert_eq!(press(&mut m, KeyCode::Enter), None);
    let s = m.status.unwrap();
    assert_eq!((s.ok, s.message.as_str()), (Some(false), "unclosed \""));
}

#[test]
fn detail_shows_the_selected_row_readably_and_scrolls() {
    let mut m = loaded();
    press(&mut m, KeyCode::Enter);
    let Some(Popup::Detail {
        title,
        lines,
        source,
        ..
    }) = &m.popup
    else {
        panic!("{:?}", m.popup)
    };
    assert_eq!(title, "Sandbox web");
    // JSON이 아니라 이름과 값이다. 같은 것을 JSON으로 내는 명령도 알린다.
    let text = crate::tui::detail::plain(lines);
    assert!(
        text.contains("name") && text.contains("web") && !text.contains('{'),
        "{text}"
    );
    assert_eq!(source, "sbx inspect web --format json");
    press(&mut m, KeyCode::Down);
    assert!(matches!(m.popup, Some(Popup::Detail { scroll: 1, .. })));
    press(&mut m, KeyCode::Esc);
    assert_eq!(m.popup, None);
}

#[test]
fn done_updates_only_the_command_it_belongs_to() {
    let mut m = loaded();
    press(&mut m, KeyCode::Char('x'));
    press(&mut m, KeyCode::Char('r'));
    m.apply(Msg::Done {
        args: strings(&["stop", "web"]),
        ok: true,
        message: String::new(),
    });
    assert_eq!(
        m.status.as_ref().unwrap().ok,
        None,
        "restart is still shown"
    );
    m.apply(Msg::Done {
        args: strings(&["restart", "web"]),
        ok: false,
        message: "boom".to_owned(),
    });
    let s = m.status.unwrap();
    assert_eq!((s.ok, s.message.as_str()), (Some(false), "boom"));
}

#[test]
fn list_errors_show_until_the_next_good_read() {
    let mut m = loaded();
    m.apply(Msg::Sandboxes(Err("no data dir".to_owned())));
    assert_eq!(m.error.as_deref(), Some("no data dir"));
    assert_eq!(m.visible_sandboxes().len(), 2, "keeps the last good list");
    m.apply(Msg::Sandboxes(Ok(vec![])));
    assert_eq!(m.error, None);
}

#[test]
fn events_follow_the_newest_unless_you_scroll_up_and_old_ones_drop() {
    let mut m = Model::default();
    for i in 0..5 {
        m.apply(Msg::Event(event("sandbox", "start", &i.to_string())));
    }
    assert_eq!(m.cursors[2].selected, 4, "follows the newest");
    m.tab = Tab::Events;
    press(&mut m, KeyCode::Up);
    m.apply(Msg::Event(event("sandbox", "stop", "5")));
    assert_eq!(m.cursors[2].selected, 3, "stays where you scrolled");
    press(&mut m, KeyCode::Char('G'));
    for i in 0..EVENTS_KEPT {
        m.apply(Msg::Event(event("image", "pull", &format!("x{i}"))));
    }
    assert_eq!(m.events.len(), EVENTS_KEPT);
    assert_eq!(m.events.front().unwrap().id, "x0");
    assert_eq!(m.cursors[2].selected, EVENTS_KEPT - 1);
}

#[test]
fn cursor_window_keeps_the_selection_visible() {
    let mut c = Cursor::default();
    assert_eq!(c.window(100, 10), 0..10);
    c.selected = 15;
    assert_eq!(c.window(100, 10), 6..16, "scrolls down just enough");
    c.selected = 3;
    assert_eq!(c.window(100, 10), 3..13, "scrolls up just enough");
    c.selected = 99;
    assert_eq!(c.window(100, 10), 90..100);
    // 줄이 줄면 고른 줄과 첫 줄을 당긴다.
    assert_eq!(c.window(5, 10), 0..5);
    assert_eq!(c.selected, 4);
    assert_eq!(c.window(0, 10), 0..0);
    assert_eq!(c.window(5, 0), 0..0);
}

#[test]
fn split_and_display_round_trip_like_a_shell() {
    for (line, want) in [
        ("ps -a", vec!["ps", "-a"]),
        ("sbx ps", vec!["ps"]),
        ("  run   alpine  ", vec!["run", "alpine"]),
        (
            "exec web sh -c 'echo \"hi\" there'",
            vec!["exec", "web", "sh", "-c", "echo \"hi\" there"],
        ),
        (r#"x "a\"b" c\ d ''"#, vec!["x", "a\"b", "c d", ""]),
    ] {
        let got = split(line).unwrap();
        assert_eq!(got, strings(&want), "{line:?}");
        // 보이는 명령 줄을 다시 나누면 같은 인자다.
        let shown = display(&got);
        assert_eq!(split(&shown).unwrap(), got, "{shown:?}");
    }
    assert_eq!(display(&strings(&["stop", "web"])), "sbx stop web");
    assert_eq!(display(&strings(&["exec", "it's"])), r"sbx exec 'it'\''s'");
    assert!(split("'open").is_err());
    assert!(split("end\\").is_err());
}

#[test]
fn help_text_lists_every_key() {
    let h = help_text();
    for k in KEYS {
        assert!(h.contains(k.keys) && h.contains(k.does), "{k:?}");
    }
}

#[test]
fn executors_tab_lists_executors_then_adapters_and_runs_executor_commands() {
    let mut m = loaded();
    assert_eq!(chars(&mut m, "4"), None);
    assert_eq!(m.tab(), Tab::Executors);
    // 아직 못 받았으면 고를 것이 없지만, 설치와 검색은 된다.
    assert_eq!(press(&mut m, KeyCode::Char('u')), None);
    assert_eq!(
        press(&mut m, KeyCode::Char('S')),
        Some(Action::Handover(strings(&["executor", "search"])))
    );
    m.apply(Msg::Executors(Ok(executors())));
    let specs: Vec<String> = m.visible_executor_rows().iter().map(|r| r.spec()).collect();
    assert_eq!(specs, ["fake@1.0", "wick@1.18.0", "firecracker@0.2.0"]);
    assert_eq!(
        press(&mut m, KeyCode::Char('u')),
        Some(Action::Run(strings(&["executor", "use", "fake@1.0"])))
    );
    // i는 고른 이름으로 설치 명령을 채우고, 고쳐 친 뒤 Enter로 터미널을 넘긴다.
    assert_eq!(press(&mut m, KeyCode::Char('i')), None);
    assert_eq!(m.prompt.as_ref().unwrap().text, "executor install fake");
    chars(&mut m, "@2.0");
    assert_eq!(
        press(&mut m, KeyCode::Enter),
        Some(Action::Handover(strings(&[
            "executor", "install", "fake@2.0"
        ])))
    );
    // 어댑터 줄: D는 adapter rm을 묻는다. u는 하지 않는다.
    press(&mut m, KeyCode::Char('G'));
    assert_eq!(press(&mut m, KeyCode::Char('u')), None);
    press(&mut m, KeyCode::Char('D'));
    assert_eq!(
        m.popup,
        Some(Popup::Confirm {
            args: strings(&["executor", "adapter", "rm", "firecracker@0.2.0"])
        })
    );
    assert_eq!(
        press(&mut m, KeyCode::Char('y')),
        Some(Action::Run(strings(&[
            "executor",
            "adapter",
            "rm",
            "firecracker@0.2.0"
        ])))
    );
    // 어댑터 줄에서 i는 이름을 비워 둔다(특정 실행기를 권하지 않는다).
    press(&mut m, KeyCode::Char('i'));
    assert_eq!(m.prompt.take().unwrap().text, "executor install ");
    press(&mut m, KeyCode::Char('P'));
    assert_eq!(
        m.popup,
        Some(Popup::Confirm {
            args: strings(&["executor", "prune"])
        })
    );
    m.popup = None;
    // 거르기와 자세히.
    chars(&mut m, "/wick");
    press(&mut m, KeyCode::Enter);
    assert_eq!(m.visible_executor_rows().len(), 1);
    press(&mut m, KeyCode::Char('g'));
    press(&mut m, KeyCode::Enter);
    assert!(
        matches!(&m.popup, Some(Popup::Detail { title, .. }) if title == "Executor wick@1.18.0")
    );
}
