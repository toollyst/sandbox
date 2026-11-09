use super::*;

use std::collections::HashMap;
use std::io::Cursor;

use crate::{ContentRef, EntryKind, LayerEntry, Meta};

/// 테스트용 내용 저장소.
#[derive(Default)]
struct Mem(HashMap<(usize, usize), Vec<u8>>);

impl ContentSource for Mem {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(Cursor::new(&self.0[&(c.layer, c.index)])))
    }
}

struct Fixture {
    entries: Vec<LayerEntry>,
    mem: Mem,
}

impl Fixture {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            mem: Mem::default(),
        }
    }

    fn meta(mode: u32) -> Meta {
        Meta {
            mode,
            uid: 1000,
            gid: 100,
            mtime: 1_700_000_000,
            xattrs: BTreeMap::new(),
        }
    }

    fn dir(&mut self, path: &str) -> &mut Self {
        self.entries.push(LayerEntry {
            path: path.into(),
            kind: EntryKind::Dir,
            meta: Self::meta(0o755),
        });
        self
    }

    fn file(&mut self, path: &str, data: &[u8]) -> &mut Self {
        let index = self.entries.len();
        self.mem.0.insert((0, index), data.to_vec());
        self.entries.push(LayerEntry {
            path: path.into(),
            kind: EntryKind::File(ContentRef {
                layer: 0,
                index,
                size: data.len() as u64,
            }),
            meta: Self::meta(0o644),
        });
        self
    }

    fn other(&mut self, path: &str, kind: EntryKind, mode: u32) -> &mut Self {
        self.entries.push(LayerEntry {
            path: path.into(),
            kind,
            meta: Self::meta(mode),
        });
        self
    }

    fn xattr(&mut self, name: &str, value: &[u8]) -> &mut Self {
        self.entries
            .last_mut()
            .unwrap()
            .meta
            .xattrs
            .insert(name.into(), value.to_vec());
        self
    }

    fn build(&self) -> Result<Vec<u8>, RootfsError> {
        let mut tree = MergedTree::new();
        tree.apply_layer(self.entries.clone())?;
        let mut out = Cursor::new(Vec::new());
        ErofsBuilder.build(&tree, &self.mem, &mut out)?;
        Ok(out.into_inner())
    }
}

/// 검증용 최소 reader. 커널과 같은 방식으로 읽는다.
struct Img<'a>(&'a [u8]);

#[derive(Debug)]
struct RInode {
    nid: u64,
    mode: u16,
    size: u64,
    i_u: u32,
    uid: u32,
    gid: u32,
    mtime: u64,
    nlink: u32,
    xattrs: Vec<u8>,
}

impl Img<'_> {
    fn u16(&self, at: usize) -> u16 {
        u16::from_le_bytes(self.0[at..at + 2].try_into().unwrap())
    }
    fn u32(&self, at: usize) -> u32 {
        u32::from_le_bytes(self.0[at..at + 4].try_into().unwrap())
    }
    fn u64(&self, at: usize) -> u64 {
        u64::from_le_bytes(self.0[at..at + 8].try_into().unwrap())
    }

    fn root(&self) -> u64 {
        assert_eq!(self.u32(1024), MAGIC);
        assert_eq!(self.0[1024 + 12], 12);
        assert_eq!(self.u32(1024 + 36) as usize * 4096, self.0.len());
        u64::from(self.u16(1024 + 14))
    }

    fn inode(&self, nid: u64) -> RInode {
        let at = (nid * 32) as usize;
        assert_eq!(self.u16(at), 1, "extended FLAT_PLAIN inode");
        let icount = self.u16(at + 2) as usize;
        let xsize = if icount == 0 {
            0
        } else {
            12 + (icount - 1) * 4
        };
        RInode {
            nid,
            mode: self.u16(at + 4),
            size: self.u64(at + 8),
            i_u: self.u32(at + 16),
            uid: self.u32(at + 24),
            gid: self.u32(at + 28),
            mtime: self.u64(at + 32),
            nlink: self.u32(at + 44),
            xattrs: self.0[at + 64..at + 64 + xsize].to_vec(),
        }
    }

    fn data(&self, i: &RInode) -> &[u8] {
        let start = i.i_u as usize * 4096;
        &self.0[start..start + i.size as usize]
    }

    fn dir(&self, i: &RInode) -> Vec<(String, u64, u8)> {
        assert_eq!(u32::from(i.mode) & 0o170000, S_IFDIR);
        let data = self.data(i);
        let mut out = Vec::new();
        for chunk in data.chunks(4096) {
            let n = u16::from_le_bytes([chunk[8], chunk[9]]) as usize / 12;
            for k in 0..n {
                let d = &chunk[k * 12..k * 12 + 12];
                let nid = u64::from_le_bytes(d[0..8].try_into().unwrap());
                let off = u16::from_le_bytes([d[8], d[9]]) as usize;
                let end = if k + 1 < n {
                    u16::from_le_bytes([chunk[k * 12 + 20], chunk[k * 12 + 21]]) as usize
                } else {
                    chunk.len()
                };
                let name = String::from_utf8(chunk[off..end].to_vec()).unwrap();
                out.push((name.trim_end_matches('\0').to_owned(), nid, d[10]));
            }
        }
        out
    }

    fn lookup(&self, path: &str) -> RInode {
        let mut cur = self.inode(self.root());
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let (_, nid, _) = self
                .dir(&cur)
                .into_iter()
                .find(|(n, _, _)| n == part)
                .unwrap_or_else(|| panic!("{part} not found in {path}"));
            cur = self.inode(nid);
        }
        cur
    }
}

