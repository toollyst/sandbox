//! scratch 디스크와 볼륨 디스크 포맷. 빈 ext2(scratch)나 저널이 있는 ext4(볼륨) 파일 시스템을 만든다.
//!
//! 게스트가 첫 부팅 때 scratch 디스크에, 처음 붙인 볼륨 디스크에 superblock이 없으면 부른다. 호스트는 빈 파일만
//! 만들고 게스트가 쓴 디스크를 해석하지 않는다. 그래서 이 형식은 게스트만 안다.
//!
//! 형식 (커널의 ext4 드라이버가 마운트한다)
//! - 블록 4096바이트, 그룹당 32768블록, inode 256바이트, 16KiB당 inode 하나.
//! - 기능: `filetype`(overlay가 요구하는 d_type), `ext_attr`(overlay가 쓰는 xattr), `sparse_super`, `large_file`.
//! - scratch([`Journal::None`])는 저널이 없다. VM이 강제로 꺼지면 다음 마운트 때 정리되지 않은 상태일 수 있다.
//!   정상 종료는 sync 뒤에 끈다. scratch는 샌드박스와 함께 버리는 쓰기 층이라 그것으로 된다.
//! - 볼륨([`Journal::Internal`])은 저널(jbd2, inode 8, 그룹 0의 데이터 맨 앞에 이어진 블록들)과 `extents`를 더한다.
//!   볼륨은 VM이 강제로 꺼져도 남아야 하는 데이터(DB)를 담는다. 저널이 없으면 `fsync`가 파일 내용과 inode만 쓰고
//!   블록 비트맵은 쓰지 않아서, 강제로 꺼진 뒤 같은 블록을 두 파일이 쓰게 될 수 있다(fsck 없이는 고칠 수 없다).
//!   저널이 있으면 커널이 마운트할 때 저널을 다시 적용해 일관된 상태로 돌아온다. 저널 크기는 mke2fs의 기본표를
//!   따르되 64MiB(16384블록)까지다(한 extent, 그룹 0 안).
//! - 메타데이터를 직접 쓴다. inode 테이블은 [`Disk::Zeroed`]면 쓰지 않는다(이미 0이다). 1GiB면 16MiB를
//!   쓰지 않으므로 첫 부팅이 빨라지고, 호스트의 sparse 파일도 그만큼 작게 남는다. [`Disk::Unknown`]이면 모두 쓴다.
//!   어느 쪽이든 결과 바이트는 같다.
//! - 같은 크기와 `uuid`면 같은 바이트가 나온다. 시각은 모두 0이다.
//! - 루트(inode 2, 0755)와 `lost+found`(inode 11, 0700)만 있다. 볼륨에는 저널 inode(8)도 있다.

use std::io::{self, Seek, SeekFrom, Write};

const BLOCK: u64 = 4096;
const BLOCKS_PER_GROUP: u64 = 8 * BLOCK;
const INODE_SIZE: u64 = 256;
const INODES_PER_BLOCK: u64 = BLOCK / INODE_SIZE;
const BYTES_PER_INODE: u64 = 16 * 1024;
const GROUP_DESC: u64 = 32;
const ROOT_INO: u32 = 2;
const LOST_FOUND_INO: u32 = 11;
const FIRST_INO: u32 = 11;
const EXTRA_ISIZE: u16 = 32;

/// superblock 매직. 이미 포맷되었는지 볼 때 쓴다(오프셋 1024 + 56).
pub const MAGIC: u16 = 0xEF53;
/// 매직의 디스크 위치.
pub const MAGIC_OFFSET: u64 = 1024 + 56;

/// 포맷할 수 있는 가장 작은 크기.
pub const MIN_BYTES: u64 = 1024 * 1024;

const COMPAT_HAS_JOURNAL: u32 = 0x0004;
const COMPAT_EXT_ATTR: u32 = 0x0008;
const INCOMPAT_FILETYPE: u32 = 0x0002;
const INCOMPAT_EXTENTS: u32 = 0x0040;
const JOURNAL_INO: u32 = 8;
const EXTENTS_FL: u32 = 0x0008_0000;
const EXTENT_MAGIC: u16 = 0xF30A;
const JBD2_MAGIC: u32 = 0xC03B_3998;
const JBD2_SUPERBLOCK_V2: u32 = 4;
/// 저널의 가장 큰 블록 수(한 extent에 들고 그룹 0에 들어간다).
const MAX_JOURNAL_BLOCKS: u64 = 16384;

