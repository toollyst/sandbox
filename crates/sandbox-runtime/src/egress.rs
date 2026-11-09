//! 나가는 연결 (호스트 쪽). [`NetworkPolicy::Allow`](sandbox_model::NetworkPolicy::Allow)일 때 VM을 소유한 프로세스가 돌린다.
//!
//! - 게스트에 연결을 [`POOL`]개 맡겨 둔다(`HostMsg::EgressSlot`). 하나가 쓰이면 곧바로 하나를 더 맡긴다.
//!   게스트에 연결할 수 없으면(VM이 끝남) 멈춘다.
//! - 게스트가 `Connect { host, port }`를 보내면 [`check`]로 허용 목록을 확인하고, 허용되면 [`DIAL_TIMEOUT`] 안에
//!   TCP로 연결해 `Connected`를 보낸다. 아니면 `Refused`와 이유를 보낸다.
//! - `Connected` 뒤로 목적지에서 받은 바이트는 `HostMsg::Data`로 보내고, 목적지가 보내기를 끝내면 `HostMsg::End`.
//!   게스트가 보낸 `GuestMsg::Data`는 목적지에 쓰고, `GuestMsg::End`면 목적지 쪽 보내기를 닫는다(반쯤 닫기).
//!   양쪽이 끝나면(또는 어느 쪽이든 오류면) 둘 다 닫는다.
//!
//! **허용 규칙** ([`check`]):
//! 1. 요청한 호스트와 포트에 맞는 규칙([`AllowRule::matches`])이 하나도 없으면 거부.
//! 2. 호스트가 IP 주소면 그 주소로 연결한다.
//! 3. 이름이면 호스트가 이름을 풀고, 루프백·사설·링크 로컬·지정되지 않은 주소·멀티캐스트를 뺀다
//!    (허용한 이름이 내부 주소를 가리키게 하는 공격을 막는다). 예외: 맞은 규칙의 이름이 `localhost`면 루프백을 허용한다.
//!    남는 주소가 없으면 거부.

use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sandbox_executor::GuestConn;
use sandbox_model::AllowRule;
use sandbox_proto::{GuestMsg, HostMsg};

use crate::client::Guest;

/// 게스트에 맡겨 두는 연결 수. 동시에 새로 시작하는 연결이 이보다 많으면 잠깐 기다린다.
pub const POOL: usize = 4;

/// 목적지 연결을 기다리는 시간.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// 맡기는 일을 시작한다. 돌아온 뒤 스레드가 돈다.
pub fn start(guest: Guest, rules: Vec<AllowRule>) {
    let rules = Arc::new(rules);
    for _ in 0..POOL {
        let (guest, rules) = (guest.clone(), rules.clone());
        thread::spawn(move || keep_slot(&guest, &rules));
    }
}

/// 연결 하나를 맡기고, 쓰이면 처리를 넘기고 다시 맡긴다.
fn keep_slot(guest: &Guest, rules: &Arc<Vec<AllowRule>>) {
    while let Ok(mut conn) = guest.egress_slot() {
        match GuestMsg::read_from(&mut *conn) {
            Ok(GuestMsg::Connect { host, port }) => {
                let rules = rules.clone();
                thread::spawn(move || serve(conn, &rules, &host, port));
            }
            // 게스트가 다른 것을 보냈다(예: 지원하지 않음). 더 맡기지 않는다.
            Ok(_) => return,
            // 게스트가 연결을 닫았다(VM이 끝나는 중일 수 있다). 잠깐 쉬고 다시 맡겨 보고, 안 되면 끝난다.
            Err(_) => thread::sleep(crate::client::RETRY),
        }
    }
}

fn serve(mut conn: Box<dyn GuestConn>, rules: &[AllowRule], host: &str, port: u16) {
    let dialed = check(rules, host, port).and_then(|addrs| dial(&addrs));
    let tcp = match dialed {
        Ok(t) => t,
        Err(reason) => {
            let _ = HostMsg::Refused { reason }.write_to(&mut *conn);
            return;
        }
    };
    if HostMsg::Connected.write_to(&mut *conn).is_err() {
        return;
    }
    splice(conn, tcp);
}

