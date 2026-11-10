//! Unix 소켓 위의 vsock 연결 방식 (Firecracker, Cloud Hypervisor와 같다).
//!
//! 클라이언트가 `CONNECT <포트>\n`을 쓰고, 서버가 `OK <숫자>\n`으로 답하면 그 뒤는 게스트 포트와의 바이트 스트림이다.
//! 서버가 답 없이 닫으면 게스트가 그 포트를 듣지 않는 것이다.

use std::io::{self, Read, Write};

/// 줄 하나의 최대 길이. 이보다 길면 규약 위반이다.
const MAX_HANDSHAKE: usize = 64;

fn read_handshake_line(s: &mut impl Read) -> io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if s.read(&mut byte)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "closed during vsock handshake (guest is not listening yet?)",
            ));
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
        if line.len() > MAX_HANDSHAKE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "vsock handshake line too long",
            ));
        }
    }
    String::from_utf8(line)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "vsock handshake is not UTF-8"))
}

/// 이미 연결된 스트림에서 클라이언트 쪽 handshake를 한다.
pub fn handshake(s: &mut (impl Read + Write), port: u32) -> io::Result<()> {
    s.write_all(format!("CONNECT {port}\n").as_bytes())?;
    s.flush()?;
    let line = read_handshake_line(s)?;
    match line.strip_prefix("OK ") {
        Some(n) if n.parse::<u64>().is_ok() => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected vsock handshake reply {line:?}"),
        )),
    }
}

/// 서버 쪽: `CONNECT <포트>`를 읽어 포트를 돌려준다. 받아들이려면 [`ack`]를 부른다.
pub fn accept(s: &mut impl Read) -> io::Result<u32> {
    let line = read_handshake_line(s)?;
    line.strip_prefix("CONNECT ")
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("expected CONNECT <port>, got {line:?}"),
            )
        })
}

/// 서버 쪽: 연결을 받아들인다.
pub fn ack(s: &mut impl Write, port: u32) -> io::Result<()> {
    s.write_all(format!("OK {port}\n").as_bytes())?;
    s.flush()
}

/// Unix 소켓에 연결하고 handshake까지 한다.
#[cfg(unix)]
pub fn connect(socket: &std::path::Path, port: u32) -> io::Result<std::os::unix::net::UnixStream> {
    let mut s = std::os::unix::net::UnixStream::connect(socket)?;
    handshake(&mut s, port)?;
    Ok(s)
}
