//! 기본 프로세스 출력 로그 `logs/main.log`. 크레이트의 가장 아래 모듈이다.
//!
//! 형식(판 2): 머리 [`HEADER`](`sbx-log`와 판 번호 바이트 2, 8바이트) 뒤에 기록이 이어진다.
//! 기록 하나는 `[흐름 1바이트][시각 u64 빅 엔디언][길이 u32 빅 엔디언][바이트]`이다.
//!
//! - 흐름은 1이 표준 출력, 2가 표준 오류다. 표준 출력과 표준 오류가 나온 순서를 지킨다.
//! - 시각은 shim이 출력 조각을 받은 때(유닉스 시각, 나노초)다. 한 조각 안의 줄들은 같은 시각이다.
//!   쓰는 쪽 하나 안에서는 시각이 줄지 않는다(벽시계가 뒤로 가면 앞 기록의 시각을 쓴다).
//!
//! 판 1(머리도 시각도 없는 형식)은 0.1.x만 썼고 0.1.x는 거뒀으므로 읽지 않는다(모르는 파일과 같다).
//!
//! 명시적 동작:
//!
//! - 새 파일(비어 있는 파일 포함)은 머리부터 쓴다.
//! - 샌드박스를 다시 시작하면 뒤에 이어 쓴다(Docker 로그와 같다). 이미 있는 파일은 머리를 확인하고 이어 쓴다.
//! - 모르는 판(머리는 맞고 판 번호가 다르다)이나 모르는 첫 바이트면 쓰지도 읽지도 않고 `InvalidData`.
//! - 파일이 [`MAX_LOG`]에 닿으면 그 뒤 출력은 버리고, 그 사실을 표준 오류 기록 하나로 남긴다(게스트가 끝없이
//!   출력해 호스트 디스크를 채우지 못한다). 머리도 크기에 든다.
//! - 읽는 쪽은 쓰는 중인 파일을 읽을 수 있다. 끝에 반쯤 쓰인 기록(반쯤 쓰인 머리도)은 아직 없는 것으로 본다.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 로그 파일 이름 (샌드박스 로그 디렉터리 안).
pub const MAIN_LOG: &str = "main.log";

/// 판 2 파일의 머리: `sbx-log`와 판 번호.
pub const HEADER: &[u8; 8] = b"sbx-log\x02";

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
            _ => Err(invalid(format!("bad log record stream {b}"))),
        }
    }
}

/// 기록 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    /// 흐름.
    pub stream: Stream,
    /// shim이 받은 시각(유닉스 시각, 나노초).
    pub time_ns: u64,
    /// 출력 조각.
    pub data: Vec<u8>,
}

/// 기록 하나의 머리 길이(데이터 앞): 흐름 1, 시각 8, 길이 4.
const RECORD_HEAD: u64 = 13;

/// 파일 앞 `head`(파일 앞에서 많아야 8바이트)가 온전한 머리인가. 비어 있거나 머리가 반쯤이면 `false`(아직 모른다).
fn detect(head: &[u8]) -> io::Result<bool> {
    if head.len() < HEADER.len() {
        return if HEADER.starts_with(head) {
            Ok(false)
        } else {
            Err(invalid("not an sbx log file".to_owned()))
        };
    }
    if head[..7] != HEADER[..7] {
        return Err(invalid("not an sbx log file".to_owned()));
    }
    if head[7] != HEADER[7] {
        return Err(invalid(format!(
            "log format version {} is not known to this sbx",
            head[7]
        )));
    }
    Ok(true)
}

/// 파일 앞에서 많아야 8바이트.
fn read_head(file: &mut File) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut head = Vec::with_capacity(HEADER.len());
    Read::by_ref(file)
        .take(HEADER.len() as u64)
        .read_to_end(&mut head)?;
    Ok(head)
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// 기록 하나의 최대 길이. 프로세스 출력 한 조각은 이보다 작다.
const MAX_RECORD: u32 = 16 * 1024 * 1024;

/// 로그 파일의 최대 바이트.
pub const MAX_LOG: u64 = 256 << 20;

/// 한도에 닿았을 때 남기는 기록.
pub const LOG_FULL: &[u8] =
    b"\n[sbx: the log reached its size limit; further output is not kept]\n";

/// 쓰는 쪽. 기록마다 한 번에 쓴다.
#[derive(Debug)]
pub struct LogWriter {
    file: File,
    /// 지금 크기.
    len: u64,
    limit: u64,
    /// 한도에 닿아 더 쓰지 않는다.
    full: bool,
    /// 마지막으로 쓴 시각. 시각이 줄지 않게 한다.
    last_ns: u64,
}

