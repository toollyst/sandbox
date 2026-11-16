//! 투명 TCP (게스트 쪽): 프록시를 모르는 프로그램도 허용된 `HOST:PORT`에 닿게 한다. VM에는 여전히 네트워크 장치가
//! 없고, 허용 목록은 여전히 이름 단위이고, 확인은 여전히 호스트가 한다.
//!
//! `HostMsg::Transparent`로 켠다(`--network allow`나 `--connect`일 때 시작에서 한 번). 켜기 전에는 아무것도 하지 않는다:
//! DNS는 모든 이름에 NXDOMAIN이고 `/etc/resolv.conf`는 이미지 그대로다.
//!
//! **켜기** ([`Transparent::enable`], 이 차례로)
//! 1. 가짜 주소 범위 [`FAKE_RANGE`] 전체를 루프백의 local 경로로 잡는다
//!    (`ip route add local 198.18.0.0/15 dev lo`와 같은 netlink 메시지 하나). 그러면 범위 안의 어느 주소에서나 들을 수
//!    있다. 실패하면 나머지도 하지 않는다(DNS는 계속 NXDOMAIN, `resolv.conf`도 그대로).
//! 2. DNS가 이름을 호스트에 묻게 한다.
//! 3. `/etc/resolv.conf`를 `nameserver 127.0.0.1`로 쓴다(쓰기 층에, 이미지는 그대로. 이미지가 링크를 두었으면 링크를
//!    파일로 바꾼다). 이미지가 바깥 이름 서버를 적어 두었어도 질의가 루프백 DNS로 온다.
//! 4. IP로 적은 규칙(호스트가 고른 IPv4 주소)마다 그 주소를 local(`/32`)로 잡고 그 포트에서 듣는다.
//!
//! **이름** ([`Transparent::answer`], DNS 질의 하나)
//! - `localhost`와 `*.localhost`는 호스트에 묻지 않고 루프백으로 답한다(RFC 6761, A는 `127.0.0.1`, AAAA는 `::1`).
//!   게스트 안의 `localhost`는 게스트 자신이다.
//! - 가짜 주소의 역방향 이름(`7.0.18.198.in-addr.arpa`)에는 그 주소를 받은 이름으로 답한다(PTR). 다른 PTR은 NXDOMAIN.
//! - 이미 주소를 준 이름: A면 그 주소, 다른 종류면 "이름은 있고 레코드는 없다"(NOERROR, 빈 답). AAAA도 그렇다.
//!   그래서 프로그램은 IPv4로 간다.
//! - 처음 보는 이름은 맡겨 둔 연결로 호스트에 묻는다([`Host::lookup`], `GuestMsg::Lookup`). 호스트는 허용 목록으로만
//!   답하고 이름을 풀지 않는다. 허용되지 않으면 NXDOMAIN이고, 그 이름은 [`MAX_REFUSED`]개까지 기억해 다시 묻지 않는다.
//!   답이 없으면(맡긴 연결이 없음) SERVFAIL이다.
//! - 허용된 이름에는 범위에서 다음 주소를 주고, 호스트가 알려 준 포트마다 그 주소에서 듣는 소켓을 연 **뒤에** 답한다.
//!   주소는 VM이 도는 동안 그 이름에 묶이고 다른 이름에 주지 않는다. 포트가 `*`인 규칙만 맞은 이름도 주소는 받지만
//!   들을 포트가 없다(그 연결은 거부된다. 프록시로는 닿는다).
//! - 질의 종류와 상관없이 처음 물을 때 주소를 준다(A와 AAAA가 함께 오므로).
//!
//! **연결**: 듣는 소켓에 연결이 오면 주소를 이름으로 되돌려 [`Host::pass`]로 넘긴다. 프록시가 보내는 것과 같은
//! `Connect { host: 이름, port }`다. 호스트의 확인(이름 풀기, 사설 주소 거부, `--egress-limit`, 기록)은 같다.
//! TCP 연결은 호스트가 답하기 전에 이미 맺어진다. 거부되면 프로그램은 바로 닫힌 연결을 본다.
//!
//! **상한**: 주소를 받은 이름 [`MAX_NAMES`]개, 듣는 소켓 [`MAX_LISTENERS`]개(IP 규칙 포함). 넘으면 새 이름에는 SERVFAIL이고
//! 콘솔에 적는다. 호스트는 게스트를 믿지 않으므로 이 상한은 게스트 자신을 지키는 것이다(호스트 쪽 상한은 호스트에 있다).
//!
//! **할 수 없는 것**: 포트가 `*`인 규칙(어느 포트로 올지 몰라 들을 수 없다), IPv6, UDP(DNS 밖), 자체 DNS를 쓰는
//! 프로그램(DoH, 이름 서버를 직접 고르는 것).

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sbx_guest_proto::FAKE_RANGE;

