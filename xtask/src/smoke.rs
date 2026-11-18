//! `smoke`: 설치한 실행기로 실제 VM을 띄워 기본 동작을 확인한다. 로컬(예: macOS HVF)과 CI(Linux KVM)에서 같은 것을 돈다.
//!
//! ```text
//! cargo xtask smoke [--sbx <파일>] [--no-internet]
//! ```
//!
//! - `--sbx`: 기본은 `target/release/sbx`. 설정은 [`bench`](crate::bench)처럼 환경 변수와 설치한 기본 실행기를 따른다.
//! - `--no-internet`: 인터넷이 필요한 확인(허용한 곳으로 HTTPS)을 건너뛴다. 이미지는 미리 받아 둬야 한다.
//!
//! 확인하는 것: 출력과 종료 코드(0, 7, 127), 표준 입력, `/dev/fd`·`/dev/stdin` 링크, 네트워크 장치 없음, 이름 풀기가 곧바로 실패함(5초 시간
//! 초과를 기다리지 않음), 쓰기 가능한 루트, detached 실행과
//! `exec`/`cp`/`logs`/`stop`(SIGTERM으로 143)/`rm`, `--connect`(다른 샌드박스의 포트에 이름으로, 열지 않은 포트는 닫힘),
//! `build`(두 스테이지, 닫힌 네트워크의 `RUN`, `COPY --from`; 만든 것을 돌리고, `-o type=oci`로 내보내 `load`하면
//! 내보낸 ID이고 그것도 돌며, 다시 만들면 `RUN`과 `COPY`가 캐시다), 허용 목록(허용한 곳은 200, 나머지는 막힘).
//! 모두 27개이고 `--no-internet`이면 25개다. 실행기가 실시간 공유(`live_share`)를 알리면 4개가 더 있다: `-v`로
//! 양쪽이 서로의 쓰기를 보고, 호스트에서 고친 파일을 게스트의 inotify(busybox `inotifyd`)가 본다. 알리지 않으면
//! 건너뛴다고 쓴다.
//! 하나라도 다르면 무엇이 달랐는지 쓰고 실패한다.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const IMAGE: &str = "alpine:3.20";
const CURL: &str = "curlimages/curl:8.11.1";

/// 빌드 확인의 Dockerfile: 첫 스테이지의 `RUN`은 닫힌 네트워크에서 돌고(이름 풀기가 곧바로 실패해 `closed`),
/// 둘째 스테이지가 그 파일을 `COPY --from`으로 가져온다.
fn dockerfile() -> String {
    format!(
        "FROM {IMAGE} AS gen\n\
         RUN echo built > /made.txt && \
         (wget -q -T 3 -O /dev/null http://example.com 2>/dev/null && echo open || echo closed) > /net.txt\n\
         FROM {IMAGE}\n\
         COPY --from=gen /made.txt /net.txt /app/\n\
         CMD [\"cat\", \"/app/made.txt\", \"/app/net.txt\"]\n"
    )
}

/// 만든 이미지를 돌린 출력.
const BUILT_OUTPUT: &str = "built\nclosed\n";

/// `--format json build`의 줄들.
fn build_events(out: &str) -> Result<Vec<serde_json::Value>, String> {
    out.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| format!("build output {l:?}: {e}")))
        .collect()
}

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

/// 기본 실행기가 실시간 공유(`live_share`)를 알리는지(`sbx info --format json`).
fn live_share(s: &Sbx) -> Result<bool, String> {
    let o = s.out(&["info", "--format", "json"], None)?;
    if !o.status.success() {
        return Err(format!("sbx info: {}", text(&o.stderr).trim()));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&o.stdout).map_err(|e| format!("sbx info: {e}"))?;
    Ok(v["executor"]["capabilities"]["live_share"] == true)
}

