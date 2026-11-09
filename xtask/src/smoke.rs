//! `smoke`: 설치한 실행기로 실제 VM을 띄워 기본 동작을 확인한다. 로컬(예: macOS HVF)과 CI(Linux KVM)에서 같은 것을 돈다.
//!
//! ```text
//! cargo xtask smoke [--sbx <파일>] [--no-internet]
//! ```
//!
//! - `--sbx`: 기본은 `target/release/sbx`. 설정은 [`bench`](crate::bench)처럼 환경 변수와 설치한 기본 실행기를 따른다.
//! - `--no-internet`: 인터넷이 필요한 확인(허용한 곳으로 HTTPS)을 건너뛴다. 이미지는 미리 받아 둬야 한다.
//!
//! 확인하는 것: 출력과 종료 코드(0, 7, 127), 표준 입력, 네트워크 장치 없음, 쓰기 가능한 루트, detached 실행과
//! `exec`/`cp`/`logs`/`stop`(SIGTERM으로 143)/`rm`, 허용 목록(허용한 곳은 200, 나머지는 막힘).
//! 하나라도 다르면 무엇이 달랐는지 쓰고 실패한다.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const IMAGE: &str = "alpine:3.20";
const CURL: &str = "curlimages/curl:8.11.1";

struct Sbx(PathBuf);

impl Sbx {
    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(&self.0);
        c.args(args);
        c
    }

    fn out(&self, args: &[&str], stdin: Option<&str>) -> Result<Output, String> {
        let mut c = self.cmd(args);
        c.stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = c
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", self.0.display()))?;
        if let Some(input) = stdin {
            let mut w = child.stdin.take().expect("piped");
            w.write_all(input.as_bytes()).map_err(|e| e.to_string())?;
        }
        child.wait_with_output().map_err(|e| e.to_string())
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// `args`를 돌려 종료 코드와 표준 출력을 확인한다.
fn expect(
    sbx: &Sbx,
    what: &str,
    args: &[&str],
    stdin: Option<&str>,
    code: i32,
    stdout: Option<&str>,
) -> Result<String, String> {
    let o = sbx.out(args, stdin)?;
    let got = o.status.code().unwrap_or(-1);
    let out = text(&o.stdout);
    if got != code || stdout.is_some_and(|s| s != out) {
        return Err(format!(
            "{what}: sbx {}\n  expected exit {code}{}\n  got exit {got}, stdout {out:?}\n  stderr: {}",
            args.join(" "),
            stdout
                .map(|s| format!(", stdout {s:?}"))
                .unwrap_or_default(),
            text(&o.stderr).trim()
        ));
    }
    eprintln!("smoke: ok   {what}");
    Ok(out)
}

/// 확인하고 몇 개를 확인했는지 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<usize, String> {
    let mut sbx = root.join("target/release/sbx");
    let mut internet = true;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sbx" => sbx = it.next().ok_or("--sbx needs a value")?.into(),
            "--no-internet" => internet = false,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if !sbx.is_file() {
        return Err(format!(
            "{} not found (build it with `cargo xtask dist`, or give --sbx)",
            sbx.display()
        ));
    }
    let s = Sbx(sbx);
    let mut n = 0;
    let mut check = |r: Result<String, String>| -> Result<String, String> {
        n += 1;
        r
    };

    check(expect(&s, "pull", &["pull", IMAGE], None, 0, None))?;
    check(expect(
        &s,
        "output",
        &["run", "--rm", IMAGE, "echo", "hello"],
        None,
        0,
        Some("hello\n"),
    ))?;
    check(expect(
        &s,
        "exit code",
        &["run", "--rm", IMAGE, "sh", "-c", "exit 7"],
        None,
        7,
        None,
    ))?;
    check(expect(
        &s,
        "command not found is 127",
        &["run", "--rm", IMAGE, "no-such-command"],
        None,
        127,
        None,
    ))?;
    check(expect(
        &s,
        "stdin",
        &["run", "--rm", "-i", IMAGE, "cat"],
        Some("piped in\n"),
        0,
        Some("piped in\n"),
    ))?;
    check(expect(
        &s,
        "no network device",
        &["run", "--rm", IMAGE, "ls", "/sys/class/net"],
        None,
        0,
        Some("lo\n"),
    ))?;
    check(expect(
        &s,
        "writable root",
        &[
            "run",
            "--rm",
            IMAGE,
            "sh",
            "-c",
            "echo x > /etc/x && cat /etc/x",
        ],
        None,
        0,
        Some("x\n"),
    ))?;

    // detached: exec, cp, logs, stop(SIGTERM → 143), rm.
    let id = check(expect(
        &s,
        "run -d",
        &[
            "run",
            "-d",
            IMAGE,
            "sh",
            "-c",
            "echo started; echo result > /out.txt; exec sleep 600",
        ],
        None,
        0,
        None,
    ))?;
    let id = id.trim().to_owned();
    let short = &id[..12.min(id.len())];
    let result = (|| {
        check(expect(
            &s,
            "exec",
            &["exec", short, "cat", "/etc/alpine-release"],
            None,
            0,
            None,
        ))?;
        let dir = std::env::temp_dir().join(format!("sbx-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let local = dir.join("out.txt");
        let local_s = local.to_string_lossy().into_owned();
        check(expect(
            &s,
            "cp out",
            &["cp", &format!("{short}:/out.txt"), &local_s],
            None,
            0,
            None,
        ))?;
        let got = std::fs::read_to_string(&local).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        if got != "result\n" {
            return Err(format!("cp out: got {got:?}"));
        }
        check(expect(
            &s,
            "logs",
            &["logs", short],
            None,
            0,
            Some("started\n"),
        ))?;
        check(expect(&s, "stop", &["stop", short], None, 0, None))?;
        check(expect(
            &s,
            "wait after stop (SIGTERM)",
            &["wait", short],
            None,
            0,
            Some("143\n"),
        ))
    })();
    // 실패해도 지운다.
    let rm = expect(&s, "rm", &["rm", "-f", short], None, 0, None);
    result?;
    check(rm)?;

    if internet {
        check(expect(
            &s,
            "allowed host",
            &[
                "run",
                "--rm",
                "--network",
                "allow",
                "--allow",
                "example.com:443",
                CURL,
                "-sS",
                "-o",
                "/dev/null",
                "-w",
                "%{http_code}\n",
                "https://example.com",
            ],
            None,
            0,
            Some("200\n"),
        ))?;
        let blocked = s.out(
            &[
                "run",
                "--rm",
                "--network",
                "allow",
                "--allow",
                "example.com:443",
                CURL,
                "-sS",
                "-m",
                "10",
                "https://github.com",
            ],
            None,
        )?;
        n += 1;
        if blocked.status.success() {
            return Err("blocked host: github.com was reachable".into());
        }
        eprintln!("smoke: ok   blocked host");
    }
    Ok(n)
}
