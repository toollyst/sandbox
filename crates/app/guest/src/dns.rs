//! 루프백 DNS. 맨 아래 모듈이다: DNS 메시지를 읽고 쓰고, 답은 [`Resolver`]에게 받는다.
//!
//! 샌드박스에는 네트워크 장치가 없다. 이미지에 `/etc/resolv.conf`가 없으면(OCI 이미지는 보통 없다) musl과 glibc는
//! `127.0.0.1:53`에 묻는데, 아무도 듣지 않으면 "연결 거부"를 무시하고 시간이 다 될 때까지(기본 5초) 기다린다.
//! 그래서 여기서 곧바로 답한다.
//!
//! **꺼져 있을 때** (`--network allow`도 `--connect`도 아닐 때, [`Resolver::enabled`]가 거짓): 모든 이름에 곧바로 "없다"(NXDOMAIN).
//! - 결과는 바뀌지 않는다: 이름은 어차피 풀리지 않았다. 같은 실패가 5초 기다리지 않고 바로 난다.
//! - 질의(opcode 0)에는 NXDOMAIN, 다른 opcode에는 NOTIMP로 답한다. 응답에는 질문 부분만 되돌려 담는다.
//!
//! **켜져 있을 때** (투명 TCP, 규칙은 `transparent` 모듈): 질문이 하나인 질의(opcode 0)는 [`Resolver::answer`]의 답으로
//! 응답한다. 답을 만드는 일은 호스트에 물을 수 있어 막히므로 질의마다 스레드 하나에서 하고, 동시에 [`MAX_INFLIGHT`]개를
//! 넘는 질의는 버린다(리졸버가 다시 보낸다). 질문이 하나가 아니거나 다른 opcode면 꺼져 있을 때와 같다.
//! IN이 아닌 class에는 NOTIMP다.
//!
//! - 응답의 답 레코드는 하나뿐이고 이름은 질문을 가리키는 압축 포인터(`0xC00C`)다. TTL은 [`TTL`]이다.
//! - 응답에 EDNS(OPT)는 붙이지 않는다(RFC 6891: 모르는 쪽은 붙이지 않는다). 응답은 512바이트를 넘지 않는다.
//! - 이름 부분은 압축하지 않은 꼴만 받는다(질의는 압축하지 않는다). 글자는 대소문자를 가리지 않고 소문자로 넘긴다.
//!   호스트 이름으로 쓸 수 없는 바이트가 있으면 이름이 없다고 본다(`name`이 `None`).
//! - 루프백에서만 듣는다. `/etc/resolv.conf`를 이 서버로 돌리는 것도 `transparent` 모듈의 일이다(켜질 때만).

use std::net::{Ipv4Addr, Ipv6Addr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// DNS 헤더 길이.
const HEADER: usize = 12;
const NXDOMAIN: u16 = 3;
const NOTIMP: u16 = 4;
const SERVFAIL: u16 = 2;

/// A 레코드.
pub const TYPE_A: u16 = 1;
/// PTR 레코드.
pub const TYPE_PTR: u16 = 12;
/// AAAA 레코드.
pub const TYPE_AAAA: u16 = 28;
/// IN class.
pub const CLASS_IN: u16 = 1;

/// 답 레코드의 TTL(초). 가짜 주소는 VM이 도는 동안 바뀌지 않는다.
pub const TTL: u32 = 600;

/// 동시에 답을 만드는 질의의 수.
pub const MAX_INFLIGHT: usize = 64;

/// 질의 `q`에 대한 NXDOMAIN 응답. 질의로 읽을 수 없으면(응답이거나 잘렸거나 압축 포인터가 있으면) `None`.
pub fn nxdomain(q: &[u8]) -> Option<Vec<u8>> {
    if q.len() < HEADER {
        return None;
    }
    let flags = u16::from_be_bytes([q[2], q[3]]);
    if flags & 0x8000 != 0 {
        return None;
    }
    let opcode = (flags >> 11) & 0xf;
    let questions = u16::from_be_bytes([q[4], q[5]]);
    let mut end = HEADER;
    for _ in 0..questions {
        end = skip_name(q, end)?;
        end += 4;
        if end > q.len() {
            return None;
        }
    }
    let rcode = if opcode == 0 { NXDOMAIN } else { NOTIMP };
    Some(header_only(q, end, flags, rcode))
}

/// `at`에서 시작하는 압축하지 않은 이름을 건너뛴 위치.
fn skip_name(q: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *q.get(at)? as usize;
        at += 1;
        if len == 0 {
            return Some(at);
        }
        // 질의의 이름은 압축하지 않는다. 다른 것은 받지 않는다.
        if len & 0xc0 != 0 {
            return None;
        }
        at += len;
    }
}

