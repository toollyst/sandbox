//! 이벤트 기록: 무엇이 언제 바뀌었는지를 디렉터리 하나에 JSON Lines로 남기고, 따라 읽는다.
//!
//! 데몬이 없으므로 상태를 바꾸는 프로세스(CLI, shim, MCP 서버)가 바꾸는 자리에서 직접 덧붙인다.
//! 무엇을 기록할지(종류, 동작, 속성)와 어느 디렉터리에 쓸지는 부르는 쪽이 정한다. 이 크레이트는 장치만 준다.
//!
//! ```text
//! <dir>/
//!   lock            덧붙이기와 세대 넘기기를 한 번에 하나만 (std 파일 잠금)
//!   <n>.jsonl       세대 n. 한 줄에 [`Event`] 하나. n은 1부터 늘어난다
//! ```
//!
//! **명시적 동작**
//!
//! - 덧붙이기([`Log::append`])는 잠금을 쥐고 시각을 정한 뒤 줄 하나를 한 번에 쓴다. 그래서 여러 프로세스가 동시에
//!   써도 줄이 섞이지 않고, 한 디렉터리 안의 순서는 덧붙인 순서다.
//! - 지금 세대가 [`MAX_GENERATION_BYTES`]에 닿으면 다음 세대로 넘기고, 바로 앞 세대 하나만 남기고 더 오래된 것은
//!   지운다. 그래서 디렉터리는 대략 그 두 배를 넘지 않는다. 오래된 기록이 사라지는 것은 정해진 동작이다.
//! - 읽기([`Reader::next_event`])는 지금 있는 것을 돌려주고, 더 없으면 `None`이다. 따라가려면 부르는 쪽이 다시 부른다.
//!   - 끝에 반쯤 쓰인 줄은 아직 없는 것으로 본다.
//!   - 다음 세대가 생기면 지금 세대를 끝까지 읽은 뒤 넘어간다(넘긴 뒤에는 앞 세대에 쓰지 않는다).
//!   - 한 세대 넘게 뒤처져 읽던 세대가 지워졌으면 남아 있는 가장 오래된 세대로 건너뛴다. 그 사이 것은 잃는다.
//!   - 읽을 수 없는 줄(깨졌거나 모르는 모양)은 건너뛴다.
//! - 기록은 알림이다. 덧붙이지 못해도 부르는 쪽의 동작(예: 샌드박스 멈춤)은 실패로 만들지 않는 것이 쓰임새다.
//!
//! - 의존: 외부 `serde`, `serde_json`뿐. workspace 크레이트를 모른다.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 세대 하나의 크기 상한(바이트). 닿으면 다음 세대로 넘긴다.
pub const MAX_GENERATION_BYTES: u64 = 1 << 20;

const LOCK: &str = "lock";
const EXT: &str = "jsonl";

/// 이벤트 하나. 기록 형식이다(사용자에게 보이는 모양은 표현 계층이 정한다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// 기록한 시각(유닉스 시각, 나노초). [`Log::append`]가 정한다.
    pub time_ns: u64,
    /// 대상의 종류(예: `sandbox`, `image`).
    pub kind: String,
    /// 동작(예: `start`).
    pub action: String,
    /// 대상의 ID.
    pub id: String,
    /// 덧붙인 정보.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl Event {
    /// 속성 없는 이벤트. 시각은 덧붙일 때 정한다.
    pub fn new(kind: &str, action: &str, id: impl Into<String>) -> Self {
        Self {
            time_ns: 0,
            kind: kind.to_owned(),
            action: action.to_owned(),
            id: id.into(),
            attributes: BTreeMap::new(),
        }
    }

    /// 속성 하나를 더한다.
    pub fn with(mut self, key: &str, value: impl Into<String>) -> Self {
        self.attributes.insert(key.to_owned(), value.into());
        self
    }
}

/// 기록 디렉터리 하나.
#[derive(Debug, Clone)]
pub struct Log {
    dir: PathBuf,
    max: u64,
}

/// 어디부터 읽을지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// 남아 있는 가장 오래된 것부터.
    Beginning,
    /// 지금 끝에서부터(앞으로 덧붙는 것만).
    End,
}

