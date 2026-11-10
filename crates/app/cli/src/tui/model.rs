//! 화면의 상태와 키. 입출력이 없다: 키나 소식([`Msg`])을 받아 상태를 바꾸고, 할 일([`Action`])을 돌려준다.
//!
//! 무엇을 하든 `sbx` 명령 하나다. 키는 그 명령 줄을 만들 뿐이고, 실행과 결과는 CLI가 정한다(정책을 두지 않는다).
//! 키 목록은 [`super::keys::KEYS`]가 정본이다.

use std::collections::VecDeque;
use std::ops::Range;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::output::{
    ChosenExecutor, Event, Image, InstalledAdapter, InstalledExecutor, Sandbox, SandboxState,
};

/// 이벤트 탭에 남기는 개수. 넘으면 오래된 것부터 버린다.
pub const EVENTS_KEPT: usize = 10_000;

/// 셸 키(`e`)가 여는 프로그램.
pub const SHELL: &str = "sh";

/// 탭.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    /// 샌드박스(`ps -a`). 처음 탭.
    #[default]
    Sandboxes,
    /// 이미지(`images`).
    Images,
    /// 이벤트(`events --since`).
    Events,
    /// 실행기와 어댑터(`executor ls`, `executor adapter ls`, `executor current`).
    Executors,
}

/// 탭 순서.
pub const TABS: [Tab; 4] = [Tab::Sandboxes, Tab::Images, Tab::Events, Tab::Executors];

impl Tab {
    /// 제목.
    pub const fn title(self) -> &'static str {
        match self {
            Tab::Sandboxes => "Sandboxes",
            Tab::Images => "Images",
            Tab::Events => "Events",
            Tab::Executors => "Executors",
        }
    }

    const fn index(self) -> usize {
        match self {
            Tab::Sandboxes => 0,
            Tab::Images => 1,
            Tab::Events => 2,
            Tab::Executors => 3,
        }
    }
}

/// 할 일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// 출력을 받아 두고 화면에 남는다(`stop`, `rm` 등). 끝나면 [`Msg::Done`].
    Run(Vec<String>),
    /// 화면을 잠시 비우고 터미널을 넘긴다(`exec -it`, `attach`, `logs -f`, `:`).
    Handover(Vec<String>),
    /// 끝낸다.
    Quit,
}

/// 밖에서 온 소식.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// `ps -a`의 결과.
    Sandboxes(Result<Vec<Sandbox>, String>),
    /// `images`의 결과.
    Images(Result<Vec<Image>, String>),
    /// `events`가 낸 줄 하나.
    Event(Event),
    /// `events`가 끝났다(끝나면 안 된다).
    EventsEnded(String),
    /// 실행기 탭의 내용(`executor ls`, `executor adapter ls`, `executor current`).
    Executors(Result<Executors, String>),
    /// [`Action::Run`]이 끝났다.
    Done {
        /// 실행한 인자.
        args: Vec<String>,
        /// 성공했는지(종료 코드 0).
        ok: bool,
        /// 실패했으면 stderr의 첫 줄.
        message: String,
    },
}

/// 실행기 탭에 보이는 것.
#[derive(Debug, Clone, PartialEq)]
pub struct Executors {
    /// 설치한 실행기.
    pub executors: Vec<InstalledExecutor>,
    /// 설치한 어댑터.
    pub adapters: Vec<InstalledAdapter>,
    /// 새 샌드박스가 쓸 실행기. 없으면 그 까닭.
    pub current: Result<ChosenExecutor, String>,
}

/// 실행기 탭의 줄 하나.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExecutorRow<'a> {
    /// 실행기.
    Executor(&'a InstalledExecutor),
    /// 어댑터.
    Adapter(&'a InstalledAdapter),
}

impl ExecutorRow<'_> {
    /// `이름@버전`.
    pub fn spec(&self) -> String {
        match self {
            Self::Executor(e) => format!("{}@{}", e.name, e.version),
            Self::Adapter(a) => format!("{}@{}", a.name, a.version),
        }
    }

    /// 이름.
    pub fn name(&self) -> &str {
        match self {
            Self::Executor(e) => &e.name,
            Self::Adapter(a) => &a.name,
        }
    }
}

/// 표 하나의 선택과 첫 줄. 보이는 줄만 그리려고 둔다(가상 스크롤).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    /// 고른 줄.
    pub selected: usize,
    /// 맨 위에 보이는 줄.
    pub offset: usize,
}

