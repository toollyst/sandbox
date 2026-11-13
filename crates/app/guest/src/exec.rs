//! 프로세스 실행 (`HostMsg::Exec`).
//!
//! - 환경은 받은 그대로 쓴다. 예외 하나: `HOME`이 없으면 사용자의 홈을 넣는다(Docker와 같다).
//! - argv의 첫 항목에 `/`가 없으면 받은 환경의 `PATH`로 찾는다.
//! - 작업 디렉터리가 없으면 `InvalidArgument`, 실행 파일이 없으면 `NotFound`,
//!   실행 권한이 없으면 `PermissionDenied`다. 호스트는 뒤의 둘을 Docker처럼 127, 126으로 바꾼다.
//! - 출력은 파이프가 닫힐 때까지 보낸다. 그래서 백그라운드 프로세스가 출력을 쥐고 있으면 그 프로세스가
//!   닫을 때까지 끝나지 않는다(`cmd | cat`과 같다). 출력을 모두 보낸 뒤 `Exited`를 보낸다.
//! - 예외: `main`(기본 프로세스)이면 프로세스가 끝난 뒤 파이프가 [`DRAIN_IDLE`] 동안 조용하면,
//!   또는 끝난 뒤 [`DRAIN_MAX`]가 지나면 그만 읽고 `Exited`를 보낸다. 이미 쓴 출력은 파이프에 있으므로 빠지지 않는다.
//!   읽는 쪽을 닫으므로 그 뒤 남은 자식이 쓰면 EPIPE를 받는다.
//! - 호스트가 연결을 닫으면 프로세스를 SIGKILL로 끝낸다.
//! - 끝난 프로세스에는 시그널을 보내지 않는다. PID가 재사용되어 다른 프로세스가 맞는 일을 막는다.
//! - 호스트가 보내는 시그널 번호는 Linux 번호다(`sys::from_linux_signal`이 이 OS의 번호로 바꾼다).
//! - `main`이면 [`Main`]에 등록해 `SignalMain`이 찾게 한다. 끝나면 지운다.
//! - 터미널(`tty`): 의사 터미널을 열어 표준 입력, 출력, 오류를 모두 거기에 붙이고, 프로세스는 새 세션을 만들어
//!   그 터미널을 제어 터미널로 삼는다. 출력은 하나로 합쳐져 `Stdout`으로만 간다(터미널이 바꾼 그대로, 예: `\n` → `\r\n`).
//!   크기는 `size`(없으면 24x80)로 시작하고 `Resize`로 바꾼다. `stdin`이 아니면 받은 입력을 버린다.
//!   `StdinClose`는 터미널에 EOF 문자(^D)를 쓴다(터미널에는 닫을 입력 파이프가 없다).
//!   tty가 아니면 `Resize`는 무시한다.
//!
//! **빌드 단계** (`HostMsg::ExecStep`, [`run_step`]): `Exec`와 같고 끝이 다르다. 프로세스는 자기 프로세스 그룹에서
//! 시작한다. 프로세스가 끝나면 그 단계가 남긴 프로세스를 SIGKILL로 끝내고([`Sweep`]), 모두 사라진 뒤에 남은 출력을
//! 보내고 `Exited`를 보낸다. 남은 프로세스가 죽었으므로 출력 파이프도 닫혀 기다림이 끝난다. [`SWEEP_TIMEOUT`] 안에
//! 사라지지 않으면 오류로 끝난다. 남은 프로세스가 층을 쥐면 다음 층을 열 수 없기 때문이다(`overlay` 모듈).
//!
//! **기본 프로세스 자리** (`HostMsg::Hold`, [`hold`]): 프로세스 없이 [`Main`]에 등록하고 `Started { pid: 0 }`을 보낸다.
//! `SignalMain`이나 그 연결의 `Signal`을 받으면 그 시그널로 끝난 것으로 `Exited`를 보낸다. 연결이 닫혀도 끝난다.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sbx_guest_proto::{CHUNK_LEN, Error, ErrorKind, Exec, ExitStatus, GuestMsg, HostMsg, WinSize};

use crate::passwd;
use crate::wire::{Out, io_error};

/// `main`일 때: 프로세스가 끝난 뒤 이만큼 출력이 없으면 그만 읽는다.
pub const DRAIN_IDLE: Duration = Duration::from_millis(50);

