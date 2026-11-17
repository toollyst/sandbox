//! 레이어 tar 읽기와 파일 내용 임시 저장.
//!
//! tar를 한 번만 읽는다. 헤더는 [`LayerEntry`]로 바꾸고, 일반 파일의 내용은 작업 공간에
//! `<레이어>-<순번>` 이름으로 저장한다. 호스트 파일 이름이 원래 경로와 무관하므로
//! 대소문자 구분, 소유자, 특수 파일 문제가 생기지 않는다.
//!
//! - 경로와 링크 대상은 UTF-8이어야 한다. 아니면 오류다.
//! - xattr은 PAX `SCHILY.xattr.*`에서 읽는다. PAX 기록은 길이로 나누어 읽는다(값에 줄바꿈이 있어도 된다,
//!   [`tarlimit`]). `tar` 크레이트가 본 경로·링크 대상·크기·uid·gid가 그 기록과 다르면 읽는 쪽마다 다른 레이어이므로
//!   오류다.
//! - 지원하는 항목 종류: 일반 파일, 디렉터리, 심볼릭 링크, 하드 링크, 문자·블록 장치, FIFO.
//!   PAX 전역 헤더는 무시하고, 그 밖의 종류(GNU sparse 등)는 오류다.
//! - 악의적인 레이어가 메모리를 채우지 못하게 상한을 둔다: 확장 헤더 하나 [`tarlimit::MAX_EXTENSION`](읽기 전에
//!   본다), 경로·링크 대상 [`MAX_PATH`], xattr 이름 [`MAX_XATTR_NAME`]·값 [`MAX_XATTR_VALUE`](Linux의 한도),
//!   레이어 하나의 경로·링크·xattr 합계 [`MAX_METADATA`]. 파일 내용은 메모리가 아니라 작업 공간에 쓴다.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use tar::EntryType;

use sbx_rootfs::{ContentRef, ContentSource, EntryKind, LayerEntry, Meta};

use crate::error::ManageError;
use crate::tarlimit;

/// 경로와 링크 대상의 길이 상한(바이트). Linux `PATH_MAX`.
pub const MAX_PATH: usize = 4096;
/// xattr 이름의 길이 상한(바이트). Linux `XATTR_NAME_MAX`.
pub const MAX_XATTR_NAME: usize = 255;
/// xattr 값의 길이 상한(바이트). Linux `XATTR_SIZE_MAX`.
pub const MAX_XATTR_VALUE: usize = 64 << 10;
/// 레이어 하나에서 메모리에 두는 경로·링크 대상·xattr의 합계 상한(바이트).
pub const MAX_METADATA: usize = 256 << 20;

fn spool_name(layer: usize, index: usize) -> String {
    format!("{layer}-{index}")
}

fn err(index: usize, reason: impl Into<String>) -> ManageError {
    ManageError::Layer {
        index,
        reason: reason.into(),
    }
}

fn utf8(layer: usize, what: &str, bytes: &[u8]) -> Result<String, ManageError> {
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        err(
            layer,
            format!("{what} is not UTF-8: {:?}", String::from_utf8_lossy(bytes)),
        )
    })
}

