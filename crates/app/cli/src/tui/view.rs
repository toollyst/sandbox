//! 그리기. [`Model`]을 화면에 옮길 뿐 상태를 정하지 않는다(표의 첫 줄만 보이는 범위에 맞춘다).
//!
//! ```text
//!  sbx   1 Sandboxes 3   2 Images 2   3 Events 41   4 Executors 3                filter: web
//! ┌ Sandboxes ───────────────────────────────────────────────────────────┐
//! │ NAME   ID            IMAGE         STATE      COMMAND    CREATED     │  표: 보이는 줄만 만든다
//! │ web    3f2a9c0d1e2b  alpine:3.20   running    sleep 1d   2 minutes   │
//! └──────────────────────────────────────────────────────────────────────┘
//! ┌ Recent events ───────────────────────────────────────────────────────┐  샌드박스 탭, 화면이 넉넉할 때
//! └──────────────────────────────────────────────────────────────────────┘
//!  ✓ sbx stop web                   s start  x stop … / filter  ? help  q quit   상태 줄(또는 입력 줄)과 키
//! ```
//!
//! 줄이 없으면 표 가운데에 무엇이 없는지와 다음에 할 일을 적는다(경고가 아니다). 거르는 글에 맞는 것이 없을 때도
//! 같다. 아래 줄의 키는 지금 할 수 있는 것만 보인다(고른 줄이 없으면 줄에 하는 키는 뺀다).

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Padding, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Table, TableState, Wrap,
};

use super::detail::DetailLine;
use super::keys::{grouped, keys_width};
use super::model::{
    Cursor, EVENTS_SINCE, ExecutorRow, Model, Popup, PromptKind, TABS, Tab, display, state_name,
};
use crate::behavior::{MIN_ID_PREFIX, familiar, human_ago, human_size};
use crate::output::{Event, Sandbox, SandboxState};

/// 샌드박스 탭에 최근 이벤트 칸을 둘 최소 높이.
const EVENTS_STRIP_MIN_HEIGHT: u16 = 24;
/// 최근 이벤트 칸의 줄 수(테두리 포함).
const EVENTS_STRIP_HEIGHT: u16 = 8;

/// 한 장을 그린다. `now_ns`는 지난 시간("2 minutes")을 셀 기준이다.
pub fn draw(frame: &mut Frame, model: &mut Model, now_ns: u64) {
    let [top, body, bottom] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    header(frame, model, top);
    match model.tab() {
        Tab::Sandboxes if body.height >= EVENTS_STRIP_MIN_HEIGHT => {
            let [list, strip] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(EVENTS_STRIP_HEIGHT)])
                    .areas(body);
            sandboxes(frame, model, list, now_ns);
            recent_events(frame, model, strip);
        }
        Tab::Sandboxes => sandboxes(frame, model, body, now_ns),
        Tab::Images => images(frame, model, body),
        Tab::Events => events(frame, model, body),
        Tab::Executors => executors(frame, model, body),
    }
    footer(frame, model, bottom);
    if let Some(p) = &mut model.popup {
        popup(frame, p);
    }
}

fn header(frame: &mut Frame, model: &Model, area: Rect) {
    let counts = [
        model.visible_sandboxes().len(),
        model.visible_images().len(),
        model.visible_events().len(),
        model.visible_executor_rows().len(),
    ];
    let mut spans = vec![Span::from(" sbx ").bold().cyan(), Span::from(" ")];
    for (i, tab) in TABS.iter().enumerate() {
        let (n, title, count) = (
            format!(" {} ", i + 1),
            tab.title(),
            format!(" {} ", counts[i]),
        );
        if *tab == model.tab() {
            let on = Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD);
            spans.extend([
                Span::styled(n, on),
                Span::styled(title, on),
                Span::styled(count, on),
            ]);
        } else {
            spans.extend([
                Span::from(n).dark_gray(),
                Span::from(title),
                Span::from(count).dark_gray(),
            ]);
        }
        spans.push(Span::from(" "));
    }
    frame.render_widget(Line::from(spans), area);
    if !model.filter.is_empty() {
        let f = Line::from(vec![
            Span::from("filter: ").dark_gray(),
            Span::from(format!("{} ", model.filter)).yellow(),
        ])
        .right_aligned();
        frame.render_widget(f, area);
    }
}

