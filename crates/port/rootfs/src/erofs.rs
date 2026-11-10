//! erofs writer. 첫 번째 rootfs 형식이다.
//!
//! erofs를 먼저 고른 이유:
//!
//! - 읽기 전용 이미지를 위해 만든 형식이고, 이 구조에서 rootfs는 항상 읽기 전용이다.
//!   쓰기는 샌드박스별 scratch 디스크가 맡는다.
//! - 저널, 블록 그룹, 할당 비트맵이 없어서 ext4보다 직접 구현할 양이 적다.
//!
//! 게스트 커널에 erofs가 켜져 있어야 한다.
//!
//! # 만드는 이미지의 모양
//!
//! 기능을 최소로 써서 오래된 커널에서도 읽히게 한다. superblock의 feature 비트는 모두 0이다.
//!
//! ```text
//! 블록 0      [0..1024) 비움, [1024..1152) superblock, [1152..) inode 영역 시작
//! inode 영역  모든 inode를 트리 경로 순서로. 루트가 첫 번째(nid 36)
//! 데이터 영역 inode 순서대로 디렉터리, 파일, 심볼릭 링크 데이터. 각각 블록 경계에서 시작
//! ```
//!
//! - 블록 크기 4096.
//! - inode는 모두 extended(64바이트). uid/gid 32비트, mtime, 64비트 크기를 담기 위해서다.
//! - 데이터 배치는 모두 `FLAT_PLAIN`(연속 블록). 압축, tail packing, chunk는 쓰지 않는다.
//! - xattr은 inode 뒤에 inline으로만 둔다(공유 xattr 없음). 이름 접두어는 `user.`, `trusted.`,
//!   `security.`, `system.posix_acl_access`, `system.posix_acl_default`만 표현할 수 있고, 나머지는 오류다.
//! - 하드 링크는 같은 inode(nid)를 가리키는 디렉터리 항목 여러 개다.
//! - 출력은 결정적이다. 같은 트리와 같은 내용이면 같은 바이트가 나온다.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};

use sbx_model::RootfsFormat;

use crate::{ContentSource, MergedTree, Node, NodeKind, RootfsBuilder, RootfsError, WriteSeek};

const BLOCK_BITS: u8 = 12;
const BLOCK: u64 = 1 << BLOCK_BITS;
const SUPER_OFFSET: usize = 1024;
const SUPER_SIZE: usize = 128;
const MAGIC: u32 = 0xE0F5_E1E2;
const SLOT: u64 = 32;
const INODE_SIZE: u64 = 64;
const NULL_ADDR: u32 = u32::MAX;
const DIRENT_SIZE: usize = 12;
const XATTR_HEADER_SIZE: usize = 12;
const NAME_MAX: usize = 255;

/// i_format: extended inode, FLAT_PLAIN.
const FORMAT_EXTENDED_PLAIN: u16 = 1;

const FT_REG: u8 = 1;
const FT_DIR: u8 = 2;
const FT_CHRDEV: u8 = 3;
const FT_BLKDEV: u8 = 4;
const FT_FIFO: u8 = 5;
const FT_SYMLINK: u8 = 7;

const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const S_IFLNK: u32 = 0o120000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;
const S_IFIFO: u32 = 0o010000;

/// xattr 이름 접두어 → erofs name index. 정확히 일치해야 하는 이름은 접미어가 빈 문자열이다.
const XATTR_EXACT: &[(&str, u8)] = &[
    ("system.posix_acl_access", 2),
    ("system.posix_acl_default", 3),
];
const XATTR_PREFIXES: &[(&str, u8)] = &[("user.", 1), ("trusted.", 4), ("security.", 6)];

/// erofs writer.
#[derive(Debug, Default, Clone, Copy)]
pub struct ErofsBuilder;

impl RootfsBuilder for ErofsBuilder {
    fn format(&self) -> RootfsFormat {
        RootfsFormat::Erofs
    }

    fn build(
        &self,
        tree: &MergedTree,
        content: &dyn ContentSource,
        out: &mut dyn WriteSeek,
    ) -> Result<(), RootfsError> {
        let image = Layout::plan(tree)?;
        image.write(content, out)
    }
}

