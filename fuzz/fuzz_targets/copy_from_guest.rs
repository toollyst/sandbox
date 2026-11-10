//! `sbx cp` 게스트 → 호스트 (`sbx-runtime`의 `hostfs::receive`, `receive_bytes`). 게스트가 보낸 항목을 호스트
//! 디렉터리에 받는다. 게스트는 아무 경로, 종류, 크기, 권한, 순서나 보낼 수 있다.
//!
//! 매번 빈 작업 공간을 만든다.
//!
//! ```text
//! <run>/outside/secret          대상 밖. 바뀌면 안 된다.
//! <run>/dest/                   대상 디렉터리
//!   escape -> ../outside        밖을 가리키는 링크 (상대)
//!   abs -> <run>/outside        밖을 가리키는 링크 (절대)
//!   u/keep                      이미 있는 디렉터리와 파일
//! ```
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - 결과가 무엇이든 `<run>`에는 `dest`와 `outside`만 있고, `outside`는 그대로다.
//!   (게스트가 링크를 만들고 그 너머로 쓰려 해도, 미리 있던 링크를 지나 쓰려 해도.)
//! - `dest` 아래 어떤 것에도 setuid, setgid, sticky 비트가 없다.
//! - `receive_bytes`가 돌려준 내용은 한도보다 크지 않다.

#![no_main]

use std::fs;
use std::path::Path;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sbx_fuzz::{Item, Name, Scratch, join, snapshot};
use sbx_guest_proto::{Entry, Error, ErrorKind, FileKind, GuestMsg};
use sbx_runtime::RuntimeError;
use sbx_runtime::fuzzing::{Received, receive, receive_bytes};

#[derive(Debug, Arbitrary)]
enum Kind {
    File {
        size: u8,
    },
    Dir,
    Symlink {
        target: Vec<Name>,
        absolute: bool,
    },
    /// 대상 밖을 가리키는 링크.
    SymlinkOut,
}

#[derive(Debug, Arbitrary)]
enum Msg {
    Entry {
        path: Vec<Name>,
        kind: Kind,
        mode: u16,
        mtime: i64,
    },
    Data(Vec<u8>),
    End,
    Error,
    Done,
}

#[derive(Debug, Arbitrary)]
enum To {
    Contents,
    T,
    U,
    Escape,
    Raw(String),
}

#[derive(Debug, Arbitrary)]
struct Input {
    to: To,
    msgs: Vec<Msg>,
    limit: u8,
}

fn guest_msg(m: &Msg, outside: &Path) -> GuestMsg {
    match m {
        Msg::Entry {
            path,
            kind,
            mode,
            mtime,
        } => GuestMsg::Entry(Entry {
            path: join(path, false),
            kind: match kind {
                Kind::File { size } => FileKind::File { size: *size as u64 },
                Kind::Dir => FileKind::Dir,
                Kind::Symlink { target, absolute } => FileKind::Symlink {
                    target: join(target, *absolute),
                },
                Kind::SymlinkOut => FileKind::Symlink {
                    target: outside.to_string_lossy().into_owned(),
                },
            },
            mode: u32::from(*mode) & 0o7777,
            uid: 0,
            gid: 0,
            mtime: *mtime,
        }),
        Msg::Data(d) => GuestMsg::Data(d.clone()),
        Msg::End => GuestMsg::End,
        Msg::Error => GuestMsg::Error(Error::new(ErrorKind::Io, "boom")),
        Msg::Done => GuestMsg::Done,
    }
}

fn feed(msgs: Vec<GuestMsg>) -> impl FnMut() -> Result<GuestMsg, RuntimeError> {
    let mut it = msgs.into_iter();
    // 게스트가 연결을 닫으면 받는 쪽은 오류를 본다.
    move || {
        it.next()
            .ok_or_else(|| RuntimeError::Protocol("connection closed".into()))
    }
}

#[cfg(unix)]
fn special_bits(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::symlink_metadata(p)
        .map(|m| {
            if m.file_type().is_symlink() {
                0
            } else {
                m.permissions().mode() & 0o7000
            }
        })
        .unwrap_or(0)
}

#[cfg(not(unix))]
fn special_bits(_: &Path) -> u32 {
    0
}

fn setup(run: &Path) {
    let outside = run.join("outside");
    let dest = run.join("dest");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"do not touch").unwrap();
    fs::create_dir_all(dest.join("u")).unwrap();
    fs::write(dest.join("u/keep"), b"kept").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("../outside", dest.join("escape")).unwrap();
        std::os::unix::fs::symlink(&outside, dest.join("abs")).unwrap();
    }
}

fn check(run: &Path, before: &std::collections::BTreeMap<std::path::PathBuf, Item>) {
    let outside = run.join("outside");
    assert_eq!(&snapshot(&outside, 1 << 16), before, "outside changed");
    let mut top: Vec<_> = fs::read_dir(run)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    top.sort();
    assert_eq!(
        top,
        ["dest", "outside"],
        "something was written next to dest"
    );
    for (rel, _) in snapshot(&run.join("dest"), 0) {
        let p = run.join("dest").join(&rel);
        assert_eq!(
            special_bits(&p),
            0,
            "{} kept setuid/setgid/sticky",
            rel.display()
        );
    }
}

fuzz_target!(|input: Input| {
    let scratch = Scratch::get("copy_from_guest");
    let to = match &input.to {
        To::Contents => None,
        To::T => Some("t".to_owned()),
        To::U => Some("u".to_owned()),
        To::Escape => Some("escape".to_owned()),
        To::Raw(s) => Some(s.clone()),
    };

    let run = scratch.fresh();
    setup(&run);
    let outside = run.join("outside");
    let before = snapshot(&outside, 1 << 16);
    let msgs: Vec<GuestMsg> = input.msgs.iter().map(|m| guest_msg(m, &outside)).collect();

    let mut next = feed(msgs.clone());
    let _ = receive(&run.join("dest"), "t", to.as_deref(), &mut next);
    check(&run, &before);

    let limit = u64::from(input.limit);
    let mut next = feed(msgs);
    if let Ok(Received::File(buf)) = receive_bytes("t", limit, &mut next) {
        assert!(buf.len() as u64 <= limit);
    }
    check(&run, &before);
});
