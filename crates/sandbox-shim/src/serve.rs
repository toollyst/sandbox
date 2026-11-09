//! shim 프로세스 (`sbx-shim start ...`).
//!
//! 1. 실행 코어를 만들고 샌드박스를 시작한다. 이 프로세스가 VM을 소유한다(attached 소유를 shim이 대신 가진다).
//! 2. 결과를 표준 출력에 한 줄로 쓴다: [`READY`] 또는 `error <이유>`. 그 뒤로는 표준 출력에 쓰지 않는다.
//!    진단은 표준 오류에 쓴다(띄운 쪽이 `logs/shim.log`로 돌린다).
//!    기본 프로세스는 기록된 대로 표준 입력을 열고(`open_stdin`) 터미널을 붙인다(`tty`, 처음 크기는 게스트 기본값).
//! 3. 기본 프로세스의 출력을 [`MAIN_LOG`]에 쓴다. Unix에서는 붙기 소켓([`attach`](crate::attach))을 열어
//!    붙은 쪽에도 보내고, 붙은 쪽의 입력, 터미널 크기, 시그널을 기본 프로세스에 전한다.
//!    소켓을 열지 못하면 표준 오류에 쓰고 붙기 없이 계속한다.
//! 4. 기본 프로세스가 끝나면(실행 코어가 샌드박스를 멈추고 기록한 뒤) 붙은 쪽에 종료 코드를 보내고
//!    ([`FLUSH_WAIT`]까지 보내기를 기다린다) 소켓을 지우고 끝난다. 종료 코드는 0이다.
//!    `remove_on_exit`이면 그 전에 샌드박스를 지운다.
//!
//! 시그널 (Unix): SIGTERM, SIGINT를 받으면 샌드박스를 멈춘다(`sbx stop`과 같다. 기다리는 시간은 기록된
//! `--stop-timeout`, 없으면 [`STOP_TIMEOUT`]).
//! SIGHUP은 무시한다(터미널이 닫혀도 남는다). shim이 강제로 죽으면 실행기 규약에 따라 VM도 끝나고,
//! 샌드박스는 소유자가 사라진 것으로 `stopped`가 된다.

use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use executor_process::ProcessExecutor;
use sandbox_model::SandboxId;
use sandbox_runtime::{EXIT_VM_ENDED, IoMode, Output, Runtime, TtySize};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::attach::ServerMsg;
use crate::config::ShimConfig;
use crate::log::{LogWriter, MAIN_LOG, Stream};

/// 끝날 때 붙은 쪽에 남은 출력과 종료 코드를 보내기를 기다리는 시간.
pub const FLUSH_WAIT: Duration = Duration::from_secs(1);

/// 시작에 성공했을 때 쓰는 줄.
pub const READY: &str = "ready";

