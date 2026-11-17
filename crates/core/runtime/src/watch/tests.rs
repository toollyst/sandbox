use std::sync::{Arc, Mutex};

use notify::event::{AccessKind, CreateKind, Flag, ModifyKind};
use sbx_guest_proto::Error;

use super::*;

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sbx-watch-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn event(kind: EventKind, p: &Path) -> Event {
    Event::new(kind).add_path(p.to_path_buf())
}

fn modify(p: &Path) -> Event {
    event(EventKind::Modify(ModifyKind::Any), p)
}

/// 공유 밖의 경로와 접근만인 사건은 버리고, 같은 모습은 다시 보내지 않는다(게스트의 알림 연산이 남기는 사건).
#[test]
fn only_changed_looks_inside_a_share_are_sent() {
    let d = dir("pending");
    std::fs::create_dir_all(d.join("src")).unwrap();
    let f = d.join("src/main.ts");
    std::fs::write(&f, b"1").unwrap();
    let shares = [Share {
        host: d.clone(),
        target: "/app".into(),
    }];
    let mut p = Pending::new(&shares);
    assert!(p.is_empty());
    p.add(&modify(&f));
    p.add(&event(EventKind::Create(CreateKind::File), &d.join("gone")));
    p.add(&event(
        EventKind::Access(AccessKind::Any),
        &d.join("read-only"),
    ));
    p.add(&modify(Path::new("/elsewhere/x")));
    assert_eq!(
        p.take(),
        [(
            "/app".to_owned(),
            vec!["gone".to_owned(), "src/main.ts".to_owned()]
        )]
    );
    assert!(p.is_empty());

    // 같은 모습: 보내지 않는다(수정 시각을 그대로 다시 쓴 것, 권한만 바꾼 것).
    let m = std::fs::metadata(&f).unwrap().modified().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(m))
        .unwrap();
    p.add(&modify(&f));
    p.add(&modify(&d.join("gone")));
    assert!(p.take().is_empty());

    // 내용이 바뀌거나 지워지면 다시 보낸다.
    std::fs::write(&f, b"22").unwrap();
    p.add(&modify(&f));
    assert_eq!(
        p.take(),
        [("/app".to_owned(), vec!["src/main.ts".to_owned()])]
    );
    std::fs::remove_file(&f).unwrap();
    p.add(&event(
        EventKind::Remove(notify::event::RemoveKind::File),
        &f,
    ));
    assert_eq!(
        p.take(),
        [("/app".to_owned(), vec!["src/main.ts".to_owned()])]
    );
    std::fs::remove_dir_all(d).unwrap();
}

/// 놓쳤다는 표시는 공유마다 뿌리를 보낸다. 많으면 나눠 보낸다. 실제 경로(링크를 푼 경로)로 온 사건도 받는다.
#[test]
fn rescans_send_the_root_and_big_batches_are_split() {
    let a = dir("split-a");
    let b = dir("split-b");
    let shares = [
        Share {
            host: a.clone(),
            target: "/a".into(),
        },
        Share {
            host: b.clone(),
            target: "/b".into(),
        },
    ];
    let mut p = Pending::new(&shares);
    p.add(&Event::new(EventKind::Other).set_flag(Flag::Rescan));
    assert_eq!(
        p.take(),
        [
            ("/a".to_owned(), vec![String::new()]),
            ("/b".to_owned(), vec![String::new()])
        ]
    );

    let n = MAX_SHARE_CHANGES as usize + 10;
    let real = a.canonicalize().unwrap();
    for i in 0..n {
        p.add(&modify(&real.join(format!("f{i:05}"))));
    }
    let out = p.take();
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|(t, _)| t == "/a"));
    assert_eq!(out[0].1.len(), MAX_SHARE_CHANGES as usize);
    assert_eq!(out[0].1.len() + out[1].1.len(), n);
    std::fs::remove_dir_all(a).unwrap();
    std::fs::remove_dir_all(b).unwrap();
}

#[test]
fn relative_paths_stop_at_names_they_cannot_send() {
    assert_eq!(relative(Path::new("")), "");
    assert_eq!(relative(Path::new("a/b/c.txt")), "a/b/c.txt");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let odd = std::ffi::OsStr::from_bytes(b"\xff");
        assert_eq!(relative(&Path::new("a").join(odd).join("x")), "a");
    }
}

type Sent = Arc<Mutex<Vec<(String, Vec<String>)>>>;

