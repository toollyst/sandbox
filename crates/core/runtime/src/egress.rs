//! 나가는 연결 (호스트 쪽). [`NetworkPolicy::Allow`](sbx_model::NetworkPolicy::Allow)일 때 VM을 소유한 프로세스가 돌린다.
//!
//! - 게스트에 연결을 [`POOL`]개 맡겨 둔다(`HostMsg::EgressSlot`). 하나가 쓰이면 곧바로 하나를 더 맡긴다.
//!   게스트에 연결할 수 없으면(VM이 끝남) 멈춘다.
//! - 게스트가 `Connect { host, port }`를 보내면 [`check`]로 허용 목록을 확인하고, 허용되면 [`DIAL_TIMEOUT`] 안에
//!   TCP로 연결해 `Connected`를 보낸다. 아니면 `Refused`와 이유를 보낸다.
//! - `Connected` 뒤로 목적지에서 받은 바이트는 `HostMsg::Data`로 보내고, 목적지가 보내기를 끝내면 `HostMsg::End`.
//!   게스트가 보낸 `GuestMsg::Data`는 목적지에 쓰고, `GuestMsg::End`면 목적지 쪽 보내기를 닫는다(반쯤 닫기).
//!   양쪽이 끝나면(또는 어느 쪽이든 오류면) 둘 다 닫는다.
//! - 동시에 열린 연결은 샌드박스마다 [`MAX_CONNECTIONS`]개까지다. 넘으면 `Refused`다(연결마다 호스트 스레드와
//!   소켓을 쓰므로 게스트가 사용자의 스레드나 파일 기술자를 다 쓰지 못하게 한다).
//!
//! - 전송량 상한([`Limits::bytes`], `--egress-limit`): 이번 시작에서 모든 연결이 주고받은 바이트 합계. 조각이
//!   상한을 넘기면 상한까지만 보내고 그 연결을 끊는다. 상한에 닿은 뒤의 새 연결은 `Refused`다.
//! - 기록([`Record`], 실행 코어의 `network_log`): 허용(`connect`: 호스트, 포트, 연결한 주소), 거부(`refuse`: 이유),
//!   닫힘(`close`: 보낸·받은 바이트). 기록하지 못해도 연결은 그대로다(알림이다).
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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use sbx_executor::GuestConn;
use sbx_guest_proto::{GuestMsg, HostMsg};
use sbx_model::AllowRule;

use crate::client::Guest;

/// 게스트에 맡겨 두는 연결 수. 동시에 새로 시작하는 연결이 이보다 많으면 잠깐 기다린다.
pub const POOL: usize = 4;

/// 샌드박스 하나가 동시에 열 수 있는 연결 수.
pub const MAX_CONNECTIONS: usize = 256;

/// 목적지 연결을 기다리는 시간.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// 나가는 연결 기록의 이벤트 종류. 연결마다 남으므로 샌드박스 수명 기록과 따로 둔다(`network_log`).
pub const NETWORK_EVENT_KIND: &str = "network";

/// 상한.
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    /// 이번 시작에서 주고받을 수 있는 바이트 합계. 없으면 끝없이.
    pub bytes: Option<u64>,
}

/// 연결 기록을 남길 곳.
#[derive(Debug, Clone)]
pub struct Record {
    /// 기록.
    pub log: sbx_events::Log,
    /// 샌드박스 ID(이벤트의 `id`).
    pub sandbox: String,
}

impl Record {
    fn append(&self, action: &str, host: &str, port: u16, extra: &[(&str, String)]) {
        let mut e = sbx_events::Event::new(NETWORK_EVENT_KIND, action, self.sandbox.clone())
            .with("host", host.to_owned())
            .with("port", port.to_string());
        for (k, v) in extra {
            e = e.with(k, v.clone());
        }
        let _ = self.log.append(e);
    }
}

/// 전송량. 모든 연결이 함께 쓴다.
#[derive(Debug, Default)]
pub(crate) struct Meter {
    used: AtomicU64,
    limit: Option<u64>,
}

impl Meter {
    /// `n`바이트 중 보낼 수 있는 만큼(상한까지)을 세고 그 수를 돌려준다.
    fn take(&self, n: usize) -> usize {
        let Some(limit) = self.limit else {
            self.used.fetch_add(n as u64, Ordering::Relaxed);
            return n;
        };
        // fetch_update는 새 Rust에서 이름이 바뀌어(try_update) MSRV와 함께 쓸 수 있는 compare_exchange로 한다.
        let mut u = self.used.load(Ordering::SeqCst);
        loop {
            let granted = limit.saturating_sub(u).min(n as u64);
            match self
                .used
                .compare_exchange(u, u + granted, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return granted as usize,
                Err(now) => u = now,
            }
        }
    }

