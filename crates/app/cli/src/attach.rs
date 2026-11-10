//! 붙어서 실행하기 (`run`, `start -a`, `exec`)와 shim이 소유한 샌드박스에 붙기 (`attach`). Docker의 attach와 같다.
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
//!
//! **`attach`** ([`attach_shim`], shim의 붙기 소켓으로): 위와 같고 다른 점은 이렇다(Docker `attach`와 같다).
//! - 붙은 뒤의 출력만 보인다. 표준 입력은 샌드박스가 `-i`로 만들어졌을 때만 전한다(`--no-stdin`이면 전하지 않는다).
//! - sbx의 입력이 끝나도 기본 프로세스의 표준 입력은 닫지 않는다.
//! - 터미널이고 입력을 전하면 떼기 키(기본 Ctrl+P Ctrl+Q, [`DetachKeys`])로 떨어진다. 샌드박스는 계속 돈다.
//! - 터미널이 아니면 `--sig-proxy`(기본 켜짐)일 때 시그널을 전한다.

use std::io::{self, IsTerminal, Read, Write};

use sbx_runtime::{IoMode, Output, Process, RuntimeError, TtySize};

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
/// `forward_stdin`이면 sbx의 표준 입력을 보낸다(`io.stdin`이어야 한다). 끝나면 프로세스의 표준 입력을 닫는다.
pub async fn attach(mut p: Process, io: IoMode, forward_stdin: bool) -> Result<i32, RuntimeError> {
    let forward_stdin = forward_stdin && io.stdin;
    let _raw = (io.tty.is_some() && forward_stdin)
        .then(RawMode::enable)
        .flatten();
    if forward_stdin {
        let input = p.input();
        read_stdin(None, move |chunk| match chunk {
            Some(b) => input.write_stdin(b),
            None => input.close_stdin(),
        });
    }
    let tasks = if io.tty.is_some() {
        let input = p.input();
        forward_resize(move |r, c| input.resize(r, c))
    } else {
        let input = p.input();
        forward_signals(move |n| input.signal(n))
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

/// 표준 입력을 읽는 스레드. 읽은 조각을 `send(Some)`으로, 끝을 `send(None)`으로 넘긴다.
/// `detach`가 있으면 떼기 키를 찾아 빼고, 다 맞으면 그것을 부르고 끝난다(`send(None)` 없이).
/// 프로세스가 끝나도 읽기에서 막혀 있을 수 있지만, sbx가 끝나면 함께 끝난다.
fn read_stdin(
    detach: Option<(DetachKeys, Box<dyn FnOnce() + Send>)>,
    send: impl Fn(Option<&[u8]>) + Send + 'static,
) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 32 * 1024];
        let mut stdin = io::stdin().lock();
        let mut detach = detach;
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => match &mut detach {
                    Some((keys, _)) => {
                        let (pass, hit) = keys.feed(&buf[..n]);
                        if !pass.is_empty() {
                            send(Some(&pass));
                        }
                        if hit {
                            if let Some((_, f)) = detach.take() {
                                f();
                            }
                            return;
                        }
                    }
                    None => send(Some(&buf[..n])),
                },
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        send(None);
    });
}

/// 떼기 키 순서. Docker `--detach-keys`와 같은 형식이다: 쉼표로 나눈 키, 키는 `ctrl-<글자>`
/// (`a`-`z`, `@`, `[`, `\`, `]`, `^`, `_`) 또는 글자 하나. 기본은 [`DetachKeys::DEFAULT`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachKeys {
    keys: Vec<u8>,
    matched: usize,
}

impl DetachKeys {
    /// 기본값.
    pub const DEFAULT: &'static str = "ctrl-p,ctrl-q";

