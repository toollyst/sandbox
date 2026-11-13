//! 놓을 항목([`Placement`])을 압축하지 않은 층 tar로 쓴다. 호스트에서 만드는 층(`COPY`, `ADD`, `WORKDIR`)이다.
//!
//! 같은 항목이면 같은 바이트다(경로 순서, GNU 헤더, 사용자·그룹 이름은 쓰지 않는다). 빈 층은 끝 표시(0 1024바이트)만
//! 있는 tar다. BuildKit의 빈 층과 같은 diff_id다.

use std::io::{self, Read, Write};

use crate::copy::{ItemKind, Placement};

/// 내용이 정확히 `left` 바이트인지 보며 읽는다.
struct Exact<R> {
    inner: R,
    left: u64,
    path: String,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let want = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::other(format!(
                "{} got shorter while the build read it",
                self.path
            )));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// `items`를 tar로 `out`에 쓴다.
pub fn write(items: &Placement, out: impl Write) -> io::Result<()> {
    let mut b = tar::Builder::new(out);
    b.mode(tar::HeaderMode::Complete);
    for (path, item) in items {
        let mut h = tar::Header::new_gnu();
        h.set_mode(item.mode);
        h.set_uid(u64::from(item.uid));
        h.set_gid(u64::from(item.gid));
        h.set_mtime(item.mtime.max(0) as u64);
        h.set_size(0);
        match &item.kind {
            ItemKind::Dir => {
                h.set_entry_type(tar::EntryType::Directory);
                b.append_data(&mut h, format!("{path}/"), io::empty())?;
            }
            ItemKind::File { size, content } => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_size(*size);
                let r = Exact {
                    inner: content.open()?,
                    left: *size,
                    path: format!("/{path}"),
                };
                b.append_data(&mut h, path, r)?;
            }
            ItemKind::Symlink(target) => {
                h.set_entry_type(tar::EntryType::Symlink);
                b.append_link(&mut h, path, target)?;
            }
            ItemKind::CharDevice(major, minor) | ItemKind::BlockDevice(major, minor) => {
                h.set_entry_type(if matches!(item.kind, ItemKind::CharDevice(..)) {
                    tar::EntryType::Char
                } else {
                    tar::EntryType::Block
                });
                h.set_device_major(*major)?;
                h.set_device_minor(*minor)?;
                b.append_data(&mut h, path, io::empty())?;
            }
            ItemKind::Fifo => {
                h.set_entry_type(tar::EntryType::Fifo);
                b.append_data(&mut h, path, io::empty())?;
            }
        }
    }
    b.into_inner()?.flush()
}
