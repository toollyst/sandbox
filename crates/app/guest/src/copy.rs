//! 파일 송수신 (`HostMsg::CopyIn`, `HostMsg::CopyOut`).
//!
//! 받기(`CopyIn`)
//! - 항목 경로는 `check_relative_path`를 통과해야 한다. 대상 디렉터리 밖으로 나갈 수 없다.
//! - 경로의 중간 구성 요소가 심볼릭 링크이면 오류다. 링크를 따라 대상 밖에 쓰지 않는다.
//! - 부모 디렉터리는 이미 있어야 한다(보내는 쪽이 디렉터리를 먼저 보낸다).
//! - 같은 이름의 파일이나 링크는 지우고 새로 만든다. 디렉터리끼리는 합친다. 디렉터리 자리에 파일이 오면 오류다.
//! - 소유자는 게스트가 root일 때만 그대로 둔다(실제 VM의 게스트는 항상 root다). root가 아니면 지금 사용자 소유로 남는다.
//! - 권한, 수정 시각을 그대로 둔다. 디렉터리의 권한과 수정 시각은 끝에, 안쪽부터 맞춘다
//!   (쓰기 권한이 없는 디렉터리 안에도 쓸 수 있게, 안에 쓰면 수정 시각이 바뀌므로).
//!   심볼릭 링크의 수정 시각은 두지 않는다.
//!
//! 정보(`Stat`): 심볼릭 링크를 따라간 결과다. 없으면 `None`. 이름이 없는 경로(`/`)는 이름 `/`로 답한다.
//!
//! 보내기(`CopyOut`)
//! - 첫 항목은 요청 경로의 마지막 이름이다. 그 경로가 링크여도 따라가지 않는다.
//! - 디렉터리 안은 이름 순서로 보낸다. 링크는 따라가지 않는다.
//! - 일반 파일, 디렉터리, 심볼릭 링크만 보낸다. 장치, FIFO, 소켓은 건너뛴다. 하드 링크는 각각 파일로 보낸다.
//! - 보내는 동안 파일이 줄면 오류로 끝낸다. 늘면 처음 크기만큼만 보낸다.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, lchown, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sbx_guest_proto::{
    CHUNK_LEN, Entry, Error, ErrorKind, FileKind, GuestMsg, HostMsg, check_relative_path,
};

use crate::wire::{Out, io_error, protocol};

fn invalid(m: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidArgument, m)
}

fn time(mtime: i64) -> SystemTime {
    if mtime >= 0 {
        UNIX_EPOCH + Duration::from_secs(mtime as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(mtime.unsigned_abs())
    }
}

/// `dest` 아래에 받는다.
pub fn receive(dest: &str, input: &mut impl Read, out: &Out) {
    match receive_inner(dest, input) {
        Ok(()) => {
            let _ = out.send(&GuestMsg::Done);
        }
        Err(e) => out.error(e),
    }
}

/// `path`가 `dest` 아래의 안전한 위치인지 본다. 중간 구성 요소는 진짜 디렉터리여야 한다.
fn target(dest: &Path, rel: &str) -> Result<PathBuf, Error> {
    check_relative_path(rel).map_err(invalid)?;
    let mut p = dest.to_path_buf();
    let mut parts = rel.split('/').peekable();
    while let Some(c) = parts.next() {
        p.push(c);
        if parts.peek().is_some() {
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(invalid(format!("{rel:?}: parent is not a directory"))),
                Err(e) => return Err(io_error(&format!("{rel:?}: parent"), &e)),
            }
        }
    }
    Ok(p)
}

/// 디렉터리가 아닌 기존 항목을 지운다. 디렉터리가 있으면 `true`.
fn clear(path: &Path, rel: &str) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Ok(true),
        Ok(_) => fs::remove_file(path)
            .map(|()| false)
            .map_err(|e| io_error(rel, &e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(rel, &e)),
    }
}

fn receive_inner(dest: &str, input: &mut impl Read) -> Result<(), Error> {
    if !dest.starts_with('/') {
        return Err(invalid(format!("destination must be absolute: {dest:?}")));
    }
    let dest = Path::new(dest);
    if !dest.is_dir() {
        return Err(Error::new(
            ErrorKind::NotFound,
            format!("destination {} is not a directory", dest.display()),
        ));
    }
    let read = |input: &mut _| HostMsg::read_from(input).map_err(|e| io_error("receive", &e));
    let mut dirs: Vec<(PathBuf, u32, i64)> = Vec::new();
    loop {
        let entry = match read(input)? {
            HostMsg::End => break,
            HostMsg::Entry(e) => e,
            other => return Err(protocol(&other, "Entry or End")),
        };
        let path = target(dest, &entry.path)?;
        let rel = entry.path.as_str();
        match &entry.kind {
            FileKind::Dir => {
                if !clear(&path, rel)? {
                    fs::create_dir(&path).map_err(|e| io_error(rel, &e))?;
                }
                dirs.push((path.clone(), entry.mode, entry.mtime));
            }
            FileKind::Symlink { target: to } => {
                if clear(&path, rel)? {
                    return Err(invalid(format!("{rel:?}: a directory is in the way")));
                }
                symlink(to, &path).map_err(|e| io_error(rel, &e))?;
            }
            FileKind::File { size } => {
                if clear(&path, rel)? {
                    return Err(invalid(format!("{rel:?}: a directory is in the way")));
                }
                let mut f = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&path)
                    .map_err(|e| io_error(rel, &e))?;
                let mut left = *size;
                while left > 0 {
                    let data = match read(input)? {
                        HostMsg::Data(d) => d,
                        other => return Err(protocol(&other, "Data")),
                    };
                    if data.len() as u64 > left {
                        return Err(Error::new(
                            ErrorKind::Protocol,
                            format!("{rel:?}: more data than the declared {size} bytes"),
                        ));
                    }
                    f.write_all(&data).map_err(|e| io_error(rel, &e))?;
                    left -= data.len() as u64;
                }
                f.set_modified(time(entry.mtime))
                    .map_err(|e| io_error(rel, &e))?;
            }
        }
        // 소유자를 먼저 바꾼다. chown은 setuid 비트를 지우므로 권한은 그 뒤에 둔다.
        if crate::sys::is_root() {
            lchown(&path, Some(entry.uid), Some(entry.gid)).map_err(|e| io_error(rel, &e))?;
        }
        if matches!(entry.kind, FileKind::File { .. }) {
            fs::set_permissions(&path, fs::Permissions::from_mode(entry.mode))
                .map_err(|e| io_error(rel, &e))?;
        }
    }
    for (dir, mode, mtime) in dirs.iter().rev() {
        File::open(dir)
            .and_then(|f| f.set_modified(time(*mtime)))
            .and_then(|()| fs::set_permissions(dir, fs::Permissions::from_mode(*mode)))
            .map_err(|e| io_error(&dir.display().to_string(), &e))?;
    }
    Ok(())
}

