//! 실제 커널로 본다: 층마다 쓴 EROFS를 loop로 마운트해 overlay 아래층으로 쌓은 모습이, 모든 층을 합친 EROFS 하나를
//! 아래층으로 쓴 모습(0.2까지의 rootfs)과 같은지. 게스트 부팅(`sbx-guest`의 `boot`)과 같은 overlay 옵션을 쓴다.
//!
//! root, loop 장치, 커널의 erofs(xattr 포함)와 overlay가 필요하므로 `SANDBOX_PRIVILEGED_TESTS=1`일 때만 돈다
//! (CI는 privileged 컨테이너에서 `cargo test -p sbx-rootfs --all-features --lib privileged`로 돌린다).

use std::collections::BTreeMap;
use std::io::{self, Cursor, Read};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::erofs::ErofsBuilder;
use crate::{ContentSource, RootfsBuilder};

/// 파일 내용: 위치마다 다른 한 바이트(층 항목의 크기는 1이다).
struct OneByte;

impl ContentSource for OneByte {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        let b = b'a' + ((c.layer * 7 + c.index) % 26) as u8;
        Ok(Box::new(Cursor::new(vec![b; c.size as usize])))
    }
}

fn sh(script: &str) {
    let out = Command::new("sh").args(["-ec", script]).output().unwrap();
    assert!(
        out.status.success(),
        "{script}\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 마운트한 것들. 버리면 거꾸로 내리고 지운다.
struct Mounts {
    dir: PathBuf,
    mounted: Vec<PathBuf>,
}

impl Mounts {
    fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("sbx-lower-kernel-{name}-{}", std::process::id()));
        let _ = Command::new("umount")
            .arg("-R")
            .arg(&dir)
            .stderr(std::process::Stdio::null())
            .status();
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir,
            mounted: Vec::new(),
        }
    }

    fn mount(&mut self, args: &str, target: &Path) {
        std::fs::create_dir_all(target).unwrap();
        sh(&format!("mount {args} {}", target.display()));
        self.mounted.push(target.to_owned());
    }

    /// `disks`(아래부터)를 EROFS로 써서 쌓는다. 위층은 tmpfs(부팅의 scratch 자리)다. 본 뿌리.
    fn stack(&mut self, disks: &[&MergedTree]) -> PathBuf {
        let mut lowers = Vec::new();
        for (i, tree) in disks.iter().enumerate() {
            let img = self.dir.join(format!("{i}.erofs"));
            let mut out = Cursor::new(Vec::new());
            ErofsBuilder.build(tree, &OneByte, &mut out).unwrap();
            std::fs::write(&img, out.into_inner()).unwrap();
            let at = self.dir.join(format!("lower/{i}"));
            self.mount(&format!("-t erofs -o loop,ro {}", img.display()), &at);
            lowers.push(at.display().to_string());
        }
        lowers.reverse();
        let up = self.dir.join("up");
        self.mount("-t tmpfs tmpfs", &up);
        std::fs::create_dir_all(up.join("upper")).unwrap();
        std::fs::create_dir_all(up.join("work")).unwrap();
        let root = self.dir.join("root");
        self.mount(
            &format!(
                "-t overlay overlay -o lowerdir={},upperdir={},workdir={}",
                lowers.join(":"),
                up.join("upper").display(),
                up.join("work").display()
            ),
            &root,
        );
        root
    }
}

