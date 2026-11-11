use super::*;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use crate::tui::model::tests::{event, executors, loaded};
use crate::tui::model::{Msg, Popup};

/// 2026-11-10T12:02:00Z: 고정한 샌드박스 생성 시각의 2분 뒤.
const NOW: u64 = 1_794_312_120_000_000_000;

fn render(m: &mut Model, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw(f, m, NOW)).unwrap();
    let buf = t.backend().buffer();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn sandboxes_tab_shows_rows_hints_and_recent_events() {
    let mut m = loaded();
    m.apply(Msg::Event(event("sandbox", "start", &"1".repeat(64))));
    let s = render(&mut m, 120, 30);
    assert!(s.contains("1 Sandboxes 2"), "{s}");
    assert!(s.contains("2 Images 1") && s.contains("3 Events 1"), "{s}");
    for want in [
        "NAME",
        "web",
        "111111111111",
        "alpine:3.20",
        "running",
        "sleep 1d",
    ] {
        assert!(s.contains(want), "{want}\n{s}");
    }
    assert!(s.contains("stopped (0)"), "{s}");
    assert!(s.contains("2 minutes ago"), "{s}");
    assert!(s.contains("Recent events") && s.contains("13:20:14"), "{s}");
    assert!(s.contains("s start  x stop"), "{s}");
    // 낮은 화면에는 최근 이벤트 칸이 없다.
    assert!(!render(&mut m, 120, 12).contains("Recent events"));
}

#[test]
fn only_visible_rows_are_drawn_and_the_selection_stays_on_screen() {
    let mut m = crate::tui::model::Model::default();
    for i in 0..500 {
        m.apply(Msg::Event(event("image", "pull", &format!("id{i:04}"))));
    }
    m.tab = Tab::Events;
    let s = render(&mut m, 100, 20);
    // 새 것을 따라가므로 마지막 줄이 보이고 처음 것은 보이지 않는다.
    assert!(s.contains("id0499") && !s.contains("id0000 "), "{s}");
    for _ in 0..495 {
        m.key(KeyEvent::from(KeyCode::Up));
    }
    let s = render(&mut m, 100, 20);
    assert!(s.contains("id0004") && !s.contains("id0499"), "{s}");
    // 표 칸(테두리, 머리줄 뺀 15줄)만큼만 그렸다.
    let c = m.cursors[2];
    assert_eq!((c.selected, c.offset), (4, 4));
}

#[test]
fn status_line_shows_the_command_line_and_its_result() {
    let mut m = loaded();
    m.key(KeyEvent::from(KeyCode::Char('x')));
    assert!(render(&mut m, 100, 10).contains("… sbx stop web"));
    m.apply(Msg::Done {
        args: vec!["stop".into(), "web".into()],
        ok: false,
        message: "no such sandbox".into(),
    });
    assert!(render(&mut m, 100, 10).contains("✗ sbx stop web: no such sandbox"));
}

#[test]
fn popups_draw_over_the_table() {
    let mut m = loaded();
    m.key(KeyEvent::from(KeyCode::Char('D')));
    let s = render(&mut m, 100, 20);
    assert!(s.contains("Run?") && s.contains("sbx rm -f web"), "{s}");
    m.popup = Some(Popup::Help { scroll: 0 });
    let s = render(&mut m, 120, 30);
    assert!(s.contains("Keys") && s.contains("exec -it ID sh"), "{s}");
    m.popup = None;
    m.key(KeyEvent::from(KeyCode::Enter));
    let s = render(&mut m, 100, 20);
    assert!(
        s.contains("rootfs_format") && s.contains("Sandbox web"),
        "{s}"
    );
    // 끝을 넘어 내리지 않는다.
    for _ in 0..500 {
        m.key(KeyEvent::from(KeyCode::Down));
    }
    render(&mut m, 100, 20);
    let Some(Popup::Detail { scroll, lines, .. }) = &m.popup else {
        panic!()
    };
    assert!((*scroll as usize) < lines.len());
}

#[test]
fn tiny_terminals_do_not_panic() {
    let mut m = loaded();
    for (w, h) in [(1, 1), (10, 3), (20, 5), (0, 0)] {
        render(&mut m, w, h);
        m.popup = Some(Popup::Help { scroll: 0 });
        render(&mut m, w, h);
        m.popup = None;
    }
}

#[test]
fn events_keep_following_the_newest_across_filter_changes() {
    let mut m = crate::tui::model::Model::default();
    for i in 0..100 {
        let mut e = event("sandbox", "start", &format!("id{i:04}"));
        e.attributes
            .insert("image".into(), format!("sha256:{}", "ef".repeat(32)));
        m.apply(Msg::Event(e));
    }
    m.tab = Tab::Events;
    // 거르면 맞는 것이 하나뿐이고, 풀면 다시 맨 아래(가장 새 것)다.
    m.filter = "id0003".into();
    render(&mut m, 100, 20);
    m.filter.clear();
    let s = render(&mut m, 100, 20);
    assert!(s.contains("id0099"), "{s}");
    // 이름이 먼저, 이미지 ID는 12자리로 줄인다.
    assert!(s.contains("name=web image=efefefefefef "), "{s}");
}

