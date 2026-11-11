//! 분리 실행(`-d`) 샌드박스의 출력 로그 `main.log` (`sbx-shim`의 `LogWriter`, `LogReader`). 쓰는 바이트는 게스트
//! 프로그램의 출력이고, `sbx logs`가 나중에 이 파일을 읽어 기록으로 나눈다.
//!
//! 두 가지를 한다.
//!
//! 1. 입력 바이트를 그대로 로그 파일로 두고 읽는다: 패닉하지 않고, 오류나 끝에서 멈춘다.
//! 2. 게스트가 고른 출력 조각들을 작은 한도로 쓰고 다시 읽는다. 출력 내용으로 기록 경계를 속일 수 없어야 한다:
//!    읽은 기록은 쓴 조각을 순서대로 자른 것이고(흐름도 같다), 한도 알림은 많아야 하나이며 맨 끝에 있고,
//!    파일은 한도를 넘지 않는다.

#![no_main]

use std::fs;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sbx_fuzz::Scratch;
use sbx_shim::log::LOG_FULL;
use sbx_shim::{LogReader, LogWriter, Record, Stream};

#[derive(Debug, Arbitrary)]
struct Input {
    raw: Vec<u8>,
    chunks: Vec<(bool, Vec<u8>)>,
    limit: u16,
}

fn read_all(path: &std::path::Path) -> std::io::Result<Vec<Record>> {
    let mut r = LogReader::open(path)?;
    let mut out = Vec::new();
    while let Some(rec) = r.next_record()? {
        out.push(rec);
    }
    Ok(out)
}

fuzz_target!(|input: Input| {
    let run = Scratch::get("shim_log").fresh();

    let raw = run.join("raw.log");
    fs::write(&raw, &input.raw).unwrap();
    if let Ok(records) = read_all(&raw) {
        let bytes: usize = records.iter().map(|r| r.data.len()).sum();
        assert!(bytes <= input.raw.len());
    }

    // 머리(8)와 알림 자리(13 + 알림 + 13)보다 큰 한도.
    let limit = 128 + u64::from(input.limit);
    let path = run.join("main.log");
    let mut w = LogWriter::open_with_limit(&path, limit).unwrap();
    for (stderr, data) in &input.chunks {
        let s = if *stderr {
            Stream::Stderr
        } else {
            Stream::Stdout
        };
        w.write(s, data).unwrap();
    }
    drop(w);
    assert!(
        fs::metadata(&path).unwrap().len() <= limit,
        "log grew past its limit"
    );

    let records = read_all(&path).expect("a log this sbx wrote reads back");
    let mut records = records.as_slice();
    if let [rest @ .., last] = records
        && last.stream == Stream::Stderr
        && last.data == LOG_FULL
    {
        records = rest;
    }
    let mut want = input.chunks.iter().filter(|(_, d)| !d.is_empty());
    for (i, rec) in records.iter().enumerate() {
        let (stderr, data) = want.next().expect("more records than chunks written");
        let s = if *stderr {
            Stream::Stderr
        } else {
            Stream::Stdout
        };
        assert_eq!(rec.stream, s, "record {i} changed stream");
        // 마지막으로 남은 조각만 한도 때문에 잘릴 수 있다.
        if i + 1 == records.len() {
            assert!(
                data.starts_with(&rec.data),
                "record {i} is not what was written"
            );
        } else {
            assert_eq!(&rec.data, data, "record {i} is not what was written");
        }
        assert!(rec.time_ns > 0, "record {i} has no time");
    }
});