impl Drop for Mounts {
    fn drop(&mut self) {
        for m in self.mounted.iter().rev() {
            let _ = Command::new("umount").arg(m).status();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 본 모습: 경로 → 종류·권한·소유자·시각·내용(파일), 대상(링크), 장치 번호, 하드 링크 묶음의 대표 경로.
/// 목록에는 있지만 열리지 않는 이름(아래층에 없는 경로의 whiteout, 모듈 문서의 알려진 차이)은 따로 센다.
fn walk(root: &Path) -> (BTreeMap<String, String>, usize) {
    fn go(
        root: &Path,
        rel: &str,
        out: &mut BTreeMap<String, String>,
        inodes: &mut BTreeMap<(u64, u64), String>,
        hidden: &mut usize,
    ) {
        let mut names: Vec<_> = std::fs::read_dir(root.join(rel))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        for name in names {
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let full = root.join(&path);
            let m = match std::fs::symlink_metadata(&full) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    *hidden += 1;
                    continue;
                }
                Err(e) => panic!("{path}: {e}"),
            };
            let t = m.file_type();
            let what = if t.is_dir() {
                "dir".to_owned()
            } else if t.is_symlink() {
                format!("link {}", std::fs::read_link(&full).unwrap().display())
            } else if t.is_file() {
                let rep = inodes
                    .entry((m.dev(), m.ino()))
                    .or_insert_with(|| path.clone())
                    .clone();
                format!("file {:?} {rep}", std::fs::read(&full).unwrap())
            } else if t.is_char_device() {
                format!("char {:x}", m.rdev())
            } else if t.is_block_device() {
                format!("block {:x}", m.rdev())
            } else {
                "fifo".to_owned()
            };
            out.insert(
                path.clone(),
                format!(
                    "{what} {:o} {}:{} {}",
                    m.mode() & 0o7777,
                    m.uid(),
                    m.gid(),
                    m.mtime()
                ),
            );
            if t.is_dir() {
                go(root, &path, out, inodes, hidden);
            }
        }
    }
    let mut out = BTreeMap::new();
    let mut hidden = 0;
    go(root, "", &mut out, &mut BTreeMap::new(), &mut hidden);
    (out, hidden)
}

/// 층을 규칙대로(혼자 설 수 없는 층까지는 합치고 그 위는 층마다 하나) 쌓은 모습과 모두 합친 모습.
fn both(
    name: &str,
    layers: &[Vec<LayerEntry>],
) -> ((BTreeMap<String, String>, usize), BTreeMap<String, String>) {
    let lowers: Vec<LowerLayer> = layers
        .iter()
        .map(|l| lower_layer(l.clone()).unwrap())
        .collect();
    let highest = lowers.iter().rposition(|l| l.needs_lower.is_some());
    let mut disks: Vec<MergedTree> = Vec::new();
    let from = match highest {
        Some(h) => {
            disks.push(merged(&layers[..=h]).unwrap());
            h + 1
        }
        None => 0,
    };
    disks.extend(lowers[from..].iter().map(|l| l.tree.clone()));
    let mut stacked = Mounts::new(&format!("{name}-stack"));
    let refs: Vec<&MergedTree> = disks.iter().collect();
    let got = walk(&stacked.stack(&refs));
    let all = merged(layers).unwrap();
    let mut oracle = Mounts::new(&format!("{name}-merged"));
    // 합친 rootfs에도 tar에 있던 문자 장치 0/0은 목록에만 보이고 열리지 않는다(0.2부터 그랬다).
    let (want, _) = walk(&oracle.stack(&[&all]));
    drop((stacked, oracle));
    (got, want)
}

fn privileged() -> bool {
    if std::env::var_os("SANDBOX_PRIVILEGED_TESTS").is_none() {
        eprintln!("SANDBOX_PRIVILEGED_TESTS not set, skipping");
        return false;
    }
    true
}

#[test]
fn privileged_kernel_stacks_layer_disks_like_the_merged_rootfs() {
    if !privileged() {
        return;
    }
    let mut d = dir("etc");
    d.meta.mode = 0o750;
    let layers = vec![
        vec![
            dir("etc"),
            file("etc/hosts", 0, 0),
            file("etc/gone", 0, 1),
            dir("var"),
            dir("var/cache"),
            file("var/cache/old", 0, 2),
            dir("opt"),
            file("opt/x", 0, 3),
            link("opt/x2", "opt/x"),
            dir("swap"),
            file("swap/f", 0, 4),
            file("tofile", 0, 5),
        ],
        vec![
            d,
            file("etc/hosts", 1, 0),
            mark("etc/.wh.gone"),
            dir("var"),
            dir("var/cache"),
            mark("var/cache/.wh..wh..opq"),
            file("var/cache/new", 1, 1),
            mark(".wh.opt"),
            file("swap", 1, 2),
            dir("tofile"),
            file("tofile/inside", 1, 3),
        ],
        vec![
            mark(".wh.swap"),
            dir("swap"),
            file("swap/again", 2, 0),
            entry("dev0", EntryKind::CharDevice { major: 0, minor: 0 }, 0),
            entry("fifo", EntryKind::Fifo, 0o600),
        ],
    ];
    assert!(
        layers
            .iter()
            .all(|l| lower_layer(l.clone()).unwrap().needs_lower.is_none())
    );
    let ((got, hidden), want) = both("fixed", &layers);
    assert_eq!(got, want);
    assert_eq!(hidden, 0);
}

#[test]
fn privileged_kernel_stacks_random_layers_like_the_merged_rootfs() {
    if !privileged() {
        return;
    }
    let mut r = Rng(0x6b65_726e_656c_0001);
    let (mut checked, mut hidden_total) = (0, 0);
    while checked < 150 {
        let n = 2 + r.below(3) as usize;
        let mut files = Vec::new();
        let layers: Vec<Vec<LayerEntry>> = (0..n)
            .map(|l| random_layer(&mut r, l, &mut files))
            .collect();
        if merged(&layers).is_err() {
            continue;
        }
        let ((got, hidden), want) = both(&format!("r{checked}"), &layers);
        assert_eq!(got, want, "{}", brief(&layers));
        hidden_total += hidden;
        checked += 1;
    }
    eprintln!(
        "{checked} random cases; {hidden_total} listed-but-hidden whiteouts (the known overlayfs difference)"
    );
}
