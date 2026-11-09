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

    /// 공유 폴더 흉내: 대상 경로에 호스트 경로를 가리키는 심볼릭 링크를 둔다. 대상이 빈 디렉터리면 바꾼다.
    fn link_share(host: &Path, target: &Path) -> io::Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::symlink_metadata(target) {
            Ok(m) if m.is_dir() => std::fs::remove_dir(target)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists", target.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::os::unix::fs::symlink(host, target)
    }

    pub(crate) fn connect(channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Ok(Box::new(vsock::connect(
            &channel.host_endpoint,
            channel.guest_port,
        )?))
    }

    /// 제어 소켓을 열고 게스트 처리를 돌린다. `shares`는 (태그, 호스트 경로)다.
    pub(crate) fn start(
        socket: &Path,
        shares: Vec<(String, PathBuf)>,
    ) -> Result<FakeVm, ExecutorError> {
        let path: PathBuf = socket.to_owned();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let life = Arc::new(Life::default());
        // 열린 연결. VM이 끝나면 모두 닫는다(실제 VM에서 전원이 꺼지면 연결이 사라지는 것과 같다).
        let open: Arc<std::sync::Mutex<Vec<UnixStream>>> = Arc::default();

        let stop: Arc<dyn Fn(&str) + Send + Sync> = {
            let (life, path, open) = (life.clone(), path.clone(), open.clone());
            Arc::new(move |reason: &str| {
                if life.end(reason) {
                    // 기다리는 accept를 깨운다.
                    let _ = UnixStream::connect(&path);
                    let _ = std::fs::remove_file(&path);
                    for c in open.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
                        let _ = c.shutdown(std::net::Shutdown::Both);
                    }
                }
            })
        };
        let ctx = Arc::new(Ctx {
            root: "/".into(),
            on_shutdown: {
                let stop = stop.clone();
                Box::new(move || stop(REASON_GUEST_SHUTDOWN))
            },
            share: Some(Box::new(move |tag: &str, target: &str, _readonly: bool| {
                let host = shares
                    .iter()
                    .find(|(t, _)| t == tag)
                    .map(|(_, h)| h.clone())
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, format!("no share tagged {tag}"))
                    })?;
                link_share(&host, Path::new(target))
            })),
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
                    let open = open.clone();
                    thread::spawn(move || {
                        match vsock::accept(&mut conn) {
                            Ok(port) if port == sandbox_proto::GUEST_PORT => {}
                            // 그 포트를 듣는 게스트가 없다: 답 없이 닫는다.
                            _ => return,
                        }
                        // 받아들인 연결만 VM 종료 때 닫을 목록에 넣는다.
                        if let Ok(c) = conn.try_clone() {
                            open.lock().unwrap_or_else(|e| e.into_inner()).push(c);
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

    pub(crate) fn start(
        _socket: &std::path::Path,
        _shares: Vec<(String, std::path::PathBuf)>,
    ) -> Result<FakeVm, ExecutorError> {
        Err(ExecutorError::Unsupported(
            "the fake guest needs Unix sockets".into(),
        ))
    }
}
