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
//!
//! **투명 TCP** (프록시를 모르는 프로그램의 길, 게스트 쪽 규칙은 `sbx-guest`의 `transparent` 모듈)
//! - [`start`]는 맡길 연결을 돌린 뒤 게스트에 `HostMsg::Transparent`를 보낸다. IP 규칙 가운데 게스트가 들을 수 있는
//!   것([`AllowRule::reach`]가 [`Reach::Literal`])을 함께 보낸다. 프록시로만 닿는 규칙은 하나마다 `proxy_only`(호스트,
//!   포트, `reason`)로 기록한다(CLI도 만들 때 경고한다). 게스트가 하지 못한 것이 있으면 `notice`(`reason`)로 기록하고
//!   시작은 계속한다(프록시는 그대로다).
//! - 맡긴 연결로 `Lookup { name }`이 오면 [`lookup`]으로 허용 목록만 보고 `Resolved`로 답한다. **이름을 풀지 않는다.**
//!   그래서 허용하지 않은 이름은 DNS로도 새지 않는다. 답한 뒤 그 연결은 다시 맡긴 연결이다(같은 스레드가 다음
//!   메시지를 읽는다).
//! - 기록: 허용된 이름은 `lookup`(`ports`, 포트가 `*`인 규칙이 맞았으면 `any_port`), 거부된 이름은 `refuse`(`reason`,
//!   `via=dns`, 포트 없음: DNS에는 포트가 없다). 이름마다 한 번, [`MAX_RECORDED_NAMES`]개까지. 넘으면 `notice`를 하나
//!   남기고 더 기록하지 않는다(게스트가 기록을 끝없이 채우지 못하게). 이름으로 쓸 수 없는 것은 기록하지 않는다.
//! - 이름 묻기는 전송량(`--egress-limit`)에 세지 않고 열린 연결 수도 차지하지 않는다. 호스트가 하는 일은 규칙
//!   맞추기뿐이고 입출력은 기록 한 줄이다.
//! - 가짜 주소로 온 연결은 게스트가 이름으로 되돌린 `Connect { host, port }`로 온다. 그 뒤는 위와 한 줄도 다르지 않다.
//!
//! **샌드박스 잇기** (`--connect`, 규칙은 [`crate::connect`])
//! - [`start`]에 잇기 목록이 있으면 네트워크 정책이 `Disabled`여도 맡길 연결과 투명 TCP를 켠다. 그때 허용 규칙은
//!   비었으므로 잇기 이름 말고는 어느 이름도 허용되지 않는다(프록시는 띄우지 않는다. 띄우는 것은 실행 코어다).
//! - 이름 묻기([`lookup`])는 허용 목록과 잇기 목록을 함께 본다(둘의 이름은 겹치지 않는다, `sbx_model::check_connect`).
//!   잇기 이름의 `lookup` 기록에는 `target`(대상)이 붙는다.
//! - `Connect { host, port }`가 잇기 규칙에 맞으면 목적지에 TCP로 연결하는 대신 대상 샌드박스의 게스트에 잇는다
//!   (`connect::Peers::open`). 전송량과 열린 연결 수는 같은 것을 쓴다: 바이트는 이 샌드박스의
//!   `--egress-limit`에 양쪽 방향 모두 센다(내용 바이트만, 틀의 머리는 세지 않는다). 기록(`connect`, `refuse`,
//!   `close`)의 `host`는 안에서 쓴 이름이고 `target`(기록된 대상)과, 찾았으면 `target_id`(대상 ID)가 붙는다.
//!   `connect`에는 `addr`가 없다.

use std::collections::HashSet;
use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use sbx_executor::GuestConn;
use sbx_guest_proto::{GuestMsg, HostMsg};
use sbx_model::{AllowRule, ConnectRule, Reach};

use crate::client::Guest;
use crate::connect::Peers;

/// 게스트에 맡겨 두는 연결 수. 동시에 새로 시작하는 연결이 이보다 많으면 잠깐 기다린다.
pub const POOL: usize = 4;

/// 샌드박스 하나가 동시에 열 수 있는 연결 수.
pub const MAX_CONNECTIONS: usize = 256;

