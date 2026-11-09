//! 묶음 압축 파일(gzip으로 압축한 tar)을 푼다. 규칙은 크레이트 문서의 "묶음 압축 파일"에 있다.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use executor_install::executable_name;

use crate::error::RepoError;

/// `archive`를 빈 디렉터리 `dest`(없으면 만든다)에 풀고, 묶음 디렉터리(맨 위에 실행 파일이 있는 곳)를 돌려준다.
/// 맨 위가 디렉터리 하나뿐이고 실행 파일이 그 안에 있으면 그 디렉터리다.
pub fn unpack(archive: &Path, dest: &Path) -> Result<PathBuf, RepoError> {
    fs::create_dir_all(dest)?;
    if fs::read_dir(dest)?.next().is_some() {
        return Err(RepoError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not empty", dest.display()),
        )));
    }
    let file = fs::File::open(archive)?;
    unpack_reader(flate2::read::GzDecoder::new(file), dest)?;
    bundle_root(dest)
}

fn bad(m: impl Into<String>) -> RepoError {
    RepoError::Archive(m.into())
}

fn unpack_reader(r: impl Read, dest: &Path) -> Result<(), RepoError> {
    let mut ar = tar::Archive::new(r);
    let entries = ar.entries().map_err(|e| bad(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        let path = entry.path().map_err(|e| bad(e.to_string()))?.into_owned();
        let mut rel = PathBuf::new();
        for c in path.components() {
            match c {
                Component::Normal(p) => rel.push(p),
                Component::CurDir => {}
                _ => return Err(bad(format!("unsafe path {}", path.display()))),
            }
        }
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
            continue;
        }
        if rel.as_os_str().is_empty() {
            if kind.is_dir() {
                continue;
            }
            return Err(bad(format!("entry {:?} has no name", path.display())));
        }
        let target = dest.join(&rel);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if kind.is_file() || kind == tar::EntryType::Continuous {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mode = entry.header().mode().unwrap_or(0o644);
            let mut out = fs::File::create(&target)?;
            io::copy(&mut entry, &mut out).map_err(|e| bad(e.to_string()))?;
            set_mode(&target, mode)?;
        } else {
            return Err(bad(format!(
                "{} is a {:?}; only regular files and directories are allowed",
                path.display(),
                kind
            )));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // 권한 비트만 쓴다(setuid 등은 버린다). 소유자는 언제나 읽고 쓸 수 있다.
    fs::set_permissions(p, fs::Permissions::from_mode((mode & 0o777) | 0o600))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> io::Result<()> {
    Ok(())
}

fn bundle_root(dest: &Path) -> Result<PathBuf, RepoError> {
    let exe = executable_name();
    if dest.join(&exe).is_file() {
        return Ok(dest.to_path_buf());
    }
    let top: Vec<_> = fs::read_dir(dest)?.collect::<Result<_, _>>()?;
    if let [only] = top.as_slice()
        && only.file_type()?.is_dir()
        && only.path().join(&exe).is_file()
    {
        return Ok(only.path());
    }
    Err(bad(format!(
        "{exe} is not at the top of the archive (or inside its single top-level directory)"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tar_gz(build: impl FnOnce(&mut tar::Builder<Vec<u8>>)) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        build(&mut b);
        let raw = b.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        io::Write::write_all(&mut gz, &raw).unwrap();
        gz.finish().unwrap()
    }

    fn file(b: &mut tar::Builder<Vec<u8>>, path: &str, mode: u32, body: &[u8]) {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(tar::EntryType::Regular);
        // set_path는 ".."를 거부하므로 이름을 직접 쓴다.
        let name = &mut h.as_old_mut().name;
        name[..path.len()].copy_from_slice(path.as_bytes());
        h.set_cksum();
        b.append(&h, body).unwrap();
    }

    fn run(name: &str, data: &[u8]) -> (PathBuf, Result<PathBuf, RepoError>) {
        let root =
            std::env::temp_dir().join(format!("sbx-exrepo-unpack-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let ar = root.join("a.tar.gz");
        fs::write(&ar, data).unwrap();
        let r = unpack(&ar, &root.join("out"));
        (root, r)
    }

    #[test]
    fn bundle_at_top_or_in_one_directory() {
        let exe = executable_name();
        let (root, r) = run(
            "top",
            &tar_gz(|b| {
                file(b, &exe, 0o755, b"#!/bin/sh\n");
                file(b, "kernel", 0o644, b"k");
            }),
        );
        let dir = r.unwrap();
        assert_eq!(dir, root.join("out"));
        assert_eq!(fs::read(dir.join("kernel")).unwrap(), b"k");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = fs::metadata(dir.join(&exe)).unwrap().permissions().mode();
            assert_eq!(m & 0o777, 0o755);
        }
        fs::remove_dir_all(root).unwrap();

        let (root, r) = run(
            "nested",
            &tar_gz(|b| {
                file(b, &format!("fc-1.2.0/{exe}"), 0o755, b"x");
                file(b, "fc-1.2.0/sub/kernel", 0o644, b"k");
            }),
        );
        let dir = r.unwrap();
        assert_eq!(dir, root.join("out/fc-1.2.0"));
        assert!(dir.join("sub/kernel").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsafe_or_incomplete_archives_are_refused() {
        let exe = executable_name();
        let (root, r) = run("dotdot", &tar_gz(|b| file(b, "../evil", 0o644, b"x")));
        assert!(matches!(r, Err(RepoError::Archive(m)) if m.contains("unsafe")));
        assert!(!root.join("evil").exists());
        fs::remove_dir_all(root).unwrap();

        let (root, r) = run("abs", &tar_gz(|b| file(b, "/tmp/evil", 0o644, b"x")));
        assert!(matches!(r, Err(RepoError::Archive(_))));
        fs::remove_dir_all(root).unwrap();

        let (root, r) = run(
            "link",
            &tar_gz(|b| {
                file(b, &exe, 0o755, b"x");
                let mut h = tar::Header::new_gnu();
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_size(0);
                b.append_link(&mut h, "kernel", "/etc/passwd").unwrap();
            }),
        );
        assert!(matches!(r, Err(RepoError::Archive(m)) if m.contains("only regular files")));
        fs::remove_dir_all(root).unwrap();

        let (root, r) = run("noexe", &tar_gz(|b| file(b, "kernel", 0o644, b"k")));
        assert!(matches!(r, Err(RepoError::Archive(m)) if m.contains("not at the top")));
        fs::remove_dir_all(root).unwrap();

        let (root, r) = run("notgz", b"plain text");
        assert!(r.is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
