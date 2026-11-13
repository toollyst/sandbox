use std::io::Read;

use sbx_guest_proto::{Error, ErrorKind};

use super::*;

fn entry(path: &str, kind: LayerKind) -> LayerEntry {
    LayerEntry {
        path: path.into(),
        kind,
        mode: 0o644,
        uid: 1000,
        gid: 1000,
        mtime: 1_700_000_000,
        xattrs: vec![],
    }
}

fn dir(path: &str) -> GuestMsg {
    GuestMsg::LayerEntry(LayerEntry {
        mode: 0o755,
        ..entry(path, LayerKind::Dir { opaque: false })
    })
}

fn file(path: &str, data: &[u8]) -> Vec<GuestMsg> {
    let mut v = vec![GuestMsg::LayerEntry(entry(
        path,
        LayerKind::File {
            size: data.len() as u64,
        },
    ))];
    if !data.is_empty() {
        v.push(GuestMsg::Data(data.to_vec()));
    }
    v
}

fn feed(msgs: Vec<GuestMsg>) -> impl FnMut() -> Result<GuestMsg, RuntimeError> {
    let mut it = msgs.into_iter();
    move || {
        it.next()
            .ok_or_else(|| RuntimeError::Protocol("connection closed".into()))
    }
}

fn tar_of(mut msgs: Vec<GuestMsg>, limit: u64) -> Result<(Vec<u8>, LayerStats), RuntimeError> {
    msgs.push(GuestMsg::End);
    let mut out = Vec::new();
    let stats = receive(&mut out, limit, &mut feed(msgs))?;
    Ok((out, stats))
}

/// (경로, 종류, 링크 대상, 내용, xattr)
type Row = (
    String,
    tar::EntryType,
    Option<String>,
    Vec<u8>,
    Vec<(String, Vec<u8>)>,
);

fn read_tar(b: &[u8]) -> Vec<Row> {
    let mut a = tar::Archive::new(b);
    let mut rows = Vec::new();
    for e in a.entries().unwrap() {
        let mut e = e.unwrap();
        let path = String::from_utf8(e.path_bytes().into_owned()).unwrap();
        let ty = e.header().entry_type();
        let link = e
            .link_name_bytes()
            .map(|l| String::from_utf8(l.into_owned()).unwrap());
        let mut xattrs = Vec::new();
        if let Some(exts) = e.pax_extensions().unwrap() {
            for x in exts {
                let x = x.unwrap();
                if let Some(n) = x.key().unwrap().strip_prefix("SCHILY.xattr.") {
                    xattrs.push((n.to_owned(), x.value_bytes().to_vec()));
                }
            }
        }
        let mut data = Vec::new();
        e.read_to_end(&mut data).unwrap();
        rows.push((path, ty, link, data, xattrs));
    }
    rows
}

#[test]
fn an_empty_layer_is_two_zero_blocks() {
    let (b, stats) = tar_of(vec![], 0).unwrap();
    assert_eq!(b, vec![0; 1024]);
    assert_eq!(
        stats,
        LayerStats {
            entries: 0,
            content_bytes: 0,
            tar_bytes: 1024
        }
    );
}