impl Log {
    /// `dir`의 기록. 디렉터리는 처음 덧붙일 때 만든다.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            max: MAX_GENERATION_BYTES,
        }
    }

    /// 디렉터리.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 덧붙인다. 시각(`time_ns`)은 잠금을 쥔 뒤 지금으로 정한다.
    pub fn append(&self, mut event: Event) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(LOCK))?;
        lock.lock()?;
        let gens = generations(&self.dir)?;
        let mut n = gens.last().copied().unwrap_or(1);
        let full = fs::metadata(self.path(n)).is_ok_and(|m| m.len() >= self.max);
        if full {
            n += 1;
            // 바로 앞 세대(n - 1)만 남긴다.
            for old in gens.iter().filter(|g| **g + 1 < n) {
                let _ = fs::remove_file(self.path(*old));
            }
        }
        event.time_ns = now_ns();
        let mut line = serde_json::to_vec(&event).map_err(io::Error::other)?;
        line.push(b'\n');
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(n))?
            .write_all(&line)
        // 잠금은 `lock`이 닫힐 때 풀린다.
    }

    /// 읽는 쪽을 만든다.
    pub fn reader(&self, start: Start) -> io::Result<Reader> {
        let gens = generations(&self.dir)?;
        let mut r = Reader {
            log: self.clone(),
            generation: 1,
            file: None,
            pending: Vec::new(),
            pos: 0,
        };
        match start {
            Start::Beginning => r.generation = gens.first().copied().unwrap_or(1),
            Start::End => {
                r.generation = gens.last().copied().unwrap_or(1);
                if let Ok(mut f) = File::open(self.path(r.generation)) {
                    // 지금 끝까지 건너뛴다(세대 전체를 읽지 않고 끝 근처만). 끝에 반쯤 쓰인 줄은 다 쓰이면 읽힌다.
                    let len = f.seek(SeekFrom::End(0))?;
                    let back = len.min(TAIL_WINDOW);
                    f.seek(SeekFrom::Start(len - back))?;
                    let mut tail = Vec::new();
                    f.read_to_end(&mut tail)?;
                    match tail.iter().rposition(|b| *b == b'\n') {
                        Some(i) => r.pending = tail[i + 1..].to_vec(),
                        // 창 안에 줄바꿈이 없으면 창 앞에서 시작한 아주 긴 줄의 끝이다. 그 줄은 버린다.
                        None if back < len => {}
                        None => r.pending = tail,
                    }
                    r.file = Some(f);
                }
            }
        }
        Ok(r)
    }

    fn path(&self, n: u64) -> PathBuf {
        self.dir.join(format!("{n}.{EXT}"))
    }
}

/// 따라 읽는 쪽. [`Reader::next_event`]가 `None`이면 지금은 더 없다.
#[derive(Debug)]
pub struct Reader {
    log: Log,
    generation: u64,
    file: Option<File>,
    /// 읽었지만 아직 꺼내지 않은 바이트. `pos` 앞은 이미 꺼냈다.
    pending: Vec<u8>,
    /// `pending`에서 다음 줄이 시작하는 곳. 줄마다 버퍼를 앞으로 옮기지 않으려고 둔다(그러면 O(n²)이다).
    pos: usize,
}

/// `Start::End`가 반쪽 줄을 찾으려고 끝에서 읽는 바이트. 이벤트 한 줄보다 훨씬 크다.
const TAIL_WINDOW: u64 = 64 << 10;

impl Reader {
    /// 다음 이벤트. 지금 더 없으면 `None`.
    pub fn next_event(&mut self) -> io::Result<Option<Event>> {
        loop {
            if let Some(e) = self.take_line() {
                return Ok(Some(e));
            }
            if self.file.is_none() {
                match File::open(self.log.path(self.generation)) {
                    Ok(f) => self.file = Some(f),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            if self.fill()? {
                continue;
            }
            // 지금 세대에서 더 읽을 것이 없다. 뒤 세대가 있으면 넘어간다.
            let gens = generations(&self.log.dir)?;
            let Some(&next) = gens.iter().find(|g| **g > self.generation) else {
                return Ok(None);
            };
            // 뒤 세대가 생긴 뒤로 이 세대는 자라지 않는다. 그 전에 덧붙은 것을 마저 읽는다.
            if self.fill()? {
                continue;
            }
            // 지워진 세대를 건너뛰었으면 남은 것 중 가장 오래된 것으로 간다.
            self.generation = if gens.contains(&(self.generation + 1)) {
                self.generation + 1
            } else {
                next
            };
            self.file = None;
            // 넘기기 전 세대 끝의 반쪽 줄은 끝내 완성되지 않는다.
            self.pending.clear();
            self.pos = 0;
        }
    }

    /// 열린 세대에서 있는 만큼 읽는다. 읽은 것이 있으면 `true`.
    fn fill(&mut self) -> io::Result<bool> {
        let Some(f) = &mut self.file else {
            return Ok(false);
        };
        // 꺼낸 것을 한 번에 버린다.
        self.pending.drain(..self.pos);
        self.pos = 0;
        let before = self.pending.len();
        f.read_to_end(&mut self.pending)?;
        Ok(self.pending.len() > before)
    }

    /// 완성된 줄 하나를 꺼낸다. 읽을 수 없는 줄은 건너뛴다.
    fn take_line(&mut self) -> Option<Event> {
        while let Some(i) = self.pending[self.pos..].iter().position(|b| *b == b'\n') {
            let line = &self.pending[self.pos..self.pos + i];
            self.pos += i + 1;
            if let Ok(e) = serde_json::from_slice::<Event>(line) {
                return Some(e);
            }
        }
        None
    }
}

/// 있는 세대 번호, 작은 것부터.
fn generations(dir: &Path) -> io::Result<Vec<u64>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else { continue };
        if let Some(n) = name
            .strip_suffix(&format!(".{EXT}"))
            .and_then(|n| n.parse::<u64>().ok())
        {
            out.push(n);
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
