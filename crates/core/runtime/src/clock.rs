//! 게스트 시계 맞추기 (호스트 쪽). VM을 소유한 프로세스(attached면 `sbx`, detached면 `sbx-shim`)가 VM마다 스레드
//! 하나로 돌린다. 재고 맞추는 규칙은 `sbx-guest`의 `clock` 모듈에 있다. 실행기에는 아무것도 더 바라지 않는다.
//!
//! **언제 보내나** ([`run`])
//! - 게스트가 준비된 직후 한 번. 부팅한 시계도 틀릴 수 있다. 이 스레드가 따로 보내므로 시작은 기다리지 않는다
//!   (기본 프로세스는 처음 몇 ms 동안 부팅한 시계를 볼 수 있다).
//! - 그 뒤 60초([`PERIOD`])마다. 잠자지 않을 때의 어긋남은 4ppm(하루 0.35초)쯤이라 60초면 0.24ms다. 그래서 주기 맞추기는
//!   늘 천천히 맞추기(slew)이고, 하루 어긋남은 잰 오차(보통 0.05ms) 수준에 머문다. 비용은 1분에 vsock 연결 하나다.
//! - 호스트 벽시계가 뛰었을 때 곧바로([`jumped`]). 잠자기에서 깬 뒤가 이것이다.
//! - 실패하면 1초([`FIRST_RETRY`])부터 두 배씩 [`PERIOD`]까지 늘려 다시 해 본다.
//!
//! **뜀 알아채기** ([`jumped`]): 스레드는 0.5초([`TICK`])씩 기다린다. 기다리기 앞뒤로 벽시계(`SystemTime`)와 단조
//! 시계(`Instant`)를 함께 읽어 둘 중 하나면 곧바로 맞춘다.
//! 1. 벽시계가 간 시간과 단조 시계가 간 시간이 100ms([`JUMP`])보다 더 다르다. 단조 시계는 잠자는 동안 서고 벽시계는
//!    가는 OS에서의 잠자기(macOS는 `Instant`가 `CLOCK_UPTIME_RAW`, Linux는 `CLOCK_MONOTONIC`), 그리고 어느 OS에서나
//!    호스트 시계를 바꾼 것(NTP의 step, 사용자가 바꿈)이 이것으로 잡힌다. 호스트 NTP의 천천히 맞추기(많아야 500ppm,
//!    0.5초에 0.25ms)는 잡히지 않는다.
//! 2. 단조 시계로 보아도 [`TICK`]보다 1초([`LATE`]) 넘게 늦게 깼다. 그동안 이 프로세스가 돌지 못했다. 단조 시계가 잠자는
//!    동안에도 가는 경우(Windows의 `Instant`는 QueryPerformanceCounter다)의 잠자기, 프로세스를 멈췄다 푼 것(SIGSTOP)이
//!    이것으로 잡힌다. 바쁜 호스트에서 잘못 잡으면 맞추기를 한 번 더 할 뿐이다.
//!
//! 두 값은 기다림 하나의 앞뒤에서만 재므로 맞추기에 걸린 시간은 들지 않는다. 깬 뒤 많아야 [`TICK`] 안에 알아채고 곧바로
//! 맞춘다. 기다림은 스레드 하나의 타이머라 0.5초에 한 번 깨어 시계 두 개를 읽는 것이 전부다.
//!
//! **믿지 않음**: 게스트가 하는 일은 호스트 시각을 묻는 것뿐이다. 호스트는 연결 하나에 [`CLOCK_ROUNDS`](sbx_guest_proto::CLOCK_ROUNDS)번까지만 답하고,
//! 그 밖에는 끝(`Done`)인지 오류인지만 본다. 게스트가 보낸 어떤 값도 호스트의 시계나 상태, 맞추는 때를 바꾸지 않는다.
//! 기록에 넣는 게스트의 오류 문장은 [`MAX_NOTE`] 글자로 자른다.
//!
//! **기록**: 실패는 샌드박스의 `logs/executor.log`(실행 코어가 실행기에 넘기는 로그. 두 소유 프로세스에 다 있다)에
//! `sbx: guest clock: …` 한 줄로, 종류마다 이번 실행에 한 번만 쓴다([`Notes`]). 기본 프로세스와 `sbx`를 쓰는 사람은
//! 아무것도 보지 못한다. 메시지를 모르는 게스트(답 없이 닫거나 `Protocol` 오류)이거나 시계를 맞출 수 없는 게스트
//! (`InvalidArgument`)면 그 줄을 남기고 이 VM에서는 그만둔다(`sbx`를 올리기 전의 게스트 init을 쓴 경우다).
//!
//! **수명**: `start`가 돌려준 [`Running`]을 버리면(소유 기록이 사라지면) 스레드가 다음 [`TICK`] 안에 끝난다.
//! 다른 프로세스(`exec`, `cp`)는 시계를 맞추지 않는다.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use sbx_guest_proto::ErrorKind;

