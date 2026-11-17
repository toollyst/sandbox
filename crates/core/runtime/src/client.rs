//! 게스트 연결의 호스트 쪽. 연결 하나에 요청 하나다(`sbx-guest-proto` 규칙). 모든 함수는 블로킹이다.
//!
//! 게스트가 보낸 것은 믿지 않는다. 순서가 틀리면 [`RuntimeError::Protocol`]이다.
//!
//! 게스트는 답하지 않을 수도 있다(연결을 받기만 하고 멈춘 에이전트). 그래서 연결마다 읽기·쓰기 하나가
//! [`REQUEST_TIMEOUT`]까지만 기다린다(`stop`, `rm -f`, MCP 시간 제한이 게스트에 묶이지 않게). 끝없이 기다려야 하는
//! 흐름(실행 중인 프로세스의 출력, 나가는 연결 자리)은 요청이 받아들여진 뒤 시간 제한을 푼다. 복사는 시간 제한을
//! 둔 채 흐른다(읽기·쓰기 하나마다의 제한이라 큰 파일도 된다).

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sbx_executor::{ChannelSpec, Executor, GuestConn};
use sbx_guest_proto::{CLOCK_ROUNDS, Entry, Exec, GuestMsg, HostMsg, PROTOCOL_VERSION};

use crate::error::RuntimeError;
use crate::{hostfs, layer};

/// 연결을 다시 시도하는 간격.
pub const RETRY: Duration = Duration::from_millis(5);

/// 게스트의 답(과 게스트로 쓰기 하나)을 기다리는 최대 시간.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

fn proto_err(m: impl Into<String>) -> RuntimeError {
    RuntimeError::Protocol(m.into())
}

/// 게스트 하나.
#[derive(Clone)]
pub struct Guest {
    executor: Arc<dyn Executor>,
    channel: ChannelSpec,
    timeout: Duration,
}

fn lost(e: &io::Error) -> RuntimeError {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => RuntimeError::Lost(format!(
            "the guest did not answer within {}s",
            REQUEST_TIMEOUT.as_secs()
        )),
        _ => RuntimeError::Lost(e.to_string()),
    }
}

fn recv(c: &mut dyn GuestConn) -> Result<GuestMsg, RuntimeError> {
    GuestMsg::read_from(c).map_err(|e| match e.kind() {
        io::ErrorKind::InvalidData => proto_err(e.to_string()),
        _ => lost(&e),
    })
}

fn send(c: &mut dyn GuestConn, m: &HostMsg) -> Result<(), RuntimeError> {
    m.write_to(c).map_err(|e| lost(&e))
}

/// 요청이 받아들여진 뒤 끝없이 흐르는 연결의 시간 제한을 푼다.
fn untimed(c: &dyn GuestConn) -> Result<(), RuntimeError> {
    c.set_timeout(None)
        .map_err(|e| RuntimeError::Lost(e.to_string()))
}

impl Guest {
    /// 만든다.
    pub fn new(executor: Arc<dyn Executor>, channel: ChannelSpec) -> Self {
        Self {
            executor,
            channel,
            timeout: REQUEST_TIMEOUT,
        }
    }

    /// 답을 기다리는 시간을 바꾼다(테스트, Unix 소켓 쌍을 쓰는 것만).
    #[cfg(all(test, unix))]
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// 연결하고 버전을 확인한다. 돌려준 연결에는 [`REQUEST_TIMEOUT`]이 걸려 있다.
    pub fn open(&self) -> Result<Box<dyn GuestConn>, RuntimeError> {
        let mut c = self
            .executor
            .connect(&self.channel)
            .map_err(|e| RuntimeError::GuestUnreachable(e.to_string()))?;
        c.set_timeout(Some(self.timeout))
            .map_err(|e| RuntimeError::GuestUnreachable(e.to_string()))?;
        send(
            &mut *c,
            &HostMsg::Hello {
                version: PROTOCOL_VERSION,
            },
        )?;
        match recv(&mut *c)? {
            GuestMsg::HelloAck { .. } => Ok(c),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected HelloAck, got {other:?}"))),
        }
    }

