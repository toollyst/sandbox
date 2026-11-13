//! 시계 맞추기 (게스트 쪽). [`HostMsg::ClockSync`] 하나를 처리한다. 언제 보내는지는 호스트(`sbx-runtime`의 `clock`)가 정한다.
//!
//! 이미지에 chrony가 없어도, 실행기에 `ptp_kvm` 같은 기능이 없어도 되게 게스트 init이 직접 맞춘다.
//! 호스트가 잠자는 동안 게스트 시계는 멈추고(깬 뒤 그만큼 늦다), 잠자지 않아도 조금씩 어긋난다(하루 0.35초쯤).
//!
//! **재기** (Cristian 방식. 오차는 왕복 시간의 절반을 넘지 않는다)
//! 1. 게스트가 단조 시계를 읽고(`m1`) [`GuestMsg::ClockAsk`]를 보낸다.
//! 2. 호스트가 벽시계 `H`를 읽어 곧바로 [`HostMsg::ClockNow`]로 답한다.
//! 3. 게스트가 받자마자 단조 시계(`m2`)와 벽시계(`g`)를 함께 읽는다.
//! 4. `H`는 `m1`과 `m2` 사이에 읽혔다(묻기 전에는 답할 수 없다). 그래서 `m2`에서 호스트 시각은 `[H, H + rtt]` 안이고
//!    (`rtt = m2 - m1`), 가운데 `H + rtt/2`로 어림하면 오차는 `rtt/2` 이하다. 차이(`offset`)는 `H + rtt/2 - g`다.
//! 5. [`CLOCK_ROUNDS`]번 재서 `rtt`가 가장 짧은 것을 쓴다(오차 한도가 가장 작다). 그것도 [`MAX_RTT`]를 넘으면
//!    맞추지 않고 오류로 답한다(재기를 믿을 수 없다). vsock 왕복은 0.1ms쯤이라 보통 오차는 0.05ms 아래다.
//!
//! 차이는 상대값이므로 잰 뒤 맞출 때까지 걸린 시간은 오차에 들지 않는다(커널이 "지금 시각에 더하기"로 맞춘다).
//!
//! **맞추기** ([`decide`])
//! - `|offset| >= STEP`([`STEP`], 128ms): 바로 맞춘다(step). 진행 중인 천천히 맞추기는 먼저 취소한다(지금 잰 차이가
//!   이미 그 몫을 담고 있다). 잠자기에서 깬 뒤처럼 큰 차이를 곧바로 없앤다.
//! - 그 아래: 천천히 맞춘다(slew, `adjtime` 방식). 커널은 1초에 0.5ms(500ppm)까지만 고치므로 시계는 거꾸로 가지 않고
//!   멈추지도 않는다. 128ms를 고치는 데 256초 걸린다. 새로 맞추면 남은 몫을 지금 잰 차이로 바꾼다.
//! - 128ms는 NTP(RFC 5905)가 step과 slew를 가르는 값(STEPT)이다. 재기 오차 한도(`MAX_RTT/2` = 10ms)보다 훨씬 커서
//!   잡음으로 step하지 않고, 하루 어긋남(0.35초)보다 훨씬 작아 주기 맞추기(호스트가 1분마다)는 늘 slew다.
//! - 뒤로 크게 맞추기도 한다. 그런 차이는 호스트 시계가 뒤로 갔거나(사용자, NTP) 게스트 안에서 누가 시계를 앞으로
//!   돌렸을 때만 생긴다. 게스트가 호스트와 같은 시각을 보는 것(TLS, 토큰, 로그를 호스트 로그와 맞대기)이 목적이므로
//!   호스트를 따른다. NTP가 step할 때와 같다. `CLOCK_MONOTONIC`은 바뀌지 않는다.
//! - 게스트 안에서 `date -s`로 바꾼 시계는 다음 맞추기에서 되돌아간다. 게스트 안의 chrony·ntpd가 있으면 둘 다 같은
//!   시각을 향해 고친다.
//!
//! **실패**: 시계를 바꾸지 못하면(권한 없음) 오류로 답한다. 호스트는 종류마다 한 번만 기록하고 다음에 다시 해 본다.
//! 사용자 프로세스는 이 일을 보지 못한다.

use std::io::{self, Read};
use std::time::Duration;

use sbx_guest_proto::{CLOCK_ROUNDS, Error, ErrorKind, GuestMsg, HostMsg};

use crate::wire::{Out, protocol};

/// 이보다 크게 어긋나면 바로 맞추고(step), 작으면 천천히 맞춘다(slew). 나노초.
pub const STEP: i64 = 128_000_000;