fn sample() -> Fixture {
    let mut f = Fixture::new();
    f.dir("bin")
        .file("bin/sh", b"#!shell")
        .xattr("security.capability", b"\x01\x00\x00\x02")
        .other("bin/ash", EntryKind::Hardlink("bin/sh".into()), 0)
        .other("bin/link", EntryKind::Symlink("sh".into()), 0o777)
        .dir("dev")
        .other(
            "dev/null",
            EntryKind::CharDevice { major: 1, minor: 3 },
            0o666,
        )
        .other("dev/fifo", EntryKind::Fifo, 0o600)
        .dir("etc")
        .file("etc/empty", b"")
        .file("etc/Hosts", b"upper")
        .file("etc/hosts", b"127.0.0.1 localhost\n")
        .dir("etc/sub");
    f
}

#[test]
fn superblock_and_root() {
    let bytes = sample().build().unwrap();
    let img = Img(&bytes);
    assert_eq!(img.root(), 36);
    let root = img.inode(36);
    assert_eq!(u32::from(root.mode) & 0o170000, S_IFDIR);
    let names: Vec<String> = img.dir(&root).into_iter().map(|(n, _, _)| n).collect();
    assert_eq!(names, vec![".", "..", "bin", "dev", "etc"]);
    // 루트의 `..`는 자기 자신.
    assert!(
        img.dir(&root)
            .iter()
            .all(|(n, nid, _)| n != ".." || *nid == 36)
    );
    // 루트 nlink = 2 + 하위 디렉터리 3.
    assert_eq!(root.nlink, 5);
    // inode 개수: 루트 1, bin/{sh,link} 3(ash는 sh와 같은 inode), dev/{null,fifo} 3, etc/{empty,Hosts,hosts,sub} 5.
    let total = 1 + 3 + 3 + 5;
    assert_eq!(img.u64(1024 + 16), total);
}

#[test]
fn files_symlinks_devices_and_metadata() {
    let bytes = sample().build().unwrap();
    let img = Img(&bytes);

    let hosts = img.lookup("etc/hosts");
    assert_eq!(img.data(&hosts), b"127.0.0.1 localhost\n");
    assert_eq!(u32::from(hosts.mode), S_IFREG | 0o644);
    assert_eq!(
        (hosts.uid, hosts.gid, hosts.mtime),
        (1000, 100, 1_700_000_000)
    );
    assert_eq!(img.data(&img.lookup("etc/Hosts")), b"upper");

    let empty = img.lookup("etc/empty");
    assert_eq!((empty.size, empty.i_u), (0, NULL_ADDR));

    let link = img.lookup("bin/link");
    assert_eq!(u32::from(link.mode) & 0o170000, S_IFLNK);
    assert_eq!(img.data(&link), b"sh");

    let null = img.lookup("dev/null");
    assert_eq!(u32::from(null.mode), S_IFCHR | 0o666);
    assert_eq!(null.i_u, (1 << 8) | 3);
    assert_eq!(u32::from(img.lookup("dev/fifo").mode) & 0o170000, S_IFIFO);

    let etc = img.lookup("etc");
    assert_eq!(etc.nlink, 3);
    let types: Vec<(String, u8)> = img.dir(&etc).into_iter().map(|(n, _, t)| (n, t)).collect();
    assert!(types.contains(&("sub".into(), FT_DIR)));
    assert!(types.contains(&("hosts".into(), FT_REG)));
}

