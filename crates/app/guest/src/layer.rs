//! 빌드 층 (`HostMsg::OpenLayer`, `HostMsg::ExportLayer`).
//!
//! 층을 쌓는 방법은 [`Layers`] 뒤에 있다. 게스트에서는 overlay(`overlay` 모듈, Linux)이고, fake 실행기에서는 그
//! 흉내다. 이 모듈은 요청을 나눠 보내고, overlay 위층 디렉터리를 층 항목([`LayerEntry`])으로 바꾼다.
//!
//! **차례** (호스트가 지킨다)
//!
//! - 첫 `OpenLayer`가 빌드를 시작한다. 그 전에는 아무것도 바뀌지 않는다. 보통의 `run`은 층을 열지 않는다.
//! - `OpenLayer`가 끝난 뒤의 `Exec`, `ExecDetached`, `CopyIn`, `CopyOut`, `Stat`은 그 층으로 본 파일 시스템에서 한다.
//! - 층을 열고 내보내는 동안 호스트는 파일 시스템을 보는 다른 요청(`Exec`, `ExecDetached`, `ExecStep`, `CopyIn`,
//!   `CopyOut`, `Stat`, `Share`)을 보내지 않고, 앞 단계의 프로세스도 끝나 있어야 한다. 층을 바꾸는 것은 연결 처리
//!   프로세스 전체의 뿌리를 바꾸는 일이기 때문이다. 파일 시스템을 보지 않는 요청은 소유 프로세스의 다른 스레드가
//!   언제든 보낸다: 시계 맞추기(`ClockSync`, 커널 시계와 연결만 쓴다)와 나가는 연결(`EgressSlot`, 이름 묻기).
//!   그 처리는 경로를 풀지 않으므로 뿌리가 바뀌는 중에 와도 된다.
//! - 층 열기가 중간에 실패하면 다음 `OpenLayer`가 성공할 때까지 파일 시스템 요청을 거부한다([`Layers::check_ready`]).
//!   엉뚱한 뿌리에 쓰거나 실행하지 않게 한다.
//!
//! **내보내기** (overlay 위층 → 층 항목, [`export_upper`])
//!
//! - 위층 디렉터리를 이름(바이트) 순서로 걷고, 디렉터리는 그 안의 항목보다 먼저 보낸다. xattr도 이름 순서다. 링크를 따라가지
//!   않는다. 맨 위는 보내지 않는다. 그래서 같은 위층은 같은 항목 차례가 된다(층 tar의 결정성, 캐시).
//! - 이름, 링크 대상, xattr 이름은 UTF-8이어야 한다. 아니면 오류다(OCI 층 tar의 PAX 기록에 담을 수 없다).
//! - 문자 장치 0/0은 whiteout이다. `trusted.overlay.opaque`가 `y`인 디렉터리는 opaque다.
//! - `trusted.overlay.*` xattr은 overlay 내부용이라 보내지 않는다. 그중 `redirect`, `metacopy`가 있으면 오류다.
//!   층은 그 기능을 끄고 마운트하므로 생기지 않는다. 생겼다면 위층만으로는 내용을 알 수 없다.
//! - 같은 inode를 가리키는 일반 파일(`nlink` > 1)은 처음 것만 내용과 함께 보내고, 나머지는 그 경로로의 하드 링크다.
//! - 소켓은 건너뛴다(OCI 층에 담을 수 없다).
//! - 층을 위해 만든 빈 마운트 자리(`skip`, 맨 위의 이름)는 보내지 않는다.
//! - sbx가 층에 둔 파일([`Planted`], 예: `--network allow`의 `/etc/resolv.conf`)은 그 자리에 sbx가 쓴 내용 그대로인
//!   일반 파일이면 보내지 않는다. 빌드 단계가 바꾸거나 지웠으면 그 변경을 보낸다(단계가 한 일이다). 맨 위의 마운트
//!   자리처럼 sbx가 만든 맨 위 디렉터리 안에 그런 파일만 있으면 그 디렉터리도 보내지 않는다. 그래서 만든 이미지에는
//!   sbx가 둔 것이 남지 않는다.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;

use sbx_guest_proto::{Error, ErrorKind, GuestMsg, LayerEntry, LayerKind};

use crate::wire::{Out, io_error, send_file_data};

/// 빌드 층을 쌓는 방법. 규칙은 모듈 문서에 있다.
pub trait Layers: Send + Sync {
    /// 새 층을 연다. 연 층의 번호(1부터).
    fn open(&self) -> Result<u32, Error>;
    /// 지금 층의 변경을 [`GuestMsg::LayerEntry`]와 [`GuestMsg::Data`]로 보낸다. 끝의 [`GuestMsg::End`]는 부르는 쪽이 보낸다.
    fn export(&self, out: &Out) -> Result<(), Error>;
    /// 파일 시스템 요청을 받아도 되는지. 빌드 전이거나 층이 열려 있으면 된다.
    fn check_ready(&self) -> Result<(), Error>;
    /// 층이 열려 있는지(빌드 단계 `ExecStep`은 열린 층에서만 한다).
    fn is_open(&self) -> bool;
}

