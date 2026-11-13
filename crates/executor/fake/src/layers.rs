//! 빌드 층 흉내 (`OpenLayer`, `ExportLayer`). overlay 대신 디렉터리 하나를 두 번 훑어 견준다.
//!
//! - 층의 뿌리는 호스트 디렉터리 하나다([`crate::build_root`]). 경로는 여전히 호스트 경로라서, 테스트는 그 아래
//!   경로로 `Exec`, `CopyIn`을 보낸다. 처음 층을 열기 전에 그 디렉터리에 둔 것이 "이미지 rootfs"다.
//! - `OpenLayer`는 지금 모습을 적어 둔다(경로마다 종류, 권한, 소유자, 수정·변경 시각, 크기, inode).
//!   `ExportLayer`는 다시 훑어 견주고, overlay가 위층에 남겼을 것을 보낸다.
//!   - 새로 생기거나 적어 둔 것과 다른 항목, 그리고 그 위 디렉터리 전부.
//!   - 지워진 항목은 맨 위 것만 whiteout으로(부모가 그대로 있는 디렉터리일 때). 지우고 같은 이름으로 다시 만든
//!     디렉터리(inode가 다르다)는 opaque이고, 그 안은 모두 보낸다.
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
    /// (마지막으로 연 층 번호, 열 때의 모습).
    state: Mutex<(u32, Option<Snap>)>,
}

impl DirLayers {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            state: Mutex::new((0, None)),
        }
    }
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
        fs::create_dir_all(&self.root).map_err(|e| io_err("build root", &e))?;
        let snap = snapshot(&self.root).map_err(|e| io_err("build root", &e))?;
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
                if b.kind == Kind::Dir && n.kind == Kind::Dir && b.ino != n.ino)
        };
        // 그대로 있는 디렉터리(그 안의 지워진 항목에 whiteout이 필요하다).
        let same_dir = |p: &str| {
            matches!((before.get(p), now.get(p)), (Some(b), Some(n))
                if b.kind == Kind::Dir && n.kind == Kind::Dir && b.ino == n.ino)
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
