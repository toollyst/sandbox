//! 레이어 tar 읽기와 파일 내용 임시 저장.
//!
//! tar를 한 번만 읽는다. 헤더는 [`LayerEntry`]로 바꾸고, 일반 파일의 내용은 작업 공간에
//! `<레이어>-<순번>` 이름으로 저장한다. 호스트 파일 이름이 원래 경로와 무관하므로
//! 대소문자 구분, 소유자, 특수 파일 문제가 생기지 않는다.
//!
//! - 경로와 링크 대상은 UTF-8이어야 한다. 아니면 오류다.
//! - xattr은 PAX `SCHILY.xattr.*`에서 읽는다.
//! - 지원하는 항목 종류: 일반 파일, 디렉터리, 심볼릭 링크, 하드 링크, 문자·블록 장치, FIFO.
//!   PAX 전역 헤더는 무시하고, 그 밖의 종류(GNU sparse 등)는 오류다.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read};
use std::path::{Path, PathBuf};

use tar::EntryType;

use sbx_rootfs::{ContentRef, ContentSource, EntryKind, LayerEntry, Meta};

use crate::error::ManageError;

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
    let mut archive = tar::Archive::new(reader);
    let mut out = Vec::new();
    for (index, entry) in archive.entries().map_err(io)?.enumerate() {
        let mut entry = entry.map_err(io)?;
        let header = entry.header();
        let entry_type = header.entry_type();
        if entry_type == EntryType::XGlobalHeader {
            continue;
        }
        let path = utf8(layer, "path", &entry.path_bytes())?;
        let mut meta = Meta {
            mode: header.mode().map_err(io)? & 0o7777,
            uid: u32::try_from(header.uid().map_err(io)?)
                .map_err(|_| err(layer, format!("uid too large at {path}")))?,
            gid: u32::try_from(header.gid().map_err(io)?)
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
        let link = |e: &tar::Entry<'_, _>| -> Result<String, ManageError> {
            let name = e
                .link_name_bytes()
                .ok_or_else(|| err(layer, format!("link without target at {path}")))?;
            utf8(layer, "link target", &name)
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
            EntryType::Symlink => EntryKind::Symlink(link(&entry)?),
            EntryType::Link => EntryKind::Hardlink(link(&entry)?),
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
        if let Some(exts) = entry.pax_extensions().map_err(io)? {
            for ext in exts {
                let ext = ext.map_err(io)?;
                if let Some(name) = ext.key().ok().and_then(|k| k.strip_prefix("SCHILY.xattr.")) {
                    meta.xattrs
                        .insert(name.to_owned(), ext.value_bytes().to_vec());
                }
            }
        }
        out.push(LayerEntry { path, kind, meta });
    }
    Ok(out)
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