/// 목적지 연결을 기다리는 시간.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// 나가는 연결 기록의 이벤트 종류. 연결마다 남으므로 샌드박스 수명 기록과 따로 둔다(`network_log`).
pub const NETWORK_EVENT_KIND: &str = "network";

/// 이름 묻기를 기록하는 이름의 수(시작마다).
pub const MAX_RECORDED_NAMES: usize = 1024;

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
        let mut attrs = vec![("host", host.to_owned()), ("port", port.to_string())];
        attrs.extend(extra.iter().cloned());
        self.event(action, &attrs);
    }

    fn event(&self, action: &str, attrs: &[(&str, String)]) {
        let mut e = sbx_events::Event::new(NETWORK_EVENT_KIND, action, self.sandbox.clone());
        for (k, v) in attrs {
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
    /// `--connect`. 없으면 잇지 않는다.
    peers: Option<Peers>,
    open: AtomicUsize,
    meter: Meter,
    record: Record,
    /// 이름 묻기를 기록한 이름들. 상한에 닿으면 `None`(더 기록하지 않는다).
    named: Mutex<Option<HashSet<String>>>,
}

impl Shared {
    /// 이름 묻기 하나를 기록한다. 규칙은 모듈 문서에 있다.
    fn note_lookup(&self, name: &str, answer: &Lookup) {
        let Some(name) = plain_name(name) else {
            return;
        };
        {
            let mut named = self.named.lock().unwrap_or_else(|e| e.into_inner());
            let Some(seen) = named.as_mut() else {
                return;
            };
            if seen.contains(&name) {
                return;
            }
            if seen.len() >= MAX_RECORDED_NAMES {
                *named = None;
                drop(named);
                self.record.event(
                    "notice",
                    &[(
                        "reason",
                        format!(
                            "more than {MAX_RECORDED_NAMES} names were looked up; later lookups are not recorded"
                        ),
                    )],
                );
                return;
            }
            seen.insert(name.clone());
        }
        if answer.allowed() {
            let ports: Vec<String> = answer.ports.iter().map(u16::to_string).collect();
            let target = self
                .peers
                .as_ref()
                .and_then(|p| p.target_of(&name))
                .map(str::to_owned);
            let mut attrs = vec![("host", name), ("ports", ports.join(","))];
            if answer.any_port {
                attrs.push(("any_port", "true".into()));
            }
            if let Some(t) = target {
                attrs.push(("target", t));
            }
            self.record.event("lookup", &attrs);
        } else {
            self.record.event(
                "refuse",
                &[
                    (
                        "reason",
                        format!("{name} is not allowed by the network policy"),
                    ),
                    ("host", name),
                    ("via", "dns".into()),
                ],
            );
        }
    }
}

/// 맡기는 일을 시작하고 투명 TCP를 켠다. 돌아온 뒤 스레드가 돈다. `peers`는 `--connect`다.
pub(crate) fn start(
    guest: Guest,
    rules: Vec<AllowRule>,
    peers: Option<Peers>,
    limits: Limits,
    record: Record,
) {
    let shared = Arc::new(Shared {
        rules,
        peers,
        open: AtomicUsize::new(0),
        meter: Meter {
            used: AtomicU64::new(0),
            limit: limits.bytes,
        },
        record,
        named: Mutex::new(Some(HashSet::new())),
    });
    for _ in 0..POOL {
        let (guest, shared) = (guest.clone(), shared.clone());
        thread::spawn(move || keep_slot(&guest, &shared));
    }
    transparent(&guest, &shared.rules, &shared.record);
}

/// 게스트의 투명 TCP를 켠다. 규칙은 모듈 문서에 있다.
fn transparent(guest: &Guest, rules: &[AllowRule], record: &Record) {
    let mut literals = Vec::new();
    for r in rules {
        match r.reach() {
            Reach::Name => {}
            Reach::Literal(a) => literals.push(a),
            Reach::ProxyOnly(why) => record.event(
                "proxy_only",
                &[
                    ("host", r.host.clone()),
                    ("port", r.port.map_or_else(|| "*".into(), |p| p.to_string())),
                    ("reason", why.why().into()),
                ],
            ),
        }
    }
    literals.sort();
    literals.dedup();
    if let Err(e) = guest.transparent(literals) {
        let restart = match &e {
            crate::error::RuntimeError::Guest(g)
                if g.kind == sbx_guest_proto::ErrorKind::Protocol =>
            {
                " (the guest is older than this sbx; restart the sandbox)"
            }
            _ => "",
        };
        record.event(
            "notice",
            &[(
                "reason",
                format!(
                    "programs that ignore HTTP_PROXY may not reach allowed hosts: {e}{restart}"
                ),
            )],
        );
    }
}

/// [`lookup`]의 답.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Lookup {
    /// 이름에 맞는 규칙의 포트들(작은 것부터, 겹치지 않게).
    pub ports: Vec<u16>,
    /// 포트가 `*`인 규칙이 맞았다(그 이름은 프록시로는 어느 포트로나 닿는다).
    pub any_port: bool,
}

