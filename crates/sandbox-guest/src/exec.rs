//! 프로세스 실행 (`HostMsg::Exec`).
//!
//! - 환경은 받은 그대로 쓴다. 예외 하나: `HOME`이 없으면 사용자의 홈을 넣는다(Docker와 같다).
//! - argv의 첫 항목에 `/`가 없으면 받은 환경의 `PATH`로 찾는다.
//! - 작업 디렉터리가 없으면 `InvalidArgument`, 실행 파일이 없으면 `NotFound`,
//!   실행 권한이 없으면 `PermissionDenied`다. 호스트는 뒤의 둘을 Docker처럼 127, 126으로 바꾼다.
//! - 출력은 파이프가 닫힐 때까지 보낸다. 그래서 백그라운드 프로세스가 출력을 쥐고 있으면 그 프로세스가
//!   닫을 때까지 끝나지 않는다(`cmd | cat`과 같다). 출력을 모두 보낸 뒤 `Exited`를 보낸다.
//! - 예외: `stop_output_at_exit`(기본 프로세스)이면 프로세스가 끝난 뒤 파이프가 [`DRAIN_IDLE`] 동안 조용하면,
//!   또는 끝난 뒤 [`DRAIN_MAX`]가 지나면 그만 읽고 `Exited`를 보낸다. 이미 쓴 출력은 파이프에 있으므로 빠지지 않는다.
//!   읽는 쪽을 닫으므로 그 뒤 남은 자식이 쓰면 EPIPE를 받는다.
//! - 호스트가 연결을 닫으면 프로세스를 SIGKILL로 끝낸다.
//! - 끝난 프로세스에는 시그널을 보내지 않는다. PID가 재사용되어 다른 프로세스가 맞는 일을 막는다.
//! - 터미널(`tty`)은 아직 지원하지 않는다. 요청하면 `InvalidArgument`다.

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sandbox_proto::{CHUNK_LEN, Error, ErrorKind, Exec, ExitStatus, GuestMsg, HostMsg};

use crate::passwd;
use crate::wire::{Out, io_error};

/// `stop_output_at_exit`일 때: 프로세스가 끝난 뒤 이만큼 출력이 없으면 그만 읽는다.
pub const DRAIN_IDLE: Duration = Duration::from_millis(50);

/// `stop_output_at_exit`일 때: 프로세스가 끝난 뒤 남은 자식이 계속 써도 이만큼 지나면 그만 읽는다.
pub const DRAIN_MAX: Duration = Duration::from_secs(1);

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
}

impl Proc {
    fn signal(&self, n: i32) {
        let alive = self.alive.lock().unwrap_or_else(|e| e.into_inner());
        if *alive {
            let _ = crate::sys::kill(self.pid, n);
        }
    }
}

/// 실행하고 끝날 때까지 처리한다.
pub fn run(x: Exec, root: &Path, input: impl Read + Send + 'static, out: Out) {
    if let Err(e) = run_inner(x, root, input, &out) {
        out.error(e);
    }
}

fn run_inner(
    x: Exec,
    root: &Path,
    mut input: impl Read + Send + 'static,
    out: &Out,
) -> Result<(), Error> {
    let Some(program) = x.args.first() else {
        return Err(Error::new(ErrorKind::InvalidArgument, "empty argv"));
    };
    if x.tty {
        return Err(Error::new(
            ErrorKind::InvalidArgument,
            "tty is not supported yet",
        ));
    }
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
        .gid(creds.gid)
        .stdin(if x.stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if !x.env.iter().any(|(k, _)| k == "HOME") {
        cmd.env("HOME", &creds.home);
    }
    let mut child = cmd.spawn().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => Error::new(
            ErrorKind::NotFound,
            format!("exec: {program:?}: executable file not found in $PATH"),
        ),
        io::ErrorKind::PermissionDenied => Error::new(
            ErrorKind::PermissionDenied,
            format!("exec: {program:?}: permission denied"),
        ),
        _ => io_error(&format!("exec {program:?}"), &e),
    })?;

    let proc = Arc::new(Proc {
        pid: child.id(),
        alive: Mutex::new(true),
    });
    if out.send(&GuestMsg::Started { pid: proc.pid }).is_err() {
        proc.signal(libc::SIGKILL);
    }

    // stop_output_at_exit가 아니면 끝을 알리지 않는다: 파이프가 닫힐 때까지 읽는다.
    let ended = Arc::new(Ended::default());
    let pumps = [
        child
            .stdout
            .take()
            .map(|s| pump(s, out.clone(), GuestMsg::Stdout, ended.clone())),
        child
            .stderr
            .take()
            .map(|s| pump(s, out.clone(), GuestMsg::Stderr, ended.clone())),
    ];
    let stdin = child.stdin.take();
    {
        let proc = proc.clone();
        thread::spawn(move || control(&mut input, stdin, &proc));
    }

    let status = (|| {
        crate::sys::wait_exited_nowait(proc.pid)?;
        *proc.alive.lock().unwrap_or_else(|e| e.into_inner()) = false;
        child.wait()
    })()
    .map_err(|e| io_error("wait", &e))?;
    if x.stop_output_at_exit {
        ended.mark();
    }
    for p in pumps.into_iter().flatten() {
        let _ = p.join();
    }
    let status = match (status.code(), status.signal()) {
        (Some(c), _) => ExitStatus::Code(c),
        (None, Some(s)) => ExitStatus::Signal(s),
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
            match crate::sys::poll_readable(r.as_fd(), DRAIN_IDLE) {
                Ok(true) => {}
                Ok(false) => match ended.since() {
                    Some(_) => break,
                    None => continue,
                },
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

/// 호스트가 보내는 입력과 제어. 연결이 끊기면 프로세스를 끝낸다.
fn control(input: &mut impl Read, mut stdin: Option<ChildStdin>, proc: &Proc) {
    loop {
        match HostMsg::read_from(input) {
            Ok(HostMsg::Stdin(b)) => {
                if let Some(s) = &mut stdin
                    && s.write_all(&b).is_err()
                {
                    // 프로세스가 입력을 닫았다. 이후 입력은 버린다.
                    stdin = None;
                }
            }
            Ok(HostMsg::StdinClose) => stdin = None,
            Ok(HostMsg::Signal(n)) => proc.signal(n),
            Ok(HostMsg::Resize(_)) => {}
            Ok(_) | Err(_) => {
                proc.signal(libc::SIGKILL);
                return;
            }
        }
    }
}
