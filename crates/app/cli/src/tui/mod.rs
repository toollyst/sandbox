//! `sbx tui`: 샌드박스와 이미지를 한 화면에서 보고 다룬다(전체 화면, ratatui).
//!
//! 코어를 직접 부르지 않는다. 보는 것도 하는 것도 모두 같은 `sbx` 실행 파일을 자식으로 부른 결과다.
//! 그래서 TUI가 보여 주는 것은 CLI가 하는 것과 어긋날 수 없고, 상태 줄의 명령 줄을 셸에 붙여 넣으면 같은 일이 된다.
//!
//! - 보기: `ps -a`, `images`(`--format json`)를 읽고, `events --since 1h`를 따라 읽는다. 실행기 탭은 `executor ls`,
//!   `executor adapter ls`, `executor current`를 읽는다(이벤트가 없으므로 열 때, `executor` 명령이 끝날 때, 터미널을
//!   돌려받을 때 다시 읽는다). 샌드박스 이벤트가 오면
//!   `ps -a`를, 이미지 이벤트가 오면 `images`를 다시 읽는다(읽는 중에 또 오면 끝난 뒤 한 번 더).
//!   `ps`보다 앞선 시각부터 이벤트를 받으므로 그 사이의 변화도 놓치지 않는다.
//! - 하기([`model::Action`]):
//!   - `Run`: 출력을 받아 두고 화면에 남는다. 끝나면 상태 줄에 결과. 화면은 이벤트로 바뀐다.
//!   - `Handover`: 대체 화면을 닫고 터미널을 자식에 넘긴다(`exec -it`, `attach`, `logs -f`, `:` 명령).
//!     기다리는 동안 SIGINT는 자식만 받는다(셸과 같다). 끝나면 Enter를 기다린 뒤 화면으로 돌아온다.
//! - 자식 프로세스
//!   - 이벤트를 따라 읽는 자식과 `Run` 자식은 따로 프로세스 그룹에 둔다. 넘긴 터미널에서 친 Ctrl+C가 닿지 않는다.
//!   - 이벤트 자식은 TUI가 끝날 때(패닉 포함) 죽인다. `Run` 자식은 사용자가 시킨 일이라 끝까지 둔다.
//! - 전역 옵션(`--data-dir` 등)은 받은 그대로 자식에 넘긴다. 상태 줄에는 적지 않는다.
//!
//! 키 목록은 [`keys`], 화면과 키 처리는 [`model`] (입출력이 없다)과 [`view`] (그리기만 한다)에 있다. 이 모듈은 둘을 프로세스와 터미널에 잇는다.

pub mod keys;
pub mod model;
pub mod view;

use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};

use crate::app::AppResult;
use crate::cli::Cli;
use crate::output::{Event, Image, Sandbox};
use model::{Action, Model, Msg};

/// 이벤트를 얼마나 거슬러 받는지(`events --since`). 화면을 열 때 최근 일을 보여 준다.
pub const EVENTS_SINCE: &str = "1h";

/// 키를 기다리는 최대 시간. 그동안 온 소식은 다음 장에 그린다.
const TICK: Duration = Duration::from_millis(100);

/// `sbx tui`.
pub fn run(cli: &Cli) -> AppResult<u8> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("sbx tui needs a terminal (stdin and stdout)".into());
    }
    let sbx = Sbx::new(cli)?;
    let (tx, rx) = mpsc::channel();
    let _events = Follower::start(&sbx, tx.clone())?;
    let mut lists = Lists::default();
    lists.refresh(List::Sandboxes, &sbx, &tx);
    lists.refresh(List::Images, &sbx, &tx);
    lists.refresh(List::Executors, &sbx, &tx);
    // 넘긴 터미널에서 친 Ctrl+C로 TUI가 죽지 않게 처리기를 둔다(무시가 아니라서 자식은 exec 때 기본으로 돌아간다).
    // raw 모드인 동안 Ctrl+C는 키로 온다.
    let _sigint = interrupt_handler()?;
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &sbx, &tx, &rx, &mut lists);
    ratatui::restore();
    result.map(|()| 0)
}

