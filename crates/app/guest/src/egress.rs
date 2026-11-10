//! 나가는 연결 프록시 (게스트 쪽). VM에는 네트워크 장치가 없다. 나가는 TCP는 모두 여기를 거쳐 호스트가 연다.
//!
//! - [`Egress::listen`]: `127.0.0.1`의 빈 포트에서 HTTP 프록시로 듣는다(`HostMsg::EgressListen`). 한 번만 연다.
//! - 호스트는 연결을 미리 맡겨 둔다(`HostMsg::EgressSlot`, [`Egress::offer`]). 프록시 클라이언트가 오면 맡겨 둔
//!   연결 하나로 `Connect`를 보내고, 호스트가 허용 목록을 확인해 `Connected`나 `Refused`로 답한다.
//!   맡겨 둔 연결이 [`SLOT_WAIT`] 안에 없으면 클라이언트에 503이다.
//! - 받는 요청:
//!   - `CONNECT host:port` (HTTPS 등): 허용되면 `200 Connection established` 뒤로 바이트를 그대로 잇는다.
//!   - 절대 형식 `GET http://host[:port]/...` (평문 HTTP): 허용되면 받은 요청을 그대로 목적지로 보낸다
//!     (서버는 절대 형식을 받아야 한다, RFC 9112 3.2.2). 포트를 생략하면 80이다.
//!   - 그 밖은 400. 머리가 [`MAX_HEAD`]를 넘으면 431.
//! - 거부되면 403과 이유를 본문으로, 호스트가 연결하지 못했으면 502다.
//! - 허용된 뒤로 클라이언트에서 받은 바이트는 `GuestMsg::Data`로 보내고, 클라이언트가 보내기를 끝내면 `GuestMsg::End`.
//!   호스트가 보낸 `HostMsg::Data`는 클라이언트에 쓰고, `HostMsg::End`면 클라이언트 쪽 보내기를 닫는다(반쯤 닫기).
//!   양쪽이 끝나면(또는 어느 쪽이든 오류면) 둘 다 닫는다. 끝을 vsock의 반쯤 닫기에 맡기지 않는다.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sbx_guest_proto::{GuestMsg, HostMsg};

use crate::wire::Conn;

/// 맡겨 둔 연결을 기다리는 시간.
pub const SLOT_WAIT: Duration = Duration::from_secs(10);

/// 요청 머리의 최대 크기.
pub const MAX_HEAD: usize = 16 * 1024;

/// 호스트가 맡긴 연결 하나.
pub struct Slot {
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
    shutdown_both: Box<dyn Fn() + Send + Sync>,
}

impl Slot {
    /// 연결로 만든다.
    pub fn new<C: Conn>(conn: C) -> io::Result<Self> {
        let writer = conn.try_clone()?;
        let full = conn.try_clone()?;
        Ok(Self {
            reader: Box::new(conn),
            writer: Box::new(writer),
            shutdown_both: Box::new(move || {
                let _ = full.shutdown_both();
            }),
        })
    }
}

/// 프록시 상태. 게스트 안에서 하나를 함께 쓴다.
#[derive(Default)]
pub struct Egress {
    port: Mutex<Option<u16>>,
    slots: Mutex<VecDeque<Slot>>,
    ready: Condvar,
}

impl Egress {
    /// 듣기 시작한다. 이미 듣고 있으면 그 포트를 돌려준다.
    pub fn listen(self: &Arc<Self>) -> io::Result<u16> {
        let mut port = self.port.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = *port {
            return Ok(p);
        }
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let p = listener.local_addr()?.port();
        let me = self.clone();
        thread::spawn(move || {
            for client in listener.incoming().flatten() {
                let me = me.clone();
                thread::spawn(move || me.handle(client));
            }
        });
        *port = Some(p);
        Ok(p)
    }

    /// 호스트가 맡긴 연결을 받아 둔다.
    pub fn offer(&self, slot: Slot) {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(slot);
        self.ready.notify_one();
    }

