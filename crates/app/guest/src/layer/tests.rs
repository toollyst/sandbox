use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::*;

/// 흉내 낸 위층: whiteout은 표시한 경로의 빈 파일, xattr은 경로마다 적은 것.
#[derive(Default)]
struct Marked {
    whiteouts: BTreeSet<PathBuf>,
    xattrs: BTreeMap<PathBuf, Vec<(String, Vec<u8>)>>,
}

impl UpperFs for Marked {
    fn xattrs(&self, path: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
        Ok(self.xattrs.get(path).cloned().unwrap_or_default())
    }
    fn is_whiteout(&self, path: &Path, meta: &fs::Metadata) -> bool {
        Kernel.is_whiteout(path, meta) || self.whiteouts.contains(path)
    }
}

#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl Write for Buf {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let d = std::env::temp_dir().join(format!("sbx-layer-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Self(d)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// 보낸 메시지: (항목, 그 뒤의 내용).
fn run(upper: &Path, skip: &[&str], fs: &dyn UpperFs) -> Result<Vec<(LayerEntry, Vec<u8>)>, Error> {
    let buf = Buf::default();
    let out = Out::new(buf.clone());
    let skip: Vec<String> = skip.iter().map(|s| s.to_string()).collect();
    export_upper(upper, &skip, &[], fs, &out)?;
    let bytes = buf.0.lock().unwrap().clone();
    let mut r = bytes.as_slice();
    let mut got: Vec<(LayerEntry, Vec<u8>)> = Vec::new();
    while !r.is_empty() {
        match GuestMsg::read_from(&mut r).unwrap() {
            GuestMsg::LayerEntry(e) => got.push((e, Vec::new())),
            GuestMsg::Data(d) => got.last_mut().unwrap().1.extend(d),
            other => panic!("unexpected {other:?}"),
        }
    }
    Ok(got)
}

fn kinds(got: &[(LayerEntry, Vec<u8>)]) -> Vec<(String, LayerKind)> {
    got.iter()
        .map(|(e, _)| (e.path.clone(), e.kind.clone()))
        .collect()
}

#[test]
fn an_empty_upper_exports_nothing() {
    let d = Dir::new("empty");
    assert!(run(&d.0, &[], &Kernel).unwrap().is_empty());
}

#[test]
fn files_dirs_links_and_fifos_in_name_order_parents_first() {
    let d = Dir::new("kinds");
    let u = &d.0;
    fs::create_dir_all(u.join("etc/app")).unwrap();
    fs::write(u.join("etc/app/conf"), "v=1\n").unwrap();
    fs::write(u.join("etc-b"), "").unwrap();
    symlink("/etc/app/conf", u.join("link")).unwrap();
    fs::hard_link(u.join("etc/app/conf"), u.join("z-hard")).unwrap();
    fs::hard_link(u.join("etc/app/conf"), u.join("a-hard")).unwrap();
    std::process::Command::new("mkfifo")
        .arg(u.join("pipe"))
        .status()
        .unwrap();
    let _sock = std::os::unix::net::UnixListener::bind(u.join("sock")).unwrap();
    let got = run(u, &[], &Kernel).unwrap();
    let dir = LayerKind::Dir { opaque: false };
    let file = |n| LayerKind::File { size: n };
    // 이름 순서: "a-hard"가 먼저 오므로 내용은 거기에 붙고 나머지 둘은 그 경로로의 하드 링크다.
    let hard = || LayerKind::Hardlink {
        target: "a-hard".into(),
    };
    assert_eq!(
        kinds(&got),
        vec![
            ("a-hard".into(), file(4)),
            ("etc".into(), dir.clone()),
            ("etc/app".into(), dir),
            ("etc/app/conf".into(), hard()),
            ("etc-b".into(), file(0)),
            (
                "link".into(),
                LayerKind::Symlink {
                    target: "/etc/app/conf".into()
                }
            ),
            ("pipe".into(), LayerKind::Fifo),
            ("z-hard".into(), hard()),
        ]
    );
    assert_eq!(got[0].1, b"v=1\n");
    let meta = fs::metadata(u.join("etc/app/conf")).unwrap();
    assert_eq!(got[0].0.mode, meta.mode() & 0o7777);
    assert_eq!((got[0].0.uid, got[0].0.gid), (meta.uid(), meta.gid()));
    assert_eq!(got[0].0.mtime, meta.mtime());
}

#[test]
fn whiteouts_opaque_dirs_and_xattrs() {
    let d = Dir::new("wh");
    let u = &d.0;
    fs::create_dir_all(u.join("var/cache")).unwrap();
    fs::write(u.join("var/gone"), "").unwrap();
    fs::write(u.join("var/cache/new"), "n").unwrap();
    fs::write(u.join("bin"), "x").unwrap();
    let mut m = Marked::default();
    m.whiteouts.insert(u.join("var/gone"));
    m.xattrs.insert(
        u.join("var/cache"),
        vec![
            ("trusted.overlay.opaque".into(), b"y".to_vec()),
            ("trusted.overlay.origin".into(), vec![1, 2]),
            ("user.keep".into(), b"1".to_vec()),
        ],
    );
    m.xattrs.insert(
        u.join("bin"),
        vec![("security.capability".into(), vec![1, 0, 0, 2])],
    );
    // "x"는 xattr whiteout이 든 디렉터리의 표시이지 opaque가 아니다.
    m.xattrs.insert(
        u.join("var"),
        vec![("trusted.overlay.opaque".into(), b"x".to_vec())],
    );
    let got = run(u, &[], &m).unwrap();
    assert_eq!(
        kinds(&got),
        vec![
            ("bin".into(), LayerKind::File { size: 1 }),
            ("var".into(), LayerKind::Dir { opaque: false }),
            ("var/cache".into(), LayerKind::Dir { opaque: true }),
            ("var/cache/new".into(), LayerKind::File { size: 1 }),
            ("var/gone".into(), LayerKind::Whiteout),
        ]
    );
    assert_eq!(
        got[0].0.xattrs,
        vec![("security.capability".into(), vec![1, 0, 0, 2])]
    );
    // overlay 내부용은 빠지고 나머지는 남는다.
    assert_eq!(got[2].0.xattrs, vec![("user.keep".into(), b"1".to_vec())]);
    assert!(got[1].0.xattrs.is_empty());
}