use crate::dns::{self, Answer};

/// 주소를 받을 수 있는 이름의 수.
pub const MAX_NAMES: usize = 4096;

/// 듣는 소켓의 수.
pub const MAX_LISTENERS: usize = 1024;

/// 기억하는 거부된 이름의 수. 넘으면 비우고 다시 센다.
pub const MAX_REFUSED: usize = 4096;

/// `resolv.conf` 내용.
pub const RESOLV_CONF: &str = "# Written by sbx (--network allow or --connect): names are answered by the sandbox's loopback DNS.\nnameserver 127.0.0.1\n";

/// 게스트 커널에 하는 일. 게스트에서는 `KernelNet`(Linux), fake 실행기는 호스트에서 흉내 낸다.
pub trait Net: Send + Sync {
    /// 가짜 주소 범위 전체를 local로 잡는다.
    fn make_range_local(&self) -> io::Result<()>;
    /// 주소 하나를 local로 잡는다(IP 규칙).
    fn make_local(&self, ip: Ipv4Addr) -> io::Result<()>;
    /// `addr`에서 듣는다.
    fn listen(&self, addr: SocketAddrV4) -> io::Result<TcpListener>;
    /// 리졸버가 루프백 DNS에 묻게 한다(`/etc/resolv.conf`).
    fn point_resolver(&self) -> io::Result<()>;
}

/// 호스트 쪽. 게스트에서는 맡겨 둔 연결로 묻는 `egress::Egress`다.
pub trait Host: Send + Sync + 'static {
    /// 이름이 허용 목록에 있는지 묻는다: (포트들, 포트가 `*`인 규칙이 맞았는지). 답이 없으면 `None`.
    fn lookup(&self, name: &str) -> Option<(Vec<u16>, bool)>;
    /// 받은 연결을 `Connect { host, port }`로 호스트에 넘기고 끝날 때까지 잇는다.
    fn pass(&self, client: TcpStream, host: &str, port: u16);
}

/// 투명 TCP 상태. 게스트 안에서 하나를 함께 쓴다(`egress::Egress`가 가진다).
pub struct Transparent {
    net: Option<Box<dyn Net>>,
    on: AtomicBool,
    book: Mutex<Book>,
    /// [`MAX_NAMES`], [`MAX_LISTENERS`] (테스트는 줄인다).
    max_names: usize,
    max_listeners: usize,
}

#[derive(Default)]
struct Book {
    names: HashMap<String, Ipv4Addr>,
    addrs: HashMap<Ipv4Addr, String>,
    refused: HashSet<String>,
    listeners: usize,
}

impl Default for Transparent {
    fn default() -> Self {
        Self::new(None)
    }
}

/// `n`번째(1부터) 가짜 주소.
fn fake_addr(n: usize) -> Ipv4Addr {
    Ipv4Addr::from(u32::from(FAKE_RANGE.0) + n as u32)
}

/// 가짜 주소의 역방향 이름(`d.c.b.a.in-addr.arpa`)이면 그 주소.
fn reverse(name: &str) -> Option<Ipv4Addr> {
    let rest = name.strip_suffix(".in-addr.arpa")?;
    let mut o = [0u8; 4];
    let mut parts = rest.split('.');
    for i in (0..4).rev() {
        let p = parts.next()?;
        if p.len() > 1 && p.starts_with('0') {
            return None;
        }
        o[i] = p.parse().ok()?;
    }
    parts.next().is_none().then_some(Ipv4Addr::from(o))
}

