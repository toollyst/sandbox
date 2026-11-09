//! 연결 위에서 메시지를 주고받는 도구. `exec`, `copy`, `server`가 함께 쓰는 가장 아래 층이다.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};

use sandbox_proto::{Error, ErrorKind, GuestMsg, HostMsg};

/// 양방향 연결. 읽는 쪽과 쓰는 쪽을 다른 스레드에서 쓰려고 복제할 수 있어야 한다.
pub trait Conn: Read + Write + Send + Sized + 'static {
    /// 같은 연결을 가리키는 다른 핸들.
    fn try_clone(&self) -> io::Result<Self>;
}

impl Conn for UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }
}

/// 게스트의 vsock 연결은 fd를 그대로 읽고 쓰므로 `File`로 다룬다.
impl Conn for std::fs::File {
    fn try_clone(&self) -> io::Result<Self> {
        std::fs::File::try_clone(self)
    }
}

/// 여러 스레드가 함께 쓰는 보내는 쪽. 메시지 하나는 잠금 하나 안에서 통째로 쓴다.
#[derive(Clone)]
pub struct Out(Arc<Mutex<Box<dyn Write + Send>>>);

impl Out {
    /// 감싼다.
    pub fn new(w: impl Write + Send + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(w))))
    }

    /// 메시지 하나를 보낸다.
    pub fn send(&self, msg: &GuestMsg) -> io::Result<()> {
        let mut w = self.0.lock().unwrap_or_else(|e| e.into_inner());
        msg.write_to(&mut *w)?;
        w.flush()
    }

    /// 오류를 보낸다. 보내기 실패는 무시한다(상대가 이미 끊은 경우).
    pub fn error(&self, e: Error) {
        let _ = self.send(&GuestMsg::Error(e));
    }
}

/// 입출력 오류를 프로토콜 오류로.
pub fn io_error(context: &str, e: &io::Error) -> Error {
    Error::new(ErrorKind::from_io(e.kind()), format!("{context}: {e}"))
}

/// 순서가 틀린 메시지.
pub fn protocol(got: &HostMsg, expected: &str) -> Error {
    let name = format!("{got:?}");
    let name = name.split(['(', ' ', '{']).next().unwrap_or("?");
    Error::new(
        ErrorKind::Protocol,
        format!("expected {expected}, got {name}"),
    )
}
