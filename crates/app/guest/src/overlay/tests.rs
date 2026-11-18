//! 실제 커널 overlayfs로 층을 쌓고 내보낸다. root와 loop 장치가 필요하므로 `SANDBOX_PRIVILEGED_TESTS=1`일 때만
//! 돈다(CI는 privileged 컨테이너에서 `cargo test -p sbx-guest --lib privileged`로 돌린다).
//!
//! 테스트는 연결 처리 프로세스의 뿌리를 옮기지 않는다(`enter: false`). 층은 `layers/root`에서 본다.
//! 내보낸 층을 OCI 규칙(whiteout, opaque, 하드 링크)대로 차례로 적용한 결과가 마지막 층으로 본 모습과 같은지 본다.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use sbx_guest_proto::{GuestMsg, LayerEntry, LayerKind};

use super::*;
use crate::mkfs;

/// 층 열기는 프로세스의 작업 디렉터리를 바꾼다(게스트에는 층이 하나뿐이다). 테스트끼리는 차례로 돈다.
static SERIAL: Mutex<()> = Mutex::new(());

fn privileged() -> Option<std::sync::MutexGuard<'static, ()>> {
    if std::env::var_os("SANDBOX_PRIVILEGED_TESTS").is_none() {
        eprintln!("SANDBOX_PRIVILEGED_TESTS not set, skipping");
        return None;
    }
    Some(SERIAL.lock().unwrap_or_else(|e| e.into_inner()))
}

