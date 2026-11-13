//! EROFS 읽기: 이 크레이트가 쓴 디스크(층 디스크, 합친 디스크)를 다시 레이어 항목으로 읽는다.
//!
//! 쓰는 쪽([`super::ErofsBuilder`])이 만드는 모양만 읽는다: extended inode, `FLAT_PLAIN`, inline xattr, 블록 4096.
//! 다른 모양(compact inode, 압축, chunk, 공유 xattr)은 [`RootfsError::Unsupported`]다.
//!
//! 쓰는 곳: 빌드가 만든 층에는 압축 blob이 없다(고른 대상의 형태 하나만 둔다). 그 층을 다른 층과 합치거나
//! (`max_disks`를 넘는 이미지, `merged/`), `COPY --from`으로 그 안의 파일을 꺼낼 때 층 디스크를 읽는다.
//!
//! **나오는 항목은 OCI 층의 모양이다**(그래서 [`MergedTree::apply_layer`](crate::MergedTree::apply_layer)에 그대로 넣는다).
//! 층 디스크는 overlay 아래층 모양이므로([`crate::lower`]) 거꾸로 바꾼다.
//!
//! - 문자 장치 0/0은 whiteout(`<부모>/.wh.<이름>`, 크기 0인 일반 파일)이 된다. overlay도 그것을 가려진 것으로 본다.
//! - xattr [`OPAQUE_XATTR`](crate::lower::OPAQUE_XATTR)` = "y"`인 디렉터리는 그 디렉터리 항목 뒤에
//!   `<디렉터리>/.wh..wh..opq`가 온다. 그 xattr은 항목에서 뺀다. 다른 `trusted.overlay.*`도 뺀다.
//! - 같은 inode를 가리키는 두 번째 이름부터는 처음 이름으로의 하드 링크다.
//! - 디렉터리는 그 안의 항목보다 먼저, 이름(바이트) 순서로 온다. 루트는 항목으로 내지 않는다.
//!
//! **믿지 않는다**: 디스크는 캐시 안의 파일이라 이 사용자 말고는 바꾸지 못하지만, 깨졌을 수 있다. 모든 자리와 길이를
//! 파일 크기 안에서 확인하고, 디렉터리 고리(같은 디렉터리 inode를 두 번 만남), 이름(`/`, NUL, `.`, `..`, UTF-8이
//! 아닌 것), 경로 길이([`MAX_PATH`]), 항목 수([`MAX_ENTRIES`])를 오류로 막는다. 패닉하지 않는다.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::{
    BLOCK, FORMAT_EXTENDED_PLAIN, INODE_SIZE, MAGIC, S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK,
    S_IFREG, SLOT, SUPER_OFFSET, XATTR_EXACT, XATTR_HEADER_SIZE, XATTR_PREFIXES,
};
use crate::lower::{OPAQUE_XATTR, OVERLAY_XATTR_PREFIX};
use crate::{ContentRef, ContentSource, EntryKind, LayerEntry, Meta, RootfsError};

/// 경로 길이 상한(바이트, Linux `PATH_MAX`).
pub const MAX_PATH: usize = 4096;
/// 디스크 하나에서 읽는 항목 수 상한.
pub const MAX_ENTRIES: usize = 4 << 20;

fn corrupt(m: impl Into<String>) -> RootfsError {
    RootfsError::Io(io::Error::new(io::ErrorKind::InvalidData, m.into()))
}

/// 읽은 디스크 하나: OCI 층 모양의 항목들과, 파일 내용의 자리(항목 순번 → 파일 안 바이트 자리).
#[derive(Debug, Clone)]
pub struct ReadLayer {
    /// 항목들. 파일 내용은 [`ContentRef`]`{ layer, index, size }`이고 `index`는 이 목록의 순번이다.
    pub entries: Vec<LayerEntry>,
    /// 항목 순번 → 내용이 시작하는 파일 안 자리.
    pub offsets: HashMap<usize, u64>,
}

struct Disk<'a, R> {
    r: &'a mut R,
    len: u64,
    meta_base: u64,
}

struct Inode {
    mode: u16,
    size: u64,
    i_u: u32,
    uid: u32,
    gid: u32,
    mtime: i64,
    nlink: u32,
    xattrs: Vec<(String, Vec<u8>)>,
}