    /// 읽는다.
    pub fn parse(s: &str) -> Result<Self, String> {
        let bad = || format!("invalid detach keys {s:?}");
        let mut keys = Vec::new();
        for k in s.split(',') {
            let byte = match k.strip_prefix("ctrl-") {
                Some(c) if c.len() == 1 => {
                    let c = c.as_bytes()[0].to_ascii_lowercase();
                    match c {
                        b'a'..=b'z' => c - b'a' + 1,
                        b'@' => 0,
                        b'[' => 27,
                        b'\\' => 28,
                        b']' => 29,
                        b'^' => 30,
                        b'_' => 31,
                        _ => return Err(bad()),
                    }
                }
                Some(_) => return Err(bad()),
                None if k.len() == 1 && k.is_ascii() => k.as_bytes()[0],
                None => return Err(bad()),
            };
            keys.push(byte);
        }
        Ok(Self { keys, matched: 0 })
    }

    /// 입력 조각을 넣는다. 보낼 바이트와 순서가 다 맞았는지를 돌려준다. 다 맞으면 그 뒤 바이트는 버린다.
    /// 순서의 앞부분까지만 맞은 바이트는 다음 조각을 볼 때까지 들고 있다(맞지 않으면 그때 보낸다).
    pub fn feed(&mut self, input: &[u8]) -> (Vec<u8>, bool) {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            if b == self.keys[self.matched] {
                self.matched += 1;
                if self.matched == self.keys.len() {
                    self.matched = 0;
                    return (out, true);
                }
                continue;
            }
            out.extend_from_slice(&self.keys[..self.matched]);
            self.matched = 0;
            if b == self.keys[0] {
                self.matched = 1;
                if self.keys.len() == 1 {
                    self.matched = 0;
                    return (out, true);
                }
            } else {
                out.push(b);
            }
        }
        (out, false)
    }
}

/// [`attach_shim`]의 옵션.
#[derive(Debug, Clone)]
pub struct ShimAttach {
    /// 표준 입력을 전한다(샌드박스가 `-i`일 때만 된다). `--no-stdin`이면 거짓.
    pub stdin: bool,
    /// 터미널이 아닐 때 시그널을 전한다.
    pub sig_proxy: bool,
    /// 떼기 키.
    pub detach_keys: DetachKeys,
}

/// [`attach_shim`]이 끝난 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShimEnd {
    /// 기본 프로세스가 끝났다. 종료 코드.
    Exit(i32),
    /// 떼기 키로 떨어졌다.
    Detached,
    /// shim과의 연결이 끊겼다(종료 코드를 받지 못했다).
    Lost,
}

/// shim의 붙기 소켓에 붙어 있는다. 규칙은 모듈 문서에 있다.
#[cfg(unix)]
pub async fn attach_shim(
    stream: tokio::net::UnixStream,
    opts: ShimAttach,
) -> Result<ShimEnd, String> {
    use sbx_shim::attach::{ClientMsg, ServerMsg};
    use tokio::sync::mpsc;

    let (mut rd, mut wr) = stream.into_split();
    let (open_stdin, tty) = match ServerMsg::read_from(&mut rd).await {
        Ok(Some(ServerMsg::Hello { stdin, tty })) => (stdin, tty),
        Ok(other) => return Err(format!("unexpected answer from sbx-shim: {other:?}")),
        Err(e) => return Err(format!("sbx-shim: {e}")),
    };
    let forward = opts.stdin && open_stdin;
    if tty && forward && !io::stdin().is_terminal() {
        return Err("the input device is not a TTY".into());
    }
    let _raw = (tty && forward).then(RawMode::enable).flatten();

    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            if m.write_to(&mut wr).await.is_err() {
                break;
            }
        }
    });
    let (detached_tx, mut detached) = mpsc::unbounded_channel::<()>();
    if forward {
        let tx = tx.clone();
        let detach = tty.then(|| {
            let f: Box<dyn FnOnce() + Send> = Box::new(move || {
                let _ = detached_tx.send(());
            });
            (opts.detach_keys.clone(), f)
        });
        // 입력이 끝나도 기본 프로세스의 표준 입력은 닫지 않는다.
        read_stdin(detach, move |chunk| {
            if let Some(b) = chunk {
                let _ = tx.send(ClientMsg::Stdin(b.to_vec()));
            }
        });
    }
    let tasks = if tty {
        if let Some(s) = host_size() {
            let _ = tx.send(ClientMsg::Resize(s.rows, s.cols));
        }
        let tx = tx.clone();
        forward_resize(move |r, c| {
            let _ = tx.send(ClientMsg::Resize(r, c));
        })
    } else if opts.sig_proxy {
        let tx = tx.clone();
        forward_signals(move |n| {
            let _ = tx.send(ClientMsg::Signal(n));
        })
    } else {
        Vec::new()
    };

    let mut out = std::io::stdout();
    let mut err = std::io::stderr();
    let end = loop {
        tokio::select! {
            m = ServerMsg::read_from(&mut rd) => match m {
                Ok(Some(ServerMsg::Stdout(b))) => {
                    let _ = out.write_all(&b).and_then(|()| out.flush());
                }
                Ok(Some(ServerMsg::Stderr(b))) => {
                    let _ = err.write_all(&b).and_then(|()| err.flush());
                }
                Ok(Some(ServerMsg::Exit(c))) => break ShimEnd::Exit(c),
                Ok(Some(ServerMsg::Hello { .. })) | Ok(None) | Err(_) => break ShimEnd::Lost,
            },
            Some(()) = detached.recv() => break ShimEnd::Detached,
        }
    };
    for t in tasks {
        t.abort();
    }
    writer.abort();
    Ok(end)
}