impl Cursor {
    /// 줄이 `len`개이고 `height`줄이 보일 때 보이는 범위. 고른 줄이 보이도록 첫 줄을 옮긴다.
    pub fn window(&mut self, len: usize, height: usize) -> Range<usize> {
        self.clamp(len);
        if height == 0 || len == 0 {
            self.offset = 0;
            return 0..0;
        }
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + height {
            self.offset = self.selected + 1 - height;
        }
        self.offset = self.offset.min(len.saturating_sub(height));
        self.offset..(self.offset + height).min(len)
    }

    fn clamp(&mut self, len: usize) {
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    fn move_by(&mut self, delta: isize, len: usize) {
        self.selected = self.selected.saturating_add_signed(delta);
        self.clamp(len);
    }
}

/// 위에 덮는 창.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Popup {
    /// 키 목록.
    Help,
    /// 고른 줄의 JSON.
    Detail {
        /// 제목.
        title: String,
        /// 내용.
        body: String,
        /// 내린 줄 수.
        scroll: u16,
    },
    /// 되돌릴 수 없는 명령 전에 묻는다.
    Confirm {
        /// 실행할 인자.
        args: Vec<String>,
    },
}

/// 아래 줄에서 받는 입력.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptKind {
    /// `/`: 거르기. 치는 대로 적용한다.
    Filter,
    /// `:`: `sbx` 명령.
    Command,
}

/// 입력 중인 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// 종류.
    pub kind: PromptKind,
    /// 친 글.
    pub text: String,
}

/// 마지막으로 실행한 명령과 그 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 인자.
    pub args: Vec<String>,
    /// 아직 실행 중이면 `None`, 끝났으면 성공 여부.
    pub ok: Option<bool>,
    /// 실패했을 때 까닭.
    pub message: String,
}

/// 화면 상태 전부.
#[derive(Debug, Clone, Default)]
pub struct Model {
    /// 지금 탭.
    pub tab: Tab,
    /// `ps -a`. 아직 못 받았으면 `None`.
    pub sandboxes: Option<Vec<Sandbox>>,
    /// `images`. 아직 못 받았으면 `None`.
    pub images: Option<Vec<Image>>,
    /// 받은 이벤트, 오래된 것부터.
    pub events: VecDeque<Event>,
    /// 이벤트 탭에서 맨 아래가 아닌 줄을 골랐다. 아니면 새 것을 따라간다(거르는 글이 바뀌어도).
    pub scrolled_back: bool,
    /// 실행기 탭. 아직 못 받았으면 `None`.
    pub executors: Option<Executors>,
    /// 탭마다의 표 위치.
    pub cursors: [Cursor; 4],
    /// 거르는 글(`/`). 비면 거르지 않는다.
    pub filter: String,
    /// 입력 중인 줄.
    pub prompt: Option<Prompt>,
    /// 덮은 창.
    pub popup: Option<Popup>,
    /// 마지막 명령.
    pub status: Option<Status>,
    /// 목록이나 이벤트를 못 받은 까닭.
    pub error: Option<String>,
}

impl Model {
    /// 지금 탭.
    pub fn tab(&self) -> Tab {
        self.tab
    }

    /// 그 탭의 표 위치.
    pub fn cursor(&mut self, tab: Tab) -> &mut Cursor {
        &mut self.cursors[tab.index()]
    }

    /// 거른 샌드박스.
    pub fn visible_sandboxes(&self) -> Vec<&Sandbox> {
        let f = self.filter.to_lowercase();
        self.sandboxes
            .iter()
            .flatten()
            .filter(|s| {
                f.is_empty()
                    || [
                        s.id.as_str(),
                        s.name.as_deref().unwrap_or(""),
                        s.image.reference.as_deref().unwrap_or(&s.image.id),
                        state_name(s.state),
                        &s.process.args.join(" "),
                    ]
                    .iter()
                    .any(|t| t.to_lowercase().contains(&f))
            })
            .collect()
    }

    /// 거른 이미지.
    pub fn visible_images(&self) -> Vec<&Image> {
        let f = self.filter.to_lowercase();
        self.images
            .iter()
            .flatten()
            .filter(|i| {
                f.is_empty()
                    || i.id.contains(&f)
                    || i.references.iter().any(|r| r.to_lowercase().contains(&f))
            })
            .collect()
    }

