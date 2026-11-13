//! 게스트 시계 흉내. 맨 아래 모듈이다. **호스트 시계는 건드리지 않는다.**
//!
//! scratch 디스크 파일 이름 뒤에 `.clock`을 붙인 파일([`clock_file`]) 하나가 게스트 시계다. 내용은 공백으로 가른 수
//! 셋이다: 게스트 시계 - 호스트 시계(나노초), 바로 맞춘 횟수, 천천히 맞춘 횟수. 파일이 없으면 모두 0이다.
//! 게스트 시계를 읽으면 호스트 시계 + 첫 수다. 바로 맞추기도 천천히 맞추기도 첫 수에 곧바로 더한다(천천히 고치는 것은
//! 커널의 일이라 흉내 내지 않는다). 테스트는 [`skew`]로 게스트 시계를 틀어 놓고 [`read`]로 맞았는지 본다.
//! 쓰기는 임시 파일에 쓴 뒤 rename한다(읽는 쪽이 반쯤 쓴 것을 보지 않게).

use std::io;
use std::path::{Path, PathBuf};

/// 게스트 시계 파일: scratch 디스크 파일 이름 뒤에 `.clock`.
pub fn clock_file(scratch: &Path) -> PathBuf {
    let mut s = scratch.as_os_str().to_owned();
    s.push(".clock");
    s.into()
}

/// 게스트 시계의 상태.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct State {
    /// 게스트 시계 - 호스트 시계(나노초). 음수면 게스트가 늦다.
    pub offset: i64,
    /// 바로 맞춘 횟수.
    pub steps: u64,
    /// 천천히 맞춘 횟수.
    pub slews: u64,
}

/// 읽는다. 없으면 기본값, 깨졌으면 오류.
pub fn read(path: &Path) -> io::Result<State> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(e),
    };
    let bad = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad clock file {text:?}"),
        )
    };
    let mut f = text.split_whitespace();
    let offset = f.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let steps = f.next().map_or(Ok(0), |v| v.parse().map_err(|_| bad()))?;
    let slews = f.next().map_or(Ok(0), |v| v.parse().map_err(|_| bad()))?;
    Ok(State {
        offset,
        steps,
        slews,
    })
}

/// 쓴다.
pub fn write(path: &Path, s: State) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, format!("{} {} {}\n", s.offset, s.steps, s.slews))?;
    std::fs::rename(&tmp, path)
}

/// 게스트 시계를 `by` 나노초 옮긴다(테스트가 "틀린 시계"를 만든다). 횟수는 그대로 둔다.
pub fn skew(path: &Path, by: i64) -> io::Result<()> {
    let mut s = read(path)?;
    s.offset = s.offset.saturating_add(by);
    write(path, s)
}

/// 파일 위의 게스트 시계. fake 게스트가 `sbx_guest::clock::Clock`으로 쓴다.
#[cfg(unix)]
pub(crate) struct FileClock {
    path: PathBuf,
    base: std::time::Instant,
}

#[cfg(unix)]
impl FileClock {
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            base: std::time::Instant::now(),
        }
    }

    fn change(&self, by: i64, step: bool) -> io::Result<()> {
        let mut s = read(&self.path)?;
        s.offset = s.offset.saturating_add(by);
        if step {
            s.steps += 1;
        } else {
            s.slews += 1;
        }
        write(&self.path, s)
    }
}

#[cfg(unix)]
impl sbx_guest::clock::Clock for FileClock {
    fn read(&self) -> sbx_guest::clock::Reading {
        let mono = self.base.elapsed();
        let host = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            Err(_) => 0,
        };
        // 읽지 못하면 틀어지지 않은 것으로 본다(테스트 도구다).
        let offset = read(&self.path).map(|s| s.offset).unwrap_or(0);
        sbx_guest::clock::Reading {
            wall: host.saturating_add(offset),
            mono,
        }
    }

    fn step(&self, by: i64) -> io::Result<()> {
        self.change(by, true)
    }

    fn slew(&self, by: i64) -> io::Result<()> {
        self.change(by, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_round_trips_and_a_missing_one_is_zero() {
        let dir = std::env::temp_dir().join(format!("sbx-fake-clock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = clock_file(&dir.join("scratch.img"));
        assert!(p.ends_with("scratch.img.clock"));
        let _ = std::fs::remove_file(&p);
        assert_eq!(read(&p).unwrap(), State::default());
        skew(&p, -5).unwrap();
        skew(&p, -5).unwrap();
        assert_eq!(read(&p).unwrap().offset, -10);
        let s = State {
            offset: 7,
            steps: 2,
            slews: 3,
        };
        write(&p, s).unwrap();
        assert_eq!(read(&p).unwrap(), s);
        std::fs::write(&p, "x").unwrap();
        assert!(read(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_clock_moves_only_the_file() {
        use sbx_guest::clock::Clock;
        let dir = std::env::temp_dir().join(format!("sbx-fake-fclock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("c");
        let _ = std::fs::remove_file(&p);
        let c = FileClock::new(p.clone());
        skew(&p, -3_600_000_000_000).unwrap();
        let host = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        let behind = host - c.read().wall;
        assert!(
            (3_599_000_000_000..3_601_000_000_000).contains(&behind),
            "{behind}"
        );
        c.step(3_600_000_000_000).unwrap();
        c.slew(-4).unwrap();
        assert_eq!(
            read(&p).unwrap(),
            State {
                offset: -4,
                steps: 1,
                slews: 1
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