/// 시그널을 받아 멈출 때 게스트가 스스로 꺼지기를 기다리는 시간.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// 진입점. 인자는 [`ShimConfig::parse`]의 형식이다.
pub fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let (config, id) = match ShimConfig::parse(&args) {
        Ok(v) => v,
        Err(e) => {
            println!("error {e}");
            return ExitCode::from(2);
        }
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            println!("error {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(serve(config, id))
}

async fn serve(config: ShimConfig, id: SandboxId) -> ExitCode {
    let started = async {
        let executor = ProcessExecutor::new(&config.executor).map_err(|e| e.to_string())?;
        let runtime =
            Runtime::new(config.runtime.clone(), Arc::new(executor)).map_err(|e| e.to_string())?;
        let sandbox = runtime.get(&id).await.map_err(|e| e.to_string())?;
        let log = LogWriter::open(&sandbox.log_dir().join(MAIN_LOG))
            .map_err(|e| format!("{MAIN_LOG}: {e}"))?;
        // 시그널은 시작 전에 잡아 둔다. 시작 중에 받으면 시작이 끝난 뒤 멈춘다.
        let signals = Signals::new();
        let spec = &sandbox.info().spec;
        let io = IoMode {
            stdin: spec.open_stdin,
            tty: spec.tty.then_some(TtySize::GUEST_DEFAULT),
        };
        let main = sandbox.start(io).await.map_err(|e| e.to_string())?;
        Ok::<_, String>((sandbox, log, signals, main, io))
    }
    .await;
    let (sandbox, mut log, mut signals, mut main, io) = match started {
        Ok(v) => {
            println!("{READY}");
            v
        }
        Err(e) => {
            println!("error {e}");
            return ExitCode::FAILURE;
        }
    };

    let clients = Clients::default();
    let socket = crate::attach::socket_path(&config.runtime.socket_dir, &id);
    let listener = listen(&socket, &clients, &main, io);
    let mut stopping = false;
    loop {
        tokio::select! {
            o = main.next() => match o {
                Some(Output::Stdout(b)) => {
                    write(&mut log, Stream::Stdout, &b);
                    clients.send(ServerMsg::Stdout(b));
                }
                Some(Output::Stderr(b)) => {
                    write(&mut log, Stream::Stderr, &b);
                    clients.send(ServerMsg::Stderr(b));
                }
                None => break,
            },
            () = signals.stop(), if !stopping => {
                stopping = true;
                eprintln!("sbx-shim: stop requested");
                let timeout = sandbox
                    .info()
                    .spec
                    .stop
                    .timeout_secs
                    .map_or(STOP_TIMEOUT, |s| Duration::from_secs(s.into()));
                if let Err(e) = sandbox.stop(None, timeout).await {
                    eprintln!("sbx-shim: stop: {e}");
                }
            }
        }
    }
    let code = main.wait().await.unwrap_or(EXIT_VM_ENDED);
    if let Some(l) = listener {
        l.abort();
    }
    clients.finish(code).await;
    let _ = std::fs::remove_file(&socket);
    if config.remove_on_exit
        && let Err(e) = sandbox.delete().await
    {
        eprintln!("sbx-shim: remove: {e}");
    }
    ExitCode::SUCCESS
}

/// 붙은 쪽 하나: 보낼 메시지 큐와 그것을 쓰는 작업.
type Client = (mpsc::Sender<ServerMsg>, JoinHandle<()>);

/// 붙은 쪽들.
#[derive(Clone, Default)]
struct Clients(Arc<Mutex<Vec<Client>>>);

impl Clients {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Client>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 모두에게 보낸다. 큐가 찼거나 끊긴 쪽은 뺀다(그 쪽의 작업이 연결을 닫는다).
    fn send(&self, m: ServerMsg) {
        self.lock().retain(|(tx, _)| tx.try_send(m.clone()).is_ok());
    }

    #[cfg(unix)]
    fn add(&self, tx: mpsc::Sender<ServerMsg>, writer: JoinHandle<()>) {
        self.lock().push((tx, writer));
    }

    /// 종료 코드를 보내고, 쓰기 작업이 끝나기를 [`FLUSH_WAIT`]까지 기다린다.
    async fn finish(&self, code: i32) {
        let all = std::mem::take(&mut *self.lock());
        let mut writers = Vec::new();
        for (tx, w) in all {
            let _ = tx.try_send(ServerMsg::Exit(code));
            writers.push(w);
        }
        let _ = tokio::time::timeout(FLUSH_WAIT, async {
            for w in writers {
                let _ = w.await;
            }
        })
        .await;
    }
}

/// 붙기 소켓을 열고 받는 작업을 시작한다. 열지 못하면 `None`.
#[cfg(unix)]
fn listen(
    path: &std::path::Path,
    clients: &Clients,
    main: &sandbox_runtime::Process,
    io: IoMode,
) -> Option<JoinHandle<()>> {
    use crate::attach::ClientMsg;
    // 전에 죽은 shim이 남긴 파일일 수 있다.
    let _ = std::fs::remove_file(path);
    let listener = match tokio::net::UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sbx-shim: attach socket {}: {e}", path.display());
            return None;
        }
    };
    let clients = clients.clone();
    let input = main.input();
    let hello = ServerMsg::Hello {
        stdin: io.stdin,
        tty: io.tty.is_some(),
    };
    Some(tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let (mut rd, mut wr) = stream.into_split();
            let (tx, mut rx) = mpsc::channel(crate::attach::QUEUE);
            let _ = tx.try_send(hello.clone());
            let writer = tokio::spawn(async move {
                while let Some(m) = rx.recv().await {
                    let last = matches!(m, ServerMsg::Exit(_));
                    if m.write_to(&mut wr).await.is_err() || last {
                        break;
                    }
                }
            });
            clients.add(tx, writer);
            let input = input.clone();
            tokio::spawn(async move {
                while let Ok(Some(m)) = ClientMsg::read_from(&mut rd).await {
                    match m {
                        ClientMsg::Stdin(b) if io.stdin => input.write_stdin(&b),
                        ClientMsg::Stdin(_) => {}
                        ClientMsg::Resize(rows, cols) => input.resize(rows, cols),
                        ClientMsg::Signal(n) => input.signal(n),
                    }
                }
            });
        }
    }))
}

#[cfg(not(unix))]
fn listen(
    _path: &std::path::Path,
    _clients: &Clients,
    _main: &sandbox_runtime::Process,
    _io: IoMode,
) -> Option<JoinHandle<()>> {
    None
}

fn write(log: &mut LogWriter, stream: Stream, data: &[u8]) {
    if let Err(e) = log.write(stream, data) {
        eprintln!("sbx-shim: {MAIN_LOG}: {e}");
    }
}

/// 멈추라는 시그널.
struct Signals {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    _hup: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    fn new() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                term: signal(SignalKind::terminate()).ok(),
                int: signal(SignalKind::interrupt()).ok(),
                // 처리기를 걸어 두기만 하면 기본 동작(종료)이 일어나지 않는다.
                _hup: signal(SignalKind::hangup()).ok(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// 멈추라는 시그널을 받을 때까지 기다린다.
    async fn stop(&mut self) {
        #[cfg(unix)]
        {
            let term = async {
                match &mut self.term {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending().await,
                }
            };
            let int = async {
                match &mut self.int {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = term => {}
                () = int => {}
            }
        }
        #[cfg(not(unix))]
        std::future::pending::<()>().await
    }
}