/// 저널을 둘지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Journal {
    /// 저널 없음(ext2, scratch).
    None,
    /// 같은 장치 안의 저널(ext4, 볼륨). 모듈 문서의 "형식".
    Internal,
}

/// mke2fs의 기본 저널 크기(블록)를 [`MAX_JOURNAL_BLOCKS`]로 자른 것. 너무 작은 파일 시스템이면 `None`.
fn journal_blocks(blocks: u64) -> Option<u64> {
    let j = match blocks {
        0..2048 => return None,
        2048..32768 => 1024,
        32768..262_144 => 4096,
        262_144..524_288 => 8192,
        _ => 16384,
    };
    Some(j.min(MAX_JOURNAL_BLOCKS))
}
const RO_COMPAT_SPARSE_SUPER: u32 = 0x0001;
const RO_COMPAT_LARGE_FILE: u32 = 0x0002;

#[derive(Debug, Clone, Copy)]
struct Group {
    start: u64,
    blocks: u64,
    has_super: bool,
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
    /// 이 그룹에서 메타데이터 다음 첫 데이터 블록.
    data: u64,
}

#[derive(Debug)]
struct Layout {
    blocks: u64,
    groups: Vec<Group>,
    inodes_per_group: u64,
    gdt_blocks: u64,
    itable_blocks: u64,
    root_block: u64,
    lost_found_block: u64,
    /// 저널의 첫 블록과 블록 수(없으면 0). 그룹 0에서 lost+found 다음에 이어진다.
    journal_block: u64,
    journal_blocks: u64,
}

fn is_power_of(mut n: u64, base: u64) -> bool {
    while n > 1 && n.is_multiple_of(base) {
        n /= base;
    }
    n == 1
}

/// `sparse_super`: 그룹 0, 1과 3, 5, 7의 거듭제곱 그룹에만 superblock 사본이 있다.
fn has_super(g: u64) -> bool {
    g == 0 || is_power_of(g, 3) || is_power_of(g, 5) || is_power_of(g, 7)
}

impl Layout {
    fn plan(bytes: u64, journal: Journal) -> io::Result<Self> {
        if bytes < MIN_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("scratch disk too small: {bytes} bytes (minimum {MIN_BYTES})"),
            ));
        }
        let mut blocks = bytes / BLOCK;
        loop {
            let count = blocks.div_ceil(BLOCKS_PER_GROUP);
            let total_inodes = (blocks * BLOCK / BYTES_PER_INODE).max(16);
            let ipg = total_inodes
                .div_ceil(count)
                .next_multiple_of(INODES_PER_BLOCK)
                .min(BLOCKS_PER_GROUP);
            let itable_blocks = ipg / INODES_PER_BLOCK;
            let gdt_blocks = (count * GROUP_DESC).div_ceil(BLOCK);
            let mut groups = Vec::new();
            for g in 0..count {
                let start = g * BLOCKS_PER_GROUP;
                let size = (blocks - start).min(BLOCKS_PER_GROUP);
                let sup = has_super(g);
                let meta_start = start + if sup { 1 + gdt_blocks } else { 0 };
                groups.push(Group {
                    start,
                    blocks: size,
                    has_super: sup,
                    block_bitmap: meta_start,
                    inode_bitmap: meta_start + 1,
                    inode_table: meta_start + 2,
                    data: meta_start + 2 + itable_blocks,
                });
            }
            // 마지막 그룹이 메타데이터와 데이터 몇 블록도 담지 못하면 그 그룹을 버리고 다시 계산한다(mke2fs와 같다).
            let last = groups[groups.len() - 1];
            let room = (last.start + last.blocks).saturating_sub(last.data);
            if room < 16 && count > 1 {
                blocks = last.start;
                continue;
            }
            if room < 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "scratch disk too small for metadata",
                ));
            }
            let first = groups[0].data;
            let journal_blocks = match journal {
                Journal::None => 0,
                Journal::Internal => {
                    let j = journal_blocks(blocks);
                    let room = (groups[0].start + groups[0].blocks).saturating_sub(first + 2);
                    match j {
                        Some(j) if j + 16 <= room => j,
                        _ => {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "disk too small for a journal",
                            ));
                        }
                    }
                }
            };
            return Ok(Self {
                blocks,
                groups,
                inodes_per_group: ipg,
                gdt_blocks,
                itable_blocks,
                root_block: first,
                lost_found_block: first + 1,
                journal_block: first + 2,
                journal_blocks,
            });
        }
    }

    fn inodes(&self) -> u64 {
        self.inodes_per_group * self.groups.len() as u64
    }

    /// 그룹의 사용 중인 블록 수.
    fn used_blocks(&self, g: usize) -> u64 {
        let gr = &self.groups[g];
        let mut used = gr.data - gr.start;
        if g == 0 {
            used += 2 + self.journal_blocks; // 루트와 lost+found의 디렉터리 블록, 저널
        }
        used
    }

    fn free_blocks(&self) -> u64 {
        (0..self.groups.len())
            .map(|g| self.groups[g].blocks - self.used_blocks(g))
            .sum()
    }
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn u32_of(v: u64) -> u32 {
    u32::try_from(v).expect("ext2 field exceeds u32")
}

