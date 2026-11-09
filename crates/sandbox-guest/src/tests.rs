//! 게스트 단독 테스트. 실제 [`server::serve`]를 Unix 소켓 쌍 위에서 돌린다.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use sandbox_proto::{
    Entry, ErrorKind, Exec, ExitStatus, FileKind, GuestMsg, HostMsg, Ident, PROTOCOL_VERSION, User,
};

use crate::server::{self, Ctx};

struct Fixture {
    dir: PathBuf,
    uid: u32,
    gid: u32,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sandbox-guest-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let m = fs::metadata(&dir).unwrap();
        Self {
            dir,
            uid: m.uid(),
            gid: m.gid(),
        }
    }

    /// 서버를 띄우고 Hello까지 마친 호스트 쪽 끝.
    fn connect(&self) -> UnixStream {
        let (host, guest) = UnixStream::pair().unwrap();
        let root = self.dir.clone();
        thread::spawn(move || {
            server::serve(
                guest,
                &Ctx {
                    root,
                    on_shutdown: Box::new(|| {}),
                },
            )
        });
        let mut host = host;
        HostMsg::Hello {
            version: PROTOCOL_VERSION,
        }
        .write_to(&mut host)
        .unwrap();
        assert_eq!(
            GuestMsg::read_from(&mut host).unwrap(),
            GuestMsg::HelloAck {
                version: PROTOCOL_VERSION
            }
        );
        host
    }

    fn exec(&self, args: &[&str], stdin: bool) -> Exec {
        Exec {
            args: args.iter().map(|s| s.to_string()).collect(),
            env: vec![("PATH".into(), "/usr/bin:/bin".into())],
            cwd: "/".into(),
            user: User {
                user: Ident::Id(self.uid),
                group: Some(Ident::Id(self.gid)),
            },
            tty: false,
            stdin,
            size: None,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn send(c: &mut UnixStream, m: HostMsg) {
    m.write_to(c).unwrap();
}

fn recv(c: &mut UnixStream) -> GuestMsg {
    GuestMsg::read_from(c).unwrap()
}

/// Exited까지 읽는다. (stdout, stderr, 상태)
fn collect(c: &mut UnixStream) -> (String, String, ExitStatus) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        match recv(c) {
            GuestMsg::Stdout(b) => out.extend(b),
            GuestMsg::Stderr(b) => err.extend(b),
            GuestMsg::Exited(s) => {
                return (
                    String::from_utf8(out).unwrap(),
                    String::from_utf8(err).unwrap(),
                    s,
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn started(c: &mut UnixStream) -> u32 {
    match recv(c) {
        GuestMsg::Started { pid } => pid,
        other => panic!("expected Started, got {other:?}"),
    }
}

fn error_kind(c: &mut UnixStream) -> ErrorKind {
    match recv(c) {
        GuestMsg::Error(e) => e.kind,
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn version_mismatch_is_reported() {
    let (mut host, guest) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        server::serve(
            guest,
            &Ctx {
                root: "/".into(),
                on_shutdown: Box::new(|| {}),
            },
        )
    });
    send(&mut host, HostMsg::Hello { version: 999 });
    assert_eq!(error_kind(&mut host), ErrorKind::Version);
}

#[test]
fn request_before_hello_is_a_protocol_error() {
    let (mut host, guest) = UnixStream::pair().unwrap();
    thread::spawn(move || {
        server::serve(
            guest,
            &Ctx {
                root: "/".into(),
                on_shutdown: Box::new(|| {}),
            },
        )
    });
    send(&mut host, HostMsg::Shutdown);
    assert_eq!(error_kind(&mut host), ErrorKind::Protocol);
}

#[test]
fn exec_streams_output_and_exit_code() {
    let f = Fixture::new("exec");
    let mut c = f.connect();
    send(
        &mut c,
        HostMsg::Exec(f.exec(&["sh", "-c", "echo hi; echo err >&2; exit 3"], false)),
    );
    started(&mut c);
    assert_eq!(
        collect(&mut c),
        ("hi\n".into(), "err\n".into(), ExitStatus::Code(3))
    );
}

#[test]
fn exec_environment_is_exactly_what_was_sent_plus_home() {
    let f = Fixture::new("env");
    let mut c = f.connect();
    let mut x = f.exec(
        &["sh", "-c", "echo \"$HOME|$FOO|$PWD\"; env | wc -l"],
        false,
    );
    x.env.push(("FOO".into(), "x y".into()));
    let dir = fs::canonicalize(&f.dir).unwrap();
    x.cwd = dir.to_str().unwrap().into();
    send(&mut c, HostMsg::Exec(x));
    started(&mut c);
    let (out, _, status) = collect(&mut c);
    let mut lines = out.lines();
    // passwd에 없는 숫자 사용자이므로 홈은 /. 환경은 PATH, FOO, HOME과 셸이 넣는 PWD 정도뿐이다.
    assert_eq!(lines.next().unwrap(), format!("/|x y|{}", dir.display()));
    let count: usize = lines.next().unwrap().trim().parse().unwrap();
    assert!(count <= 6, "unexpected inherited environment: {count} vars");
    assert_eq!(status, ExitStatus::Code(0));
}

#[test]
fn exec_stdin_is_forwarded_and_closed() {
    let f = Fixture::new("stdin");
    let mut c = f.connect();
    send(&mut c, HostMsg::Exec(f.exec(&["cat"], true)));
    started(&mut c);
    send(&mut c, HostMsg::Stdin(b"abc".to_vec()));
    send(&mut c, HostMsg::Stdin(b"def".to_vec()));
    send(&mut c, HostMsg::StdinClose);
    assert_eq!(
        collect(&mut c),
        ("abcdef".into(), "".into(), ExitStatus::Code(0))
    );
}

#[test]
fn exec_without_stdin_reads_dev_null() {
    let f = Fixture::new("nostdin");
    let mut c = f.connect();
    send(&mut c, HostMsg::Exec(f.exec(&["cat"], false)));
    started(&mut c);
    assert_eq!(collect(&mut c).2, ExitStatus::Code(0));
}

#[test]
fn exec_failures_have_distinct_kinds() {
    let f = Fixture::new("fail");
    let mut c = f.connect();
    send(
        &mut c,
        HostMsg::Exec(f.exec(&["no-such-binary-xyz"], false)),
    );
    assert_eq!(error_kind(&mut c), ErrorKind::NotFound);

    let mut c = f.connect();
    let mut x = f.exec(&["sh"], false);
    x.cwd = "/no/such/dir".into();
    send(&mut c, HostMsg::Exec(x));
    assert_eq!(error_kind(&mut c), ErrorKind::InvalidArgument);

    let script = f.dir.join("noexec.sh");
    fs::write(&script, "#!/bin/sh\n").unwrap();
    let mut c = f.connect();
    send(
        &mut c,
        HostMsg::Exec(f.exec(&[script.to_str().unwrap()], false)),
    );
    assert_eq!(error_kind(&mut c), ErrorKind::PermissionDenied);

    let mut c = f.connect();
    let mut x = f.exec(&["sh"], false);
    x.user.user = Ident::Name("ghost".into());
    send(&mut c, HostMsg::Exec(x));
    assert_eq!(error_kind(&mut c), ErrorKind::InvalidArgument);

    let mut c = f.connect();
    send(&mut c, HostMsg::Exec(f.exec(&[], false)));
    assert_eq!(error_kind(&mut c), ErrorKind::InvalidArgument);
}

#[test]
fn exec_signal_is_delivered() {
    let f = Fixture::new("signal");
    let mut c = f.connect();
    send(&mut c, HostMsg::Exec(f.exec(&["sleep", "30"], false)));
    started(&mut c);
    send(&mut c, HostMsg::Signal(15));
    assert_eq!(collect(&mut c).2, ExitStatus::Signal(15));
}

#[test]
fn host_disconnect_kills_the_process() {
    let f = Fixture::new("disconnect");
    let mut c = f.connect();
    send(&mut c, HostMsg::Exec(f.exec(&["sleep", "30"], false)));
    let pid = started(&mut c);
    drop(c);
    let deadline = Instant::now() + Duration::from_secs(10);
    while crate::sys::kill(pid, 0).is_ok() {
        assert!(Instant::now() < deadline, "process {pid} still alive");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn shutdown_syncs_replies_then_calls_back() {
    let (mut host, guest) = UnixStream::pair().unwrap();
    let called = Arc::new(AtomicBool::new(false));
    let flag = called.clone();
    let t = thread::spawn(move || {
        server::serve(
            guest,
            &Ctx {
                root: "/".into(),
                on_shutdown: Box::new(move || flag.store(true, Ordering::SeqCst)),
            },
        )
    });
    send(
        &mut host,
        HostMsg::Hello {
            version: PROTOCOL_VERSION,
        },
    );
    recv(&mut host);
    send(&mut host, HostMsg::Shutdown);
    assert_eq!(recv(&mut host), GuestMsg::Done);
    t.join().unwrap();
    assert!(called.load(Ordering::SeqCst));
}

// --- 파일 ---

/// 경로 → (종류, 권한, 내용 또는 링크 대상). 수정 시각도 비교한다.
fn snapshot(root: &Path) -> Vec<(String, String, u32, String, i64)> {
    let mut out = Vec::new();
    fn walk(root: &Path, p: &Path, out: &mut Vec<(String, String, u32, String, i64)>) {
        let m = fs::symlink_metadata(p).unwrap();
        let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
        let (kind, body) = if m.is_symlink() {
            (
                "link",
                fs::read_link(p).unwrap().to_string_lossy().into_owned(),
            )
        } else if m.is_dir() {
            ("dir", String::new())
        } else {
            ("file", String::from_utf8(fs::read(p).unwrap()).unwrap())
        };
        let mtime = if m.is_symlink() { 0 } else { m.mtime() };
        out.push((rel, kind.into(), m.mode() & 0o7777, body, mtime));
        if m.is_dir() {
            let mut names: Vec<_> = fs::read_dir(p)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            names.sort();
            for n in names {
                walk(root, &n, out);
            }
        }
    }
    walk(root, root, &mut out);
    out
}

fn copy_out(f: &Fixture, path: &Path) -> Vec<GuestMsg> {
    let mut c = f.connect();
    send(
        &mut c,
        HostMsg::CopyOut {
            path: path.to_str().unwrap().into(),
        },
    );
    let mut msgs = Vec::new();
    loop {
        match recv(&mut c) {
            GuestMsg::End => return msgs,
            m @ (GuestMsg::Entry(_) | GuestMsg::Data(_)) => msgs.push(m),
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn copy_in(f: &Fixture, dest: &Path, msgs: Vec<HostMsg>) -> GuestMsg {
    let mut c = f.connect();
    send(
        &mut c,
        HostMsg::CopyIn {
            dest: dest.to_str().unwrap().into(),
        },
    );
    for m in msgs {
        send(&mut c, m);
    }
    send(&mut c, HostMsg::End);
    recv(&mut c)
}

fn to_host(msgs: Vec<GuestMsg>) -> Vec<HostMsg> {
    msgs.into_iter()
        .map(|m| match m {
            GuestMsg::Entry(e) => HostMsg::Entry(e),
            GuestMsg::Data(d) => HostMsg::Data(d),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn copy_out_then_in_preserves_tree() {
    let f = Fixture::new("copy");
    let src = f.dir.join("src/tree");
    fs::create_dir_all(src.join("sub/deeper")).unwrap();
    fs::write(src.join("a.txt"), "hello").unwrap();
    fs::set_permissions(src.join("a.txt"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(src.join("sub/b.sh"), "#!/bin/sh\n").unwrap();
    fs::set_permissions(src.join("sub/b.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(src.join("sub/deeper/empty"), "").unwrap();
    symlink("../a.txt", src.join("sub/link")).unwrap();
    // 큰 파일은 여러 Data 조각으로 간다.
    fs::write(
        src.join("big"),
        "x".repeat(sandbox_proto::CHUNK_LEN * 2 + 7),
    )
    .unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o750)).unwrap();

    let msgs = copy_out(&f, &src);
    match &msgs[0] {
        GuestMsg::Entry(e) => assert_eq!((e.path.as_str(), &e.kind), ("tree", &FileKind::Dir)),
        other => panic!("{other:?}"),
    }
    let dest = f.dir.join("dest");
    fs::create_dir(&dest).unwrap();
    assert_eq!(copy_in(&f, &dest, to_host(msgs)), GuestMsg::Done);
    assert_eq!(snapshot(&src), snapshot(&dest.join("tree")));
}

#[test]
fn copy_in_overwrites_files_and_merges_dirs() {
    let f = Fixture::new("overwrite");
    let dest = f.dir.join("d");
    fs::create_dir_all(dest.join("keep")).unwrap();
    fs::write(dest.join("keep/old"), "old").unwrap();
    symlink("/etc/passwd", dest.join("f")).unwrap();
    let entry = |path: &str, kind: FileKind| Entry {
        path: path.into(),
        mode: if kind == FileKind::Dir { 0o755 } else { 0o644 },
        kind,
        uid: f.uid,
        gid: f.gid,
        mtime: 0,
    };
    let r = copy_in(
        &f,
        &dest,
        vec![
            HostMsg::Entry(entry("keep", FileKind::Dir)),
            HostMsg::Entry(entry("keep/new", FileKind::File { size: 1 })),
            HostMsg::Data(b"n".to_vec()),
            // 링크 자리에 파일: 링크를 따라가지 않고 링크를 바꾼다.
            HostMsg::Entry(entry("f", FileKind::File { size: 2 })),
            HostMsg::Data(b"ok".to_vec()),
        ],
    );
    assert_eq!(r, GuestMsg::Done);
    assert_eq!(fs::read_to_string(dest.join("keep/old")).unwrap(), "old");
    assert_eq!(fs::read_to_string(dest.join("keep/new")).unwrap(), "n");
    assert!(!fs::symlink_metadata(dest.join("f")).unwrap().is_symlink());
    assert_eq!(fs::read_to_string(dest.join("f")).unwrap(), "ok");
}

#[test]
fn copy_in_cannot_escape_destination() {
    let f = Fixture::new("escape");
    let dest = f.dir.join("d");
    let outside = f.dir.join("outside");
    fs::create_dir_all(&dest).unwrap();
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, dest.join("link")).unwrap();
    let entry = |path: &str| {
        HostMsg::Entry(Entry {
            path: path.into(),
            kind: FileKind::File { size: 0 },
            mode: 0o644,
            uid: f.uid,
            gid: f.gid,
            mtime: 0,
        })
    };
    for bad in ["../outside/x", "/abs", "link/x", "a/./b", ""] {
        let r = copy_in(&f, &dest, vec![entry(bad)]);
        assert!(
            matches!(&r, GuestMsg::Error(e) if e.kind == ErrorKind::InvalidArgument),
            "{bad:?}: {r:?}"
        );
    }
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn copy_in_rejects_more_data_than_declared() {
    let f = Fixture::new("toolong");
    let dest = f.dir.join("d");
    fs::create_dir_all(&dest).unwrap();
    let r = copy_in(
        &f,
        &dest,
        vec![
            HostMsg::Entry(Entry {
                path: "x".into(),
                kind: FileKind::File { size: 1 },
                mode: 0o644,
                uid: f.uid,
                gid: f.gid,
                mtime: 0,
            }),
            HostMsg::Data(b"too much".to_vec()),
        ],
    );
    assert!(matches!(r, GuestMsg::Error(e) if e.kind == ErrorKind::Protocol));
}

#[test]
fn copy_out_of_missing_path_is_not_found() {
    let f = Fixture::new("missing");
    let mut c = f.connect();
    send(
        &mut c,
        HostMsg::CopyOut {
            path: f.dir.join("nope").to_str().unwrap().into(),
        },
    );
    assert_eq!(error_kind(&mut c), ErrorKind::NotFound);
    let mut c = f.connect();
    send(&mut c, HostMsg::CopyOut { path: "/".into() });
    assert_eq!(error_kind(&mut c), ErrorKind::InvalidArgument);
}