/// 실시간 공유(`-v`): 양쪽이 서로의 쓰기를 보고, 호스트에서 고친 파일을 게스트의 파일 감시(busybox `inotifyd`)가 본다.
/// 실행기가 `live_share`를 알릴 때만 돈다. 확인한 수를 돌려준다.
fn live_mount(s: &Sbx) -> Result<usize, String> {
    let dir = std::env::temp_dir().join(format!("sbx-smoke-share-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let result = (|| {
        std::fs::write(dir.join("in.txt"), "hello").map_err(|e| e.to_string())?;
        let v = format!("{}:/src", dir.display());
        expect(
            s,
            "live mount both ways",
            &[
                "run",
                "--rm",
                "-v",
                &v,
                IMAGE,
                "sh",
                "-c",
                "cat /src/in.txt; echo back > /src/out.txt",
            ],
            None,
            0,
            Some("hello"),
        )?;
        let back = std::fs::read_to_string(dir.join("out.txt")).unwrap_or_default();
        if back != "back\n" {
            return Err(format!("live mount both ways: the host read {back:?}"));
        }
        let name = format!("sbx-smoke-watch-{}", std::process::id());
        let watched = (|| {
            expect(
                s,
                "live mount watcher",
                &[
                    "run",
                    "-d",
                    "--name",
                    &name,
                    "-v",
                    &v,
                    IMAGE,
                    "sh",
                    "-c",
                    "inotifyd - /src > /tmp/events",
                ],
                None,
                0,
                None,
            )?;
            std::thread::sleep(std::time::Duration::from_secs(1));
            std::fs::write(dir.join("app.js"), "v2").map_err(|e| e.to_string())?;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let o = s.out(&["exec", &name, "cat", "/tmp/events", "/src/app.js"], None)?;
                let out = text(&o.stdout);
                if out.contains("app.js") && out.ends_with("v2") {
                    eprintln!("smoke: ok   live mount host edit reaches inotify");
                    return Ok(());
                }
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "live mount host edit: the guest's inotifyd saw no app.js within 10s: {out:?} {}",
                        text(&o.stderr).trim()
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        })();
        let rm = expect(
            s,
            "live mount watcher rm",
            &["rm", "-f", &name],
            None,
            0,
            None,
        );
        watched?;
        rm?;
        Ok(4)
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// 확인하고 몇 개를 확인했는지 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<usize, String> {
    let mut sbx = crate::paths::target_dir(root).join("release/sbx");
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
        "/dev/fd and /dev/stdin links",
        &[
            "run",
            "--rm",
            "-i",
            IMAGE,
            "sh",
            "-c",
            "test -L /dev/fd && test -L /dev/stderr && cat /dev/stdin && cat /dev/fd/0",
        ],
        Some("via /dev/stdin\n"),
        0,
        Some("via /dev/stdin\n"),
    ))?;
    check(expect(
        &s,
        "no network device",
        &["run", "--rm", IMAGE, "ls", "/sys/class/net"],
        None,
        0,
        Some("lo\n"),
    ))?;
    // 네트워크가 없으면 이름 풀기는 곧바로 실패한다(게스트의 루프백 DNS). 리졸버 시간 초과는 5초다.
    let t = std::time::Instant::now();
    check(expect(
        &s,
        "name lookup fails fast",
        &[
            "run",
            "--rm",
            IMAGE,
            "sh",
            "-c",
            "wget -q -O- http://example.com",
        ],
        None,
        1,
        Some(""),
    ))?;
    if t.elapsed() > std::time::Duration::from_secs(3) {
        return Err(format!(
            "name lookup fails fast: took {:?} (the resolver timed out)",
            t.elapsed()
        ));
    }
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

    // detached: exec, cp(이름으로), logs, stop(SIGTERM → 143), rm.
    let name = format!("sbx-smoke-{}", std::process::id());
    let id = check(expect(
        &s,
        "run -d",
        &[
            "run",
            "-d",
            "--name",
            &name,
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
            "cp out (by name)",
            &["cp", &format!("{name}:/out.txt"), &local_s],
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

    // --connect: 한 샌드박스의 포트에 다른 샌드박스가 이름으로 닿는다(busybox nc, 프록시 없음). 열지 않은 포트는 닫혀 있다.
    let db = format!("sbx-smoke-db-{}", std::process::id());
    let result = (|| {
        check(expect(
            &s,
            "connect target",
            &[
                "run",
                "-d",
                "--name",
                &db,
                IMAGE,
                "sh",
                "-c",
                "while true; do echo pong | nc -l -p 5432; done",
            ],
            None,
            0,
            None,
        ))?;
        let client = format!(
            "for i in 1 2 3 4 5; do r=$(nc -w 3 {db} 5432 </dev/null) && [ -n \"$r\" ] && break; sleep 1; done; \
             echo \"$r\"; nc -w 3 {db} 5433 </dev/null >/dev/null 2>&1 && echo open || echo closed"
        );
        check(expect(
            &s,
            "connect by name",
            &[
                "run",
                "--rm",
                "--connect",
                &format!("{db}:5432"),
                IMAGE,
                "sh",
                "-c",
                &client,
            ],
            None,
            0,
            Some("pong\nclosed\n"),
        ))
    })();
    let rm = expect(&s, "connect target rm", &["rm", "-f", &db], None, 0, None);
    result?;
    check(rm)?;

    // build: 두 스테이지(닫힌 네트워크의 RUN, COPY --from)를 만들어 돌리고, OCI 아카이브로 내보내 들이면 내보낸
    // ID이고 그것도 돈다. 바뀐 것 없이 다시 만들면 RUN과 COPY가 모두 캐시다. Docker도 인터넷도 쓰지 않는다.
    let pid = std::process::id();
    let tag = format!("sbx-smoke-build:{pid}");
    let dir = std::env::temp_dir().join(format!("sbx-smoke-build-{pid}"));
    let ctx = dir.join("ctx");
    let archive = dir.join("image.tar");
    let mut made: Vec<String> = Vec::new();
    let result = (|| {
        std::fs::create_dir_all(&ctx).map_err(|e| format!("{}: {e}", ctx.display()))?;
        std::fs::write(ctx.join("Dockerfile"), dockerfile())
            .map_err(|e| format!("{}: {e}", ctx.display()))?;
        let ctx_s = ctx.to_string_lossy().into_owned();
        let built = check(expect(
            &s,
            "build (two stages, closed network, COPY --from)",
            &["build", "-q", "-t", &tag, &ctx_s],
            None,
            0,
            None,
        ))?;
        made.push(built.trim().to_owned());
        check(expect(
            &s,
            "run the built image",
            &["run", "--rm", &tag],
            None,
            0,
            Some(BUILT_OUTPUT),
        ))?;
        let dest = format!("type=oci,dest={}", archive.display());
        let out = check(expect(
            &s,
            "build -o type=oci",
            &["--format", "json", "build", "-t", &tag, "-o", &dest, &ctx_s],
            None,
            0,
            None,
        ))?;
        let exported = build_events(&out)?
            .into_iter()
            .find(|e| e["type"] == "exported")
            .and_then(|e| e["id"].as_str().map(str::to_owned))
            .ok_or_else(|| format!("build -o type=oci: no exported line in {out:?}"))?;
        made.push(exported.clone());
        let archive_s = archive.to_string_lossy().into_owned();
        let loaded = check(expect(
            &s,
            "load the archive",
            &["--format", "json", "load", "-i", &archive_s],
            None,
            0,
            None,
        ))?;
        let loaded: serde_json::Value =
            serde_json::from_str(&loaded).map_err(|e| format!("load: {e}: {loaded:?}"))?;
        if loaded[0]["id"] != exported.as_str() {
            return Err(format!(
                "load the archive: the image ID is {}, the build exported {exported}",
                loaded[0]["id"]
            ));
        }
        eprintln!("smoke: ok   the loaded image ID is the exported one");
        check(expect(
            &s,
            "run the loaded image",
            &["run", "--rm", &tag],
            None,
            0,
            Some(BUILT_OUTPUT),
        ))?;
        let again = check(expect(
            &s,
            "rebuild",
            &["--format", "json", "build", "-t", &tag, &ctx_s],
            None,
            0,
            None,
        ))?;
        // 단계마다 마지막 상태(`started` 뒤의 `cached`나 `done`).
        let mut last: Vec<(String, String)> = Vec::new();
        for e in build_events(&again)? {
            if e["type"] != "step" || e["status"] == "started" {
                continue;
            }
            let text = e["text"].as_str().unwrap_or_default().to_owned();
            last.push((text, e["status"].as_str().unwrap_or_default().to_owned()));
        }
        let layered: Vec<&(String, String)> = last
            .iter()
            .filter(|(t, _)| t.starts_with("RUN") || t.starts_with("COPY"))
            .collect();
        if layered.len() != 2 || layered.iter().any(|(_, st)| st != "cached") {
            return Err(format!(
                "rebuild: RUN and COPY should both come from the cache, got {last:?}"
            ));
        }
        eprintln!("smoke: ok   the rebuild came from the cache");
        Ok(())
    })();
    // 실패해도 지운다(빌드 캐시는 남긴다: 이 사용자의 것이기도 하다).
    let _ = s.out(&["rmi", "-f", &tag], None);
    for id in &made {
        let _ = s.out(&["rmi", "-f", id], None);
    }
    let _ = std::fs::remove_dir_all(&dir);
    result?;

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

    if live_share(&s)? {
        n += live_mount(&s)?;
    } else {
        eprintln!("smoke: skip live mount (the executor does not announce live sharing)");
    }
    Ok(n)
}