fn event_loop(
    terminal: &mut DefaultTerminal,
    sbx: &Sbx,
    tx: &Sender<Msg>,
    rx: &Receiver<Msg>,
    lists: &mut Lists,
) -> AppResult<()> {
    let mut model = Model::default();
    loop {
        terminal.draw(|f| view::draw(f, &mut model, now_ns()))?;
        if event::poll(TICK)?
            && let TermEvent::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match model.key(key) {
                None => {}
                Some(Action::Quit) => return Ok(()),
                Some(Action::Run(args)) => spawn_run(sbx, args, tx.clone()),
                Some(Action::Handover(args)) => {
                    handover(terminal, sbx, &args)?;
                    // 넘긴 동안의 변화는 이벤트로 오지만, `:` 명령이 다른 데이터 디렉터리를 썼을 수도 있어 다시 읽는다.
                    lists.refresh(List::Sandboxes, sbx, tx);
                    lists.refresh(List::Images, sbx, tx);
                    lists.refresh(List::Executors, sbx, tx);
                }
            }
        }
        while let Ok(msg) = rx.try_recv() {
            match &msg {
                Msg::Event(e) if e.kind == "sandbox" => lists.refresh(List::Sandboxes, sbx, tx),
                Msg::Event(e) if e.kind == "image" => lists.refresh(List::Images, sbx, tx),
                Msg::Sandboxes(_) => lists.done(List::Sandboxes, sbx, tx),
                Msg::Images(_) => lists.done(List::Images, sbx, tx),
                Msg::Executors(_) => lists.done(List::Executors, sbx, tx),
                // 실행기는 이벤트를 내지 않는다. `executor` 명령이 끝나면 다시 읽는다.
                Msg::Done { args, .. } if args.first().is_some_and(|a| a == "executor") => {
                    lists.refresh(List::Executors, sbx, tx)
                }
                _ => {}
            }
            model.apply(msg);
        }
    }
}

/// 같은 `sbx`를 자식으로 부른다.
#[derive(Debug, Clone)]
pub struct Sbx {
    exe: PathBuf,
    globals: Vec<OsString>,
}

impl Sbx {
    /// 지금 실행 파일과 받은 전역 옵션.
    pub fn new(cli: &Cli) -> io::Result<Self> {
        let mut globals = Vec::new();
        for (flag, value) in [
            ("--data-dir", &cli.data_dir),
            ("--cache-dir", &cli.cache_dir),
            ("--socket-dir", &cli.socket_dir),
            ("--guest-init", &cli.guest_init),
            ("--executor", &cli.executor),
            ("--shim", &cli.shim),
        ] {
            if let Some(v) = value {
                globals.push(flag.into());
                globals.push(v.into());
            }
        }
        Ok(Self {
            exe: std::env::current_exe()?,
            globals,
        })
    }

    /// `sbx <전역 옵션> <args>`.
    pub fn command(&self, args: &[String]) -> Command {
        let mut c = Command::new(&self.exe);
        c.args(&self.globals).args(args);
        c
    }

    /// 터미널과 떨어진 자식: 따로 프로세스 그룹, stdin 없음, stdout과 stderr는 받는다.
    fn detached(&self, args: &[String]) -> Command {
        let mut c = self.command(args);
        c.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut c, 0);
        c
    }

    /// `--format json <args>`를 끝까지 실행하고 stdout을 `T`로 읽는다. 실패하면 stderr의 첫 줄.
    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let mut all = vec!["--format".to_owned(), "json".to_owned()];
        all.extend(args.iter().map(|s| s.to_string()));
        let out = self.detached(&all).output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(first_line(&out.stderr, &out.status));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("sbx {}: {e}", args.join(" ")))
    }
}

/// 다시 읽는 목록.
#[derive(Debug, Clone, Copy)]
enum List {
    Sandboxes,
    Images,
    Executors,
}

/// 목록마다 읽는 중인지, 읽는 중에 또 읽으라고 했는지.
#[derive(Debug, Default)]
struct Lists {
    busy: [bool; 3],
    again: [bool; 3],
}

impl Lists {
    fn refresh(&mut self, list: List, sbx: &Sbx, tx: &Sender<Msg>) {
        let i = list as usize;
        if self.busy[i] {
            self.again[i] = true;
            return;
        }
        self.busy[i] = true;
        let (sbx, tx) = (sbx.clone(), tx.clone());
        thread::spawn(move || {
            let msg = match list {
                List::Sandboxes => Msg::Sandboxes(sbx.json::<Vec<Sandbox>>(&["ps", "-a"])),
                List::Images => Msg::Images(sbx.json::<Vec<Image>>(&["images"])),
                List::Executors => Msg::Executors(executors(&sbx)),
            };
            let _ = tx.send(msg);
        });
    }