/// 질문(`q[..end]`)만 되돌려 담은 응답. QR, 같은 opcode, RD는 그대로, RA.
fn header_only(q: &[u8], end: usize, flags: u16, rcode: u16) -> Vec<u8> {
    let opcode = (flags >> 11) & 0xf;
    let out = 0x8000 | (opcode << 11) | (flags & 0x0100) | 0x0080 | rcode;
    let mut r = q[..end].to_vec();
    r[2..4].copy_from_slice(&out.to_be_bytes());
    r[6..HEADER].fill(0);
    r
}

/// 질문이 하나인 질의.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    /// 헤더와 질문(응답에 그대로 되돌린다).
    raw: Vec<u8>,
    /// 소문자 이름, 끝의 `.` 없이. 호스트 이름으로 쓸 수 없는 바이트가 있으면 `None`.
    pub name: Option<String>,
    /// 레코드 종류.
    pub qtype: u16,
    /// class.
    pub qclass: u16,
}

/// 질문이 하나인 질의(opcode 0)로 읽는다. 아니면 `None`(그때는 [`nxdomain`]이 답한다).
pub fn parse(q: &[u8]) -> Option<Query> {
    if q.len() < HEADER {
        return None;
    }
    let flags = u16::from_be_bytes([q[2], q[3]]);
    if flags & 0x8000 != 0 || (flags >> 11) & 0xf != 0 {
        return None;
    }
    if u16::from_be_bytes([q[4], q[5]]) != 1 {
        return None;
    }
    let mut labels = Vec::new();
    let mut at = HEADER;
    let mut plain = true;
    loop {
        let len = *q.get(at)? as usize;
        at += 1;
        if len == 0 {
            break;
        }
        if len & 0xc0 != 0 {
            return None;
        }
        let label = q.get(at..at + len)?;
        at += len;
        plain &= label
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_');
        labels.push(label);
    }
    let fixed = q.get(at..at + 4)?;
    let qtype = u16::from_be_bytes([fixed[0], fixed[1]]);
    let qclass = u16::from_be_bytes([fixed[2], fixed[3]]);
    let end = at + 4;
    let name = (plain && !labels.is_empty()).then(|| {
        labels
            .iter()
            .map(|l| String::from_utf8_lossy(l).to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(".")
    });
    let name = name.filter(|n| n.len() <= 253);
    Some(Query {
        raw: q[..end].to_vec(),
        name,
        qtype,
        qclass,
    })
}

/// 질의의 답.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// 이름이 없다(NXDOMAIN).
    NxDomain,
    /// 이름은 있고 그 종류의 레코드는 없다(NOERROR, 답 없음).
    NoData,
    /// A 레코드 하나.
    A(Ipv4Addr),
    /// AAAA 레코드 하나.
    Aaaa(Ipv6Addr),
    /// PTR 레코드 하나(이름).
    Ptr(String),
    /// 지금은 답할 수 없다(SERVFAIL).
    ServFail,
    /// 하지 않는 질의(NOTIMP).
    NotImp,
}

