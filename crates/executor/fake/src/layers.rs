//! 빌드 층 흉내 (`OpenLayer`, `ExportLayer`). overlay 대신 디렉터리 하나를 두 번 훑어 견준다.
//!
//! - 층의 뿌리는 호스트 디렉터리 하나다([`crate::build_root`]). 경로는 여전히 호스트 경로라서, 테스트는 그 아래
//!   경로로 `Exec`, `CopyIn`을 보낸다. 처음 층을 열기 전에 그 디렉터리에 둔 것이 "이미지 rootfs"다.
//! - `OpenLayer`는 지금 모습을 적어 둔다(경로마다 종류, 권한, 소유자, 수정·변경 시각, 크기, inode).
//!   `ExportLayer`는 다시 훑어 견주고, overlay가 위층에 남겼을 것을 보낸다.
//!   - 새로 생기거나 적어 둔 것과 다른 항목, 그리고 그 위 디렉터리 전부.
//!   - 지워진 항목은 맨 위 것만 whiteout으로(부모가 그대로 있는 디렉터리일 때). 지우고 같은 이름으로 다시 만든
//!     디렉터리는 opaque이고(overlay와 같다), 그 안은 모두 보낸다.
//!   - 같은 디렉터리인지는 inode로 가린다. 파일 시스템은 지운 inode 번호를 곧 다시 쓰므로(ext4가 그렇다, APFS는
//!     아니다) `OpenLayer`가 디렉터리를 열어 두어(처음 [`MAX_HELD`]개) 그 번호가 층이 열린 동안 다시 쓰이지 않게
//!     한다. 그 밖의 것은 만든 시각도 함께 견준다(파일 시스템이 알려 줄 때). 호스트 OS에 따라 같은 변경이 다른 층이
//!     되지 않게.
//!   - 같은 inode의 일반 파일은 처음 것만 내용과 함께, 나머지는 하드 링크로.
//! - 실제 overlay와 다른 점: 아래층 파일의 하드 링크는 끊어지지 않는다(overlay는 복사할 때 끊는다). xattr은
//!   보내지 않는다. 소켓은 건너뛴다.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sbx_guest::layer::Layers;
use sbx_guest::wire::{Out, io_error, send_file_data};
use sbx_guest_proto::{Error, ErrorKind, GuestMsg, LayerEntry, LayerKind, MAX_LAYERS};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Symlink(String),
    Fifo,
    Char(u64),
    Block(u64),
    Socket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Node {
    kind: Kind,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: (i64, i64),
    ctime: (i64, i64),
    size: u64,
    dev: u64,
    ino: u64,
    /// 만든 시각(파일 시스템이 알려 줄 때). inode 번호가 다시 쓰였을 때 다른 것임을 안다.
    born: Option<std::time::SystemTime>,
    nlink: u64,
}

type Snap = BTreeMap<String, Node>;

fn snapshot(root: &Path) -> io::Result<Snap> {
    fn walk(dir: &Path, rel: &str, out: &mut Snap) -> io::Result<()> {
        for c in fs::read_dir(dir)? {
            let c = c?;
            let name = c.file_name();
            let name = name.to_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("name is not UTF-8: {name:?}"),
                )
            })?;
            let path = if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            let m = fs::symlink_metadata(c.path())?;
            let ft = m.file_type();
            let kind = if ft.is_dir() {
                Kind::Dir
            } else if ft.is_symlink() {
                let t = fs::read_link(c.path())?;
                Kind::Symlink(t.to_string_lossy().into_owned())
            } else if ft.is_file() {
                Kind::File
            } else if ft.is_fifo() {
                Kind::Fifo
            } else if ft.is_char_device() {
                Kind::Char(m.rdev())
            } else if ft.is_block_device() {
                Kind::Block(m.rdev())
            } else {
                Kind::Socket
            };
            out.insert(
                path.clone(),
                Node {
                    kind: kind.clone(),
                    mode: m.mode() & 0o7777,
                    uid: m.uid(),
                    gid: m.gid(),
                    mtime: (m.mtime(), m.mtime_nsec()),
                    ctime: (m.ctime(), m.ctime_nsec()),
                    size: m.len(),
                    dev: m.dev(),
                    ino: m.ino(),
                    born: m.created().ok(),
                    nlink: m.nlink(),
                },
            );
            if kind == Kind::Dir {
                walk(&c.path(), &path, out)?;
            }
        }
        Ok(())
    }
    let mut out = Snap::new();
    walk(root, "", &mut out)?;
    Ok(out)
}