/// `main`일 때: 프로세스가 끝난 뒤 남은 자식이 계속 써도 이만큼 지나면 그만 읽는다.
pub const DRAIN_MAX: Duration = Duration::from_secs(1);

/// 빌드 단계가 남긴 프로세스가 사라지기를 기다리는 시간.
pub const SWEEP_TIMEOUT: Duration = Duration::from_secs(10);

/// 빌드 단계(`ExecStep`)가 끝난 뒤 남은 프로세스를 끝내는 방법.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sweep {
    /// VM 안의 모든 프로세스(PID 1과 연결 처리 프로세스 빼고, 커널 스레드는 시그널을 받지 않는다). 단계가 새 세션이나
    /// 그룹으로 떼어 낸 데몬도 끝난다. 빌드 VM에는 단계의 프로세스밖에 없다. 게스트(PID 1의 자식)에서만 쓴다.
    Everything,
    /// 단계의 프로세스 그룹만. 호스트에서 도는 fake 실행기와 테스트가 쓴다(호스트의 다른 프로세스를 건드리지 않는다).
    /// 그룹을 떠난 프로세스(`setsid`)는 남는다.
    Group,
}

/// 끝을 알리는 표시. 프로세스가 끝난 시각을 한 번 남긴다.
#[derive(Default)]
struct Ended {
    set: AtomicBool,
    at: Mutex<Option<Instant>>,
}

impl Ended {
    fn mark(&self) {
        *self.at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        self.set.store(true, Ordering::Release);
    }

    fn since(&self) -> Option<Duration> {
        if !self.set.load(Ordering::Acquire) {
            return None;
        }
        self.at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|t| t.elapsed())
    }
}

struct Proc {
    pid: u32,
    alive: Mutex<bool>,
    /// 기본 프로세스 자리([`hold`])면 시그널(Linux 번호)을 보낼 곳.
    hold: Option<Mutex<std::sync::mpsc::Sender<Option<i32>>>>,
}

impl Proc {
    /// 호스트 번호의 시그널을 보낸다. 이미 끝났으면 보내지 않고 `false`.
    fn signal(&self, n: i32) -> bool {
        let alive = self.alive.lock().unwrap_or_else(|e| e.into_inner());
        if *alive {
            let _ = crate::sys::kill(self.pid, n);
        }
        *alive
    }
}

/// 실행 중인 기본 프로세스. 게스트 안에서 하나를 함께 쓴다.
#[derive(Default)]
pub struct Main(Mutex<Option<Arc<Proc>>>);

impl Main {
    /// Linux 번호의 시그널을 보낸다. 실행 중인 기본 프로세스가 없으면 `false`.
    pub fn signal(&self, linux: i32) -> bool {
        let current = self.0.lock().unwrap_or_else(|e| e.into_inner()).clone();
        current.is_some_and(|p| match &p.hold {
            Some(tx) => tx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .send(Some(linux))
                .is_ok(),
            None => p.signal(crate::sys::from_linux_signal(linux)),
        })
    }

    fn set(&self, p: &Arc<Proc>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(p.clone());
    }

    fn clear(&self, p: &Arc<Proc>) {
        let mut current = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if current.as_ref().is_some_and(|c| Arc::ptr_eq(c, p)) {
            *current = None;
        }
    }
}

/// 실행하고 끝날 때까지 처리한다.
pub fn run(x: Exec, root: &Path, input: impl Read + Send + 'static, out: Out, main: &Main) {
    if let Err(e) = run_inner(x, root, input, &out, main, None) {
        out.error(e);
    }
}

/// 빌드 단계 하나를 실행한다(`HostMsg::ExecStep`). 끝나면 단계가 남긴 프로세스를 `sweep`대로 끝낸다(모듈 문서).
pub fn run_step(
    x: Exec,
    root: &Path,
    input: impl Read + Send + 'static,
    out: Out,
    main: &Main,
    sweep: Sweep,
) {
    if x.main {
        return out.error(Error::new(
            ErrorKind::InvalidArgument,
            "a build step is not the main process",
        ));
    }
    if let Err(e) = run_inner(x, root, input, &out, main, Some(sweep)) {
        out.error(e);
    }
}