fn wait_until(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !f() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn count(sent: &Sent, name: &str) -> usize {
    sent.lock()
        .unwrap()
        .iter()
        .filter(|(_, ps)| ps.iter().any(|p| p == name))
        .count()
}

/// 실제 감시(FSEvents, inotify): 호스트에서 고친 파일은 보내고, 게스트의 알림 연산(수정 시각을 그대로)은 되돌려 보내지
/// 않는다. 읽기 전용이 아닌 공유만 본다는 것은 부르는 쪽(`lib`)의 일이다. 메시지를 모르는 게스트면 한 줄 남기고 멈춘다.
#[test]
fn host_edits_reach_the_guest_without_echoes() {
    let d = dir("live");
    let sent: Sent = Arc::default();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let refuse = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let send: SendFn = {
        let (sent, refuse) = (sent.clone(), refuse.clone());
        Box::new(move |target: &str, paths: &[String]| {
            sent.lock()
                .unwrap()
                .push((target.to_owned(), paths.to_vec()));
            if refuse.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(RuntimeError::Guest(Error::new(
                    ErrorKind::Protocol,
                    "unknown host message 0x30",
                )));
            }
            Ok(())
        })
    };
    let notes = {
        let lines = lines.clone();
        Notes::about(
            TOPIC,
            Box::new(move |l: &str| lines.lock().unwrap().push(l.to_owned())),
        )
    };
    let running = start(
        vec![Share {
            host: d.clone(),
            target: "/app".into(),
        }],
        send,
        notes,
    )
    .expect("watching");
    // FSEvents는 감시를 연 직후의 사건을 놓칠 수 있다. 잠깐 둔다.
    std::thread::sleep(Duration::from_millis(300));
    let f = d.join("index.html");
    std::fs::write(&f, b"<h1>1</h1>").unwrap();
    wait_until("the edit was not sent", Duration::from_secs(10), || {
        count(&sent, "index.html") >= 1
    });

    // 쓰기 하나가 사건 둘(만듦, 바뀜)로 늦게 올 수 있다. 다 온 뒤에 센다.
    std::thread::sleep(MAX_WAIT * 2);
    // 게스트의 알림 연산 흉내: 수정 시각을 그대로 다시 쓴다(ctime만 바뀐다). 되돌려 보내지 않는다.
    let before = count(&sent, "index.html");
    let m = std::fs::metadata(&f).unwrap().modified().unwrap();
    std::fs::File::options()
        .write(true)
        .open(&f)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(m))
        .unwrap();
    std::thread::sleep(MAX_WAIT * 3);
    assert_eq!(count(&sent, "index.html"), before, "{:?}", sent.lock());

    // 메시지를 모르는 게스트: 한 줄 남기고 멈춘다.
    refuse.store(true, std::sync::atomic::Ordering::SeqCst);
    std::fs::write(&f, b"<h1>2</h1>").unwrap();
    wait_until("the refusal was not noted", Duration::from_secs(10), || {
        !lines.lock().unwrap().is_empty()
    });
    let l = lines.lock().unwrap().clone();
    assert!(
        l.len() == 1 && l[0].starts_with("sbx: share changes: ") && l[0].contains("restart"),
        "{l:?}"
    );
    let after = sent.lock().unwrap().len();
    std::fs::write(&f, b"<h1>3</h1>").unwrap();
    std::thread::sleep(MAX_WAIT * 3);
    assert_eq!(sent.lock().unwrap().len(), after, "kept sending");
    drop(running);
    std::fs::remove_dir_all(d).unwrap();
}

/// 볼 디렉터리가 없으면 그 공유만 감시 없이 간다(한 줄 남긴다).
#[test]
fn a_missing_folder_is_noted_and_skipped() {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let notes = {
        let lines = lines.clone();
        Notes::about(
            TOPIC,
            Box::new(move |l: &str| lines.lock().unwrap().push(l.to_owned())),
        )
    };
    let gone = std::env::temp_dir().join(format!("sbx-watch-{}-none", std::process::id()));
    let r = start(
        vec![Share {
            host: gone,
            target: "/x".into(),
        }],
        Box::new(|_: &str, _: &[String]| Ok(())),
        notes,
    );
    assert!(r.is_none());
    let l = lines.lock().unwrap().clone();
    assert!(l.len() == 1 && l[0].contains("/x"), "{l:?}");
    assert!(
        start(
            vec![],
            Box::new(|_: &str, _: &[String]| Ok(())),
            Notes::about(TOPIC, Box::new(|_| {}))
        )
        .is_none()
    );
}