impl LogWriter {
    /// 이어 쓰기로 연다. 디렉터리가 없으면 만든다. 한도는 [`MAX_LOG`].
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::open_with_limit(path, MAX_LOG)
    }

    /// 한도를 정해 연다. 비어 있으면 [`HEADER`]를 쓰고, 아니면 머리를 확인하고 이어 쓴다.
    pub fn open_with_limit(path: &Path, limit: u64) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(path)?;
        let mut len = file.metadata()?.len();
        if len == 0 {
            file.write_all(HEADER)?;
            len = HEADER.len() as u64;
        } else if !detect(&read_head(&mut file)?)? {
            return Err(invalid("log file header is cut short".to_owned()));
        }
        Ok(Self {
            file,
            len,
            limit,
            full: len >= limit,
            last_ns: 0,
        })
    }

    /// 기록 하나를 지금 시각으로 쓴다. 한도를 넘는 부분은 버린다.
    pub fn write(&mut self, stream: Stream, data: &[u8]) -> io::Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        self.write_at(stream, now, data)
    }

    /// 기록 하나를 `time_ns`로 쓴다. 앞 기록보다 이르면 앞 기록의 시각을 쓴다.
    fn write_at(&mut self, stream: Stream, time_ns: u64, data: &[u8]) -> io::Result<()> {
        let time_ns = time_ns.max(self.last_ns);
        self.last_ns = time_ns;
        let head = RECORD_HEAD;
        // 한도 알림 기록이 들어갈 자리를 늘 남긴다.
        let reserve = head + LOG_FULL.len() as u64 + head;
        for chunk in data.chunks(MAX_RECORD as usize) {
            if self.full {
                return Ok(());
            }
            let room = self.limit.saturating_sub(self.len + reserve);
            let chunk = &chunk[..chunk.len().min(room as usize)];
            if !chunk.is_empty() {
                self.record(stream, time_ns, chunk)?;
            }
            if self.len + reserve >= self.limit {
                self.record(Stream::Stderr, time_ns, LOG_FULL)?;
                self.full = true;
            }
        }
        Ok(())
    }

    fn record(&mut self, stream: Stream, time_ns: u64, chunk: &[u8]) -> io::Result<()> {
        let mut rec = Vec::with_capacity(RECORD_HEAD as usize + chunk.len());
        rec.push(stream.byte());
        rec.extend_from_slice(&time_ns.to_be_bytes());
        rec.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
        rec.extend_from_slice(chunk);
        self.file.write_all(&rec)?;
        self.len += rec.len() as u64;
        Ok(())
    }
}

/// 읽는 쪽. 읽은 위치를 기억하므로 파일이 자라면 이어 읽는다(`logs -f`).
#[derive(Debug)]
pub struct LogReader {
    file: File,
    /// 머리를 확인했다. 파일이 비어 있거나 머리가 반쯤이면 아직이다.
    started: bool,
    pos: u64,
}

impl LogReader {
    /// 연다. 파일이 없으면 `NotFound`.
    pub fn open(path: &Path) -> io::Result<Self> {
        Ok(Self {
            file: File::open(path)?,
            started: false,
            pos: 0,
        })
    }

    /// 다음 기록. 지금 끝까지 다 읽었으면(끝에 반쯤 쓰인 기록이나 머리만 있어도) `None`.
    pub fn next_record(&mut self) -> io::Result<Option<Record>> {
        if !self.started {
            if !detect(&read_head(&mut self.file)?)? {
                return Ok(None);
            }
            self.started = true;
            self.pos = HEADER.len() as u64;
        }
        self.file.seek(SeekFrom::Start(self.pos))?;
        let mut head = [0u8; RECORD_HEAD as usize];
        if !read_full(&mut self.file, &mut head)? {
            return Ok(None);
        }
        let stream = Stream::from_byte(head[0])?;
        let time_ns = u64::from_be_bytes(head[1..9].try_into().expect("8 bytes"));
        let len = u32::from_be_bytes(head[9..13].try_into().expect("4 bytes"));
        if len > MAX_RECORD {
            return Err(invalid(format!("log record of {len} bytes")));
        }
        let mut data = vec![0u8; len as usize];
        if !read_full(&mut self.file, &mut data)? {
            return Ok(None);
        }
        self.pos += RECORD_HEAD + u64::from(len);
        Ok(Some(Record {
            stream,
            time_ns,
            data,
        }))
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
    use std::path::PathBuf;

    /// 테스트마다 따로 쓰는 로그 경로.
    fn log_path(name: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sbx-shim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.join("logs").join(MAIN_LOG), dir)
    }

    fn rec(stream: Stream, time_ns: u64, data: &[u8]) -> Option<Record> {
        Some(Record {
            stream,
            time_ns,
            data: data.to_vec(),
        })
    }