/// 가장 짧은 왕복이 이보다 길면 맞추지 않는다. 오차 한도는 이것의 절반(10ms)이다.
pub const MAX_RTT: Duration = Duration::from_millis(20);

/// 시계 하나를 같이 읽은 값.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// 벽시계: Unix 시각(나노초).
    pub wall: i64,
    /// 단조 시계: 아무 기준에서 잰 시간. 왕복을 재는 데만 쓴다.
    pub mono: Duration,
}

/// 게스트 시계. 게스트에서는 커널 시계([`Kernel`])이고, 테스트와 fake 실행기는 호스트 시계를 건드리지 않는 것을 준다.
pub trait Clock: Send + Sync {
    /// 벽시계와 단조 시계를 함께 읽는다.
    fn read(&self) -> Reading;
    /// 벽시계를 `by` 나노초 바로 옮긴다. 진행 중인 천천히 맞추기는 취소한다.
    fn step(&self, by: i64) -> io::Result<()>;
    /// 벽시계를 `by` 나노초만큼 천천히 옮긴다. 남은 몫이 있으면 이것으로 바꾼다.
    fn slew(&self, by: i64) -> io::Result<()>;
}

/// 한 번 잰 것.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// 호스트 시각 - 게스트 시각(나노초). 양수면 게스트가 늦다.
    pub offset: i64,
    /// 왕복 시간. 오차 한도는 이것의 절반이다.
    pub rtt: Duration,
}

/// 묻기 전(`before`)과 답을 받은 뒤(`after`)에 읽은 값과 호스트 시각(`host`)으로 차이를 어림한다. 넘치면 끝에 붙인다.
pub fn estimate(before: Reading, host: i64, after: Reading) -> Sample {
    let rtt = after.mono.saturating_sub(before.mono);
    let half = i128::try_from(rtt.as_nanos() / 2).unwrap_or(i128::MAX);
    let offset = i128::from(host) + half - i128::from(after.wall);
    Sample {
        offset: offset.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64,
        rtt,
    }
}

/// 할 일.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 바로 옮긴다(나노초).
    Step(i64),
    /// 천천히 옮긴다(나노초).
    Slew(i64),
}

/// 가장 좋은 재기(`rtt`가 가장 짧은 것)로 할 일을 정한다. 믿을 수 없으면(재지 못함, 너무 느림) 이유.
pub fn decide(samples: &[Sample]) -> Result<Action, String> {
    let best = samples
        .iter()
        .min_by_key(|s| s.rtt)
        .ok_or_else(|| "no clock sample".to_owned())?;
    if best.rtt > MAX_RTT {
        return Err(format!(
            "the round trip to the host took {}ms (more than {}ms): the clock is left as it is",
            best.rtt.as_millis(),
            MAX_RTT.as_millis()
        ));
    }
    Ok(if best.offset.unsigned_abs() >= STEP.unsigned_abs() {
        Action::Step(best.offset)
    } else {
        Action::Slew(best.offset)
    })
}

/// 나노초 차이를 커널의 `(초, 나노초)`로 나눈다. 나노초는 언제나 `0..1e9`다(음수면 초가 내려간다).
pub fn split(by: i64) -> (i64, i64) {
    (by.div_euclid(1_000_000_000), by.rem_euclid(1_000_000_000))
}

/// [`HostMsg::ClockSync`] 하나를 처리한다. Hello 뒤 요청을 읽은 다음 부른다.
pub fn sync<R: Read>(clock: &dyn Clock, input: &mut R, out: &Out) {
    let mut samples = Vec::with_capacity(CLOCK_ROUNDS as usize);
    for _ in 0..CLOCK_ROUNDS {
        let before = clock.read();
        if out.send(&GuestMsg::ClockAsk).is_err() {
            return;
        }
        let host = match HostMsg::read_from(input) {
            Ok(HostMsg::ClockNow { unix_nanos }) => unix_nanos,
            Ok(other) => return out.error(protocol(&other, "ClockNow")),
            Err(_) => return,
        };
        samples.push(estimate(before, host, clock.read()));
    }
    let applied = match decide(&samples) {
        Ok(Action::Step(by)) => clock.step(by),
        Ok(Action::Slew(by)) => clock.slew(by),
        Err(why) => return out.error(Error::new(ErrorKind::Io, why)),
    };
    match applied {
        Ok(()) => {
            let _ = out.send(&GuestMsg::Done);
        }
        Err(e) => out.error(crate::wire::io_error("set the clock", &e)),
    }
}

