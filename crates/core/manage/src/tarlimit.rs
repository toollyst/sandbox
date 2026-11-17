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
//!
//! PAX 기록도 여기서 읽는다([`parse_pax`]). 기록은 길이로 나뉘고 값에는 줄바꿈을 포함한 아무 바이트나 올 수 있다
//! (xattr 값은 바이너리다. POSIX, Go `archive/tar`, GNU tar가 그렇게 읽는다). `tar` 크레이트는 줄바꿈으로 나누어
//! 그런 값을 "malformed pax extension"으로 거부하고, `path`·`size` 같은 기록을 값 안의 줄에서 찾을 수도 있다. 그래서
//! 다음 항목에 붙은 기록을 [`PaxSlot`]으로 넘기고, 레이어 읽기가 xattr을 거기서 얻고 크레이트가 본 경로·크기와
//! 견준다(다르면 읽는 쪽마다 다른 레이어이므로 거부한다).

use std::cell::RefCell;
use std::io::{self, Read};
use std::rc::Rc;

/// 확장 헤더 하나의 내용 상한(바이트).
pub const MAX_EXTENSION: u64 = 1 << 20;

const BLOCK: usize = 512;

/// PAX 기록들(키, 값), 적힌 순서. 같은 키가 또 오면 뒤의 것이 이긴다.
pub type PaxRecords = Vec<(Vec<u8>, Vec<u8>)>;

/// 마지막으로 읽은 항목 헤더에 붙은 PAX 확장 헤더의 내용([`parse_pax`]로 읽히는 것만). [`Limited`]가 채우고, 그
/// 항목을 받은 쪽이 [`PaxSlot::take`]한다.
#[derive(Clone, Default)]
pub struct PaxSlot(Rc<RefCell<Vec<u8>>>);

impl PaxSlot {
    /// 지금 항목의 PAX 내용을 꺼낸다(없으면 빈 것).
    pub fn take(&self) -> Vec<u8> {
        std::mem::take(&mut self.0.borrow_mut())
    }
}

/// `records`에서 `key`의 값(마지막 것).
pub fn pax_value<'a>(records: &'a PaxRecords, key: &[u8]) -> Option<&'a [u8]> {
    records
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_slice())
}

/// [`Read`]를 감싸 확장 헤더의 크기를 본다.
pub struct Limited<R> {
    inner: R,
    header: [u8; BLOCK],
    filled: usize,
    /// 지금 항목의 남은 데이터(512 단위로 올린 것).
    data_left: u64,
    /// 모으는 `x` 헤더 내용과 그 실제 크기.
    pax: Option<(Vec<u8>, usize)>,
    /// 다음 항목에 붙을 PAX 내용(다 읽은 `x`).
    next_pax: Option<Vec<u8>>,
    /// 다음 항목의 크기(`x`의 `size`).
    next_size: Option<u64>,
    /// 지금 항목의 기록을 넘기는 곳.
    slot: PaxSlot,
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
            next_pax: None,
            next_size: None,
            slot: PaxSlot::default(),
            passthrough: false,
        }
    }

    /// 항목마다 그 PAX 기록을 받는 곳. 감싼 것을 `tar::Archive`에 넘기기 전에 얻는다.
    pub fn pax_slot(&self) -> PaxSlot {
        self.slot.clone()
    }

    /// 다 모은 `x` 헤더 내용: 바로 다음 항목 하나에 적용한다(뒤의 `x`가 앞의 것을 바꾼다).
    fn pax_done(&mut self, buf: Vec<u8>) -> io::Result<()> {
        let records = parse_pax(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if let Some(v) = pax_value(&records, b"size") {
            let s = std::str::from_utf8(v)
                .ok()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a PAX size that is not a number",
                    )
                })?;
            self.next_size = Some(s);
        }
        self.next_pax = Some(buf);
        Ok(())
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
                    self.pax_done(buf)?;
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
        } else {
            if let Some(s) = self.next_size.take() {
                size = s;
            }
            *self.slot.0.borrow_mut() = self.next_pax.take().unwrap_or_default();
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
        {
            self.pax_done(buf)?;
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

/// PAX 확장 헤더의 내용: `<길이> <키>=<값>\n`이 이어진 것. 길이는 그 기록 전체(자기 자릿수와 줄바꿈 포함)의
/// 바이트 수이고, 값은 그 길이 안의 아무 바이트다(줄바꿈도). 모양이 어긋나면 오류다(Go `archive/tar`처럼).
pub fn parse_pax(mut b: &[u8]) -> Result<PaxRecords, &'static str> {
    const BAD: &str = "malformed PAX record";
    let mut out = Vec::new();
    while !b.is_empty() {
        let sp = b.iter().position(|&c| c == b' ').ok_or(BAD)?;
        let digits = &b[..sp];
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return Err(BAD);
        }
        let len: usize = std::str::from_utf8(digits)
            .ok()
            .and_then(|d| d.parse().ok())
            .ok_or(BAD)?;
        if len <= sp + 1 || len > b.len() {
            return Err(BAD);
        }
        let rec = b[sp + 1..len].strip_suffix(b"\n").ok_or(BAD)?;
        let eq = rec.iter().position(|&c| c == b'=').ok_or(BAD)?;
        if eq == 0 {
            return Err(BAD);
        }
        out.push((rec[..eq].to_vec(), rec[eq + 1..].to_vec()));
        b = &b[len..];
    }
    Ok(out)
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
    }

    #[test]
    fn pax_records_are_split_by_length_not_by_newline() {
        let size = |b: &[u8]| {
            parse_pax(b)
                .map(|r| pax_value(&r, b"size").map(<[u8]>::to_vec))
                .ok()
                .flatten()
        };
        assert_eq!(size(b"9 size=5\n"), Some(b"5".to_vec()));
        assert_eq!(size(b"11 path=ab\n9 size=7\n"), Some(b"7".to_vec()));
        // 값 안의 줄바꿈과, 기록처럼 보이는 줄.
        let r = parse_pax(b"35 SCHILY.xattr.user.a=x\n9 size=9\n\n").unwrap();
        assert_eq!(
            r,
            vec![(b"SCHILY.xattr.user.a".to_vec(), b"x\n9 size=9\n".to_vec())]
        );
        assert_eq!(pax_value(&r, b"size"), None);
        for bad in [
            &b"99 size=5\n"[..],
            b"9 size=5x",
            b"x size=5\n",
            b"5 =5\n",
            b"7 size\n",
            b"9 size=5\n3",
        ] {
            assert!(
                parse_pax(bad).is_err(),
                "{:?}",
                String::from_utf8_lossy(bad)
            );
        }
    }
}