fn sh(script: &str) {
    let out = Command::new("sh").args(["-ec", script]).output().unwrap();
    assert!(
        out.status.success(),
        "{script}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// scratch(우리 mkfs로 포맷한 ext4)를 loop로 마운트한 작업 공간. 버리면 모두 내린다.
///
/// loop 장치는 직접 붙이고 쓰기로 되돌린 뒤(`blockdev --setrw`) `mount -w`로 마운트한다. 앞서 누가 그 장치에
/// 읽기 전용 표시(BLKROSET)를 남겼으면 떼어도 남고, `mount -o loop`는 그런 장치를 알리지 않고 읽기 전용으로
/// 마운트한다(그래서 층 준비가 "Read-only file system"으로 실패했다). `-w`면 그때 마운트가 실패한다.
struct Scratch {
    dir: PathBuf,
    dev: String,
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("sbx-overlay-{name}-{}", std::process::id()));
        let _ = Command::new("umount")
            .arg("-R")
            .arg(dir.join("s"))
            .stderr(std::process::Stdio::null())
            .status();
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("s")).unwrap();
        let img = dir.join("scratch.img");
        let bytes = 256 << 20;
        let mut f = fs::File::create(&img).unwrap();
        f.set_len(bytes).unwrap();
        mkfs::format(&mut f, bytes, *b"sbx-overlay-test", mkfs::Disk::Zeroed).unwrap();
        drop(f);
        let out = Command::new("losetup")
            .args(["-f", "--show"])
            .arg(&img)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let dev = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        sh(&format!(
            "blockdev --setrw {dev} && mount -w -t ext4 -o noatime {dev} {}",
            dir.join("s").display()
        ));
        Self { dir, dev }
    }

    fn layers(&self) -> PathBuf {
        self.dir.join("s/layers")
    }

    fn root(&self) -> PathBuf {
        self.layers().join("root")
    }

    /// 층 디렉터리를 준비하고 `lower`에 `base`로 이미지 rootfs를 만든다.
    fn overlay(&self, base: fn(&Path)) -> Overlay {
        Overlay::new(
            self.layers(),
            false,
            Box::new(move |dir| {
                fresh_dir(dir)?;
                fs::create_dir(dir.join("lower"))?;
                base(&dir.join("lower"));
                Ok(())
            }),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = Command::new("umount").arg("-R").arg(self.root()).status();
        let _ = Command::new("umount").arg(self.dir.join("s")).status();
        let _ = Command::new("losetup").args(["-d", &self.dev]).status();
        let _ = fs::remove_dir_all(&self.dir);
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

fn export(o: &Overlay) -> Vec<(LayerEntry, Vec<u8>)> {
    let buf = Buf::default();
    o.export(&Out::new(buf.clone())).unwrap();
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
    got
}

/// 비교할 모습: 종류(내용 포함)와 권한.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    Dir(u32),
    File(u32, Vec<u8>),
    Link(String),
    Fifo(u32),
    Char(u32, u32, u32),
}

type Tree = BTreeMap<String, Node>;

fn remove_below(t: &mut Tree, p: &str) {
    let prefix = format!("{p}/");
    t.retain(|k, _| !k.starts_with(&prefix));
}

/// OCI 층 규칙으로 적용한다(sbx-rootfs의 병합과 같은 규칙: whiteout과 opaque는 아래층에만, 먼저).
fn apply(t: &mut Tree, layer: &[(LayerEntry, Vec<u8>)]) {
    for (e, _) in layer {
        match e.kind {
            LayerKind::Whiteout => {
                t.remove(&e.path);
                remove_below(t, &e.path);
            }
            LayerKind::Dir { opaque: true } => remove_below(t, &e.path),
            _ => {}
        }
    }
    for (e, data) in layer {
        let node = match &e.kind {
            LayerKind::Whiteout => continue,
            LayerKind::Dir { .. } => Node::Dir(e.mode),
            LayerKind::File { .. } => Node::File(e.mode, data.clone()),
            LayerKind::Hardlink { target } => t[target].clone(),
            LayerKind::Symlink { target } => Node::Link(target.clone()),
            LayerKind::Fifo => Node::Fifo(e.mode),
            LayerKind::CharDevice { major, minor } => Node::Char(e.mode, *major, *minor),
            LayerKind::BlockDevice { .. } => panic!("no block devices in this test"),
        };
        if !matches!(node, Node::Dir(_)) {
            remove_below(t, &e.path);
        }
        t.insert(e.path.clone(), node);
    }
}

/// 디렉터리의 모습. 맨 위의 `skip`은 뺀다.
fn read(root: &Path, skip: &[&str]) -> Tree {
    fn walk(dir: &Path, rel: &str, t: &mut Tree, skip: &[&str]) {
        for c in fs::read_dir(dir).unwrap() {
            let c = c.unwrap();
            let name = c.file_name().into_string().unwrap();
            if rel.is_empty() && skip.contains(&name.as_str()) {
                continue;
            }
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let m = fs::symlink_metadata(c.path()).unwrap();
            let mode = m.mode() & 0o7777;
            let ft = m.file_type();
            use std::os::unix::fs::FileTypeExt;
            let node = if ft.is_dir() {
                walk(&c.path(), &path, t, skip);
                Node::Dir(mode)
            } else if ft.is_symlink() {
                Node::Link(fs::read_link(c.path()).unwrap().to_string_lossy().into())
            } else if ft.is_file() {
                Node::File(mode, fs::read(c.path()).unwrap())
            } else if ft.is_fifo() {
                Node::Fifo(mode)
            } else if ft.is_char_device() {
                let (a, b) = crate::sys::dev_split(m.rdev());
                Node::Char(mode, a, b)
            } else {
                panic!("unexpected {path}");
            };
            t.insert(path, node);
        }
    }
    let mut t = Tree::new();
    walk(root, "", &mut t, skip);
    t
}

fn kinds(layer: &[(LayerEntry, Vec<u8>)]) -> Vec<(&str, &LayerKind)> {
    layer
        .iter()
        .map(|(e, _)| (e.path.as_str(), &e.kind))
        .collect()
}

fn base(lower: &Path) {
    fs::create_dir_all(lower.join("etc")).unwrap();
    fs::write(lower.join("etc/hosts"), "base\n").unwrap();
    fs::write(lower.join("etc/keep"), "k\n").unwrap();
    fs::create_dir_all(lower.join("data/d1/sub")).unwrap();
    fs::write(lower.join("data/d1/old"), "old\n").unwrap();
    fs::write(lower.join("data/d1/sub/x"), "x\n").unwrap();
    fs::create_dir_all(lower.join("lib")).unwrap();
    fs::write(lower.join("lib/target"), "t\n").unwrap();
    symlink("target", lower.join("lib/link")).unwrap();
}

/// 이미지가 층 디스크 둘로 왔을 때(부팅이 쌓는 것과 같이 `lower/1`이 `lower/0` 위): 빌드 층은 그 둘 위에 쌓이고,
/// 이미지의 위 층이 지운 것(whiteout, 문자 장치 0/0)은 빌드 층에서도 보이지 않는다.
#[test]
fn privileged_build_layers_stack_on_an_image_of_several_layer_disks() {
    let Some(_serial) = privileged() else {
        return;
    };
    let s = Scratch::new("multi");
    let o = Overlay::new(
        s.layers(),
        false,
        Box::new(|dir| {
            fresh_dir(dir)?;
            let (bottom, top) = (dir.join("lower/0"), dir.join("lower/1"));
            fs::create_dir_all(bottom.join("etc"))?;
            fs::write(bottom.join("etc/hosts"), "bottom\n")?;
            fs::write(bottom.join("etc/gone"), "x\n")?;
            fs::create_dir_all(top.join("etc"))?;
            fs::write(top.join("etc/hosts"), "top\n")?;
            sh(&format!("mknod {}/etc/gone c 0 0", top.display()));
            Ok(())
        }),
    )
    .with_base(vec!["lower/1".into(), "lower/0".into()]);
    let r = s.root();
    assert_eq!(o.open().unwrap(), 1);
    assert_eq!(fs::read_to_string(r.join("etc/hosts")).unwrap(), "top\n");
    assert!(!r.join("etc/gone").exists());
    fs::write(r.join("etc/new"), "n\n").unwrap();
    let l1 = export(&o);
    let k = kinds(&l1);
    // 이미지의 층은 내보내지 않는다. 빌드 층의 변경만 나간다.
    assert_eq!(
        k,
        [
            ("etc", &LayerKind::Dir { opaque: false }),
            ("etc/new", &LayerKind::File { size: 2 })
        ]
    );
    assert_eq!(o.open().unwrap(), 2);
    assert_eq!(fs::read_to_string(r.join("etc/new")).unwrap(), "n\n");
    assert_eq!(fs::read_to_string(r.join("etc/hosts")).unwrap(), "top\n");
}

/// 투명 TCP가 켜져 있으면 층마다 `/etc/resolv.conf`가 루프백 DNS를 가리키고(보통의 샌드박스와 같다), 내보낸 층에는
/// 없다. 단계가 그 파일을 바꾸면 그 변경은 나간다. `/etc`가 없는 이미지에서는 `/etc`도 남지 않는다.
#[test]
fn privileged_layers_see_the_sandbox_resolver_but_do_not_export_it() {
    let Some(_serial) = privileged() else {
        return;
    };
    let s = Scratch::new("resolv");
    let o = s.overlay(base).with_resolver(Box::new(|| true));
    let r = s.root();
    assert_eq!(o.open().unwrap(), 1);
    assert_eq!(
        fs::read_to_string(r.join("etc/resolv.conf")).unwrap(),
        crate::transparent::RESOLV_CONF
    );
    fs::write(r.join("etc/app.conf"), "a\n").unwrap();
    let l1 = export(&o);
    assert_eq!(
        kinds(&l1),
        [
            ("etc", &LayerKind::Dir { opaque: false }),
            ("etc/app.conf", &LayerKind::File { size: 2 })
        ]
    );
    assert_eq!(o.open().unwrap(), 2);
    fs::write(r.join("etc/resolv.conf"), "nameserver 10.0.0.1\n").unwrap();
    let l2 = export(&o);
    assert!(
        kinds(&l2).contains(&("etc/resolv.conf", &LayerKind::File { size: 20 })),
        "{:?}",
        kinds(&l2)
    );

    let s = Scratch::new("resolv-bare");
    let o = s
        .overlay(|lower| fs::write(lower.join("only"), "o\n").unwrap())
        .with_resolver(Box::new(|| true));
    assert_eq!(o.open().unwrap(), 1);
    assert!(s.root().join("etc/resolv.conf").is_file());
    assert!(export(&o).is_empty());
}

#[test]
fn privileged_three_stacked_layers_export_what_the_guest_saw() {
    let Some(_serial) = privileged() else {
        return;
    };
    let s = Scratch::new("stack");
    let o = s.overlay(base);
    let r = s.root();
    assert_eq!(o.check_ready().map_err(|e| e.message), Ok(()));

    // 층 1: 더하기, 바꾸기, 지우기(whiteout), 지우고 다시 만든 디렉터리(opaque), 하드 링크, 심볼릭 링크, FIFO, 장치.
    assert_eq!(o.open().unwrap(), 1);
    // 이미지 rootfs(첫 열기가 준비한다).
    let mut tree = read(&s.layers().join("lower"), &[]);
    assert_eq!(read(&r, &["proc", "sys", "dev"]), tree);
    fs::write(r.join("new.txt"), "new\n").unwrap();
    fs::write(r.join("etc/hosts"), "changed\n").unwrap();
    fs::remove_file(r.join("etc/keep")).unwrap();
    fs::remove_dir_all(r.join("data/d1")).unwrap();
    fs::create_dir(r.join("data/d1")).unwrap();
    fs::write(r.join("data/d1/new"), "fresh\n").unwrap();
    fs::write(r.join("h1"), "same inode\n").unwrap();
    fs::hard_link(r.join("h1"), r.join("h2")).unwrap();
    symlink("/new.txt", r.join("sym")).unwrap();
    sh(&format!(
        "mkfifo {r}/fifo && mknod {r}/null c 1 3",
        r = r.display()
    ));
    let l1 = export(&o);
    let opaque = LayerKind::Dir { opaque: true };
    let dir = LayerKind::Dir { opaque: false };
    let k = kinds(&l1);
    assert!(k.contains(&("etc/keep", &LayerKind::Whiteout)), "{k:?}");
    assert!(k.contains(&("data/d1", &opaque)), "{k:?}");
    assert!(k.contains(&("data", &dir)), "{k:?}");
    assert!(
        k.contains(&(
            "h2",
            &LayerKind::Hardlink {
                target: "h1".into()
            }
        )),
        "{k:?}"
    );
    assert!(
        k.contains(&("null", &LayerKind::CharDevice { major: 1, minor: 3 })),
        "{k:?}"
    );
    // 이미지에 없던 마운트 자리(/proc, /sys, /dev)는 위층에 생겼지만 내보내지 않는다.
    for stub in ["proc", "sys", "dev"] {
        assert!(s.layers().join("1").join(stub).is_dir());
        assert!(!k.iter().any(|(p, _)| *p == stub), "{stub} exported: {k:?}");
    }
    // 아래층에서 온 파일이 없는 xattr(overlay 내부용)은 보내지 않는다.
    assert!(l1.iter().all(|(e, _)| e.xattrs.is_empty()), "{l1:?}");
    apply(&mut tree, &l1);

    // 층 2: 바꾸지 않는다. 빈 층이다.
    assert_eq!(o.open().unwrap(), 2);
    assert_eq!(read(&r, &["proc", "sys", "dev"]), tree);
    let l2 = export(&o);
    assert!(l2.is_empty(), "{l2:?}");

    // 층 3: 층 1에서 만든 것을 지우고, 아래층 파일의 권한만 바꾼다(metacopy가 꺼져 있으면 내용까지 올라온다).
    assert_eq!(o.open().unwrap(), 3);
    fs::remove_file(r.join("new.txt")).unwrap();
    fs::remove_file(r.join("h1")).unwrap();
    fs::set_permissions(r.join("lib/target"), fs::Permissions::from_mode(0o600)).unwrap();
    // redirect_dir=off: 아래층 디렉터리의 이름은 바꿀 수 없다(mv는 복사로 바꿔 한다).
    let e = fs::rename(r.join("lib"), r.join("lib2")).unwrap_err();
    assert_eq!(e.raw_os_error(), Some(libc::EXDEV), "{e}");
    let l3 = export(&o);
    let k = kinds(&l3);
    assert!(k.contains(&("new.txt", &LayerKind::Whiteout)), "{k:?}");
    assert!(k.contains(&("h1", &LayerKind::Whiteout)), "{k:?}");
    let target = l3.iter().find(|(e, _)| e.path == "lib/target").unwrap();
    assert_eq!(
        (&target.0.kind, target.0.mode, target.1.as_slice()),
        (&LayerKind::File { size: 2 }, 0o600, &b"t\n"[..])
    );
    apply(&mut tree, &l3);
    assert_eq!(read(&r, &["proc", "sys", "dev"]), tree);
    // 하드 링크는 h1이 지워져도 h2에 남는다.
    assert_eq!(
        tree.get("h2"),
        Some(&Node::File(0o644, b"same inode\n".to_vec()))
    );
}

#[test]
fn privileged_a_busy_layer_is_not_detached_and_blocks_requests_until_reopened() {
    let Some(_serial) = privileged() else {
        return;
    };
    let s = Scratch::new("busy");
    let o = s.overlay(base);
    assert_eq!(o.open().unwrap(), 1);
    let held = fs::File::open(s.root().join("etc/hosts")).unwrap();
    let e = o.open().unwrap_err();
    assert!(e.message.contains("still in use"), "{e:?}");
    assert!(o.check_ready().is_err());
    let buf = Buf::default();
    assert!(o.export(&Out::new(buf)).is_err());
    drop(held);
    // 다시 열면 남은 마운트부터 내리고 같은 번호로 연다.
    assert_eq!(o.open().unwrap(), 2);
    assert!(o.check_ready().is_ok());
    assert!(s.root().join("etc/hosts").is_file());
}

#[test]
fn privileged_layers_stack_up_to_the_kernel_limit() {
    let Some(_serial) = privileged() else {
        return;
    };
    let s = Scratch::new("limit");
    let o = s.overlay(base);
    for n in 1..=sbx_guest_proto::MAX_LAYERS {
        assert_eq!(o.open().map_err(|e| e.message), Ok(n));
        if n == 1 || n == sbx_guest_proto::MAX_LAYERS {
            fs::write(s.root().join(format!("f{n}")), "x").unwrap();
        }
    }
    // 층 500은 아래층 500개(rootfs와 앞 층 499개)를 본다.
    assert!(s.root().join("f1").is_file() && s.root().join("etc/hosts").is_file());
    let e = o.open().unwrap_err();
    assert!(e.message.contains("at most"), "{e:?}");
    // 넘쳐도 지금 층은 그대로 쓴다.
    assert!(o.check_ready().is_ok());
    let names: Vec<String> = export(&o).into_iter().map(|(e, _)| e.path).collect();
    assert_eq!(names, vec![format!("f{}", sbx_guest_proto::MAX_LAYERS)]);
}

/// 연결 처리 프로세스처럼 뿌리를 층 안으로 옮긴다(`enter: true`). 뿌리는 프로세스 전체의 것이므로 이 테스트
/// 실행 파일을 자식으로 다시 띄워 그 안에서 한다([`enter_child`]).
#[test]
fn privileged_the_agent_enters_each_layer_and_exports_through_its_fd() {
    let Some(_serial) = privileged() else {
        return;
    };
    let s = Scratch::new("enter");
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "overlay::tests::enter_child", "--nocapture"])
        .env("SBX_OVERLAY_CHILD", s.layers())
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success() && text.contains("child ok"), "{text}");
    // 자식이 쓴 것은 층 디렉터리에 남는다(뿌리 밖에서 본 모습).
    assert_eq!(
        fs::read_to_string(s.layers().join("1/made")).unwrap(),
        "in 1"
    );
    // 커널이 쓴 whiteout은 문자 장치 0/0이다.
    use std::os::unix::fs::FileTypeExt;
    let wh = s.layers().join("2/made").symlink_metadata().unwrap();
    assert!(wh.file_type().is_char_device() && wh.rdev() == 0);
}

