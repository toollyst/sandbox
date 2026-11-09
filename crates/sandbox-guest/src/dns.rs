//! 루프백 DNS: 모든 이름에 곧바로 "없다"(NXDOMAIN)고 답한다. 맨 아래 모듈이다.
//!
//! 샌드박스에는 네트워크 장치가 없다. 이미지에 `/etc/resolv.conf`가 없으면(OCI 이미지는 보통 없다) musl과 glibc는
//! `127.0.0.1:53`에 묻는데, 아무도 듣지 않으면 "연결 거부"를 무시하고 시간이 다 될 때까지(기본 5초) 기다린다.
//! 그래서 이름을 쓰는 명령(curl, wget, pip)이 매번 5초 뒤에야 실패했다. 여기서 곧바로 답해 같은 실패가 바로 난다.
//!
//! - 결과는 바뀌지 않는다: 이름은 어차피 풀리지 않았다. 허용 목록(`--network allow`)의 연결은 게스트 안 프록시를
//!   거쳐 호스트가 이름을 풀므로 이 답을 쓰지 않는다. `/etc/hosts`의 이름(localhost)은 리졸버가 먼저 본다.
//! - 질의(opcode 0)에는 NXDOMAIN, 다른 opcode에는 NOTIMP로 답한다. 응답에는 질문 부분만 되돌려 담는다.
//! - 루프백에서만 듣는다. 이미지의 `/etc/resolv.conf`는 건드리지 않는다.

use std::net::UdpSocket;

/// DNS 헤더 길이.
const HEADER: usize = 12;
const NXDOMAIN: u16 = 3;
const NOTIMP: u16 = 4;

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
        loop {
            let len = *q.get(end)? as usize;
            end += 1;
            if len == 0 {
                break;
            }
            // 질의의 이름은 압축하지 않는다. 다른 것은 받지 않는다.
            if len & 0xc0 != 0 {
                return None;
            }
            end += len;
        }
        end += 4;
        if end > q.len() {
            return None;
        }
    }
    let rcode = if opcode == 0 { NXDOMAIN } else { NOTIMP };
    // QR, 같은 opcode, RD는 그대로, RA.
    let out = 0x8000 | (opcode << 11) | (flags & 0x0100) | 0x0080 | rcode;
    let mut r = q[..end].to_vec();
    r[2..4].copy_from_slice(&out.to_be_bytes());
    r[6..HEADER].fill(0);
    Some(r)
}

/// `sock`으로 오는 질의에 답한다. 돌아오지 않는다(받기가 실패하면 돌아온다).
pub fn serve(sock: &UdpSocket) -> std::io::Result<()> {
    let mut buf = [0u8; 4096];
    loop {
        let (n, from) = sock.recv_from(&mut buf)?;
        if let Some(r) = nxdomain(&buf[..n]) {
            let _ = sock.send_to(&r, from);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `example.com` A 질의(EDNS OPT 레코드가 붙은 것).
    fn query() -> Vec<u8> {
        let mut q = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1];
        q.extend_from_slice(b"\x07example\x03com\x00");
        q.extend_from_slice(&[0, 1, 0, 1]);
        // OPT: 이름 0, TYPE 41, CLASS 4096, TTL 0, RDLEN 0.
        q.extend_from_slice(&[0, 0, 41, 0x10, 0, 0, 0, 0, 0, 0, 0]);
        q
    }

    #[test]
    fn queries_get_nxdomain_with_the_question_echoed() {
        let q = query();
        let r = nxdomain(&q).unwrap();
        assert_eq!(&r[..2], &[0xab, 0xcd]);
        // QR, opcode 0, RD, RA, NXDOMAIN.
        assert_eq!(&r[2..4], &[0x81, 0x83]);
        // 질문 1, 나머지 0. 질문 뒤(OPT)는 담지 않는다.
        assert_eq!(&r[4..12], &[0, 1, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&r[12..], &q[12..12 + 13 + 4]);
    }

    #[test]
    fn other_messages_are_ignored_or_not_implemented() {
        let mut q = query();
        q[2] |= 0x80;
        assert!(nxdomain(&q).is_none(), "a response");
        assert!(nxdomain(&q[..11]).is_none(), "short");
        let mut q = query();
        q.truncate(20);
        assert!(nxdomain(&q).is_none(), "cut in the name");
        let mut q = query();
        q[12] = 0xc0;
        assert!(nxdomain(&q).is_none(), "compressed name");
        // opcode 2 (STATUS) → NOTIMP.
        let mut q = query();
        q[2] = 0x10;
        assert_eq!(nxdomain(&q).unwrap()[3] & 0xf, NOTIMP as u8);
    }

    #[test]
    fn serves_over_udp() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || serve(&server));
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        client.send_to(&query(), addr).unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = client.recv_from(&mut buf).unwrap();
        assert_eq!(buf[3] & 0xf, NXDOMAIN as u8);
        assert_eq!(n, HEADER + 17);
    }
}