/// 기본 프로세스 자리를 채운다(`HostMsg::Hold`, 모듈 문서).
pub fn hold(mut input: impl Read + Send + 'static, out: Out, main: &Main) {
    let (tx, rx) = std::sync::mpsc::channel::<Option<i32>>();
    let proc = Arc::new(Proc {
        pid: 0,
        alive: Mutex::new(true),
        hold: Some(Mutex::new(tx.clone())),
    });
    main.set(&proc);
    if out.send(&GuestMsg::Started { pid: 0 }).is_err() {
        main.clear(&proc);
        return;
    }
    thread::spawn(move || {
        loop {
            match HostMsg::read_from(&mut input) {
                Ok(HostMsg::Signal(n)) => {
                    let _ = tx.send(Some(n));
                }
                Ok(_) => {}
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            }
        }
    });
    let got = rx.recv().ok().flatten();
    main.clear(&proc);
    if let Some(n) = got {
        let _ = out.send(&GuestMsg::Exited(ExitStatus::Signal(n)));
    }
}

/// 단계가 남긴 프로세스를 끝내고 사라지기를 기다린다. `pgid`는 단계 프로세스(이미 끝나 거둔 것)의 그룹이다.
fn sweep(how: Sweep, pgid: u32) -> Result<(), Error> {
    let deadline = Instant::now() + SWEEP_TIMEOUT;
    loop {
        let left = match how {
            Sweep::Group => {
                let _ = crate::sys::kill_group(pgid, libc::SIGKILL);
                crate::sys::group_alive(pgid)
            }
            Sweep::Everything => sweep_everything()?,
        };
        if !left {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                ErrorKind::Io,
                format!(
                    "processes left by the build step did not end within {}s",
                    SWEEP_TIMEOUT.as_secs()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// 모두에게 SIGKILL을 보내고, 아직 살아 있는 프로세스(좀비가 아닌 사용자 프로세스)가 있는지.
#[cfg(target_os = "linux")]
fn sweep_everything() -> Result<bool, Error> {
    crate::sys::linux::kill_everyone_else(libc::SIGKILL).map_err(|e| io_error("kill", &e))?;
    let me = std::process::id();
    let dir = std::fs::read_dir("/proc").map_err(|e| io_error("/proc", &e))?;
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if let Ok(stat) = std::fs::read_to_string(entry.path().join("stat"))
            && is_live_user_process(pid, &stat, me)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(target_os = "linux"))]
fn sweep_everything() -> Result<bool, Error> {
    Err(Error::new(
        ErrorKind::InvalidArgument,
        "ending every process is only done inside the VM",
    ))
}

/// `/proc/<pid>/stat` 한 줄로 본다: PID 1, `me`, 커널 스레드(PID 2와 그 자식), 좀비(`Z`)와 죽은 것(`X`)이 아니면 참.
/// 이름(`comm`)에 괄호와 공백이 있을 수 있으므로 마지막 `)` 뒤를 읽는다.
pub fn is_live_user_process(pid: u32, stat: &str, me: u32) -> bool {
    if pid <= 2 || pid == me {
        return false;
    }
    let Some(rest) = stat.rfind(')').map(|i| &stat[i + 1..]) else {
        return false;
    };
    let mut f = rest.split_ascii_whitespace();
    let state = f.next().unwrap_or("");
    let ppid: u32 = f.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    ppid != 2 && !matches!(state, "Z" | "X" | "x")
}

/// 떼어 놓고 실행한다(`HostMsg::ExecDetached`): 표준 입출력은 `/dev/null`, `Started`를 보내고 곧바로 끝난다.
/// 연결이 닫혀도 프로세스는 계속 돈다. 이 프로세스(연결 처리)가 부모이므로 스레드 하나가 거둔다.
pub fn run_detached(x: Exec, root: &Path, out: Out) {
    let started = (|| {
        if x.tty || x.stdin || x.main {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                "a detached process has no terminal or input and is not the main process",
            ));
        }
        let mut cmd = command(&x, root)?;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // 자기 프로세스 그룹: 연결 처리 쪽 그룹에 보내는 시그널을 받지 않는다.
        cmd.process_group(0);
        let child = cmd.spawn().map_err(|e| spawn_error(&x.args[0], e))?;
        Ok(child)
    })();
    match started {
        Ok(mut child) => {
            let _ = out.send(&GuestMsg::Started { pid: child.id() });
            thread::spawn(move || child.wait());
        }
        Err(e) => out.error(e),
    }
}

/// 받은 그대로 [`Command`]를 만든다: argv, 환경, 작업 디렉터리, 사용자. 입출력은 부르는 쪽이 정한다.
fn command(x: &Exec, root: &Path) -> Result<Command, Error> {
    let Some(program) = x.args.first() else {
        return Err(Error::new(ErrorKind::InvalidArgument, "empty argv"));
    };
    if !x.cwd.starts_with('/') {
        return Err(Error::new(
            ErrorKind::InvalidArgument,
            format!("working directory must be absolute: {:?}", x.cwd),
        ));
    }
    if !Path::new(&x.cwd).is_dir() {
        return Err(Error::new(
            ErrorKind::InvalidArgument,
            format!("working directory {:?} does not exist", x.cwd),
        ));
    }
    let creds = passwd::resolve(root, &x.user)?;

    let mut cmd = Command::new(program);
    cmd.args(&x.args[1..])
        .env_clear()
        .envs(x.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&x.cwd)
        .uid(creds.uid)
        .gid(creds.gid);
    if !x.env.iter().any(|(k, _)| k == "HOME") {
        cmd.env("HOME", &creds.home);
    }
    Ok(cmd)
}

fn spawn_error(program: &str, e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::NotFound => Error::new(
            ErrorKind::NotFound,
            format!("exec: {program:?}: executable file not found in $PATH"),
        ),
        io::ErrorKind::PermissionDenied => Error::new(
            ErrorKind::PermissionDenied,
            format!("exec: {program:?}: permission denied"),
        ),
        _ => io_error(&format!("exec {program:?}"), &e),
    }
}