/// 호스트에 물을 만한 이름인지: 글자 조각 1..=63자, 모두 253자 안, IPv4 주소 꼴이 아님.
fn plain_name(name: &str) -> bool {
    name.len() <= 253
        && name.parse::<Ipv4Addr>().is_err()
        && name.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

fn log(msg: &str) {
    eprintln!("sbx-guest: transparent tcp: {msg}");
}

/// 듣는 소켓 하나의 받기. 연결마다 스레드 하나로 호스트에 넘긴다.
fn accept_loop(listener: TcpListener, host: Arc<dyn Host>, name: String, port: u16) {
    loop {
        match listener.accept() {
            Ok((client, _)) => {
                let (host, name) = (host.clone(), name.clone());
                let _ = std::thread::Builder::new()
                    .stack_size(256 * 1024)
                    .spawn(move || host.pass(client, &name, port));
            }
            // 파일 기술자가 모자라는 등. 같은 오류로 돌지 않게 잠깐 쉰다.
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

impl Transparent {
    /// `net`이 없으면 켤 수 없다([`Transparent::enable`]가 오류).
    pub fn new(net: Option<Box<dyn Net>>) -> Self {
        Self {
            net,
            on: AtomicBool::new(false),
            book: Mutex::default(),
            max_names: MAX_NAMES,
            max_listeners: MAX_LISTENERS,
        }
    }

    /// 켜져 있는지.
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::SeqCst)
    }

    fn book(&self) -> std::sync::MutexGuard<'_, Book> {
        self.book.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 켠다. 차례는 모듈 문서에 있다. 하지 못한 것을 모아 오류로 돌려준다(한 것은 남는다).
    pub fn enable(&self, host: Arc<dyn Host>, literals: &[SocketAddrV4]) -> Result<(), String> {
        let Some(net) = &self.net else {
            return Err("transparent tcp is not supported by this guest".into());
        };
        net.make_range_local()
            .map_err(|e| format!("cannot make {}/{} local: {e}", FAKE_RANGE.0, FAKE_RANGE.1))?;
        self.on.store(true, Ordering::SeqCst);
        let mut failed = Vec::new();
        if let Err(e) = net.point_resolver() {
            failed.push(format!("cannot write /etc/resolv.conf: {e}"));
        }
        for a in literals {
            let ip = *a.ip();
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() || ip.is_broadcast() {
                failed.push(format!("{a}: not a remote address"));
                continue;
            }
            if sbx_guest_proto::in_fake_range(ip) {
                failed.push(format!("{a}: inside {}/{}", FAKE_RANGE.0, FAKE_RANGE.1));
                continue;
            }
            let mut book = self.book();
            if book.listeners >= self.max_listeners {
                failed.push(format!("{a}: more than {} listeners", self.max_listeners));
                continue;
            }
            match net.make_local(ip).and_then(|()| net.listen(*a)) {
                Ok(l) => {
                    book.listeners += 1;
                    let (host, name) = (host.clone(), ip.to_string());
                    let port = a.port();
                    std::thread::spawn(move || accept_loop(l, host, name, port));
                }
                Err(e) => failed.push(format!("{a}: {e}")),
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join("; "))
        }
    }

    /// 가짜 주소를 받은 이름. 프록시가 가짜 주소로 온 요청을 이름으로 되돌릴 때 쓴다.
    pub fn name_of(&self, ip: Ipv4Addr) -> Option<String> {
        self.book().addrs.get(&ip).cloned()
    }

    /// DNS 질의 하나의 답. 규칙은 모듈 문서에 있다. `name`은 소문자이고 끝의 `.`이 없다.
    pub fn answer(&self, host: &Arc<dyn Host>, name: &str, qtype: u16) -> Answer {
        if name == "localhost" || name.ends_with(".localhost") {
            return match qtype {
                dns::TYPE_A => Answer::A(Ipv4Addr::LOCALHOST),
                dns::TYPE_AAAA => Answer::Aaaa(Ipv6Addr::LOCALHOST),
                _ => Answer::NoData,
            };
        }
        if name.ends_with(".arpa") {
            return match reverse(name).and_then(|ip| self.name_of(ip)) {
                Some(n) if qtype == dns::TYPE_PTR => Answer::Ptr(n),
                Some(_) => Answer::NoData,
                None => Answer::NxDomain,
            };
        }
        if !plain_name(name) {
            return Answer::NxDomain;
        }
        {
            let book = self.book();
            if let Some(ip) = book.names.get(name) {
                return record(*ip, qtype);
            }
            if book.refused.contains(name) {
                return Answer::NxDomain;
            }
        }
        let Some((ports, any_port)) = host.lookup(name) else {
            return Answer::ServFail;
        };
        if ports.is_empty() && !any_port {
            let mut book = self.book();
            if book.refused.len() >= MAX_REFUSED {
                book.refused.clear();
            }
            book.refused.insert(name.to_owned());
            return Answer::NxDomain;
        }
        match self.assign(host, name, &ports) {
            Some(ip) => record(ip, qtype),
            None => Answer::ServFail,
        }
    }

    /// 이름에 주소를 주고 포트마다 듣는다. 이미 주었으면 그 주소. 상한을 넘으면 `None`.
    fn assign(&self, host: &Arc<dyn Host>, name: &str, ports: &[u16]) -> Option<Ipv4Addr> {
        let net = self.net.as_ref()?;
        let mut book = self.book();
        if let Some(ip) = book.names.get(name) {
            return Some(*ip);
        }
        if book.names.len() >= self.max_names {
            log(&format!(
                "more than {} names; {name} gets no address",
                self.max_names
            ));
            return None;
        }
        let ip = fake_addr(book.names.len() + 1);
        let mut ports = ports.to_vec();
        ports.sort_unstable();
        ports.dedup();
        for port in ports {
            if book.listeners >= self.max_listeners {
                log(&format!(
                    "more than {} listeners; {name}:{port} is not listened for",
                    self.max_listeners
                ));
                break;
            }
            match net.listen(SocketAddrV4::new(ip, port)) {
                Ok(l) => {
                    book.listeners += 1;
                    let (host, name) = (host.clone(), name.to_owned());
                    std::thread::spawn(move || accept_loop(l, host, name, port));
                }
                Err(e) => log(&format!("listen on {ip}:{port} for {name}: {e}")),
            }
        }
        book.names.insert(name.to_owned(), ip);
        book.addrs.insert(ip, name.to_owned());
        Some(ip)
    }
}

fn record(ip: Ipv4Addr, qtype: u16) -> Answer {
    if qtype == dns::TYPE_A {
        Answer::A(ip)
    } else {
        Answer::NoData
    }
}

/// `root` 아래 `etc/resolv.conf`를 [`RESOLV_CONF`]로 쓴다. 임시 파일에 쓰고 이름을 바꾸므로 그 자리의 심볼릭 링크는
/// 링크 대상이 아니라 링크 자신이 바뀐다. `etc`가 없으면 만든다.
pub fn write_resolv_conf(root: &Path) -> io::Result<()> {
    let etc = root.join("etc");
    std::fs::create_dir_all(&etc)?;
    let tmp = etc.join(".resolv.conf.sbx");
    let _ = std::fs::remove_file(&tmp);
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)?;
        f.write_all(RESOLV_CONF.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, etc.join("resolv.conf"))
}

/// 게스트 커널에 하는 [`Net`]: netlink로 local 경로, 보통의 소켓, `/etc/resolv.conf`.
#[cfg(target_os = "linux")]
pub struct KernelNet;

#[cfg(target_os = "linux")]
impl Net for KernelNet {
    fn make_range_local(&self) -> io::Result<()> {
        crate::sys::linux::add_local_route(FAKE_RANGE.0, FAKE_RANGE.1)
    }

    fn make_local(&self, ip: Ipv4Addr) -> io::Result<()> {
        crate::sys::linux::add_local_route(ip, 32)
    }

    fn listen(&self, addr: SocketAddrV4) -> io::Result<TcpListener> {
        TcpListener::bind(addr)
    }

    fn point_resolver(&self) -> io::Result<()> {
        write_resolv_conf(Path::new("/"))
    }
}

#[cfg(test)]
mod tests;