/// 게스트 커널의 시계: 벽시계는 `CLOCK_REALTIME`, 단조 시계는 `CLOCK_MONOTONIC_RAW`(천천히 맞추기는
/// `CLOCK_MONOTONIC`의 빠르기도 바꾸므로 그것과 견주면 보이지 않는다). 바로 맞추면 콘솔에 한 줄 쓴다(실행기 로그에 남는다).
#[cfg(target_os = "linux")]
#[derive(Default)]
pub struct Kernel;

#[cfg(target_os = "linux")]
impl Clock for Kernel {
    fn read(&self) -> Reading {
        let mono = crate::sys::linux::monotonic_raw();
        let wall = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_nanos()).unwrap_or(i64::MAX),
            Err(e) => i64::try_from(e.duration().as_nanos()).map_or(i64::MIN, |n| -n),
        };
        Reading { wall, mono }
    }

    fn step(&self, by: i64) -> io::Result<()> {
        let (secs, nanos) = split(by);
        crate::sys::linux::clock_slew_micros(0)?;
        crate::sys::linux::clock_step(secs, nanos)?;
        eprintln!(
            "sbx-guest: clock stepped by {:+.3}s to match the host",
            by as f64 / 1e9
        );
        Ok(())
    }

    fn slew(&self, by: i64) -> io::Result<()> {
        crate::sys::linux::clock_slew_micros(by / 1_000)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn at(wall: i64, mono_ms: u64) -> Reading {
        Reading {
            wall,
            mono: Duration::from_millis(mono_ms),
        }
    }

    #[test]
    fn the_estimate_takes_the_middle_of_the_round_trip() {
        // 게스트가 10ms에 묻고 12ms에 받았다. 호스트는 1000ms라고 답했다. 받을 때 게스트 벽시계는 400ms.
        let s = estimate(at(0, 10), 1_000_000_000, at(400_000_000, 12));
        assert_eq!(s.rtt, Duration::from_millis(2));
        assert_eq!(s.offset, 1_000_000_000 + 1_000_000 - 400_000_000);
        // 게스트가 앞서면 음수다.
        let s = estimate(at(0, 0), 5, at(1_000_000_005, 0));
        assert_eq!(s.offset, -1_000_000_000);
        // 단조 시계가 거꾸로 읽혀도 왕복은 0이다.
        assert_eq!(estimate(at(0, 5), 0, at(0, 3)).rtt, Duration::ZERO);
    }

    #[test]
    fn the_error_is_within_half_the_round_trip() {
        // 호스트가 왕복 안 어느 때 읽었든, 참 차이와 어림의 차이는 rtt/2 이하다.
        let true_offset: i64 = 3_000_000_000;
        let rtt_ms = 8u64;
        for host_read_at_ms in 0..=rtt_ms {
            let before = at(1_000_000_000, 100);
            let host = before.wall + true_offset + host_read_at_ms as i64 * 1_000_000;
            let after = at(before.wall + rtt_ms as i64 * 1_000_000, 100 + rtt_ms);
            let s = estimate(before, host, after);
            assert!(
                (s.offset - true_offset).unsigned_abs() <= rtt_ms * 1_000_000 / 2,
                "{s:?} at {host_read_at_ms}ms"
            );
        }
    }

    #[test]
    fn far_apart_clocks_do_not_overflow() {
        let s = estimate(at(i64::MIN, 0), i64::MAX, at(i64::MIN, 0));
        assert_eq!(s.offset, i64::MAX);
        let s = estimate(at(i64::MAX, 0), i64::MIN, at(i64::MAX, 0));
        assert_eq!(s.offset, i64::MIN);
        assert!(matches!(decide(&[s]), Ok(Action::Step(i64::MIN))));
    }

    fn sample(offset_ms: i64, rtt_us: u64) -> Sample {
        Sample {
            offset: offset_ms * 1_000_000,
            rtt: Duration::from_micros(rtt_us),
        }
    }

    #[test]
    fn large_offsets_step_and_small_ones_slew() {
        assert_eq!(
            decide(&[sample(3_600_000, 100)]),
            Ok(Action::Step(3_600_000_000_000))
        );
        assert_eq!(decide(&[sample(128, 100)]), Ok(Action::Step(128_000_000)));
        assert_eq!(decide(&[sample(-128, 100)]), Ok(Action::Step(-128_000_000)));
        assert_eq!(decide(&[sample(127, 100)]), Ok(Action::Slew(127_000_000)));
        assert_eq!(decide(&[sample(-5, 100)]), Ok(Action::Slew(-5_000_000)));
        assert_eq!(decide(&[sample(0, 100)]), Ok(Action::Slew(0)));
    }

    #[test]
    fn the_shortest_round_trip_wins_and_slow_ones_are_refused() {
        // 느린 재기는 차이가 커 보여도 쓰지 않는다.
        let samples = [sample(500, 9_000), sample(2, 80), sample(300, 15_000)];
        assert_eq!(decide(&samples), Ok(Action::Slew(2_000_000)));
        let e = decide(&[sample(1, 25_000), sample(1, 21_000)]).unwrap_err();
        assert!(e.contains("21ms"), "{e}");
        assert!(decide(&[]).is_err());
        assert_eq!(decide(&[sample(1, 20_000)]), Ok(Action::Slew(1_000_000)));
    }

    #[test]
    fn negative_offsets_split_into_whole_seconds_and_positive_nanoseconds() {
        assert_eq!(split(0), (0, 0));
        assert_eq!(split(1_500_000_000), (1, 500_000_000));
        assert_eq!(split(-1_500_000_000), (-2, 500_000_000));
        assert_eq!(split(-1), (-1, 999_999_999));
        assert_eq!(split(-1_000_000_000), (-1, 0));
        let (s, n) = split(i64::MIN);
        assert!((0..1_000_000_000).contains(&n));
        assert_eq!(
            i128::from(s) * 1_000_000_000 + i128::from(n),
            i128::from(i64::MIN)
        );
    }

    /// 손으로 움직이는 시계. 읽을 때마다 단조 시계가 `tick`만큼 간다(왕복 하나가 `tick`이다).
    struct Manual {
        wall: Mutex<i64>,
        mono: Mutex<Duration>,
        tick: Duration,
        actions: Mutex<Vec<Action>>,
    }

    impl Manual {
        fn new(wall: i64, tick: Duration) -> Self {
            Self {
                wall: Mutex::new(wall),
                mono: Mutex::new(Duration::ZERO),
                tick,
                actions: Mutex::default(),
            }
        }
    }

    impl Clock for Manual {
        fn read(&self) -> Reading {
            let mut m = self.mono.lock().unwrap();
            *m += self.tick;
            Reading {
                wall: *self.wall.lock().unwrap(),
                mono: *m,
            }
        }
        fn step(&self, by: i64) -> io::Result<()> {
            *self.wall.lock().unwrap() += by;
            self.actions.lock().unwrap().push(Action::Step(by));
            Ok(())
        }
        fn slew(&self, by: i64) -> io::Result<()> {
            self.actions.lock().unwrap().push(Action::Slew(by));
            Ok(())
        }
    }

    /// 호스트 노릇: `ClockAsk`마다 `host`를 답하고 끝 메시지를 돌려준다.
    fn host_side(stream: &mut std::os::unix::net::UnixStream, host: i64) -> (u32, GuestMsg) {
        let mut asks = 0;
        loop {
            match GuestMsg::read_from(stream).unwrap() {
                GuestMsg::ClockAsk => {
                    asks += 1;
                    HostMsg::ClockNow { unix_nanos: host }
                        .write_to(stream)
                        .unwrap();
                }
                other => return (asks, other),
            }
        }
    }

    fn run(clock: &Manual, host: i64) -> (u32, GuestMsg) {
        let (mut h, g) = std::os::unix::net::UnixStream::pair().unwrap();
        let out = Out::new(g.try_clone().unwrap());
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut g = g;
                sync(clock, &mut g, &out)
            });
            host_side(&mut h, host)
        })
    }

    #[test]
    fn an_exchange_steps_a_clock_that_is_an_hour_behind() {
        // 왕복 2µs.
        let c = Manual::new(1_000, Duration::from_micros(2));
        let (asks, end) = run(&c, 3_600_000_000_000 + 1_000);
        assert_eq!(asks, CLOCK_ROUNDS);
        assert_eq!(end, GuestMsg::Done);
        let a = c.actions.lock().unwrap().clone();
        // 어림은 호스트 시각 + rtt/2(=1µs)다.
        assert_eq!(a, vec![Action::Step(3_600_000_000_000 + 1_000)]);
    }

    #[test]
    fn an_exchange_slews_a_small_offset() {
        let c = Manual::new(10_000_000_000, Duration::from_micros(2));
        let (_, end) = run(&c, 10_000_000_000 - 3_000_000);
        assert_eq!(end, GuestMsg::Done);
        assert_eq!(
            *c.actions.lock().unwrap(),
            vec![Action::Slew(-3_000_000 + 1_000)]
        );
    }

    #[test]
    fn a_slow_exchange_changes_nothing() {
        // 왕복 30ms.
        let c = Manual::new(0, Duration::from_millis(30));
        let (_, end) = run(&c, 3_600_000_000_000);
        assert!(
            matches!(end, GuestMsg::Error(e) if e.kind == ErrorKind::Io && e.message.contains("30ms"))
        );
        assert!(c.actions.lock().unwrap().is_empty());
    }

    #[test]
    fn a_wrong_answer_is_a_protocol_error() {
        let c = Manual::new(0, Duration::from_micros(1));
        let (mut h, g) = std::os::unix::net::UnixStream::pair().unwrap();
        let out = Out::new(g.try_clone().unwrap());
        let t = std::thread::spawn(move || {
            let mut g = g;
            sync(&c, &mut g, &out);
            c.actions.into_inner().unwrap()
        });
        assert_eq!(GuestMsg::read_from(&mut h).unwrap(), GuestMsg::ClockAsk);
        HostMsg::End.write_to(&mut h).unwrap();
        assert!(
            matches!(GuestMsg::read_from(&mut h).unwrap(), GuestMsg::Error(e) if e.kind == ErrorKind::Protocol)
        );
        assert!(t.join().unwrap().is_empty());
    }

    /// 실제 커널 시계를 바로·천천히 옮긴다. **그 커널의 벽시계를 바꾼다.** 컨테이너는 호스트와 벽시계를 함께 쓴다
    /// (Linux 시간 이름공간은 `CLOCK_MONOTONIC`과 `CLOCK_BOOTTIME`만 나눈다). 그래서 root(`CAP_SYS_TIME`)와
    /// `SANDBOX_PRIVILEGED_TESTS=1`에 더해 `SANDBOX_CLOCK_TESTS=1`일 때만 돈다. 버려도 되는 VM 안에서만 켠다
    /// (예: sbx 샌드박스 안. 끝에 시계를 되돌리고, 다음 맞추기가 어차피 호스트에 맞춘다). CI는 켜지 않는다.
    #[cfg(target_os = "linux")]
    #[test]
    fn privileged_kernel_clock_steps_and_slews() {
        if std::env::var_os("SANDBOX_PRIVILEGED_TESTS").is_none()
            || std::env::var_os("SANDBOX_CLOCK_TESTS").is_none()
        {
            eprintln!("SANDBOX_PRIVILEGED_TESTS and SANDBOX_CLOCK_TESTS not both set, skipping");
            return;
        }
        let k = Kernel;
        // 벽시계 - 단조 시계(나노초, `CLOCK_MONOTONIC_RAW`). 둘 다 같은 빠르기로 가면 그대로다.
        let gap = || {
            let r = k.read();
            i128::from(r.wall) - r.mono.as_nanos() as i128
        };
        let near = |a: i128, b: i128, ms: i128| (a - b).abs() < ms * 1_000_000;

        // 바로: 2초 앞으로, 다시 2.5초 뒤로(음수의 나눗셈), 0.5초 앞으로. 처음과 같아진다.
        let start = gap();
        k.step(2_000_000_000).unwrap();
        assert!(near(gap(), start + 2_000_000_000, 50));
        k.step(-2_500_000_000).unwrap();
        assert!(near(gap(), start - 500_000_000, 50));
        k.step(500_000_000).unwrap();
        assert!(near(gap(), start, 50));

        // 천천히 앞으로 50ms: 1초에 0.5ms쯤 가고 바로 다 가지 않는다.
        let before = gap();
        k.slew(50_000_000).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        let moved = gap() - before;
        assert!(
            (200_000..=3_000_000).contains(&moved),
            "moved {moved}ns in 2s"
        );

        // 천천히 뒤로: 벽시계는 거꾸로 가지 않는다. 바로 맞추기는 남은 천천히 맞추기를 취소한다.
        k.slew(-50_000_000).unwrap();
        let mut last = k.read().wall;
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < until {
            let now = k.read().wall;
            assert!(now >= last, "the clock went back by {}ns", last - now);
            last = now;
        }
        k.step(0).unwrap();
        let settled = gap();
        std::thread::sleep(Duration::from_secs(1));
        assert!(near(gap(), settled, 1), "a cancelled slew kept going");
        // 처음 차이로 되돌린다.
        k.step((start - gap()) as i64).unwrap();
        assert!(near(gap(), start, 50));
    }
}