fn superblock(l: &Layout, uuid: [u8; 16], group: u64) -> [u8; 1024] {
    let journal = l.journal_blocks > 0;
    let mut s = [0u8; 1024];
    let used_inodes = u64::from(FIRST_INO); // 1..=11
    put32(&mut s, 0, u32_of(l.inodes()));
    put32(&mut s, 4, u32_of(l.blocks));
    put32(&mut s, 8, 0); // 예약 블록 없음
    put32(&mut s, 12, u32_of(l.free_blocks()));
    put32(&mut s, 16, u32_of(l.inodes() - used_inodes));
    put32(&mut s, 20, 0); // first_data_block (4K 블록에서는 0)
    put32(&mut s, 24, 2); // log_block_size: 1024 << 2
    put32(&mut s, 28, 2); // log_frag_size
    put32(&mut s, 32, u32_of(BLOCKS_PER_GROUP));
    put32(&mut s, 36, u32_of(BLOCKS_PER_GROUP));
    put32(&mut s, 40, u32_of(l.inodes_per_group));
    put16(&mut s, 52, 0); // mnt_count
    put16(&mut s, 54, 0xFFFF); // max_mnt_count: 검사 강제 없음
    put16(&mut s, 56, MAGIC);
    put16(&mut s, 58, 1); // state: clean
    put16(&mut s, 60, 1); // errors: continue
    put32(&mut s, 72, 0); // creator_os: Linux
    put32(&mut s, 76, 1); // rev_level: dynamic
    put32(&mut s, 84, FIRST_INO);
    put16(&mut s, 88, INODE_SIZE as u16);
    put16(&mut s, 90, u16::try_from(group).unwrap_or(u16::MAX));
    let (compat, incompat) = if journal {
        (
            COMPAT_EXT_ATTR | COMPAT_HAS_JOURNAL,
            INCOMPAT_FILETYPE | INCOMPAT_EXTENTS,
        )
    } else {
        (COMPAT_EXT_ATTR, INCOMPAT_FILETYPE)
    };
    put32(&mut s, 92, compat);
    put32(&mut s, 96, incompat);
    put32(&mut s, 100, RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE);
    s[104..120].copy_from_slice(&uuid);
    let label: &[u8] = if journal {
        b"sbx-volume"
    } else {
        b"sbx-scratch"
    };
    s[120..120 + label.len()].copy_from_slice(label);
    if journal {
        put32(&mut s, 224, JOURNAL_INO); // s_journal_inum
        // s_jnl_blocks: 저널 inode의 i_block과 크기 사본. e2fsck가 저널 inode를 견준다.
        let inode = journal_inode(l);
        s[268..268 + 60].copy_from_slice(&inode[40..100]);
        put32(&mut s, 268 + 15 * 4, 0); // i_size_high
        put32(&mut s, 268 + 16 * 4, u32_of(l.journal_blocks * BLOCK));
        s[253] = 1; // s_jnl_backup_type: EXT3_JNL_BACKUP_BLOCKS
    }
    put16(&mut s, 348, EXTRA_ISIZE); // min_extra_isize
    put16(&mut s, 350, EXTRA_ISIZE); // want_extra_isize
    s
}