struct Inode<'a> {
    node: &'a Node,
    nlink: u32,
    size: u64,
    xattrs: Vec<u8>,
    nid: u64,
    blkaddr: u32,
    /// 디렉터리면 (이름, 자식 inode 번호), 이름 순서. `.`과 `..` 포함.
    entries: Vec<(Vec<u8>, usize)>,
}

impl Inode<'_> {
    fn file_type(&self) -> u8 {
        match self.node.kind {
            NodeKind::Dir => FT_DIR,
            NodeKind::File(_) => FT_REG,
            NodeKind::Symlink(_) => FT_SYMLINK,
            NodeKind::CharDevice { .. } => FT_CHRDEV,
            NodeKind::BlockDevice { .. } => FT_BLKDEV,
            NodeKind::Fifo => FT_FIFO,
        }
    }

    fn has_data(&self) -> bool {
        matches!(
            self.node.kind,
            NodeKind::Dir | NodeKind::File(_) | NodeKind::Symlink(_)
        ) && self.size > 0
    }

    fn blocks(&self) -> u64 {
        if self.has_data() {
            self.size.div_ceil(BLOCK)
        } else {
            0
        }
    }
}

struct Layout<'a> {
    inodes: Vec<Inode<'a>>,
    meta_bytes: u64,
    total_blocks: u64,
}

fn unsupported(m: String) -> RootfsError {
    RootfsError::Unsupported(m)
}

fn xattr_ibody(path: &str, xattrs: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>, RootfsError> {
    if xattrs.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = vec![0u8; XATTR_HEADER_SIZE];
    for (name, value) in xattrs {
        let (index, suffix) = XATTR_EXACT
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, i)| (*i, ""))
            .or_else(|| {
                XATTR_PREFIXES
                    .iter()
                    .find_map(|(p, i)| name.strip_prefix(p).map(|s| (*i, s)))
            })
            .ok_or_else(|| unsupported(format!("xattr {name:?} at {path:?}")))?;
        if suffix.len() > usize::from(u8::MAX) || value.len() > usize::from(u16::MAX) {
            return Err(unsupported(format!("xattr {name:?} too large at {path:?}")));
        }
        out.push(suffix.len() as u8);
        out.push(index);
        out.extend_from_slice(&(value.len() as u16).to_le_bytes());
        out.extend_from_slice(suffix.as_bytes());
        out.extend_from_slice(value);
        out.resize(out.len().next_multiple_of(4), 0);
    }
    Ok(out)
}

/// 디렉터리 항목을 블록에 나눠 담는다. 각 블록이 담는 항목 범위와 디렉터리 크기를 돌려준다.
fn pack_dir(names: &[&[u8]]) -> (Vec<std::ops::Range<usize>>, u64) {
    let mut blocks = Vec::new();
    let mut start = 0;
    let mut used = 0usize;
    for (i, name) in names.iter().enumerate() {
        let need = DIRENT_SIZE + name.len();
        if used + need > BLOCK as usize {
            blocks.push(start..i);
            start = i;
            used = 0;
        }
        used += need;
    }
    blocks.push(start..names.len());
    let size = (blocks.len() as u64 - 1) * BLOCK + used as u64;
    (blocks, size)
}