use crate::client::Guest;
use crate::error::RuntimeError;

/// 기다리는 한 번. 깬 뒤 이 안에 알아챈다.
pub const TICK: Duration = Duration::from_millis(500);
/// 주기 맞추기의 간격.
pub const PERIOD: Duration = Duration::from_secs(60);
/// 기다림 하나에서 벽시계와 단조 시계가 이보다 더 다르게 가면 뛰었다고 본다.
pub const JUMP: Duration = Duration::from_millis(100);
/// 기다림이 단조 시계로 이보다 더 늦게 끝나면 뛰었다고 본다.
pub const LATE: Duration = Duration::from_secs(1);
/// 실패한 뒤 처음 다시 해 보는 때. 두 배씩 [`PERIOD`]까지 는다.
pub const FIRST_RETRY: Duration = Duration::from_secs(1);
/// 기록 한 줄에 넣는 오류 문장의 최대 글자 수.
pub const MAX_NOTE: usize = 300;

/// 호스트의 두 시계를 함께 읽은 것.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// 벽시계.
    pub wall: SystemTime,
    /// 단조 시계.
    pub mono: Instant,
}

impl Reading {
    /// 지금.
    pub fn now() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }
}

/// 왜 뛰었다고 보았나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Jump {
    /// 벽시계가 단조 시계보다 이만큼(나노초) 더 갔다(음수면 덜 갔다).
    Wall(i128),
    /// 단조 시계로 이만큼 걸렸다(기다린 것보다 [`LATE`] 넘게 길다).
    Late(Duration),
}

/// `waited`를 기다리기 전(`before`)과 뒤(`after`)에 읽은 값으로 호스트 시계가 뛰었는지 본다. 규칙은 모듈 문서에 있다.
pub fn jumped(before: Reading, after: Reading, waited: Duration) -> Option<Jump> {
    let mono = after.mono.saturating_duration_since(before.mono);
    let wall = match after.wall.duration_since(before.wall) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    };
    let skew = wall - mono.as_nanos() as i128;
    if skew.unsigned_abs() > JUMP.as_nanos() {
        Some(Jump::Wall(skew))
    } else if mono > waited + LATE {
        Some(Jump::Late(mono))
    } else {
        None
    }
}

/// 벽시계를 Unix 나노초로. 1970년 앞이면 음수, 넘치면 끝에 붙인다.
pub fn unix_nanos(t: SystemTime) -> i64 {
    match t.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
        Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}

/// 기다리는 쪽. 실제로는 `Instant`, `SystemTime`과 멈춤 채널이고, 테스트는 가짜 타이머를 준다.
pub trait Ticker {
    /// 지금.
    fn now(&mut self) -> Reading;
    /// `d`만큼 기다린다. 그만두라는 뜻이면 거짓.
    fn wait(&mut self, d: Duration) -> bool;
}