#[cfg(unix)]
fn forward_signals(
    send: impl Fn(i32) + Clone + Send + 'static,
) -> Vec<tokio::task::JoinHandle<()>> {
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
            let send = send.clone();
            Some(tokio::spawn(async move {
                while s.recv().await.is_some() {
                    send(guest);
                }
            }))
        })
        .collect()
}

#[cfg(not(unix))]
fn forward_signals(
    send: impl Fn(i32) + Clone + Send + 'static,
) -> Vec<tokio::task::JoinHandle<()>> {
    // Ctrl+C를 게스트 SIGINT(2)로.
    vec![tokio::spawn(async move {
        while tokio::signal::ctrl_c().await.is_ok() {
            send(2);
        }
    })]
}

#[cfg(unix)]
fn forward_resize(send: impl Fn(u16, u16) + Send + 'static) -> Vec<tokio::task::JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    let Ok(mut s) = signal(SignalKind::window_change()) else {
        return Vec::new();
    };
    vec![tokio::spawn(async move {
        while s.recv().await.is_some() {
            if let Some(size) = host_size() {
                send(size.rows, size.cols);
            }
        }
    })]
}

#[cfg(not(unix))]
fn forward_resize(_send: impl Fn(u16, u16) + Send + 'static) -> Vec<tokio::task::JoinHandle<()>> {
    // Windows 콘솔 크기 변경 알림은 아직 받지 않는다. 처음 크기만 쓴다.
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_keys_parse_like_docker() {
        assert_eq!(
            DetachKeys::parse(DetachKeys::DEFAULT).unwrap().keys,
            vec![16, 17]
        );
        assert_eq!(
            DetachKeys::parse("ctrl-@,ctrl-[,a").unwrap().keys,
            vec![0, 27, b'a']
        );
        for bad in ["", "ctrl-", "ctrl-1", "ctrl-ab", "ab", "a,,b"] {
            assert!(DetachKeys::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn detach_keys_are_found_across_chunks_and_partial_matches_pass_through() {
        let mut k = DetachKeys::parse(DetachKeys::DEFAULT).unwrap();
        assert_eq!(k.feed(b"ab"), (b"ab".to_vec(), false));
        // ^P 하나는 들고 있다가, 다음이 ^Q가 아니면 함께 보낸다.
        assert_eq!(k.feed(&[16]), (vec![], false));
        assert_eq!(k.feed(b"x"), (vec![16, b'x'], false));
        // ^P ^P ^Q: 앞의 ^P는 보내고 뒤의 둘로 떨어진다.
        assert_eq!(k.feed(&[16, 16]), (vec![16], false));
        assert_eq!(k.feed(&[17, b'z']), (vec![], true));
        let mut one = DetachKeys::parse("ctrl-x").unwrap();
        assert_eq!(one.feed(b"ab\x18c"), (b"ab".to_vec(), true));
    }
}