impl<'a> Layout<'a> {
    fn plan(tree: &'a MergedTree) -> Result<Self, RootfsError> {
        let mut inodes: Vec<Inode<'a>> = Vec::new();
        let mut index_of: HashMap<&'a str, usize> = HashMap::new();
        let mut group_of: HashMap<u64, usize> = HashMap::new();

        for (path, node) in tree.iter() {
            if let Some(&i) = node.link_group.and_then(|g| group_of.get(&g)) {
                inodes[i].nlink += 1;
                index_of.insert(path, i);
                continue;
            }
            let size = match &node.kind {
                NodeKind::File(c) => c.size,
                NodeKind::Symlink(t) => t.len() as u64,
                _ => 0,
            };
            let i = inodes.len();
            inodes.push(Inode {
                node,
                nlink: 1,
                size,
                xattrs: xattr_ibody(path, &node.meta.xattrs)?,
                nid: 0,
                blkaddr: NULL_ADDR,
                entries: Vec::new(),
            });
            index_of.insert(path, i);
            if let Some(g) = node.link_group {
                group_of.insert(g, i);
            }
        }
        if !matches!(inodes.first().map(|i| &i.node.kind), Some(NodeKind::Dir)) {
            return Err(RootfsError::InvalidPath("root is not a directory".into()));
        }

        // 디렉터리 항목.
        let mut children: Vec<Vec<(Vec<u8>, usize)>> = vec![Vec::new(); inodes.len()];
        for (path, _) in tree.iter() {
            if path.is_empty() {
                continue;
            }
            let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
            if name.len() > NAME_MAX {
                return Err(unsupported(format!(
                    "name longer than {NAME_MAX} bytes: {path:?}"
                )));
            }
            children[index_of[parent]].push((name.as_bytes().to_vec(), index_of[path]));
        }
        for (path, node) in tree.iter() {
            if node.kind != NodeKind::Dir {
                continue;
            }
            let i = index_of[path];
            let mut entries = std::mem::take(&mut children[i]);
            let subdirs = entries
                .iter()
                .filter(|(_, c)| inodes[*c].node.kind == NodeKind::Dir)
                .count();
            entries.push((b".".to_vec(), i));
            entries.push((b"..".to_vec(), index_of[parent_of(path)]));
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let names: Vec<&[u8]> = entries.iter().map(|(n, _)| n.as_slice()).collect();
            let (_, size) = pack_dir(&names);
            let inode = &mut inodes[i];
            inode.nlink = 2 + subdirs as u32;
            inode.size = size;
            inode.entries = entries;
        }

        // nid: superblock 바로 뒤부터. 64바이트 inode 헤더가 블록 경계를 넘지 않게 한다.
        let mut offset = (SUPER_OFFSET + SUPER_SIZE) as u64;
        for inode in &mut inodes {
            if offset % BLOCK + INODE_SIZE > BLOCK {
                offset = offset.next_multiple_of(BLOCK);
            }
            inode.nid = offset / SLOT;
            offset += (INODE_SIZE + inode.xattrs.len() as u64).next_multiple_of(SLOT);
        }
        if inodes[0].nid > u64::from(u16::MAX) {
            return Err(unsupported("root nid does not fit in 16 bits".into()));
        }
        let meta_bytes = offset.next_multiple_of(BLOCK);

        // 데이터 블록.
        let mut block = meta_bytes / BLOCK;
        for inode in &mut inodes {
            if inode.has_data() {
                inode.blkaddr = u32::try_from(block)
                    .map_err(|_| unsupported("image larger than 16 TiB".into()))?;
                block += inode.blocks();
            }
        }
        u32::try_from(block).map_err(|_| unsupported("image larger than 16 TiB".into()))?;

        Ok(Self {
            inodes,
            meta_bytes,
            total_blocks: block,
        })
    }