/// 실패의 종류(기록은 종류마다 한 번).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 이 메시지를 모르거나 시계를 맞출 수 없는 게스트. 그만둔다.
    Unsupported,
    /// 연결하지 못함.
    Unreachable,
    /// 연결이 끊기거나 답이 없음.
    Lost,
    /// 게스트가 규약에 맞지 않는 것을 보냄.
    Protocol,
    /// 게스트가 재지 못했거나 맞추지 못했다고 알림.
    Guest(ErrorKind),
    /// 그 밖.
    Other,
}

/// 오류의 종류.
pub fn kind(e: &RuntimeError) -> Kind {
    match e {
        RuntimeError::Guest(g)
            if matches!(g.kind, ErrorKind::Protocol | ErrorKind::InvalidArgument) =>
        {
            Kind::Unsupported
        }
        RuntimeError::Guest(g) => Kind::Guest(g.kind),
        RuntimeError::GuestUnreachable(_) => Kind::Unreachable,
        RuntimeError::Lost(_) => Kind::Lost,
        RuntimeError::Protocol(_) => Kind::Protocol,
        _ => Kind::Other,
    }
}

/// 기록 하나가 무엇에 대한 것인지: 줄의 앞머리, 종류를 정하는 법, 메시지를 모르는 게스트일 때 덧붙이는 말.
/// 시계 맞추기([`Topic::CLOCK`])와 공유 폴더 변경 알림(`watch` 모듈)이 같은 기록 방식을 쓴다.
#[derive(Clone, Copy)]
pub struct Topic {
    /// 줄의 앞머리(예: `sbx: guest clock: `).
    pub prefix: &'static str,
    /// 오류의 종류.
    pub kind: fn(&RuntimeError) -> Kind,
    /// [`Kind::Unsupported`]일 때 덧붙이는 말.
    pub unsupported: &'static str,
}

impl Topic {
    /// 시계 맞추기.
    pub const CLOCK: Topic = Topic {
        prefix: "sbx: guest clock: ",
        kind,
        unsupported: "; this guest cannot keep its clock in step with the host \
                      (restart the sandbox after upgrading sbx)",
    };
}

/// 종류마다 한 번만 쓰는 기록.
pub struct Notes {
    topic: Topic,
    seen: Vec<Kind>,
    sink: Box<dyn FnMut(&str) + Send>,
}

impl Notes {
    /// `sink`가 한 줄씩 받는다(시계 맞추기의 기록).
    pub fn new(sink: Box<dyn FnMut(&str) + Send>) -> Self {
        Self::about(Topic::CLOCK, sink)
    }

    /// `topic`의 기록. `sink`가 한 줄씩 받는다.
    pub fn about(topic: Topic, sink: Box<dyn FnMut(&str) + Send>) -> Self {
        Self {
            topic,
            seen: Vec::new(),
            sink,
        }
    }

    /// 파일 끝에 덧붙인다(실행기도 같은 파일에 덧붙여 쓴다). 쓰지 못해도 그만이다.
    pub fn append_to(path: PathBuf) -> Self {
        Self::about_in(Topic::CLOCK, path)
    }

    /// `topic`의 기록을 파일 끝에 덧붙인다([`Notes::append_to`]처럼).
    pub fn about_in(topic: Topic, path: PathBuf) -> Self {
        Self::about(
            topic,
            Box::new(move |line| {
                if let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                {
                    let _ = f.write_all(format!("{line}\n").as_bytes());
                }
            }),
        )
    }

    /// `e`를 그 종류로 처음이면 쓴다.
    pub fn once(&mut self, e: &RuntimeError) {
        let k = (self.topic.kind)(e);
        if self.seen.contains(&k) {
            return;
        }
        self.seen.push(k);
        let mut msg = clean(&e.to_string());
        if k == Kind::Unsupported {
            msg.push_str(self.topic.unsupported);
        }
        (self.sink)(&format!("{}{msg}", self.topic.prefix));
    }

    /// 한 줄을 쓴다(종류를 세지 않는다). 제어 문자는 공백으로 바꾸고 [`MAX_NOTE`] 글자로 자른다.
    pub fn line(&mut self, msg: &str) {
        (self.sink)(&format!("{}{}", self.topic.prefix, clean(msg)));
    }
}