    fn append(path: &Path, bytes: &[u8]) {
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    #[test]
    fn records_keep_order_and_time_and_partial_tail_is_not_read_yet() {
        let (path, dir) = log_path("log");
        let mut w = LogWriter::open(&path).unwrap();
        w.write_at(Stream::Stdout, 10, b"out1").unwrap();
        w.write_at(Stream::Stderr, 20, b"err").unwrap();
        assert_eq!(&std::fs::read(&path).unwrap()[..8], HEADER);
        let mut r = LogReader::open(&path).unwrap();
        assert_eq!(r.next_record().unwrap(), rec(Stream::Stdout, 10, b"out1"));
        assert_eq!(r.next_record().unwrap(), rec(Stream::Stderr, 20, b"err"));
        assert_eq!(r.next_record().unwrap(), None);

        // 반쯤 쓰인 기록: 머리 일부, 그다음 머리 전부와 데이터 일부.
        append(&path, &[1, 0, 0, 0]);
        assert_eq!(r.next_record().unwrap(), None);
        append(&path, &[0, 0, 0, 0, 30, 0, 0, 0, 3, b'a']);
        assert_eq!(r.next_record().unwrap(), None);
        append(&path, b"bc");
        assert_eq!(r.next_record().unwrap(), rec(Stream::Stdout, 30, b"abc"));

        // 다시 열어 이어 쓴다. 머리를 다시 쓰지 않는다.
        let mut w = LogWriter::open(&path).unwrap();
        w.write_at(Stream::Stdout, 40, b"again").unwrap();
        assert_eq!(r.next_record().unwrap(), rec(Stream::Stdout, 40, b"again"));
        assert_eq!(r.next_record().unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn times_are_now_and_never_go_back_within_a_writer() {
        let (path, dir) = log_path("time");
        let now = || {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        };
        let mut w = LogWriter::open(&path).unwrap();
        let before = now();
        w.write(Stream::Stdout, b"a").unwrap();
        let after = now();
        // 벽시계가 뒤로 갔다.
        w.write_at(Stream::Stdout, 5, b"b").unwrap();
        let mut r = LogReader::open(&path).unwrap();
        let a = r.next_record().unwrap().unwrap().time_ns;
        assert!(before <= a && a <= after, "{before} {a} {after}");
        let b = r.next_record().unwrap().unwrap().time_ns;
        assert_eq!(b, a);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_or_half_written_header_is_not_read_yet() {
        let (path, dir) = log_path("head");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        let mut r = LogReader::open(&path).unwrap();
        assert_eq!(r.next_record().unwrap(), None);
        append(&path, b"sbx-");
        assert_eq!(r.next_record().unwrap(), None);
        append(&path, b"log\x02");
        assert_eq!(r.next_record().unwrap(), None);
        append(&path, &[2, 0, 0, 0, 0, 0, 0, 0, 7, 0, 0, 0, 1, b'x']);
        assert_eq!(r.next_record().unwrap(), rec(Stream::Stderr, 7, b"x"));

        // 비어 있는 파일에 쓰면 머리부터 쓴다.
        std::fs::write(&path, b"").unwrap();
        LogWriter::open(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), HEADER);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_versions_are_refused() {
        let (path, dir) = log_path("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 0.1.x가 쓴 판 1(머리 없이 흐름 바이트로 시작)도 모르는 파일이다.
        for bytes in [
            &b"sbx-log\x03"[..],
            b"garbage!",
            &[1, 0, 0, 0, 2, b'h', b'i', 0][..],
        ] {
            std::fs::write(&path, bytes).unwrap();
            let e = LogReader::open(&path).unwrap().next_record().unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{bytes:?}");
            let e = LogWriter::open(&path).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{bytes:?}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "must not write");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn endless_output_stops_at_the_limit_with_one_note() {
        {
            let (path, dir) = log_path("full");
            let limit = 1000;
            let mut w = LogWriter::open_with_limit(&path, limit).unwrap();
            for _ in 0..100 {
                w.write(Stream::Stdout, &[b'y'; 64]).unwrap();
            }
            let len = std::fs::metadata(&path).unwrap().len();
            assert!(len <= limit, "{len}");
            let mut r = LogReader::open(&path).unwrap();
            let mut last = None;
            while let Some(rec) = r.next_record().unwrap() {
                last = Some((rec.stream, rec.data));
            }
            assert_eq!(last, Some((Stream::Stderr, LOG_FULL.to_vec())));
            // 다시 열어도(재시작) 더 쓰지 않는다.
            let mut w = LogWriter::open_with_limit(&path, limit).unwrap();
            w.write(Stream::Stdout, b"more").unwrap();
            assert_eq!(std::fs::metadata(&path).unwrap().len(), len);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