/// 줄이 없을 때 표 가운데에 적는 것: 무엇이 없는지와 다음에 할 일.
struct Empty<'a> {
    what: String,
    next: Line<'a>,
}

impl<'a> Empty<'a> {
    fn new(what: impl Into<String>, next: Line<'a>) -> Self {
        Self {
            what: what.into(),
            next,
        }
    }
}

/// 안내 한 줄. 짝수 번째 조각(0부터)은 그냥 글, 홀수 번째는 키나 명령이라 굵게.
fn hint(parts: &[&'static str]) -> Line<'static> {
    Line::from(
        parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if i % 2 == 1 {
                    Span::from(*p).bold()
                } else {
                    Span::from(*p).dark_gray()
                }
            })
            .collect::<Vec<_>>(),
    )
}

/// `area` 가운데에 제목 한 줄과 안내 한 줄.
fn centered_message(frame: &mut Frame, area: Rect, what: &str, next: Line) {
    let mut lines = vec![Line::from(what.to_owned()).bold()];
    if !next.spans.is_empty() {
        lines.push(Line::from(""));
        lines.push(next);
    }
    let [v] = Layout::vertical([Constraint::Length(lines.len() as u16)])
        .flex(Flex::Center)
        .areas(area);
    frame.render_widget(Paragraph::new(lines).centered(), v);
}

/// 표 하나의 모양.
struct Spec<'a> {
    title: &'a str,
    /// 줄 수(거른 뒤).
    len: usize,
    /// 아직 한 번도 못 읽었으면 `false`.
    loaded: bool,
    header: &'a [&'a str],
    widths: &'a [Constraint],
    /// 거르는 글(`/`). 맞는 것이 없을 때의 안내가 달라진다.
    filter: &'a str,
    /// 줄이 하나도 없을 때(거르지 않았는데).
    empty: Empty<'a>,
}

/// 테두리 안에 머리줄과 보이는 줄만 담은 표, 오른쪽에 스크롤 막대. `rows`는 범위를 받아 그 줄만 만든다
/// (줄이 만 개여도 한 장에 만드는 것은 보이는 만큼이다).
fn table<'a>(
    frame: &mut Frame,
    area: Rect,
    cursor: &mut Cursor,
    spec: Spec<'a>,
    rows: impl FnOnce(std::ops::Range<usize>) -> Vec<Row<'a>>,
) {
    let Spec {
        title,
        len,
        loaded,
        header,
        widths,
        filter,
        empty,
    } = spec;
    let block = Block::bordered()
        .title(format!(" {title} "))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if !loaded {
        centered_message(frame, inner, "Loading…", Line::from(""));
        return;
    }
    if len == 0 {
        if filter.is_empty() {
            centered_message(frame, inner, &empty.what, empty.next);
        } else {
            let what = format!("Nothing matches “{filter}”");
            centered_message(
                frame,
                inner,
                &what,
                hint(&["Press ", "Esc", " to clear the filter"]),
            );
        }
        return;
    }
    // 머리줄 한 줄을 뺀 만큼이 보인다.
    let range = cursor.window(len, inner.height.saturating_sub(1) as usize);
    let offset = range.start;
    let t = Table::new(rows(range), widths.to_vec())
        .header(Row::new(header.iter().copied()).bold().dark_gray())
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .column_spacing(2);
    let mut state = TableState::new().with_selected(Some(cursor.selected - offset));
    frame.render_stateful_widget(t, inner, &mut state);
    if len > inner.height.saturating_sub(1) as usize {
        let mut sb = ScrollbarState::new(len).position(cursor.selected);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut sb,
        );
    }
}

/// 실행기가 하나도 없고 고를 수도 없다(PATH에도 없다). 샌드박스를 만들기 전에 설치해야 한다.
fn no_executor(model: &Model) -> bool {
    model
        .executors
        .as_ref()
        .is_some_and(|x| x.executors.is_empty() && x.current.is_err())
}