    fn done(&mut self, list: List, sbx: &Sbx, tx: &Sender<Msg>) {
        let i = list as usize;
        self.busy[i] = false;
        if std::mem::take(&mut self.again[i]) {
            self.refresh(list, sbx, tx);
        }
    }
}

/// 실행기 탭의 내용. 실행기가 하나도 없으면 `current`가 그 까닭이다(오류가 아니다).
fn executors(sbx: &Sbx) -> Result<model::Executors, String> {
    Ok(model::Executors {
        executors: sbx.json(&["executor", "ls"])?,
        adapters: sbx.json(&["executor", "adapter", "ls"])?,
        current: sbx.json(&["executor", "current"]),
    })
}

/// `events --since`를 따라 읽는 자식. 버리면 죽인다.
struct Follower {
    child: Child,
}

impl Follower {
    fn start(sbx: &Sbx, tx: Sender<Msg>) -> io::Result<Self> {
        let args: Vec<String> = ["--format", "json", "events", "--since", EVENTS_SINCE]
            .map(str::to_owned)
            .into();
        let mut child = sbx.detached(&args).spawn()?;
        let stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(e) = serde_json::from_str::<Event>(&line)
                    && tx.send(Msg::Event(e)).is_err()
                {
                    return;
                }
            }
            // 죽이지 않았는데 끝났다(데이터 디렉터리를 못 읽는 등). 까닭을 보인다.
            let mut err = Vec::new();
            let _ = stderr.read_to_end(&mut err);
            let why = String::from_utf8_lossy(&err)
                .trim()
                .lines()
                .last()
                .map(str::to_owned);
            let _ = tx.send(Msg::EventsEnded(format!(
                "events stopped{}",
                why.map(|w| format!(": {w}")).unwrap_or_default()
            )));
        });
        Ok(Self { child })
    }
}

impl Drop for Follower {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// [`Action::Run`]: 끝나면 [`Msg::Done`].
fn spawn_run(sbx: &Sbx, args: Vec<String>, tx: Sender<Msg>) {
    let mut c = sbx.detached(&args);
    thread::spawn(move || {
        let (ok, message) = match c.output() {
            Ok(o) if o.status.success() => (true, String::new()),
            Ok(o) => (false, first_line(&o.stderr, &o.status)),
            Err(e) => (false, e.to_string()),
        };
        let _ = tx.send(Msg::Done { args, ok, message });
    });
}

/// [`Action::Handover`]: 화면을 닫고 자식에 터미널을 넘긴 뒤 돌아온다.
fn handover(terminal: &mut DefaultTerminal, sbx: &Sbx, args: &[String]) -> AppResult<()> {
    ratatui::restore();
    let mut out = io::stdout();
    writeln!(out, "$ {}", model::display(args))?;
    out.flush()?;
    let ended = match sbx.command(args).status() {
        Ok(s) => describe(&s),
        Err(e) => format!("could not run: {e}"),
    };
    write!(out, "\n[{ended}] Press Enter to return to sbx tui.")?;
    out.flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    *terminal = ratatui::init();
    terminal.clear()?;
    Ok(())
}

fn describe(s: &ExitStatus) -> String {
    if let Some(c) = s.code() {
        return format!("exit {c}");
    }
    #[cfg(unix)]
    if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(s) {
        return format!("killed by signal {sig}");
    }
    "killed by a signal".to_owned()
}

fn first_line(stderr: &[u8], status: &ExitStatus) -> String {
    // 첫 줄이 까닭이다. 대상이 여럿인 명령은 마지막 줄이 요약("1 of 1 sandbox(es) failed")이다.
    String::from_utf8_lossy(stderr)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| describe(status))
}

#[cfg(unix)]
fn interrupt_handler() -> io::Result<tokio::signal::unix::Signal> {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
}

/// 처리기를 둘 시그널이 없는 곳(Windows: raw 모드에서 Ctrl+C는 키로 온다).
#[cfg(not(unix))]
struct NoInterruptHandler;

#[cfg(not(unix))]
fn interrupt_handler() -> io::Result<NoInterruptHandler> {
    Ok(NoInterruptHandler)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}
