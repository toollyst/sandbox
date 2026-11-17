//! 쓴 디스크를 다시 읽어 합친 모습이 원래 층을 합친 모습과 같은지 본다.

use std::collections::BTreeMap;
use std::io::Cursor;

use super::*;
use crate::erofs::ErofsBuilder;
use crate::lower::lower_layer;
use crate::{MergedTree, NodeKind, RootfsBuilder};

/// 테스트용 내용: (층, 순번) → 바이트.
#[derive(Default)]
struct Mem(BTreeMap<(usize, usize), Vec<u8>>);

impl ContentSource for Mem {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(
            self.0
                .get(&(c.layer, c.index))
                .map(|v| v.as_slice())
                .unwrap_or_default(),
        ))
    }
}

fn meta(mode: u32) -> Meta {
    Meta {
        mode,
        uid: 1000,
        gid: 1001,
        mtime: 1_700_000_000,
        xattrs: BTreeMap::new(),
    }
}

struct Layer {
    layer: usize,
    entries: Vec<LayerEntry>,
}

impl Layer {
    fn new(layer: usize) -> Self {
        Self {
            layer,
            entries: Vec::new(),
        }
    }
    fn push(&mut self, path: &str, kind: EntryKind, mode: u32) -> &mut Self {
        self.entries.push(LayerEntry {
            path: path.into(),
            kind,
            meta: meta(mode),
        });
        self
    }
    fn dir(&mut self, path: &str) -> &mut Self {
        self.push(path, EntryKind::Dir, 0o755)
    }
    fn file(&mut self, path: &str, data: &[u8], mem: &mut Mem) -> &mut Self {
        let index = self.entries.len();
        mem.0.insert((self.layer, index), data.to_vec());
        self.push(
            path,
            EntryKind::File(ContentRef {
                layer: self.layer,
                index,
                size: data.len() as u64,
            }),
            0o644,
        )
    }
}

/// 층 하나를 층 디스크로 쓰고 다시 읽는다.
fn round_trip(entries: Vec<LayerEntry>, mem: &Mem, layer: usize) -> (ReadLayer, Vec<u8>) {
    let lower = lower_layer(entries).unwrap();
    assert!(lower.needs_lower.is_none(), "{:?}", lower.needs_lower);
    let mut disk = Cursor::new(Vec::new());
    ErofsBuilder.build(&lower.tree, mem, &mut disk).unwrap();
    let bytes = disk.into_inner();
    let read = read_layer(&mut Cursor::new(bytes.clone()), layer).unwrap();
    (read, bytes)
}

/// 비교할 모습: 경로마다 종류, 권한, 소유자, 시각, xattr, 내용.
fn view(tree: &MergedTree, content: &dyn ContentSource) -> BTreeMap<String, String> {
    tree.iter()
        .map(|(p, n)| {
            let kind = match &n.kind {
                NodeKind::File(c) => {
                    let mut b = Vec::new();
                    content.open(c).unwrap().read_to_end(&mut b).unwrap();
                    format!("file {:?}", String::from_utf8_lossy(&b))
                }
                k => format!("{k:?}"),
            };
            let m = &n.meta;
            (
                p.to_owned(),
                format!(
                    "{kind} {:o} {}:{} {} {:?} link={}",
                    m.mode,
                    m.uid,
                    m.gid,
                    m.mtime,
                    m.xattrs,
                    n.link_group.is_some()
                ),
            )
        })
        .collect()
}

struct Disk(Vec<u8>, HashMap<usize, u64>);

impl ContentSource for Disk {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        if c.size == 0 {
            return Ok(Box::new(io::empty()));
        }
        let at = self.1[&c.index] as usize;
        Ok(Box::new(&self.0[at..at + c.size as usize]))
    }
}

/// 내용을 층 번호로 나눠 고른다.
struct ByLayer<'a>(Vec<&'a dyn ContentSource>);

impl ContentSource for ByLayer<'_> {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        self.0[c.layer].open(c)
    }
}