/// 제어 문자를 공백으로 바꾸고 [`MAX_NOTE`] 글자로 자른다(게스트가 보낸 문장도 들어간다).
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_NOTE)
        .collect()
}

/// 맞추기를 돈다: 곧바로 한 번, [`PERIOD`]마다, 뛰면 곧바로, 실패하면 늘려 가며 다시. `ticker.wait`가 거짓이거나
/// 게스트가 맞추기를 모르면 끝난다.
pub fn run(
    ticker: &mut dyn Ticker,
    sync: &mut dyn FnMut() -> Result<(), RuntimeError>,
    notes: &mut Notes,
) {
    let mut next = ticker.now().mono;
    let mut retry = FIRST_RETRY;
    loop {
        if ticker.now().mono >= next {
            match sync() {
                Ok(()) => {
                    retry = FIRST_RETRY;
                    next = ticker.now().mono + PERIOD;
                }
                Err(e) => {
                    notes.once(&e);
                    if kind(&e) == Kind::Unsupported {
                        return;
                    }
                    next = ticker.now().mono + retry;
                    retry = (retry * 2).min(PERIOD);
                }
            }
        }
        let before = ticker.now();
        if !ticker.wait(TICK) {
            return;
        }
        let after = ticker.now();
        if jumped(before, after, TICK).is_some() {
            next = after.mono;
        }
    }
}

/// 맞추기가 돌고 있다. 버리면 멈춘다.
pub struct Running {
    _stop: mpsc::Sender<()>,
}

struct Real(mpsc::Receiver<()>);

impl Ticker for Real {
    fn now(&mut self) -> Reading {
        Reading::now()
    }

    fn wait(&mut self, d: Duration) -> bool {
        matches!(self.0.recv_timeout(d), Err(mpsc::RecvTimeoutError::Timeout))
    }
}

