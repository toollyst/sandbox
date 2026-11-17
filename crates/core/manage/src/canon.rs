//! 층 항목을 정해진 모양의 tar로 쓴다. 빌드가 만든 층의 diff_id와 내보내기(`save`, `push`, `build --output`)가 같은
//! 바이트를 내는 곳은 여기 하나다.
//!
//! 빌드가 만든 층은 층 디스크(EROFS)로만 둔다(고른 대상의 형태 하나만). 그 층의 diff_id는 **층 디스크를 다시 읽은
//! 항목**(`sbx_rootfs::erofs::read::read_layer`)을 이 모양으로 쓴 tar의 sha256이다. 그래서 언제 내보내도 디스크에서
//! 같은 tar가 나오고, 그 sha256이 이미지 config의 diff_id와 같다(내보낼 때 대조한다).
//!
//! **모양** (같은 항목을 같은 차례로 주면 같은 바이트다)
//!
//! - 항목은 받은 차례로 쓴다(디스크에서 읽은 항목은 디렉터리가 그 안보다 먼저, 이름 바이트 순서다).
//! - GNU 머리. 사용자·그룹 이름은 비운다. 디렉터리 이름은 `/`로 끝난다. 100바이트를 넘는 이름과 링크 대상은 GNU
//!   긴 이름 항목(`././@LongLink`)으로 쓴다.
//! - xattr이 있으면 바로 앞에 PAX 확장 머리(`SCHILY.xattr.<이름>`, 이름 순서)를 둔다.
//! - 권한은 아래 12비트, 시각은 초(음수는 0). whiteout과 opaque 표시는 크기 0인 일반 파일이다.
//! - 끝에 0으로 찬 512바이트 블록 두 개. 항목이 없는 층은 이 1024바이트뿐이다([`sbx_store::EMPTY_LAYER`]).

use std::io::{self, Read, Write};

use sbx_rootfs::{ContentSource, EntryKind, LayerEntry};

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
        let want = buf
            .len()
            .min(usize::try_from(self.left).unwrap_or(usize::MAX));
        let n = self.inner.read(&mut buf[..want])?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("{} is shorter than its recorded size", self.path),
            ));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

/// `entries`를 tar로 `out`에 쓴다. 파일 내용은 `contents`로 연다.
pub fn write(
    entries: &[LayerEntry],
    contents: &dyn ContentSource,
    out: impl Write,
) -> io::Result<()> {
    let mut b = tar::Builder::new(out);
    for e in entries {
        if !e.meta.xattrs.is_empty() {
            let pax: Vec<(String, &[u8])> = e
                .meta
                .xattrs
                .iter()
                .map(|(k, v)| (format!("SCHILY.xattr.{k}"), v.as_slice()))
                .collect();
            b.append_pax_extensions(pax.iter().map(|(k, v)| (k.as_str(), *v)))?;
        }
        let mut h = tar::Header::new_gnu();
        h.set_mode(e.meta.mode & 0o7777);
        h.set_uid(u64::from(e.meta.uid));
        h.set_gid(u64::from(e.meta.gid));
        h.set_mtime(e.meta.mtime.max(0) as u64);
        h.set_size(0);
        let path = e.path.trim_start_matches('/');
        match &e.kind {
            EntryKind::Dir => {
                h.set_entry_type(tar::EntryType::Directory);
                let p = format!("{}/", path.trim_end_matches('/'));
                b.append_data(&mut h, p, io::empty())?;
            }
            EntryKind::File(c) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_size(c.size);
                let r = Exact {
                    inner: contents.open(c)?,
                    left: c.size,
                    path: format!("/{path}"),
                };
                b.append_data(&mut h, path, r)?;
            }
            EntryKind::Symlink(t) => {
                h.set_entry_type(tar::EntryType::Symlink);
                b.append_link(&mut h, path, t)?;
            }
            EntryKind::Hardlink(t) => {
                h.set_entry_type(tar::EntryType::Link);
                b.append_link(&mut h, path, t.trim_start_matches('/'))?;
            }
            EntryKind::CharDevice { major, minor } | EntryKind::BlockDevice { major, minor } => {
                h.set_entry_type(if matches!(e.kind, EntryKind::CharDevice { .. }) {
                    tar::EntryType::Char
                } else {
                    tar::EntryType::Block
                });
                h.set_device_major(*major)?;
                h.set_device_minor(*minor)?;
                b.append_data(&mut h, path, io::empty())?;
            }
            EntryKind::Fifo => {
                h.set_entry_type(tar::EntryType::Fifo);
                b.append_data(&mut h, path, io::empty())?;
            }
        }
    }
    b.into_inner()?.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sbx_rootfs::{ContentRef, Meta};

    struct Mem(Vec<Vec<u8>>);

    impl ContentSource for Mem {
        fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
            Ok(Box::new(self.0.get(c.index).map_or(&[][..], |v| &v[..])))
        }
    }

    fn meta() -> Meta {
        Meta {
            mode: 0o755,
            uid: 0,
            gid: 0,
            mtime: 1,
            xattrs: Default::default(),
        }
    }

    #[test]
    fn an_empty_layer_is_the_two_end_blocks() {
        let mut out = Vec::new();
        write(&[], &Mem(vec![]), &mut out).unwrap();
        assert_eq!(out, vec![0u8; 1024]);
        assert_eq!(
            sbx_store::digest::sha256(&out).to_string(),
            sbx_store::EMPTY_LAYER
        );
    }

    #[test]
    fn the_same_entries_give_the_same_bytes_and_read_back() {
        let long = format!("d/{}", "n".repeat(150));
        let mut x = meta();
        x.xattrs.insert("user.k".into(), b"v".to_vec());
        let entries = vec![
            LayerEntry {
                path: "d".into(),
                kind: EntryKind::Dir,
                meta: meta(),
            },
            LayerEntry {
                path: long.clone(),
                kind: EntryKind::File(ContentRef {
                    layer: 0,
                    index: 0,
                    size: 3,
                }),
                meta: x,
            },
            LayerEntry {
                path: "d/l".into(),
                kind: EntryKind::Hardlink(long.clone()),
                meta: meta(),
            },
        ];
        let mem = Mem(vec![b"abc".to_vec()]);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        write(&entries, &mem, &mut a).unwrap();
        write(&entries, &mem, &mut b).unwrap();
        assert_eq!(a, b);
        let dir = std::env::temp_dir().join(format!("sbx-canon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let back = crate::layer::read(0, &a[..], &dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(back.len(), 3);
        assert_eq!(back[1].path, long);
        assert_eq!(back[1].meta.xattrs["user.k"], b"v");
        assert_eq!(back[2].kind, EntryKind::Hardlink(long));
        // 내용이 줄면 오류다(조용히 짧은 tar를 쓰지 않는다).
        let short = Mem(vec![b"ab".to_vec()]);
        assert!(write(&entries, &short, &mut Vec::new()).is_err());
    }
}