/// 경로 정보를 보낸다.
pub fn stat(path: &str, out: &Out) {
    let reply = (|| {
        if !path.starts_with('/') {
            return Err(invalid(format!("path must be absolute: {path:?}")));
        }
        let meta = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(path, &e)),
        };
        let name = Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("/")
            .to_owned();
        let kind = if meta.is_dir() {
            FileKind::Dir
        } else {
            FileKind::File { size: meta.len() }
        };
        Ok(Some(Entry {
            path: name,
            kind,
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
            mtime: meta.mtime(),
        }))
    })();
    match reply {
        Ok(s) => {
            let _ = out.send(&GuestMsg::Stat(s));
        }
        Err(e) => out.error(e),
    }
}

/// `path`를 보낸다.
pub fn send(path: &str, out: &Out) {
    match send_inner(path, out) {
        Ok(()) => {
            let _ = out.send(&GuestMsg::End);
        }
        Err(e) => out.error(e),
    }
}

fn send_inner(path: &str, out: &Out) -> Result<(), Error> {
    if !path.starts_with('/') {
        return Err(invalid(format!("path must be absolute: {path:?}")));
    }
    let p = Path::new(path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| check_relative_path(n).is_ok())
        .ok_or_else(|| invalid(format!("cannot copy {path:?}: no file name")))?;
    let meta = fs::symlink_metadata(p).map_err(|e| io_error(path, &e))?;
    walk(p, name.to_owned(), &meta, out)
}

fn send_msg(out: &Out, m: &GuestMsg) -> Result<(), Error> {
    out.send(m).map_err(|e| io_error("send", &e))
}

fn walk(p: &Path, rel: String, meta: &fs::Metadata, out: &Out) -> Result<(), Error> {
    let ft = meta.file_type();
    let kind = if ft.is_dir() {
        FileKind::Dir
    } else if ft.is_symlink() {
        let to = fs::read_link(p).map_err(|e| io_error(&rel, &e))?;
        let to = to
            .to_str()
            .ok_or_else(|| invalid(format!("{rel:?}: link target is not UTF-8")))?;
        FileKind::Symlink {
            target: to.to_owned(),
        }
    } else if ft.is_file() {
        FileKind::File { size: meta.len() }
    } else {
        return Ok(());
    };
    send_msg(
        out,
        &GuestMsg::Entry(Entry {
            path: rel.clone(),
            kind: kind.clone(),
            mode: meta.mode() & 0o7777,
            uid: meta.uid(),
            gid: meta.gid(),
            mtime: meta.mtime(),
        }),
    )?;
    match kind {
        FileKind::File { size } => {
            let f = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(p)
                .map_err(|e| io_error(&rel, &e))?;
            let mut r = f.take(size);
            let mut buf = vec![0; CHUNK_LEN];
            let mut sent = 0u64;
            loop {
                let n = r.read(&mut buf).map_err(|e| io_error(&rel, &e))?;
                if n == 0 {
                    break;
                }
                send_msg(out, &GuestMsg::Data(buf[..n].to_vec()))?;
                sent += n as u64;
            }
            if sent != size {
                return Err(Error::new(
                    ErrorKind::Io,
                    format!("{rel:?} shrank while copying ({sent} of {size} bytes)"),
                ));
            }
        }
        FileKind::Dir => {
            let mut children: Vec<_> = fs::read_dir(p)
                .map_err(|e| io_error(&rel, &e))?
                .collect::<Result<_, _>>()
                .map_err(|e| io_error(&rel, &e))?;
            children.sort_by_key(|e| e.file_name());
            for c in children {
                let name = c.file_name();
                let Some(name) = name.to_str() else {
                    return Err(invalid(format!("{rel:?}: a name is not UTF-8")));
                };
                let meta = c.metadata().map_err(|e| io_error(name, &e))?;
                walk(&c.path(), format!("{rel}/{name}"), &meta, out)?;
            }
        }
        FileKind::Symlink { .. } => {}
    }
    Ok(())
}
