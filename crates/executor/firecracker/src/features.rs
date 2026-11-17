//! VMM이 upstream Firecracker 밖에서 더 가진 장치(확장)를 알아낸다. 규칙은 크레이트 문서의 "확장"에 있다.
//!
//! - `<firecracker> --features`를 표준 입력 없이 띄운다. [`PROBE_TIMEOUT`] 안에 0으로 끝나고 표준 출력이 JSON 객체일
//!   때만 그 답을 믿는다. 그 밖(그 인자를 모르는 upstream Firecracker는 오류로 끝난다, 시간을 넘김, JSON이 아님)은 모두
//!   "확장 없음"이다. upstream으로 보고 설정 파일에 `extensions`를 넣지 않는다.
//! - 읽는 것은 최상위 `extensions` 하나다. 이름 목록(`["fs"]`)이거나 이름을 키로 한 객체(`{"fs": {...}}`)다. 모르는
//!   이름과 다른 필드는 무시한다. 표준 출력은 [`MAX_OUTPUT`] 바이트까지만 읽는다.
//! - 이 인자는 wick이 제안한 것이다. 다른 VMM이 같은 인자로 같은 모양을 답하면 그대로 쓴다. sbx는 어느 VMM인지 보지 않는다.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// `--features`의 답을 기다리는 시간. `capabilities`가 이 안에 끝나야 한다(sbx는 30초까지 기다린다).
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `--features`의 표준 출력에서 읽는 최대 바이트 수.
pub const MAX_OUTPUT: u64 = 64 * 1024;

/// 확장 이름: 공유 폴더(virtio-fs).
pub const FS: &str = "fs";

/// 알아낸 확장.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Features {
    /// virtio-fs 공유 폴더를 붙일 수 있다(설정 파일의 `extensions.fs`).
    pub fs: bool,
}

/// `--features`의 표준 출력을 읽는다. 읽을 수 없으면 확장 없음이다.
pub fn parse(stdout: &[u8]) -> Features {
    let Ok(Value::Object(top)) = serde_json::from_slice::<Value>(stdout) else {
        return Features::default();
    };
    let has = |name: &str| match top.get("extensions") {
        Some(Value::Array(names)) => names.iter().any(|n| n.as_str() == Some(name)),
        Some(Value::Object(map)) => map.contains_key(name),
        _ => false,
    };
    Features { fs: has(FS) }
}

/// `firecracker --features`로 알아낸다(시간 제한 [`PROBE_TIMEOUT`]).
pub fn probe(firecracker: &Path) -> Features {
    probe_within(firecracker, PROBE_TIMEOUT)
}

/// [`probe`]를 `timeout` 안에. 시간을 넘기면 그 프로세스를 죽이고 확장 없음이다.
pub fn probe_within(firecracker: &Path, timeout: Duration) -> Features {
    let Ok(mut child) = Command::new(firecracker)
        .arg("--features")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Features::default();
    };
    let Some(out) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Features::default();
    };
    // 표준 출력은 따로 읽는다: 다 읽기 전에는 끝나지 않는 VMM도, 출력을 닫지 않고 멈춘 VMM도 시간 안에 끊는다.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.take(MAX_OUTPUT).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    match status {
        // 출력의 끝도 남은 시간 안에만 기다린다(출력을 쥔 자식을 남기고 끝난 VMM). 넘기면 읽기 스레드를 버린다.
        Some(s) if s.success() => rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map(|b| parse(&b))
            .unwrap_or_default(),
        _ => Features::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_are_read_as_a_list_or_an_object() {
        assert!(parse(br#"{"extensions":["fs","gpu"]}"#).fs);
        assert!(parse(br#"{"extensions":{"fs":{"max_shares":8}},"version":"1.18.0"}"#).fs);
        assert!(!parse(br#"{"extensions":["gpu"]}"#).fs);
        assert!(!parse(br#"{"extensions":{}}"#).fs);
        // 모양이 다르거나 JSON이 아니면 확장 없음.
        assert!(!parse(br#"{"extensions":"fs"}"#).fs);
        assert!(!parse(br#"["fs"]"#).fs);
        assert!(!parse(b"Firecracker v1.18.0").fs);
        assert!(!parse(b"").fs);
    }

    #[cfg(unix)]
    fn script(name: &str, body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("sbx-fc-features-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("firecracker");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, p)
    }

    /// upstream처럼 인자를 모르면 오류로 끝난다: 확장 없음. 알리면 그대로. 멈추면 시간 안에 끊는다. 없는 파일도 확장 없음.
    #[cfg(unix)]
    #[test]
    fn probing_trusts_only_a_clean_answer_in_time() {
        let (d1, upstream) = script(
            "upstream",
            r#"echo "Arguments parsing error: Found argument '$1' which wasn't expected" >&2; exit 1"#,
        );
        assert_eq!(probe(&upstream), Features::default());
        let (d2, wick) = script(
            "ext",
            r#"[ "$1" = --features ] && echo '{"extensions":["fs"]}' && exit 0; exit 2"#,
        );
        assert!(probe(&wick).fs);
        // 0이 아닌 끝은 출력이 있어도 믿지 않는다.
        let (d3, failing) = script("failing", r#"echo '{"extensions":["fs"]}'; exit 1"#);
        assert!(!probe(&failing).fs);
        let (d4, stuck) = script("stuck", "exec sleep 30");
        let t = Instant::now();
        assert!(!probe_within(&stuck, Duration::from_millis(200)).fs);
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        assert!(!probe(&d4.join("missing")).fs);
        for d in [d1, d2, d3, d4] {
            std::fs::remove_dir_all(d).unwrap();
        }
    }
}
