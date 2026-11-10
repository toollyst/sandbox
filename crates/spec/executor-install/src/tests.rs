use super::*;

fn root(name: &str) -> PathBuf {
    let r = std::env::temp_dir().join(format!("sbx-exinst-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&r);
    r
}

fn spec(s: &str) -> Spec {
    s.parse().unwrap()
}

#[test]
fn specs_and_versions() {
    assert_eq!(
        spec("firecracker@1.2.0"),
        Spec {
            name: "firecracker".into(),
            version: Some("1.2.0".into())
        }
    );
    assert_eq!(spec("fc").version, None);
    assert_eq!(spec("fc@latest"), spec("fc"));
    for bad in ["", "@1", "a@", "a@b@c", "../x", ".hidden", "a/b", "a b"] {
        assert!(bad.parse::<Spec>().is_err(), "{bad}");
    }
    assert!(version_matches("1.18.0", "1.18.0+wick.2"));
    assert!(version_matches("1.18", "1.18.3"));
    assert!(version_matches("1.18.0", "1.18.0"));
    assert!(!version_matches("1.1", "1.18"));
    assert!(!version_matches("1.18.0+wick.1", "1.18.0+wick.10"));
    assert_eq!(version_line("1.18.0+wick.2"), "1.18.0");
    assert_eq!(version_line("1.18.0-wick.2"), "1.18.0");
    assert_eq!(version_line("1.19.0-rc1+wick.1"), "1.19.0");
    assert_eq!(version_line("2.0"), "2.0");
    assert_eq!(
        version_cmp("1.18.0+wick.10", "1.18.0+wick.9"),
        Ordering::Greater
    );
    assert_eq!(version_cmp("1.10.0", "1.9.3"), Ordering::Greater);
    assert_eq!(version_cmp("1.2", "1.2.0"), Ordering::Less);
    assert_eq!(version_cmp("0.1.0-rc1", "0.1.0-rc2"), Ordering::Less);
    assert_eq!(version_cmp("007", "7"), Ordering::Equal);
}

#[test]
fn a_version_named_latest_is_not_installed() {
    let r = root("latest");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    let ex = Executors::new(r.join("executors"));
    let latest = Spec {
        name: "fc".into(),
        version: Some(LATEST.into()),
    };
    assert!(matches!(
        ex.install(&latest, &src, None, |_| Ok(())),
        Err(InstallError::NoVersion)
    ));
    assert!(ex.list().unwrap().is_empty());
    fs::remove_dir_all(r).unwrap();
}

#[test]
fn install_use_list_remove() {
    let r = root("flow");
    let src = r.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    fs::write(src.join("kernel"), b"k").unwrap();
    fs::write(src.join("sub/x"), b"x").unwrap();
    let ex = Executors::new(r.join("executors"));
    assert!(ex.list().unwrap().is_empty());
    assert_eq!(ex.default().unwrap(), None);

    // 묶음 디렉터리 설치. 처음 설치한 것이 기본이 된다.
    let a = ex.install(&spec("fc@1.9"), &src, None, |_| Ok(())).unwrap();
    assert!(
        a.executable.is_file() && a.dir.join("sub/x").is_file() && a.dir.join("kernel").is_file()
    );
    assert_eq!(ex.default().unwrap(), Some(a.clone()));
    // 실행 파일 하나 설치. 기본은 그대로.
    let b = ex
        .install(&spec("fc@1.10"), &src.join(executable_name()), None, |_| {
            Ok(())
        })
        .unwrap();
    assert_eq!(ex.default().unwrap(), Some(a.clone()));
    // 이름만 주면 가장 높은 버전.
    assert_eq!(ex.find(&spec("fc")).unwrap(), Some(b.clone()));
    let names: Vec<String> = ex.list().unwrap().iter().map(|i| i.to_string()).collect();
    assert_eq!(names, ["fc@1.9", "fc@1.10"]);

    // 이미 있으면, 버전이 없으면, 확인에 실패하면 거부. 실패하면 아무것도 남지 않는다.
    assert!(matches!(
        ex.install(&spec("fc@1.9"), &src, None, |_| Ok(())),
        Err(InstallError::AlreadyInstalled(_))
    ));
    assert!(matches!(
        ex.install(&spec("fc"), &src, None, |_| Ok(())),
        Err(InstallError::NoVersion)
    ));
    assert!(matches!(
        ex.install(&spec("fc@2"), &src, None, |_| Err("protocol 9".into())),
        Err(InstallError::Rejected(m)) if m == "protocol 9"
    ));
    assert!(matches!(
        ex.install(&spec("fc@3"), &src.join("sub"), None, |_| Ok(())),
        Err(InstallError::NoExecutable(_))
    ));
    let left: Vec<_> = fs::read_dir(r.join("executors"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".staging"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(ex.list().unwrap().len(), 2);

    // 기본 바꾸기, 지우기.
    assert_eq!(ex.set_default(&spec("fc@1.10")).unwrap(), b);
    assert!(ex.set_default(&spec("fc@7")).is_err());
    assert!(ex.remove(&spec("fc")).is_err());
    ex.remove(&spec("fc@1.10")).unwrap();
    assert_eq!(ex.default().unwrap(), None);
    ex.remove(&spec("fc@1.9")).unwrap();
    assert!(ex.list().unwrap().is_empty());
    fs::remove_dir_all(r).unwrap();
}

#[test]
fn vmm_packages_record_their_adapter() {
    let r = root("vmm");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("firecracker"), b"\x7fELF").unwrap();
    let ex = Executors::new(r.join("executors"));
    // 어댑터를 정하지 않으면 sbx-executor가 없는 묶음은 설치되지 않는다.
    assert!(matches!(
        ex.install(&spec("wick@1.18.0"), &src, None, |_| Ok(())),
        Err(InstallError::NoExecutable(_))
    ));
    assert!(matches!(
        ex.install(&spec("wick@1.18.0"), &src, Some("../x"), |_| Ok(())),
        Err(InstallError::BadAdapter(_))
    ));
    // 확인 함수는 임시 위치의 묶음과 어댑터 이름을 받는다.
    let mut seen = None;
    let w = ex
        .install(&spec("wick@1.18.0"), &src, Some("firecracker"), |s| {
            seen = Some((s.adapter.clone(), s.dir.join("firecracker").is_file()));
            Ok(())
        })
        .unwrap();
    assert_eq!(seen, Some((Some("firecracker".into()), true)));
    assert_eq!(w.adapter.as_deref(), Some("firecracker"));
    assert_eq!(ex.find(&spec("wick")).unwrap(), Some(w.clone()));
    assert!(w.dir.join("firecracker").is_file());
    // 실행 파일 하나를 VMM으로 설치하면 이름을 그대로 둔다.
    let one = ex
        .install(
            &spec("myfc@1"),
            &src.join("firecracker"),
            Some("firecracker"),
            |_| Ok(()),
        )
        .unwrap();
    assert!(one.dir.join("firecracker").is_file());
    assert!(!one.executable.exists());
    fs::remove_dir_all(r).unwrap();
}

#[test]
fn a_main_version_finds_its_newest_patch() {
    let r = root("lines");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    let ex = Executors::new(r.join("executors"));
    for v in [
        "1.18.0+wick.1",
        "1.18.0+wick.10",
        "1.18.0+wick.2",
        "1.19.0+wick.1",
    ] {
        ex.install(&spec(&format!("wick@{v}")), &src, None, |_| Ok(()))
            .unwrap();
    }
    let found = |s: &str| ex.find(&spec(s)).unwrap().map(|i| i.version);
    assert_eq!(found("wick@1.18.0").as_deref(), Some("1.18.0+wick.10"));
    assert_eq!(
        found("wick@1.18.0+wick.2").as_deref(),
        Some("1.18.0+wick.2")
    );
    assert_eq!(found("wick@1").as_deref(), Some("1.19.0+wick.1"));
    assert_eq!(found("wick@1.17"), None);
    // 지우기는 정확한 버전만.
    assert!(ex.remove(&spec("wick@1.18.0")).is_err());
    assert!(ex.remove(&spec("wick@1.18.0+wick.1")).is_ok());
    fs::remove_dir_all(r).unwrap();
}

#[test]
fn a_name_comes_from_one_origin_and_packages_cannot_claim_one() {
    let r = root("origin");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    // 묶음 안에 든 출처·어댑터 파일은 믿지 않는다.
    fs::write(src.join(ORIGIN_FILE), b"official\n").unwrap();
    fs::write(src.join(ADAPTER_FILE), b"firecracker\n").unwrap();
    let ex = Executors::new(r.join("executors"));

    let a = ex.install(&spec("fc@1"), &src, None, |_| Ok(())).unwrap();
    assert_eq!((a.origin, a.adapter), (None, None));
    // 출처가 없는 옛 설치는 막지 않는다.
    let b = ex
        .install_from(&spec("fc@2"), &src, None, Some("mine"), |_| Ok(()))
        .unwrap();
    assert_eq!(b.origin.as_deref(), Some("mine"));
    assert_eq!(
        ex.find(&spec("fc@2")).unwrap().unwrap().origin.as_deref(),
        Some("mine")
    );
    // 같은 출처는 된다, 다른 출처는 거부한다(아무것도 남지 않는다).
    ex.install_from(&spec("fc@3"), &src, None, Some("mine"), |_| Ok(()))
        .unwrap();
    assert!(matches!(
        ex.install_from(&spec("fc@4"), &src, None, Some("other"), |_| Ok(())),
        Err(InstallError::OtherOrigin { name, have, want }) if name == "fc" && have == "mine" && want == "other"
    ));
    assert!(ex.find(&spec("fc@4")).unwrap().is_none());
    // 직접 설치도 출처 하나로 친다.
    assert!(matches!(
        ex.install_from(&spec("fc@5"), &src, None, Some(LOCAL_ORIGIN), |_| Ok(())),
        Err(InstallError::OtherOrigin { .. })
    ));
    // 다른 이름은 상관없다.
    ex.install_from(&spec("other@1"), &src, None, Some("other"), |_| Ok(()))
        .unwrap();
    assert!(
        ex.install_from(&spec("x@1"), &src, None, Some("../x"), |_| Ok(()))
            .is_err()
    );
    let _ = fs::remove_dir_all(&r);
}

#[test]
fn bad_origins_and_hostile_marker_files_leave_nothing_behind() {
    let r = root("origin-hostile");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    let ex = Executors::new(r.join("executors"));
    // 출처 이름은 줄 하나, 경로가 아닌 이름이어야 한다.
    for bad in ["", "a b", "a\nb", ".hidden", "../x", "a/b", "über"] {
        assert!(
            ex.install_from(&spec("fc@1"), &src, None, Some(bad), |_| Ok(()))
                .is_err(),
            "{bad:?}"
        );
    }
    // 묶음이 출처 파일 자리에 디렉터리를 두면 설치하지 않는다(지울 수 없는 것을 출처로 남기지 않는다).
    fs::create_dir_all(src.join(ORIGIN_FILE)).unwrap();
    assert!(
        ex.install_from(&spec("fc@1"), &src, None, Some("mine"), |_| Ok(()))
            .is_err()
    );
    fs::remove_dir_all(src.join(ORIGIN_FILE)).unwrap();
    // 확인이 실패하면 설치하지 않고, 임시 디렉터리도 기본값도 남기지 않는다.
    assert!(matches!(
        ex.install_from(&spec("fc@1"), &src, None, Some("mine"), |_| Err("no".into())),
        Err(InstallError::Rejected(m)) if m == "no"
    ));
    assert!(ex.list().unwrap().is_empty());
    assert_eq!(ex.default().unwrap(), None);
    let left: Vec<_> = fs::read_dir(r.join("executors"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name())
                .filter(|n| n != ".install.lock")
                .collect()
        })
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
    let _ = fs::remove_dir_all(&r);
}

#[test]
fn an_unreadable_origin_record_counts_as_no_origin() {
    let r = root("origin-corrupt");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    let ex = Executors::new(r.join("executors"));
    let a = ex
        .install_from(&spec("fc@1"), &src, None, Some("mine"), |_| Ok(()))
        .unwrap();
    // 손으로 깨뜨린 기록(이름이 아닌 내용)은 출처로 읽지 않는다.
    fs::write(a.dir.join(ORIGIN_FILE), "../../etc\n").unwrap();
    assert_eq!(ex.find(&spec("fc@1")).unwrap().unwrap().origin, None);
    fs::write(a.dir.join(ORIGIN_FILE), "  mine \n").unwrap();
    assert_eq!(
        ex.find(&spec("fc@1")).unwrap().unwrap().origin.as_deref(),
        Some("mine")
    );
    let _ = fs::remove_dir_all(&r);
}

#[test]
fn names_and_versions_are_lowercase_so_case_cannot_alias_a_directory() {
    // 대소문자를 가리지 않는 파일 시스템(macOS, Windows)에서 `Fake`는 `fake` 디렉터리다.
    for bad in ["Fake", "fake@1.0-RC1", "FAKE@1", "fake@V1"] {
        assert!(bad.parse::<Spec>().is_err(), "{bad}");
    }
    assert!("fake@1.18.0+wick.1".parse::<Spec>().is_ok());
    let r = root("case");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("vmm"), b"").unwrap();
    let ex = Executors::new(r.join("executors"));
    // 어댑터 이름도 같다(VMM 묶음이 대소문자만 바꾼 어댑터를 가리키지 못한다).
    assert!(matches!(
        ex.install_from(
            &spec("wick@1"),
            &src.join("vmm"),
            Some("Firecracker"),
            Some("b"),
            |_| Ok(())
        ),
        Err(InstallError::BadAdapter(_))
    ));
    // 손으로 만든 대문자 디렉터리는 설치된 것으로 보지 않는다.
    fs::create_dir_all(r.join("executors/Fake/1.0")).unwrap();
    fs::create_dir_all(r.join("executors/fake2/1.0-RC")).unwrap();
    assert!(ex.list().unwrap().is_empty());
    let _ = fs::remove_dir_all(&r);
}

#[test]
fn concurrent_installs_from_different_origins_cannot_both_win() {
    let r = root("origin-race");
    let src = r.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join(executable_name()), b"#!/bin/sh\n").unwrap();
    let ex = Executors::new(r.join("executors"));
    // 같은 이름의 다른 버전을 출처 여럿에서 동시에 설치한다. 확인(verify)을 느리게 해 검사와 rename 사이를 벌린다.
    let won: Vec<String> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..6)
            .map(|t| {
                let (ex, src) = (&ex, &src);
                s.spawn(move || {
                    let origin = format!("o{t}");
                    ex.install_from(&spec(&format!("fc@{t}")), src, None, Some(&origin), |_| {
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        Ok(())
                    })
                    .ok()
                    .map(|_| origin)
                })
            })
            .collect();
        hs.into_iter().filter_map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(won.len(), 1, "{won:?}");
    let origins: Vec<_> = ex.list().unwrap().into_iter().map(|i| i.origin).collect();
    assert_eq!(origins, [Some(won[0].clone())]);
    // 받기 전 검사도 같은 답을 한다.
    assert!(ex.check_origin(&spec("fc"), &won[0]).is_ok());
    assert!(matches!(
        ex.check_origin(&spec("fc"), "other"),
        Err(InstallError::OtherOrigin { .. })
    ));
    let _ = fs::remove_dir_all(&r);
}