/// 저널 inode: 일반 파일 0600, extent 하나가 저널 블록 전체를 가리킨다.
fn journal_inode(l: &Layout) -> [u8; INODE_SIZE as usize] {
    let mut i = [0u8; INODE_SIZE as usize];
    put16(&mut i, 0, 0o100600);
    put32(&mut i, 4, u32_of(l.journal_blocks * BLOCK));
    put16(&mut i, 26, 1);
    put32(&mut i, 28, u32_of(l.journal_blocks * (BLOCK / 512)));
    put32(&mut i, 32, EXTENTS_FL);
    // extent 머리: 매직, 항목 1, 최대 4, 깊이 0.
    put16(&mut i, 40, EXTENT_MAGIC);
    put16(&mut i, 42, 1);
    put16(&mut i, 44, 4);
    put16(&mut i, 46, 0);
    // extent 하나: 논리 블록 0부터 journal_blocks개, 물리 블록 journal_block부터.
    put32(&mut i, 52, 0);
    put16(
        &mut i,
        56,
        u16::try_from(l.journal_blocks).expect("journal fits one extent"),
    );
    put16(&mut i, 58, 0);
    put32(&mut i, 60, u32_of(l.journal_block));
    put16(&mut i, 128, EXTRA_ISIZE);
    i
}

/// jbd2 superblock(v2, big-endian). `s_start`가 0이라 비어 있는(다시 적용할 것이 없는) 저널이다.
fn journal_superblock(l: &Layout, uuid: [u8; 16]) -> Vec<u8> {
    let mut b = vec![0u8; BLOCK as usize];
    let be = |b: &mut [u8], at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_be_bytes());
    be(&mut b, 0, JBD2_MAGIC);
    be(&mut b, 4, JBD2_SUPERBLOCK_V2);
    be(&mut b, 8, 0); // h_sequence
    be(&mut b, 12, BLOCK as u32); // s_blocksize
    be(&mut b, 16, u32_of(l.journal_blocks)); // s_maxlen
    be(&mut b, 20, 1); // s_first
    be(&mut b, 24, 1); // s_sequence
    be(&mut b, 28, 0); // s_start: 비었다
    b[48..64].copy_from_slice(&uuid);
    be(&mut b, 64, 1); // s_nr_users
    b
}

fn group_descriptors(l: &Layout) -> Vec<u8> {
    let mut t = vec![0u8; (l.gdt_blocks * BLOCK) as usize];
    for (g, gr) in l.groups.iter().enumerate() {
        let d = &mut t[g * GROUP_DESC as usize..(g + 1) * GROUP_DESC as usize];
        put32(d, 0, u32_of(gr.block_bitmap));
        put32(d, 4, u32_of(gr.inode_bitmap));
        put32(d, 8, u32_of(gr.inode_table));
        put16(d, 12, (gr.blocks - l.used_blocks(g)) as u16);
        let used_inodes = if g == 0 { u64::from(FIRST_INO) } else { 0 };
        put16(d, 14, (l.inodes_per_group - used_inodes) as u16);
        put16(d, 16, if g == 0 { 2 } else { 0 });
    }
    t
}

/// 앞의 `used`비트와, `valid`비트 뒤의 패딩을 1로 둔 비트맵 블록.
fn bitmap(used: u64, valid: u64) -> Vec<u8> {
    let mut b = vec![0u8; BLOCK as usize];
    for i in (0..used).chain(valid..BLOCK * 8) {
        b[(i / 8) as usize] |= 1 << (i % 8);
    }
    b
}

fn dir_inode(mode: u16, links: u16, block: u64) -> [u8; INODE_SIZE as usize] {
    let mut i = [0u8; INODE_SIZE as usize];
    put16(&mut i, 0, mode);
    put32(&mut i, 4, BLOCK as u32); // size
    put16(&mut i, 26, links);
    put32(&mut i, 28, (BLOCK / 512) as u32); // i_blocks (512바이트 단위)
    put32(&mut i, 40, u32_of(block)); // i_block[0]
    put16(&mut i, 128, EXTRA_ISIZE);
    i
}

fn dir_block(entries: &[(u32, &str)]) -> Vec<u8> {
    let mut b = vec![0u8; BLOCK as usize];
    let mut at = 0usize;
    for (n, (ino, name)) in entries.iter().enumerate() {
        let len = (8 + name.len()).next_multiple_of(4);
        let rec = if n + 1 == entries.len() {
            BLOCK as usize - at
        } else {
            len
        };
        put32(&mut b, at, *ino);
        put16(&mut b, at + 4, rec as u16);
        b[at + 6] = name.len() as u8;
        b[at + 7] = 2; // file_type: 디렉터리
        b[at + 8..at + 8 + name.len()].copy_from_slice(name.as_bytes());
        at += rec;
    }
    b
}