fn parent(p: &str) -> Option<&str> {
    p.rsplit_once('/').map(|(a, _)| a)
}

fn ancestors(p: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(parent(p), |a| parent(a))
}

/// 디렉터리 하나를 층의 뿌리로 쓰는 흉내.
pub(crate) struct DirLayers {
    root: PathBuf,
    /// 받은 층 디스크. 첫 층을 열 때 뿌리가 없으면 펼친다([`crate::rootdir::populate`]).
    disks: Vec<PathBuf>,
    /// 층을 열었다. 그 뒤로 연결의 경로를 뿌리 아래로 옮긴다([`crate::rootdir::rebase`]).
    opened: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// (마지막으로 연 층 번호, 열 때의 모습, 그때 있던 디렉터리를 열어 둔 것).
    state: Mutex<(u32, Option<Snap>, Vec<fs::File>)>,
}

impl DirLayers {
    pub(crate) fn new(
        root: PathBuf,
        disks: Vec<PathBuf>,
        opened: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            root,
            disks,
            opened,
            state: Mutex::new((0, None, Vec::new())),
        }
    }
}

/// 층이 열린 동안 열어 두는 디렉터리 수의 상한. 파일 디스크립터 한도(macOS 기본 256)를 다 쓰지 않게.
const MAX_HELD: usize = 128;

/// 모습에 있는 디렉터리를 연다. 연 동안은 지워져도 inode가 남아 그 번호를 새 디렉터리가 받지 않는다. 상한을 넘거나
/// 열 수 없으면 거기서 그친다(만든 시각으로 가린다).
fn hold_dirs(root: &Path, snap: &Snap) -> Vec<fs::File> {
    snap.iter()
        .filter(|(_, n)| n.kind == Kind::Dir)
        .take(MAX_HELD)
        .map_while(|(p, _)| fs::File::open(root.join(p)).ok())
        .collect()
}

/// 지우고 다시 만든 것이 아니라 같은 디렉터리다.
fn same_object(b: &Node, n: &Node) -> bool {
    b.dev == n.dev && b.ino == n.ino && b.born == n.born
}

fn io_err(what: &str, e: &io::Error) -> Error {
    io_error(what, e)
}

