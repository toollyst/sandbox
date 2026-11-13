//! 투명 TCP 흉내([`crate::net_dir`])를 테스트에서 쓰는 도구: "게스트 안에서 프록시를 모르는 프로그램"이 하는 일을
//! 대신한다. 루프백 DNS에 A 질의를 보내 가짜 주소를 받고, 경로 표로 그 주소가 실제로 듣는 곳을 찾아 붙는다.
//! 실제 VM에서는 커널의 local 경로가 경로 표 노릇을 한다.

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// DNS 답.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// rcode(0 NOERROR, 2 SERVFAIL, 3 NXDOMAIN).
    pub rcode: u8,
    /// A 레코드의 주소들.
    pub addrs: Vec<Ipv4Addr>,
}

/// `root` 아래에서 [`crate::net_dir`] 디렉터리를 찾는다(DNS 포트 파일이 있는 첫 것).
pub fn find(root: &Path) -> Option<PathBuf> {
    let mut stack = vec![root.to_owned()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if !t.is_dir() {
                continue;
            }
            if p.extension().is_some_and(|x| x == "net") && p.join(crate::NET_DNS).is_file() {
                return Some(p);
            }
            stack.push(p);
        }
    }
    None
}

/// `dir`의 루프백 DNS에 `name`의 `qtype` 질의를 보낸다.
pub fn lookup(dir: &Path, name: &str, qtype: u16) -> io::Result<Reply> {
    let port: u16 = std::fs::read_to_string(dir.join(crate::NET_DNS))?
        .trim()
        .parse()
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    let mut q = vec![0x51, 0x7a, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for l in name.split('.') {
        q.push(l.len() as u8);
        q.extend_from_slice(l.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&1u16.to_be_bytes());
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    sock.set_read_timeout(Some(Duration::from_secs(10)))?;
    sock.send_to(&q, ("127.0.0.1", port))?;
    let mut buf = [0u8; 512];
    let (n, _) = sock.recv_from(&mut buf)?;
    let r = &buf[..n];
    let bad = || io::Error::from(io::ErrorKind::InvalidData);
    if r.len() < q.len() || r[..2] != q[..2] {
        return Err(bad());
    }
    let answers = u16::from_be_bytes([r[6], r[7]]);
    let mut at = q.len();
    let mut addrs = Vec::new();
    for _ in 0..answers {
        // 이름은 질문을 가리키는 포인터(2바이트)다.
        let rr = r.get(at..at + 12).ok_or_else(bad)?;
        let rtype = u16::from_be_bytes([rr[2], rr[3]]);
        let len = u16::from_be_bytes([rr[10], rr[11]]) as usize;
        let data = r.get(at + 12..at + 12 + len).ok_or_else(bad)?;
        if rtype == 1 && len == 4 {
            addrs.push(Ipv4Addr::new(data[0], data[1], data[2], data[3]));
        }
        at += 12 + len;
    }
    Ok(Reply {
        rcode: r[3] & 0xf,
        addrs,
    })
}

/// 가짜 주소 `addr`이 실제로 듣는 곳. 듣지 않으면 `None`.
pub fn route(dir: &Path, addr: SocketAddrV4) -> Option<SocketAddr> {
    let table = std::fs::read_to_string(dir.join(crate::NET_ROUTES)).ok()?;
    table.lines().find_map(|l| {
        let (from, to) = l.split_once(' ')?;
        (from.parse::<SocketAddrV4>().ok()? == addr).then(|| to.parse().ok())?
    })
}

/// 프록시를 모르는 프로그램처럼 `name:port`에 붙는다: DNS로 주소를 받고 그 주소의 그 포트로.
/// 이름이 없으면 `NotFound`, 듣지 않는 포트면 `ConnectionRefused`다.
pub fn connect(dir: &Path, name: &str, port: u16) -> io::Result<TcpStream> {
    let reply = lookup(dir, name, 1)?;
    let Some(ip) = reply.addrs.first() else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{name}: rcode {}", reply.rcode),
        ));
    };
    let to = route(dir, SocketAddrV4::new(*ip, port))
        .ok_or_else(|| io::Error::from(io::ErrorKind::ConnectionRefused))?;
    TcpStream::connect(to)
}