fn sandboxes(frame: &mut Frame, model: &mut Model, area: Rect, now_ns: u64) {
    // 표 위치는 꺼내 쓰고 돌려놓는다(줄은 모델을 빌린 채로 그린다).
    let mut cursor = *model.cursor(Tab::Sandboxes);
    let rows = model.visible_sandboxes();
    // 이름과 이미지 칸은 내용만큼(넘치면 자른다). 남는 자리는 명령 칸이 갖는다.
    let name_w = fit(
        rows.iter()
            .map(|s| s.name.as_deref().unwrap_or("").chars().count()),
        4,
        24,
    );
    let image_w = fit(rows.iter().map(|s| image_label(s).chars().count()), 5, 32);
    let widths = [
        Constraint::Length(name_w),
        Constraint::Length(MIN_ID_PREFIX as u16),
        Constraint::Length(image_w),
        Constraint::Length(14),
        Constraint::Fill(1),
        // "Less than a second ago"가 다 들어간다.
        Constraint::Length(22),
    ];
    let next = if no_executor(model) {
        hint(&[
            "Install an executor first: press ",
            "4",
            " for Executors, then ",
            "S",
            " to search",
        ])
    } else {
        hint(&[
            "Press ",
            ":",
            " and type ",
            "run -it alpine sh",
            " to start one",
        ])
    };
    let spec = Spec {
        title: "Sandboxes",
        len: rows.len(),
        loaded: model.sandboxes.is_some(),
        header: &["NAME", "ID", "IMAGE", "STATE", "COMMAND", "CREATED"],
        widths: &widths,
        filter: &model.filter,
        empty: Empty::new("No sandboxes yet", next),
    };
    table(frame, area, &mut cursor, spec, |range| {
        rows[range]
            .iter()
            .map(|s| {
                Row::new(vec![
                    Cell::from(s.name.clone().unwrap_or_default()),
                    Cell::from(short(&s.id).to_owned()),
                    Cell::from(image_label(s).to_owned()),
                    Cell::from(state(s)),
                    Cell::from(s.process.args.join(" ")),
                    Cell::from(created(s, now_ns)),
                ])
            })
            .collect()
    });
    *model.cursor(Tab::Sandboxes) = cursor;
}

fn images(frame: &mut Frame, model: &mut Model, area: Rect) {
    let mut cursor = *model.cursor(Tab::Images);
    let rows = model.visible_images();
    let spec = Spec {
        title: "Images",
        len: rows.len(),
        loaded: model.images.is_some(),
        header: &["REFERENCE", "ID", "ARCH", "SIZE", "PREPARED"],
        widths: &[
            Constraint::Fill(1),
            Constraint::Length(MIN_ID_PREFIX as u16),
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Max(20),
        ],
        filter: &model.filter,
        empty: Empty::new(
            "No images yet",
            hint(&[
                "run pulls what it needs, or press ",
                ":",
                " and type ",
                "pull IMAGE",
            ]),
        ),
    };
    table(frame, area, &mut cursor, spec, |range| {
        rows[range]
            .iter()
            .map(|i| {
                let refs: Vec<&str> = i.references.iter().map(|r| familiar(r)).collect();
                Row::new(vec![
                    Cell::from(if refs.is_empty() {
                        "<none>".to_owned()
                    } else {
                        refs.join(", ")
                    }),
                    Cell::from(short(&i.id).to_owned()),
                    Cell::from(i.arch.clone()),
                    Cell::from(human_size(i.compressed_size)),
                    Cell::from(i.prepared.join(", ")),
                ])
            })
            .collect()
    });
    *model.cursor(Tab::Images) = cursor;
}