    /// 거른 이벤트, 오래된 것부터.
    pub fn visible_events(&self) -> Vec<&Event> {
        let f = self.filter.to_lowercase();
        self.events
            .iter()
            .filter(|e| {
                f.is_empty()
                    || [e.kind.as_str(), &e.action, &e.id]
                        .into_iter()
                        .chain(e.attributes.values().map(String::as_str))
                        .any(|t| t.to_lowercase().contains(&f))
            })
            .collect()
    }

    /// 거른 실행기 탭 줄: 실행기, 그다음 어댑터.
    pub fn visible_executor_rows(&self) -> Vec<ExecutorRow<'_>> {
        let f = self.filter.to_lowercase();
        let Some(x) = &self.executors else {
            return Vec::new();
        };
        x.executors
            .iter()
            .map(ExecutorRow::Executor)
            .chain(x.adapters.iter().map(ExecutorRow::Adapter))
            .filter(|r| f.is_empty() || r.spec().to_lowercase().contains(&f))
            .collect()
    }

    fn len(&self, tab: Tab) -> usize {
        match tab {
            Tab::Sandboxes => self.visible_sandboxes().len(),
            Tab::Images => self.visible_images().len(),
            Tab::Events => self.visible_events().len(),
            Tab::Executors => self.visible_executor_rows().len(),
        }
    }

    fn selected_executor_row(&self) -> Option<ExecutorRow<'_>> {
        let i = self.cursors[Tab::Executors.index()].selected;
        self.visible_executor_rows().get(i).copied()
    }

    fn selected_sandbox(&self) -> Option<&Sandbox> {
        let i = self.cursors[Tab::Sandboxes.index()].selected;
        self.visible_sandboxes().get(i).copied()
    }

    fn selected_image(&self) -> Option<&Image> {
        let i = self.cursors[Tab::Images.index()].selected;
        self.visible_images().get(i).copied()
    }

    /// 소식을 반영한다.
    pub fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::Sandboxes(Ok(v)) => {
                self.sandboxes = Some(v);
                self.error = None;
            }
            Msg::Images(Ok(v)) => {
                self.images = Some(v);
                self.error = None;
            }
            Msg::Executors(Ok(v)) => {
                self.executors = Some(v);
                self.error = None;
            }
            Msg::Sandboxes(Err(e))
            | Msg::Images(Err(e))
            | Msg::Executors(Err(e))
            | Msg::EventsEnded(e) => {
                self.error = Some(e);
            }
            Msg::Event(e) => {
                self.events.push_back(e);
                let dropped = self.events.len() > EVENTS_KEPT;
                if dropped {
                    self.events.pop_front();
                }
                if !self.scrolled_back {
                    let last = self.len(Tab::Events).saturating_sub(1);
                    self.cursor(Tab::Events).selected = last;
                } else if dropped {
                    // 보던 줄이 그대로 보이게 한 칸 당긴다.
                    let c = self.cursor(Tab::Events);
                    c.selected = c.selected.saturating_sub(1);
                }
            }
            Msg::Done { args, ok, message } => {
                // 지금 보이는 상태가 그 명령의 것일 때만 바꾼다(뒤에 다른 명령을 냈으면 그것을 보인다).
                if let Some(s) = &mut self.status
                    && s.args == args
                {
                    s.ok = Some(ok);
                    s.message = message;
                }
            }
        }
        // 줄이 줄어 고른 줄이 사라졌으면 그리면서(`Cursor::window`) 맞춘다.
    }

    /// 키 하나. 할 일이 있으면 돌려준다.
    pub fn key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            return Some(Action::Quit);
        }
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        if self.popup.is_some() {
            return self.popup_key(key);
        }
        let tab = self.tab();
        let len = self.len(tab);
        let page = 10;
        match key.code {
            KeyCode::Char('q') => return Some(Action::Quit),
            KeyCode::Tab => self.tab = TABS[(tab.index() + 1) % TABS.len()],
            KeyCode::BackTab => self.tab = TABS[(tab.index() + TABS.len() - 1) % TABS.len()],
            KeyCode::Char(c @ '1'..='4') => self.tab = TABS[c as usize - '1' as usize],
            KeyCode::Down | KeyCode::Char('j') => self.cursor(tab).move_by(1, len),
            KeyCode::Up | KeyCode::Char('k') => self.cursor(tab).move_by(-1, len),
            KeyCode::PageDown => self.cursor(tab).move_by(page, len),
            KeyCode::PageUp => self.cursor(tab).move_by(-page, len),
            KeyCode::Home | KeyCode::Char('g') => self.cursor(tab).selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.cursor(tab).selected = len.saturating_sub(1),
            _ => return self.other_key(tab, key),
        }
        if tab == Tab::Events {
            self.scrolled_back = self.cursor(tab).selected + 1 < len;
        }
        None
    }

    /// 움직이는 키가 아닌 것.
    fn other_key(&mut self, tab: Tab, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('?') => self.popup = Some(Popup::Help),
            KeyCode::Char('/') => {
                // 새로 친다(lazygit, less와 같다). 치는 대로 거른다.
                self.filter.clear();
                self.prompt = Some(Prompt {
                    kind: PromptKind::Filter,
                    text: String::new(),
                })
            }
            KeyCode::Char(':') => {
                self.prompt = Some(Prompt {
                    kind: PromptKind::Command,
                    text: String::new(),
                })
            }
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Enter => self.popup = self.detail(),
            KeyCode::Char(c) => return self.command_key(tab, c),
            _ => {}
        }
        None
    }

    /// 탭마다의 명령 키.
    fn command_key(&mut self, tab: Tab, c: char) -> Option<Action> {
        let args = |v: &[&str], target: String| -> Vec<String> {
            v.iter()
                .map(|s| s.to_string())
                .chain(std::iter::once(target))
                .collect()
        };
        match tab {
            Tab::Sandboxes => {
                let target = target(self.selected_sandbox()?);
                match c {
                    's' => Some(self.run(args(&["start"], target))),
                    'x' => Some(self.run(args(&["stop"], target))),
                    'r' => Some(self.run(args(&["restart"], target))),
                    'K' => self.confirm(args(&["kill"], target)),
                    'D' => self.confirm(args(&["rm", "-f"], target)),
                    'e' => {
                        let mut a = args(&["exec", "-it"], target);
                        a.push(SHELL.to_owned());
                        Some(Action::Handover(a))
                    }
                    'a' => Some(Action::Handover(args(&["attach"], target))),
                    'l' => Some(Action::Handover(args(&["logs", "-f"], target))),
                    _ => None,
                }
            }
            Tab::Images => {
                let image = self.selected_image()?;
                let target = image
                    .references
                    .first()
                    .cloned()
                    .unwrap_or_else(|| image.id.clone());
                match c {
                    'D' => self.confirm(args(&["rmi"], target)),
                    _ => None,
                }
            }
            Tab::Events => None,
            Tab::Executors => self.executor_key(c),
        }
    }

    /// 실행기 탭의 명령 키. `i`, `S`, `P`는 고른 줄이 없어도 된다.
    fn executor_key(&mut self, c: char) -> Option<Action> {
        let words = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match c {
            'i' => {
                let name = self
                    .selected_executor_row()
                    .filter(|r| matches!(r, ExecutorRow::Executor(_)))
                    .map_or(String::new(), |r| r.name().to_owned());
                // 고쳐 칠 수 있게 명령 줄에 채운다. 고른 실행기가 없으면 이름은 비워 둔다(특정 실행기를 권하지 않는다). 받는 동안 진행을 보이도록 터미널을 넘긴다.
                self.prompt = Some(Prompt {
                    kind: PromptKind::Command,
                    text: format!("executor install {name}"),
                });
                return None;
            }
            'S' => return Some(Action::Handover(words(&["executor", "search"]))),
            'P' => return self.confirm(words(&["executor", "prune"])),
            _ => {}
        }
        let row = self.selected_executor_row()?;
        let spec = row.spec();
        match (c, row) {
            ('u', ExecutorRow::Executor(_)) => Some(self.run(words(&["executor", "use", &spec]))),
            ('D', ExecutorRow::Executor(_)) => self.confirm(words(&["executor", "rm", &spec])),
            ('D', ExecutorRow::Adapter(_)) => {
                self.confirm(words(&["executor", "adapter", "rm", &spec]))
            }
            _ => None,
        }
    }

    /// 바로 실행한다. 상태 줄에 명령 줄을 띄운다.
    fn run(&mut self, args: Vec<String>) -> Action {
        self.status = Some(Status {
            args: args.clone(),
            ok: None,
            message: String::new(),
        });
        Action::Run(args)
    }

    /// 묻는다. `y`를 받으면 그때 실행한다([`Model::popup_key`]). 지금은 할 일이 없다.
    fn confirm(&mut self, args: Vec<String>) -> Option<Action> {
        self.popup = Some(Popup::Confirm { args });
        None
    }

    fn detail(&self) -> Option<Popup> {
        let (title, body) = match self.tab() {
            Tab::Sandboxes => {
                let s = self.selected_sandbox()?;
                (target(s), serde_json::to_string_pretty(s).ok()?)
            }
            Tab::Images => {
                let i = self.selected_image()?;
                let title = i.references.first().unwrap_or(&i.id).clone();
                (title, serde_json::to_string_pretty(i).ok()?)
            }
            Tab::Executors => {
                let r = self.selected_executor_row()?;
                let body = match r {
                    ExecutorRow::Executor(e) => serde_json::to_string_pretty(e).ok()?,
                    ExecutorRow::Adapter(a) => serde_json::to_string_pretty(a).ok()?,
                };
                (r.spec(), body)
            }
            Tab::Events => {
                let i = self.cursors[Tab::Events.index()].selected;
                let e = *self.visible_events().get(i)?;
                (
                    format!("{} {}", e.kind, e.action),
                    serde_json::to_string_pretty(e).ok()?,
                )
            }
        };
        Some(Popup::Detail {
            title,
            body,
            scroll: 0,
        })
    }

    fn popup_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.popup.as_mut()? {
            Popup::Help => self.popup = None,
            Popup::Confirm { args } => {
                let args = args.clone();
                self.popup = None;
                if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                    return Some(self.run(args));
                }
            }
            Popup::Detail { scroll, .. } => match key.code {
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
                _ => self.popup = None,
            },
        }
        None
    }

    fn prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        let p = self.prompt.as_mut()?;
        match key.code {
            KeyCode::Esc => {
                if p.kind == PromptKind::Filter {
                    self.filter.clear();
                }
                self.prompt = None;
            }
            KeyCode::Enter => {
                let p = self.prompt.take()?;
                if p.kind == PromptKind::Command {
                    match split(&p.text) {
                        Ok(args) if args.is_empty() => {}
                        Ok(args) => return Some(Action::Handover(args)),
                        Err(e) => {
                            self.status = Some(Status {
                                args: vec![p.text],
                                ok: Some(false),
                                message: e,
                            })
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                p.text.pop();
            }
            KeyCode::Char(c) => p.text.push(c),
            _ => {}
        }
        if let Some(p) = &self.prompt
            && p.kind == PromptKind::Filter
        {
            self.filter = p.text.clone();
        }
        None
    }
}

/// 명령에 넘기는 샌드박스: 이름이 있으면 이름, 없으면 전체 ID.
pub fn target(s: &Sandbox) -> String {
    s.name.clone().unwrap_or_else(|| s.id.clone())
}

/// 상태 이름(OCI, `ps`와 같다).
pub const fn state_name(s: SandboxState) -> &'static str {
    match s {
        SandboxState::Creating => "creating",
        SandboxState::Created => "created",
        SandboxState::Running => "running",
        SandboxState::Stopped => "stopped",
    }
}

