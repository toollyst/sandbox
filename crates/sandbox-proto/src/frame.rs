//! 프레이밍: 본문 길이(u32, big-endian) 다음에 본문.

use std::io::{self, Read, Write};

/// 프레임 본문 최대 길이(바이트).
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// 프레임 하나를 쓴다. 형식은 본문 길이(u32, big-endian) 다음에 본문이다.
pub fn write_frame<W: Write + ?Sized>(w: &mut W, payload: &[u8]) -> io::Result<()> {
    if payload.len() > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame exceeds MAX_FRAME_LEN",
        ));
    }
    // MAX_FRAME_LEN이 u32 범위 안이므로 잘리지 않는다.
    w.write_all(&(payload.len() as u32).to_be_bytes())?;
    w.write_all(payload)
}

/// 프레임 하나를 읽는다. 길이가 상한을 넘으면 본문을 읽지 않고 오류를 낸다.
pub fn read_frame<R: Read + ?Sized>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds MAX_FRAME_LEN",
        ));
    }
    let mut buf = vec![0; len];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"hello").unwrap();
        write_frame(&mut wire, b"").unwrap();
        let mut r = wire.as_slice();
        assert_eq!(read_frame(&mut r).unwrap(), b"hello");
        assert_eq!(read_frame(&mut r).unwrap(), b"");
        assert_eq!(
            read_frame(&mut r).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn oversized_length_is_rejected_before_reading_body() {
        let wire = ((MAX_FRAME_LEN + 1) as u32).to_be_bytes();
        let err = read_frame(&mut wire.as_slice()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_body_is_an_error() {
        let mut wire = 10u32.to_be_bytes().to_vec();
        wire.extend_from_slice(b"abc");
        let err = read_frame(&mut wire.as_slice()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