    fn take(&self) -> Option<Slot> {
        let deadline = Instant::now() + SLOT_WAIT;
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(s) = slots.pop_front() {
                return Some(s);
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            slots = self
                .ready
                .wait_timeout(slots, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn handle(&self, mut client: TcpStream) {
        let (head, extra) = match read_head(&mut client) {
            Ok(Some(v)) => v,
            Ok(None) => return reply(&mut client, 431, "request header too large"),
            Err(_) => return,
        };
        let Some(req) = parse_request(&head) else {
            return reply(
                &mut client,
                400,
                "expected CONNECT host:port or an absolute http:// URL",
            );
        };
        // 맡겨 둔 연결이 이미 끊겼을 수 있다(호스트가 다시 맡긴다). 보내기가 되는 것을 쓴다.
        let mut slot = loop {
            let Some(mut s) = self.take() else {
                return reply(&mut client, 503, "no connection from the host is available");
            };
            let msg = GuestMsg::Connect {
                host: req.host.clone(),
                port: req.port,
            };
            if msg
                .write_to(&mut s.writer)
                .and_then(|()| s.writer.flush())
                .is_ok()
            {
                break s;
            }
        };
        match HostMsg::read_from(&mut slot.reader) {
            Ok(HostMsg::Connected) => {}
            Ok(HostMsg::Refused { reason }) => return reply(&mut client, 403, &reason),
            _ => return reply(&mut client, 502, "the host did not answer"),
        }
        let first: Vec<u8> = if req.tunnel {
            if client
                .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                .is_err()
            {
                (slot.shutdown_both)();
                return;
            }
            extra
        } else {
            [head, extra].concat()
        };
        splice(client, slot, &first);
    }
}

/// 요청 하나.
#[derive(Debug, PartialEq, Eq)]
struct Request {
    host: String,
    port: u16,
    /// `CONNECT`인지.
    tunnel: bool,
}

/// 머리(`\r\n\r\n`까지)와 그 뒤에 이미 읽은 바이트. 머리가 너무 크면 `None`.
fn read_head(r: &mut impl Read) -> io::Result<Option<(Vec<u8>, Vec<u8>)>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let extra = buf.split_off(end + 4);
            return Ok(Some((buf, extra)));
        }
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
        let n = r.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// 요청 줄을 읽는다.
fn parse_request(head: &[u8]) -> Option<Request> {
    let line = std::str::from_utf8(head).ok()?.lines().next()?;
    let mut parts = line.split(' ');
    let (method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return None;
    }
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_authority(target)?;
        return Some(Request {
            host,
            port: port?,
            tunnel: true,
        });
    }
    let rest = target.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let (host, port) = split_authority(authority)?;
    Some(Request {
        host,
        port: port.unwrap_or(80),
        tunnel: false,
    })
}

/// `host[:port]`, `[v6][:port]`. 포트가 숫자가 아니면 `None`.
fn split_authority(a: &str) -> Option<(String, Option<u16>)> {
    let (host, port) = if let Some(rest) = a.strip_prefix('[') {
        let (h, after) = rest.split_once(']')?;
        (h, after.strip_prefix(':'))
    } else {
        match a.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') => (h, Some(p)),
            Some(_) => return None,
            None => (a, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(p) => Some(p.parse::<u16>().ok().filter(|&n| n != 0)?),
        None => None,
    };
    Some((host.to_owned(), port))
}

fn reply(client: &mut TcpStream, code: u16, reason: &str) {
    let text = match code {
        400 => "Bad Request",
        403 => "Forbidden",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Service Unavailable",
    };
    let body = format!("{reason}\n");
    let _ = write!(
        client,
        "HTTP/1.1 {code} {text}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// 양쪽을 잇는다. `first`는 목적지로 먼저 보낼 바이트다. 규칙은 모듈 문서에 있다.
fn splice(client: TcpStream, slot: Slot, first: &[u8]) {
    let Slot {
        mut reader,
        mut writer,
        shutdown_both,
    } = slot;
    let Ok(mut client_in) = client.try_clone() else {
        shutdown_both();
        return;
    };
    let mut client_out = client;
    let first = first.to_vec();
    // 클라이언트 → 호스트.
    let up = thread::spawn(move || {
        let mut send = |m: GuestMsg| m.write_to(&mut writer).and_then(|()| writer.flush());
        if !first.is_empty() && send(GuestMsg::Data(first)).is_err() {
            return;
        }
        let mut buf = vec![0u8; sbx_guest_proto::CHUNK_LEN];
        loop {
            match client_in.read(&mut buf) {
                Ok(0) => {
                    let _ = send(GuestMsg::End);
                    return;
                }
                Ok(n) => {
                    if send(GuestMsg::Data(buf[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return,
            }
        }
    });
    // 호스트 → 클라이언트.
    let ok = loop {
        match HostMsg::read_from(&mut reader) {
            Ok(HostMsg::Data(b)) => {
                if client_out.write_all(&b).is_err() {
                    break false;
                }
            }
            Ok(HostMsg::End) => {
                let _ = client_out.shutdown(Shutdown::Write);
                break true;
            }
            _ => break false,
        }
    };
    if !ok {
        // 호스트 쪽이 끊겼다. 클라이언트를 닫아 올라가는 쪽도 끝나게 한다.
        let _ = client_out.shutdown(Shutdown::Both);
    }
    let _ = up.join();
    shutdown_both();
    let _ = client_out.shutdown(Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests() {
        let r = |s: &str| parse_request(s.as_bytes());
        assert_eq!(
            r("CONNECT pypi.org:443 HTTP/1.1\r\nHost: x\r\n\r\n"),
            Some(Request {
                host: "pypi.org".into(),
                port: 443,
                tunnel: true
            })
        );
        assert_eq!(
            r("GET http://example.com/a?b HTTP/1.1\r\n\r\n"),
            Some(Request {
                host: "example.com".into(),
                port: 80,
                tunnel: false
            })
        );
        assert_eq!(
            r("GET http://[::1]:8080/ HTTP/1.0\r\n\r\n").unwrap().host,
            "::1"
        );
        assert_eq!(r("CONNECT [::1]:443 HTTP/1.1\r\n\r\n").unwrap().port, 443);
        for bad in [
            "CONNECT pypi.org HTTP/1.1",
            "GET /path HTTP/1.1",
            "GET https://x/ HTTP/1.1",
            "GET http://:80/ HTTP/1.1",
            "CONNECT a:0 HTTP/1.1",
            "CONNECT a:443 SPDY/3",
            "CONNECT ::1:443 HTTP/1.1",
        ] {
            assert_eq!(r(&format!("{bad}\r\n\r\n")), None, "{bad}");
        }
    }

    #[test]
    fn head_and_extra() {
        let mut input: &[u8] = b"CONNECT a:1 HTTP/1.1\r\n\r\nhello";
        let (head, extra) = read_head(&mut input).unwrap().unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        assert_eq!(extra, b"hello");
        let big = vec![b'a'; MAX_HEAD + 10];
        assert!(read_head(&mut big.as_slice()).unwrap().is_none());
    }
}
