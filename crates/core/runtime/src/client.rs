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
use sbx_guest_proto::{Entry, Exec, GuestMsg, HostMsg, PROTOCOL_VERSION};

use crate::error::RuntimeError;
use crate::hostfs;

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

    /// 답을 기다리는 시간을 바꾼다(테스트).
    #[cfg(test)]
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