/// 실행기 탭. 위 한 줄은 새 샌드박스가 쓸 실행기와 그 까닭(`executor current`), 아래는 실행기와 어댑터 표.
/// `*`는 기본, `>`는 지금 쓸 것이다.
fn executors(frame: &mut Frame, model: &mut Model, area: Rect) {
    let current = model.executors.as_ref().map(|x| &x.current);
    // 위 줄은 알릴 것이 있을 때만: 지금 쓸 것, 또는 설치한 것이 있는데 고를 수 없는 까닭.
    // 아무것도 설치하지 않았으면 표 가운데의 안내가 그 말을 한다(경고가 아니다).
    let line = match current {
        Some(Ok(c)) => {
            let what = match (&c.name, &c.version) {
                (Some(n), Some(v)) => format!("{n}@{v}"),
                _ => c.program.clone(),
            };
            let mut spans = vec![
                Span::from(" current: ").dark_gray(),
                Span::from(what).bold(),
                Span::from(format!("  · {}", c.source)).dark_gray(),
            ];
            if let Some(a) = &c.adapter {
                spans.push(Span::from(format!("  · adapter {a}")).dark_gray());
            }
            Some(Line::from(spans))
        }
        Some(Err(e)) if !no_executor(model) => {
            Some(Line::from(format!(" current: none ({e})")).yellow())
        }
        _ => None,
    };
    let list = match line {
        Some(line) => {
            let [top, list] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            frame.render_widget(line, top);
            list
        }
        None => area,
    };
    let current = current
        .and_then(|c| c.as_ref().ok())
        .and_then(|c| Some(format!("{}@{}", c.name.as_ref()?, c.version.as_ref()?)));
    let mut cursor = *model.cursor(Tab::Executors);
    let rows = model.visible_executor_rows();
    // 칸마다 내용만큼(넘치면 자른다). 남는 자리는 경로 칸이 갖는다.
    let cells: Vec<[String; 7]> = rows
        .iter()
        .map(|r| executor_cells(r, current.as_deref()))
        .collect();
    let w = |i: usize, min: usize, max: usize| {
        fit(cells.iter().map(|c| c[i].chars().count()), min, max)
    };
    let widths = [
        Constraint::Length(1),
        Constraint::Length(8),
        Constraint::Length(w(2, 4, 24)),
        Constraint::Length(w(3, 7, 24)),
        Constraint::Length(w(4, 17, 32)),
        Constraint::Length(w(5, 4, 16)),
        Constraint::Fill(1),
    ];
    let spec = Spec {
        title: "Executors and adapters",
        len: rows.len(),
        loaded: model.executors.is_some(),
        header: &[
            "",
            "KIND",
            "NAME",
            "VERSION",
            "ADAPTER / USED BY",
            "FROM",
            "PATH",
        ],
        widths: &widths,
        filter: &model.filter,
        empty: Empty::new(
            "No executors installed",
            hint(&[
                "Press ",
                "S",
                " to search the repositories, or ",
                "i",
                " to install one",
            ]),
        ),
    };
    table(frame, list, &mut cursor, spec, |range| {
        cells[range]
            .iter()
            .map(|c| Row::new(c.iter().cloned().map(Cell::from)))
            .collect()
    });
    *model.cursor(Tab::Executors) = cursor;
}

/// 실행기 탭 한 줄의 칸: 표시(`>` 지금 쓸 것, `*` 기본), 종류, 이름, 버전, 어댑터나 그것을 쓰는 VMM 묶음, 출처, 경로.
fn executor_cells(r: &ExecutorRow, current: Option<&str>) -> [String; 7] {
    let dash = |v: Option<String>| v.unwrap_or_else(|| "-".into());
    match r {
        ExecutorRow::Executor(e) => {
            let mark = if current == Some(r.spec().as_str()) {
                ">"
            } else if e.default {
                "*"
            } else {
                ""
            };
            [
                mark.into(),
                "executor".into(),
                e.name.clone(),
                e.version.clone(),
                dash(e.adapter.clone()),
                dash(e.origin.clone()),
                e.path.clone(),
            ]
        }
        ExecutorRow::Adapter(a) => [
            String::new(),
            "adapter".into(),
            a.name.clone(),
            a.version.clone(),
            dash((!a.used_by.is_empty()).then(|| a.used_by.join(", "))),
            dash(a.origin.clone()),
            a.path.clone(),
        ],
    }
}

const EVENT_HEADER: [&str; 5] = ["TIME (UTC)", "TYPE", "ACTION", "ID", "ATTRIBUTES"];
const EVENT_WIDTHS: [Constraint; 5] = [
    Constraint::Length(10),
    Constraint::Length(7),
    Constraint::Length(7),
    Constraint::Length(MIN_ID_PREFIX as u16),
    Constraint::Fill(1),
];