#[test]
fn a_layer_disk_reads_back_as_the_layer_it_was_made_from() {
    let mut mem = Mem::default();
    // 층 0: 바탕.
    let mut base = Layer::new(0);
    base.dir("etc")
        .file("etc/hosts", b"base\n", &mut mem)
        .file("etc/keep", b"k\n", &mut mem)
        .dir("data")
        .dir("data/d1")
        .file("data/d1/old", b"old\n", &mut mem)
        .dir("lib")
        .file("lib/target", b"t\n", &mut mem);
    // 층 1: 바꾸기, whiteout, opaque, 하드 링크, 심볼릭 링크, 장치, FIFO, xattr, 큰 파일.
    let mut top = Layer::new(1);
    let big: Vec<u8> = (0..9000u32).map(|i| (i % 251) as u8).collect();
    top.dir("etc")
        .file("etc/hosts", b"changed\n", &mut mem)
        .push(
            "etc/.wh.keep",
            EntryKind::File(ContentRef {
                layer: 1,
                index: 99,
                size: 0,
            }),
            0,
        )
        .dir("data")
        .dir("data/d1")
        .push(
            "data/d1/.wh..wh..opq",
            EntryKind::File(ContentRef {
                layer: 1,
                index: 98,
                size: 0,
            }),
            0,
        )
        .file("data/d1/new", b"fresh\n", &mut mem)
        .dir("lib")
        .file("lib/h1", b"same\n", &mut mem)
        .push("lib/h2", EntryKind::Hardlink("lib/h1".into()), 0o644)
        .push("lib/link", EntryKind::Symlink("target".into()), 0o777)
        .push(
            "lib/null",
            EntryKind::CharDevice { major: 1, minor: 3 },
            0o666,
        )
        .push(
            "lib/loop",
            EntryKind::BlockDevice {
                major: 7,
                minor: 300,
            },
            0o660,
        )
        .push("lib/pipe", EntryKind::Fifo, 0o600)
        .file("lib/big", &big, &mut mem);
    top.entries
        .last_mut()
        .unwrap()
        .meta
        .xattrs
        .insert("user.note".into(), b"v".to_vec());

    let mut want = MergedTree::new();
    want.apply_layer(base.entries.clone()).unwrap();
    want.apply_layer(top.entries.clone()).unwrap();

    let (r0, d0) = round_trip(base.entries, &mem, 0);
    let (r1, d1) = round_trip(top.entries, &mem, 1);
    // whiteout과 opaque 표시가 OCI 이름으로 돌아온다.
    let names: Vec<&str> = r1.entries.iter().map(|e| e.path.as_str()).collect();
    assert!(names.contains(&"etc/.wh.keep"), "{names:?}");
    assert!(names.contains(&"data/d1/.wh..wh..opq"), "{names:?}");
    // 디렉터리는 그 안의 항목보다 먼저 온다.
    let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
    assert!(pos("data/d1") < pos("data/d1/new"));
    let mut got = MergedTree::new();
    got.apply_layer(r0.entries).unwrap();
    got.apply_layer(r1.entries).unwrap();
    let (c0, c1) = (Disk(d0, r0.offsets), Disk(d1, r1.offsets));
    let src = ByLayer(vec![&c0, &c1]);
    assert_eq!(view(&got, &src), view(&want, &mem));
}

#[test]
fn a_merged_disk_reads_back_as_the_whole_tree() {
    let mut mem = Mem::default();
    let mut l = Layer::new(0);
    l.dir("a")
        .file("a/x", b"x", &mut mem)
        .dir("a/b")
        .file("a/b/y", b"yy", &mut mem);
    let mut tree = MergedTree::new();
    tree.apply_layer(l.entries).unwrap();
    let mut disk = Cursor::new(Vec::new());
    ErofsBuilder.build(&tree, &mem, &mut disk).unwrap();
    let bytes = disk.into_inner();
    let r = read_layer(&mut Cursor::new(bytes.clone()), 0).unwrap();
    let mut got = MergedTree::new();
    got.apply_layer(r.entries).unwrap();
    assert_eq!(view(&got, &Disk(bytes, r.offsets)), view(&tree, &mem));
}

