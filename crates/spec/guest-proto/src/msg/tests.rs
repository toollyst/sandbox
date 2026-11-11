use super::*;

fn entry(path: &str, kind: FileKind) -> Entry {
    Entry {
        path: path.into(),
        kind,
        mode: 0o4755,
        uid: 1000,
        gid: 1000,
        mtime: -5,
    }
}

fn host_samples() -> Vec<HostMsg> {
    vec![
        HostMsg::Hello { version: 1 },
        HostMsg::Exec(Exec {
            args: vec!["/bin/sh".into(), "-c".into(), "echo 한글".into()],
            env: vec![("PATH".into(), "/bin".into()), ("EMPTY".into(), "".into())],
            cwd: "/".into(),
            user: User {
                user: Ident::Name("nobody".into()),
                group: Some(Ident::Id(65534)),
            },
            tty: true,
            stdin: true,
            size: Some(WinSize { rows: 24, cols: 80 }),
            main: true,
        }),
        HostMsg::Exec(Exec {
            args: vec![],
            env: vec![],
            cwd: "/w".into(),
            user: User {
                user: Ident::Id(0),
                group: None,
            },
            tty: false,
            stdin: false,
            size: None,
            main: false,
        }),
        HostMsg::Stdin(b"in".to_vec()),
        HostMsg::Stdin(vec![]),
        HostMsg::StdinClose,
        HostMsg::Signal(15),
        HostMsg::Resize(WinSize { rows: 1, cols: 2 }),
        HostMsg::CopyIn { dest: "/w".into() },
        HostMsg::CopyOut {
            path: "/etc".into(),
        },
        HostMsg::Entry(entry("a/b", FileKind::File { size: 3 })),
        HostMsg::Entry(entry("a", FileKind::Dir)),
        HostMsg::Entry(entry(
            "l",
            FileKind::Symlink {
                target: "../x".into(),
            },
        )),
        HostMsg::Data(vec![0, 1, 2]),
        HostMsg::End,
        HostMsg::Shutdown,
        HostMsg::Stat { path: "/w".into() },
        HostMsg::Share {
            tag: "sbx0".into(),
            target: "/data".into(),
            readonly: true,
        },
        HostMsg::EgressListen,
        HostMsg::EgressSlot,
        HostMsg::Connected,
        HostMsg::Refused {
            reason: "not allowed".into(),
        },
        HostMsg::SignalMain(15),
        HostMsg::Usage,
        HostMsg::Forward { port: 8080 },
        HostMsg::ExecDetached(Exec {
            args: vec!["sleep".into(), "9".into()],
            env: vec![],
            cwd: "/".into(),
            user: User {
                user: Ident::Id(0),
                group: None,
            },
            tty: false,
            stdin: false,
            size: None,
            main: false,
        }),
    ]
}

fn guest_samples() -> Vec<GuestMsg> {
    vec![
        GuestMsg::HelloAck { version: 1 },
        GuestMsg::Started { pid: 42 },
        GuestMsg::Usage(Usage {
            cpus: 2,
            cpu_busy: 10,
            cpu_total: 400,
            memory_total: 512 << 20,
            memory_used: 100 << 20,
            disk_total: 1 << 30,
            disk_used: 3 << 20,
            pids: 7,
        }),
        GuestMsg::Stdout(b"hi\n".to_vec()),
        GuestMsg::Stderr(vec![0xFF]),
        GuestMsg::Exited(ExitStatus::Code(-1)),
        GuestMsg::Exited(ExitStatus::Signal(9)),
        GuestMsg::Entry(entry("x", FileKind::File { size: 0 })),
        GuestMsg::Data(vec![9; 10]),
        GuestMsg::End,
        GuestMsg::Done,
        GuestMsg::Error(Error::new(ErrorKind::NotFound, "no such file")),
        GuestMsg::Error(Error::new(ErrorKind::Version, "")),
        GuestMsg::Stat(None),
        GuestMsg::Stat(Some(entry("w", FileKind::Dir))),
        GuestMsg::Listening { port: 3128 },
        GuestMsg::Connect {
            host: "pypi.org".into(),
            port: 443,
        },
    ]
}

#[test]
fn every_message_roundtrips_through_frames() {
    let mut wire = Vec::new();
    for m in host_samples() {
        m.write_to(&mut wire).unwrap();
    }
    let mut r = wire.as_slice();
    for m in host_samples() {
        assert_eq!(HostMsg::read_from(&mut r).unwrap(), m);
    }
    assert!(r.is_empty());

    let mut wire = Vec::new();
    for m in guest_samples() {
        m.write_to(&mut wire).unwrap();
    }
    let mut r = wire.as_slice();
    for m in guest_samples() {
        assert_eq!(GuestMsg::read_from(&mut r).unwrap(), m);
    }
}

