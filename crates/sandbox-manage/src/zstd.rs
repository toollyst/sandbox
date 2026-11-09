//! zstd 레이어 풀기 (`tar+zstd`). 순수 Rust 디코더(`ruzstd`)를 쓴다.
//!
//! 레이어 하나가 프레임 여럿을 이어 붙인 것일 수 있다(zstd 형식이 허용하고, `zstd:chunked` 이미지가 그렇다).
//! 프레임을 차례로 풀어 하나로 잇는다. 건너뛰는 프레임(skippable frame, 메타데이터)은 건너뛴다.
//! 프레임 머리를 읽기 전에 입력이 끝나면 끝이다.

use std::io::{self, BufRead, Read};

use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
use ruzstd::decoding::{BlockDecodingStrategy, FrameDecoder};

/// 이어 붙인 zstd 프레임을 푸는 읽기.
pub struct ZstdReader<R> {
    src: R,
    dec: FrameDecoder,
    in_frame: bool,
}

impl<R: BufRead> ZstdReader<R> {
    /// 만든다. 첫 프레임은 처음 읽을 때 연다.
    pub fn new(src: R) -> Self {
        Self {
            src,
            dec: FrameDecoder::new(),
            in_frame: false,
        }
    }
}

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("zstd: {e}"))
}

impl<R: BufRead> Read for ZstdReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.in_frame {
                while self.dec.can_collect() == 0 && !self.dec.is_finished() {
                    self.dec
                        .decode_blocks(&mut self.src, BlockDecodingStrategy::UptoBlocks(1))
                        .map_err(invalid)?;
                }
                let n = self.dec.read(buf)?;
                if n > 0 {
                    return Ok(n);
                }
                // 이 프레임을 다 냈다.
                self.in_frame = false;
            }
            if self.src.fill_buf()?.is_empty() {
                return Ok(0);
            }
            match self.dec.init(&mut self.src) {
                Ok(()) => self.in_frame = true,
                Err(FrameDecoderError::ReadFrameHeaderError(ReadFrameHeaderError::SkipFrame {
                    length,
                    ..
                })) => {
                    let skipped = io::copy(
                        &mut (&mut self.src).take(u64::from(length)),
                        &mut io::sink(),
                    )?;
                    if skipped != u64::from(length) {
                        return Err(invalid("truncated skippable frame"));
                    }
                }
                Err(e) => return Err(invalid(e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    fn unzstd(b: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        ZstdReader::new(b).read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn frames_are_joined_and_skippable_frames_skipped() {
        let a = vec![b'a'; 300_000];
        let b = b"second frame".to_vec();
        let mut blob = compress_to_vec(a.as_slice(), CompressionLevel::Fastest);
        // 건너뛰는 프레임: 마법수(0x184D2A50..=0x184D2A5F) + 길이 + 내용.
        blob.extend_from_slice(&0x184D_2A50u32.to_le_bytes());
        blob.extend_from_slice(&3u32.to_le_bytes());
        blob.extend_from_slice(b"xyz");
        blob.extend_from_slice(&compress_to_vec(b.as_slice(), CompressionLevel::Fastest));
        assert_eq!(unzstd(&blob).unwrap(), [a, b].concat());
        assert_eq!(unzstd(b"").unwrap(), b"");
    }

    #[test]
    fn damage_is_an_error() {
        let blob = compress_to_vec(&b"hello world, hello world"[..], CompressionLevel::Fastest);
        assert!(unzstd(&blob[..blob.len() - 2]).is_err());
        assert!(unzstd(b"not zstd at all").is_err());
        let mut skip = 0x184D_2A50u32.to_le_bytes().to_vec();
        skip.extend_from_slice(&10u32.to_le_bytes());
        skip.extend_from_slice(b"abc");
        assert!(unzstd(&skip).is_err());
    }
}