    fn exhausted(&self) -> Option<u64> {
        self.limit
            .filter(|l| self.used.load(Ordering::SeqCst) >= *l)
    }
}

struct Shared {
    rules: Vec<AllowRule>,
    open: AtomicUsize,
    meter: Meter,
    record: Record,
}

/// 맡기는 일을 시작한다. 돌아온 뒤 스레드가 돈다.
pub fn start(guest: Guest, rules: Vec<AllowRule>, limits: Limits, record: Record) {
    let shared = Arc::new(Shared {
        rules,
        open: AtomicUsize::new(0),
        meter: Meter {
            used: AtomicU64::new(0),
            limit: limits.bytes,
        },
        record,
    });
    for _ in 0..POOL {
        let (guest, shared) = (guest.clone(), shared.clone());
        thread::spawn(move || keep_slot(&guest, &shared));
    }
}

/// 열린 연결 수를 하나 차지한다. 버리면 돌려준다.
struct Opened(Arc<Shared>);

impl Opened {
    fn take(open: &Arc<Shared>, max: usize) -> Option<Self> {
        let mut n = open.open.load(Ordering::SeqCst);
        loop {
            if n >= max {
                return None;
            }
            match open
                .open
                .compare_exchange(n, n + 1, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return Some(Self(open.clone())),
                Err(now) => n = now,
            }
        }
    }
}