/// `OpenLayer`를 처리한다.
pub fn open(layers: Option<&dyn Layers>, out: &Out) {
    match layers.map(|l| l.open()) {
        Some(Ok(layer)) => {
            let _ = out.send(&GuestMsg::LayerOpened { layer });
        }
        Some(Err(e)) => out.error(e),
        None => out.error(unsupported()),
    }
}

/// `ExportLayer`를 처리한다.
pub fn export(layers: Option<&dyn Layers>, out: &Out) {
    match layers.map(|l| l.export(out)) {
        Some(Ok(())) => {
            let _ = out.send(&GuestMsg::End);
        }
        Some(Err(e)) => out.error(e),
        None => out.error(unsupported()),
    }
}

fn unsupported() -> Error {
    Error::new(
        ErrorKind::InvalidArgument,
        "build layers are not supported by this guest",
    )
}

/// overlay 위층 디렉터리를 읽는 방법. 게스트는 [`Kernel`]을 쓴다. 테스트는 whiteout과 xattr을 흉내 낸다
/// (문자 장치 0/0과 `trusted.*` xattr은 root여야 만든다).
pub trait UpperFs {
    /// 확장 속성 (이름, 값). 심볼릭 링크를 따라가지 않는다.
    fn xattrs(&self, path: &Path) -> io::Result<Vec<(String, Vec<u8>)>>;
    /// overlay의 whiteout인지.
    fn is_whiteout(&self, path: &Path, meta: &fs::Metadata) -> bool;
}

/// 커널 overlayfs가 쓴 위층.
pub struct Kernel;

impl UpperFs for Kernel {
    #[cfg(target_os = "linux")]
    fn xattrs(&self, path: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
        use crate::sys::linux::{get_xattr, list_xattrs};
        let mut out = Vec::new();
        for name in list_xattrs(path)? {
            let Some(value) = get_xattr(path, &name)? else {
                continue; // 그 사이에 지워졌다.
            };
            let name = String::from_utf8(name).map_err(|n| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "xattr name is not UTF-8: {:?}",
                        String::from_utf8_lossy(n.as_bytes())
                    ),
                )
            })?;
            out.push((name, value));
        }
        Ok(out)
    }

    /// Linux 밖(호스트에서 도는 테스트)에서는 xattr을 읽지 않는다.
    #[cfg(not(target_os = "linux"))]
    fn xattrs(&self, _path: &Path) -> io::Result<Vec<(String, Vec<u8>)>> {
        Ok(Vec::new())
    }

    fn is_whiteout(&self, _path: &Path, meta: &fs::Metadata) -> bool {
        meta.file_type().is_char_device() && meta.rdev() == 0
    }
}

/// overlay 내부용 xattr 이름의 앞부분.
pub const OVERLAY_XATTR: &str = "trusted.overlay.";

/// sbx가 층에 둔 파일 하나: 층 안의 상대 경로와 쓴 내용. 그대로면 내보내지 않는다(모듈 문서).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planted {
    /// 층 안의 상대 경로(`etc/resolv.conf`).
    pub path: String,
    /// sbx가 쓴 내용.
    pub content: Vec<u8>,
}

/// 위층 디렉터리 `upper`의 내용을 층 항목으로 보낸다. 규칙은 모듈 문서에 있다.
/// `skip`은 맨 위에서 비어 있으면(또는 [`Planted`] 파일만 있으면) 건너뛸 디렉터리 이름이다(층을 위해 만든 자리).
pub fn export_upper(
    upper: &Path,
    skip: &[String],
    planted: &[Planted],
    fs: &dyn UpperFs,
    out: &Out,
) -> Result<(), Error> {
    let mut w = Walk {
        fs,
        out,
        links: HashMap::new(),
        planted,
    };
    w.dir(upper, None, skip)
}

/// `abs`(층 안 `rel`)가 sbx가 둔 그대로의 파일인지.
fn is_planted(planted: &[Planted], abs: &Path, rel: &str, meta: &fs::Metadata) -> bool {
    planted.iter().any(|p| {
        p.path == rel
            && meta.is_file()
            && meta.len() == p.content.len() as u64
            && fs::read(abs).is_ok_and(|b| b == p.content)
    })
}

struct Walk<'a> {
    fs: &'a dyn UpperFs,
    out: &'a Out,
    /// 이미 보낸 일반 파일: (장치, inode) → 경로. `nlink` > 1인 것만.
    links: HashMap<(u64, u64), String>,
    planted: &'a [Planted],
}