#[test]
fn files_on_disk_are_read_through_disk_contents() {
    let mut mem = Mem::default();
    let mut l = Layer::new(0);
    l.file("hello", b"hello\n", &mut mem);
    let (r, bytes) = round_trip(l.entries, &mem, 3);
    let dir = std::env::temp_dir().join(format!("sbx-erofs-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("layer.erofs");
    std::fs::write(&path, bytes).unwrap();
    let mut c = DiskContents::new();
    c.add(3, &path, r.offsets);
    let EntryKind::File(cref) = &r.entries[0].kind else {
        panic!("{:?}", r.entries[0]);
    };
    assert_eq!(cref.layer, 3);
    let mut s = String::new();
    c.open(cref).unwrap().read_to_string(&mut s).unwrap();
    assert_eq!(s, "hello\n");
    std::fs::remove_dir_all(&dir).unwrap();
}

/// 깨진 디스크는 오류다. 패닉하지 않는다.
#[test]
fn damaged_disks_are_errors_not_panics() {
    let mut mem = Mem::default();
    let mut l = Layer::new(0);
    l.dir("d")
        .file("d/f", b"data", &mut mem)
        .push("d/l", EntryKind::Symlink("f".into()), 0o777);
    let (_, bytes) = round_trip(l.entries, &mem, 0);
    assert!(read_layer(&mut Cursor::new(vec![0u8; 100]), 0).is_err());
    assert!(read_layer(&mut Cursor::new(bytes[..2000].to_vec()), 0).is_err());
    // 바이트 하나씩, 여러 값으로 바꿔 본다. 결과는 무엇이든(받아들이거나 오류) 패닉은 없어야 한다.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..3000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let mut b = bytes.clone();
        let at = (x as usize) % 8192.min(b.len());
        b[at] = (x >> 32) as u8;
        let _ = read_layer(&mut Cursor::new(b), 0);
    }
}

/// 크기 `n`인 내용(블록 경계가 어긋나면 드러나게 자리마다 다른 바이트).
fn pattern(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u32).wrapping_mul(2_654_435_761).to_le_bytes()[3] ^ seed)
        .collect()
}

/// 비교할 모습(내용은 바이트 그대로): 경로 → (종류, 권한, 소유자, 시각, xattr, 하드 링크인지, 내용).
fn exact(tree: &MergedTree, content: &dyn ContentSource) -> BTreeMap<String, (String, Vec<u8>)> {
    tree.iter()
        .map(|(p, n)| {
            let mut bytes = Vec::new();
            let kind = match &n.kind {
                NodeKind::File(c) => {
                    content.open(c).unwrap().read_to_end(&mut bytes).unwrap();
                    assert_eq!(bytes.len() as u64, c.size, "{p}");
                    "file".to_owned()
                }
                k => format!("{k:?}"),
            };
            let m = &n.meta;
            let head = format!(
                "{kind} {:o} {}:{} {} {:?} link={}",
                m.mode,
                m.uid,
                m.gid,
                m.mtime,
                m.xattrs,
                n.link_group.is_some()
            );
            (p.to_owned(), (head, bytes))
        })
        .collect()
}

