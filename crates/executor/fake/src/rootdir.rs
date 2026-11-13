//! 빌드 VM 흉내의 뿌리와 경로 (Unix).
//!
//! - **뿌리 펼치기**([`populate`]): 첫 빌드 층을 열 때 뿌리 디렉터리가 없으면 받은 층 디스크(EROFS, 아래부터)를 합쳐
//!   그 디렉터리에 펼친다. 실제 게스트가 층 디스크를 overlay 아래층으로 쌓는 것의 흉내다. EROFS가 아닌 디스크(테스트가
//!   준 가짜 디스크)는 건너뛴다. 소유자는 지금 사용자로 남는다(호스트에서 root가 아니다). 장치, FIFO는 만들지 않는다.
//! - **경로 옮기기**([`rebase`]): 층이 열린 뒤에는 연결이 보내는 절대 경로(`Exec`의 작업 디렉터리, `CopyIn`의 대상,
//!   `CopyOut`, `Stat`)를 뿌리 디렉터리 아래로 옮긴다. 그래서 빌드 코어는 이미지 안의 경로(`/app`)를 그대로 쓴다.
//!   명령(argv)과 그 프로그램이 여는 경로는 옮기지 않는다(호스트에서 돈다). 테스트의 `RUN`은 상대 경로를 쓴다.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sbx_guest_proto::HostMsg;
use sbx_rootfs::erofs::read::{DiskContents, read_layer};
use sbx_rootfs::{ContentSource, MergedTree, NodeKind};

/// EROFS 슈퍼블록의 매직(1024바이트 자리).
const EROFS_MAGIC: u32 = 0xE0F5_E1E2;

fn is_erofs(path: &Path) -> bool {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut b = [0u8; 4];
    f.seek(SeekFrom::Start(1024)).is_ok()
        && f.read_exact(&mut b).is_ok()
        && u32::from_le_bytes(b) == EROFS_MAGIC
}

/// 층 디스크들을 합쳐 `root`(없어야 한다)에 펼친다.
pub(crate) fn populate(root: &Path, disks: &[PathBuf]) -> io::Result<()> {
    let mut tree = MergedTree::new();
    let mut contents = DiskContents::new();
    for (i, d) in disks.iter().enumerate().filter(|(_, d)| is_erofs(d)) {
        let r = read_layer(&mut io::BufReader::new(fs::File::open(d)?), i)
            .map_err(|e| io::Error::other(format!("{}: {e}", d.display())))?;
        tree.apply_layer(r.entries)
            .map_err(|e| io::Error::other(e.to_string()))?;
        contents.add(i, d, r.offsets);
    }
    fs::create_dir_all(root)?;
    let mut dirs = Vec::new();
    for (path, node) in tree.iter().filter(|(p, _)| !p.is_empty()) {
        let at = root.join(path);
        match &node.kind {
            NodeKind::Dir => {
                fs::create_dir_all(&at)?;
                dirs.push((at, node.meta.mode));
            }
            NodeKind::File(c) => {
                let mut src = contents.open(c)?;
                io::copy(&mut src, &mut fs::File::create(&at)?)?;
                set_mode(&at, node.meta.mode)?;
            }
            NodeKind::Symlink(t) => std::os::unix::fs::symlink(t, &at)?,
            _ => {}
        }
    }
    // 디렉터리 권한은 안쪽부터 끝에 둔다(쓰기 권한이 없는 디렉터리 안에도 먼저 쓴다). 지금 사용자가 쓸 수 있게 남긴다.
    for (at, mode) in dirs.into_iter().rev() {
        set_mode(&at, mode | 0o700)?;
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
}

fn under(root: &Path, path: &str) -> String {
    let rel = path.trim_start_matches('/');
    root.join(rel).to_string_lossy().into_owned()
}

/// 메시지의 절대 경로를 `root` 아래로 옮긴다.
pub(crate) fn rebase(root: &Path, msg: HostMsg) -> HostMsg {
    match msg {
        HostMsg::Exec(mut x) => {
            x.cwd = under(root, &x.cwd);
            HostMsg::Exec(x)
        }
        HostMsg::ExecStep(mut x) => {
            x.cwd = under(root, &x.cwd);
            HostMsg::ExecStep(x)
        }
        HostMsg::ExecDetached(mut x) => {
            x.cwd = under(root, &x.cwd);
            HostMsg::ExecDetached(x)
        }
        HostMsg::CopyIn { dest } => HostMsg::CopyIn {
            dest: under(root, &dest),
        },
        HostMsg::CopyOut { path } => HostMsg::CopyOut {
            path: under(root, &path),
        },
        HostMsg::Stat { path } => HostMsg::Stat {
            path: under(root, &path),
        },
        m => m,
    }
}

/// 읽을 때 메시지를 하나씩 풀어 경로를 옮기고 다시 묶는 연결. 쓰기는 그대로 같은 소켓에 한다.
pub(crate) struct Rebased {
    conn: UnixStream,
    /// 옮긴 메시지의 남은 바이트(복제본끼리 함께 쓴다. 읽는 쪽은 하나다).
    pending: Arc<Mutex<(Vec<u8>, usize)>>,
    root: Arc<PathBuf>,
    opened: Arc<AtomicBool>,
}

impl Rebased {
    pub(crate) fn new(conn: UnixStream, root: PathBuf, opened: Arc<AtomicBool>) -> Self {
        Self {
            conn,
            pending: Arc::default(),
            root: Arc::new(root),
            opened,
        }
    }
}

impl Read for Rebased {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut p = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        if p.1 >= p.0.len() {
            let m = match HostMsg::read_from(&mut self.conn) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
                Err(e) => return Err(e),
            };
            let m = if self.opened.load(Ordering::SeqCst) {
                rebase(&self.root, m)
            } else {
                m
            };
            p.0.clear();
            m.write_to(&mut p.0)?;
            p.1 = 0;
        }
        let n = buf.len().min(p.0.len() - p.1);
        buf[..n].copy_from_slice(&p.0[p.1..p.1 + n]);
        p.1 += n;
        Ok(n)
    }
}

impl Write for Rebased {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.conn.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush()
    }
}

impl sbx_guest::wire::Conn for Rebased {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            conn: self.conn.try_clone()?,
            pending: self.pending.clone(),
            root: self.root.clone(),
            opened: self.opened.clone(),
        })
    }
    fn shutdown_both(&self) -> io::Result<()> {
        self.conn.shutdown(std::net::Shutdown::Both)
    }
}
