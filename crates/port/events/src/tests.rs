use super::*;

use std::sync::atomic::{AtomicUsize, Ordering};

fn temp_dir(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!(
        "sbx-events-{}-{name}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&d);
    d
}

fn small(dir: &Path, max: u64) -> Log {
    Log {
        dir: dir.to_owned(),
        max,
    }
}

fn drain(r: &mut Reader) -> Vec<Event> {
    std::iter::from_fn(|| r.next_event().unwrap()).collect()
}

#[test]
fn appended_events_read_back_in_order_with_times() {
    let log = Log::new(temp_dir("order"));
    let mut r = log.reader(Start::Beginning).unwrap();
    assert!(
        r.next_event().unwrap().is_none(),
        "nothing yet, no directory yet"
    );
    log.append(Event::new("sandbox", "create", "a").with("name", "web"))
        .unwrap();
    log.append(Event::new("sandbox", "start", "a")).unwrap();
    let got = drain(&mut r);
    assert_eq!(
        got.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
        ["create", "start"]
    );
    assert_eq!(got[0].attributes["name"], "web");
    assert!(got[0].time_ns > 0 && got[0].time_ns <= got[1].time_ns);
    // 더 없으면 None이고, 덧붙으면 이어서 읽힌다.
    assert!(r.next_event().unwrap().is_none());
    log.append(Event::new("sandbox", "stop", "a")).unwrap();
    assert_eq!(r.next_event().unwrap().unwrap().action, "stop");
}

#[test]
fn reading_from_the_end_sees_only_new_events() {
    let log = Log::new(temp_dir("end"));
    log.append(Event::new("image", "pull", "old")).unwrap();
    let mut r = log.reader(Start::End).unwrap();
    assert!(r.next_event().unwrap().is_none());
    log.append(Event::new("image", "pull", "new")).unwrap();
    assert_eq!(
        drain(&mut r).iter().map(|e| &*e.id).collect::<Vec<_>>(),
        ["new"]
    );
}

#[test]
fn half_written_line_waits_and_bad_lines_are_skipped() {
    let dir = temp_dir("partial");
    let log = Log::new(&dir);
    log.append(Event::new("sandbox", "create", "a")).unwrap();
    let mut r = log.reader(Start::Beginning).unwrap();
    assert_eq!(drain(&mut r).len(), 1);
    let path = dir.join("1.jsonl");
    let line = r#"{"time_ns":5,"kind":"sandbox","action":"start","id":"a"}"#;
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(b"not json\n{\"unknown\":1}\n").unwrap();
    f.write_all(&line.as_bytes()[..10]).unwrap();
    assert!(
        r.next_event().unwrap().is_none(),
        "half a line is not there yet"
    );
    f.write_all(&line.as_bytes()[10..]).unwrap();
    assert!(r.next_event().unwrap().is_none(), "still no newline");
    f.write_all(b"\n").unwrap();
    assert_eq!(r.next_event().unwrap().unwrap().action, "start");
}

#[test]
fn generations_roll_over_keep_one_previous_and_readers_follow() {
    let dir = temp_dir("roll");
    let log = small(&dir, 200);
    let mut follower = log.reader(Start::Beginning).unwrap();
    let mut seen = Vec::new();
    for i in 0..40 {
        log.append(Event::new("sandbox", "start", format!("{i}")))
            .unwrap();
        // 따라 읽는 쪽이 뒤처지지 않으면 하나도 잃지 않는다.
        seen.extend(drain(&mut follower).into_iter().map(|e| e.id));
    }
    assert_eq!(seen, (0..40).map(|i| i.to_string()).collect::<Vec<_>>());
    let gens = generations(&dir).unwrap();
    assert_eq!(gens.len(), 2, "{gens:?}");
    assert_eq!(gens[0] + 1, gens[1]);
    // 처음부터 읽으면 남은 두 세대만 나온다(오래된 것은 지웠다). 순서는 그대로다.
    let all: Vec<u64> = drain(&mut log.reader(Start::Beginning).unwrap())
        .iter()
        .map(|e| e.id.parse().unwrap())
        .collect();
    assert!(!all.is_empty() && all.len() < 40, "{all:?}");
    assert!(all.windows(2).all(|w| w[0] + 1 == w[1]), "{all:?}");
    assert_eq!(*all.last().unwrap(), 39);
}

#[test]
fn a_reader_far_behind_skips_to_what_is_left() {
    let dir = temp_dir("behind");
    let log = small(&dir, 100);
    log.append(Event::new("sandbox", "start", "first")).unwrap();
    let mut r = log.reader(Start::Beginning).unwrap();
    for i in 0..30 {
        log.append(Event::new("sandbox", "start", format!("{i}")))
            .unwrap();
    }
    // 세대 1은 지워졌다. 읽는 쪽은 남은 것으로 건너뛰어 끝까지 읽는다.
    let got = drain(&mut r);
    assert_eq!(got.last().unwrap().id, "29");
    assert!(got.len() < 31);
}

#[test]
fn concurrent_appends_do_not_interleave() {
    let dir = temp_dir("threads");
    let log = small(&dir, 1 << 30);
    std::thread::scope(|s| {
        for t in 0..8 {
            let log = log.clone();
            s.spawn(move || {
                for i in 0..200 {
                    let long = "x".repeat(500);
                    log.append(
                        Event::new("sandbox", "start", format!("{t}-{i}")).with("pad", long),
                    )
                    .unwrap();
                }
            });
        }
    });
    let got = drain(&mut log.reader(Start::Beginning).unwrap());
    assert_eq!(got.len(), 8 * 200);
    let mut ids: Vec<_> = got.iter().map(|e| e.id.clone()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 8 * 200);
    // 한 디렉터리 안에서는 덧붙인 순서가 곧 시각 순서다(시각은 잠금 안에서 정한다).
    assert!(got.windows(2).all(|w| w[0].time_ns <= w[1].time_ns));
}