/// 속성: `name`을 먼저, 이미지 ID(`sha256:…`)는 앞 12자리로.
fn attributes(e: &Event) -> String {
    let mut out: Vec<String> = e
        .attributes
        .get("name")
        .map(|n| format!("name={n}"))
        .into_iter()
        .collect();
    for (k, v) in e.attributes.iter().filter(|(k, _)| *k != "name") {
        let v = if v.starts_with("sha256:") {
            short(v)
        } else {
            v.as_str()
        };
        out.push(format!("{k}={v}"));
    }
    out.join(" ")
}

fn event_row(e: &Event) -> Row<'static> {
    Row::new(vec![
        Cell::from(clock(&e.time).to_owned()),
        Cell::from(e.kind.clone()),
        Cell::from(e.action.clone()).style(action_style(&e.action)),
        Cell::from(short(&e.id).to_owned()),
        Cell::from(attributes(e)),
    ])
}

fn events(frame: &mut Frame, model: &mut Model, area: Rect) {
    let mut cursor = *model.cursor(Tab::Events);
    let rows = model.visible_events();
    if !model.scrolled_back {
        cursor.selected = rows.len().saturating_sub(1);
    }
    let spec = Spec {
        title: "Events",
        len: rows.len(),
        loaded: true,
        header: &EVENT_HEADER,
        widths: &EVENT_WIDTHS,
        filter: &model.filter,
        empty: Empty::new(
            format!("No events in the last {}", since_words()),
            hint(&["New ones show up here as they happen"]),
        ),
    };
    table(frame, area, &mut cursor, spec, |range| {
        rows[range].iter().map(|e| event_row(e)).collect()
    });
    *model.cursor(Tab::Events) = cursor;
}

/// 샌드박스 탭 아래의 최근 이벤트(거르지 않는다, 고를 수 없다).
fn recent_events(frame: &mut Frame, model: &Model, area: Rect) {
    let block = Block::bordered()
        .title(" Recent events ")
        .padding(Padding::horizontal(1))
        .dark_gray();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if model.events.is_empty() {
        let what = format!("No events in the last {}", since_words());
        let [v] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(inner);
        frame.render_widget(Paragraph::new(what).centered().dark_gray(), v);
        return;
    }
    let n = inner.height.saturating_sub(1) as usize;
    let start = model.events.len().saturating_sub(n);
    let rows: Vec<Row> = model.events.range(start..).map(event_row).collect();
    let t = Table::new(rows, EVENT_WIDTHS)
        .header(Row::new(EVENT_HEADER).bold())
        .column_spacing(2);
    frame.render_widget(t, inner);
}

/// `events --since`의 기간을 말로(`1h` → `hour`).
fn since_words() -> &'static str {
    match EVENTS_SINCE {
        "1h" => "hour",
        other => other,
    }
}

/// 지금 탭에서 할 수 있는 키(키, 하는 일). 줄에 하는 키는 고른 줄이 있을 때만.
fn hints(model: &Model) -> Vec<(&'static str, &'static str)> {
    let mut h = Vec::new();
    match model.tab() {
        Tab::Sandboxes if !model.visible_sandboxes().is_empty() => h.extend([
            ("s", "start"),
            ("x", "stop"),
            ("e", "shell"),
            ("l", "logs"),
            ("D", "rm"),
            ("enter", "details"),
        ]),
        Tab::Images if !model.visible_images().is_empty() => {
            h.extend([("D", "rmi"), ("enter", "details")])
        }
        Tab::Events if !model.visible_events().is_empty() => h.push(("enter", "details")),
        Tab::Executors => {
            let rows = model.visible_executor_rows();
            let i = model.cursors[3].selected.min(rows.len().saturating_sub(1));
            let executor = matches!(rows.get(i), Some(ExecutorRow::Executor(_)));
            if executor {
                h.push(("u", "use"));
            }
            h.extend([("i", "install"), ("S", "search")]);
            if !rows.is_empty() {
                h.extend([("D", "rm"), ("P", "prune")]);
            }
        }
        _ => {}
    }
    h.extend([
        ("/", "filter"),
        (":", "command"),
        ("?", "help"),
        ("q", "quit"),
    ]);
    h
}