impl Lookup {
    /// 허용된 이름인지.
    pub fn allowed(&self) -> bool {
        !self.ports.is_empty() || self.any_port
    }
}

/// 소문자로 바꾸고 끝의 `.`을 뗀 이름. 이름으로 쓸 수 없으면(IP 주소 꼴, 빈 조각, 63자 넘는 조각, 253자 넘음,
/// 글자·숫자·`-`·`_` 밖의 바이트) `None`.
fn plain_name(name: &str) -> Option<String> {
    let n = name.strip_suffix('.').unwrap_or(name);
    let ok = !n.is_empty()
        && n.len() <= 253
        && n.parse::<IpAddr>().is_err()
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    ok.then(|| n.to_ascii_lowercase())
}

/// 게스트가 물은 이름이 허용 목록이나 잇기 목록(`connect`)에 있는지(투명 TCP의 DNS). **이름을 풀지 않는다.**
/// IP 규칙은 이름에 맞지 않는다. 이름으로 쓸 수 없는 것은 허용되지 않은 것이다.
pub fn lookup(rules: &[AllowRule], connect: &[ConnectRule], name: &str) -> Lookup {
    let Some(name) = plain_name(name) else {
        return Lookup::default();
    };
    let mut out = Lookup::default();
    for r in rules.iter().filter(|r| !r.is_ip() && r.matches_host(&name)) {
        match r.port {
            Some(p) => out.ports.push(p),
            None => out.any_port = true,
        }
    }
    out.ports.extend(
        connect
            .iter()
            .filter(|r| r.matches_name(&name))
            .map(|r| r.port),
    );
    out.ports.sort_unstable();
    out.ports.dedup();
    out
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

/// 연결 하나를 맡기고, 쓰이면 처리를 넘기고 다시 맡긴다. 이름 묻기는 답한 뒤 같은 연결을 계속 읽는다.
fn keep_slot(guest: &Guest, shared: &Arc<Shared>) {
    while let Ok(mut conn) = guest.egress_slot() {
        loop {
            match GuestMsg::read_from(&mut *conn) {
                Ok(GuestMsg::Lookup { name }) => {
                    let connect = shared.peers.as_ref().map_or(&[][..], |p| &p.rules[..]);
                    let answer = lookup(&shared.rules, connect, &name);
                    shared.note_lookup(&name, &answer);
                    let reply = HostMsg::Resolved {
                        ports: answer.ports,
                        any_port: answer.any_port,
                    };
                    if reply.write_to(&mut *conn).is_err() {
                        break;
                    }
                }
                Ok(GuestMsg::Connect { host, port }) => {
                    let peer = shared
                        .peers
                        .as_ref()
                        .and_then(|p| p.rule(&host, port))
                        .cloned();
                    let refuse = |conn: &mut Box<dyn GuestConn>, reason: String| {
                        let mut extra = vec![("reason", reason.clone())];
                        if let Some(r) = &peer {
                            extra.push(("target", r.target.clone()));
                        }
                        shared.record.append("refuse", &host, port, &extra);
                        let _ = HostMsg::Refused { reason }.write_to(&mut **conn);
                    };
                    if let Some(limit) = shared.meter.exhausted() {
                        refuse(
                            &mut conn,
                            format!("the sandbox used its egress limit of {limit} bytes"),
                        );
                        break;
                    }
                    let Some(opened) = Opened::take(shared, MAX_CONNECTIONS) else {
                        refuse(
                            &mut conn,
                            format!("more than {MAX_CONNECTIONS} connections are open"),
                        );
                        break;
                    };
                    thread::spawn(move || match peer {
                        Some(rule) => serve_peer(conn, opened, &rule),
                        None => serve(conn, opened, &host, port),
                    });
                    break;
                }
                // 게스트가 다른 것을 보냈다(예: 지원하지 않음). 더 맡기지 않는다.
                Ok(_) => return,
                // 게스트가 연결을 닫았다(VM이 끝나는 중일 수 있다). 잠깐 쉬고 다시 맡겨 보고, 안 되면 끝난다.
                Err(_) => {
                    thread::sleep(crate::client::RETRY);
                    break;
                }
            }
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
    let (sent, received) = splice(conn, Far::Tcp(tcp), Some(&shared.meter));
    let mut extra = vec![
        ("sent", sent.to_string()),
        ("received", received.to_string()),
    ];
    if let Some(limit) = shared.meter.exhausted() {
        extra.push(("limit", limit.to_string()));
    }
    shared.record.append("close", host, port, &extra);
}

/// 잇기 규칙에 맞은 연결: 대상 샌드박스의 게스트에 잇는다. 규칙은 모듈 문서와 [`crate::connect`]에 있다.
fn serve_peer(mut conn: Box<dyn GuestConn>, opened: Opened, rule: &ConnectRule) {
    let shared = &opened.0;
    let Some(peers) = &shared.peers else {
        return;
    };
    let (host, port) = (rule.name.as_str(), rule.port);
    let mut extra = vec![("target", rule.target.clone())];
    let peer = match peers.open(rule) {
        Ok(p) => p,
        Err((reason, id)) => {
            if let Some(id) = id {
                extra.push(("target_id", id.to_string()));
            }
            extra.push(("reason", reason.clone()));
            shared.record.append("refuse", host, port, &extra);
            let _ = HostMsg::Refused { reason }.write_to(&mut *conn);
            return;
        }
    };
    extra.push(("target_id", peer.id.to_string()));
    shared.record.append("connect", host, port, &extra);
    if HostMsg::Connected.write_to(&mut *conn).is_err() {
        let _ = peer.conn.shutdown();
        return;
    }
    let (sent, received) = splice(conn, Far::Guest(peer.conn), Some(&shared.meter));
    extra.push(("sent", sent.to_string()));
    extra.push(("received", received.to_string()));
    if let Some(limit) = shared.meter.exhausted() {
        extra.push(("limit", limit.to_string()));
    }
    shared.record.append("close", host, port, &extra);
}

/// 테스트 프로세스에서 호스트가 풀어 본 이름들. 이름 묻기가 이름을 풀지 않는 것을 테스트가 확인한다.
#[cfg(test)]
static RESOLVED: Mutex<Vec<String>> = Mutex::new(Vec::new());

#[cfg(test)]
fn resolved(host: &str) {
    RESOLVED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(host.to_owned());
}

/// 호스트가 `name`을 풀어 본 적이 있는지(테스트).
#[cfg(test)]
pub(crate) fn was_resolved(name: &str) -> bool {
    RESOLVED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .any(|h| h.eq_ignore_ascii_case(name))
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
    #[cfg(test)]
    resolved(host);
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

/// [`splice`]의 반대쪽: 목적지에 연 TCP 연결이거나, 다른 샌드박스의 게스트에 `Forward`로 연 연결(`--connect`).
/// 게스트 연결은 이 샌드박스의 게스트 연결과 같은 틀이다(받기는 `GuestMsg::Data`·`End`, 보내기는
/// `HostMsg::Data`·`End`).
pub(crate) enum Far {
    /// TCP.
    Tcp(TcpStream),
    /// 다른 게스트.
    Guest(Box<dyn GuestConn>),
}

impl Far {
    fn try_clone(&self) -> io::Result<Far> {
        Ok(match self {
            Far::Tcp(t) => Far::Tcp(t.try_clone()?),
            Far::Guest(c) => Far::Guest(c.try_clone_conn()?),
        })
    }

    /// 조각 하나. 상대가 보내기를 끝냈으면 `None`.
    fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<Option<Vec<u8>>> {
        match self {
            Far::Tcp(t) => loop {
                match io::Read::read(t, buf) {
                    Ok(0) => return Ok(None),
                    Ok(n) => return Ok(Some(buf[..n].to_vec())),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(e),
                }
            },
            Far::Guest(c) => match GuestMsg::read_from(&mut **c)? {
                GuestMsg::Data(b) => Ok(Some(b)),
                GuestMsg::End => Ok(None),
                _ => Err(io::Error::from(io::ErrorKind::InvalidData)),
            },
        }
    }

    fn write_all(&mut self, b: &[u8]) -> io::Result<()> {
        match self {
            Far::Tcp(t) => io::Write::write_all(t, b),
            Far::Guest(c) => HostMsg::Data(b.to_vec()).write_to(&mut **c),
        }
    }

    /// 보내기를 끝낸다(반쯤 닫기).
    fn end(&mut self) {
        match self {
            Far::Tcp(t) => {
                let _ = t.shutdown(Shutdown::Write);
            }
            Far::Guest(c) => {
                let _ = HostMsg::End.write_to(&mut **c);
            }
        }
    }

    fn close(&self) {
        match self {
            Far::Tcp(t) => {
                let _ = t.shutdown(Shutdown::Both);
            }
            Far::Guest(c) => {
                let _ = c.shutdown();
            }
        }
    }
}

/// 양쪽을 잇는다. `meter`가 있으면 그 상한 안에서만 보낸다. 보낸(게스트 → 반대쪽)·받은 바이트를 돌려준다.
/// 반대쪽에서 읽다 오류가 나면 둘 다 닫는다.
pub(crate) fn splice(conn: Box<dyn GuestConn>, far: Far, meter: Option<&Meter>) -> (u64, u64) {
    let (Ok(mut conn_out), Ok(mut far_in)) = (conn.try_clone_conn(), far.try_clone()) else {
        return (0, 0);
    };
    // 보낼 수 있는 길이. 줄었으면 그만큼 보내고 끊는다.
    let take = |n: usize| meter.map_or(n, |m| m.take(n));
    let mut conn_in = conn;
    let mut far_out = far;
    let mut sent = 0u64;
    let received = thread::scope(|s| {
        // 반대쪽 → 게스트.
        let down = s.spawn(|| {
            let mut buf = vec![0u8; sbx_guest_proto::CHUNK_LEN];
            let mut received = 0u64;
            loop {
                match far_in.read_chunk(&mut buf) {
                    Ok(None) => {
                        let _ = HostMsg::End.write_to(&mut *conn_out);
                        break;
                    }
                    Ok(Some(b)) => {
                        let n = b.len();
                        let ok = take(n);
                        let sent_ok = ok == 0
                            || HostMsg::Data(b[..ok].to_vec())
                                .write_to(&mut *conn_out)
                                .is_ok();
                        received += if sent_ok { ok as u64 } else { 0 };
                        if ok < n || !sent_ok {
                            // 상한이거나 게스트가 끊겼다: 반대쪽에서 더 읽지 않게 닫는다.
                            far_in.close();
                            let _ = conn_out.shutdown();
                            break;
                        }
                    }
                    Err(_) => {
                        // 반대쪽이 끊겼다. 게스트 쪽도 닫아 올라가는 쪽이 끝나게 한다.
                        far_in.close();
                        let _ = conn_out.shutdown();
                        break;
                    }
                }
            }
            received
        });
        // 게스트 → 반대쪽.
        let up_ok = loop {
            match GuestMsg::read_from(&mut *conn_in) {
                Ok(GuestMsg::Data(b)) => {
                    let ok = take(b.len());
                    if far_out.write_all(&b[..ok]).is_err() {
                        break false;
                    }
                    sent += ok as u64;
                    if ok < b.len() {
                        break false;
                    }
                }
                Ok(GuestMsg::End) => {
                    far_out.end();
                    break true;
                }
                _ => break false,
            }
        };
        if !up_ok {
            // 게스트 쪽이 끊겼거나 상한이다. 반대쪽을 닫아 내려가는 쪽도 끝나게 한다.
            far_out.close();
            let _ = conn_in.shutdown();
        }
        down.join().unwrap_or(0)
    });
    far_out.close();
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
            peers: None,
            open: AtomicUsize::new(0),
            meter: Meter {
                used: AtomicU64::new(0),
                limit,
            },
            record: Record {
                log: sbx_events::Log::new(std::env::temp_dir().join("sbx-egress-unused")),
                sandbox: "x".into(),
            },
            named: Mutex::new(Some(HashSet::new())),
        })
    }

    #[test]
    fn lookups_answer_from_the_list_only() {
        let r = rules(&[
            "github.com:22",
            "github.com:443",
            "GitHub.com:22",
            "*.githubusercontent.com:443",
            "cdn.example:*",
            "140.82.112.3:22",
            "[2001:db8::1]:443",
        ]);
        assert_eq!(
            lookup(&r, &[], "GitHub.com."),
            Lookup {
                ports: vec![22, 443],
                any_port: false
            }
        );
        assert_eq!(
            lookup(&r, &[], "raw.githubusercontent.com").ports,
            vec![443]
        );
        assert!(!lookup(&r, &[], "githubusercontent.com").allowed());
        assert_eq!(
            lookup(&r, &[], "cdn.example"),
            Lookup {
                ports: vec![],
                any_port: true
            }
        );
        // IP 규칙은 이름에 맞지 않는다. 이름이 아닌 것은 허용되지 않는다.
        let too_long_label = format!("{}.github.com", "a".repeat(64));
        for not in [
            "140.82.112.3",
            "2001:db8::1",
            "evil.example",
            "",
            ".",
            "a..github.com",
            "git hub.com",
            too_long_label.as_str(),
        ] {
            assert!(!lookup(&r, &[], not).allowed(), "{not:?}");
        }
        let label = "a".repeat(63);
        let long = format!("{label}.{label}.{label}.{label}.githubusercontent.com");
        assert!(!lookup(&r, &[], &long).allowed());
        // 묻기만으로는 아무 이름도 풀지 않는다.
        for n in [
            "github.com",
            "cdn.example",
            "evil.example",
            "raw.githubusercontent.com",
        ] {
            assert!(!was_resolved(n), "{n} was resolved");
        }
    }

    #[test]
    fn lookups_are_recorded_once_per_name_and_capped() {
        let dir = std::env::temp_dir().join(format!("sbx-egress-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = sbx_events::Log::new(dir.clone());
        let mut s = Arc::into_inner(shared(None)).unwrap();
        s.record.log = log.clone();
        s.rules = rules(&["github.com:22"]);
        let allowed = lookup(&s.rules, &[], "github.com");
        s.note_lookup("github.com", &allowed);
        s.note_lookup("GITHUB.com.", &allowed);
        s.note_lookup("evil.example", &Lookup::default());
        s.note_lookup("not a name", &Lookup::default());
        for i in 0..(MAX_RECORDED_NAMES + 10) {
            s.note_lookup(&format!("n{i}.example"), &Lookup::default());
        }
        let mut reader = log.reader(sbx_events::Start::Beginning).unwrap();
        let mut events = Vec::new();
        while let Some(e) = reader.next_event().unwrap() {
            events.push(e);
        }
        let actions: Vec<&str> = events.iter().map(|e| e.action.as_str()).collect();
        assert_eq!(
            actions.len(),
            MAX_RECORDED_NAMES + 1,
            "one per name, then one notice"
        );
        assert_eq!(actions[0], "lookup");
        assert_eq!(events[0].attributes["host"], "github.com");
        assert_eq!(events[0].attributes["ports"], "22");
        assert_eq!(actions[1], "refuse");
        assert_eq!(events[1].attributes["host"], "evil.example");
        assert_eq!(events[1].attributes["via"], "dns");
        assert!(!events[1].attributes.contains_key("port"));
        assert_eq!(*actions.last().unwrap(), "notice");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_fake_range_is_the_same_on_both_sides() {
        assert_eq!(sbx_model::FAKE_RANGE, sbx_guest_proto::FAKE_RANGE);
        for ip in [
            "198.18.0.0",
            "198.19.255.255",
            "198.17.255.255",
            "198.20.0.0",
            "10.0.0.1",
        ] {
            let ip: std::net::Ipv4Addr = ip.parse().unwrap();
            assert_eq!(
                sbx_model::in_fake_range(ip),
                sbx_guest_proto::in_fake_range(ip),
                "{ip}"
            );
        }
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