    fn superblock(&self) -> [u8; SUPER_SIZE] {
        let mut sb = [0u8; SUPER_SIZE];
        sb[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        // checksum(4..8), feature_compat(8..12): 0
        sb[12] = BLOCK_BITS;
        // sb_extslots(13): 0
        sb[14..16].copy_from_slice(&(self.inodes[0].nid as u16).to_le_bytes());
        sb[16..24].copy_from_slice(&(self.inodes.len() as u64).to_le_bytes());
        // build_time(24..32), build_time_nsec(32..36): 0
        sb[36..40].copy_from_slice(&(self.total_blocks as u32).to_le_bytes());
        // meta_blkaddr(40..44), xattr_blkaddr(44..48), uuid, volume_name, feature_incompat: 0
        sb
    }

    fn inode(&self, index: usize) -> Vec<u8> {
        let inode = &self.inodes[index];
        let meta = &inode.node.meta;
        let (type_bits, i_u) = match &inode.node.kind {
            NodeKind::Dir => (S_IFDIR, inode.blkaddr),
            NodeKind::File(_) => (S_IFREG, inode.blkaddr),
            NodeKind::Symlink(_) => (S_IFLNK, inode.blkaddr),
            NodeKind::CharDevice { major, minor } => (S_IFCHR, encode_dev(*major, *minor)),
            NodeKind::BlockDevice { major, minor } => (S_IFBLK, encode_dev(*major, *minor)),
            NodeKind::Fifo => (S_IFIFO, 0),
        };
        let icount: u16 = if inode.xattrs.is_empty() {
            0
        } else {
            ((inode.xattrs.len() - XATTR_HEADER_SIZE) / 4 + 1) as u16
        };
        let mut b = vec![0u8; INODE_SIZE as usize];
        b[0..2].copy_from_slice(&FORMAT_EXTENDED_PLAIN.to_le_bytes());
        b[2..4].copy_from_slice(&icount.to_le_bytes());
        b[4..6].copy_from_slice(&((type_bits | (meta.mode & 0o7777)) as u16).to_le_bytes());
        b[8..16].copy_from_slice(&inode.size.to_le_bytes());
        b[16..20].copy_from_slice(&i_u.to_le_bytes());
        b[20..24].copy_from_slice(&(index as u32 + 1).to_le_bytes());
        b[24..28].copy_from_slice(&meta.uid.to_le_bytes());
        b[28..32].copy_from_slice(&meta.gid.to_le_bytes());
        b[32..40].copy_from_slice(&(meta.mtime.max(0) as u64).to_le_bytes());
        b[44..48].copy_from_slice(&inode.nlink.to_le_bytes());
        b.extend_from_slice(&inode.xattrs);
        b
    }

    fn dir_data(&self, inode: &Inode<'_>) -> Vec<u8> {
        let names: Vec<&[u8]> = inode.entries.iter().map(|(n, _)| n.as_slice()).collect();
        let (blocks, size) = pack_dir(&names);
        let mut out = Vec::with_capacity(size as usize);
        for (bi, range) in blocks.iter().enumerate() {
            let start = out.len();
            let entries = &inode.entries[range.clone()];
            let mut nameoff = DIRENT_SIZE * entries.len();
            for (name, child) in entries {
                let c = &self.inodes[*child];
                out.extend_from_slice(&c.nid.to_le_bytes());
                out.extend_from_slice(&(nameoff as u16).to_le_bytes());
                out.push(c.file_type());
                out.push(0);
                nameoff += name.len();
            }
            for (name, _) in entries {
                out.extend_from_slice(name);
            }
            if bi + 1 < blocks.len() {
                out.resize(start + BLOCK as usize, 0);
            }
        }
        debug_assert_eq!(out.len() as u64, size);
        out
    }

    fn write(
        &self,
        content: &dyn ContentSource,
        out: &mut dyn WriteSeek,
    ) -> Result<(), RootfsError> {
        let mut meta = vec![0u8; self.meta_bytes as usize];
        meta[SUPER_OFFSET..SUPER_OFFSET + SUPER_SIZE].copy_from_slice(&self.superblock());
        for (i, inode) in self.inodes.iter().enumerate() {
            let bytes = self.inode(i);
            let at = (inode.nid * SLOT) as usize;
            meta[at..at + bytes.len()].copy_from_slice(&bytes);
        }
        out.write_all(&meta)?;

        let mut written = self.meta_bytes;
        for inode in &self.inodes {
            if !inode.has_data() {
                continue;
            }
            debug_assert_eq!(written, u64::from(inode.blkaddr) * BLOCK);
            match &inode.node.kind {
                NodeKind::Dir => out.write_all(&self.dir_data(inode))?,
                NodeKind::Symlink(target) => out.write_all(target.as_bytes())?,
                NodeKind::File(c) => {
                    let mut reader = content.open(c)?.take(c.size);
                    let copied = io::copy(&mut reader, out)?;
                    if copied != c.size {
                        return Err(RootfsError::Io(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            format!("file content is {copied} bytes, expected {}", c.size),
                        )));
                    }
                }
                _ => {}
            }
            let pad = inode.blocks() * BLOCK - inode.size;
            out.write_all(&vec![0u8; pad as usize])?;
            written += inode.blocks() * BLOCK;
        }
        out.flush()?;
        Ok(())
    }
}

fn parent_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(p, _)| p)
}

/// 커널 `new_encode_dev`와 같은 장치 번호 인코딩.
fn encode_dev(major: u32, minor: u32) -> u32 {
    (minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)
}

#[cfg(test)]
mod tests;
