//! 가짜 게스트: 제어 소켓에서 vsock 방식으로 연결을 받아 `sandbox-guest`의 연결 처리를 돌린다.

#[cfg(not(unix))]
pub(crate) use other::{connect, start};
#[cfg(unix)]
pub(crate) use unix::{connect, start};

#[cfg(unix)]
mod unix {
    use std::io;
    use std::sync::Arc;

    use sandbox_executor::{ChannelSpec, ExecutorError, GuestConn};

    use crate::vm::{FakeVm, Life, REASON_GUEST_SHUTDOWN};

    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::thread;

    use executor_proto::vsock;
    use sandbox_guest::server::{self, Ctx};

    pub(crate) fn connect(channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Ok(Box::new(vsock::connect(
            &channel.host_endpoint,
            channel.guest_port,
        )?))
    }

    /// 제어 소켓을 열고 게스트 처리를 돌린다.
    pub(crate) fn start(socket: &Path) -> Result<FakeVm, ExecutorError> {
        let path: PathBuf = socket.to_owned();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let life = Arc::new(Life::default());

        let stop: Arc<dyn Fn(&str) + Send + Sync> = {
            let (life, path) = (life.clone(), path.clone());
            Arc::new(move |reason: &str| {
                if life.end(reason) {
                    // 기다리는 accept를 깨운다.
                    let _ = UnixStream::connect(&path);
                    let _ = std::fs::remove_file(&path);
                }
            })
        };
        let ctx = Arc::new(Ctx {
            root: "/".into(),
            on_shutdown: {
                let stop = stop.clone();
                Box::new(move || stop(REASON_GUEST_SHUTDOWN))
            },
        });
        {
            let life = life.clone();
            thread::spawn(move || {
                for conn in listener.incoming() {
                    if life.reason().is_some() {
                        break;
                    }
                    let Ok(mut conn) = conn else { continue };
                    let ctx = ctx.clone();
                    thread::spawn(move || {
                        match vsock::accept(&mut conn) {
                            Ok(port) if port == sandbox_proto::GUEST_PORT => {}
                            // 그 포트를 듣는 게스트가 없다: 답 없이 닫는다.
                            _ => return,
                        }
                        if vsock::ack(&mut conn, port_ack()).is_ok() {
                            server::serve(conn, &ctx);
                        }
                    });
                }
            });
        }
        Ok(FakeVm { life, stop })
    }

    /// `OK` 뒤의 숫자. 규약은 아무 숫자나 허용한다. 호스트 쪽 임시 포트를 흉내 낸다.
    fn port_ack() -> u32 {
        1_073_741_824
    }
}

#[cfg(not(unix))]
mod other {
    use std::io;

    use sandbox_executor::{ChannelSpec, ExecutorError, GuestConn};

    use crate::vm::FakeVm;

    pub(crate) fn connect(_channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the fake guest needs Unix sockets",
        ))
    }

    pub(crate) fn start(_socket: &std::path::Path) -> Result<FakeVm, ExecutorError> {
        Err(ExecutorError::Unsupported(
            "the fake guest needs Unix sockets".into(),
        ))
    }
}