/// `q`에 `a`로 답한 응답.
pub fn respond(q: &Query, a: &Answer) -> Vec<u8> {
    let flags = u16::from_be_bytes([q.raw[2], q.raw[3]]);
    let rcode = match a {
        Answer::NxDomain => NXDOMAIN,
        Answer::ServFail => SERVFAIL,
        Answer::NotImp => NOTIMP,
        _ => 0,
    };
    let mut r = header_only(&q.raw, q.raw.len(), flags, rcode);
    let (rtype, rdata): (u16, Vec<u8>) = match a {
        Answer::A(ip) => (TYPE_A, ip.octets().to_vec()),
        Answer::Aaaa(ip) => (TYPE_AAAA, ip.octets().to_vec()),
        Answer::Ptr(name) => match encode_name(name) {
            Some(n) => (TYPE_PTR, n),
            None => return r,
        },
        _ => return r,
    };
    // 답 하나.
    r[6..8].copy_from_slice(&1u16.to_be_bytes());
    r.extend_from_slice(&[0xc0, 0x0c]);
    r.extend_from_slice(&rtype.to_be_bytes());
    r.extend_from_slice(&CLASS_IN.to_be_bytes());
    r.extend_from_slice(&TTL.to_be_bytes());
    r.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    r.extend_from_slice(&rdata);
    r
}

/// 이름을 DNS 꼴로. 이름이 아니면 `None`.
fn encode_name(name: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    (out.len() <= 255).then_some(out)
}

/// 답을 만드는 쪽.
pub trait Resolver: Send + Sync {
    /// 켜져 있는지. 아니면 모든 질의에 [`nxdomain`]으로 답한다.
    fn enabled(&self) -> bool;
    /// 이름 하나의 답. 호스트에 물으면 막힌다. `name`은 소문자이고 끝의 `.`이 없다.
    fn answer(&self, name: &str, qtype: u16) -> Answer;
}

/// 언제나 꺼져 있는 [`Resolver`].
pub struct Off;

impl Resolver for Off {
    fn enabled(&self) -> bool {
        false
    }

    fn answer(&self, _: &str, _: u16) -> Answer {
        Answer::NxDomain
    }
}

/// 켜져 있을 때 질의 하나의 답(class와 이름을 본 뒤 `resolver`에 넘긴다).
pub fn answer_query(q: &Query, resolver: &dyn Resolver) -> Answer {
    if q.qclass != CLASS_IN {
        return Answer::NotImp;
    }
    match &q.name {
        Some(name) => resolver.answer(name, q.qtype),
        None => Answer::NxDomain,
    }
}

/// `sock`으로 오는 질의에 답한다. 돌아오지 않는다(받기가 실패하면 돌아온다).
pub fn serve(sock: &UdpSocket, resolver: Arc<dyn Resolver>) -> std::io::Result<()> {
    let inflight = Arc::new(AtomicUsize::new(0));
    let mut buf = [0u8; 4096];
    loop {
        let (n, from) = sock.recv_from(&mut buf)?;
        let q = &buf[..n];
        let parsed = resolver.enabled().then(|| parse(q)).flatten();
        let Some(query) = parsed else {
            if let Some(r) = nxdomain(q) {
                let _ = sock.send_to(&r, from);
            }
            continue;
        };
        let Some(slot) = Inflight::take(&inflight) else {
            continue;
        };
        let Ok(reply) = sock.try_clone() else {
            continue;
        };
        let resolver = resolver.clone();
        // 스레드를 만들지 못하면 질의를 버린다(리졸버가 다시 보낸다). 자리는 닫힘과 함께 돌아간다.
        let _ = std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let _slot = slot;
                let a = answer_query(&query, &*resolver);
                let _ = reply.send_to(&respond(&query, &a), from);
            });
    }
}

/// 답을 만드는 중인 질의 하나의 자리. 버리면 돌려준다.
struct Inflight(Arc<AtomicUsize>);

impl Inflight {
    fn take(n: &Arc<AtomicUsize>) -> Option<Self> {
        if n.fetch_add(1, Ordering::SeqCst) >= MAX_INFLIGHT {
            n.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(n.clone()))
    }
}

impl Drop for Inflight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
