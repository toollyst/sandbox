//! 묶음 압축 파일(gzip으로 압축한 tar)을 푼다. 규칙은 크레이트 문서의 "묶음 압축 파일"에 있다.

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

use sbx_executor_install::executable_name;

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
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() || kind.is_pax_local_extensions() {
            continue;
        }
        let pax: Vec<(String, String)> = match entry.pax_extensions() {
            Ok(Some(exts)) => exts
                .filter_map(|x| {
                    let x = x.ok()?;
                    Some((x.key().ok()?.to_owned(), x.value().ok()?.to_owned()))
                })
                .collect(),
            Ok(None) => Vec::new(),
            Err(e) => return Err(bad(e.to_string())),
        };
        let sparse = Sparse::from_pax(&pax)?;
        let path = match &sparse {
            Some(s) => PathBuf::from(&s.name),
            None => entry.path().map_err(|e| bad(e.to_string()))?.into_owned(),
        };
        let mut rel = PathBuf::new();
        for c in path.components() {
            match c {
                Component::Normal(p) => rel.push(p),
                Component::CurDir => {}
                _ => return Err(bad(format!("unsafe path {}", path.display()))),
            }
        }
        if rel.as_os_str().is_empty() {
            if kind.is_dir() {
                continue;
            }
            return Err(bad(format!("entry {:?} has no name", path.display())));
        }
        let target = dest.join(&rel);
        // GNUSparse('S')는 tar 크레이트가 읽으면서 구멍을 0으로 채운다.
        let regular = kind.is_file()
            || kind == tar::EntryType::Continuous
            || kind == tar::EntryType::GNUSparse;
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else if regular {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mode = entry.header().mode().unwrap_or(0o644);
            let mut out = fs::File::create(&target)?;
            match sparse {
                Some(s) => s.write(&mut entry, &mut out)?,
                None => {
                    io::copy(&mut entry, &mut out).map_err(|e| bad(e.to_string()))?;
                }
            }
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

/// pax 형식의 sparse 파일 (GNU tar와 libarchive(macOS `tar`)가 0이 많은 파일을 이렇게 쓴다).
/// 1.0은 맵이 데이터 앞에(10진수 줄, 512바이트로 채움), 0.1은 `GNU.sparse.map`에 있다. 다른 버전은 거부한다.
struct Sparse {
    name: String,
    real_size: u64,
    /// 1.0이면 비어 있고 데이터 앞에서 읽는다.
    map: Option<Vec<(u64, u64)>>,
}

impl Sparse {
    fn from_pax(pax: &[(String, String)]) -> Result<Option<Self>, RepoError> {
        let get = |k: &str| {
            pax.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        if !pax.iter().any(|(k, _)| k.starts_with("GNU.sparse.")) {
            return Ok(None);
        }
        let num = |k: &str| -> Result<u64, RepoError> {
            get(k)
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| bad(format!("sparse file without a valid {k}")))
        };
        let name = get("GNU.sparse.name")
            .ok_or_else(|| bad("sparse file without GNU.sparse.name"))?
            .to_owned();
        match (get("GNU.sparse.major"), get("GNU.sparse.minor")) {
            (Some("1"), Some("0")) => Ok(Some(Self {
                name,
                real_size: num("GNU.sparse.realsize")?,
                map: None,
            })),
            (None, None) if get("GNU.sparse.map").is_some() => {
                let nums: Vec<u64> = get("GNU.sparse.map")
                    .unwrap_or_default()
                    .split(',')
                    .map(|n| n.trim().parse())
                    .collect::<Result<_, _>>()
                    .map_err(|_| bad("invalid GNU.sparse.map"))?;
                if !nums.len().is_multiple_of(2) {
                    return Err(bad("invalid GNU.sparse.map"));
                }
                Ok(Some(Self {
                    name,
                    real_size: num("GNU.sparse.size")?,
                    map: Some(nums.chunks(2).map(|c| (c[0], c[1])).collect()),
                }))
            }
            _ => Err(bad(format!(
                "{name}: this sparse file format is not supported (use pax sparse 1.0, or a non-sparse archive)"
            ))),
        }
    }

    /// 데이터 조각을 제자리에 쓴다. 구멍은 파일 길이로 0이 된다.
    fn write(self, data: &mut impl Read, out: &mut fs::File) -> Result<(), RepoError> {
        use std::io::{Seek, SeekFrom};
        let map = match self.map {
            Some(m) => m,
            None => read_map_1_0(data)?,
        };
        for &(offset, size) in &map {
            if offset
                .checked_add(size)
                .is_none_or(|end| end > self.real_size)
            {
                return Err(bad(format!("{}: sparse map is out of range", self.name)));
            }
            out.seek(SeekFrom::Start(offset))?;
            let copied = io::copy(&mut data.by_ref().take(size), out)?;
            if copied != size {
                return Err(bad(format!("{}: sparse data is truncated", self.name)));
            }
        }
        out.set_len(self.real_size)?;
        Ok(())
    }
}

/// pax sparse 1.0의 맵: 조각 수, 그리고 (offset, size) 쌍을 줄마다 10진수로. 512바이트 경계까지 채워져 있다.
fn read_map_1_0(data: &mut impl Read) -> Result<Vec<(u64, u64)>, RepoError> {
    let mut consumed = 0u64;
    let mut line = || -> Result<u64, RepoError> {
        let mut s = Vec::new();
        let mut b = [0u8; 1];
        loop {
            data.read_exact(&mut b)
                .map_err(|_| bad("sparse map is truncated"))?;
            consumed += 1;
            if b[0] == b'\n' {
                break;
            }
            if !b[0].is_ascii_digit() || s.len() > 20 {
                return Err(bad("invalid sparse map"));
            }
            s.push(b[0]);
        }
        std::str::from_utf8(&s)
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| bad("invalid sparse map"))
    };
    let n = line()?;
    if n > 1 << 20 {
        return Err(bad("sparse map is too large"));
    }
    let mut map = Vec::with_capacity(n as usize);
    for _ in 0..n {
        map.push((line()?, line()?));
    }
    let pad = (512 - consumed % 512) % 512;
    io::copy(&mut data.take(pad), &mut io::sink())?;
    Ok(map)
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

    /// pax sparse 항목: 확장 헤더와, (1.0이면 맵 + 패딩이 앞에 붙은) 데이터.
    fn sparse(b: &mut tar::Builder<Vec<u8>>, pax: &[(&str, &str)], stored: &str, data: &[u8]) {
        b.append_pax_extensions(pax.iter().map(|(k, v)| (*k, v.as_bytes())))
            .unwrap();
        file(b, stored, 0o644, data);
    }

    #[test]
    fn pax_sparse_files_are_expanded() {
        let exe = executable_name();
        // 1.0 (macOS tar): 조각 두 개 [0..3) "abc", [10..12) "xy", 실제 크기 16.
        let mut v10 = b"2\n0\n3\n10\n2\n".to_vec();
        v10.resize(512, 0);
        v10.extend_from_slice(b"abcxy");
        // 0.1 (GNU tar): 맵이 헤더에 있다.
        let (root, r) = run(
            "sparse",
            &tar_gz(|b| {
                file(b, &exe, 0o755, b"x");
                sparse(
                    b,
                    &[
                        ("GNU.sparse.major", "1"),
                        ("GNU.sparse.minor", "0"),
                        ("GNU.sparse.name", "kernel"),
                        ("GNU.sparse.realsize", "16"),
                    ],
                    "GNUSparseFile.0/kernel",
                    &v10,
                );
                sparse(
                    b,
                    &[
                        ("GNU.sparse.name", "sub/disk"),
                        ("GNU.sparse.size", "8"),
                        ("GNU.sparse.map", "2,2,6,1"),
                    ],
                    "GNUSparseFile.0/disk",
                    b"hiz",
                );
            }),
        );
        let dir = r.unwrap();
        assert_eq!(
            fs::read(dir.join("kernel")).unwrap(),
            b"abc\0\0\0\0\0\0\0xy\0\0\0\0"
        );
        assert_eq!(fs::read(dir.join("sub/disk")).unwrap(), b"\0\0hi\0\0z\0");
        assert!(!dir.join("GNUSparseFile.0").exists());
        fs::remove_dir_all(root).unwrap();

        // 모르는 버전, 범위를 넘는 맵, 안전하지 않은 이름은 거부한다.
        for (name, pax) in [
            (
                "v00",
                vec![("GNU.sparse.name", "k"), ("GNU.sparse.numblocks", "1")],
            ),
            (
                "range",
                vec![
                    ("GNU.sparse.name", "k"),
                    ("GNU.sparse.size", "2"),
                    ("GNU.sparse.map", "0,3"),
                ],
            ),
            (
                "evil",
                vec![
                    ("GNU.sparse.name", "../k"),
                    ("GNU.sparse.size", "1"),
                    ("GNU.sparse.map", "0,1"),
                ],
            ),
        ] {
            let (root, r) = run(
                name,
                &tar_gz(|b| {
                    file(b, &exe, 0o755, b"x");
                    sparse(b, &pax, "GNUSparseFile.0/k", b"abc");
                }),
            );
            assert!(matches!(r, Err(RepoError::Archive(_))), "{name}: {r:?}");
            fs::remove_dir_all(root).unwrap();
        }
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
