use super::*;

use std::io::{Cursor, Read};

fn vm() -> VmSpec {
    VmSpec {
        guest_init: "/c/guest/initramfs".into(),
        kernel: None,
        disks: vec![
            DiskSpec {
                path: "/c/rootfs.erofs".into(),
                role: DiskRole::Rootfs,
                readonly: true,
            },
            DiskSpec {
                path: "/d/scratch.img".into(),
                role: DiskRole::Scratch,
                readonly: false,
            },
        ],
        vcpus: 1,
        memory_mib: 512,
        shares: vec![],
        control_socket: "/d/control.sock".into(),
        hostname: "box".into(),
    }
}

/// 규격 문서에 쓰는 모양 그대로다. 바뀌면 VERSION을 올려야 한다.
#[test]
fn wire_shape_is_stable() {
    let mut out = Vec::new();
    write_line(&mut out, &Request::Kill {}).unwrap();
    write_line(&mut out, &Event::Started { pid: Some(7) }).unwrap();
    write_line(
        &mut out,
        &Event::Exited {
            reason: "guest powered off".into(),
        },
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "{\"type\":\"kill\"}\n{\"type\":\"started\",\"pid\":7}\n{\"type\":\"exited\",\"reason\":\"guest powered off\"}\n"
    );
    let caps: Capabilities = serde_json::from_str(
        r#"{"version":2,"name":"other-vmm","arch":"amd64","rootfs_formats":["erofs"],"live_share":false,"custom_kernel":true,"snapshot":true,"max_disks":2}"#,
    )
    .unwrap();
    assert_eq!(caps.arch, Arch::Amd64);
}

#[test]
fn messages_roundtrip() {
    let mut buf = Vec::new();
    let launch = Request::Launch {
        version: VERSION,
        vm: vm(),
    };
    write_line(&mut buf, &launch).unwrap();
    write_line(&mut buf, &Request::Kill {}).unwrap();
    let mut r = Cursor::new(buf);
    assert_eq!(read_line::<Request>(&mut r).unwrap(), Some(launch));
    assert_eq!(
        read_line::<Request>(&mut r).unwrap(),
        Some(Request::Kill {})
    );
    assert_eq!(read_line::<Request>(&mut r).unwrap(), None);
}

#[test]
fn bad_lines_are_errors() {
    for bad in [
        "{\"type\":\"reboot\"}\n",
        "{\"type\":\"kill\",\"extra\":1}\n",
        "not json\n",
        "{\"type\":\"kill\"}",
    ] {
        let r = read_line::<Request>(&mut Cursor::new(bad.as_bytes()));
        assert_eq!(r.unwrap_err().kind(), io::ErrorKind::InvalidData, "{bad:?}");
    }
    let long = vec![b' '; MAX_LINE + 10];
    assert!(read_line::<Request>(&mut Cursor::new(long)).is_err());
}

#[test]
fn vsock_handshake() {
    // 서버 쪽.
    let mut req = Cursor::new(b"CONNECT 1024\n".to_vec());
    assert_eq!(vsock::accept(&mut req).unwrap(), 1024);
    assert!(vsock::accept(&mut Cursor::new(b"HELLO\n".to_vec())).is_err());
    let mut out = Vec::new();
    vsock::ack(&mut out, 1024).unwrap();
    assert_eq!(out, b"OK 1024\n");

    // 클라이언트 쪽: 쓴 것과 받은 것을 나눠 본다.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }
    impl Read for Duplex {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.input.read(b)
        }
    }
    impl Write for Duplex {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.output.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut d = Duplex {
        input: Cursor::new(b"OK 1073741824\nrest".to_vec()),
        output: Vec::new(),
    };
    vsock::handshake(&mut d, 1024).unwrap();
    assert_eq!(d.output, b"CONNECT 1024\n");
    let mut rest = String::new();
    d.input.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "rest");

    let mut closed = Duplex {
        input: Cursor::new(Vec::new()),
        output: Vec::new(),
    };
    assert_eq!(
        vsock::handshake(&mut closed, 1024).unwrap_err().kind(),
        io::ErrorKind::ConnectionRefused
    );
}

#[test]
fn the_highest_common_version_is_used() {
    assert_eq!(negotiate(VERSION, VERSION), Some(VERSION));
    // 실행기가 더 새 버전까지 말해도 이 sbx의 가장 높은 버전에서 만난다.
    assert_eq!(negotiate(MIN_VERSION, VERSION + 5), Some(VERSION));
    // 겹치지 않으면 없다.
    assert_eq!(negotiate(VERSION + 1, VERSION + 2), None);
    assert_eq!(negotiate(0, MIN_VERSION - 1), None);
    assert_eq!(negotiate(3, 2), None);
    // 범위를 알리는 capabilities 한 줄은 옛 모양으로도 읽힌다(모르는 필드는 무시한다).
    let line = format!(
        r#"{{"version":{},"min_version":{VERSION},"name":"x","arch":"arm64","rootfs_formats":[],"live_share":false,"custom_kernel":false,"snapshot":false,"max_disks":2,"future":1}}"#,
        VERSION + 1
    );
    let h: CapabilitiesHeader = serde_json::from_str(&line).unwrap();
    assert_eq!((h.min_version, h.version), (Some(VERSION), VERSION + 1));
    let c: Capabilities = serde_json::from_str(&line).unwrap();
    assert_eq!(c.min_version, Some(VERSION));
}

#[test]
fn negotiation_matches_its_definition_everywhere() {
    // 정의: 두 범위 [min, max]와 [MIN_VERSION, VERSION]이 겹치면 겹친 곳의 가장 큰 값, 아니면 없음.
    let edges = [
        0,
        1,
        MIN_VERSION - 1,
        MIN_VERSION,
        VERSION,
        VERSION + 1,
        1000,
        u32::MAX - 1,
        u32::MAX,
    ];
    let mut values: Vec<u32> = (0..=VERSION + 3).chain(edges).collect();
    values.sort_unstable();
    values.dedup();
    for &min in &values {
        for &max in &values {
            let want = (min <= max)
                .then(|| (min.max(MIN_VERSION), max.min(VERSION)))
                .filter(|(lo, hi)| lo <= hi)
                .map(|(_, hi)| hi);
            assert_eq!(negotiate(min, max), want, "negotiate({min}, {max})");
            if let Some(v) = want {
                assert!((min..=max).contains(&v) && (MIN_VERSION..=VERSION).contains(&v));
            }
        }
    }
}