    /// 게스트가 연결을 받을 때까지 기다린다. `alive`가 거짓이 되면(VM이 끝나면) 바로 실패한다.
    pub fn wait_ready(
        &self,
        timeout: Duration,
        alive: &mut dyn FnMut() -> bool,
    ) -> Result<(), RuntimeError> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.open() {
                Ok(_) => return Ok(()),
                Err(RuntimeError::GuestUnreachable(why)) => {
                    if !alive() {
                        return Err(RuntimeError::GuestUnreachable(format!(
                            "the vm exited before the guest was ready ({why})"
                        )));
                    }
                    if Instant::now() >= deadline {
                        return Err(RuntimeError::GuestUnreachable(format!(
                            "guest not ready after {}s ({why})",
                            timeout.as_secs()
                        )));
                    }
                    std::thread::sleep(RETRY);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// 프로세스를 시작한다. 연결과 게스트 PID를 돌려준다.
    pub fn exec(&self, x: Exec) -> Result<(Box<dyn GuestConn>, u32), RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::Exec(x))?;
        match recv(&mut *c)? {
            GuestMsg::Started { pid } => {
                untimed(&*c)?;
                Ok((c, pid))
            }
            GuestMsg::Error(e) => Err(RuntimeError::CommandFailed(e)),
            other => Err(proto_err(format!("expected Started, got {other:?}"))),
        }
    }