/// 쓰고 다시 읽으면 같은 트리와 같은 바이트다. 모든 종류, 크기 경계(0, 1, BLOCK-1, BLOCK, BLOCK+1, 여러 블록,
/// 몇 MiB), 하드 링크(크기 0인 파일의 것도), 긴 이름(255바이트)과 긴 링크 대상, xattr, 깊은 디렉터리, 여러 블록에 걸친
/// 넓은 디렉터리, 빈 디렉터리. 크기 0인 파일은 블록 주소가 `NULL_ADDR`이다(패키지 설치가 늘 만든다:
/// `apk add`의 `lib/apk/db/lock`).
#[test]
fn every_kind_and_size_boundary_round_trips() {
    let b = BLOCK as usize;
    let mut mem = Mem::default();
    let mut l = Layer::new(0);
    l.dir("s");
    for (k, n) in [0, 1, b - 1, b, b + 1, 3 * b, 3 * b + 17, (3 << 20) + 5]
        .into_iter()
        .enumerate()
    {
        l.file(&format!("s/f{n}"), &pattern(n, k as u8), &mut mem);
    }
    l.dir("h")
        .file("h/empty", b"", &mut mem)
        .push(
            "h/empty-again",
            EntryKind::Hardlink("h/empty".into()),
            0o644,
        )
        .file("h/big", &pattern(2 * b + 3, 9), &mut mem)
        .push("h/big-again", EntryKind::Hardlink("h/big".into()), 0o644)
        .push("h/big-third", EntryKind::Hardlink("h/big".into()), 0o644);
    l.dir("l")
        .push("l/short", EntryKind::Symlink("../s/f1".into()), 0o777)
        .push("l/long", EntryKind::Symlink("x/".repeat(1500)), 0o777)
        .push(
            "l/abs",
            EntryKind::Symlink("/usr/lib/libpq.so.5".into()),
            0o777,
        )
        .push(
            "l/null",
            EntryKind::CharDevice { major: 1, minor: 3 },
            0o666,
        )
        .push(
            "l/disk",
            EntryKind::BlockDevice {
                major: 259,
                minor: 70_000,
            },
            0o660,
        )
        .push("l/fifo", EntryKind::Fifo, 0o600);
    let long = format!("n/{}", "a".repeat(255));
    l.dir("n").file(&long, b"long name", &mut mem).dir("empty");
    let mut deep = String::from("d");
    l.dir(&deep);
    for i in 0..60 {
        deep.push_str(&format!("/{i}"));
        l.dir(&deep);
    }
    l.file(&format!("{deep}/leaf"), b"deep", &mut mem);
    l.dir("w");
    for i in 0..3000usize {
        l.file(
            &format!("w/lib-{i:05}.so.{}", i % 7),
            &pattern(i % 5, i as u8),
            &mut mem,
        );
    }
    for (path, name, value) in [
        ("s/f1", "user.a", &b"1"[..]),
        ("s/f4096", "security.capability", &[1, 0, 0, 2, 0, 0][..]),
        ("w", "user.dir", b"d"),
        ("n", "system.posix_acl_access", &[2, 0, 0, 0][..]),
    ] {
        let e = l.entries.iter_mut().find(|e| e.path == path).unwrap();
        e.meta.xattrs.insert(name.into(), value.to_vec());
    }

    let mut want = MergedTree::new();
    want.apply_layer(l.entries.clone()).unwrap();
    let (read, bytes) = round_trip(l.entries, &mem, 0);
    // 크기 0인 파일에는 디스크 안 자리가 없다.
    let empty = read.entries.iter().position(|e| e.path == "s/f0").unwrap();
    assert!(!read.offsets.contains_key(&empty));
    let mut got = MergedTree::new();
    got.apply_layer(read.entries).unwrap();
    let disk = Disk(bytes, read.offsets);
    assert_eq!(exact(&got, &disk), exact(&want, &mem));
}

/// 가장 작은 재현: 크기 0인 파일 하나. 그 inode의 블록 주소는 `NULL_ADDR`(0xffffffff)이고 읽는 쪽이 그것을
/// 데이터 자리로 보면 "data is outside the disk"였다.
#[test]
fn an_empty_file_reads_back() {
    let mut mem = Mem::default();
    let mut l = Layer::new(0);
    l.file("lock", b"", &mut mem);
    let (read, _) = round_trip(l.entries, &mem, 0);
    assert_eq!(read.entries.len(), 1);
    assert!(matches!(
        read.entries[0].kind,
        EntryKind::File(ContentRef { size: 0, .. })
    ));
}