/// 허용 목록을 확인하고 연결할 주소를 돌려준다. 규칙은 모듈 문서에 있다.
pub fn check(rules: &[AllowRule], host: &str, port: u16) -> Result<Vec<SocketAddr>, String> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let matching: Vec<&AllowRule> = rules.iter().filter(|r| r.matches(host, port)).collect();
    if matching.is_empty() {
        return Err(format!(
            "{host}:{port} is not allowed by the network policy"
        ));
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let loopback_ok = matching.iter().any(|r| r.host == "localhost");
    let addrs: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("cannot resolve {host}: {e}"))?
        .filter(|a| (loopback_ok && a.ip().is_loopback()) || is_public(a.ip()))
        .collect();
    if addrs.is_empty() {
        return Err(format!(
            "{host} resolves only to loopback or private addresses, which the network policy does not allow by name"
        ));
    }
    Ok(addrs)
}

/// 이름으로 허용해도 되는 주소인지.
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                // 100.64.0.0/10 (통신사 NAT)
                || (o[0] == 100 && (o[1] & 0xC0) == 64))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 (고유 로컬), fe80::/10 (링크 로컬)
                || (first & 0xFE00) == 0xFC00
                || (first & 0xFFC0) == 0xFE80)
        }
    }
}

fn dial(addrs: &[SocketAddr]) -> Result<TcpStream, String> {
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(a, DIAL_TIMEOUT) {
            Ok(t) => return Ok(t),
            Err(e) => last = Some(format!("{a}: {e}")),
        }
    }
    Err(format!(
        "cannot connect: {}",
        last.unwrap_or_else(|| "no address".into())
    ))
}

fn splice(conn: Box<dyn GuestConn>, tcp: TcpStream) {
    let (Ok(mut conn_out), Ok(mut tcp_in)) = (conn.try_clone_conn(), tcp.try_clone()) else {
        return;
    };
    let mut conn_in = conn;
    let mut tcp_out = tcp;
    // 목적지 → 게스트.
    let down = thread::spawn(move || {
        let mut buf = vec![0u8; sandbox_proto::CHUNK_LEN];
        loop {
            match io::Read::read(&mut tcp_in, &mut buf) {
                Ok(0) => break HostMsg::End.write_to(&mut *conn_out).is_ok(),
                Ok(n) => {
                    if HostMsg::Data(buf[..n].to_vec())
                        .write_to(&mut *conn_out)
                        .is_err()
                    {
                        break false;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break false,
            }
        }
    });
    // 게스트 → 목적지.
    let up_ok = loop {
        match GuestMsg::read_from(&mut *conn_in) {
            Ok(GuestMsg::Data(b)) => {
                if io::Write::write_all(&mut tcp_out, &b).is_err() {
                    break false;
                }
            }
            Ok(GuestMsg::End) => {
                let _ = tcp_out.shutdown(Shutdown::Write);
                break true;
            }
            _ => break false,
        }
    };
    if !up_ok {
        // 게스트 쪽이 끊겼다. 목적지를 닫아 내려가는 쪽도 끝나게 한다.
        let _ = tcp_out.shutdown(Shutdown::Both);
    }
    let _ = down.join();
    let _ = tcp_out.shutdown(Shutdown::Both);
    let _ = conn_in.shutdown();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(list: &[&str]) -> Vec<AllowRule> {
        list.iter().map(|r| r.parse().unwrap()).collect()
    }

    #[test]
    fn policy() {
        let r = rules(&["127.0.0.1:8080", "localhost:*", "*.example.com:443"]);
        assert_eq!(
            check(&r, "127.0.0.1", 8080).unwrap(),
            vec!["127.0.0.1:8080".parse().unwrap()]
        );
        assert!(check(&r, "127.0.0.1", 8081).is_err());
        assert!(check(&r, "10.0.0.1", 8080).is_err());
        // localhost를 이름으로 적었으면 루프백을 허용한다.
        assert!(
            check(&r, "localhost", 9)
                .unwrap()
                .iter()
                .all(|a| a.ip().is_loopback())
        );
        // 규칙에 없는 이름은 풀어 보지도 않는다.
        assert!(
            check(&r, "example.com", 443)
                .unwrap_err()
                .contains("not allowed")
        );
    }

    #[test]
    fn names_resolving_to_internal_addresses_are_refused() {
        // localhost 대신 다른 이름으로 루프백을 가리키는 경우. 시스템 해석기가 "localhost."을 루프백으로 푼다.
        let r = rules(&["*.localhost:*"]);
        if let Err(e) = check(&r, "a.localhost", 80) {
            assert!(e.contains("private") || e.contains("resolve"), "{e}");
        } else {
            panic!("a.localhost should not be allowed by name");
        }
        for ip in [
            "10.1.2.3",
            "192.168.0.1",
            "172.16.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
    }
}
