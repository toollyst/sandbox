//! sha256 계산. 받은 데이터를 믿지 않고 직접 계산해 digest와 대조할 때 쓴다.

use std::io::{self, Read, Write};

use sha2::{Digest as _, Sha256};

use sbx_model::Digest;

/// 바이트의 sha256 digest.
pub fn sha256(bytes: &[u8]) -> Digest {
    to_digest(Sha256::digest(bytes).as_slice())
}

fn to_digest(hash: &[u8]) -> Digest {
    let mut s = String::with_capacity(7 + 64);
    s.push_str("sha256:");
    for b in hash {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0xf), 16).unwrap_or('0'));
    }
    // 위에서 만든 형식은 항상 올바르다.
    Digest::parse(&s).unwrap_or_else(|_| unreachable!("sha256 hex is always valid"))
}

/// 읽으면서 sha256을 계산하는 reader.
#[derive(Debug)]
pub struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
    bytes: u64,
}

impl<R: Read> HashingReader<R> {
    /// 감싼다.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    /// 지금까지 읽은 바이트 수.
    pub fn byte_count(&self) -> u64 {
        self.bytes
    }

    /// 지금까지 읽은 데이터의 digest.
    pub fn digest(&self) -> Digest {
        to_digest(self.hasher.clone().finalize().as_slice())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
}

/// 쓰면서 sha256을 계산하는 writer.
#[derive(Debug)]
pub struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> HashingWriter<W> {
    /// 감싼다.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            bytes: 0,
        }
    }

    /// 지금까지 쓴 바이트 수.
    pub fn byte_count(&self) -> u64 {
        self.bytes
    }

    /// 지금까지 쓴 데이터의 digest.
    pub fn digest(&self) -> Digest {
        to_digest(self.hasher.clone().finalize().as_slice())
    }

    /// 감싼 writer를 돌려받는다.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn known_values() {
        assert_eq!(sha256(b"").as_str(), EMPTY);
        assert_eq!(sha256(b"abc").as_str(), ABC);
    }

    #[test]
    fn reader_and_writer_agree() {
        let mut r = HashingReader::new(&b"abc"[..]);
        let mut w = HashingWriter::new(Vec::new());
        io::copy(&mut r, &mut w).unwrap();
        assert_eq!(r.digest().as_str(), ABC);
        assert_eq!(w.digest().as_str(), ABC);
        assert_eq!((r.byte_count(), w.byte_count()), (3, 3));
    }
}
