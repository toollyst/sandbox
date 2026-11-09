//! 붙어서 실행하기 (`run`, `start`, `exec`). Docker의 attach와 같다.
//!
//! - 프로세스의 표준 출력과 표준 오류를 받는 대로 sbx의 표준 출력과 표준 오류에 쓴다. 쓰기가 실패해도(예: 파이프가 닫힘)
//!   프로세스가 끝날 때까지 계속 받는다.
//! - `stdin`이면 sbx의 표준 입력을 읽어 보내고, 끝(EOF)이면 프로세스의 표준 입력을 닫는다.
//!   아니면 프로세스의 표준 입력은 처음부터 닫혀 있다(Docker에서 `-i`가 없을 때와 같다).
//! - [`FORWARDED_SIGNALS`]를 받으면 프로세스에 전한다.
//! - 프로세스가 끝나면 그 종료 코드를 돌려준다.

use std::io::{self, Read, Write};

use sandbox_runtime::{Output, Process, ProcessInput, RuntimeError};

#[cfg(unix)]
use crate::behavior::FORWARDED_SIGNALS;

/// 프로세스가 끝날 때까지 붙어 있는다. 종료 코드를 돌려준다.
pub async fn attach(mut p: Process, stdin: bool) -> Result<i32, RuntimeError> {
    if stdin {
        forward_stdin(p.input());
    }
    let signals = forward_signals(p.input());
    let mut out = io::stdout();
    let mut err = io::stderr();
    while let Some(o) = p.next().await {
        let _ = match o {
            Output::Stdout(b) => out.write_all(&b).and_then(|()| out.flush()),
            Output::Stderr(b) => err.write_all(&b).and_then(|()| err.flush()),
        };
    }
    for s in signals {
        s.abort();
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
