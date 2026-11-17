//! 빌드 층 게스트 → 호스트 (`sbx-runtime`의 `layer::receive`). 게스트가 보낸 층 항목을 OCI 층 tar로 쓴다.
//! 게스트는 아무 경로, 종류, 크기, 링크 대상, xattr, 순서나 보낼 수 있다.
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - 받아들였으면 출력은 512바이트 블록으로 끝나고, 크기 보고가 맞고, 파일 내용 합계가 한도 안이다.
//! - 받아들인 tar는 `prepare`의 길(`sbx-manage`의 레이어 읽기, `sbx-rootfs`의 병합)을 오류 없이 지난다.
//!   하드 링크가 없는 대상을 가리키거나, 디렉터리가 아닌 것 아래에 무엇이 오는 층은 받지 않는다는 뜻이다.
//! - tar 안의 `.wh.` 이름은 게스트가 종류로 보낸 whiteout과 opaque 디렉터리뿐이다(사용자 이름이 지우기가 되지 않는다).
//! - tar 안의 이름은 게스트가 보낸 경로(와 whiteout·opaque 표시)이고, `tar` 크레이트(PAX 기록을 줄바꿈으로 나누어
//!   읽는다)와 sbx의 레이어 읽기(길이로 나누어 읽는다)가 같게 읽는다. 이름에는 줄바꿈이 올 수 있다.

#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sbx_fuzz::{Name, join};
use sbx_guest_proto::{Error, ErrorKind, GuestMsg, LayerEntry, LayerKind};
use sbx_rootfs::MergedTree;
use sbx_runtime::RuntimeError;
use sbx_runtime::layer::receive;

#[derive(Debug, Arbitrary)]
enum Kind {
    File { size: u8 },
    Dir { opaque: bool },
    Symlink { target: Vec<Name>, absolute: bool },
    Hardlink { target: Vec<Name> },
    Char { major: u32, minor: u32 },
    Fifo,
    Whiteout,
}

#[derive(Debug, Arbitrary)]
enum Msg {
    Entry {
        path: Vec<Name>,
        kind: Kind,
        mode: u16,
        uid: u32,
        mtime: i64,
        xattrs: Vec<(Name, Vec<u8>)>,
    },
    Data(Vec<u8>),
    End,
    Error,
    Done,
}

#[derive(Debug, Arbitrary)]
struct Input {
    msgs: Vec<Msg>,
    limit: u16,
}

fn guest_msg(m: &Msg) -> GuestMsg {
    match m {
        Msg::Entry {
            path,
            kind,
            mode,
            uid,
            mtime,
            xattrs,
        } => GuestMsg::LayerEntry(LayerEntry {
            path: join(path, false),
            kind: match kind {
                Kind::File { size } => LayerKind::File {
                    size: u64::from(*size),
                },
                Kind::Dir { opaque } => LayerKind::Dir { opaque: *opaque },
                Kind::Symlink { target, absolute } => LayerKind::Symlink {
                    target: join(target, *absolute),
                },
                Kind::Hardlink { target } => LayerKind::Hardlink {
                    target: join(target, false),
                },
                Kind::Char { major, minor } => LayerKind::CharDevice {
                    major: *major,
                    minor: *minor,
                },
                Kind::Fifo => LayerKind::Fifo,
                Kind::Whiteout => LayerKind::Whiteout,
            },
            mode: u32::from(*mode) & 0o7777,
            uid: *uid,
            gid: 0,
            mtime: *mtime,
            xattrs: xattrs
                .iter()
                .map(|(n, v)| (format!("user.{}", n.render()), v.clone()))
                .collect(),
        }),
        Msg::Data(d) => GuestMsg::Data(d.clone()),
        Msg::End => GuestMsg::End,
        Msg::Error => GuestMsg::Error(Error::new(ErrorKind::Io, "boom")),
        Msg::Done => GuestMsg::Done,
    }
}

fuzz_target!(|input: Input| {
    let msgs: Vec<GuestMsg> = input.msgs.iter().map(guest_msg).collect();
    // 받아들였다면 첫 End까지가 층이다(내용 자리의 End는 오류다).
    let markers = msgs
        .iter()
        .take_while(|m| !matches!(m, GuestMsg::End))
        .filter(|m| {
            matches!(
                m,
                GuestMsg::LayerEntry(LayerEntry {
                    kind: LayerKind::Whiteout | LayerKind::Dir { opaque: true },
                    ..
                })
            )
        })
        .count();
    // 받아들였다면 tar의 이름은 이것이다(모듈 문서의 tar 형식).
    let expected: Vec<String> = msgs
        .iter()
        .take_while(|m| !matches!(m, GuestMsg::End))
        .filter_map(|m| match m {
            GuestMsg::LayerEntry(e) => Some(e),
            _ => None,
        })
        .flat_map(|e| match e.kind {
            LayerKind::Dir { opaque } => {
                let mut v = vec![format!("{}/", e.path)];
                if opaque {
                    v.push(format!("{}/.wh..wh..opq", e.path));
                }
                v
            }
            LayerKind::Whiteout => vec![match e.path.rsplit_once('/') {
                Some((dir, name)) => format!("{dir}/.wh.{name}"),
                None => format!(".wh.{}", e.path),
            }],
            _ => vec![e.path.clone()],
        })
        .collect();
    let mut it = msgs.into_iter();
    let mut next = || {
        it.next()
            .ok_or_else(|| RuntimeError::Protocol("connection closed".into()))
    };
    let limit = u64::from(input.limit);
    let mut out = Vec::new();
    let Ok(stats) = receive(&mut out, limit, &mut next) else {
        return;
    };
    assert_eq!(stats.tar_bytes, out.len() as u64);
    assert_eq!(out.len() % 512, 0);
    assert!(stats.content_bytes <= limit);

    let names: Vec<String> = tar::Archive::new(out.as_slice())
        .entries()
        .unwrap()
        .map(|e| String::from_utf8(e.unwrap().path_bytes().into_owned()).unwrap())
        .collect();
    let wh = names
        .iter()
        .filter(|n| {
            n.trim_end_matches('/')
                .rsplit('/')
                .next()
                .is_some_and(|last| last.starts_with(".wh."))
        })
        .count();
    assert_eq!(wh, markers, "{names:?}");
    // `tar` 크레이트(PAX 기록을 줄바꿈으로 나눈다)도 보낸 이름 그대로 읽는다.
    assert_eq!(names, expected);

    let spool = std::env::temp_dir().join(format!("sbx-fuzz-layer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&spool);
    std::fs::create_dir_all(&spool).unwrap();
    let entries = sbx_manage::fuzzing::read_layer(0, out.as_slice(), &spool)
        .expect("an accepted layer reads back");
    // sbx의 레이어 읽기(PAX 기록을 길이로 나눈다)도 같은 이름을 본다.
    let read: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(read, expected);
    MergedTree::new()
        .apply_layer(entries)
        .expect("an accepted layer merges");
    let _ = std::fs::remove_dir_all(&spool);
});
