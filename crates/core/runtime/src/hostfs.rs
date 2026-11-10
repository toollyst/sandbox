//! 호스트 쪽 파일 송수신. 게스트 쪽(`sbx-guest`의 copy)과 같은 메시지를 만든다.
//!
//! 보내기 (호스트 → 게스트)
//! - 링크를 따라가지 않는다. 일반 파일, 디렉터리, 심볼릭 링크만 보낸다(다른 것은 건너뛴다).
//! - 소유자는 uid 0, gid 0으로 보낸다(Docker `cp`와 같다). 권한은 호스트 값이다. Windows에서는 디렉터리 0755, 파일 0644다.
//!
//! 받기 (게스트 → 호스트)
//! - 게스트가 보낸 경로를 믿지 않는다. 안전한 상대 경로만 받고, 중간 구성 요소가 심볼릭 링크이면 오류다.
//! - 소유자는 바꾸지 않는다(지금 사용자). 권한(Unix)과 수정 시각은 둔다. 디렉터리 권한은 끝에 둔다.
//! - Windows에서는 심볼릭 링크를 만들지 않고 오류를 낸다.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sbx_guest_proto::{CHUNK_LEN, Entry, FileKind, GuestMsg, HostMsg, check_relative_path};

use crate::error::RuntimeError;

fn proto_err(m: impl Into<String>) -> RuntimeError {
    RuntimeError::Protocol(m.into())
}