fn footer(frame: &mut Frame, model: &Model, area: Rect) {
    if let Some(p) = &model.prompt {
        let lead = match p.kind {
            PromptKind::Filter => "/",
            PromptKind::Command => ":sbx ",
        };
        let line = Line::from(vec![Span::from(lead).bold(), Span::from(p.text.as_str())]);
        frame.render_widget(line, area);
        let x = area.x + (lead.chars().count() + p.text.chars().count()) as u16;
        frame.set_cursor_position((x.min(area.right().saturating_sub(1)), area.y));
        return;
    }
    let left = if let Some(e) = &model.error {
        Line::from(format!(" ✗ {e}")).red()
    } else if let Some(s) = &model.status {
        let (mark, style) = match s.ok {
            None => ("…", Style::new().yellow()),
            Some(true) => ("✓", Style::new().green()),
            Some(false) => ("✗", Style::new().red()),
        };
        let mut spans = vec![
            Span::styled(format!(" {mark} "), style),
            Span::from(display(&s.args)),
        ];
        if !s.message.is_empty() {
            spans.push(Span::styled(format!(": {}", s.message), style));
        }
        Line::from(spans)
    } else {
        Line::from("")
    };
    // 키는 오른쪽에, 상태 줄과 겹치지 않을 만큼만(앞의 것부터 뺀다: 끝의 help와 quit는 남는다).
    let room = (area.width as usize).saturating_sub(left.width() + 2);
    let mut keys = hints(model);
    let width = |k: &[(&str, &str)]| -> usize {
        k.iter()
            .map(|(a, b)| a.chars().count() + 1 + b.len() + 2)
            .sum::<usize>()
    };
    while !keys.is_empty() && width(&keys) > room {
        keys.remove(0);
    }
    let mut spans = Vec::new();
    for (k, what) in keys {
        spans.push(Span::from(k).bold());
        spans.push(Span::from(format!(" {what}  ")).dark_gray());
    }
    frame.render_widget(Line::from(spans).right_aligned(), area);
    frame.render_widget(left, area);
}

fn popup(frame: &mut Frame, p: &mut Popup) {
    let area = frame.area();
    match p {
        Popup::Help { scroll } => {
            let w = keys_width();
            let mut lines: Vec<Line> = Vec::new();
            for (title, keys) in grouped() {
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(format!(" {title}")).bold().cyan());
                for h in keys {
                    lines.push(Line::from(vec![
                        Span::from(format!("   {:<w$}  ", h.keys)).bold(),
                        Span::from(h.does),
                    ]));
                }
            }
            let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 5;
            scrolling(
                frame,
                area,
                " Keys ",
                lines,
                width,
                scroll,
                " any key closes ",
            );
        }
        Popup::Confirm { args } => {
            let r = centered(area, (display(args).len() as u16 + 6).max(40), 5);
            frame.render_widget(Clear, r);
            let text = vec![
                Line::from(""),
                Line::from(format!(" {}", display(args))).bold(),
                Line::from(vec![
                    Span::from(" y").bold(),
                    Span::from(" run   ").dark_gray(),
                    Span::from("any other key").bold(),
                    Span::from(" cancel").dark_gray(),
                ]),
            ];
            frame.render_widget(
                Paragraph::new(text).block(Block::bordered().title(" Run? ").yellow()),
                r,
            );
        }
        Popup::Detail {
            title,
            lines,
            source,
            scroll,
        } => {
            let body: Vec<Line> = lines.iter().map(detail_line).collect();
            let footer = format!(" same as: {source} ");
            let content_w = body.iter().map(Line::width).max().unwrap_or(0);
            let width = (content_w.max(footer.chars().count()) as u16 + 4).max(50);
            let title = format!(" {title} ");
            scrolling(frame, area, &title, body, width, scroll, &footer);
        }
    }
}