#[test]
fn redirect_and_metacopy_are_errors() {
    for what in ["redirect", "metacopy"] {
        let d = Dir::new(what);
        fs::create_dir(d.0.join("moved")).unwrap();
        let mut m = Marked::default();
        m.xattrs.insert(
            d.0.join("moved"),
            vec![(format!("trusted.overlay.{what}"), b"/x".to_vec())],
        );
        let e = run(&d.0, &[], &m).unwrap_err();
        assert!(e.message.contains(what), "{e:?}");
    }
}

#[test]
fn empty_mount_stubs_at_the_top_are_skipped() {
    let d = Dir::new("stubs");
    let u = &d.0;
    fs::create_dir_all(u.join("proc")).unwrap();
    fs::create_dir_all(u.join("dev")).unwrap();
    fs::write(u.join("dev/kept"), "").unwrap();
    fs::create_dir_all(u.join("x/proc")).unwrap();
    let got = run(u, &["proc", "dev"], &Kernel).unwrap();
    let dir = LayerKind::Dir { opaque: false };
    // 비어 있지 않은 자리와 맨 위가 아닌 같은 이름은 남는다.
    assert_eq!(
        kinds(&got),
        vec![
            ("dev".into(), dir.clone()),
            ("dev/kept".into(), LayerKind::File { size: 0 }),
            ("x".into(), dir.clone()),
            ("x/proc".into(), dir),
        ]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn names_that_are_not_utf8_are_errors() {
    use std::os::unix::ffi::OsStrExt;
    let d = Dir::new("utf8");
    fs::write(d.0.join(std::ffi::OsStr::from_bytes(b"bad\xff")), "").unwrap();
    let e = run(&d.0, &[], &Kernel).unwrap_err();
    assert!(e.message.contains("UTF-8"), "{e:?}");
}

#[test]
fn the_server_answers_layer_requests_without_layers_with_an_error() {
    let buf = Buf::default();
    let out = Out::new(buf.clone());
    open(None, &out);
    export(None, &out);
    let bytes = buf.0.lock().unwrap().clone();
    let mut r = bytes.as_slice();
    for _ in 0..2 {
        match GuestMsg::read_from(&mut r).unwrap() {
            GuestMsg::Error(e) => assert_eq!(e.kind, ErrorKind::InvalidArgument),
            other => panic!("{other:?}"),
        }
    }
}

fn run_planted(
    upper: &Path,
    skip: &[&str],
    planted: &[Planted],
) -> Result<Vec<(String, LayerKind)>, Error> {
    let buf = Buf::default();
    let out = Out::new(buf.clone());
    let skip: Vec<String> = skip.iter().map(|s| s.to_string()).collect();
    export_upper(upper, &skip, planted, &Kernel, &out)?;
    let bytes = buf.0.lock().unwrap().clone();
    let mut r = bytes.as_slice();
    let mut got = Vec::new();
    while !r.is_empty() {
        if let GuestMsg::LayerEntry(e) = GuestMsg::read_from(&mut r).unwrap() {
            got.push((e.path, e.kind));
        }
    }
    Ok(got)
}

/// sbx가 층에 둔 `/etc/resolv.conf`(`--network allow`)는 만든 이미지에 남지 않는다. 단계가 바꾼 것은 남는다.
#[test]
fn files_sbx_planted_are_not_exported_unless_a_step_changed_them() {
    let resolv = Planted {
        path: "etc/resolv.conf".into(),
        content: crate::transparent::RESOLV_CONF.as_bytes().to_vec(),
    };
    let dir = LayerKind::Dir { opaque: false };
    // 이미지에 /etc가 있었다: 위층에는 복사된 etc/와 sbx의 파일. etc/는 보내고(Docker의 층에도 있다) 파일은 뺀다.
    let d = Dir::new("planted");
    fs::create_dir(d.0.join("etc")).unwrap();
    crate::transparent::write_resolv_conf(&d.0).unwrap();
    fs::write(d.0.join("etc/hosts.new"), "h").unwrap();
    let got = run_planted(&d.0, &[], std::slice::from_ref(&resolv)).unwrap();
    assert_eq!(
        got,
        vec![
            ("etc".into(), dir.clone()),
            ("etc/hosts.new".into(), LayerKind::File { size: 1 })
        ]
    );
    // /etc를 sbx가 만들었고 그 안에 sbx의 파일만 있으면 /etc도 뺀다.
    let d = Dir::new("planted-stub");
    crate::transparent::write_resolv_conf(&d.0).unwrap();
    let got = run_planted(&d.0, &["etc"], std::slice::from_ref(&resolv)).unwrap();
    assert!(got.is_empty(), "{got:?}");
    // 단계가 바꾼 resolv.conf는 그 단계가 한 일이다.
    fs::write(d.0.join("etc/resolv.conf"), "nameserver 10.0.0.1\n").unwrap();
    let got = run_planted(&d.0, &["etc"], std::slice::from_ref(&resolv)).unwrap();
    assert_eq!(
        got,
        vec![
            ("etc".into(), dir),
            ("etc/resolv.conf".into(), LayerKind::File { size: 20 })
        ]
    );
}
