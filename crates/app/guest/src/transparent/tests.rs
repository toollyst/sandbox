use super::*;

use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;

/// 호스트의 루프백에서 흉내 내는 [`Net`]: 가짜 주소 대신 `127.0.0.1`의 빈 포트에서 듣고, 어디서 듣는지 적어 둔다.
#[derive(Default)]
struct LoopNet {
    routes: Mutex<HashMap<SocketAddrV4, SocketAddr>>,
    made_local: Mutex<Vec<Ipv4Addr>>,
    range_local: AtomicBool,
    resolv_root: Option<std::path::PathBuf>,
}

impl Net for Arc<LoopNet> {
    fn make_range_local(&self) -> io::Result<()> {
        self.range_local.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn make_local(&self, ip: Ipv4Addr) -> io::Result<()> {
        self.made_local.lock().unwrap().push(ip);
        Ok(())
    }

    fn listen(&self, addr: SocketAddrV4) -> io::Result<TcpListener> {
        let l = TcpListener::bind("127.0.0.1:0")?;
        self.routes.lock().unwrap().insert(addr, l.local_addr()?);
        Ok(l)
    }

    fn point_resolver(&self) -> io::Result<()> {
        match &self.resolv_root {
            Some(r) => write_resolv_conf(r),
            None => Ok(()),
        }
    }
}

impl LoopNet {
    fn route(&self, addr: &str) -> Option<SocketAddr> {
        self.routes
            .lock()
            .unwrap()
            .get(&addr.parse::<SocketAddrV4>().unwrap())
            .copied()
    }
}

/// 허용 목록 대신 정한 답을 주는 호스트. 물은 수와 넘겨받은 연결을 센다.
struct FakeHost {
    answers: HashMap<&'static str, (Vec<u16>, bool)>,
    lookups: AtomicUsize,
    passed: Mutex<Vec<(String, u16)>>,
    silent: bool,
}

impl FakeHost {
    fn new(answers: &[(&'static str, &[u16], bool)]) -> Arc<Self> {
        Arc::new(Self {
            answers: answers
                .iter()
                .map(|(n, p, any)| (*n, (p.to_vec(), *any)))
                .collect(),
            lookups: AtomicUsize::new(0),
            passed: Mutex::default(),
            silent: false,
        })
    }
}

impl Host for FakeHost {
    fn lookup(&self, name: &str) -> Option<(Vec<u16>, bool)> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        if self.silent {
            return None;
        }
        Some(self.answers.get(name).cloned().unwrap_or_default())
    }

    fn pass(&self, mut client: TcpStream, host: &str, port: u16) {
        self.passed.lock().unwrap().push((host.to_owned(), port));
        let _ = write!(client, "to {host}:{port}");
    }
}

fn setup(answers: &[(&'static str, &[u16], bool)]) -> (Transparent, Arc<LoopNet>, Arc<FakeHost>) {
    let net = Arc::new(LoopNet::default());
    let t = Transparent::new(Some(Box::new(net.clone())));
    let host = FakeHost::new(answers);
    t.enable(host.clone(), &[]).unwrap();
    (t, net, host)
}

fn dyn_host(h: &Arc<FakeHost>) -> Arc<dyn Host> {
    h.clone()
}

fn read_all(addr: SocketAddr) -> String {
    let mut c = TcpStream::connect(addr).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut s = String::new();
    c.read_to_string(&mut s).unwrap();
    s
}

#[test]
fn a_guest_without_a_net_cannot_turn_it_on() {
    let t = Transparent::default();
    let err = t.enable(FakeHost::new(&[]), &[]).unwrap_err();
    assert!(err.contains("not supported"), "{err}");
    assert!(!t.is_on());
}

#[test]
fn allowed_names_get_stable_fake_addresses_and_listeners_before_the_answer() {
    let (t, net, host) = setup(&[
        ("github.com", &[22, 443, 22], false),
        ("pypi.org", &[443], false),
    ]);
    assert!(net.range_local.load(Ordering::SeqCst));
    let h = dyn_host(&host);
    assert_eq!(
        t.answer(&h, "github.com", dns::TYPE_A),
        Answer::A(Ipv4Addr::new(198, 18, 0, 1))
    );
    // 답하기 전에 이미 듣고 있다. 포트는 겹치지 않게.
    assert!(net.route("198.18.0.1:22").is_some());
    assert!(net.route("198.18.0.1:443").is_some());
    assert_eq!(net.routes.lock().unwrap().len(), 2);
    assert_eq!(
        t.answer(&h, "pypi.org", dns::TYPE_AAAA),
        Answer::NoData,
        "AAAA of an allowed name: the name exists, no IPv6 record"
    );
    assert_eq!(
        t.answer(&h, "pypi.org", dns::TYPE_A),
        Answer::A(Ipv4Addr::new(198, 18, 0, 2))
    );
    // 같은 이름은 같은 주소이고 다시 묻지 않는다.
    assert_eq!(
        t.answer(&h, "github.com", dns::TYPE_A),
        Answer::A(Ipv4Addr::new(198, 18, 0, 1))
    );
    assert_eq!(t.answer(&h, "github.com", 16), Answer::NoData, "TXT");
    assert_eq!(host.lookups.load(Ordering::SeqCst), 2);
    // 역방향 이름.
    assert_eq!(
        t.answer(&h, "1.0.18.198.in-addr.arpa", dns::TYPE_PTR),
        Answer::Ptr("github.com".into())
    );
    assert_eq!(
        t.answer(&h, "9.0.18.198.in-addr.arpa", dns::TYPE_PTR),
        Answer::NxDomain
    );
    assert_eq!(
        t.name_of(Ipv4Addr::new(198, 18, 0, 2)).as_deref(),
        Some("pypi.org")
    );
    assert_eq!(host.lookups.load(Ordering::SeqCst), 2);

    // 연결은 이름과 포트로 호스트에 넘어간다.
    let to = net.route("198.18.0.1:22").unwrap();
    assert_eq!(read_all(to), "to github.com:22");
    assert_eq!(
        host.passed.lock().unwrap().as_slice(),
        &[("github.com".to_owned(), 22)]
    );
}

#[test]
fn refused_names_are_nxdomain_and_not_asked_again() {
    let (t, net, host) = setup(&[]);
    let h = dyn_host(&host);
    assert_eq!(t.answer(&h, "evil.example", dns::TYPE_A), Answer::NxDomain);
    assert_eq!(
        t.answer(&h, "evil.example", dns::TYPE_AAAA),
        Answer::NxDomain
    );
    assert_eq!(host.lookups.load(Ordering::SeqCst), 1);
    assert!(net.routes.lock().unwrap().is_empty());
}

#[test]
fn localhost_odd_names_and_a_silent_host() {
    let (t, _net, host) = setup(&[("localhost", &[80], false)]);
    let h = dyn_host(&host);
    // localhost는 게스트 자신이다. 호스트에 묻지 않는다.
    assert_eq!(
        t.answer(&h, "localhost", dns::TYPE_A),
        Answer::A(Ipv4Addr::LOCALHOST)
    );
    assert_eq!(
        t.answer(&h, "db.localhost", dns::TYPE_AAAA),
        Answer::Aaaa(Ipv6Addr::LOCALHOST)
    );
    assert_eq!(t.answer(&h, "localhost", 15), Answer::NoData);
    for odd in [
        "1.2.3.4",
        "a..b",
        "x.in-addr.arpa",
        "1.2.3.4.in-addr.arpa",
        &"a".repeat(64),
    ] {
        assert_eq!(t.answer(&h, odd, dns::TYPE_A), Answer::NxDomain, "{odd}");
    }
    assert_eq!(host.lookups.load(Ordering::SeqCst), 0);
    let silent = Arc::new(FakeHost {
        silent: true,
        ..Arc::into_inner(FakeHost::new(&[])).unwrap()
    });
    let h: Arc<dyn Host> = silent.clone();
    assert_eq!(t.answer(&h, "github.com", dns::TYPE_A), Answer::ServFail);
    // 답이 없었던 이름은 기억하지 않는다.
    assert_eq!(t.answer(&h, "github.com", dns::TYPE_A), Answer::ServFail);
    assert_eq!(silent.lookups.load(Ordering::SeqCst), 2);
}

#[test]
fn any_port_names_get_an_address_but_nothing_listens() {
    let (t, net, host) = setup(&[("cdn.example", &[], true), ("mixed.example", &[443], true)]);
    let h = dyn_host(&host);
    assert_eq!(
        t.answer(&h, "cdn.example", dns::TYPE_A),
        Answer::A(Ipv4Addr::new(198, 18, 0, 1))
    );
    assert!(net.routes.lock().unwrap().is_empty());
    assert_eq!(
        t.answer(&h, "mixed.example", dns::TYPE_A),
        Answer::A(Ipv4Addr::new(198, 18, 0, 2))
    );
    assert_eq!(net.routes.lock().unwrap().len(), 1);
}

#[test]
fn names_and_listeners_are_capped() {
    struct Many;
    impl Host for Many {
        fn lookup(&self, _: &str) -> Option<(Vec<u16>, bool)> {
            Some((vec![], true))
        }
        fn pass(&self, _: TcpStream, _: &str, _: u16) {}
    }
    let net = Arc::new(LoopNet::default());
    let mut t = Transparent::new(Some(Box::new(net.clone())));
    t.max_names = 300;
    let h: Arc<dyn Host> = Arc::new(Many);
    t.enable(h.clone(), &[]).unwrap();
    for i in 0..t.max_names {
        assert!(matches!(
            t.answer(&h, &format!("n{i}.example"), dns::TYPE_A),
            Answer::A(_)
        ));
    }
    assert_eq!(
        t.answer(&h, "one-too-many.example", dns::TYPE_A),
        Answer::ServFail
    );
    assert_eq!(
        t.answer(&h, "n299.example", dns::TYPE_A),
        Answer::A(Ipv4Addr::new(198, 18, 1, 44))
    );
    assert!(sbx_guest_proto::in_fake_range(fake_addr(MAX_NAMES)));

    // 듣는 소켓: 상한을 넘으면 상한까지만. 남은 이름도 주소는 받는다.
    let net = Arc::new(LoopNet::default());
    let mut t = Transparent::new(Some(Box::new(net.clone())));
    t.max_listeners = 3;
    let host = FakeHost::new(&[
        ("big.example", &[1, 2, 3, 4, 5], false),
        ("next.example", &[9], false),
    ]);
    let h = dyn_host(&host);
    t.enable(h.clone(), &[]).unwrap();
    assert!(matches!(
        t.answer(&h, "big.example", dns::TYPE_A),
        Answer::A(_)
    ));
    assert_eq!(net.routes.lock().unwrap().len(), 3);
    assert!(matches!(
        t.answer(&h, "next.example", dns::TYPE_A),
        Answer::A(_)
    ));
    assert_eq!(net.routes.lock().unwrap().len(), 3);
    let err = t
        .enable(h, &["140.82.112.3:22".parse().unwrap()])
        .unwrap_err();
    assert!(err.contains("more than 3 listeners"), "{err}");
}

#[test]
fn ip_rules_are_made_local_and_listened_for() {
    let net = Arc::new(LoopNet::default());
    let t = Transparent::new(Some(Box::new(net.clone())));
    let host = FakeHost::new(&[]);
    let err = t
        .enable(
            host.clone(),
            &[
                "140.82.112.3:22".parse().unwrap(),
                "127.0.0.1:80".parse().unwrap(),
                "198.18.0.5:80".parse().unwrap(),
            ],
        )
        .unwrap_err();
    assert!(
        err.contains("127.0.0.1:80") && err.contains("198.18.0.5:80"),
        "{err}"
    );
    assert!(!err.contains("140.82"), "{err}");
    assert!(t.is_on(), "what could be done stays done");
    assert_eq!(
        net.made_local.lock().unwrap().as_slice(),
        &[Ipv4Addr::new(140, 82, 112, 3)]
    );
    let to = net.route("140.82.112.3:22").unwrap();
    assert_eq!(read_all(to), "to 140.82.112.3:22");
}

#[test]
fn resolv_conf_is_written_in_place_of_the_image_one_or_its_link() {
    let root = std::env::temp_dir().join(format!("sbx-resolv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("etc")).unwrap();
    std::fs::create_dir_all(root.join("run")).unwrap();
    // 이미지가 링크를 두었다: 링크 대상은 바뀌지 않는다.
    std::fs::write(root.join("run/stub"), "nameserver 8.8.8.8\n").unwrap();
    std::os::unix::fs::symlink("../run/stub", root.join("etc/resolv.conf")).unwrap();
    let net = Arc::new(LoopNet {
        resolv_root: Some(root.clone()),
        ..LoopNet::default()
    });
    let t = Transparent::new(Some(Box::new(net)));
    t.enable(FakeHost::new(&[]), &[]).unwrap();
    let p = root.join("etc/resolv.conf");
    assert!(std::fs::symlink_metadata(&p).unwrap().file_type().is_file());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), RESOLV_CONF);
    assert_eq!(
        std::fs::read_to_string(root.join("run/stub")).unwrap(),
        "nameserver 8.8.8.8\n"
    );
    // `etc`가 없어도 된다. 다시 써도 된다.
    let bare = root.join("bare");
    write_resolv_conf(&bare).unwrap();
    write_resolv_conf(&bare).unwrap();
    assert_eq!(
        std::fs::read_to_string(bare.join("etc/resolv.conf")).unwrap(),
        RESOLV_CONF
    );
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn reverse_names_and_plain_names() {
    assert_eq!(
        reverse("7.0.18.198.in-addr.arpa"),
        Some(Ipv4Addr::new(198, 18, 0, 7))
    );
    for bad in [
        "7.0.18.in-addr.arpa",
        "1.7.0.18.198.in-addr.arpa",
        "07.0.18.198.in-addr.arpa",
        "256.0.18.198.in-addr.arpa",
        "x.0.18.198.in-addr.arpa",
        "7.0.18.198.ip6.arpa",
    ] {
        assert_eq!(reverse(bad), None, "{bad}");
    }
    assert!(plain_name("github.com"));
    assert!(plain_name("_svc.a-b.example"));
    assert!(!plain_name("10.0.0.1"));
    assert!(!plain_name("a b"));
    assert!(!plain_name(""));
    assert_eq!(fake_addr(1), Ipv4Addr::new(198, 18, 0, 1));
    assert_eq!(fake_addr(256), Ipv4Addr::new(198, 18, 1, 0));
}

/// 실제 커널: 범위를 local로 잡고 그 안의 아무 주소에서 듣고 연결한다. IP 규칙의 `/32`도.
/// `CAP_NET_ADMIN`이 필요하고 경로 표를 바꾸므로 `SANDBOX_PRIVILEGED_TESTS=1`일 때만 돈다(privileged 컨테이너).
#[cfg(target_os = "linux")]
#[test]
fn privileged_kernel_routes_the_fake_range_to_listeners() {
    if std::env::var_os("SANDBOX_PRIVILEGED_TESTS").is_none() {
        eprintln!("SANDBOX_PRIVILEGED_TESTS not set, skipping");
        return;
    }
    // 루프백이 내려가 있을 수 있다(새 네트워크 이름 공간).
    let _ = std::process::Command::new("ip")
        .args(["link", "set", "lo", "up"])
        .status();
    let net = KernelNet;
    net.make_range_local().unwrap();
    // 두 번 해도 된다(다시 시작한 게스트).
    net.make_range_local().unwrap();
    for addr in ["198.18.0.9:2222", "198.19.255.254:443"] {
        let a: SocketAddrV4 = addr.parse().unwrap();
        let l = net.listen(a).unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            c.write_all(b"hi").unwrap();
        });
        let mut s = String::new();
        TcpStream::connect(a)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        t.join().unwrap();
        assert_eq!(s, "hi", "{addr}");
    }
    // 듣지 않는 포트는 곧바로 거부된다(시간 초과가 아니다).
    let e = TcpStream::connect_timeout(&"198.18.0.9:2223".parse().unwrap(), Duration::from_secs(2))
        .unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
    // IP 규칙(TEST-NET-3).
    let ip = Ipv4Addr::new(203, 0, 113, 7);
    net.make_local(ip).unwrap();
    let a = SocketAddrV4::new(ip, 5432);
    let l = net.listen(a).unwrap();
    let t = std::thread::spawn(move || l.accept().map(|_| ()).unwrap());
    TcpStream::connect(a).unwrap();
    t.join().unwrap();
    let out = std::process::Command::new("ip")
        .args(["route", "show", "table", "local"])
        .output();
    if let Ok(o) = out {
        let table = String::from_utf8_lossy(&o.stdout);
        assert!(table.contains("local 198.18.0.0/15 dev lo"), "{table}");
        assert!(table.contains("local 203.0.113.7 dev lo"), "{table}");
    }
}

/// 연결 처리 전체: 호스트 노릇을 하는 테스트가 `Transparent`로 켜고, 연결을 맡기고, 이름 묻기와 `Connect`에 답한다.
/// DNS는 UDP로, 프로그램은 듣는 소켓에 TCP로 붙는다(가짜 주소 대신 [`LoopNet`]이 적어 둔 곳).
#[test]
fn the_whole_path_over_the_guest_protocol() {
    use sbx_guest_proto::{GuestMsg, HostMsg, PROTOCOL_VERSION};
    use std::net::UdpSocket;
    use std::os::unix::net::UnixStream;

    let net = Arc::new(LoopNet::default());
    let egress = Arc::new(crate::egress::Egress::with_net(Box::new(net.clone())));
    let ctx = Arc::new(crate::server::Ctx {
        root: std::env::temp_dir(),
        on_shutdown: Box::new(|| {}),
        share: None,
        egress: egress.clone(),
        main: Default::default(),
        layers: None,
        clock: None,
    });
    let conn = || {
        let (mut host, guest) = UnixStream::pair().unwrap();
        let ctx = ctx.clone();
        std::thread::spawn(move || crate::server::serve(guest, &ctx));
        HostMsg::Hello {
            version: PROTOCOL_VERSION,
        }
        .write_to(&mut host)
        .unwrap();
        assert!(matches!(
            GuestMsg::read_from(&mut host).unwrap(),
            GuestMsg::HelloAck { .. }
        ));
        host
    };
    let dns = UdpSocket::bind("127.0.0.1:0").unwrap();
    let dns_addr = dns.local_addr().unwrap();
    let resolver = egress.resolver();
    std::thread::spawn(move || dns::serve(&dns, resolver));
    let ask = |name: &str| {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.extend_from_slice(&[0, 0, 1, 0, 1]);
        let c = UdpSocket::bind("127.0.0.1:0").unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.send_to(&q, dns_addr).unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = c.recv_from(&mut buf).unwrap();
        buf[..n].to_vec()
    };

    // 켜기 전: NXDOMAIN, 호스트에 묻지 않는다.
    assert_eq!(ask("github.com")[3] & 0xf, 3);

    let mut c = conn();
    HostMsg::Transparent { literals: vec![] }
        .write_to(&mut c)
        .unwrap();
    assert_eq!(GuestMsg::read_from(&mut c).unwrap(), GuestMsg::Done);

    // 연결 하나를 맡긴다. 이름 묻기와 그 뒤의 Connect가 같은 연결로 온다.
    let mut slot = conn();
    HostMsg::EgressSlot.write_to(&mut slot).unwrap();
    let host = std::thread::spawn(move || {
        let mut seen = Vec::new();
        loop {
            match GuestMsg::read_from(&mut slot).unwrap() {
                GuestMsg::Lookup { name } => {
                    let ports = if name == "github.com" {
                        vec![22]
                    } else {
                        vec![]
                    };
                    seen.push(name);
                    HostMsg::Resolved {
                        ports,
                        any_port: false,
                    }
                    .write_to(&mut slot)
                    .unwrap();
                }
                GuestMsg::Connect { host, port } => {
                    seen.push(format!("connect {host}:{port}"));
                    HostMsg::Connected.write_to(&mut slot).unwrap();
                    HostMsg::Data(b"SSH-2.0-test\r\n".to_vec())
                        .write_to(&mut slot)
                        .unwrap();
                    HostMsg::End.write_to(&mut slot).unwrap();
                    let mut got = Vec::new();
                    loop {
                        match GuestMsg::read_from(&mut slot).unwrap() {
                            GuestMsg::Data(b) => got.extend(b),
                            GuestMsg::End => break,
                            other => panic!("{other:?}"),
                        }
                    }
                    seen.push(String::from_utf8(got).unwrap());
                    return seen;
                }
                other => panic!("{other:?}"),
            }
        }
    });

    let r = ask("evil.example");
    assert_eq!(r[3] & 0xf, 3, "refused name: NXDOMAIN");
    let r = ask("GitHub.com");
    assert_eq!(r[3] & 0xf, 0);
    assert_eq!(&r[r.len() - 4..], &[198, 18, 0, 1]);

    let to = net
        .route("198.18.0.1:22")
        .expect("listening before the answer");
    let mut client = TcpStream::connect(to).unwrap();
    client.write_all(b"SSH-2.0-client\r\n").unwrap();
    client.shutdown(std::net::Shutdown::Write).unwrap();
    let mut banner = String::new();
    client.read_to_string(&mut banner).unwrap();
    assert_eq!(banner, "SSH-2.0-test\r\n");
    assert_eq!(
        host.join().unwrap(),
        [
            "evil.example",
            "github.com",
            "connect github.com:22",
            "SSH-2.0-client\r\n"
        ]
    );

    // 프록시에 가짜 주소로 와도 이름으로 되돌려 묻는다.
    let mut slot = conn();
    HostMsg::EgressSlot.write_to(&mut slot).unwrap();
    let port = egress.listen().unwrap();
    let mut p = TcpStream::connect(("127.0.0.1", port)).unwrap();
    p.write_all(b"CONNECT 198.18.0.1:443 HTTP/1.1\r\n\r\n")
        .unwrap();
    assert_eq!(
        GuestMsg::read_from(&mut slot).unwrap(),
        GuestMsg::Connect {
            host: "github.com".into(),
            port: 443
        }
    );
    HostMsg::Refused {
        reason: "github.com:443 is not allowed".into(),
    }
    .write_to(&mut slot)
    .unwrap();
    let mut reply = String::new();
    p.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 403"), "{reply}");
}