fn mtime_of(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn mode_of(m: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(m: &fs::Metadata) -> u32 {
    if m.is_dir() { 0o755 } else { 0o644 }
}

/// `src`를 보낸다. `top`이 있으면 그 이름으로, 없으면 `src`(디렉터리)의 내용을 바로 보낸다.
pub fn send(
    src: &Path,
    top: Option<&str>,
    emit: &mut dyn FnMut(HostMsg) -> io::Result<()>,
) -> Result<(), RuntimeError> {
    match top {
        Some(name) => {
            let meta = fs::symlink_metadata(src)?;
            walk(src, name.to_owned(), &meta, emit)
        }
        None => {
            for c in sorted_children(src)? {
                let meta = c.metadata()?;
                let name = file_name(&c.file_name())?;
                walk(&c.path(), name, &meta, emit)?;
            }
            Ok(())
        }
    }
}

fn file_name(n: &std::ffi::OsStr) -> Result<String, RuntimeError> {
    n.to_str()
        .map(str::to_owned)
        .ok_or_else(|| RuntimeError::Invalid(format!("file name is not UTF-8: {n:?}")))
}

fn sorted_children(dir: &Path) -> Result<Vec<fs::DirEntry>, RuntimeError> {
    let mut v: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    v.sort_by_key(|e| e.file_name());
    Ok(v)
}

fn walk(
    p: &Path,
    rel: String,
    meta: &fs::Metadata,
    emit: &mut dyn FnMut(HostMsg) -> io::Result<()>,
) -> Result<(), RuntimeError> {
    let ft = meta.file_type();
    let kind = if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_symlink() {
        let to = fs::read_link(p)?;
        FileKind::Symlink {
            target: to
                .to_str()
                .ok_or_else(|| RuntimeError::Invalid(format!("link target is not UTF-8: {to:?}")))?
                .to_owned(),
        }
    } else if ft.is_file() {
        FileKind::File { size: meta.len() }
    } else {
        return Ok(());
    };
    emit(HostMsg::Entry(Entry {
        path: rel.clone(),
        kind: kind.clone(),
        mode: mode_of(meta),
        uid: 0,
        gid: 0,
        mtime: mtime_of(meta),
    }))?;
    match kind {
        FileKind::File { size } => {
            let mut r = File::open(p)?.take(size);
            let mut buf = vec![0; CHUNK_LEN];
            let mut sent = 0u64;
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                emit(HostMsg::Data(buf[..n].to_vec()))?;
                sent += n as u64;
            }
            if sent != size {
                return Err(RuntimeError::Invalid(format!(
                    "{} shrank while copying",
                    p.display()
                )));
            }
        }
        FileKind::Dir => {
            for c in sorted_children(p)? {
                let meta = c.metadata()?;
                let name = file_name(&c.file_name())?;
                walk(&c.path(), format!("{rel}/{name}"), &meta, emit)?;
            }
        }
        FileKind::Symlink { .. } => {}
    }
    Ok(())
}

/// 게스트가 보낸 맨 위 이름 `from`을 `to`로 바꾼다. `to`가 없으면 맨 위 항목은 버리고 그 안의 것만 둔다.
pub fn rename_top(path: &str, from: &str, to: Option<&str>) -> Option<String> {
    let rest = if path == from {
        ""
    } else {
        path.strip_prefix(from)?.strip_prefix('/')?
    };
    match (to, rest) {
        (Some(t), "") => Some(t.to_owned()),
        (Some(t), r) => Some(format!("{t}/{r}")),
        (None, "") => None,
        (None, r) => Some(r.to_owned()),
    }
}

fn time(mtime: i64) -> SystemTime {
    if mtime >= 0 {
        UNIX_EPOCH + Duration::from_secs(mtime as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(mtime.unsigned_abs())
    }
}

fn target(dest: &Path, rel: &str) -> Result<PathBuf, RuntimeError> {
    check_relative_path(rel).map_err(proto_err)?;
    let mut p = dest.to_path_buf();
    let mut parts = rel.split('/').peekable();
    while let Some(c) = parts.next() {
        p.push(c);
        if parts.peek().is_some() {
            let m =
                fs::symlink_metadata(&p).map_err(|e| proto_err(format!("{rel:?}: parent: {e}")))?;
            if !m.is_dir() {
                return Err(proto_err(format!("{rel:?}: parent is not a directory")));
            }
        }
    }
    Ok(p)
}

/// 디렉터리가 아닌 기존 항목을 지운다. 디렉터리가 있으면 `true`.
fn clear(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => fs::remove_file(path).map(|()| false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_p: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// `dest` 아래에 받는다. `next`는 다음 메시지를 준다. `End`에서 끝난다.
/// 게스트가 보낸 맨 위 이름 `from`은 [`rename_top`]으로 `to`로 바꾼다. 맨 위 밖의 경로는 규약 위반이다.
pub fn receive(
    dest: &Path,
    from: &str,
    to: Option<&str>,
    next: &mut dyn FnMut() -> Result<GuestMsg, RuntimeError>,
) -> Result<(), RuntimeError> {
    if !dest.is_dir() {
        return Err(RuntimeError::Invalid(format!(
            "destination directory {} does not exist",
            dest.display()
        )));
    }
    let mut dirs: Vec<(PathBuf, u32, i64)> = Vec::new();
    loop {
        let entry = match next()? {
            GuestMsg::End => break,
            GuestMsg::Entry(e) => e,
            GuestMsg::Error(e) => return Err(RuntimeError::Guest(e)),
            other => return Err(proto_err(format!("expected Entry or End, got {other:?}"))),
        };
        let inside = entry.path == from || entry.path.starts_with(&format!("{from}/"));
        if !inside {
            return Err(proto_err(format!(
                "{:?} is outside the requested {from:?}",
                entry.path
            )));
        }
        let mapped = rename_top(&entry.path, from, to);
        let path = match &mapped {
            Some(rel) => Some(target(dest, rel)?),
            None => None,
        };
        match (&entry.kind, &path) {
            (FileKind::Dir, None) => {}
            (FileKind::Dir, Some(p)) => {
                if !clear(p)? {
                    fs::create_dir(p)?;
                }
                dirs.push((p.clone(), entry.mode, entry.mtime));
            }
            (FileKind::Symlink { target: to }, Some(p)) => {
                if clear(p)? {
                    return Err(RuntimeError::Invalid(format!(
                        "{}: a directory is in the way",
                        p.display()
                    )));
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(to, p)?;
                #[cfg(not(unix))]
                return Err(RuntimeError::Invalid(format!(
                    "cannot create symbolic link {} -> {to} on this OS",
                    p.display()
                )));
            }
            (FileKind::File { size }, Some(p)) => {
                if clear(p)? {
                    return Err(RuntimeError::Invalid(format!(
                        "{}: a directory is in the way",
                        p.display()
                    )));
                }
                let mut f = OpenOptions::new().write(true).create_new(true).open(p)?;
                let mut left = *size;
                while left > 0 {
                    let data = match next()? {
                        GuestMsg::Data(d) => d,
                        other => return Err(proto_err(format!("expected Data, got {other:?}"))),
                    };
                    if data.len() as u64 > left {
                        return Err(proto_err("more data than the declared size"));
                    }
                    f.write_all(&data)?;
                    left -= data.len() as u64;
                }
                f.set_modified(time(entry.mtime))?;
                drop(f);
                set_mode(p, entry.mode)?;
            }
            (_, None) => {
                return Err(proto_err("only a directory can be unpacked as contents"));
            }
        }
    }
    for (dir, mode, mtime) in dirs.iter().rev() {
        File::open(dir)?.set_modified(time(*mtime))?;
        set_mode(dir, *mode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_top_rules() {
        assert_eq!(rename_top("a", "a", Some("b")), Some("b".into()));
        assert_eq!(rename_top("a/x/y", "a", Some("b")), Some("b/x/y".into()));
        assert_eq!(rename_top("a", "a", None), None);
        assert_eq!(rename_top("a/x", "a", None), Some("x".into()));
        assert_eq!(rename_top("ab/x", "a", None), None);
        assert_eq!(rename_top("other", "a", Some("b")), None);
    }
}
