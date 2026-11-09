//! 본문 인코딩의 기본 단위.
//!
//! [`Decoder`]는 남은 바이트보다 많이 읽지 않는다. 목록의 개수를 보고 미리 할당하지도 않는다.
//! 그래서 상대가 개수를 크게 속여도 메모리를 쓰지 않고 오류로 끝난다.

use std::fmt;

/// 해석 오류.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed message: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

impl From<DecodeError> for std::io::Error {
    fn from(e: DecodeError) -> Self {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    }
}

/// 해석 결과.
pub type DecodeResult<T> = Result<T, DecodeError>;

fn err<T>(m: impl Into<String>) -> DecodeResult<T> {
    Err(DecodeError(m.into()))
}

/// 본문 쓰기.
#[derive(Debug, Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    /// 메시지 종류로 시작한다.
    pub fn new(tag: u8) -> Self {
        Self { buf: vec![tag] }
    }

    /// 다 쓴 본문.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// u8.
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    /// bool. 0 또는 1.
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.u8(v as u8)
    }

    /// u16.
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// u32.
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// i32.
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// u64.
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// i64.
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }

    /// 길이(u32) 다음에 내용. 길이가 u32를 넘으면 패닉한다. 프레임 상한이 그보다 훨씬 작다.
    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        let len = u32::try_from(v.len()).expect("field longer than u32::MAX");
        self.u32(len);
        self.buf.extend_from_slice(v);
        self
    }

    /// UTF-8 문자열.
    pub fn str(&mut self, v: &str) -> &mut Self {
        self.bytes(v.as_bytes())
    }

    /// 개수(u32) 다음에 항목.
    pub fn list<T>(&mut self, items: &[T], mut f: impl FnMut(&mut Self, &T)) -> &mut Self {
        let len = u32::try_from(items.len()).expect("list longer than u32::MAX");
        self.u32(len);
        for item in items {
            f(self, item);
        }
        self
    }

    /// 0, 또는 1 다음에 값.
    pub fn opt<T>(&mut self, v: Option<&T>, f: impl FnOnce(&mut Self, &T)) -> &mut Self {
        match v {
            None => self.u8(0),
            Some(v) => {
                self.u8(1);
                f(self, v);
                self
            }
        }
    }
}

/// 본문 읽기.
#[derive(Debug)]
pub struct Decoder<'a> {
    rest: &'a [u8],
}

impl<'a> Decoder<'a> {
    /// 본문 전체를 받는다.
    pub fn new(body: &'a [u8]) -> Self {
        Self { rest: body }
    }

    /// 남은 바이트가 없어야 한다.
    pub fn end(&self) -> DecodeResult<()> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            err(format!("{} trailing bytes", self.rest.len()))
        }
    }

    fn take(&mut self, n: usize) -> DecodeResult<&'a [u8]> {
        if self.rest.len() < n {
            return err(format!("needs {n} bytes, {} left", self.rest.len()));
        }
        let (head, tail) = self.rest.split_at(n);
        self.rest = tail;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> DecodeResult<[u8; N]> {
        let mut a = [0; N];
        a.copy_from_slice(self.take(N)?);
        Ok(a)
    }

    /// u8.
    pub fn u8(&mut self) -> DecodeResult<u8> {
        Ok(self.take(1)?[0])
    }

    /// bool. 0과 1 외의 값은 오류.
    pub fn bool(&mut self) -> DecodeResult<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => err(format!("bool must be 0 or 1, got {v}")),
        }
    }

    /// u16.
    pub fn u16(&mut self) -> DecodeResult<u16> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    /// u32.
    pub fn u32(&mut self) -> DecodeResult<u32> {
        Ok(u32::from_be_bytes(self.array()?))
    }

    /// i32.
    pub fn i32(&mut self) -> DecodeResult<i32> {
        Ok(i32::from_be_bytes(self.array()?))
    }

    /// u64.
    pub fn u64(&mut self) -> DecodeResult<u64> {
        Ok(u64::from_be_bytes(self.array()?))
    }

    /// i64.
    pub fn i64(&mut self) -> DecodeResult<i64> {
        Ok(i64::from_be_bytes(self.array()?))
    }

    /// 길이(u32) 다음에 내용.
    pub fn bytes(&mut self) -> DecodeResult<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    /// UTF-8 문자열.
    pub fn string(&mut self) -> DecodeResult<String> {
        let b = self.bytes()?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .or_else(|_| err("string is not UTF-8"))
    }

    /// 개수(u32) 다음에 항목. 개수만큼 미리 할당하지 않는다.
    pub fn list<T>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> DecodeResult<T>,
    ) -> DecodeResult<Vec<T>> {
        let n = self.u32()?;
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(f(self)?);
        }
        Ok(out)
    }

    /// 0, 또는 1 다음에 값.
    pub fn opt<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> DecodeResult<T>,
    ) -> DecodeResult<Option<T>> {
        match self.u8()? {
            0 => Ok(None),
            1 => f(self).map(Some),
            v => err(format!("option tag must be 0 or 1, got {v}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_roundtrip() {
        let mut e = Encoder::new(7);
        e.u8(1)
            .bool(true)
            .u16(0xBEEF)
            .u32(5)
            .i32(-3)
            .u64(u64::MAX)
            .i64(-1)
            .bytes(b"ab")
            .str("한글")
            .list(&[1u32, 2], |e, v| {
                e.u32(*v);
            })
            .opt(None::<&u32>, |e, v| {
                e.u32(*v);
            })
            .opt(Some(&9u32), |e, v| {
                e.u32(*v);
            });
        let body = e.finish();
        let mut d = Decoder::new(&body);
        assert_eq!(d.u8().unwrap(), 7);
        assert_eq!(d.u8().unwrap(), 1);
        assert!(d.bool().unwrap());
        assert_eq!(d.u16().unwrap(), 0xBEEF);
        assert_eq!(d.u32().unwrap(), 5);
        assert_eq!(d.i32().unwrap(), -3);
        assert_eq!(d.u64().unwrap(), u64::MAX);
        assert_eq!(d.i64().unwrap(), -1);
        assert_eq!(d.bytes().unwrap(), b"ab");
        assert_eq!(d.string().unwrap(), "한글");
        assert_eq!(d.list(|d| d.u32()).unwrap(), vec![1, 2]);
        assert_eq!(d.opt(|d| d.u32()).unwrap(), None);
        assert_eq!(d.opt(|d| d.u32()).unwrap(), Some(9));
        d.end().unwrap();
    }

    #[test]
    fn lies_about_length_are_errors() {
        // 바이트열 길이를 남은 것보다 크게.
        let body = [0, 0, 0, 10, b'a'];
        assert!(Decoder::new(&body).bytes().is_err());
        // 목록 개수를 u32::MAX로. 항목을 읽다가 오류가 나야 하고, 미리 할당하지 않는다.
        let body = [0xFF, 0xFF, 0xFF, 0xFF];
        assert!(Decoder::new(&body).list(|d| d.u32()).is_err());
    }

    #[test]
    fn invalid_values_are_errors() {
        assert!(Decoder::new(&[2]).bool().is_err());
        assert!(Decoder::new(&[2]).opt(|d| d.u8()).is_err());
        assert!(Decoder::new(&[0, 0, 0, 1, 0xFF]).string().is_err());
        let mut d = Decoder::new(&[1, 2]);
        d.u8().unwrap();
        assert!(d.end().is_err());
    }
}
