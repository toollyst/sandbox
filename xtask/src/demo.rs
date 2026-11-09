//! `demo`: README의 데모(움직이는 SVG)를 실제로 돌려 만든다.
//!
//! ```text
//! cargo xtask demo [--sbx <파일>] [--out <SVG>]
//! ```
//!
//! - [`SCENES`]의 명령을 bash 하나에서 차례로 실제로 돌린다(앞 명령의 변수가 남는다). 출력과 그 시각은 실제 그대로다.
//!   만든 것은 타자 치는 모습(글자마다 [`TYPE`])과 명령 사이의 쉼([`PAUSE`])뿐이다. 이미지는 먼저 받고 준비해 두고,
//!   화면에 담지 않고 한 번씩 돌려 둔다(처음 몇 번은 디스크 캐시가 차지 않아 느리다).
//! - 결과: 움직이는 SVG(기본 `assets/demo.svg`, 바깥 도구 없이 CSS 애니메이션만 쓴다. GitHub README에서 돈다)와
//!   asciinema cast v2(`target/demo/demo.cast`, `asciinema play`나 다른 변환기로 쓸 수 있다).
//! - `--sbx`: 기본은 `target/release/sbx`. 설정은 [`bench`](crate::bench)처럼 환경 변수와 설치한 기본 실행기를 따른다.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// 보여 줄 명령. 화면에 친 그대로 돈다.
const SCENES: &[&str] = &[
    "sbx run --rm alpine uname -sr",
    "time sbx run --rm alpine true",
    "sbx run --rm alpine ls /sys/class/net     # no network device at all",
    "sbx run --rm --network allow --allow example.com:443 curlimages/curl -s https://example.com | grep -o '<title>.*</title>'",
    "sbx run --rm --network allow --allow example.com:443 curlimages/curl -sS https://github.com",
    "id=$(sbx run -d alpine sleep 600)",
    "sbx exec $id cat /etc/alpine-release",
    "sbx ps",
    "sbx rm -f $id",
    "sbx run --rm python:3.12-alpine python -c 'print(2**100)'",
];
/// 먼저 받고 준비해 두는 이미지.
const IMAGES: &[&str] = &["alpine", "curlimages/curl", "python:3.12-alpine"];
/// 글자 하나 치는 시간.
const TYPE: Duration = Duration::from_millis(30);
/// 다 치고 Enter까지.
const BEFORE_ENTER: Duration = Duration::from_millis(300);
/// 결과를 읽을 시간.
const PAUSE: Duration = Duration::from_millis(1500);
/// 끝에서 다시 시작하기 전.
const END_HOLD: Duration = Duration::from_millis(4000);
const MARKER: &str = "__sbx_demo_done__";
/// 화면 크기(글자).
const COLS: usize = 100;
const ROWS: usize = 24;

