//! 붙어서 실행하기 (`run`, `start`, `exec`). Docker의 attach와 같다.
//!
//! - 프로세스의 표준 출력과 표준 오류를 받는 대로 sbx의 표준 출력과 표준 오류에 쓴다. 쓰기가 실패해도(예: 파이프가 닫힘)
//!   프로세스가 끝날 때까지 계속 받는다.
//! - `-i`면 sbx의 표준 입력을 읽어 보내고, 끝(EOF)이면 프로세스의 표준 입력을 닫는다.
//!   아니면 프로세스의 표준 입력은 처음부터 닫혀 있다(Docker에서 `-i`가 없을 때와 같다).
//! - 프로세스가 끝나면 그 종료 코드를 돌려준다.
//!
//! **터미널 없이**: [`FORWARDED_SIGNALS`]를 받으면 프로세스에 전한다.
//!
//! **`-t` (터미널)**, Docker와 같다:
//! - 게스트 터미널의 처음 크기는 sbx 표준 출력 터미널의 크기다(터미널이 아니거나 크기가 0이면 게스트 기본값).
//!   sbx 터미널의 크기가 바뀌면(SIGWINCH) 게스트에도 알린다.
//! - `-i`이면 sbx의 터미널을 raw 모드로 바꾸고, 끝나면 되돌린다. 그래서 Ctrl+C 같은 키는 바이트로 게스트 터미널에 가고,
//!   시그널은 게스트 터미널이 만든다. `-i`인데 표준 입력이 터미널이 아니면 오류다("the input device is not a TTY").
//! - 시그널을 전하지 않는다(Docker의 `--sig-proxy`는 터미널이 아닐 때만이다).

use std::io::{self, IsTerminal, Read, Write};

use sandbox_runtime::{IoMode, Output, Process, ProcessInput, RuntimeError, TtySize};

#[cfg(unix)]
use crate::behavior::FORWARDED_SIGNALS;

/// `-i`, `-t`로 입출력 방식을 정한다. `-it`인데 표준 입력이 터미널이 아니면 오류.
pub fn io_mode(interactive: bool, tty: bool) -> Result<IoMode, String> {
    if tty && interactive && !io::stdin().is_terminal() {
        return Err("the input device is not a TTY".into());
    }
    Ok(IoMode {
        stdin: interactive,
        tty: tty.then(|| host_size().unwrap_or(TtySize::GUEST_DEFAULT)),
    })
}

/// sbx 표준 출력 터미널의 크기. 터미널이 아니거나 크기를 0으로 알리면(모름) `None`.
fn host_size() -> Option<TtySize> {
    if !io::stdout().is_terminal() {
        return None;
    }
    let (cols, rows) = crossterm::terminal::size().ok()?;
    (rows > 0 && cols > 0).then_some(TtySize { rows, cols })
}

/// raw 모드를 켜 두고, 버려지면 되돌린다.
struct RawMode;

impl RawMode {
    fn enable() -> Option<Self> {
        crossterm::terminal::enable_raw_mode()
            .ok()
            .map(|()| RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// 프로세스가 끝날 때까지 붙어 있는다. `io`는 그 프로세스를 띄울 때 쓴 것이어야 한다. 종료 코드를 돌려준다.
pub async fn attach(mut p: Process, io: IoMode) -> Result<i32, RuntimeError> {
    let _raw = (io.tty.is_some() && io.stdin)
        .then(RawMode::enable)
        .flatten();
    if io.stdin {
        forward_stdin(p.input());
    }
    let tasks = if io.tty.is_some() {
        forward_resize(p.input())
    } else {
        forward_signals(p.input())
    };
    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    while let Some(o) = p.next().await {
        let _ = match o {
            Output::Stdout(b) => out.write_all(&b).and_then(|()| out.flush()),
            Output::Stderr(b) => err.write_all(&b).and_then(|()| err.flush()),
        };
    }
    for t in tasks {
        t.abort();
    }
    p.wait().await
}

/// 표준 입력을 읽는 스레드. 프로세스가 끝나도 읽기에서 막혀 있을 수 있지만, sbx가 끝나면 함께 끝난다.
fn forward_stdin(input: ProcessInput) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 32 * 1024];
        let mut stdin = io::stdin().lock();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => input.write_stdin(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        input.close_stdin();
    });
}

#[cfg(unix)]
fn forward_signals(input: ProcessInput) -> Vec<tokio::task::JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let kind = |name: &str| match name {
        "HUP" => Some(SignalKind::hangup()),
        "INT" => Some(SignalKind::interrupt()),
        "QUIT" => Some(SignalKind::quit()),
        "USR1" => Some(SignalKind::user_defined1()),
        "USR2" => Some(SignalKind::user_defined2()),
        "TERM" => Some(SignalKind::terminate()),
        _ => None,
    };
    FORWARDED_SIGNALS
        .iter()
        .filter_map(|&(name, guest)| {
            let mut s = signal(kind(name)?).ok()?;
            let input = input.clone();
            Some(tokio::spawn(async move {
                while s.recv().await.is_some() {
                    input.signal(guest);
                }
            }))
        })
        .collect()
}

#[cfg(not(unix))]
fn forward_signals(input: ProcessInput) -> Vec<tokio::task::JoinHandle<()>> {
    // Ctrl+C를 게스트 SIGINT(2)로.
    vec![tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            input.signal(2);
        }
    })]
}

#[cfg(unix)]
fn forward_resize(input: ProcessInput) -> Vec<tokio::task::JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut s) = signal(SignalKind::window_change()) else {
        return Vec::new();
    };
    vec![tokio::spawn(async move {
        while s.recv().await.is_some() {
            if let Some(size) = host_size() {
                input.resize(size.rows, size.cols);
            }
        }
    })]
}

#[cfg(not(unix))]
fn forward_resize(_input: ProcessInput) -> Vec<tokio::task::JoinHandle<()>> {
    // Windows 콘솔 크기 변경 알림은 아직 받지 않는다. 처음 크기만 쓴다.
    Vec::new()
}