#[test]
fn executors_tab_shows_current_marks_and_adapters() {
    let mut m = loaded();
    m.tab = Tab::Executors;
    assert!(render(&mut m, 120, 20).contains("Loading…"));
    m.apply(Msg::Executors(Ok(executors())));
    let s = render(&mut m, 140, 20);
    assert!(s.contains("4 Executors 3"), "{s}");
    assert!(
        s.contains("current: wick@1.18.0") && s.contains("adapter firecracker@0.2.0"),
        "{s}"
    );
    // 지금 쓸 것은 >, 어댑터 줄에는 그것을 쓰는 VMM 묶음.
    let wick = s.lines().find(|l| l.contains("/d/executors/wick")).unwrap();
    assert!(wick.contains(">") && wick.contains("firecracker"), "{wick}");
    // 어디서 받았는지(FROM).
    assert!(s.contains("FROM") && wick.contains("official"), "{wick}");
    let adapter = s
        .lines()
        .find(|l| l.contains("/d/adapters/firecracker"))
        .unwrap();
    assert!(adapter.contains("wick@1.18.0"), "{adapter}");
    assert!(s.contains("u use  i install"), "{s}");
    // 설치한 것이 없으면 경고 줄 없이 표 가운데에 무엇이 없는지와 할 일을 적는다.
    let mut x = executors();
    x.executors.clear();
    x.adapters.clear();
    x.current = Err(crate::choose::NO_EXECUTOR.into());
    m.apply(Msg::Executors(Ok(x.clone())));
    let s = render(&mut m, 140, 20);
    assert!(s.contains("No executors installed"), "{s}");
    assert!(
        s.contains("Press S to search the repositories, or i to install one"),
        "{s}"
    );
    assert!(
        !s.contains("current:") && !s.contains("no executor is installed"),
        "{s}"
    );
    // 고른 줄이 없으면 줄에 하는 키(u, D)와 prune은 아래 줄에 없다.
    assert!(
        s.contains("i install  S search  / filter") && !s.contains("u use"),
        "{s}"
    );
    // 그때 샌드박스 탭은 실행기부터 설치하라고 한다.
    m.sandboxes = Some(Vec::new());
    m.tab = Tab::Sandboxes;
    let s = render(&mut m, 140, 30);
    assert!(
        s.contains("No sandboxes yet") && s.contains("Install an executor first"),
        "{s}"
    );
    assert!(!s.contains("s start"), "{s}");
    // 설치한 것이 있는데 고를 수 없으면 그 까닭은 보인다.
    x.executors = executors().executors;
    m.apply(Msg::Executors(Ok(x)));
    m.tab = Tab::Executors;
    let s = render(&mut m, 140, 20);
    assert!(s.contains("current: none (no executor is installed"), "{s}");
}

#[test]
fn empty_lists_say_so_in_the_middle_and_filters_say_what_did_not_match() {
    let mut m = crate::tui::model::Model {
        tab: Tab::Images,
        ..Default::default()
    };
    assert!(render(&mut m, 100, 20).contains("Loading…"));
    m.apply(Msg::Images(Ok(Vec::new())));
    let s = render(&mut m, 100, 21);
    let row = s.lines().position(|l| l.contains("No images yet")).unwrap();
    // 표 칸(머리줄 1, 테두리 2줄을 뺀 19줄)의 가운데쯤, 가로도 가운데.
    assert!(
        (8..=11).contains(&row),
        "{row}
{s}"
    );
    let line = s.lines().nth(row).unwrap();
    assert!(line.find("No images").unwrap() > 30, "{line}");
    m.tab = Tab::Events;
    assert!(render(&mut m, 100, 20).contains("No events in the last hour"));
    let mut m = loaded();
    m.filter = "nope".into();
    let s = render(&mut m, 100, 20);
    assert!(
        s.contains("Nothing matches “nope”") && s.contains("Press Esc to clear the filter"),
        "{s}"
    );
}

#[test]
fn details_are_readable_and_name_the_same_command() {
    let mut m = loaded();
    m.key(KeyEvent::from(KeyCode::Enter));
    let s = render(&mut m, 120, 40);
    assert!(
        s.contains("Sandbox web") && s.contains("same as: sbx inspect web --format json"),
        "{s}"
    );
    assert!(
        s.contains("reference  docker.io/library/alpine:3.20"),
        "{s}"
    );
    assert!(!s.contains('{') && !s.contains("\"id\""), "{s}");
}

#[test]
fn help_scrolls_to_every_group_on_a_short_screen() {
    let mut m = loaded();
    m.key(KeyEvent::from(KeyCode::Char('?')));
    let s = render(&mut m, 120, 20);
    assert!(s.contains("Everywhere") && s.contains("↑↓ scroll"), "{s}");
    assert!(!s.contains("executor prune"), "{s}");
    for _ in 0..100 {
        m.key(KeyEvent::from(KeyCode::Down));
    }
    let s = render(&mut m, 120, 20);
    assert!(
        s.contains("Executors") && s.contains("executor prune"),
        "{s}"
    );
    m.key(KeyEvent::from(KeyCode::Char('x')));
    assert_eq!(m.popup, None);
}
