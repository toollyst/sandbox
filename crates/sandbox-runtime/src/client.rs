//! 게스트 연결의 호스트 쪽. 연결 하나에 요청 하나다(`sandbox-proto` 규칙). 모든 함수는 블로킹이다.
//!
//! 게스트가 보낸 것은 믿지 않는다. 순서가 틀리면 [`RuntimeError::Protocol`]이다.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sandbox_executor::{ChannelSpec, Executor, GuestConn};
use sandbox_proto::{Entry, Exec, GuestMsg, HostMsg, PROTOCOL_VERSION};

use crate::error::RuntimeError;
use crate::hostfs;

/// 연결을 다시 시도하는 간격.
pub const RETRY: Duration = Duration::from_millis(20);

fn proto_err(m: impl Into<String>) -> RuntimeError {
    RuntimeError::Protocol(m.into())
}

/// 게스트 하나.
#[derive(Clone)]
pub struct Guest {
    executor: Arc<dyn Executor>,
    channel: ChannelSpec,
}

fn recv(c: &mut dyn GuestConn) -> Result<GuestMsg, RuntimeError> {
    GuestMsg::read_from(c).map_err(|e| match e.kind() {
        io::ErrorKind::InvalidData => proto_err(e.to_string()),
        _ => RuntimeError::Lost(e.to_string()),
    })
}

fn send(c: &mut dyn GuestConn, m: &HostMsg) -> Result<(), RuntimeError> {
    m.write_to(c).map_err(|e| RuntimeError::Lost(e.to_string()))
}

impl Guest {
    /// 만든다.
    pub fn new(executor: Arc<dyn Executor>, channel: ChannelSpec) -> Self {
        Self { executor, channel }
    }

    /// 연결하고 버전을 확인한다.
    pub fn open(&self) -> Result<Box<dyn GuestConn>, RuntimeError> {
        let mut c = self
            .executor
            .connect(&self.channel)
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
            GuestMsg::Started { pid } => Ok((c, pid)),
            GuestMsg::Error(e) => Err(RuntimeError::CommandFailed(e)),
            other => Err(proto_err(format!("expected Started, got {other:?}"))),
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

    /// 연결 하나를 나가는 연결용으로 맡긴다. 게스트가 쓸 때 `Connect`가 온다.
    pub fn egress_slot(&self) -> Result<Box<dyn GuestConn>, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::EgressSlot)?;
        Ok(c)
    }

    /// 기본 프로세스에 시그널(Linux 번호)을 보낸다. 실행 중인 기본 프로세스가 없으면 `Ok(false)`.
    pub fn signal_main(&self, signal: i32) -> Result<bool, RuntimeError> {
        let mut c = self.open()?;
        send(&mut *c, &HostMsg::SignalMain(signal))?;
        match recv(&mut *c)? {
            GuestMsg::Done => Ok(true),
            GuestMsg::Error(e) if e.kind == sandbox_proto::ErrorKind::NotFound => Ok(false),
            GuestMsg::Error(e) => Err(RuntimeError::Guest(e)),
            other => Err(proto_err(format!("expected Done, got {other:?}"))),
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