/// 레이어 하나를 읽는다. 일반 파일 내용은 `spool`에 저장한다.
pub fn read(layer: usize, reader: impl Read, spool: &Path) -> Result<Vec<LayerEntry>, ManageError> {
    let io = |e: io::Error| err(layer, e.to_string());
    let limited = tarlimit::Limited::new(reader);
    let slot = limited.pax_slot();
    let mut archive = tar::Archive::new(limited);
    let mut out = Vec::new();
    let mut budget = MAX_METADATA;
    let mut spend = |n: usize, path: &str| -> Result<(), ManageError> {
        budget = budget.checked_sub(n).ok_or_else(|| {
            err(
                layer,
                format!("more than {MAX_METADATA} bytes of names and xattrs (at {path})"),
            )
        })?;
        Ok(())
    };
    for (index, entry) in archive.entries().map_err(io)?.enumerate() {
        let mut entry = entry.map_err(io)?;
        let raw_pax = slot.take();
        let pax = tarlimit::parse_pax(&raw_pax).map_err(|e| err(layer, e))?;
        let entry_type = entry.header().entry_type();
        if entry_type == EntryType::XGlobalHeader {
            continue;
        }
        let fixed = fix_from_pax(layer, &entry, &raw_pax, &pax)?;
        let header = entry.header();
        let path_bytes = &fixed.path;
        if path_bytes.len() > MAX_PATH {
            return Err(err(
                layer,
                format!("a path of {} bytes; at most {MAX_PATH}", path_bytes.len()),
            ));
        }
        let path = utf8(layer, "path", path_bytes)?;
        spend(path.len(), &path)?;
        let mut meta = Meta {
            mode: header.mode().map_err(io)? & 0o7777,
            uid: u32::try_from(fixed.uid)
                .map_err(|_| err(layer, format!("uid too large at {path}")))?,
            gid: u32::try_from(fixed.gid)
                .map_err(|_| err(layer, format!("gid too large at {path}")))?,
            mtime: i64::try_from(header.mtime().map_err(io)?).unwrap_or(i64::MAX),
            xattrs: BTreeMap::new(),
        };
        let device = |h: &tar::Header| -> Result<(u32, u32), ManageError> {
            Ok((
                h.device_major().map_err(io)?.unwrap_or(0),
                h.device_minor().map_err(io)?.unwrap_or(0),
            ))
        };
        let link = || -> Result<String, ManageError> {
            let name = fixed
                .link
                .as_deref()
                .ok_or_else(|| err(layer, format!("link without target at {path}")))?;
            if name.len() > MAX_PATH {
                return Err(err(
                    layer,
                    format!(
                        "a link target of {} bytes at {path}; at most {MAX_PATH}",
                        name.len()
                    ),
                ));
            }
            utf8(layer, "link target", name)
        };
        let kind = match entry_type {
            EntryType::Regular | EntryType::Continuous => {
                let file = File::create(spool.join(spool_name(layer, index))).map_err(io)?;
                let mut w = BufWriter::new(file);
                let size = io::copy(&mut entry, &mut w).map_err(io)?;
                w.into_inner().map_err(|e| io(e.into_error()))?;
                EntryKind::File(ContentRef { layer, index, size })
            }
            EntryType::Directory => EntryKind::Dir,
            EntryType::Symlink => {
                let t = link()?;
                spend(t.len(), &path)?;
                EntryKind::Symlink(t)
            }
            EntryType::Link => {
                let t = link()?;
                spend(t.len(), &path)?;
                EntryKind::Hardlink(t)
            }
            EntryType::Char => {
                let (major, minor) = device(entry.header())?;
                EntryKind::CharDevice { major, minor }
            }
            EntryType::Block => {
                let (major, minor) = device(entry.header())?;
                EntryKind::BlockDevice { major, minor }
            }
            EntryType::Fifo => EntryKind::Fifo,
            other => {
                return Err(err(
                    layer,
                    format!("unsupported tar entry type {other:?} at {path}"),
                ));
            }
        };
        for (key, value) in &pax {
            if let Some(name) = std::str::from_utf8(key)
                .ok()
                .and_then(|k| k.strip_prefix("SCHILY.xattr."))
            {
                if name.len() > MAX_XATTR_NAME || value.len() > MAX_XATTR_VALUE {
                    return Err(err(
                        layer,
                        format!(
                            "an xattr at {path} is too large ({} byte name, {} byte value; at most {MAX_XATTR_NAME} and {MAX_XATTR_VALUE})",
                            name.len(),
                            value.len()
                        ),
                    ));
                }
                spend(name.len() + value.len(), &path)?;
                meta.xattrs.insert(name.to_owned(), value.clone());
            }
        }
        out.push(LayerEntry { path, kind, meta });
    }
    Ok(out)
}

/// 항목의 경로, 링크 대상, uid, gid를 길이로 읽은 PAX 기록대로 정한다.
///
/// `tar` 크레이트는 기록을 줄바꿈으로 나누어 찾는다. 그래서 값에 줄바꿈이 든 기록(바이너리 xattr) 뒤의 기록을 놓치거나
/// (빈 줄에서 그친다), 값 안에 기록처럼 보이는 줄을 기록으로 본다.
///
/// - 놓친 `path`·`linkpath`·`uid`·`gid`는 기록대로 고친다. GNU 긴 이름이 있으면 그것이 이긴다(크레이트와 Go
///   `archive/tar`가 그렇다).
/// - 놓친 `size`는 고칠 수 없다(크레이트가 이미 다른 크기로 읽었다): 오류.
/// - 기록이 아닌 줄을 기록으로 보았으면 오류다(읽는 쪽마다 다른 레이어다).
struct Fixed {
    path: Vec<u8>,
    link: Option<Vec<u8>>,
    uid: u64,
    gid: u64,
}

