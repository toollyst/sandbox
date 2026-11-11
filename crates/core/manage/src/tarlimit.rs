//! tar 확장 헤더의 크기 상한.
//!
//! `tar` 크레이트는 GNU 긴 이름·긴 링크(`L`, `K`)와 PAX 헤더(`x`, `g`)의 내용을 선언한 크기만큼 통째로 메모리에
//! 읽는다. 악의적인 레이어는 수 KB로 압축되는 수백 MiB짜리 긴 이름을 낼 수 있다. 그래서 tar 바이트 흐름을 그대로
//! 넘기며 512바이트 헤더를 따라가다가, 확장 헤더가 [`MAX_EXTENSION`]보다 크다고 선언하면 그 내용을 읽기 **전에**
//! 오류로 끝낸다. 크레이트가 받는 바이트는 바꾸지 않는다.
//!
//! 따라가는 규칙(POSIX ustar, GNU, PAX): 헤더 하나(512바이트) 뒤에 크기를 512 단위로 올린 데이터가 온다. 크기는
//! 8진수 글자이거나 첫 바이트의 최상위 비트가 켜진 base-256이다. PAX `x` 헤더의 `size`는 다음 항목의 크기를
//! 바꾼다(그래서 `x`의 내용은 상한 안에서 모아 읽는다). GNU sparse(`S`)는 확장 블록이 따로 있어 따라가지 않고,
//! 그 뒤로는 그대로 넘긴다(레이어 읽기가 그 항목을 거부한다).

use std::io::{self, Read};

/// 확장 헤더 하나의 내용 상한(바이트).
pub const MAX_EXTENSION: u64 = 1 << 20;

const BLOCK: usize = 512;

/// [`Read`]를 감싸 확장 헤더의 크기를 본다.
pub struct Limited<R> {
    inner: R,
    header: [u8; BLOCK],
    filled: usize,
    /// 지금 항목의 남은 데이터(512 단위로 올린 것).
    data_left: u64,
    /// 모으는 `x` 헤더 내용과 그 실제 크기.
    pax: Option<(Vec<u8>, usize)>,
    /// 다음 항목의 크기(`x`의 `size`).
    next_size: Option<u64>,
    /// 더 따라가지 않는다(GNU sparse).
    passthrough: bool,
}

impl<R: Read> Limited<R> {
    /// 감싼다.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            header: [0; BLOCK],
            filled: 0,
            data_left: 0,
            pax: None,
            next_size: None,
            passthrough: false,
        }
    }

    fn observe(&mut self, mut b: &[u8]) -> io::Result<()> {
        while !b.is_empty() && !self.passthrough {
            if self.data_left > 0 {
                let k = usize::try_from(self.data_left)
                    .unwrap_or(usize::MAX)
                    .min(b.len());
                if let Some((buf, size)) = &mut self.pax {
                    let want = size.saturating_sub(buf.len()).min(k);
                    buf.extend_from_slice(&b[..want]);
                }
                self.data_left -= k as u64;
                b = &b[k..];
                if self.data_left == 0
                    && let Some((buf, _)) = self.pax.take()
                {
                    // 바로 다음 항목 하나에 적용한다(뒤의 `x`가 앞의 것을 바꾼다).
                    if let Some(s) = pax_size(&buf) {
                        self.next_size = Some(s);
                    }
                }
                continue;
            }
            let k = (BLOCK - self.filled).min(b.len());
            self.header[self.filled..self.filled + k].copy_from_slice(&b[..k]);
            self.filled += k;
            b = &b[k..];
            if self.filled == BLOCK {
                self.filled = 0;
                self.on_header()?;
            }
        }
        Ok(())
    }

    fn on_header(&mut self) -> io::Result<()> {
        let h = &self.header;
        if h.iter().all(|&x| x == 0) {
            return Ok(());
        }
        let kind = h[156];
        let mut size = header_size(&h[124..136])?;
        let extension = matches!(kind, b'L' | b'K' | b'x' | b'g');
        if extension {
            if size > MAX_EXTENSION {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "a tar extension header ({}) of {size} bytes; at most {MAX_EXTENSION} are allowed",
                        kind as char
                    ),
                ));
            }
        } else if let Some(s) = self.next_size.take() {
            size = s;
        }
        if kind == b'S' {
            self.passthrough = true;
            return Ok(());
        }
        if kind == b'x' {
            // MAX_EXTENSION 이하라 usize에 들어간다.
            let n = size as usize;
            self.pax = Some((Vec::with_capacity(n), n));
        }
        self.data_left = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        if self.data_left == 0
            && let Some((buf, _)) = self.pax.take()
            && let Some(s) = pax_size(&buf)
        {
            self.next_size = Some(s);
        }
        Ok(())
    }
}

impl<R: Read> Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.observe(&buf[..n])?;
        Ok(n)
    }
}