/// [`privileged_the_agent_enters_each_layer_and_exports_through_its_fd`]의 자식. 그 밖에서는 아무것도 하지 않는다.
#[test]
fn enter_child() {
    let Some(dir) = std::env::var_os("SBX_OVERLAY_CHILD") else {
        return;
    };
    let o = Overlay::new(
        PathBuf::from(dir),
        true,
        Box::new(|dir| {
            fresh_dir(dir)?;
            fs::create_dir(dir.join("lower"))?;
            base(&dir.join("lower"));
            Ok(())
        }),
    );
    assert_eq!(o.open().unwrap(), 1);
    // 이제 `/`가 층 1이다. 이미지 rootfs와 /proc가 보인다.
    assert_eq!(fs::read_to_string("/etc/hosts").unwrap(), "base\n");
    assert!(Path::new("/proc/self").exists());
    fs::write("/made", "in 1").unwrap();
    let names: Vec<String> = export(&o).into_iter().map(|(e, _)| e.path).collect();
    assert_eq!(names, vec!["made".to_owned()]);
    // 층 2: 처음 뿌리로 돌아가 층 1을 내리고 층 2로 들어온다.
    assert_eq!(o.open().unwrap(), 2);
    assert_eq!(fs::read_to_string("/made").unwrap(), "in 1");
    fs::remove_file("/made").unwrap();
    let got = export(&o);
    assert_eq!(kinds(&got), vec![("made", &LayerKind::Whiteout)]);
    println!("child ok");
}