/// 화면의 한 줄.
struct Row {
    /// (종류, 글자): 종류는 CSS 클래스(`p` 프롬프트, `c` 주석, `` 보통).
    spans: Vec<(&'static str, String)>,
    /// 보이기 시작하는 시각(초).
    appear: f64,
    /// 타자로 채우면: (시작 시각, 글자 수, 앞에 이미 보이는 글자 수).
    typed: Option<(f64, usize, usize)>,
}

fn len(spans: &[(&str, String)]) -> usize {
    spans.iter().map(|(_, s)| s.chars().count()).sum()
}

/// `spans`를 [`COLS`] 글자씩 자른다.
fn wrap(spans: Vec<(&'static str, String)>) -> Vec<Vec<(&'static str, String)>> {
    let mut rows = vec![Vec::new()];
    let mut used = 0;
    for (class, text) in spans {
        let mut chunk = String::new();
        for ch in text.chars() {
            if used == COLS {
                if !chunk.is_empty() {
                    rows.last_mut()
                        .expect("one")
                        .push((class, std::mem::take(&mut chunk)));
                }
                rows.push(Vec::new());
                used = 0;
            }
            chunk.push(ch);
            used += 1;
        }
        if !chunk.is_empty() {
            rows.last_mut().expect("one").push((class, chunk));
        }
    }
    rows
}

/// 명령 줄: 프롬프트, 명령, `#` 뒤의 주석.
fn command_spans(scene: &str) -> Vec<(&'static str, String)> {
    let mut v = vec![("p", "$ ".to_owned())];
    match scene.find("  #") {
        Some(i) => {
            v.push(("", scene[..i].to_owned()));
            v.push(("c", scene[i..].to_owned()));
        }
        None => v.push(("", scene.to_owned())),
    }
    v
}

/// ESC로 시작하는 제어 순서를 뺀다(색은 쓰지 않는다).
fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 줄들을 움직이는 SVG로. `total`(초)마다 처음부터 다시 돈다.
fn svg(rows: &[Row], total: f64) -> String {
    const FONT: f64 = 14.0;
    const CW: f64 = 8.4;
    const LH: f64 = 20.0;
    const PAD: f64 = 18.0;
    const BAR: f64 = 34.0;
    let (w, h) = (
        COLS as f64 * CW + 2.0 * PAD,
        BAR + ROWS as f64 * LH + 2.0 * PAD - 6.0,
    );
    let pct = |t: f64| (t / total * 100.0).clamp(0.0, 100.0);
    let mut css = String::new();
    let mut body = String::new();
    // 스크롤: 줄이 화면보다 많아지면 그 줄이 나타날 때 한 줄 올린다.
    let mut scroll = String::from("@keyframes s{0%{transform:translateY(0)}");
    for (i, r) in rows.iter().enumerate().skip(ROWS) {
        let up = (i + 1 - ROWS) as f64 * LH;
        let _ = write!(
            scroll,
            "{:.3}%{{transform:translateY(-{:.0}px)}}{:.3}%{{transform:translateY(-{up:.0}px)}}",
            (pct(r.appear) - 0.001).max(0.0),
            up - LH,
            pct(r.appear),
        );
    }
    scroll.push('}');
    css.push_str(&scroll);
    for (i, r) in rows.iter().enumerate() {
        let y = i as f64 * LH;
        let a = pct(r.appear);
        let _ = write!(
            css,
            "@keyframes a{i}{{0%,{:.3}%{{opacity:0}}{a:.3}%,100%{{opacity:1}}}}.a{i}{{animation:a{i} {total:.3}s step-end infinite}}",
            (a - 0.001).max(0.0)
        );
        let n = len(&r.spans);
        let _ = write!(
            body,
            "<text class=\"a{i}\" x=\"0\" y=\"{:.1}\"{}>",
            y + FONT,
            if n > 0 {
                format!(
                    " textLength=\"{:.1}\" lengthAdjust=\"spacingAndGlyphs\"",
                    n as f64 * CW
                )
            } else {
                String::new()
            }
        );
        for (class, text) in &r.spans {
            if class.is_empty() {
                body.push_str(&xml(text));
            } else {
                let _ = write!(body, "<tspan class=\"{class}\">{}</tspan>", xml(text));
            }
        }
        body.push_str("</text>");
        // 타자: 배경색 덮개를 글자 단위로 오른쪽으로 민다.
        if let Some((start, chars, shown)) = r.typed {
            let (s, e) = (pct(start), pct(start + chars as f64 * TYPE.as_secs_f64()));
            let dx = chars as f64 * CW;
            let _ = write!(
                css,
                "@keyframes t{i}{{0%,{s:.3}%{{transform:translateX(0);animation-timing-function:steps({chars},end)}}{e:.3}%,100%{{transform:translateX({dx:.1}px)}}}}.t{i}{{animation:t{i} {total:.3}s infinite}}"
            );
            let _ = write!(
                body,
                "<rect class=\"t{i}\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{LH}\" fill=\"#1d1f27\"/>",
                shown as f64 * CW,
                y,
                dx + CW,
            );
        }
    }
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w:.0}" height="{h:.0}" viewBox="0 0 {w:.0} {h:.0}" role="img" aria-label="sbx demo: run, network allow list, exec, ps, rm">
<style>
text{{font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace;font-size:{FONT}px;fill:#d7dae0;white-space:pre}}
.p{{fill:#7ee787;font-weight:bold}}.c{{fill:#7d8590}}
.scr{{animation:s {total:.3}s step-end infinite}}
{css}
</style>
<rect width="{w:.0}" height="{h:.0}" rx="10" fill="#1d1f27"/>
<circle cx="20" cy="17" r="6" fill="#ff5f57"/><circle cx="40" cy="17" r="6" fill="#febc2e"/><circle cx="60" cy="17" r="6" fill="#28c840"/>
<clipPath id="screen"><rect x="0" y="0" width="{sw:.1}" height="{sh:.1}"/></clipPath>
<g transform="translate({PAD} {top})"><g clip-path="url(#screen)"><g class="scr">
{body}
</g></g></g>
</svg>
"##,
        sw = COLS as f64 * CW + CW,
        sh = ROWS as f64 * LH,
        top = BAR + PAD / 2.0,
    )
}

/// 만든 파일(SVG, cast)을 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut sbx = crate::paths::target_dir(root).join("release/sbx");
    let mut out = root.join("assets/demo.svg");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--sbx" => sbx = it.next().ok_or("--sbx needs a value")?.into(),
            "--out" => out = it.next().ok_or("--out needs a value")?.into(),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let sbx = sbx
        .canonicalize()
        .map_err(|e| format!("{}: {e} (build it with `cargo xtask dist`)", sbx.display()))?;
    for image in IMAGES {
        eprintln!("demo: preparing {image}");
        for cmd in ["pull", "prepare"] {
            let ok = Command::new(&sbx)
                .args([cmd, image])
                .stdout(Stdio::null())
                .status()
                .map_err(|e| e.to_string())?
                .success();
            if !ok {
                return Err(format!("sbx {cmd} {image} failed"));
            }
        }
    }

    // 처음 몇 번은 디스크 캐시가 차지 않아 느리다. 화면에 담지 않고 먼저 돌린다.
    eprintln!("demo: warming up");
    for image in IMAGES.iter().chain(IMAGES) {
        let _ = Command::new(&sbx)
            .args(["run", "--rm", image, "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    // 화면의 `sbx`가 이 sbx가 되게 PATH 맨 앞에 둔다.
    let dir = sbx.parent().expect("a file").to_owned();
    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut shell = Command::new("bash")
        .args(["--noprofile", "--norc"])
        .env("PATH", path)
        .env("TIMEFORMAT", "real %3Rs")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run bash: {e}"))?;
    let mut stdin = shell.stdin.take().expect("piped");
    let stdout = shell.stdout.take().expect("piped");
    writeln!(stdin, "exec 2>&1").map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send((Instant::now(), line)).is_err() {
                break;
            }
        }
    });

    let mut cast = format!(
        "{{\"version\": 2, \"width\": {COLS}, \"height\": {ROWS}, \"env\": {{\"TERM\": \"xterm-256color\"}}}}\n"
    );
    let mut emit = |t: Duration, s: &str| {
        let _ = writeln!(
            cast,
            "[{:.3}, \"o\", {}]",
            t.as_secs_f64(),
            serde_json::to_string(s).expect("string")
        );
    };
    let mut rows: Vec<Row> = Vec::new();
    let mut clock = PAUSE / 2;
    for scene in SCENES {
        // 프롬프트가 나타나고, 명령을 친다.
        let appear = clock.as_secs_f64();
        emit(clock, "\x1b[1;32m$\x1b[0m ");
        let mut typed_before = 0;
        for (k, spans) in wrap(command_spans(scene)).into_iter().enumerate() {
            let n = len(&spans);
            let shown = if k == 0 { 2 } else { 0 };
            let chars = n - shown;
            rows.push(Row {
                spans,
                appear,
                typed: Some((
                    appear + PAUSE.as_secs_f64() / 3.0 + typed_before as f64 * TYPE.as_secs_f64(),
                    chars,
                    shown,
                )),
            });
            typed_before += chars;
        }
        clock += PAUSE / 3;
        for c in scene.chars() {
            emit(clock, &c.to_string());
            clock += TYPE;
        }
        clock += BEFORE_ENTER;
        emit(clock, "\r\n");

        // 실제로 돌리고, 줄이 나온 실제 시각에 보인다.
        let sent = Instant::now();
        let start = clock;
        writeln!(stdin, "{scene}\necho {MARKER}").map_err(|e| e.to_string())?;
        stdin.flush().map_err(|e| e.to_string())?;
        loop {
            let (at, line) = rx
                .recv_timeout(Duration::from_secs(120))
                .map_err(|_| format!("no output from: {scene}"))?;
            clock = start + at.duration_since(sent);
            if line == MARKER {
                break;
            }
            let line = strip_ansi(&line);
            emit(clock, &format!("{line}\r\n"));
            for spans in wrap(vec![("", line)]) {
                rows.push(Row {
                    spans,
                    appear: clock.as_secs_f64(),
                    typed: None,
                });
            }
        }
        eprintln!(
            "demo: {:>5} ms  {scene}",
            clock.saturating_sub(start).as_millis()
        );
        clock += PAUSE;
    }
    drop(stdin);
    let _ = shell.wait();
    emit(clock, "\x1b[1;32m$\x1b[0m ");
    rows.push(Row {
        spans: vec![("p", "$ ".into())],
        appear: clock.as_secs_f64(),
        typed: None,
    });
    clock += END_HOLD;
    emit(clock, "");

    let cast_path = crate::paths::target_dir(root).join("demo/demo.cast");
    for p in [&cast_path, &out] {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
    }
    std::fs::write(&cast_path, &cast).map_err(|e| e.to_string())?;
    std::fs::write(&out, svg(&rows, clock.as_secs_f64()))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(vec![out, cast_path])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_commands_wrap_and_comments_are_split() {
        let long = "x".repeat(COLS + 5);
        let rows = wrap(command_spans(&long));
        assert_eq!(rows.len(), 2);
        assert_eq!(len(&rows[0]), COLS);
        assert_eq!(len(&rows[1]), 7);
        let s = command_spans("ls   # hi");
        assert_eq!(s[2], ("c", "  # hi".to_owned()));
        assert_eq!(strip_ansi("\x1b[1;32mok\x1b[0m"), "ok");
        assert_eq!(xml("<a&b>"), "&lt;a&amp;b&gt;");
    }
}