fn fix_from_pax<R: Read>(
    layer: usize,
    entry: &tar::Entry<'_, R>,
    raw: &[u8],
    pax: &tarlimit::PaxRecords,
) -> Result<Fixed, ManageError> {
    let ambiguous = |key: &str| {
        err(
            layer,
            format!(
                "the PAX {key} record of {:?} is ambiguous (a record value holds a line that reads as another record)",
                String::from_utf8_lossy(&entry.path_bytes())
            ),
        )
    };
    let io = |e: io::Error| err(layer, e.to_string());
    for key in ["path", "linkpath", "size", "uid", "gid"] {
        let mut real = pax.iter().filter(|(k, _)| k == key.as_bytes());
        let forged = tar::PaxExtensions::new(raw)
            .filter_map(Result::ok)
            .filter(|x| x.key_bytes() == key.as_bytes())
            .any(|x| !real.any(|(_, v)| v == x.value_bytes()));
        if forged {
            return Err(ambiguous(key));
        }
    }
    let number = |key: &str, v: &[u8]| {
        std::str::from_utf8(v)
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .ok_or_else(|| {
                err(
                    layer,
                    format!("PAX {key} {:?} is not a number", String::from_utf8_lossy(v)),
                )
            })
    };
    if let Some(v) = tarlimit::pax_value(pax, b"size")
        && number("size", v)? != entry.size()
    {
        return Err(ambiguous("size"));
    }
    let h = entry.header();
    // 크레이트가 헤더의 것을 쓰고 있으면(놓쳤으면) 기록대로. 아니면 크레이트가 본 것(기록 또는 GNU 긴 이름)이다.
    let path = match tarlimit::pax_value(pax, b"path") {
        Some(v) if entry.path_bytes() == h.path_bytes() => v.to_vec(),
        _ => entry.path_bytes().into_owned(),
    };
    let link = match tarlimit::pax_value(pax, b"linkpath") {
        Some(v) if entry.link_name_bytes() == h.link_name_bytes() => Some(v.to_vec()),
        _ => entry.link_name_bytes().map(std::borrow::Cow::into_owned),
    };
    let uid = match tarlimit::pax_value(pax, b"uid") {
        Some(v) => number("uid", v)?,
        None => h.uid().map_err(io)?,
    };
    let gid = match tarlimit::pax_value(pax, b"gid") {
        Some(v) => number("gid", v)?,
        None => h.gid().map_err(io)?,
    };
    Ok(Fixed {
        path,
        link,
        uid,
        gid,
    })
}

/// 층들의 파일 내용: blob에서 읽은 층은 작업 공간([`Spool`])에, 층 디스크만 있는 층(빌드가 만든 것)은 그 디스크에
/// 있다.
pub struct Contents {
    spool: Spool,
    disks: sbx_rootfs::erofs::read::DiskContents,
}

impl Contents {
    /// `work`에 저장한 내용과 디스크 `disks`.
    pub fn new(work: &Path, disks: sbx_rootfs::erofs::read::DiskContents) -> Self {
        Self {
            spool: Spool::new(work),
            disks,
        }
    }
}

impl Contents {
    /// [`ContentSource::open`]과 같고, 돌려받은 것이 이것을 빌리지 않는다.
    pub fn open_owned(&self, content: &ContentRef) -> io::Result<Box<dyn Read + Send>> {
        if self.disks.has(content.layer) {
            self.disks.open_owned(content)
        } else {
            let file = File::open(
                self.spool
                    .dir
                    .join(spool_name(content.layer, content.index)),
            )?;
            Ok(Box::new(BufReader::new(file)))
        }
    }
}

impl ContentSource for Contents {
    fn open(&self, content: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        if self.disks.has(content.layer) {
            self.disks.open(content)
        } else {
            self.spool.open(content)
        }
    }
}

/// 작업 공간에 저장한 파일 내용.
pub struct Spool {
    dir: PathBuf,
}

impl Spool {
    /// `dir`(레이어를 읽을 때 준 작업 공간)에 저장한 내용을 읽는다.
    pub fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_owned(),
        }
    }
}