impl Layers for DirLayers {
    fn open(&self) -> Result<u32, Error> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.0 >= MAX_LAYERS {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!("a build can have at most {MAX_LAYERS} layers"),
            ));
        }
        if st.0 == 0 && !self.root.exists() {
            crate::rootdir::populate(&self.root, &self.disks)
                .map_err(|e| io_err("build root", &e))?;
        }
        fs::create_dir_all(&self.root).map_err(|e| io_err("build root", &e))?;
        // 앞 층의 것을 먼저 닫는다(한도 안에서 이번 층의 것을 연다).
        st.2.clear();
        let snap = snapshot(&self.root).map_err(|e| io_err("build root", &e))?;
        st.2 = hold_dirs(&self.root, &snap);
        self.opened.store(true, std::sync::atomic::Ordering::SeqCst);
        st.0 += 1;
        st.1 = Some(snap);
        Ok(st.0)
    }

    fn export(&self, out: &Out) -> Result<(), Error> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(before) = &st.1 else {
            return Err(Error::new(ErrorKind::InvalidArgument, "no layer is open"));
        };
        let now = snapshot(&self.root).map_err(|e| io_err("build root", &e))?;
        // 지우고 다시 만든 디렉터리.
        let replaced = |p: &str| {
            matches!((before.get(p), now.get(p)), (Some(b), Some(n))
                if b.kind == Kind::Dir && n.kind == Kind::Dir && !same_object(b, n))
        };
        // 그대로 있는 디렉터리(그 안의 지워진 항목에 whiteout이 필요하다).
        let same_dir = |p: &str| {
            matches!((before.get(p), now.get(p)), (Some(b), Some(n))
                if b.kind == Kind::Dir && n.kind == Kind::Dir && same_object(b, n))
        };
        let mut send: BTreeSet<String> = BTreeSet::new();
        for (p, n) in &now {
            let under_replaced = ancestors(p).any(replaced);
            if under_replaced || before.get(p) != Some(n) {
                send.insert(p.clone());
            }
        }
        let mut whiteouts: BTreeSet<String> = BTreeSet::new();
        for p in before.keys() {
            if now.contains_key(p) {
                continue;
            }
            match parent(p) {
                None => {
                    whiteouts.insert(p.clone());
                }
                Some(a) if same_dir(a) && !ancestors(p).any(replaced) => {
                    whiteouts.insert(p.clone());
                }
                Some(_) => {}
            }
        }
        for p in send.clone().iter().chain(whiteouts.iter()) {
            for a in ancestors(p) {
                send.insert(a.to_owned());
            }
        }
        let all: BTreeSet<&String> = send.iter().chain(whiteouts.iter()).collect();
        let mut links: HashMap<(u64, u64), String> = HashMap::new();
        for p in all {
            let entry = if whiteouts.contains(p) {
                LayerEntry {
                    path: p.clone(),
                    kind: LayerKind::Whiteout,
                    mode: 0,
                    uid: 0,
                    gid: 0,
                    mtime: 0,
                    xattrs: vec![],
                }
            } else {
                let n = &now[p];
                let kind = match &n.kind {
                    Kind::Dir => LayerKind::Dir {
                        opaque: replaced(p),
                    },
                    Kind::File => match links.get(&(n.dev, n.ino)) {
                        Some(first) => LayerKind::Hardlink {
                            target: first.clone(),
                        },
                        None => {
                            if n.nlink > 1 {
                                links.insert((n.dev, n.ino), p.clone());
                            }
                            LayerKind::File { size: n.size }
                        }
                    },
                    Kind::Symlink(t) => LayerKind::Symlink { target: t.clone() },
                    Kind::Fifo => LayerKind::Fifo,
                    Kind::Char(r) | Kind::Block(r) => {
                        let (major, minor) = split(*r);
                        if matches!(n.kind, Kind::Char(_)) {
                            LayerKind::CharDevice { major, minor }
                        } else {
                            LayerKind::BlockDevice { major, minor }
                        }
                    }
                    Kind::Socket => continue,
                };
                LayerEntry {
                    path: p.clone(),
                    kind,
                    mode: n.mode,
                    uid: n.uid,
                    gid: n.gid,
                    mtime: n.mtime.0,
                    xattrs: vec![],
                }
            };
            let size = match entry.kind {
                LayerKind::File { size } => Some(size),
                _ => None,
            };
            out.send(&GuestMsg::LayerEntry(entry))
                .map_err(|e| io_err("send", &e))?;
            if let Some(size) = size {
                send_file_data(out, &self.root.join(p), p, size)?;
            }
        }
        Ok(())
    }

    fn check_ready(&self) -> Result<(), Error> {
        Ok(())
    }

    fn is_open(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0 > 0
    }
}

/// 장치 번호를 (major, minor)로. 호스트 OS의 인코딩이다(Linux와 BSD 계열).
fn split(rdev: u64) -> (u32, u32) {
    if cfg!(target_os = "linux") {
        let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
        let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
        (major as u32, minor as u32)
    } else {
        (((rdev >> 24) & 0xff) as u32, (rdev & 0xff_ffff) as u32)
    }
}
