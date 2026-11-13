use super::*;

/// `name` 질의(EDNS OPT 레코드가 붙은 것).
fn query_for(name: &str, qtype: u16) -> Vec<u8> {
    let mut q = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1];
    q.extend_from_slice(&encode_name(name).unwrap());
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&CLASS_IN.to_be_bytes());
    // OPT: 이름 0, TYPE 41, CLASS 4096, TTL 0, RDLEN 0.
    q.extend_from_slice(&[0, 0, 41, 0x10, 0, 0, 0, 0, 0, 0, 0]);
    q
}

/// `example.com` A 질의.
fn query() -> Vec<u8> {
    query_for("example.com", TYPE_A)
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
    assert!(parse(&q).is_none(), "a response");
    assert!(nxdomain(&q[..11]).is_none(), "short");
    let mut q = query();
    q.truncate(20);
    assert!(nxdomain(&q).is_none(), "cut in the name");
    assert!(parse(&q).is_none(), "cut in the name");
    let mut q = query();
    q[12] = 0xc0;
    assert!(nxdomain(&q).is_none(), "compressed name");
    assert!(parse(&q).is_none(), "compressed name");
    // opcode 2 (STATUS) → NOTIMP. 켜져 있어도 parse가 받지 않으므로 같다.
    let mut q = query();
    q[2] = 0x10;
    assert_eq!(nxdomain(&q).unwrap()[3] & 0xf, NOTIMP as u8);
    assert!(parse(&q).is_none());
    // 질문 둘: parse는 받지 않는다(꺼져 있을 때처럼 NXDOMAIN).
    let mut q = query_for("a.b", TYPE_A);
    q.truncate(q.len() - 11);
    let second = q[12..].to_vec();
    q.extend_from_slice(&second);
    q[5] = 2;
    assert!(parse(&q).is_none());
    assert_eq!(nxdomain(&q).unwrap()[3] & 0xf, NXDOMAIN as u8);
}

#[test]
fn a_question_is_read_lowercase_and_odd_names_are_none() {
    let q = parse(&query_for("GitHub.COM", TYPE_AAAA)).unwrap();
    assert_eq!(q.name.as_deref(), Some("github.com"));
    assert_eq!(q.qtype, TYPE_AAAA);
    assert_eq!(q.qclass, CLASS_IN);
    // 호스트 이름으로 쓸 수 없는 바이트.
    let mut raw = query_for("a.b", TYPE_A);
    raw[13] = b'.';
    assert_eq!(parse(&raw).unwrap().name, None);
    raw[13] = 0xff;
    assert_eq!(parse(&raw).unwrap().name, None);
    // 뿌리 이름(빈 이름).
    let mut raw = vec![0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0];
    raw.extend_from_slice(&[0, 1, 0, 1]);
    assert_eq!(parse(&raw).unwrap().name, None);
    // 253자를 넘는 이름(조각 63자 넷). 질의 꼴로는 읽지만 이름은 없다.
    let mut raw = vec![0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for c in *b"abcd" {
        raw.push(63);
        raw.extend(std::iter::repeat_n(c, 63));
    }
    raw.extend_from_slice(&[0, 0, 1, 0, 1]);
    assert_eq!(parse(&raw).unwrap().name, None);
}

#[test]
fn answers_carry_one_record_pointing_at_the_question() {
    let raw = query_for("github.com", TYPE_A);
    let q = parse(&raw).unwrap();
    let r = respond(&q, &Answer::A(Ipv4Addr::new(198, 18, 0, 7)));
    // QR, RD, RA, NOERROR. 질문 1, 답 1.
    assert_eq!(&r[2..4], &[0x81, 0x80]);
    assert_eq!(&r[4..12], &[0, 1, 0, 1, 0, 0, 0, 0]);
    let qend = 12 + 12 + 4;
    assert_eq!(&r[12..qend], &raw[12..qend]);
    let rr = &r[qend..];
    assert_eq!(&rr[..2], &[0xc0, 0x0c]);
    assert_eq!(&rr[2..4], &TYPE_A.to_be_bytes());
    assert_eq!(&rr[4..6], &CLASS_IN.to_be_bytes());
    assert_eq!(&rr[6..10], &TTL.to_be_bytes());
    assert_eq!(&rr[10..12], &[0, 4]);
    assert_eq!(&rr[12..], &[198, 18, 0, 7]);

    let r = respond(&q, &Answer::Aaaa(Ipv6Addr::LOCALHOST));
    assert_eq!(r.len(), qend + 12 + 16);
    let r = respond(&q, &Answer::Ptr("github.com".into()));
    assert_eq!(&r[qend + 2..qend + 4], &TYPE_PTR.to_be_bytes());
    assert_eq!(&r[qend + 12..], &encode_name("github.com").unwrap()[..]);

    // 답이 없는 것: 질문만, rcode만 다르다.
    for (a, rcode) in [
        (Answer::NoData, 0u8),
        (Answer::NxDomain, 3),
        (Answer::ServFail, 2),
        (Answer::NotImp, 4),
    ] {
        let r = respond(&q, &a);
        assert_eq!(r.len(), qend, "{a:?}");
        assert_eq!(r[3] & 0xf, rcode, "{a:?}");
        assert_eq!(&r[6..8], &[0, 0], "{a:?}");
    }
}

struct Fixed(Answer);

impl Resolver for Fixed {
    fn enabled(&self) -> bool {
        true
    }

    fn answer(&self, name: &str, qtype: u16) -> Answer {
        assert_eq!(name, "example.com");
        assert_eq!(qtype, TYPE_A);
        self.0.clone()
    }
}

#[test]
fn non_in_classes_and_odd_names_do_not_reach_the_resolver() {
    let r = Fixed(Answer::ServFail);
    let mut raw = query();
    let q = parse(&raw).unwrap();
    assert_eq!(answer_query(&q, &r), Answer::ServFail);
    // CHAOS class.
    raw[12 + 13 + 3] = 3;
    assert_eq!(answer_query(&parse(&raw).unwrap(), &r), Answer::NotImp);
    let mut raw = query();
    raw[13] = b'!';
    assert_eq!(answer_query(&parse(&raw).unwrap(), &r), Answer::NxDomain);
}

fn ask(server: std::net::SocketAddr, q: &[u8]) -> Vec<u8> {
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    client.send_to(q, server).unwrap();
    let mut buf = [0u8; 512];
    let (n, _) = client.recv_from(&mut buf).unwrap();
    buf[..n].to_vec()
}

#[test]
fn serves_over_udp_off_and_on() {
    let off = UdpSocket::bind("127.0.0.1:0").unwrap();
    let off_addr = off.local_addr().unwrap();
    std::thread::spawn(move || serve(&off, Arc::new(Off)));
    let r = ask(off_addr, &query());
    assert_eq!(r[3] & 0xf, NXDOMAIN as u8);
    assert_eq!(r.len(), HEADER + 17);

    let on = UdpSocket::bind("127.0.0.1:0").unwrap();
    let on_addr = on.local_addr().unwrap();
    let resolver: Arc<dyn Resolver> = Arc::new(Fixed(Answer::A(Ipv4Addr::new(198, 18, 0, 1))));
    std::thread::spawn(move || serve(&on, resolver));
    let r = ask(on_addr, &query());
    assert_eq!(r[3] & 0xf, 0);
    assert_eq!(&r[r.len() - 4..], &[198, 18, 0, 1]);
}