#[test]
fn every_kind_becomes_the_oci_tar_entry() {
    let long = format!("d/{}", "n".repeat(150));
    let mut msgs = vec![dir("d")];
    msgs.extend(file("d/f", b"hello"));
    msgs.extend(file(&long, b""));
    msgs.push(GuestMsg::LayerEntry(LayerEntry {
        xattrs: vec![
            ("security.capability".into(), vec![1, 0, 0, 2]),
            ("user.k".into(), b"v".to_vec()),
        ],
        ..entry("d/cap", LayerKind::File { size: 1 })
    }));
    msgs.push(GuestMsg::Data(b"x".to_vec()));
    msgs.push(GuestMsg::LayerEntry(entry(
        "d/h",
        LayerKind::Hardlink {
            target: "d/f".into(),
        },
    )));
    msgs.push(GuestMsg::LayerEntry(entry(
        "d/s",
        LayerKind::Symlink {
            target: format!("/{}", "t".repeat(120)),
        },
    )));
    msgs.push(GuestMsg::LayerEntry(entry(
        "d/c",
        LayerKind::CharDevice { major: 1, minor: 3 },
    )));
    msgs.push(GuestMsg::LayerEntry(entry(
        "d/b",
        LayerKind::BlockDevice { major: 8, minor: 1 },
    )));
    msgs.push(GuestMsg::LayerEntry(entry("d/p", LayerKind::Fifo)));
    msgs.push(GuestMsg::LayerEntry(entry("d/gone", LayerKind::Whiteout)));
    msgs.push(GuestMsg::LayerEntry(entry("top-gone", LayerKind::Whiteout)));
    msgs.push(GuestMsg::LayerEntry(LayerEntry {
        mode: 0o700,
        ..entry("o", LayerKind::Dir { opaque: true })
    }));
    let (b, stats) = tar_of(msgs, 100).unwrap();
    assert_eq!((stats.entries, stats.content_bytes), (12, 6));
    assert_eq!(stats.tar_bytes, b.len() as u64);
    assert_eq!(b.len() % 512, 0);
    let rows = read_tar(&b);
    use tar::EntryType as T;
    let got: Vec<(&str, T, Option<&str>, &[u8])> = rows
        .iter()
        .map(|r| (r.0.as_str(), r.1, r.2.as_deref(), r.3.as_slice()))
        .collect();
    let target = format!("/{}", "t".repeat(120));
    assert_eq!(
        got,
        vec![
            ("d/", T::Directory, None, &b""[..]),
            ("d/f", T::Regular, None, b"hello"),
            (long.as_str(), T::Regular, None, b""),
            ("d/cap", T::Regular, None, b"x"),
            ("d/h", T::Link, Some("d/f"), b""),
            ("d/s", T::Symlink, Some(target.as_str()), b""),
            ("d/c", T::Char, None, b""),
            ("d/b", T::Block, None, b""),
            ("d/p", T::Fifo, None, b""),
            ("d/.wh.gone", T::Regular, None, b""),
            (".wh.top-gone", T::Regular, None, b""),
            ("o/", T::Directory, None, b""),
            ("o/.wh..wh..opq", T::Regular, None, b""),
        ]
    );
    assert_eq!(
        rows[3].4,
        vec![
            ("security.capability".into(), vec![1, 0, 0, 2]),
            ("user.k".into(), b"v".to_vec())
        ]
    );
    let mut a = tar::Archive::new(b.as_slice());
    let headers: Vec<tar::Header> = a
        .entries()
        .unwrap()
        .map(|e| e.unwrap().header().clone())
        .collect();
    assert_eq!(headers[0].mode().unwrap(), 0o755);
    assert_eq!(headers[1].uid().unwrap(), 1000);
    assert_eq!(headers[1].mtime().unwrap(), 1_700_000_000);
    assert_eq!(headers[6].device_major().unwrap(), Some(1));
    assert_eq!(headers[6].device_minor().unwrap(), Some(3));
    assert_eq!(headers[11].mode().unwrap(), 0o700);
}

#[test]
fn large_ids_and_odd_times_still_read_back() {
    let msgs = vec![GuestMsg::LayerEntry(LayerEntry {
        uid: u32::MAX,
        gid: 3_000_000,
        mtime: -5,
        ..entry("f", LayerKind::File { size: 0 })
    })];
    let (b, _) = tar_of(msgs, 0).unwrap();
    let mut a = tar::Archive::new(b.as_slice());
    let e = a.entries().unwrap().next().unwrap().unwrap();
    assert_eq!(e.header().uid().unwrap(), u64::from(u32::MAX));
    assert_eq!(e.header().gid().unwrap(), 3_000_000);
    // 음수 시각은 0으로 자른다.
    assert_eq!(e.header().mtime().unwrap(), 0);
}