impl<R: Read + Seek> Disk<'_, R> {
    fn bytes(&mut self, at: u64, len: u64) -> Result<Vec<u8>, RootfsError> {
        at.checked_add(len)
            .filter(|e| *e <= self.len)
            .ok_or_else(|| corrupt(format!("read of {len} bytes at {at} is outside the disk")))?;
        self.r.seek(SeekFrom::Start(at))?;
        let mut b = vec![0u8; len as usize];
        self.r.read_exact(&mut b)?;
        Ok(b)
    }

    fn inode(&mut self, nid: u64) -> Result<Inode, RootfsError> {
        let at = nid
            .checked_mul(SLOT)
            .and_then(|o| o.checked_add(self.meta_base))
            .ok_or_else(|| corrupt(format!("inode {nid} is outside the disk")))?;
        let h = self.bytes(at, INODE_SIZE)?;
        let u16_at = |o: usize| u16::from_le_bytes([h[o], h[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes(h[o..o + 4].try_into().unwrap_or_default());
        let u64_at = |o: usize| u64::from_le_bytes(h[o..o + 8].try_into().unwrap_or_default());
        if u16_at(0) != FORMAT_EXTENDED_PLAIN {
            return Err(RootfsError::Unsupported(format!(
                "inode {nid}: only extended FLAT_PLAIN inodes are read (format {:#x})",
                u16_at(0)
            )));
        }
        let icount = usize::from(u16_at(2));
        let xattrs = if icount == 0 {
            Vec::new()
        } else {
            let size = XATTR_HEADER_SIZE + (icount - 1) * 4;
            let body = self.bytes(at + INODE_SIZE, size as u64)?;
            parse_xattrs(&body).map_err(|m| corrupt(format!("inode {nid}: {m}")))?
        };
        Ok(Inode {
            mode: u16_at(4),
            size: u64_at(8),
            i_u: u32_at(16),
            uid: u32_at(24),
            gid: u32_at(28),
            mtime: i64::try_from(u64_at(32)).unwrap_or(i64::MAX),
            nlink: u32_at(44),
            xattrs,
        })
    }

    fn data_at(&self, i: &Inode) -> Result<u64, RootfsError> {
        let start = u64::from(i.i_u)
            .checked_mul(BLOCK)
            .filter(|s| s.checked_add(i.size).is_some_and(|e| e <= self.len))
            .ok_or_else(|| corrupt("data is outside the disk"))?;
        Ok(start)
    }

    /// 디렉터리 항목들: (이름, nid). `.`과 `..`는 뺀다.
    fn dir(&mut self, i: &Inode) -> Result<Vec<(String, u64)>, RootfsError> {
        let start = self.data_at(i)?;
        let data = self.bytes(start, i.size)?;
        let mut out = Vec::new();
        for chunk in data.chunks(BLOCK as usize) {
            if chunk.len() < 12 {
                return Err(corrupt("short directory block"));
            }
            let first = usize::from(u16::from_le_bytes([chunk[8], chunk[9]]));
            if first < 12 || first % 12 != 0 || first > chunk.len() {
                return Err(corrupt("bad directory block"));
            }
            let n = first / 12;
            for k in 0..n {
                let d = &chunk[k * 12..k * 12 + 12];
                let nid = u64::from_le_bytes(d[0..8].try_into().unwrap_or_default());
                let off = usize::from(u16::from_le_bytes([d[8], d[9]]));
                let end = if k + 1 < n {
                    usize::from(u16::from_le_bytes([chunk[k * 12 + 20], chunk[k * 12 + 21]]))
                } else {
                    chunk.len()
                };
                if off < first || end < off || end > chunk.len() {
                    return Err(corrupt("bad directory entry"));
                }
                let raw = &chunk[off..end];
                // 블록의 마지막 이름 뒤는 0으로 채워져 있다.
                let raw = match raw.iter().position(|b| *b == 0) {
                    Some(p) if k + 1 == n => &raw[..p],
                    _ => raw,
                };
                let name = std::str::from_utf8(raw)
                    .map_err(|_| corrupt("a name is not UTF-8"))?
                    .to_owned();
                if name == "." || name == ".." {
                    continue;
                }
                if name.is_empty() || name.contains(['/', '\0']) {
                    return Err(corrupt(format!("bad name {name:?}")));
                }
                out.push((name, nid));
            }
        }
        Ok(out)
    }
}

fn parse_xattrs(body: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut at = XATTR_HEADER_SIZE;
    while at + 4 <= body.len() {
        let name_len = usize::from(body[at]);
        let index = body[at + 1];
        let value_len = usize::from(u16::from_le_bytes([body[at + 2], body[at + 3]]));
        if name_len == 0 && index == 0 && value_len == 0 {
            break;
        }
        let name_end = at + 4 + name_len;
        let value_end = name_end + value_len;
        if value_end > body.len() {
            return Err("xattr outside its inode".into());
        }
        let suffix = std::str::from_utf8(&body[at + 4..name_end])
            .map_err(|_| "xattr name is not UTF-8".to_owned())?;
        let name = if let Some((n, _)) = XATTR_EXACT.iter().find(|(_, i)| *i == index) {
            if !suffix.is_empty() {
                return Err("xattr name after an exact index".into());
            }
            (*n).to_owned()
        } else if let Some((p, _)) = XATTR_PREFIXES.iter().find(|(_, i)| *i == index) {
            format!("{p}{suffix}")
        } else {
            return Err(format!("unknown xattr name index {index}"));
        };
        out.push((name, body[name_end..value_end].to_vec()));
        at = value_end.next_multiple_of(4);
    }
    Ok(out)
}

/// `r`(EROFS 디스크 하나)를 OCI 층 모양의 항목으로 읽는다. `layer`는 내용 [`ContentRef`]에 적을 층 번호다.
/// 규칙은 모듈 문서에 있다.
pub fn read_layer(r: &mut (impl Read + Seek), layer: usize) -> Result<ReadLayer, RootfsError> {
    let len = r.seek(SeekFrom::End(0))?;
    let mut d = Disk {
        r,
        len,
        meta_base: 0,
    };
    let sb = d.bytes(SUPER_OFFSET as u64, 64)?;
    if u32::from_le_bytes(sb[0..4].try_into().unwrap_or_default()) != MAGIC {
        return Err(corrupt("not an EROFS disk"));
    }
    if sb[12] != 12 {
        return Err(RootfsError::Unsupported(format!(
            "block size 2^{} (only 4096 is read)",
            sb[12]
        )));
    }
    let root = u64::from(u16::from_le_bytes([sb[14], sb[15]]));
    d.meta_base = u64::from(u32::from_le_bytes(
        sb[40..44].try_into().unwrap_or_default(),
    )) * BLOCK;

    let mut out = ReadLayer {
        entries: Vec::new(),
        offsets: HashMap::new(),
    };
    let mut seen_dirs: HashSet<u64> = HashSet::new();
    let mut first_name: HashMap<u64, String> = HashMap::new();
    let root_inode = d.inode(root)?;
    if u32::from(root_inode.mode) & 0o170000 != S_IFDIR {
        return Err(corrupt("the root is not a directory"));
    }
    seen_dirs.insert(root);
    // (디렉터리 경로, 그 항목들). 깊이 우선이되 디렉터리 항목 바로 뒤에 그 안을 낸다.
    let mut stack: Vec<(String, std::vec::IntoIter<(String, u64)>)> = Vec::new();
    let mut children = d.dir(&root_inode)?;
    children.sort();
    stack.push((String::new(), children.into_iter()));
    while let Some((dir, iter)) = stack.last_mut() {
        let Some((name, nid)) = iter.next() else {
            stack.pop();
            continue;
        };
        let path = if dir.is_empty() {
            name.clone()
        } else {
            format!("{dir}/{name}")
        };
        if path.len() > MAX_PATH {
            return Err(corrupt(format!("a path longer than {MAX_PATH} bytes")));
        }
        if out.entries.len() >= MAX_ENTRIES {
            return Err(corrupt(format!("more than {MAX_ENTRIES} entries")));
        }
        let i = d.inode(nid)?;
        let mut xattrs = std::collections::BTreeMap::new();
        let mut opaque = false;
        for (k, v) in &i.xattrs {
            if k == OPAQUE_XATTR {
                opaque = v.as_slice() == b"y";
            } else if !k.starts_with(OVERLAY_XATTR_PREFIX) {
                xattrs.insert(k.clone(), v.clone());
            }
        }
        let meta = Meta {
            mode: u32::from(i.mode) & 0o7777,
            uid: i.uid,
            gid: i.gid,
            mtime: i.mtime,
            xattrs,
        };
        let ftype = u32::from(i.mode) & 0o170000;
        let index = out.entries.len();
        let kind = match ftype {
            S_IFDIR => {
                if !seen_dirs.insert(nid) {
                    return Err(corrupt(format!("{path:?}: a directory loop")));
                }
                let mut c = d.dir(&i)?;
                c.sort();
                out.entries.push(LayerEntry {
                    path: path.clone(),
                    kind: EntryKind::Dir,
                    meta,
                });
                if opaque {
                    out.entries
                        .push(whiteout(format!("{path}/.wh..wh..opq"), layer));
                }
                stack.push((path, c.into_iter()));
                continue;
            }
            S_IFREG => match first_name.get(&nid) {
                Some(first) if i.nlink > 1 => EntryKind::Hardlink(first.clone()),
                _ => {
                    if i.nlink > 1 {
                        first_name.insert(nid, path.clone());
                    }
                    let at = d.data_at(&i)?;
                    out.offsets.insert(index, at);
                    EntryKind::File(ContentRef {
                        layer,
                        index,
                        size: i.size,
                    })
                }
            },
            S_IFLNK => {
                if i.size as usize > MAX_PATH {
                    return Err(corrupt(format!("{path:?}: link target too long")));
                }
                let at = d.data_at(&i)?;
                let t = d.bytes(at, i.size)?;
                EntryKind::Symlink(
                    String::from_utf8(t).map_err(|_| corrupt("a link target is not UTF-8"))?,
                )
            }
            S_IFCHR | S_IFBLK => {
                let (major, minor) = decode_dev(i.i_u);
                if ftype == S_IFCHR && major == 0 && minor == 0 {
                    let (parent, base) = match path.rsplit_once('/') {
                        Some((p, b)) => (format!("{p}/"), b),
                        None => (String::new(), path.as_str()),
                    };
                    let wh = format!("{parent}.wh.{base}");
                    out.entries.push(whiteout(wh, layer));
                    continue;
                }
                if ftype == S_IFCHR {
                    EntryKind::CharDevice { major, minor }
                } else {
                    EntryKind::BlockDevice { major, minor }
                }
            }
            S_IFIFO => EntryKind::Fifo,
            _ => return Err(corrupt(format!("{path:?}: unknown file type"))),
        };
        out.entries.push(LayerEntry { path, kind, meta });
    }
    Ok(out)
}

/// whiteout이나 opaque 표시: 크기 0인 일반 파일(내용을 읽지 않는다).
fn whiteout(path: String, layer: usize) -> LayerEntry {
    LayerEntry {
        path,
        kind: EntryKind::File(ContentRef {
            layer,
            index: usize::MAX,
            size: 0,
        }),
        meta: Meta {
            mode: 0,
            ..Meta::implicit_dir()
        },
    }
}

/// 커널 `new_decode_dev`.
fn decode_dev(v: u32) -> (u32, u32) {
    let major = (v & 0xfff00) >> 8;
    let minor = (v & 0xff) | ((v >> 12) & 0xfff00);
    (major, minor)
}

/// 여러 디스크의 파일 내용: ([`ContentRef::layer`], 항목 순번) → (디스크 파일, 자리). 크기 0인 것(whiteout)은
/// 빈 내용이다.
#[derive(Debug, Clone, Default)]
pub struct DiskContents {
    disks: HashMap<usize, (PathBuf, HashMap<usize, u64>)>,
}

impl DiskContents {
    /// 빈 것.
    pub fn new() -> Self {
        Self::default()
    }

    /// 그 층 번호의 디스크가 있는지.
    pub fn has(&self, layer: usize) -> bool {
        self.disks.contains_key(&layer)
    }

    /// 디스크 하나를 더한다(그 디스크의 [`ReadLayer::offsets`]).
    pub fn add(&mut self, layer: usize, disk: &Path, offsets: HashMap<usize, u64>) {
        self.disks.insert(layer, (disk.to_owned(), offsets));
    }
}

impl ContentSource for DiskContents {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        if c.size == 0 {
            return Ok(Box::new(io::empty()));
        }
        let (path, offsets) = self
            .disks
            .get(&c.layer)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such layer disk"))?;
        let at = *offsets
            .get(&c.index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file in the disk"))?;
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(at))?;
        Ok(Box::new(io::BufReader::new(f).take(c.size)))
    }
}

#[cfg(test)]
mod tests;
