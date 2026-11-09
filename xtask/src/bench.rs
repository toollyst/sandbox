//! `bench`: `sbx` 실행 파일로 시간을 잰다. 누구나 자기 환경에서 같은 숫자를 다시 낼 수 있게 한다.
//!
//! ```text
//! cargo xtask bench [--sbx <파일>] [--image <참조>] [--runs <N>] [--parallel <K>]
//! ```
//!
//! - `--sbx`: 기본은 `target/release/sbx` (`cargo xtask dist`가 만든 것). 실행기, 디렉터리 같은 설정은 환경 변수
//!   (`SBX_EXECUTOR`, `SBX_DATA_DIR`, `SBX_CACHE_DIR`, ...)와 설치한 기본 실행기를 그대로 따른다.
//! - `--image`: 기본 `alpine:3.20`. 재기 전에 `pull`과 `prepare`를 한다(재지 않는다).
//!
//! 재는 것 (모두 `sbx` 프로세스를 띄워서 끝날 때까지의 벽시계 시간):
//!
//! | 이름 | 명령 | 뜻 |
//! |---|---|---|
//! | run | `run --rm <이미지> true` × N | VM 부팅 → 명령 → 끄기 → 지우기 전체 |
//! | exec | `exec <ID> true` × N | 실행 중인 샌드박스 안의 명령 하나 |
//! | parallel | `run --rm <이미지> true` K개 동시 | K개가 모두 끝날 때까지 |
//!
//! 결과는 Markdown 표로 표준 출력에 쓴다. 실패한 명령이 있으면 그 출력과 함께 멈춘다.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Options {
    sbx: PathBuf,
    image: String,
    runs: usize,
    parallel: usize,
}

fn parse(root: &Path, args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        sbx: root.join("target/release/sbx"),
        image: "alpine:3.20".into(),
        runs: 20,
        parallel: 10,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--sbx" => o.sbx = value()?.into(),
            "--image" => o.image = value()?,
            "--runs" => o.runs = value()?.parse().map_err(|_| "--runs: not a number")?,
            "--parallel" => {
                o.parallel = value()?.parse().map_err(|_| "--parallel: not a number")?
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.runs == 0 || o.parallel == 0 {
        return Err("--runs and --parallel must be at least 1".into());
    }
    Ok(o)
}

fn sbx(o: &Options, args: &[&str]) -> Result<Output, String> {
    let out = Command::new(&o.sbx)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", o.sbx.display()))?;
    if !out.status.success() {
        return Err(format!(
            "sbx {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out)
}

fn timed(o: &Options, args: &[&str]) -> Result<Duration, String> {
    let t = Instant::now();
    sbx(o, args)?;
    Ok(t.elapsed())
}

/// 정렬된 표본의 백분위 (가장 가까운 순위).
fn percentile(sorted: &[Duration], p: f64) -> Duration {
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn row(name: &str, what: &str, mut samples: Vec<Duration>) -> String {
    samples.sort();
    format!(
        "| {name} | {what} | {} | {} | {} | {} | {} |",
        samples.len(),
        ms(samples[0]),
        ms(percentile(&samples, 50.0)),
        ms(percentile(&samples, 90.0)),
        ms(*samples.last().expect("at least one sample")),
    )
}

/// 끝날 때 지운다 (중간에 실패해도).
struct Detached<'a> {
    o: &'a Options,
    id: String,
}

impl Drop for Detached<'_> {
    fn drop(&mut self) {
        let _ = sbx(self.o, &["rm", "-f", &self.id]);
    }
}

/// 재고 표를 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<String, String> {
    let o = parse(root, args)?;
    if !o.sbx.is_file() {
        return Err(format!(
            "{} not found (build it with `cargo xtask dist`, or give --sbx)",
            o.sbx.display()
        ));
    }
    let img = o.image.as_str();
    eprintln!("bench: pull and prepare {img}");
    sbx(&o, &["pull", img])?;
    sbx(&o, &["prepare", img])?;
    // 한 번 돌려서 캐시(게스트 init 꺼내기 등)를 데운다.
    sbx(&o, &["run", "--rm", img, "true"])?;

    let mut rows = Vec::new();
    eprintln!("bench: run x{}", o.runs);
    let samples = (0..o.runs)
        .map(|_| timed(&o, &["run", "--rm", img, "true"]))
        .collect::<Result<Vec<_>, _>>()?;
    rows.push(row("run", &format!("`sbx run --rm {img} true`"), samples));

    eprintln!("bench: exec x{}", o.runs);
    let out = sbx(&o, &["run", "-d", img, "sleep", "600"])?;
    let d = Detached {
        o: &o,
        id: String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    };
    let samples = (0..o.runs)
        .map(|_| timed(&o, &["exec", &d.id, "true"]))
        .collect::<Result<Vec<_>, _>>()?;
    rows.push(row("exec", "`sbx exec <ID> true`", samples));
    drop(d);

    eprintln!("bench: {} in parallel", o.parallel);
    let t = Instant::now();
    let children = (0..o.parallel)
        .map(|_| {
            Command::new(&o.sbx)
                .args(["run", "--rm", img, "true"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("cannot run {}: {e}", o.sbx.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for c in children {
        let out = c.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "parallel run failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    rows.push(row(
        "parallel",
        &format!(
            "{} × `sbx run --rm {img} true` at once (all done)",
            o.parallel
        ),
        vec![t.elapsed()],
    ));

    let host = Command::new("uname")
        .arg("-srm")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    let executor = sbx(&o, &["executor", "ls"])
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .find(|l| l.starts_with('*'))
                .map(|l| {
                    l.split_whitespace()
                        .skip(1)
                        .take(2)
                        .collect::<Vec<_>>()
                        .join("@")
                })
        })
        .or_else(|| std::env::var("SBX_EXECUTOR").ok())
        .unwrap_or_else(|| "sbx-executor on PATH".into());
    let mut table = format!(
        "host: {host}, executor: {executor}\n\n| name | command | runs | min | p50 | p90 | max |\n|---|---|---|---|---|---|---|\n"
    );
    for r in rows {
        table.push_str(&r);
        table.push('\n');
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let s: Vec<Duration> = (1..=10).map(Duration::from_millis).collect();
        assert_eq!(percentile(&s, 50.0), Duration::from_millis(5));
        assert_eq!(percentile(&s, 90.0), Duration::from_millis(9));
        assert_eq!(percentile(&s, 100.0), Duration::from_millis(10));
        assert_eq!(percentile(&s[..1], 90.0), Duration::from_millis(1));
    }
}