#[test]
fn directions_do_not_mix() {
    for m in host_samples() {
        assert!(GuestMsg::decode(&m.encode()).is_err(), "{m:?}");
    }
    for m in guest_samples() {
        assert!(HostMsg::decode(&m.encode()).is_err(), "{m:?}");
    }
}

#[test]
fn every_truncation_and_extension_is_rejected() {
    // 어느 지점에서 잘라도, 끝에 한 바이트를 붙여도 오류여야 한다. 패닉하면 안 된다.
    for m in host_samples() {
        let body = m.encode();
        for n in 0..body.len() {
            assert!(HostMsg::decode(&body[..n]).is_err(), "{m:?} cut at {n}");
        }
        let mut longer = body.clone();
        longer.push(0);
        assert!(HostMsg::decode(&longer).is_err(), "{m:?} extended");
    }
    for m in guest_samples() {
        let body = m.encode();
        for n in 0..body.len() {
            assert!(GuestMsg::decode(&body[..n]).is_err(), "{m:?} cut at {n}");
        }
        let mut longer = body.clone();
        longer.push(0);
        assert!(GuestMsg::decode(&longer).is_err(), "{m:?} extended");
    }
}

#[test]
fn entry_mode_outside_permission_bits_is_rejected() {
    let mut body = HostMsg::Entry(entry("a", FileKind::Dir)).encode();
    // 경로(4+1) 뒤 종류(1) 뒤가 mode. 파일 형식 비트(S_IFREG)를 넣는다.
    let at = 1 + 4 + 1 + 1;
    body[at..at + 4].copy_from_slice(&0o100644u32.to_be_bytes());
    assert!(HostMsg::decode(&body).is_err());
}

#[test]
fn relative_paths() {
    for ok in ["a", "a/b", "...", ".hidden", "a b/c"] {
        assert!(check_relative_path(ok).is_ok(), "{ok}");
    }
    for bad in ["", "/a", "a/", "a//b", ".", "..", "a/../b", "./a", "a\0b"] {
        assert!(check_relative_path(bad).is_err(), "{bad:?}");
    }
}

/// 작은 결정적 난수 (xorshift64*). 테스트가 같은 입력을 되풀이하게 한다.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// 적대적 게스트: 아무 바이트나, 맞는 메시지를 흐트러뜨린 바이트를 보내도 해석은 패닉 없이 끝나고,
/// 성공하면 다시 인코딩한 값이 같다(받아들인 것은 규약 안의 것이다).
#[test]
fn hostile_bytes_never_panic_the_decoder() {
    let mut rng = Rng(0x5eed_5bc0_ffee);
    let seeds: Vec<Vec<u8>> = guest_samples()
        .iter()
        .map(GuestMsg::encode)
        .chain(host_samples().iter().map(HostMsg::encode))
        .collect();
    for i in 0..200_000 {
        let body: Vec<u8> = if i % 2 == 0 {
            let len = rng.below(64);
            (0..len).map(|_| rng.next() as u8).collect()
        } else {
            let mut b = seeds[rng.below(seeds.len())].clone();
            for _ in 0..=rng.below(4) {
                match rng.below(3) {
                    0 if !b.is_empty() => {
                        let at = rng.below(b.len());
                        b[at] = rng.next() as u8;
                    }
                    1 => {
                        let at = rng.below(b.len() + 1);
                        b.insert(at, rng.next() as u8);
                    }
                    _ if !b.is_empty() => {
                        let at = rng.below(b.len());
                        b.remove(at);
                    }
                    _ => {}
                }
            }
            b
        };
        if let Ok(m) = GuestMsg::decode(&body) {
            assert_eq!(GuestMsg::decode(&m.encode()).as_ref(), Ok(&m), "{body:?}");
        }
        if let Ok(m) = HostMsg::decode(&body) {
            assert_eq!(HostMsg::decode(&m.encode()).as_ref(), Ok(&m), "{body:?}");
        }
        // 프레임 읽기도: 길이 머리에 어떤 값이 와도 미리 크게 잡지 않는다.
        let mut framed = (body.len() as u32).to_be_bytes().to_vec();
        if i % 7 == 0 {
            framed[..4].copy_from_slice(&(rng.next() as u32).to_be_bytes());
        }
        framed.extend_from_slice(&body);
        let _ = GuestMsg::read_from(&mut &framed[..]);
    }
}