#[test]
fn the_same_entries_give_the_same_bytes() {
    let make = || {
        let mut m = vec![dir("a")];
        m.extend(file("a/x", b"1234"));
        m.push(GuestMsg::LayerEntry(entry("a/y", LayerKind::Whiteout)));
        tar_of(m, 10).unwrap().0
    };
    assert_eq!(make(), make());
}

fn rejected(msgs: Vec<GuestMsg>) -> RuntimeError {
    tar_of(msgs, 1 << 20).unwrap_err()
}

fn is_protocol(e: &RuntimeError) -> bool {
    matches!(e, RuntimeError::Protocol(_))
}

#[test]
fn hostile_paths_are_rejected() {
    for p in ["/abs", "a/../b", "..", "a//b", "", "./a", "a/"] {
        let e = rejected(file(p, b""));
        assert!(is_protocol(&e), "{p:?}: {e:?}");
    }
    // OCI의 지우기 표시와 같은 이름.
    let e = rejected(file(".wh.x", b""));
    assert!(e.to_string().contains(".wh."), "{e}");
    let mut m = vec![dir("d")];
    m.extend(file("d/.wh..wh..opq", b""));
    assert!(is_protocol(&rejected(m)));
    // 너무 긴 이름과 경로.
    assert!(is_protocol(&rejected(file(&"n".repeat(256), b""))));
    let deep = vec!["d"; 2100].join("/");
    assert!(is_protocol(&rejected(file(&deep, b""))));
}

#[test]
fn structure_is_checked() {
    // 같은 경로 두 번.
    let mut m = file("a", b"");
    m.extend(file("a", b""));
    assert!(rejected(m).to_string().contains("twice"));
    // 부모가 앞서 오지 않았다.
    assert!(rejected(file("p/c", b"")).to_string().contains("parent"));
    // 부모가 디렉터리가 아니다.
    let mut m = file("p", b"");
    m.extend(file("p/c", b""));
    assert!(rejected(m).to_string().contains("parent"));
    // whiteout 아래.
    let m = vec![
        GuestMsg::LayerEntry(entry("w", LayerKind::Whiteout)),
        GuestMsg::LayerEntry(entry("w/x", LayerKind::Whiteout)),
    ];
    assert!(is_protocol(&rejected(m)));
    // 하드 링크: 없는 대상, 디렉터리 대상, 뒤에 오는 대상, 자기 자신.
    let link = |p: &str, t: &str| {
        GuestMsg::LayerEntry(entry(
            p,
            LayerKind::Hardlink {
                target: t.to_owned(),
            },
        ))
    };
    assert!(is_protocol(&rejected(vec![link("l", "nope")])));
    assert!(is_protocol(&rejected(vec![dir("d"), link("l", "d")])));
    let mut m = vec![link("l", "z")];
    m.extend(file("z", b""));
    assert!(is_protocol(&rejected(m)));
    assert!(is_protocol(&rejected(vec![link("l", "l")])));
    let m = vec![
        GuestMsg::LayerEntry(entry(
            "s",
            LayerKind::Symlink {
                target: "/x".into(),
            },
        )),
        link("l", "s"),
    ];
    assert!(is_protocol(&rejected(m)));
    // 빈 링크 대상, NUL.
    for t in ["", "a\0b"] {
        let m = vec![GuestMsg::LayerEntry(entry(
            "s",
            LayerKind::Symlink { target: t.into() },
        ))];
        assert!(is_protocol(&rejected(m)), "{t:?}");
    }
    // 장치 번호가 tar 필드를 넘는다.
    let m = vec![GuestMsg::LayerEntry(entry(
        "c",
        LayerKind::CharDevice {
            major: 1 << 21,
            minor: 0,
        },
    ))];
    assert!(is_protocol(&rejected(m)));
}