fn invalid(m: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidArgument, m)
}

/// 비었거나 sbx가 둔 그대로의 파일만 있는 디렉터리인지(`rel`은 층 안 경로).
fn holds_only_planted(planted: &[Planted], p: &Path, rel: &str) -> io::Result<bool> {
    for c in fs::read_dir(p)? {
        let c = c?;
        let Some(name) = c.file_name().to_str().map(str::to_owned) else {
            return Ok(false);
        };
        let meta = fs::symlink_metadata(c.path())?;
        if !is_planted(planted, &c.path(), &format!("{rel}/{name}"), &meta) {
            return Ok(false);
        }
    }
    Ok(true)
}

impl Walk<'_> {
    fn send(&self, e: LayerEntry) -> Result<(), Error> {
        self.out
            .send(&GuestMsg::LayerEntry(e))
            .map_err(|e| io_error("send", &e))
    }

    fn dir(&mut self, abs: &Path, rel: Option<&str>, skip: &[String]) -> Result<(), Error> {
        let at = rel.unwrap_or("/");
        let mut children: Vec<_> = fs::read_dir(abs)
            .map_err(|e| io_error(at, &e))?
            .collect::<Result<_, _>>()
            .map_err(|e| io_error(at, &e))?;
        children.sort_by_key(|e| e.file_name());
        for c in children {
            let name = c.file_name();
            let Some(name) = name.to_str() else {
                return Err(invalid(format!("{at}: a name is not UTF-8: {name:?}")));
            };
            let path = match rel {
                Some(r) => format!("{r}/{name}"),
                None => name.to_owned(),
            };
            let child = c.path();
            let meta = fs::symlink_metadata(&child).map_err(|e| io_error(&path, &e))?;
            if rel.is_none()
                && meta.is_dir()
                && skip.iter().any(|s| s == name)
                && holds_only_planted(self.planted, &child, &path)
                    .map_err(|e| io_error(&path, &e))?
            {
                continue;
            }
            if is_planted(self.planted, &child, &path, &meta) {
                continue;
            }
            self.entry(&child, path, &meta)?;
        }
        Ok(())
    }

    fn entry(&mut self, abs: &Path, path: String, meta: &fs::Metadata) -> Result<(), Error> {
        let base = |kind| LayerEntry {
            path: path.clone(),
            kind,
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
            mtime: meta.mtime(),
            xattrs: Vec::new(),
        };
        if self.fs.is_whiteout(abs, meta) {
            return self.send(base(LayerKind::Whiteout));
        }
        let ft = meta.file_type();
        if ft.is_socket() {
            return Ok(());
        }
        let mut opaque = false;
        let mut xattrs = Vec::new();
        for (name, value) in self.fs.xattrs(abs).map_err(|e| io_error(&path, &e))? {
            match name.strip_prefix(OVERLAY_XATTR) {
                Some("opaque") => opaque = value == b"y",
                Some(what @ ("redirect" | "metacopy")) => {
                    return Err(Error::new(
                        ErrorKind::Io,
                        format!("{path}: overlay {what} cannot be exported as a layer"),
                    ));
                }
                Some(_) => {}
                None => xattrs.push((name, value)),
            }
        }
        // 파일 시스템이 주는 차례는 정해져 있지 않다. 같은 층이 같은 바이트가 되게 이름 순서로 보낸다.
        xattrs.sort();
        let kind = if ft.is_dir() {
            LayerKind::Dir { opaque }
        } else if ft.is_symlink() {
            let to = fs::read_link(abs).map_err(|e| io_error(&path, &e))?;
            let to = to
                .to_str()
                .ok_or_else(|| invalid(format!("{path}: link target is not UTF-8")))?;
            LayerKind::Symlink {
                target: to.to_owned(),
            }
        } else if ft.is_file() {
            let key = (meta.dev(), meta.ino());
            match self.links.get(&key) {
                Some(first) if meta.nlink() > 1 => LayerKind::Hardlink {
                    target: first.clone(),
                },
                _ => {
                    if meta.nlink() > 1 {
                        self.links.insert(key, path.clone());
                    }
                    LayerKind::File { size: meta.len() }
                }
            }
        } else if ft.is_char_device() || ft.is_block_device() {
            let (major, minor) = crate::sys::dev_split(meta.rdev());
            if ft.is_char_device() {
                LayerKind::CharDevice { major, minor }
            } else {
                LayerKind::BlockDevice { major, minor }
            }
        } else if ft.is_fifo() {
            LayerKind::Fifo
        } else {
            return Ok(());
        };
        let entry = LayerEntry {
            xattrs,
            ..base(kind.clone())
        };
        self.send(entry)?;
        match kind {
            LayerKind::File { size } => send_file_data(self.out, abs, &path, size),
            LayerKind::Dir { .. } => self.dir(abs, Some(&path), &[]),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