/// 게스트 하나의 맞추기를 스레드로 띄운다. 실패는 `log`에 쓴다. 스레드를 띄우지 못하면 맞추기 없이 간다.
pub(crate) fn start(guest: Guest, log: PathBuf) -> Running {
    let (stop, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("sbx-clock".into())
        .spawn(move || {
            let mut ticker = Real(rx);
            let mut notes = Notes::append_to(log);
            run(
                &mut ticker,
                &mut || guest.clock_sync(&|| unix_nanos(SystemTime::now())),
                &mut notes,
            );
        });
    Running { _stop: stop }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use sbx_guest_proto::Error;

    use super::*;

    fn at(wall_ms: u64, mono_ms: u64, base: Instant) -> Reading {
        Reading {
            wall: SystemTime::UNIX_EPOCH
                + Duration::from_secs(1_700_000_000)
                + Duration::from_millis(wall_ms),
            mono: base + Duration::from_millis(mono_ms),
        }
    }

    #[test]
    fn a_wall_clock_that_runs_ahead_of_the_monotonic_one_is_a_jump() {
        let b = Instant::now();
        let w = TICK;
        // 제때 깼다.
        assert_eq!(jumped(at(0, 0, b), at(500, 500, b), w), None);
        // 호스트 NTP가 천천히 맞추는 정도(수 ms)는 아니다. 100ms를 넘어야 한다.
        assert_eq!(jumped(at(0, 0, b), at(550, 500, b), w), None);
        assert_eq!(jumped(at(0, 0, b), at(600, 500, b), w), None);
        assert_eq!(
            jumped(at(0, 0, b), at(601, 500, b), w),
            Some(Jump::Wall(101_000_000))
        );
        // macOS·Linux의 잠자기: 단조 시계는 서고 벽시계는 한 시간 갔다.
        assert_eq!(
            jumped(at(0, 0, b), at(3_600_500, 500, b), w),
            Some(Jump::Wall(3_600_000_000_000))
        );
        // 호스트 시계를 뒤로 돌렸다.
        assert_eq!(
            jumped(at(10_000, 0, b), at(5_000, 500, b), w),
            Some(Jump::Wall(-5_500_000_000))
        );
    }

    #[test]
    fn a_timer_that_fires_late_is_a_jump() {
        let b = Instant::now();
        // Windows의 잠자기나 SIGSTOP: 두 시계가 함께 갔지만 0.5초를 기다렸는데 10초 걸렸다.
        assert_eq!(
            jumped(at(0, 0, b), at(10_000, 10_000, b), TICK),
            Some(Jump::Late(Duration::from_secs(10)))
        );
        // 1초 넘게 늦어야 한다.
        assert_eq!(jumped(at(0, 0, b), at(1_500, 1_500, b), TICK), None);
        assert!(jumped(at(0, 0, b), at(1_501, 1_501, b), TICK).is_some());
        // 단조 시계가 거꾸로 읽혀도 패닉하지 않는다.
        assert_eq!(jumped(at(0, 500, b), at(0, 0, b), TICK), None);
    }

    #[test]
    fn unix_nanos_handles_both_sides_of_1970() {
        assert_eq!(unix_nanos(SystemTime::UNIX_EPOCH), 0);
        assert_eq!(
            unix_nanos(SystemTime::UNIX_EPOCH + Duration::from_nanos(5)),
            5
        );
        assert_eq!(
            unix_nanos(SystemTime::UNIX_EPOCH - Duration::from_secs(1)),
            -1_000_000_000
        );
    }

    /// 가짜 타이머: 기다릴 때마다 각본의 (벽시계, 단조 시계) 걸음만큼 간다. 각본이 없으면 [`TICK`]씩 함께 가고,
    /// `until`(단조 시계)을 넘으면 그만둔다.
    struct Fake {
        now: Reading,
        start: Instant,
        script: Vec<(Duration, Duration)>,
        until: Duration,
    }

    impl Fake {
        fn new(until_secs: u64) -> Self {
            let start = Instant::now();
            Self {
                now: Reading {
                    wall: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                    mono: start,
                },
                start,
                script: Vec::new(),
                until: Duration::from_secs(until_secs),
            }
        }

        /// 지금까지 단조 시계로 간 시간.
        fn elapsed(&self) -> Duration {
            self.now.mono - self.start
        }
    }

    impl Ticker for Fake {
        fn now(&mut self) -> Reading {
            self.now
        }

        fn wait(&mut self, d: Duration) -> bool {
            let (wall, mono) = if self.script.is_empty() {
                (d, d)
            } else {
                self.script.remove(0)
            };
            self.now.wall += wall;
            self.now.mono += mono;
            self.elapsed() <= self.until
        }
    }

    /// `ticker`로 `run`을 돌리고 맞추기를 부른 때(초, 단조 시계)와 기록을 돌려준다. `answer`가 맞추기의 결과를 정한다.
    fn drive(
        fake: Fake,
        answer: impl Fn(usize) -> Result<(), RuntimeError>,
    ) -> (Vec<f64>, Vec<String>) {
        let fake = Arc::new(Mutex::new(fake));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let lines = Arc::new(Mutex::new(Vec::new()));
        let mut notes = {
            let lines = lines.clone();
            Notes::new(Box::new(move |l| lines.lock().unwrap().push(l.to_owned())))
        };
        struct Shared(Arc<Mutex<Fake>>);
        impl Ticker for Shared {
            fn now(&mut self) -> Reading {
                self.0.lock().unwrap().now()
            }
            fn wait(&mut self, d: Duration) -> bool {
                self.0.lock().unwrap().wait(d)
            }
        }
        let mut ticker = Shared(fake.clone());
        let mut sync = || {
            let t = fake.lock().unwrap().elapsed().as_secs_f64();
            let mut c = calls.lock().unwrap();
            c.push(t);
            answer(c.len() - 1)
        };
        run(&mut ticker, &mut sync, &mut notes);
        let calls = calls.lock().unwrap().clone();
        let lines = lines.lock().unwrap().clone();
        (calls, lines)
    }

    #[test]
    fn it_syncs_at_once_then_every_period() {
        let (calls, lines) = drive(Fake::new(130), |_| Ok(()));
        assert_eq!(calls, vec![0.0, 60.0, 120.0]);
        assert!(lines.is_empty());
    }

    #[test]
    fn a_host_sleep_or_clock_change_syncs_at_once() {
        let mut f = Fake::new(30);
        // 10초 뒤 잠자기(macOS·Linux): 기다림 하나 동안 벽시계만 한 시간 간다.
        f.script = vec![(TICK, TICK); 20];
        f.script.push((Duration::from_secs(3600) + TICK, TICK));
        // 다시 10초 뒤 Windows의 잠자기나 SIGSTOP: 둘 다 5초 간다.
        f.script.extend(vec![(TICK, TICK); 20]);
        f.script
            .push((Duration::from_secs(5), Duration::from_secs(5)));
        // 호스트 NTP가 조금씩 고치는 것은 뜀이 아니다.
        f.script.push((TICK + Duration::from_millis(1), TICK));
        let (calls, _) = drive(f, |_| Ok(()));
        assert_eq!(calls, vec![0.0, 10.5, 25.5]);
    }

    #[test]
    fn failures_are_retried_sooner_and_noted_once_per_kind() {
        let (calls, lines) = drive(Fake::new(200), |i| match i {
            0..=2 => Err(RuntimeError::Lost("the guest did not answer".into())),
            3 => Err(RuntimeError::Guest(Error::new(
                ErrorKind::Io,
                "round trip too slow",
            ))),
            _ => Ok(()),
        });
        // 1, 2, 4, 8초 뒤에 다시. 맞춘 뒤로는 60초마다.
        assert_eq!(calls, vec![0.0, 1.0, 3.0, 7.0, 15.0, 75.0, 135.0, 195.0]);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("sbx: guest clock: "), "{lines:?}");
        assert!(lines[0].contains("did not answer"), "{lines:?}");
        assert!(lines[1].contains("round trip too slow"), "{lines:?}");
    }

    #[test]
    fn retries_back_off_to_the_period() {
        let (calls, lines) = drive(Fake::new(400), |_| {
            Err(RuntimeError::GuestUnreachable("gone".into()))
        });
        assert_eq!(
            calls,
            vec![
                0.0, 1.0, 3.0, 7.0, 15.0, 31.0, 63.0, 123.0, 183.0, 243.0, 303.0, 363.0
            ]
        );
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn an_older_guest_stops_the_sync_quietly() {
        for e in [
            RuntimeError::Guest(Error::new(ErrorKind::Protocol, "unknown host message 0x20")),
            RuntimeError::Guest(Error::new(
                ErrorKind::InvalidArgument,
                "this guest cannot set its clock",
            )),
        ] {
            let e = std::cell::RefCell::new(Some(e));
            let (calls, lines) = drive(Fake::new(600), |_| Err(e.borrow_mut().take().unwrap()));
            assert_eq!(calls, vec![0.0]);
            assert_eq!(lines.len(), 1);
            assert!(lines[0].contains("restart the sandbox"), "{lines:?}");
        }
    }

    #[test]
    fn notes_are_one_line_and_short() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let mut notes = {
            let lines = lines.clone();
            Notes::new(Box::new(move |l: &str| {
                lines.lock().unwrap().push(l.to_owned())
            }))
        };
        let long = format!("a\nb{}", "x".repeat(10_000));
        notes.once(&RuntimeError::Protocol(long));
        notes.once(&RuntimeError::Protocol("again".into()));
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert!(!lines[0].contains('\n'));
        assert!(
            lines[0].chars().count() <= MAX_NOTE + 40,
            "{}",
            lines[0].len()
        );
    }

    #[test]
    fn dropping_the_handle_ends_the_real_ticker() {
        let (stop, rx) = mpsc::channel::<()>();
        let mut t = Real(rx);
        assert!(t.wait(Duration::from_millis(1)));
        drop(stop);
        let started = Instant::now();
        assert!(!t.wait(Duration::from_secs(10)));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
