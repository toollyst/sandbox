//! 기본 프로세스 출력 로그 `logs/main.log`. 크레이트의 가장 아래 모듈이다.
//!
//! 형식: 기록이 이어진다. 기록 하나는 `[흐름 1바이트][길이 u32 빅 엔디언][바이트]`이다.
//! 흐름은 1이 표준 출력, 2가 표준 오류다. 표준 출력과 표준 오류가 나온 순서를 지킨다.
//! 샌드박스를 다시 시작하면 뒤에 이어 쓴다(Docker 로그와 같다).
//!
//! 읽는 쪽은 쓰는 중인 파일을 읽을 수 있다. 끝에 반쯤 쓰인 기록은 아직 없는 것으로 본다.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// 로그 파일 이름 (샌드박스 로그 디렉터리 안).
pub const MAIN_LOG: &str = "main.log";

/// 출력 흐름.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// 표준 출력.
    Stdout,
    /// 표준 오류.
    Stderr,
}

impl Stream {
    fn byte(self) -> u8 {
        match self {
            Stream::Stdout => 1,
            Stream::Stderr => 2,
        }
    }

    fn from_byte(b: u8) -> io::Result<Self> {
        match b {
            1 => Ok(Stream::Stdout),
            2 => Ok(Stream::Stderr),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bad log record stream {b}"),
            )),
        }
    }
}

/// 기록 하나의 최대 길이. 프로세스 출력 한 조각은 이보다 작다.
const MAX_RECORD: u32 = 16 * 1024 * 1024;

/// 쓰는 쪽. 기록마다 한 번에 쓴다.
#[derive(Debug)]
pub struct LogWriter(File);

impl LogWriter {
    /// 이어 쓰기로 연다. 디렉터리가 없으면 만든다.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(Self(
            OpenOptions::new().create(true).append(true).open(path)?,
        ))
    }

    /// 기록 하나를 쓴다.
    pub fn write(&mut self, stream: Stream, data: &[u8]) -> io::Result<()> {
        for chunk in data.chunks(MAX_RECORD as usize) {
            let mut rec = Vec::with_capacity(5 + chunk.len());
            rec.push(stream.byte());
            rec.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
            rec.extend_from_slice(chunk);
            self.0.write_all(&rec)?;
        }
        Ok(())
    }
}

/// 읽는 쪽. 읽은 위치를 기억하므로 파일이 자라면 이어 읽는다(`logs -f`).
#[derive(Debug)]
pub struct LogReader {
    file: File,
    pos: u64,
}

impl LogReader {
    /// 연다. 파일이 없으면 `NotFound`.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            pos: 0,
        })
    }

    /// 다음 기록. 지금 끝까지 다 읽었으면(끝에 반쯤 쓰인 기록만 있어도) `None`.
    pub fn next_record(&mut self) -> io::Result<Option<(Stream, Vec<u8>)>> {
        self.file.seek(SeekFrom::Start(self.pos))?;
        let mut head = [0u8; 5];
        if !read_full(&mut self.file, &mut head)? {
            return Ok(None);
        }
        let stream = Stream::from_byte(head[0])?;
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
        if len > MAX_RECORD {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("log record of {len} bytes"),
            ));
        }
        let mut data = vec![0u8; len as usize];
        if !read_full(&mut self.file, &mut data)? {
            return Ok(None);
        }
        self.pos += 5 + u64::from(len);
        Ok(Some((stream, data)))
    }
}

/// 다 채웠으면 `true`, 그 전에 파일이 끝나면 `false`.
fn read_full(r: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => return Ok(false),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_keep_order_and_partial_tail_is_not_read_yet() {
        let dir = std::env::temp_dir().join(format!("sbx-shim-log-{}", std::process::id()));
        let path = dir.join("logs").join(MAIN_LOG);
        let mut w = LogWriter::open(&path).unwrap();
        w.write(Stream::Stdout, b"out1").unwrap();
        w.write(Stream::Stderr, b"err").unwrap();
        let mut r = LogReader::open(&path).unwrap();
        assert_eq!(
            r.next_record().unwrap(),
            Some((Stream::Stdout, b"out1".to_vec()))
        );
        assert_eq!(
            r.next_record().unwrap(),
            Some((Stream::Stderr, b"err".to_vec()))
        );
        assert_eq!(r.next_record().unwrap(), None);

        // 반쯤 쓰인 기록: 머리만 있다.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(&[1, 0, 0, 0, 3, b'a']).unwrap();
        assert_eq!(r.next_record().unwrap(), None);
        f.write_all(b"bc").unwrap();
        assert_eq!(
            r.next_record().unwrap(),
            Some((Stream::Stdout, b"abc".to_vec()))
        );

        // 다시 열어 이어 쓴다.
        let mut w = LogWriter::open(&path).unwrap();
        w.write(Stream::Stdout, b"again").unwrap();
        assert_eq!(
            r.next_record().unwrap(),
            Some((Stream::Stdout, b"again".to_vec()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