fn write_at(w: &mut (impl Write + Seek), block: u64, offset: u64, data: &[u8]) -> io::Result<()> {
    w.seek(SeekFrom::Start(block * BLOCK + offset))?;
    w.write_all(data)
}

/// 포맷할 장치의 지금 내용.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disk {
    /// 모두 0이다 (sbx가 새로 만든 scratch: 실행기 규약의 약속).
    Zeroed,
    /// 모른다. 0이어야 할 곳도 모두 쓴다.
    Unknown,
}

/// `bytes` 크기의 장치에 빈 ext2(저널 없음, scratch)를 만든다. 쓴 파일 시스템 크기(바이트)를 돌려준다.
pub fn format(
    w: &mut (impl Write + Seek),
    bytes: u64,
    uuid: [u8; 16],
    disk: Disk,
) -> io::Result<u64> {
    format_with(w, bytes, uuid, disk, Journal::None)
}

/// [`format`]과 같고, `journal`이면 저널이 있는 ext4(볼륨)를 만든다(모듈 문서의 "형식").
pub fn format_with(
    w: &mut (impl Write + Seek),
    bytes: u64,
    uuid: [u8; 16],
    disk: Disk,
    journal: Journal,
) -> io::Result<u64> {
    let l = Layout::plan(bytes, journal)?;
    let gdt = group_descriptors(&l);
    let zero = vec![0u8; BLOCK as usize];
    for (g, gr) in l.groups.iter().enumerate() {
        if gr.has_super {
            // 원본은 디스크의 바이트 1024(블록 0 안, 앞 1024바이트는 부트 영역)에 있다.
            // 사본은 그 그룹 첫 블록의 시작(오프셋 0)에 있다.
            let mut first = vec![0u8; BLOCK as usize];
            let at = if g == 0 { 1024 } else { 0 };
            first[at..at + 1024].copy_from_slice(&superblock(&l, uuid, g as u64));
            write_at(w, gr.start, 0, &first)?;
            write_at(w, gr.start + 1, 0, &gdt)?;
        }
        let used = l.used_blocks(g);
        write_at(w, gr.block_bitmap, 0, &bitmap(used, gr.blocks))?;
        let used_inodes = if g == 0 { u64::from(FIRST_INO) } else { 0 };
        write_at(
            w,
            gr.inode_bitmap,
            0,
            &bitmap(used_inodes, l.inodes_per_group),
        )?;
        if disk == Disk::Unknown {
            for b in 0..l.itable_blocks {
                write_at(w, gr.inode_table + b, 0, &zero)?;
            }
        }
    }
    let itable = l.groups[0].inode_table;
    let slot = |ino: u32| u64::from(ino - 1) * INODE_SIZE;
    // 루트: ., .., lost+found 의 .. → 링크 3.
    write_at(
        w,
        itable,
        slot(ROOT_INO),
        &dir_inode(0o40755, 3, l.root_block),
    )?;
    write_at(
        w,
        itable,
        slot(LOST_FOUND_INO),
        &dir_inode(0o40700, 2, l.lost_found_block),
    )?;
    write_at(
        w,
        l.root_block,
        0,
        &dir_block(&[
            (ROOT_INO, "."),
            (ROOT_INO, ".."),
            (LOST_FOUND_INO, "lost+found"),
        ]),
    )?;
    write_at(
        w,
        l.lost_found_block,
        0,
        &dir_block(&[(LOST_FOUND_INO, "."), (ROOT_INO, "..")]),
    )?;
    if l.journal_blocks > 0 {
        write_at(w, itable, slot(JOURNAL_INO), &journal_inode(&l))?;
        write_at(w, l.journal_block, 0, &journal_superblock(&l, uuid))?;
    }
    w.flush()?;
    Ok(l.blocks * BLOCK)
}

/// 이미 ext 파일 시스템이 있는지(매직으로만 본다).
pub fn is_formatted(r: &mut (impl io::Read + Seek)) -> io::Result<bool> {
    let mut m = [0u8; 2];
    r.seek(SeekFrom::Start(MAGIC_OFFSET))?;
    match r.read_exact(&mut m) {
        Ok(()) => Ok(u16::from_le_bytes(m) == MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests;
