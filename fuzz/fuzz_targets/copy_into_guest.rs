//! `sbx cp` 호스트 → 게스트, 게스트 쪽(`sbx-guest`의 `copy::receive`). 호스트가 보낸 항목을 게스트 디렉터리에
//! 받는다. 게스트 init은 VM 안에서 root로 돌지만, 받는 규칙(대상 밖에 쓰지 않는다)은 같아야 한다: 그 규칙이
//! 깨지면 `cp`가 이미지의 파일을 대상 밖에서 덮어쓴다.
//!
//! 매번 빈 작업 공간을 만든다(`copy_from_guest`와 같은 모양).
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
//! - 받는 쪽은 `Done`이나 `Error` 하나로 답한다.

#![no_main]

use std::fs;
use std::io::Cursor;
use std::path::Path;
use std::sync::{Arc, Mutex};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sbx_fuzz::{Name, Scratch, join, snapshot};
use sbx_guest_proto::{Entry, FileKind, GuestMsg, HostMsg};

#[derive(Debug, Arbitrary)]
enum Kind {
    File { size: u8 },
    Dir,
    Symlink { target: Vec<Name>, absolute: bool },
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
    /// 받기 중에 오면 안 되는 메시지.
    Stray,
}

/// 대상 디렉터리. 사용자가 `sbx cp`에 적은 경로라 링크여도 따라간다(`cp`, Docker와 같다). 그래서 미리 놓은
/// 링크(`escape`, `abs`)는 대상으로 고르지 않는다: 지키는 것은 항목이 대상 밖으로 나가지 않는 것이다.
#[derive(Debug, Arbitrary)]
enum Into {
    Dest,
    /// 이미 있는 하위 디렉터리.
    U,
    /// 없는 디렉터리(오류여야 한다).
    Missing,
}

#[derive(Debug, Arbitrary)]
struct Input {
    into: Into,
    msgs: Vec<Msg>,
}

/// 받는 쪽의 답을 모은다.
#[derive(Clone, Default)]
struct Sink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn host_msg(m: &Msg, outside: &Path) -> HostMsg {
    match m {
        Msg::Entry {
            path,
            kind,
            mode,
            mtime,
        } => HostMsg::Entry(Entry {
            path: join(path, false),
            kind: match kind {
                Kind::File { size } => FileKind::File {
                    size: u64::from(*size),
                },
                Kind::Dir => FileKind::Dir,
                Kind::Symlink { target, absolute } => FileKind::Symlink {
                    target: join(target, *absolute),
                },
                Kind::SymlinkOut => FileKind::Symlink {
                    target: outside.to_string_lossy().into_owned(),
                },
            },
            mode: u32::from(*mode) & 0o7777,
            // 퍼저는 root가 아니다: 소유자는 바뀌지 않는다(받는 쪽이 root일 때만 그대로 둔다).
            uid: 0,
            gid: 0,
            mtime: *mtime,
        }),
        Msg::Data(d) => HostMsg::Data(d.clone()),
        Msg::End => HostMsg::End,
        Msg::Stray => HostMsg::Shutdown,
    }
}

fn setup(run: &Path) {
    let outside = run.join("outside");
    let dest = run.join("dest");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"do not touch").unwrap();
    fs::create_dir_all(dest.join("u")).unwrap();
    fs::write(dest.join("u/keep"), b"kept").unwrap();
    std::os::unix::fs::symlink("../outside", dest.join("escape")).unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("abs")).unwrap();
}

fuzz_target!(|input: Input| {
    let scratch = Scratch::get("copy_into_guest");
    let run = scratch.fresh();
    setup(&run);
    let outside = run.join("outside");
    let before = snapshot(&outside, 1 << 16);

    let mut wire = Vec::new();
    for m in &input.msgs {
        host_msg(m, &outside).write_to(&mut wire).unwrap();
    }
    let dest = match input.into {
        Into::Dest => run.join("dest"),
        Into::U => run.join("dest/u"),
        Into::Missing => run.join("dest/missing"),
    };
    let sink = Sink::default();
    sbx_guest::copy::receive(
        &dest.to_string_lossy(),
        &mut Cursor::new(wire),
        &sbx_guest::wire::Out::new(sink.clone()),
    );

    // 답은 하나다.
    let reply = sink.0.lock().unwrap().clone();
    let mut r = Cursor::new(reply);
    let first = GuestMsg::read_from(&mut r).expect("one reply");
    assert!(
        matches!(first, GuestMsg::Done | GuestMsg::Error(_)),
        "{first:?}"
    );
    assert!(GuestMsg::read_from(&mut r).is_err(), "more than one reply");

    assert_eq!(snapshot(&outside, 1 << 16), before, "outside changed");
    let mut top: Vec<_> = fs::read_dir(&run)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    top.sort();
    assert_eq!(
        top,
        ["dest", "outside"],
        "something was written next to dest"
    );
});