    /// 빌드 단계 하나를 실행한다([`HostMsg::ExecStep`]). [`Guest::exec`]와 같이 연결과 PID를 돌려준다.
    pub fn exec_step(&self, x: Exec) -> Result<(Box<dyn GuestConn>, u32), RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::ExecStep(x))?;
        match recv(&mut *c)? {
            GuestMsg::Started { pid } => {
                untimed(&*c)?;
                Ok((c, pid))
            }
            GuestMsg::Error(e) => Err(RuntimeError::CommandFailed(e)),
            other => Err(proto_err(format!("expected Started, got {other:?}"))),
        }
    }

    /// 기본 프로세스 자리를 채운다([`HostMsg::Hold`]). 연결과 PID(0)를 돌려준다.
    pub fn hold(&self) -> Result<(Box<dyn GuestConn>, u32), RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::Hold)?;
        match recv(&mut *c)? {
            GuestMsg::Started { pid } => {
                untimed(&*c)?;
                Ok((c, pid))
            }
            GuestMsg::Error(e) if e.kind == sbx_guest_proto::ErrorKind::Protocol => {
                Err(RuntimeError::Invalid(format!(
                    "the guest init is older than this sbx and cannot run a sandbox without a program ({})",
                    e.message
                )))
            }
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Started, got {other:?}"))),
        }
    }

    /// 붙인 층 디스크 수를 알린다([`HostMsg::CheckLayers`]). 게스트가 다르게 셌으면 오류다(게스트는 VM을 끈다).
    /// 이 메시지를 모르는 게스트는 층 디스크를 하나만 쌓으므로 `count`가 1일 때만 넘어간다.
    pub fn check_layers(&self, count: usize) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        let n = u32::try_from(count)
            .map_err(|_| RuntimeError::Invalid(format!("{count} layer disks")))?;
        send(&mut *c, &HostMsg::CheckLayers { count: n })?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(()),
            GuestMsg::Error(e) if e.kind == sbx_guest_proto::ErrorKind::Protocol => {
                if count == 1 {
                    Ok(())
                } else {
                    Err(RuntimeError::Invalid(format!(
                        "the guest init does not check layer disks and stacks only one; this image needs {count} \
                         (use the guest init that ships with this sbx)"
                    )))
                }
            }
            GuestMsg::Error(e) => Err(RuntimeError::Invalid(format!(
                "the executor did not attach the layer disks sbx gave it: {}",
                e.message
            ))),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 항목들을 게스트 디렉터리 `dest_dir` 아래에 놓는다(`CopyIn`). 항목의 경로, 메타데이터, 내용은 부르는 쪽이 정한다
    /// ([`crate::hostfs::Placed`]). 부모는 앞서 디렉터리로 와야 한다.
    pub fn copy_entries(
        &self,
        dest_dir: &str,
        entries: Vec<crate::hostfs::Placed>,
    ) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::CopyIn {
                dest: dest_dir.to_owned(),
            },
        )?;
        let mut writer = c
            .try_clone_conn()
            .map_err(|e| RuntimeError::Lost(e.to_string()))?;
        let sent = (|| -> Result<(), RuntimeError> {
            for e in entries {
                hostfs::send_placed(e, &mut |m| m.write_to(&mut *writer))?;
            }
            send(&mut *writer, &HostMsg::End)
        })();
        // 이쪽의 내용이 모자라 멈췄으면 게스트의 답을 기다리지 않는다(게스트는 나머지를 기다린다). 연결을 닫는다.
        if let Err(e @ RuntimeError::Invalid(_)) = sent {
            return Err(e);
        }
        match (recv(&mut *c), sent) {
            (Ok(GuestMsg::Done), Ok(())) => Ok(()),
            (Ok(GuestMsg::Error(e)), _) => Err(RuntimeError::Guest(e)),
            (_, Err(e)) => Err(e),
            (Ok(other), Ok(())) => Err(proto_err(format!("expected Done, got {other:?}"))),
            (Err(e), Ok(())) => Err(e),
        }
    }

    /// 떼어 놓고 실행한다([`HostMsg::ExecDetached`]). 게스트 안의 PID.
    pub fn exec_detached(&self, x: Exec) -> Result<u32, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::ExecDetached(x))?;
        match recv(&mut *c) {
            Ok(GuestMsg::Started { pid }) => Ok(pid),
            Ok(GuestMsg::Error(e)) => Err(RuntimeError::CommandFailed(e)),
            Ok(other) => Err(proto_err(format!("expected Started, got {other:?}"))),
            // 이 요청을 모르는 게스트(0.2.0이 띄운 VM)는 답 없이 닫는다.
            Err(RuntimeError::Lost(e)) => Err(RuntimeError::Lost(format!(
                "{e}; the guest may be older than this sbx (the sandbox was started before an upgrade): \
                 restart the sandbox to run detached commands"
            ))),
            Err(e) => Err(e),
        }
    }

    /// `-p`: 게스트 `127.0.0.1:port`로 이어진 연결([`HostMsg::Forward`]). 그 뒤로 Data·End를 주고받는다.
    pub fn forward(&self, port: u16) -> Result<Box<dyn GuestConn>, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::Forward { port })?;
        match recv(&mut *c)? {
            GuestMsg::Done => {
                untimed(&*c)?;
                Ok(c)
            }
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 게스트의 자원 사용량([`HostMsg::Usage`]).
    pub fn usage(&self) -> Result<sbx_guest_proto::Usage, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::Usage)?;
        match recv(&mut *c) {
            Ok(GuestMsg::Usage(u)) => Ok(u),
            Ok(GuestMsg::Error(e)) => Err(RuntimeError::Guest(e)),
            Ok(other) => Err(proto_err(format!("expected Usage, got {other:?}"))),
            Err(RuntimeError::Lost(e)) => Err(RuntimeError::Lost(format!(
                "{e}; the guest may be older than this sbx (the sandbox was started before an upgrade): \
                 restart the sandbox to see its usage"
            ))),
            Err(e) => Err(e),
        }
    }

    /// 경로 정보 (링크를 따라간다).
    pub fn stat(&self, path: &str) -> Result<Option<Entry>, RuntimeError> {
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::Stat {
                path: path.to_owned(),
            },
        )?;
        match recv(&mut *c)? {
            GuestMsg::Stat(s) => Ok(s),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Stat, got {other:?}"))),
        }
    }

    /// 호스트 `src`를 게스트 디렉터리 `dest_dir` 아래에 놓는다. `top`의 뜻은 [`hostfs::send`]와 같다.
    pub fn copy_in(
        &self,
        src: &Path,
        dest_dir: &str,
        top: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::CopyIn {
                dest: dest_dir.to_owned(),
            },
        )?;
        let mut writer = c
            .try_clone_conn()
            .map_err(|e| RuntimeError::Lost(e.to_string()))?;
        let sent = hostfs::send(src, top, &mut |m| m.write_to(&mut *writer))
            .and_then(|()| send(&mut *writer, &HostMsg::End));
        // 보내다 실패했더라도 게스트가 이유를 보냈을 수 있다. 그것을 먼저 본다.
        match (recv(&mut *c), sent) {
            (Ok(GuestMsg::Done), Ok(())) => Ok(()),
            (Ok(GuestMsg::Error(e)), _) => Err(RuntimeError::Guest(e)),
            (_, Err(e)) => Err(e),
            (Ok(other), Ok(())) => Err(proto_err(format!("expected Done, got {other:?}"))),
            (Err(e), Ok(())) => Err(e),
        }
    }

    /// 게스트 `path`를 호스트 디렉터리 `dest_dir` 아래에 `to` 이름으로(없으면 내용만) 놓는다.
    pub fn copy_out(
        &self,
        path: &str,
        dest_dir: &Path,
        to: Option<&str>,
    ) -> Result<(), RuntimeError> {
        let from = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| RuntimeError::Invalid(format!("cannot copy {path:?}")))?
            .to_owned();
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::CopyOut {
                path: path.to_owned(),
            },
        )?;
        hostfs::receive(dest_dir, &from, to, &mut || recv(&mut *c))
    }

    /// 게스트 파일 하나를 메모리로 받는다. 맨 위 심볼릭 링크는 따라가지 않는다. [`hostfs::receive_bytes`].
    pub fn read_bytes(&self, path: &str, limit: u64) -> Result<hostfs::Received, RuntimeError> {
        let from = path
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .ok_or_else(|| RuntimeError::Invalid(format!("cannot read {path:?}")))?
            .to_owned();
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::CopyOut {
                path: path.to_owned(),
            },
        )?;
        hostfs::receive_bytes(&from, limit, &mut || recv(&mut *c))
    }

    /// 실행기가 붙인 공유 폴더를 게스트 `target`에 마운트하게 한다.
    pub fn share(&self, tag: &str, target: &str, readonly: bool) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::Share {
                tag: tag.to_owned(),
                target: target.to_owned(),
                readonly,
            },
        )?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(()),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 공유 폴더 `target` 아래의 바뀐 경로들을 게스트의 파일 감시에 알리게 한다([`HostMsg::ShareChanged`], 규칙은
    /// [`crate::watch`]). 이 메시지를 모르는 게스트는 `Guest(Protocol)` 오류다.
    pub fn share_changed(&self, target: &str, paths: &[String]) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(
            &mut *c,
            &HostMsg::ShareChanged {
                target: target.to_owned(),
                paths: paths.to_vec(),
            },
        )?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(()),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 이름 붙은 볼륨 디스크(`disk`번째)를 `target`에 붙이게 한다(`HostMsg::Volume`). 게스트가 처음이면 포맷하고,
    /// `copy`이고 비었으면 이미지 내용으로 채운다. 실패는 볼륨 이름을 붙여 알린다.
    pub fn volume(
        &self,
        name: &str,
        disk: usize,
        target: &str,
        readonly: bool,
        copy: bool,
    ) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        let disk =
            u32::try_from(disk).map_err(|_| RuntimeError::Invalid(format!("disk {disk}")))?;
        send(
            &mut *c,
            &HostMsg::Volume {
                disk,
                target: target.to_owned(),
                readonly,
                copy,
            },
        )?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(()),
            GuestMsg::Error(e) => Err(RuntimeError::Invalid(format!(
                "cannot attach volume {name} at {target}: {}",
                e.message
            ))),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 게스트 안의 나가는 연결 프록시를 띄우고 그 포트를 받는다.
    pub fn egress_listen(&self) -> Result<u16, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::EgressListen)?;
        match recv(&mut *c)? {
            GuestMsg::Listening { port } => Ok(port),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Listening, got {other:?}"))),
        }
    }

    /// 투명 TCP를 켠다([`HostMsg::Transparent`]). 게스트가 하지 못한 것이 있으면 [`RuntimeError::Guest`]다.
    pub fn transparent(&self, literals: Vec<std::net::SocketAddrV4>) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::Transparent { literals })?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(()),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 연결 하나를 나가는 연결용으로 맡긴다. 게스트가 쓸 때 `Connect`가 온다.
    pub fn egress_slot(&self) -> Result<Box<dyn GuestConn>, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::EgressSlot)?;
        untimed(&*c)?;
        Ok(c)
    }

    /// 기본 프로세스에 시그널(Linux 번호)을 보낸다. 실행 중인 기본 프로세스가 없으면 `Ok(false)`.
    pub fn signal_main(&self, signal: i32) -> Result<bool, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::SignalMain(signal))?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(true),
            GuestMsg::Error(e) if e.kind == sbx_guest_proto::ErrorKind::NotFound => Ok(false),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }

    /// 빌드 층을 하나 연다([`HostMsg::OpenLayer`]). 연 층의 번호.
    pub fn open_layer(&self) -> Result<u32, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::OpenLayer)?;
        match recv(&mut *c)? {
            GuestMsg::LayerOpened { layer } => Ok(layer),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected LayerOpened, got {other:?}"))),
        }
    }

    /// 지금 층을 tar로 `sink`에 쓴다([`HostMsg::ExportLayer`], [`layer::receive`]).
    pub fn export_layer(
        &self,
        sink: &mut dyn io::Write,
        limit: u64,
    ) -> Result<layer::LayerStats, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::ExportLayer)?;
        layer::receive(sink, limit, &mut || recv(&mut *c))
    }

    /// 게스트 시계를 맞춘다([`HostMsg::ClockSync`], 규칙은 [`crate::clock`]). `now`는 호스트 벽시계(Unix 나노초)이고
    /// 묻는 때마다 새로 읽는다. 게스트의 묻기에는 [`CLOCK_ROUNDS`]번까지만 답한다.
    /// 이 메시지를 모르는 게스트(첫 답 없이 닫음)는 `Guest(Protocol)` 오류다.
    pub fn clock_sync(&self, now: &dyn Fn() -> i64) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::ClockSync)?;
        let mut answered = 0;
        loop {
            let m = GuestMsg::read_from(&mut *c).map_err(|e| match e.kind() {
                io::ErrorKind::UnexpectedEof if answered == 0 => {
                    RuntimeError::Guest(sbx_guest_proto::Error::new(
                        sbx_guest_proto::ErrorKind::Protocol,
                        "the guest closed the connection without answering ClockSync",
                    ))
                }
                io::ErrorKind::InvalidData => proto_err(e.to_string()),
                _ => lost(&e),
            })?;
            match m {
                GuestMsg::ClockAsk if answered < CLOCK_ROUNDS => {
                    send(&mut *c, &HostMsg::ClockNow { unix_nanos: now() })?;
                    answered += 1;
                }
                GuestMsg::ClockAsk => {
                    return Err(proto_err(format!(
                        "the guest asked for the time more than {CLOCK_ROUNDS} times"
                    )));
                }
                GuestMsg::Done => return Ok(()),
                GuestMsg::Error(e) => return Err(RuntimeError::Guest(e)),
                other => {
                    return Err(proto_err(format!(
                        "expected ClockAsk or Done, got {other:?}"
                    )));
                }
            }
        }
    }

    /// 게스트에 종료를 요청한다. `Done`을 받으면 돌아온다.
    pub fn shutdown(&self) -> Result<(), RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::Shutdown)?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(()),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Read;
    use std::os::unix::net::UnixStream;
    use std::sync::Mutex;

    use sbx_executor::{Capabilities, ExecutorError, VmConfig, VmHandle};
    use sbx_model::BoxFuture;

    use super::*;

    /// 연결마다 짝의 다른 끝을 `peers`에 남기는 실행기. 짝은 읽기만 하고 답하지 않는다(멈춘 게스트).
    struct Silent {
        peers: Mutex<Vec<UnixStream>>,
    }

    impl Executor for Silent {
        fn capabilities(&self) -> Capabilities {
            unimplemented!()
        }
        fn launch(&self, _: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>> {
            unimplemented!()
        }
        fn connect(&self, _: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
            let (a, b) = UnixStream::pair()?;
            self.peers.lock().unwrap().push(b);
            Ok(Box::new(a))
        }
    }

    /// 연결마다 짝의 다른 끝에서 `guest`를 돌리는 실행기(Hello는 대신 받아 준다).
    struct Scripted(Arc<dyn Fn(UnixStream) + Send + Sync>);

    impl Executor for Scripted {
        fn capabilities(&self) -> Capabilities {
            unimplemented!()
        }
        fn launch(&self, _: VmConfig) -> BoxFuture<'_, Result<Box<dyn VmHandle>, ExecutorError>> {
            unimplemented!()
        }
        fn connect(&self, _: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
            let (a, mut b) = UnixStream::pair()?;
            let guest = self.0.clone();
            std::thread::spawn(move || {
                let ack = GuestMsg::HelloAck {
                    version: PROTOCOL_VERSION,
                };
                if HostMsg::read_from(&mut b).is_ok() && ack.write_to(&mut b).is_ok() {
                    guest(b)
                }
            });
            Ok(Box::new(a))
        }
    }

    fn scripted(guest: impl Fn(UnixStream) + Send + Sync + 'static) -> Guest {
        Guest::new(
            Arc::new(Scripted(Arc::new(guest))),
            ChannelSpec {
                host_endpoint: "x".into(),
                guest_port: 1024,
            },
        )
        .with_timeout(Duration::from_secs(2))
    }

    #[test]
    fn clock_sync_answers_each_ask_with_a_fresh_time() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let g = {
            let seen = seen.clone();
            scripted(move |mut c| {
                assert_eq!(HostMsg::read_from(&mut c).unwrap(), HostMsg::ClockSync);
                for _ in 0..CLOCK_ROUNDS {
                    GuestMsg::ClockAsk.write_to(&mut c).unwrap();
                    seen.lock()
                        .unwrap()
                        .push(HostMsg::read_from(&mut c).unwrap());
                }
                GuestMsg::Done.write_to(&mut c).unwrap();
            })
        };
        let n = std::sync::atomic::AtomicI64::new(100);
        g.clock_sync(&|| n.fetch_add(1, std::sync::atomic::Ordering::SeqCst))
            .unwrap();
        let want: Vec<HostMsg> = (100..100 + i64::from(CLOCK_ROUNDS))
            .map(|unix_nanos| HostMsg::ClockNow { unix_nanos })
            .collect();
        assert_eq!(*seen.lock().unwrap(), want);
    }

    #[test]
    fn an_older_guest_that_closes_is_reported_as_not_knowing_clock_sync() {
        // 0.2.x 게스트는 모르는 요청을 해석하지 못하고 답 없이 닫는다.
        let g = scripted(|mut c| {
            let _ = HostMsg::read_from(&mut c);
        });
        let e = g.clock_sync(&|| 0).unwrap_err();
        assert!(
            matches!(&e, RuntimeError::Guest(g) if g.kind == sbx_guest_proto::ErrorKind::Protocol),
            "{e:?}"
        );
        assert_eq!(crate::clock::kind(&e), crate::clock::Kind::Unsupported);
    }

    #[test]
    fn a_guest_that_keeps_asking_gets_only_so_many_answers() {
        let answers = Arc::new(Mutex::new(0u32));
        let g = {
            let answers = answers.clone();
            scripted(move |mut c| {
                let _ = HostMsg::read_from(&mut c);
                loop {
                    if GuestMsg::ClockAsk.write_to(&mut c).is_err() {
                        return;
                    }
                    match HostMsg::read_from(&mut c) {
                        Ok(HostMsg::ClockNow { .. }) => *answers.lock().unwrap() += 1,
                        _ => return,
                    }
                }
            })
        };
        let e = g.clock_sync(&|| 0).unwrap_err();
        assert!(matches!(e, RuntimeError::Protocol(_)), "{e:?}");
        assert_eq!(*answers.lock().unwrap(), CLOCK_ROUNDS);
        // 엉뚱한 답도 규약 위반이다.
        let g = scripted(|mut c| {
            let _ = HostMsg::read_from(&mut c);
            GuestMsg::Stdout(vec![1; 10]).write_to(&mut c).unwrap();
        });
        assert!(matches!(
            g.clock_sync(&|| 0).unwrap_err(),
            RuntimeError::Protocol(_)
        ));
    }

    #[test]
    fn a_silent_guest_cannot_stall_the_host() {
        let ex = Arc::new(Silent {
            peers: Mutex::new(Vec::new()),
        });
        let g = Guest::new(
            ex.clone(),
            ChannelSpec {
                host_endpoint: "x".into(),
                guest_port: 1024,
            },
        )
        .with_timeout(Duration::from_millis(200));
        let t = Instant::now();
        let e = g.shutdown().unwrap_err();
        assert!(matches!(e, RuntimeError::Lost(_)), "{e:?}");
        assert!(e.to_string().contains("did not answer"), "{e}");
        assert!(t.elapsed() < Duration::from_secs(5));
        // 게스트 쪽은 Hello를 받았다(연결이 닫혀 있어도 버퍼에 남아 있다).
        let mut peer = ex.peers.lock().unwrap().pop().unwrap();
        let mut buf = [0u8; 1];
        assert_eq!(peer.read(&mut buf).unwrap(), 1);
    }
}
