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
    for bad in ["", "@1", "a@", "a@b@c", "../x", ".hidden", "a/b", "a b"] {
        assert!(bad.parse::<Spec>().is_err(), "{bad}");
    }
    assert_eq!(version_cmp("1.10.0", "1.9.3"), Ordering::Greater);
    assert_eq!(version_cmp("1.2", "1.2.0"), Ordering::Less);
    assert_eq!(version_cmp("0.1.0-rc1", "0.1.0-rc2"), Ordering::Less);
    assert_eq!(version_cmp("007", "7"), Ordering::Equal);
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
    let a = ex.install(&spec("fc@1.9"), &src, |_| Ok(())).unwrap();
    assert!(
        a.executable.is_file() && a.dir.join("sub/x").is_file() && a.dir.join("kernel").is_file()
    );
    assert_eq!(ex.default().unwrap(), Some(a.clone()));
    // 실행 파일 하나 설치. 기본은 그대로.
    let b = ex
        .install(&spec("fc@1.10"), &src.join(executable_name()), |_| Ok(()))
        .unwrap();
    assert_eq!(ex.default().unwrap(), Some(a.clone()));
    // 이름만 주면 가장 높은 버전.
    assert_eq!(ex.find(&spec("fc")).unwrap(), Some(b.clone()));
    let names: Vec<String> = ex.list().unwrap().iter().map(|i| i.to_string()).collect();
    assert_eq!(names, ["fc@1.9", "fc@1.10"]);

    // 이미 있으면, 버전이 없으면, 확인에 실패하면 거부. 실패하면 아무것도 남지 않는다.
    assert!(matches!(
        ex.install(&spec("fc@1.9"), &src, |_| Ok(())),
        Err(InstallError::AlreadyInstalled(_))
    ));
    assert!(matches!(
        ex.install(&spec("fc"), &src, |_| Ok(())),
        Err(InstallError::NoVersion)
    ));
    assert!(matches!(
        ex.install(&spec("fc@2"), &src, |_| Err("protocol 9".into())),
        Err(InstallError::Rejected(m)) if m == "protocol 9"
    ));
    assert!(matches!(
        ex.install(&spec("fc@3"), &src.join("sub"), |_| Ok(())),
        Err(InstallError::NoExecutable(_))
    ));
    let left: Vec<_> = fs::read_dir(r.join("executors"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
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