#[test]
fn hardlinks_share_one_inode() {
    let bytes = sample().build().unwrap();
    let img = Img(&bytes);
    let sh = img.lookup("bin/sh");
    let ash = img.lookup("bin/ash");
    assert_eq!(sh.nid, ash.nid);
    assert_eq!(sh.nlink, 2);
}

#[test]
fn inline_xattr_layout() {
    let bytes = sample().build().unwrap();
    let img = Img(&bytes);
    let x = img.lookup("bin/sh").xattrs;
    // 헤더 12바이트 + 항목(4 + "capability" 10 + 값 4 = 18 → 20으로 정렬).
    assert_eq!(x.len(), 32);
    assert_eq!((x[12], x[13]), (10, 6));
    assert_eq!(u16::from_le_bytes([x[14], x[15]]), 4);
    assert_eq!(&x[16..26], b"capability");
    assert_eq!(&x[26..30], b"\x01\x00\x00\x02");
}

#[test]
fn large_directory_spans_blocks_in_sorted_order() {
    let mut f = Fixture::new();
    f.dir("d");
    let mut expected = Vec::new();
    for i in 0..600 {
        let name = format!("file-with-a-fairly-long-name-{i:04}");
        f.file(&format!("d/{name}"), name.as_bytes());
        expected.push(name);
    }
    let bytes = f.build().unwrap();
    let img = Img(&bytes);
    let d = img.lookup("d");
    assert!(d.size > 4096 * 3, "directory should span several blocks");
    let names: Vec<String> = img
        .dir(&d)
        .into_iter()
        .map(|(n, _, _)| n)
        .filter(|n| n != "." && n != "..")
        .collect();
    assert_eq!(names, expected);
    let last = img.lookup("d/file-with-a-fairly-long-name-0599");
    assert_eq!(img.data(&last), b"file-with-a-fairly-long-name-0599");
}

#[test]
fn output_is_deterministic() {
    assert_eq!(sample().build().unwrap(), sample().build().unwrap());
}

#[test]
fn unrepresentable_inputs_are_errors() {
    let mut f = Fixture::new();
    f.file("a", b"x").xattr("unknown.ns", b"v");
    assert!(matches!(f.build(), Err(RootfsError::Unsupported(_))));

    let mut f = Fixture::new();
    f.file(&"n".repeat(256), b"x");
    assert!(matches!(f.build(), Err(RootfsError::Unsupported(_))));
}

#[test]
fn short_content_is_an_error() {
    let mut f = Fixture::new();
    f.file("a", b"abc");
    f.mem.0.insert((0, 0), b"ab".to_vec());
    assert!(matches!(f.build(), Err(RootfsError::Io(_))));
}

/// erofs 공식 도구로 검증한다. `fsck.erofs`가 PATH에 있으면 돌고, 없으면 건너뛴다.
/// `SANDBOX_REQUIRE_FSCK_EROFS=1`이면 도구가 없을 때 실패한다(CI의 Linux 작업).
#[test]
fn fsck_erofs_accepts_and_extracts_image() {
    let found = std::process::Command::new("fsck.erofs")
        .arg("--help")
        .output()
        .is_ok();
    if !found {
        assert!(
            std::env::var_os("SANDBOX_REQUIRE_FSCK_EROFS").is_none(),
            "fsck.erofs is required but not found"
        );
        eprintln!("fsck.erofs not found; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sandbox-erofs-fsck-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("rootfs.erofs");
    std::fs::write(&img, sample().build().unwrap()).unwrap();
    let out_dir = dir.join("out");
    let out = std::process::Command::new("fsck.erofs")
        .arg(format!("--extract={}", out_dir.display()))
        .arg("--no-preserve")
        .arg(&img)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fsck.erofs failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read(out_dir.join("etc/hosts")).unwrap(),
        b"127.0.0.1 localhost\n"
    );
    assert_eq!(std::fs::read(out_dir.join("etc/Hosts")).unwrap(), b"upper");
    assert_eq!(
        std::fs::read_link(out_dir.join("bin/link")).unwrap(),
        std::path::Path::new("sh")
    );
    std::fs::remove_dir_all(dir).unwrap();
}