impl Drop for Opened {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// 연결 하나를 맡기고, 쓰이면 처리를 넘기고 다시 맡긴다.
fn keep_slot(guest: &Guest, shared: &Arc<Shared>) {
    while let Ok(mut conn) = guest.egress_slot() {
        match GuestMsg::read_from(&mut *conn) {
            Ok(GuestMsg::Connect { host, port }) => {
                let refuse = |conn: &mut Box<dyn GuestConn>, reason: String| {
                    shared
                        .record
                        .append("refuse", &host, port, &[("reason", reason.clone())]);
                    let _ = HostMsg::Refused { reason }.write_to(&mut **conn);
                };
                if let Some(limit) = shared.meter.exhausted() {
                    refuse(
                        &mut conn,
                        format!("the sandbox used its egress limit of {limit} bytes"),
                    );
                    continue;
                }
                let Some(opened) = Opened::take(shared, MAX_CONNECTIONS) else {
                    refuse(
                        &mut conn,
                        format!("more than {MAX_CONNECTIONS} connections are open"),
                    );
                    continue;
                };
                thread::spawn(move || serve(conn, opened, &host, port));
            }
            // 게스트가 다른 것을 보냈다(예: 지원하지 않음). 더 맡기지 않는다.
            Ok(_) => return,
            // 게스트가 연결을 닫았다(VM이 끝나는 중일 수 있다). 잠깐 쉬고 다시 맡겨 보고, 안 되면 끝난다.
            Err(_) => thread::sleep(crate::client::RETRY),
        }
    }
}

fn serve(mut conn: Box<dyn GuestConn>, opened: Opened, host: &str, port: u16) {
    let shared = &opened.0;
    let dialed = check(&shared.rules, host, port).and_then(|addrs| dial(&addrs));
    let tcp = match dialed {
        Ok(t) => t,
        Err(reason) => {
            shared
                .record
                .append("refuse", host, port, &[("reason", reason.clone())]);
            let _ = HostMsg::Refused { reason }.write_to(&mut *conn);
            return;
        }
    };
    let addr = tcp.peer_addr().map(|a| a.to_string()).unwrap_or_default();
    shared
        .record
        .append("connect", host, port, &[("addr", addr)]);
    if HostMsg::Connected.write_to(&mut *conn).is_err() {
        return;
    }
    let (sent, received) = splice(conn, tcp, Some(&shared.meter));
    let mut extra = vec![
        ("sent", sent.to_string()),
        ("received", received.to_string()),
    ];
    if let Some(limit) = shared.meter.exhausted() {
        extra.push(("limit", limit.to_string()));
    }
    shared.record.append("close", host, port, &extra);
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
        .filter(|a| (loopback_ok && a.ip().is_loopback()) || sbx_model::is_public_ip(a.ip()))
        .collect();
    if addrs.is_empty() {
        return Err(format!(
            "{host} resolves only to loopback or private addresses, which the network policy does not allow by name"
        ));
    }
    Ok(addrs)
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

/// 양쪽을 잇는다. `meter`가 있으면 그 상한 안에서만 보낸다. 보낸(게스트 → 목적지)·받은 바이트를 돌려준다.
pub(crate) fn splice(
    conn: Box<dyn GuestConn>,
    tcp: TcpStream,
    meter: Option<&Meter>,
) -> (u64, u64) {
    let (Ok(mut conn_out), Ok(mut tcp_in)) = (conn.try_clone_conn(), tcp.try_clone()) else {
        return (0, 0);
    };
    // 보낼 수 있는 길이. 줄었으면 그만큼 보내고 끊는다.
    let take = |n: usize| meter.map_or(n, |m| m.take(n));
    let mut conn_in = conn;
    let mut tcp_out = tcp;
    let mut sent = 0u64;
    let received = thread::scope(|s| {
        // 목적지 → 게스트.
        let down = s.spawn(|| {
            let mut buf = vec![0u8; sbx_guest_proto::CHUNK_LEN];
            let mut received = 0u64;
            loop {
                match io::Read::read(&mut tcp_in, &mut buf) {
                    Ok(0) => {
                        let _ = HostMsg::End.write_to(&mut *conn_out);
                        break;
                    }
                    Ok(n) => {
                        let ok = take(n);
                        let sent_ok = ok == 0
                            || HostMsg::Data(buf[..ok].to_vec())
                                .write_to(&mut *conn_out)
                                .is_ok();
                        received += if sent_ok { ok as u64 } else { 0 };
                        if ok < n || !sent_ok {
                            // 상한이거나 게스트가 끊겼다: 목적지에서 더 읽지 않게 닫는다.
                            let _ = tcp_in.shutdown(Shutdown::Both);
                            let _ = conn_out.shutdown();
                            break;
                        }
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            received
        });
        // 게스트 → 목적지.
        let up_ok = loop {
            match GuestMsg::read_from(&mut *conn_in) {
                Ok(GuestMsg::Data(b)) => {
                    let ok = take(b.len());
                    if io::Write::write_all(&mut tcp_out, &b[..ok]).is_err() {
                        break false;
                    }
                    sent += ok as u64;
                    if ok < b.len() {
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
            // 게스트 쪽이 끊겼거나 상한이다. 목적지를 닫아 내려가는 쪽도 끝나게 한다.
            let _ = tcp_out.shutdown(Shutdown::Both);
            let _ = conn_in.shutdown();
        }
        down.join().unwrap_or(0)
    });
    let _ = tcp_out.shutdown(Shutdown::Both);
    let _ = conn_in.shutdown();
    (sent, received)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(list: &[&str]) -> Vec<AllowRule> {
        list.iter().map(|r| r.parse().unwrap()).collect()
    }

    fn shared(limit: Option<u64>) -> Arc<Shared> {
        Arc::new(Shared {
            rules: Vec::new(),
            open: AtomicUsize::new(0),
            meter: Meter {
                used: AtomicU64::new(0),
                limit,
            },
            record: Record {
                log: sbx_events::Log::new(std::env::temp_dir().join("sbx-egress-unused")),
                sandbox: "x".into(),
            },
        })
    }

    #[test]
    fn open_connections_are_counted_and_capped() {
        let open = shared(None);
        let held: Vec<Opened> = (0..3).filter_map(|_| Opened::take(&open, 3)).collect();
        assert_eq!(held.len(), 3);
        assert!(Opened::take(&open, 3).is_none());
        drop(held);
        assert_eq!(open.open.load(Ordering::SeqCst), 0);
        assert!(Opened::take(&open, 3).is_some());
    }

    #[test]
    fn the_meter_grants_up_to_the_limit() {
        let s = shared(Some(10));
        assert_eq!(s.meter.take(6), 6);
        assert_eq!(s.meter.exhausted(), None);
        assert_eq!(s.meter.take(5), 4);
        assert_eq!(s.meter.exhausted(), Some(10));
        assert_eq!(s.meter.take(1), 0);
        let free = shared(None);
        assert_eq!(free.meter.take(usize::MAX), usize::MAX);
        assert_eq!(free.meter.exhausted(), None);
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
        // 주소 범위 규칙은 sbx_model::is_public_ip의 테스트에 있다.
    }
}