/// 헤더의 크기 필드: 8진수 글자(NUL·공백으로 끝남) 또는 base-256.
fn header_size(f: &[u8]) -> io::Result<u64> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "bad size in a tar header");
    if f[0] & 0x80 != 0 {
        let mut v: u64 = 0;
        for &x in &f[1..] {
            v = v.checked_mul(256).ok_or_else(bad)? | u64::from(x);
        }
        return Ok(v);
    }
    let s: Vec<u8> = f
        .iter()
        .copied()
        .skip_while(|&c| c == b' ')
        .take_while(|&c| c != 0 && c != b' ')
        .collect();
    if s.is_empty() {
        return Ok(0);
    }
    let s = std::str::from_utf8(&s).map_err(|_| bad())?;
    u64::from_str_radix(s, 8).map_err(|_| bad())
}

/// PAX 기록(`<길이> <키>=<값>\n`)에서 `size`.
fn pax_size(mut b: &[u8]) -> Option<u64> {
    let mut found = None;
    while !b.is_empty() {
        let sp = b.iter().position(|&c| c == b' ')?;
        let len: usize = std::str::from_utf8(&b[..sp]).ok()?.parse().ok()?;
        if len <= sp + 1 || len > b.len() {
            return found;
        }
        let rec = &b[sp + 1..len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(v) = rec.strip_prefix(b"size=") {
            found = std::str::from_utf8(v).ok().and_then(|v| v.parse().ok());
        }
        b = &b[len..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(bytes: Vec<u8>) -> io::Result<Vec<(String, Vec<u8>)>> {
        let mut a = tar::Archive::new(Limited::new(io::Cursor::new(bytes)));
        let mut out = Vec::new();
        for e in a.entries()? {
            let mut e = e?;
            let p = String::from_utf8_lossy(&e.path_bytes()).into_owned();
            let mut d = Vec::new();
            e.read_to_end(&mut d)?;
            out.push((p, d));
        }
        Ok(out)
    }

    fn gnu_long(path_len: usize) -> Vec<u8> {
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(3);
        h.set_mode(0o644);
        let path = format!("{}/f", "d".repeat(path_len));
        b.append_data(&mut h, &path, &b"abc"[..]).unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(2);
        h.set_mode(0o644);
        b.append_data(&mut h, "next", &b"xy"[..]).unwrap();
        b.into_inner().unwrap()
    }

    #[test]
    fn ordinary_long_names_pass_through_unchanged() {
        let got = read_all(gnu_long(300)).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, b"abc");
        assert_eq!(got[1], ("next".into(), b"xy".to_vec()));
    }

    #[test]
    fn a_huge_extension_header_is_refused_before_it_is_read() {
        let e = read_all(gnu_long(MAX_EXTENSION as usize + 10)).unwrap_err();
        assert!(e.to_string().contains("extension header"), "{e}");
    }

    #[test]
    fn a_pax_size_moves_the_next_entry() {
        // 헤더의 크기 필드는 0, PAX `size`가 실제 크기(5)다. 따라가는 쪽이 이것을 놓치면 다음 항목을 헤더로 읽는다.
        let mut b = tar::Builder::new(Vec::new());
        b.append_pax_extensions([("size", &b"5"[..])]).unwrap();
        let mut h = tar::Header::new_ustar();
        h.set_path("a").unwrap();
        h.set_size(0);
        h.set_mode(0o644);
        h.set_cksum();
        let mut raw = b.into_inner().unwrap();
        // into_inner이 붙인 끝 표시(빈 블록 둘)를 떼고 이어 붙인다.
        raw.truncate(raw.len() - 2 * BLOCK);
        raw.extend_from_slice(h.as_bytes());
        let mut data = b"hello".to_vec();
        data.resize(BLOCK, 0);
        raw.extend_from_slice(&data);
        // 다음 항목은 크기 필드에 1 GiB짜리 긴 이름을 선언한 GNU `L`이다. 위 크기를 놓치면 이것을 보지 못한다.
        let mut l = tar::Header::new_gnu();
        l.set_entry_type(tar::EntryType::GNULongName);
        l.set_size(1 << 30);
        l.set_cksum();
        raw.extend_from_slice(l.as_bytes());
        raw.extend_from_slice(&[0; BLOCK]);
        let e = read_all(raw).unwrap_err();
        assert!(e.to_string().contains("extension header"), "{e}");
    }

    #[test]
    fn sizes_are_read_in_both_encodings() {
        assert_eq!(header_size(b"00000001750\0").unwrap(), 0o1750);
        assert_eq!(header_size(b"     17 \0\0\0\0").unwrap(), 0o17);
        assert_eq!(header_size(b"\0\0\0\0\0\0\0\0\0\0\0\0").unwrap(), 0);
        let mut f = [0u8; 12];
        f[0] = 0x80;
        f[11] = 1;
        f[10] = 2;
        assert_eq!(header_size(&f).unwrap(), 0x201);
        assert!(header_size(b"9\0\0\0\0\0\0\0\0\0\0\0").is_err());
        assert_eq!(pax_size(b"9 size=5\n"), Some(5));
        assert_eq!(pax_size(b"11 path=ab\n9 size=7\n"), Some(7));
        assert_eq!(pax_size(b"99 size=5\n"), None);
    }
}