/// `:` 명령 줄을 인자로 나눈다. 셸처럼 `'…'`, `"…"`, `\`를 받는다(변수와 글롭은 없다).
pub fn split(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut started = false;
    let mut chars = line.trim_start_matches("sbx ").chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if started {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            '\'' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => cur.push(c),
                        None => return Err("unclosed '".to_owned()),
                    }
                }
            }
            '"' => {
                started = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c) => cur.push(c),
                            None => return Err("unclosed \"".to_owned()),
                        },
                        Some(c) => cur.push(c),
                        None => return Err("unclosed \"".to_owned()),
                    }
                }
            }
            '\\' => {
                started = true;
                match chars.next() {
                    Some(c) => cur.push(c),
                    None => return Err("trailing \\".to_owned()),
                }
            }
            c => {
                started = true;
                cur.push(c);
            }
        }
    }
    if started {
        out.push(cur);
    }
    Ok(out)
}

/// 명령 줄을 보이는 대로 적는다: `sbx` 뒤에 인자, 셸에 그대로 붙여 넣을 수 있게 따옴표를 친다.
/// 전역 옵션(`--data-dir` 등)은 적지 않는다(TUI가 받은 것을 그대로 넘긴다).
pub fn display(args: &[String]) -> String {
    let mut s = String::from("sbx");
    for a in args {
        s.push(' ');
        let plain = !a.is_empty()
            && a.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '-' | '_' | '.' | '/' | ':' | '=' | '@' | ',')
            });
        if plain {
            s.push_str(a);
        } else {
            s.push('\'');
            s.push_str(&a.replace('\'', r"'\''"));
            s.push('\'');
        }
    }
    s
}

#[cfg(test)]
pub(crate) mod tests;
