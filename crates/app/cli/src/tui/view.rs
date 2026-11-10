//! 그리기. [`Model`]을 화면에 옮길 뿐 상태를 정하지 않는다(표의 첫 줄만 보이는 범위에 맞춘다).
//!
//! ```text
//!  sbx  1 Sandboxes 3 │ 2 Images 2 │ 3 Events 41 │ 4 Executors 3  filter: web
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │ NAME   ID            IMAGE         STATE      COMMAND    CREATED    │  표: 보이는 줄만 만든다
//! │ web    3f2a9c0d1e2b  alpine:3.20   running    sleep 1d   2 minutes  │
//! └──────────────────────────────────────────────────────────────────────┘
//! ┌ Recent events ───────────────────────────────────────────────────────┐  샌드박스 탭, 화면이 넉넉할 때
//! └──────────────────────────────────────────────────────────────────────┘
//!  ✓ sbx stop web                                          ? help  q quit   상태 줄 또는 입력 줄
//! ```

use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Cell, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table,
    TableState, Wrap,
};

use super::keys::KEYS;
use super::model::{Cursor, ExecutorRow, Model, Popup, PromptKind, TABS, Tab, display, state_name};
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
    let mut spans = vec![Span::from(" sbx ").bold().reversed(), Span::from(" ")];
    for (i, tab) in TABS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::from(" │ ").dark_gray());
        }
        let text = format!("{} {} {}", i + 1, tab.title(), counts[i]);
        spans.push(if *tab == model.tab() {
            Span::from(text).bold().underlined()
        } else {
            Span::from(text)
        });
    }
    frame.render_widget(Line::from(spans), area);
    if !model.filter.is_empty() {
        let f = Line::from(format!("filter: {} ", model.filter))
            .yellow()
            .right_aligned();
        frame.render_widget(f, area);
    }
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
    } = spec;
    let block = Block::bordered().title(format!(" {title} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if !loaded {
        frame.render_widget(Paragraph::new("loading…").dark_gray(), inner);
        return;
    }
    if len == 0 {
        frame.render_widget(Paragraph::new("nothing here").dark_gray(), inner);
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
    let spec = Spec {
        title: "Sandboxes",
        len: rows.len(),
        loaded: model.sandboxes.is_some(),
        header: &["NAME", "ID", "IMAGE", "STATE", "COMMAND", "CREATED"],
        widths: &widths,
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
    let [top, list] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    let current = model.executors.as_ref().map(|x| &x.current);
    let line = match current {
        None => Line::from(" current: …").dark_gray(),
        Some(Ok(c)) => {
            let what = match (&c.name, &c.version) {
                (Some(n), Some(v)) => format!("{n}@{v}"),
                _ => c.program.clone(),
            };
            let mut spans = vec![
                Span::from(" current: ").dark_gray(),
                Span::from(what).bold(),
                Span::from(format!("  ({})", c.source)).dark_gray(),
            ];
            if let Some(a) = &c.adapter {
                spans.push(Span::from(format!("  adapter {a}")).dark_gray());
            }
            Line::from(spans)
        }
        Some(Err(e)) => Line::from(format!(" current: {e}")).yellow(),
    };
    frame.render_widget(line, top);
    let current = current
        .and_then(|c| c.as_ref().ok())
        .and_then(|c| Some(format!("{}@{}", c.name.as_ref()?, c.version.as_ref()?)));
    let mut cursor = *model.cursor(Tab::Executors);
    let rows = model.visible_executor_rows();
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
        widths: &[
            Constraint::Length(2),
            Constraint::Length(8),
            Constraint::Max(20),
            Constraint::Max(20),
            Constraint::Max(28),
            Constraint::Max(12),
            Constraint::Fill(1),
        ],
    };
    table(frame, list, &mut cursor, spec, |range| {
        rows[range]
            .iter()
            .map(|r| {
                let mark = match r {
                    ExecutorRow::Executor(e) => {
                        let now = current.as_deref() == Some(r.spec().as_str());
                        match (e.default, now) {
                            (_, true) => ">",
                            (true, false) => "*",
                            _ => "",
                        }
                    }
                    ExecutorRow::Adapter(_) => "",
                };
                let (kind, name, version, link, origin, path) = match r {
                    ExecutorRow::Executor(e) => (
                        "executor",
                        &e.name,
                        &e.version,
                        e.adapter.clone().unwrap_or_else(|| "-".into()),
                        &e.origin,
                        &e.path,
                    ),
                    ExecutorRow::Adapter(a) => (
                        "adapter",
                        &a.name,
                        &a.version,
                        if a.used_by.is_empty() {
                            "-".into()
                        } else {
                            a.used_by.join(", ")
                        },
                        &a.origin,
                        &a.path,
                    ),
                };
                Row::new(vec![
                    Cell::from(mark),
                    Cell::from(kind),
                    Cell::from(name.clone()),
                    Cell::from(version.clone()),
                    Cell::from(link),
                    Cell::from(origin.clone().unwrap_or_else(|| "-".into())),
                    Cell::from(path.clone()),
                ])
            })
            .collect()
    });
    *model.cursor(Tab::Executors) = cursor;
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
    };
    table(frame, area, &mut cursor, spec, |range| {
        rows[range].iter().map(|e| event_row(e)).collect()
    });
    *model.cursor(Tab::Events) = cursor;
}