fn run_inner(
    x: Exec,
    root: &Path,
    mut input: impl Read + Send + 'static,
    out: &Out,
    main: &Main,
    step: Option<Sweep>,
) -> Result<(), Error> {
    let mut cmd = command(&x, root)?;
    if step.is_some() {
        // 단계의 프로세스 그룹. Sweep::Group은 이 그룹을 끝낸다.
        cmd.process_group(0);
    }
    let program = &x.args[0];
    // 터미널이면 주 쪽을 쥐고, 종 쪽을 자식의 표준 입출력으로 준다.
    let tty = if x.tty {
        let size = x.size.unwrap_or(WinSize { rows: 24, cols: 80 });
        let (master, slave) =
            crate::sys::openpty(size.rows, size.cols).map_err(|e| io_error("openpty", &e))?;
        let dup = |fd: &OwnedFd| fd.try_clone().map_err(|e| io_error("openpty", &e));
        cmd.stdin(Stdio::from(dup(&slave)?))
            .stdout(Stdio::from(dup(&slave)?))
            .stderr(Stdio::from(slave));
        crate::sys::controlling_tty_on_stdin(&mut cmd);
        Some(File::from(master))
    } else {
        cmd.stdin(if x.stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        None
    };
    let spawned = cmd.spawn();
    // 부모가 쥔 종 쪽을 닫는다. 그래야 자식들이 모두 닫으면 주 쪽에서 끝을 본다.
    drop(cmd);
    let mut child = spawned.map_err(|e| spawn_error(program, e))?;

    let proc = Arc::new(Proc {
        pid: child.id(),
        alive: Mutex::new(true),
        hold: None,
    });
    if x.main {
        main.set(&proc);
    }
    if out.send(&GuestMsg::Started { pid: proc.pid }).is_err() {
        proc.signal(libc::SIGKILL);
    }

    // main이 아니면 끝을 알리지 않는다: 파이프가 닫힐 때까지 읽는다.
    let ended = Arc::new(Ended::default());
    let (pumps, stdin) = match tty {
        Some(master) => {
            let writer = master.try_clone().map_err(|e| io_error("tty", &e))?;
            let pump = pump(master, out.clone(), GuestMsg::Stdout, ended.clone());
            (
                [Some(pump), None],
                Input::Tty {
                    master: writer,
                    forward: x.stdin,
                },
            )
        }
        None => (
            [
                child
                    .stdout
                    .take()
                    .map(|s| pump(s, out.clone(), GuestMsg::Stdout, ended.clone())),
                child
                    .stderr
                    .take()
                    .map(|s| pump(s, out.clone(), GuestMsg::Stderr, ended.clone())),
            ],
            match child.stdin.take() {
                Some(s) => Input::Pipe(s),
                None => Input::None,
            },
        ),
    };
    {
        let proc = proc.clone();
        thread::spawn(move || control(&mut input, stdin, &proc));
    }

    let status = (|| {
        crate::sys::wait_exited_nowait(proc.pid)?;
        *proc.alive.lock().unwrap_or_else(|e| e.into_inner()) = false;
        child.wait()
    })()
    .map_err(|e| io_error("wait", &e));
    if x.main {
        main.clear(&proc);
    }
    let status = status?;
    if let Some(how) = step {
        // 남은 프로세스가 출력 파이프를 쥐고 있을 수 있다. 끝내야 아래의 읽기가 끝난다.
        sweep(how, proc.pid)?;
    }
    if x.main {
        ended.mark();
    }
    for p in pumps.into_iter().flatten() {
        let _ = p.join();
    }
    let status = match (status.code(), status.signal()) {
        (Some(c), _) => ExitStatus::Code(c),
        (None, Some(s)) => ExitStatus::Signal(crate::sys::to_linux_signal(s)),
        (None, None) => ExitStatus::Code(-1),
    };
    let _ = out.send(&GuestMsg::Exited(status));
    Ok(())
}

/// 파이프를 읽어 보낸다. 닫히면 끝난다. `ended`가 표시되면 [`DRAIN_IDLE`], [`DRAIN_MAX`] 규칙으로 일찍 끝난다.
fn pump(
    mut r: impl Read + AsFd + Send + 'static,
    out: Out,
    wrap: fn(Vec<u8>) -> GuestMsg,
    ended: Arc<Ended>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = vec![0; CHUNK_LEN];
        loop {
            // 막히는 read 대신 기다렸다가 읽는다. 그래야 끝 표시를 볼 수 있다.
            // 끝 표시는 기다리기 전에 본다. 기다리는 동안 프로세스가 쓰고 끝나면, 기다린 뒤에 본 끝 표시로
            // 그만두어 이미 파이프에 있는 출력을 버리게 된다.
            let was_ended = ended.since().is_some();
            match crate::sys::poll_readable(r.as_fd(), DRAIN_IDLE) {
                Ok(true) => {}
                Ok(false) if was_ended => break,
                Ok(false) => continue,
                Err(_) => break,
            }
            if ended.since().is_some_and(|d| d >= DRAIN_MAX) {
                break;
            }
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if out.send(&wrap(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    })
}

/// 프로세스의 입력 쪽.
enum Input {
    /// 입력이 없음 (`/dev/null`).
    None,
    /// 입력 파이프. 닫으면 프로세스는 EOF를 본다.
    Pipe(ChildStdin),
    /// 터미널의 주 쪽. `forward`가 아니면 받은 입력을 버린다.
    Tty { master: File, forward: bool },
}

impl Input {
    fn write(&mut self, b: &[u8]) {
        let failed = match self {
            Input::None | Input::Tty { forward: false, .. } => false,
            Input::Pipe(s) => s.write_all(b).is_err(),
            Input::Tty { master, .. } => master.write_all(b).is_err(),
        };
        if failed {
            // 프로세스가 입력을 닫았다. 이후 입력은 버린다.
            *self = Input::None;
        }
    }

    fn close(&mut self) {
        match self {
            Input::Pipe(_) => *self = Input::None,
            // 터미널은 EOF 문자를 받으면 읽는 쪽에 끝을 알린다(정규 모드).
            Input::Tty {
                master,
                forward: true,
            } => {
                let _ = master.write_all(&[0x04]);
            }
            Input::None | Input::Tty { .. } => {}
        }
    }

    fn resize(&self, s: WinSize) {
        if let Input::Tty { master, .. } = self {
            let _ = crate::sys::set_winsize(master.as_fd(), s.rows, s.cols);
        }
    }
}

/// 호스트가 보내는 입력과 제어. 연결이 끊기면 프로세스를 끝낸다.
fn control(input: &mut impl Read, mut stdin: Input, proc: &Proc) {
    loop {
        match HostMsg::read_from(input) {
            Ok(HostMsg::Stdin(b)) => stdin.write(&b),
            Ok(HostMsg::StdinClose) => stdin.close(),
            Ok(HostMsg::Signal(n)) => {
                proc.signal(crate::sys::from_linux_signal(n));
            }
            Ok(HostMsg::Resize(s)) => stdin.resize(s),
            Ok(_) | Err(_) => {
                proc.signal(libc::SIGKILL);
                return;
            }
        }
    }
}