impl ContentSource for Spool {
    fn open(&self, content: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        let file = File::open(self.dir.join(spool_name(content.layer, content.index)))?;
        Ok(Box::new(BufReader::new(file)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: usize = 512;

    /// PAX 확장 헤더(`records`, 적은 순서 그대로) 뒤에 `name`의 일반 파일 하나. 헤더의 크기 필드는 `header_size`, 내용은
    /// `data`다.
    fn one_file(records: &[(&str, &[u8])], name: &str, header_size: u64, data: &[u8]) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        b.append_pax_extensions(records.iter().copied()).unwrap();
        let mut raw = b.into_inner().unwrap();
        raw.truncate(raw.len() - 2 * BLOCK);
        let mut h = tar::Header::new_ustar();
        h.set_path(name).unwrap();
        h.set_size(header_size);
        h.set_mode(0o644);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        h.set_cksum();
        raw.extend_from_slice(h.as_bytes());
        raw.extend_from_slice(data);
        raw.resize(raw.len().div_ceil(BLOCK) * BLOCK, 0);
        raw.extend_from_slice(&[0; 2 * BLOCK]);
        raw
    }

    fn read_one(raw: &[u8]) -> Result<Vec<LayerEntry>, ManageError> {
        let dir = std::env::temp_dir().join(format!(
            "sbx-layer-pax-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let r = read(0, raw, &dir);
        let _ = std::fs::remove_dir_all(&dir);
        r
    }

    /// xattr 값은 바이너리다. gid 10의 ACL 항목에는 0x0a(줄바꿈)가 들어 있다. Go `archive/tar`가 쓰는 순서(키 순서,
    /// `SCHILY.xattr.*`가 `path`보다 앞)라서 줄바꿈 뒤의 `path`도 기록대로 읽어야 한다.
    #[test]
    fn binary_xattr_values_with_newlines_read_back() {
        let acl: &[u8] = &[
            2, 0, 0, 0, 1, 0, 6, 0, 255, 255, 255, 255, 8, 0, 5, 0, 10, 0, 0, 0, 32, 0, 5, 0, 255,
            255, 255, 255,
        ];
        let long = format!("usr/share/{}/file", "n".repeat(120));
        let raw = one_file(
            &[
                ("SCHILY.xattr.system.posix_acl_access", acl),
                ("SCHILY.xattr.user.text", b"two\n\nlines\n"),
                ("path", long.as_bytes()),
                ("uid", b"4242"),
            ],
            "short-name",
            3,
            b"abc",
        );
        let got = read_one(&raw).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, long);
        assert_eq!(got[0].meta.uid, 4242);
        assert_eq!(got[0].meta.xattrs["system.posix_acl_access"], acl);
        assert_eq!(got[0].meta.xattrs["user.text"], b"two\n\nlines\n");
    }

    /// 경로 자체에 줄바꿈이 있어도(Linux 파일 이름에 올 수 있다) PAX `path` 기록대로 읽는다. 크레이트는 그 기록을
    /// 놓치고 머리의 이름을 쓴다.
    #[test]
    fn a_pax_path_with_a_newline_reads_back() {
        let long = format!("{}\n{}/.wh..wh..opq", "d".repeat(60), "e".repeat(50));
        let raw = one_file(&[("path", long.as_bytes())], "short-name", 0, b"");
        let got = read_one(&raw).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, long);
    }

    /// 값 안의 줄이 기록처럼 읽히면(`tar` 크레이트는 그것을 경로로 쓴다) 받지 않는다.
    #[test]
    fn a_record_hidden_in_a_value_is_refused() {
        let raw = one_file(&[("SCHILY.xattr.user.a", b"x\n11 path=ev")], "real", 0, b"");
        let e = read_one(&raw).unwrap_err().to_string();
        assert!(
            e.contains("PAX path record") && e.contains("ambiguous"),
            "{e}"
        );
    }

    /// 줄바꿈이 든 값 뒤의 `size`는 크레이트가 놓쳐 다른 크기로 읽으므로 받지 않는다.
    #[test]
    fn a_size_after_a_newline_value_is_refused() {
        let raw = one_file(
            &[("SCHILY.xattr.user.a", b"x\n"), ("size", b"5")],
            "f",
            0,
            b"hello",
        );
        let e = read_one(&raw).unwrap_err().to_string();
        assert!(e.contains("size"), "{e}");
    }

    #[test]
    fn a_malformed_pax_header_is_refused() {
        let mut raw = one_file(&[("SCHILY.xattr.user.a", b"v")], "f", 0, b"");
        // 첫 기록의 길이를 망가뜨린다("25 SCHILY..."의 "25"를 "99"로).
        let at = raw.windows(3).position(|w| w == b"25 ").unwrap();
        raw[at..at + 2].copy_from_slice(b"99");
        assert!(read_one(&raw).is_err());
    }
}