/// 샌드박스 탭 아래의 최근 이벤트(거르지 않는다, 고를 수 없다).
fn recent_events(frame: &mut Frame, model: &Model, area: Rect) {
    let block = Block::bordered().title(" Recent events ").dark_gray();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let n = inner.height.saturating_sub(1) as usize;
    let start = model.events.len().saturating_sub(n);
    let rows: Vec<Row> = model.events.range(start..).map(event_row).collect();
    let t = Table::new(rows, EVENT_WIDTHS)
        .header(Row::new(EVENT_HEADER).bold())
        .column_spacing(2);
    frame.render_widget(t, inner);
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
    let hints = Line::from(match model.tab() {
        Tab::Sandboxes => "s start  x stop  e shell  l logs  D rm  ? help  q quit ",
        Tab::Images => "D rmi  : command  ? help  q quit ",
        Tab::Events => "/ filter  ? help  q quit ",
        Tab::Executors => "u use  i install  S search  D rm  P prune  ? help  q quit ",
    })
    .dark_gray()
    .right_aligned();
    frame.render_widget(hints, area);
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
    frame.render_widget(left, area);
}

fn popup(frame: &mut Frame, p: &mut Popup) {
    let area = frame.area();
    match p {
        Popup::Help => {
            let lines: Vec<Line> = KEYS
                .iter()
                .map(|h| {
                    Line::from(vec![
                        Span::from(format!(" {:<28}", h.keys)).bold(),
                        Span::from(format!("{:<11}", h.tabs)).dark_gray(),
                        Span::from(h.does),
                    ])
                })
                .collect();
            let r = centered(area, 96, lines.len() as u16 + 2);
            frame.render_widget(Clear, r);
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(" Keys (any key closes) ")),
                r,
            );
        }
        Popup::Confirm { args } => {
            let r = centered(area, (display(args).len() as u16 + 6).max(40), 5);
            frame.render_widget(Clear, r);
            let text = vec![
                Line::from(""),
                Line::from(format!(" {}", display(args))).bold(),
                Line::from(" y: run   any other key: cancel").dark_gray(),
            ];
            frame.render_widget(
                Paragraph::new(text).block(Block::bordered().title(" Run? ").yellow()),
                r,
            );
        }
        Popup::Detail {
            title,
            body,
            scroll,
        } => {
            let r = centered(
                area,
                area.width.saturating_sub(8),
                area.height.saturating_sub(4),
            );
            frame.render_widget(Clear, r);
            let lines = body.lines().count();
            let visible = r.height.saturating_sub(2) as usize;
            // 끝을 넘어 내리지 않는다.
            *scroll = (*scroll).min(lines.saturating_sub(visible) as u16);
            frame.render_widget(
                Paragraph::new(body.as_str())
                    .wrap(Wrap { trim: false })
                    .scroll((*scroll, 0))
                    .block(
                        Block::bordered()
                            .title(format!(" {title} "))
                            .title_bottom(" Up Down scroll, any other key closes "),
                    ),
                r,
            );
            if lines > visible {
                let mut sb =
                    ScrollbarState::new(lines.saturating_sub(visible)).position(*scroll as usize);
                frame.render_stateful_widget(
                    Scrollbar::new(ScrollbarOrientation::VerticalRight),
                    r,
                    &mut sb,
                );
            }
        }
    }
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
