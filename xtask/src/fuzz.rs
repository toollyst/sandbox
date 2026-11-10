//! `cargo xtask fuzz`: `fuzz/`의 퍼징 대상을 차례로 돌린다. CI(`fuzz.yml`)와 로컬이 같은 명령을 쓴다.
//!
//! ```text
//! cargo xtask fuzz [--time <초>] [대상...] [-- <libFuzzer 옵션>...]
//! cargo xtask fuzz --seeds
//! ```
//!
//! - 대상을 주지 않으면 `fuzz/fuzz_targets/*.rs` 전부다(이름 순). 대상마다 `--time`초(기본 60) 돈다.
//! - 대상마다 `cargo +<툴체인> fuzz run <대상> fuzz/corpus/<대상> fuzz/seeds/<대상>`이다. libFuzzer는 새로 찾은
//!   입력을 첫 디렉터리(`corpus`, git이 무시한다)에만 쓴다. 저장소에 둔 시작 입력(`seeds`)은 바뀌지 않는다.
//!   `fuzz/dict/<대상>.dict`가 있으면 사전으로 준다. 입력 하나가 30초를 넘으면 멈춘 것으로 보고 실패한다.
//! - 툴체인은 `SBX_FUZZ_TOOLCHAIN`, 없으면 `nightly`다(sanitizer에 nightly가 필요하다). `cargo-fuzz`가 있어야 한다.
//! - 한 대상이 실패해도(충돌, 시간 초과, 메모리 초과) 나머지를 돌리고, 끝에 표를 내고 실패로 끝난다.
//!   재현 입력은 `fuzz/artifacts/<대상>/`에 남는다.
//! - `--seeds`: `fuzz/seeds/`를 다시 만든다(`fuzz/examples/seeds.rs`, 안정 툴체인으로 된다).

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

/// 대상 이름들 (`fuzz/fuzz_targets/*.rs`, 이름 순).
pub fn targets(root: &Path) -> Result<Vec<String>, String> {
    let dir = root.join("fuzz/fuzz_targets");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".rs"))
                .map(str::to_owned)
        })
        .collect();
    out.sort();
    Ok(out)
}

/// 대상 하나의 결과.
struct Outcome {
    target: String,
    ok: bool,
    runs: String,
    cov: String,
    corpus: String,
}

/// 명령줄을 읽고 돌린다. 모두 통과하면 표를, 아니면 표를 담은 오류를 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<String, String> {
    let (ours, passthrough) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], &args[i + 1..]),
        None => (args, &[][..]),
    };
    let mut secs = 60u64;
    let mut chosen = Vec::new();
    let mut it = ours.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--time" => {
                secs = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--time needs a number of seconds")?;
            }
            "--seeds" => return seeds(root),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            t => chosen.push(t.to_owned()),
        }
    }
    let all = targets(root)?;
    for t in &chosen {
        if !all.contains(t) {
            return Err(format!("no fuzz target {t:?} (have: {})", all.join(", ")));
        }
    }
    let list = if chosen.is_empty() { all } else { chosen };
    let toolchain = std::env::var("SBX_FUZZ_TOOLCHAIN").unwrap_or_else(|_| "nightly".into());

    let mut outcomes = Vec::new();
    for t in &list {
        eprintln!("fuzz: {t} for {secs}s");
        outcomes.push(one(root, &toolchain, t, secs, passthrough)?);
    }

    let mut table =
        String::from("| target | result | runs | coverage | corpus |\n|---|---|---|---|---|\n");
    for o in &outcomes {
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            o.target,
            if o.ok { "ok" } else { "FAILED" },
            o.runs,
            o.cov,
            o.corpus
        ));
    }
    if outcomes.iter().all(|o| o.ok) {
        Ok(table)
    } else {
        Err(format!(
            "{table}\nreproducers are in fuzz/artifacts/<target>/; rerun one with \
             `cargo +{toolchain} fuzz run <target> <file>`"
        ))
    }
}

fn one(
    root: &Path,
    toolchain: &str,
    target: &str,
    secs: u64,
    passthrough: &[String],
) -> Result<Outcome, String> {
    let fuzz = root.join("fuzz");
    let corpus = format!("fuzz/corpus/{target}");
    std::fs::create_dir_all(root.join(&corpus)).map_err(|e| format!("{corpus}: {e}"))?;
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root)
        // `cargo xtask`가 물려준 툴체인 고정을 지우고 `+<툴체인>`을 따르게 한다.
        .env_remove("RUSTUP_TOOLCHAIN")
        .env_remove("CARGO")
        .env_remove("RUSTC")
        .arg(format!("+{toolchain}"))
        .args(["fuzz", "run", target, &corpus]);
    if fuzz.join("seeds").join(target).is_dir() {
        cmd.arg(format!("fuzz/seeds/{target}"));
    }
    cmd.arg("--")
        .arg(format!("-max_total_time={secs}"))
        // 입력 하나가 이보다 오래 걸리면 멈춘 것으로 보고 실패한다(CI와 같다).
        .arg("-timeout=30")
        .arg("-print_final_stats=1");
    if fuzz.join("dict").join(format!("{target}.dict")).is_file() {
        cmd.arg(format!("-dict=fuzz/dict/{target}.dict"));
    }
    cmd.args(passthrough).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| {
        format!("cannot run cargo +{toolchain} fuzz (is cargo-fuzz installed?): {e}")
    })?;
    let (mut runs, mut cov, mut corpus_size) =
        (String::from("?"), String::from("?"), String::from("?"));
    if let Some(err) = child.stderr.take() {
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            eprintln!("{line}");
            if let Some(n) = line.strip_prefix("stat::number_of_executed_units:") {
                runs = n.trim().to_owned();
            }
            // `#123 DONE cov: 456 ft: 789 corp: 12/34b ...`, 또는 중간 줄(`pulse`, `NEW`)의 마지막 값.
            let mut words = line.split_whitespace();
            while let Some(w) = words.next() {
                match w {
                    "cov:" => cov = words.next().unwrap_or("?").to_owned(),
                    "corp:" => corpus_size = words.next().unwrap_or("?").to_owned(),
                    _ => {}
                }
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    Ok(Outcome {
        target: target.to_owned(),
        ok: status.success(),
        runs,
        cov,
        corpus: corpus_size,
    })
}

fn seeds(root: &Path) -> Result<String, String> {
    let status = Command::new("cargo")
        .current_dir(root)
        .env_remove("CARGO")
        .args([
            "run",
            "--manifest-path",
            "fuzz/Cargo.toml",
            "--example",
            "seeds",
            "--",
            "fuzz/seeds",
        ])
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if status.success() {
        Ok("fuzz/seeds\n".into())
    } else {
        Err(format!("seed generator failed: {status}"))
    }
}