/// 가운데에 뜨는 창: 내용만큼 크고(화면보다 크면 화면에서 조금 작게), 넘치면 내리고 스크롤 막대를 둔다.
/// 넘칠 때는 아래 제목에 내리는 키를 적는다.
fn scrolling(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    body: Vec<Line>,
    width: u16,
    scroll: &mut u16,
    footer: &str,
) {
    let total = body.len();
    let width = width.min(area.width.saturating_sub(4));
    let height = (total as u16 + 2).min(area.height.saturating_sub(2));
    let r = centered(area, width, height);
    frame.render_widget(Clear, r);
    let visible = r.height.saturating_sub(2) as usize;
    // 끝을 넘어 내리지 않는다.
    *scroll = (*scroll).min(total.saturating_sub(visible) as u16);
    let overflow = total > visible;
    let bottom = if overflow {
        format!(" ↑↓ scroll ·{footer}")
    } else {
        footer.to_owned()
    };
    frame.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll((*scroll, 0))
            .block(
                Block::bordered()
                    .title(title.to_owned())
                    .title_bottom(Line::from(bottom).dark_gray())
                    .padding(Padding::horizontal(1)),
            ),
        r,
    );
    if overflow {
        let mut sb = ScrollbarState::new(total.saturating_sub(visible)).position(*scroll as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            r,
            &mut sb,
        );
    }
}

/// 상세 창의 줄 하나: 단계마다 두 칸, 목록 항목은 `- `, 이름은 굵게, 값은 형제끼리 한 칸에.
fn detail_line(l: &DetailLine) -> Line<'static> {
    let mut indent = "  ".repeat(l.depth as usize);
    if l.bullet {
        indent.replace_range(indent.len().saturating_sub(2).., "- ");
    }
    let mut spans = vec![Span::from(indent).dark_gray()];
    if !l.key.is_empty() {
        spans.push(Span::from(l.key.clone()).bold());
        if !l.value.is_empty() {
            let pad = l.key_width.saturating_sub(l.key.chars().count()) + 2;
            spans.push(Span::from(" ".repeat(pad)));
        }
    }
    spans.push(Span::from(l.value.clone()));
    Line::from(spans)
}

/// 가운데에 `width`×`height`(화면보다 크면 화면만큼).
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    let [r] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(v);
    r
}

/// 칸 너비: 가장 긴 것(머리줄 `min` 이상, `max` 이하).
fn fit(lens: impl Iterator<Item = usize>, min: usize, max: usize) -> u16 {
    lens.fold(min, usize::max).min(max) as u16
}

fn image_label(s: &Sandbox) -> &str {
    s.image
        .reference
        .as_deref()
        .map(familiar)
        .unwrap_or(short(&s.image.id))
}

/// ID 앞 12자리(`sha256:`은 뗀다).
fn short(id: &str) -> &str {
    let hex = id.strip_prefix("sha256:").unwrap_or(id);
    &hex[..hex.len().min(MIN_ID_PREFIX)]
}

/// RFC 3339 시각에서 시:분:초.
fn clock(time: &str) -> &str {
    time.split_once('T')
        .map(|(_, t)| &t[..t.len().min(8)])
        .unwrap_or(time)
}

fn state(s: &Sandbox) -> Span<'static> {
    let name = state_name(s.state);
    match s.state {
        SandboxState::Running => Span::from(name).green(),
        SandboxState::Creating => Span::from(name).cyan(),
        SandboxState::Created => Span::from(name).yellow(),
        SandboxState::Stopped => match s.exit_code {
            Some(0) => Span::from(format!("{name} (0)")).dark_gray(),
            Some(c) => Span::from(format!("{name} ({c})")).red(),
            None if s.error.is_some() => Span::from(format!("{name} (error)")).red(),
            None => Span::from(name).dark_gray(),
        },
    }
}

fn action_style(action: &str) -> Style {
    match action {
        "start" | "pull" | "prepare" => Style::new().fg(Color::Green),
        "stop" | "remove" | "delete" | "untag" => Style::new().fg(Color::Red),
        _ => Style::new(),
    }
}

fn created(s: &Sandbox, now_ns: u64) -> String {
    let Some(at) = s
        .created
        .as_deref()
        .and_then(|c| crate::parse::time_ns(c, now_ns).ok())
    else {
        return String::new();
    };
    human_ago(Duration::from_nanos(now_ns.saturating_sub(at)))
}

#[cfg(test)]
mod tests;