#[test]
fn xattr_names_and_sizes_are_checked() {
    for name in [
        "",
        "trusted.overlay.opaque",
        "user.a=b",
        "user.a\nb",
        "user.\0",
    ] {
        let m = vec![GuestMsg::LayerEntry(LayerEntry {
            xattrs: vec![(name.into(), b"y".to_vec())],
            ..entry("f", LayerKind::Fifo)
        })];
        assert!(is_protocol(&rejected(m)), "{name:?}");
    }
    let bad = |xattrs: Vec<(String, Vec<u8>)>| {
        rejected(vec![GuestMsg::LayerEntry(LayerEntry {
            xattrs,
            ..entry("f", LayerKind::Fifo)
        })])
    };
    assert!(is_protocol(&bad(vec![(
        "user.big".into(),
        vec![0; MAX_XATTR_VALUE + 1]
    )])));
    // 같은 이름 두 번, 값의 줄바꿈(퍼징이 찾았다: 관리 쪽의 레이어 읽기가 그 PAX 기록을 읽지 못한다).
    assert!(is_protocol(&bad(vec![
        ("user.a".into(), b"1".to_vec()),
        ("user.a".into(), b"2".to_vec())
    ])));
    assert!(is_protocol(&bad(vec![(
        "user.a".into(),
        vec![0, 255, 255, 255, 10]
    )])));
    // 바이너리 값(NUL, 0xff)은 된다.
    let m = vec![GuestMsg::LayerEntry(LayerEntry {
        xattrs: vec![("user.bin".into(), vec![0, 59, 255])],
        ..entry("f", LayerKind::Fifo)
    })];
    let (b, _) = tar_of(m, 0).unwrap();
    assert_eq!(
        read_tar(&b)[0].4,
        vec![("user.bin".into(), vec![0, 59, 255])]
    );
}

#[test]
fn contents_are_limited_and_exact() {
    // 알린 크기가 한도를 넘으면 내용을 받기 전에 멈춘다.
    let mut asked = 0;
    let mut msgs = file("big", b"0123456789").into_iter();
    let mut next = || {
        asked += 1;
        msgs.next()
            .ok_or_else(|| RuntimeError::Protocol("closed".into()))
    };
    let e = receive(&mut Vec::new(), 9, &mut next).unwrap_err();
    assert!(matches!(e, RuntimeError::Invalid(_)), "{e:?}");
    assert_eq!(asked, 1);
    // 한도는 층 전체의 합계다.
    let mut m = file("a", b"12345");
    m.extend(file("b", b"12345"));
    assert!(matches!(tar_of(m, 9), Err(RuntimeError::Invalid(_))));
    // 알린 것보다 많은 내용.
    let m = vec![
        GuestMsg::LayerEntry(entry("f", LayerKind::File { size: 2 })),
        GuestMsg::Data(b"123".to_vec()),
    ];
    assert!(is_protocol(&rejected(m)));
    // 내용이 다 오기 전에 다음 항목이나 끝.
    let m = vec![
        GuestMsg::LayerEntry(entry("f", LayerKind::File { size: 2 })),
        GuestMsg::Data(b"1".to_vec()),
    ];
    assert!(is_protocol(&rejected(m)));
    let mut m = vec![GuestMsg::LayerEntry(entry(
        "f",
        LayerKind::File { size: 2 },
    ))];
    m.extend(file("g", b""));
    assert!(is_protocol(&rejected(m)));
    // 일반 파일이 아닌 항목 뒤의 내용.
    let m = vec![dir("d"), GuestMsg::Data(b"x".to_vec())];
    assert!(is_protocol(&rejected(m)));
}

#[test]
fn guest_errors_and_stray_messages_end_the_layer() {
    let m = vec![
        dir("d"),
        GuestMsg::Error(Error::new(ErrorKind::Io, "disk on fire")),
    ];
    assert!(matches!(rejected(m), RuntimeError::Guest(_)));
    let m = vec![GuestMsg::Done];
    assert!(is_protocol(&rejected(m)));
    // 끝없이 끝나지 않는 연결은 끊긴 것으로 끝난다(feed가 닫는다).
    let mut out = Vec::new();
    assert!(receive(&mut out, 10, &mut feed(vec![dir("d")])).is_err());
}
