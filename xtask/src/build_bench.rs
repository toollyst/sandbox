//! `bench build`: 같은 Dockerfile을 `sbx build`와 `docker build`로 만들어 시간을 나란히 잰다(로드맵 M9 D6).
//!
//! ```text
//! cargo xtask bench build [--sbx <파일>] [--image <참조>] [--runs <N>] [--no-docker] [--network]
//!                         [--cpus <N>] [--memory <크기>] [--format markdown|json] [--dry-run]
//! ```
//!
//! - `--sbx`, `--image`: [`bench`](mod@crate::bench)와 같다(기본 `target/release/sbx`, `alpine:3.20`). 설정은 sbx와
//!   같은 환경 변수와 설치한 기본 실행기를 따른다.
//! - `--runs`: 경우마다 첫 빌드(cold) 뒤에 잴 빌드 수(warm, 기본 10).
//! - `--no-docker`: Docker와 견주지 않는다. `docker`가 PATH에 없거나 데몬에 닿지 않거나 플랫폼이 다르면 그 까닭을
//!   쓰고 sbx만 잰다(실패하지 않는다). 그래서 Docker가 없는 CI 러너에서도 돈다.
//! - `--network`: 네트워크를 쓰는 경우(`apk add`)를 더한다. 바탕이 Alpine이어야 한다.
//! - `--cpus`, `--memory`: sbx 빌드 VM의 `--cpus`, `-m`(기본은 sbx의 기본값). Docker 쪽은 Docker의 VM이나 호스트를 쓴다.
//! - `--format json`: 기계가 읽을 출력(아래 [`render_json`]). 기본은 Markdown 표.
//! - `--dry-run`: 아무것도 돌리지 않고 Dockerfile과 돌릴 명령을 쓴다.
//!
//! 경우(모두 같은 바탕, 경우마다 cold 한 번 + warm N번, sbx와 docker가 번갈아 먼저):
//!
//! | 이름 | Dockerfile | 재는 빌드 |
//! |---|---|---|
//! | no-cache | 한 스테이지(WORKDIR, COPY, 파일 200개를 만드는 RUN, COPY src/, RUN) | `build --no-cache` |
//! | cached | 같은 것 | 바뀐 것 없이 다시: 모든 단계가 캐시(sbx는 VM을 띄우지 않는다) |
//! | one-file-changed | 같은 것 | 빌드마다 `src/main.txt`를 바꾼다: `COPY src/`부터 다시, 앞은 캐시 |
//! | multi-stage | 세 스테이지, 마지막이 `COPY --from` 둘 | `build --no-cache` |
//! | network (`--network`) | `RUN apk add --no-cache jq` | `build --no-cache`, 미러에서 받는다 |
//!
//! 공정하게 하려고 정한 것:
//!
//! - 바탕 이미지는 재기 전에 양쪽에 받아 둔다(sbx는 `prepare`까지). 재지 않는다. `FROM`은 양쪽 모두 그 바탕에 붙인
//!   이 실행만의 로컬 태그(`sbx-bench-build/base:<nonce>`)라서 어느 빌더도 레지스트리에 묻지 않는다. 플랫폼은
//!   양쪽 모두 호스트 아키텍처(`linux/<arch>`)이고, Docker 서버가 다른 플랫폼이면 견주지 않는다.
//! - 각 빌드는 그 도구 프로세스를 띄워 끝날 때까지의 벽시계 시간이다(`-q`). 재는 동안 다른 일은 하지 않는다.
//! - 만든 이미지가 같은지: 경우마다 마지막 이미지를 양쪽에서 한 번씩 돌려(재지 않는다) 확인할 경로의 파일 목록(종류,
//!   권한, 소유자, 크기, sha256)과 작업 디렉터리, 사용자, `ENV`를 비교한다. 시각과 층 digest는 비교하지 않는다.
//!   다르면 표를 쓴 뒤 실패한다.
//! - 이 실행이 만든 것만 지운다: 빌드마다 고유한 태그(`sbx-bench-build/<경우>:<nonce>-<i>`)와 바탕 태그, 이 실행이
//!   받은 바탕 이미지, Docker의 빌드 기록(`docker buildx history rm`), 임시 디렉터리. 태그로만 지우고 ID로는 지우지
//!   않는다. 두 빌더의 빌드 캐시는 남는다(사용자의 것과 섞여 있어 이 실행의 것만 고를 수 없다).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::bench::percentile;

const DEFAULT_IMAGE: &str = "alpine:3.20";
/// 이 벤치가 만드는 모든 태그의 저장소 접두사. 끊겼을 때 손으로 지울 때도 이것으로 찾는다.
const REPO: &str = "sbx-bench-build";
/// 만든 이미지에 붙이는 라벨(값은 그 실행의 nonce).
const LABEL: &str = "io.github.toollyst.sandbox.bench";
/// `network` 경우가 받는 곳.
const APK_MIRROR: &str = "dl-cdn.alpinelinux.org:443";
const DOCKER: &str = "docker";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Markdown,
    Json,
}

#[derive(Debug)]
struct Options {
    sbx: PathBuf,
    image: String,
    runs: usize,
    docker: bool,
    network: bool,
    cpus: Option<String>,
    memory: Option<String>,
    format: Format,
    dry_run: bool,
}

fn parse(root: &Path, args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        sbx: crate::paths::target_dir(root).join("release/sbx"),
        image: DEFAULT_IMAGE.into(),
        runs: 10,
        docker: true,
        network: false,
        cpus: None,
        memory: None,
        format: Format::Markdown,
        dry_run: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--sbx" => o.sbx = value()?.into(),
            "--image" => o.image = value()?,
            "--runs" => o.runs = value()?.parse().map_err(|_| "--runs: not a number")?,
            "--no-docker" => o.docker = false,
            "--network" => o.network = true,
            "--cpus" => o.cpus = Some(value()?),
            "--memory" => o.memory = Some(value()?),
            "--format" => {
                o.format = match value()?.as_str() {
                    "markdown" => Format::Markdown,
                    "json" => Format::Json,
                    other => return Err(format!("--format: markdown or json, not {other}")),
                }
            }
            "--dry-run" => o.dry_run = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if o.runs == 0 {
        return Err("--runs must be at least 1".into());
    }
    Ok(o)
}

// ── 명령 돌리기 ────────────────────────────────────────────────────────

/// 끝난 명령 하나.
struct Ran {
    ok: bool,
    stdout: String,
    stderr: String,
    /// 띄워서 끝날 때까지.
    elapsed: Duration,
}

/// 명령을 돌리는 곳. 실제로는 [`Real`], `--dry-run`은 [`Dry`], 테스트는 가짜.
trait Shell {
    /// 띄우지 못하면 `Err`. 0이 아닌 종료 코드는 `Ran::ok`로 알린다.
    fn run(&mut self, program: &str, args: &[String]) -> Result<Ran, String>;
}

struct Real;

impl Shell for Real {
    fn run(&mut self, program: &str, args: &[String]) -> Result<Ran, String> {
        let t = Instant::now();
        let out = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        let elapsed = t.elapsed();
        Ok(Ran {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            elapsed,
        })
    }
}

/// 돌리지 않고 명령 줄을 모은다. 모두 성공하고 출력은 비어 있다.
#[derive(Default)]
struct Dry {
    lines: Vec<String>,
}

impl Shell for Dry {
    fn run(&mut self, program: &str, args: &[String]) -> Result<Ran, String> {
        self.lines
            .push(format!("$ {}", command_line(program, args)));
        Ok(Ran {
            ok: true,
            stdout: String::new(),
            stderr: String::new(),
            elapsed: Duration::ZERO,
        })
    }
}

/// 셸 낱말 하나.
fn quote(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c));
    if plain {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// 셸에 붙여 쓸 수 있는 명령 줄.
fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .map(quote)
        .collect::<Vec<_>>()
        .join(" ")
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// 성공해야 하는 명령. 실패면 그 명령 줄과 표준 오류.
fn must(sh: &mut dyn Shell, program: &str, args: &[String]) -> Result<Ran, String> {
    let r = sh.run(program, args)?;
    if !r.ok {
        return Err(format!(
            "{} failed: {}",
            command_line(program, args),
            r.stderr.trim()
        ));
    }
    Ok(r)
}

/// `docker.io/library/alpine:3.20`처럼 정규화한 참조(sbx `images`의 `references`와 같은 꼴).
fn normalize(reference: &str) -> String {
    let (name, rest) = match reference.find('@') {
        Some(at) => (&reference[..at], reference[at..].to_owned()),
        None => {
            let slash = reference.rfind('/').map_or(0, |i| i + 1);
            match reference[slash..].find(':') {
                Some(c) => (&reference[..slash + c], reference[slash + c..].to_owned()),
                None => (reference, ":latest".to_owned()),
            }
        }
    };
    let first = name.split('/').next().unwrap_or_default();
    let registry =
        name.contains('/') && (first.contains('.') || first.contains(':') || first == "localhost");
    let name = if registry {
        name.to_owned()
    } else if name.contains('/') {
        format!("docker.io/{name}")
    } else {
        format!("docker.io/library/{name}")
    };
    format!("{name}{rest}")
}

// ── 경우 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `--no-cache`: 모든 단계를 돌린다.
    NoCache,
    /// 한 번 채운 뒤 바뀐 것 없이 다시.
    Cached,
    /// 한 번 채운 뒤 빌드마다 `src/main.txt`를 바꾼다.
    OneFileChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum File {
    /// 한 스테이지.
    App,
    /// 세 스테이지, `COPY --from`.
    MultiStage,
    /// `apk add`(네트워크).
    Apk,
}

#[derive(Debug, Clone, Copy)]
struct Case {
    name: &'static str,
    what: &'static str,
    kind: Kind,
    file: File,
    network: bool,
    /// 같은 파일인지 볼 경로.
    paths: &'static [&'static str],
}

fn cases(network: bool) -> Vec<Case> {
    let mut v = vec![
        Case {
            name: "no-cache",
            what: "`build --no-cache` of one stage: WORKDIR, COPY, a RUN making 200 files, COPY src/, a RUN",
            kind: Kind::NoCache,
            file: File::App,
            network: false,
            paths: &["/app"],
        },
        Case {
            name: "cached",
            what: "the same Dockerfile again, nothing changed: every step comes from the cache",
            kind: Kind::Cached,
            file: File::App,
            network: false,
            paths: &["/app"],
        },
        Case {
            name: "one-file-changed",
            what: "the same Dockerfile after `src/main.txt` changed: `COPY src/` and the RUN after it run again, \
                   the steps before come from the cache",
            kind: Kind::OneFileChanged,
            file: File::App,
            network: false,
            paths: &["/app"],
        },
        Case {
            name: "multi-stage",
            what: "`build --no-cache` of three stages: two independent ones with a RUN each, then two `COPY --from`",
            kind: Kind::NoCache,
            file: File::MultiStage,
            network: false,
            paths: &["/app"],
        },
    ];
    if network {
        v.push(Case {
            name: "network",
            what: "`build --no-cache` of `RUN apk add --no-cache jq`: downloads from dl-cdn.alpinelinux.org \
                   (sbx: `--network allow --allow dl-cdn.alpinelinux.org:443`)",
            kind: Kind::NoCache,
            file: File::Apk,
            network: true,
            paths: &["/usr/bin/jq", "/etc/apk/world"],
        });
    }
    v
}

fn dockerfile(file: File, base: &str, nonce: &str) -> String {
    match file {
        File::App => format!(
            "FROM {base}\n\
             WORKDIR /app\n\
             COPY deps.txt ./\n\
             RUN mkdir gen && for i in $(seq 1 200); do echo \"line $i\" > gen/f$i.txt; done && sha256sum gen/* > gen.sums\n\
             COPY src/ ./src/\n\
             RUN cat src/*.txt deps.txt > bundle.txt && wc -l < bundle.txt > lines.txt\n\
             ENV APP_MODE=bench\n\
             LABEL {LABEL}={nonce}\n\
             CMD [\"cat\", \"/app/lines.txt\"]\n"
        ),
        File::MultiStage => format!(
            "FROM {base} AS gen\n\
             WORKDIR /work\n\
             COPY src/ ./src/\n\
             RUN mkdir out && for i in $(seq 1 100); do cat src/main.txt > out/copy$i.txt; done && sha256sum out/copy*.txt > out/SUMS\n\
             FROM {base} AS docs\n\
             RUN mkdir /docs && echo docs > /docs/README\n\
             FROM {base}\n\
             WORKDIR /app\n\
             COPY --from=gen /work/out/ ./out/\n\
             COPY --from=docs /docs/README ./\n\
             ENV APP_MODE=multi\n\
             LABEL {LABEL}={nonce}\n\
             CMD [\"ls\", \"/app\"]\n"
        ),
        File::Apk => format!(
            "FROM {base}\n\
             RUN apk add --no-cache jq\n\
             LABEL {LABEL}={nonce}\n\
             CMD [\"jq\", \"--version\"]\n"
        ),
    }
}

/// `one-file-changed`가 `i`번째 빌드 앞에 쓰는 내용. 실행마다(nonce), 빌드마다 다르다.
fn changed(nonce: &str, i: usize) -> String {
    format!("main {nonce} {i}\n")
}

fn write_context(dir: &Path, case: &Case, base: &str, nonce: &str) -> Result<(), String> {
    let put = |name: &str, data: &str| {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&p, data).map_err(|e| format!("{}: {e}", p.display()))
    };
    put("Dockerfile", &dockerfile(case.file, base, nonce))?;
    if case.file != File::Apk {
        let deps: String = (1..=50).map(|i| format!("dep-{i} 1.{i}.0\n")).collect();
        put("deps.txt", &deps)?;
        put("src/main.txt", "main\n")?;
        put("src/util.txt", "util\n")?;
    }
    Ok(())
}

/// 만든 이미지 안에서 돌려 같은 파일인지 볼 출력: 작업 디렉터리, 사용자, `APP_MODE`, 그리고 경로마다 디렉터리
/// (권한, 소유자), 나머지(종류, 권한, 소유자, 크기, 링크 대상), 파일의 sha256. 정렬한다. 시각과 디렉터리 크기는
/// 파일 시스템마다 달라 넣지 않는다. busybox와 coreutils 모두에서 돈다.
fn files_script(paths: &[&str]) -> String {
    let mut s = String::from(
        "{ echo \"workdir $(pwd)\"; echo \"user $(id -u):$(id -g)\"; echo \"APP_MODE=${APP_MODE-unset}\"; ",
    );
    for p in paths {
        s.push_str(&format!(
            "find {p} -type d | xargs -r stat -c 'dir %n %a %u:%g'; \
             find {p} ! -type d | xargs -r stat -c 'entry %N %F %a %u:%g %s'; \
             find {p} -type f | xargs -r sha256sum; "
        ));
    }
    s.push_str("} | sort");
    s
}

/// 처음으로 다른 줄(없으면 `None`).
fn first_difference(a: &str, b: &str) -> Option<String> {
    let (mut la, mut lb) = (a.lines(), b.lines());
    for n in 1.. {
        match (la.next(), lb.next()) {
            (None, None) => return None,
            (x, y) if x == y => continue,
            (x, y) => {
                return Some(format!(
                    "line {n}: sbx {:?}, docker {:?}",
                    x.unwrap_or("(none)"),
                    y.unwrap_or("(none)")
                ));
            }
        }
    }
    None
}

// ── 도구 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Sbx,
    Docker,
}

impl Side {
    fn label(self) -> &'static str {
        match self {
            Side::Sbx => "sbx",
            Side::Docker => "docker",
        }
    }
}

/// 빌드하는 쪽 하나와 그것에 만든 것.
struct Tool {
    side: Side,
    program: String,
    /// sbx: `--cpus`, `-m`.
    build_extra: Vec<String>,
    /// Docker: `--metadata-file`을 쓸 디렉터리(빌드 기록 ID를 얻는다).
    meta_dir: Option<PathBuf>,
    builds: usize,
    /// 빌드에 붙인 태그(지울 것).
    tags: Vec<String>,
    /// 바탕에 붙인 태그.
    base_tag: Option<String>,
    /// 이 실행이 받은 바탕(지울 것).
    pulled: Option<String>,
    /// Docker의 빌드 기록 ID.
    records: Vec<String>,
}

impl Tool {
    fn new(side: Side, program: &str) -> Tool {
        Tool {
            side,
            program: program.to_owned(),
            build_extra: Vec::new(),
            meta_dir: None,
            builds: 0,
            tags: Vec::new(),
            base_tag: None,
            pulled: None,
            records: Vec::new(),
        }
    }

    fn run(&self, sh: &mut dyn Shell, args: &[&str]) -> Result<Ran, String> {
        must(sh, &self.program, &strings(args))
    }

    /// 바탕을 받아(없을 때만) 준비하고 이 실행의 태그를 붙인다. 재지 않는다.
    fn setup(
        &mut self,
        sh: &mut dyn Shell,
        image: &str,
        base_tag: &str,
        platform: &str,
    ) -> Result<(), String> {
        match self.side {
            Side::Sbx => {
                let out = self.run(sh, &["--format", "json", "images"])?.stdout;
                let wanted = normalize(image);
                let present = if out.trim().is_empty() {
                    false
                } else {
                    let list: Vec<Value> = serde_json::from_str(&out)
                        .map_err(|e| format!("sbx images: {e}: {out:?}"))?;
                    list.iter().any(|i| {
                        i["references"]
                            .as_array()
                            .is_some_and(|r| r.iter().any(|r| r.as_str() == Some(&wanted)))
                    })
                };
                if !present {
                    self.run(sh, &["pull", image])?;
                    self.pulled = Some(image.to_owned());
                }
                self.run(sh, &["prepare", image])?;
            }
            Side::Docker => {
                let inspect = |sh: &mut dyn Shell| {
                    sh.run(
                        DOCKER,
                        &strings(&[
                            "image",
                            "inspect",
                            "--format",
                            "{{.Os}}/{{.Architecture}}",
                            image,
                        ]),
                    )
                };
                let mut r = inspect(sh)?;
                if !r.ok {
                    self.run(sh, &["pull", "-q", "--platform", platform, image])?;
                    self.pulled = Some(image.to_owned());
                    r = inspect(sh)?;
                }
                let got = r.stdout.trim();
                if !r.ok || (!got.is_empty() && got != platform) {
                    return Err(format!(
                        "Docker's {image} is for {got:?}, sbx builds for {platform}: pull {image} for {platform} in Docker"
                    ));
                }
            }
        }
        self.run(sh, &["tag", image, base_tag])?;
        self.base_tag = Some(base_tag.to_owned());
        Ok(())
    }

    fn build_args(
        &self,
        ctx: &Path,
        tag: &str,
        no_cache: bool,
        network: bool,
        meta: Option<&Path>,
    ) -> Vec<String> {
        let mut a = strings(&["build", "-q"]);
        if no_cache {
            a.push("--no-cache".into());
        }
        match self.side {
            Side::Sbx => {
                a.extend(self.build_extra.iter().cloned());
                if network {
                    a.extend(strings(&["--network", "allow", "--allow", APK_MIRROR]));
                }
            }
            Side::Docker => {
                if let Some(m) = meta {
                    a.push("--metadata-file".into());
                    a.push(m.to_string_lossy().into_owned());
                }
            }
        }
        a.push("-t".into());
        a.push(tag.to_owned());
        a.push(ctx.to_string_lossy().into_owned());
        a
    }

    /// 빌드 한 번. 그 프로세스의 시간을 돌려준다.
    fn build(
        &mut self,
        sh: &mut dyn Shell,
        ctx: &Path,
        tag: &str,
        no_cache: bool,
        network: bool,
    ) -> Result<Duration, String> {
        let meta = self
            .meta_dir
            .as_ref()
            .map(|d| d.join(format!("build-{}.json", self.builds)));
        self.builds += 1;
        let args = self.build_args(ctx, tag, no_cache, network, meta.as_deref());
        // 실패해도 태그가 생겼을 수 있다.
        self.tags.push(tag.to_owned());
        let r = sh.run(&self.program, &args)?;
        if let Some(id) = meta.as_deref().and_then(build_record) {
            self.records.push(id);
        }
        if !r.ok {
            let mut msg = format!(
                "{} failed: {}",
                command_line(&self.program, &args),
                r.stderr.trim()
            );
            if self.side == Side::Docker
                && (r.stderr.contains("pull access denied")
                    || r.stderr.contains("failed to resolve source metadata"))
            {
                msg.push_str(
                    "\n(Docker's builder looked for the base in a registry: this bench needs a builder that sees \
                     the local image store, the `docker` driver; `docker buildx ls` shows them)",
                );
            }
            return Err(msg);
        }
        Ok(r.elapsed)
    }

    /// 만든 이미지 안에서 [`files_script`]를 돌린 출력.
    fn files(&self, sh: &mut dyn Shell, tag: &str, script: &str) -> Result<String, String> {
        Ok(self
            .run(sh, &["run", "--rm", tag, "sh", "-c", script])?
            .stdout)
    }

    /// 이 실행이 만든 것을 지운다(태그로만). 지우지 못한 것을 돌려준다.
    fn cleanup(&mut self, sh: &mut dyn Shell) -> Vec<String> {
        let mut problems = Vec::new();
        let mut tags = std::mem::take(&mut self.tags);
        tags.dedup();
        tags.extend(self.base_tag.take());
        tags.extend(self.pulled.take());
        for t in &tags {
            let args = strings(&["rmi", t]);
            match sh.run(&self.program, &args) {
                Ok(r) if r.ok => {}
                Ok(r) if r.stderr.to_lowercase().contains("no such image") => {}
                Ok(r) => problems.push(format!(
                    "{}: {}",
                    command_line(&self.program, &args),
                    r.stderr.trim()
                )),
                Err(e) => problems.push(e),
            }
        }
        if !self.records.is_empty() {
            let mut args = strings(&["buildx", "history", "rm"]);
            args.append(&mut self.records);
            match sh.run(&self.program, &args) {
                Ok(r) if r.ok => {}
                Ok(r) => problems.push(format!(
                    "Docker kept {} build records ({}: {})",
                    args.len() - 3,
                    command_line(&self.program, &args),
                    r.stderr.trim()
                )),
                Err(e) => problems.push(e),
            }
        }
        problems
    }
}

/// `docker build --metadata-file`이 쓴 빌드 기록 ID(`buildx.build.ref`의 마지막 조각).
fn build_record(meta: &Path) -> Option<String> {
    let v: Value = serde_json::from_slice(&std::fs::read(meta).ok()?).ok()?;
    let r = v["buildx.build.ref"].as_str()?;
    r.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

// ── 재기 ──────────────────────────────────────────────────────────────

/// 한 도구의 시간: 첫 빌드와 그 뒤.
#[derive(Debug, Clone)]
struct Samples {
    cold: Duration,
    warm: Vec<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Stats {
    cold: Duration,
    n: usize,
    min: Duration,
    p50: Duration,
    p90: Duration,
    max: Duration,
}

impl Samples {
    /// 첫 표본이 cold다. 적어도 둘이어야 한다.
    fn from_runs(mut all: Vec<Duration>) -> Samples {
        let cold = all.remove(0);
        Samples { cold, warm: all }
    }

    fn stats(&self) -> Stats {
        let mut s = self.warm.clone();
        s.sort();
        Stats {
            cold: self.cold,
            n: s.len(),
            min: s[0],
            p50: percentile(&s, 50.0),
            p90: percentile(&s, 90.0),
            max: *s.last().expect("at least one warm run"),
        }
    }
}

/// 같은 파일인지 본 결과.
#[derive(Debug, Clone, PartialEq)]
enum Same {
    Yes,
    /// 처음 다른 줄.
    No(String),
    /// 견줄 상대가 없거나 돌리지 않았다.
    NotChecked,
}

#[derive(Debug, Clone)]
struct CaseResult {
    case: Case,
    sides: Vec<(Side, Samples)>,
    same: Same,
}

fn tag_for(case: &Case, nonce: &str, i: usize) -> String {
    format!("{REPO}/{}:{nonce}-{i}", case.name)
}

/// 이 실행의 이름과 횟수.
#[derive(Clone, Copy)]
struct Plan<'a> {
    /// 바탕에 붙인 태그(`FROM`).
    base_tag: &'a str,
    nonce: &'a str,
    /// warm 빌드 수.
    runs: usize,
    /// 만든 이미지가 같은 파일인지 본다(`--dry-run`이면 보지 않는다).
    check: bool,
}

/// 경우 하나: 채우기(필요하면, 재지 않는다), cold 한 번과 warm `runs`번, 그리고 같은 파일인지.
fn measure_case(
    sh: &mut dyn Shell,
    tools: &mut [Tool],
    case: &Case,
    ctx: &Path,
    plan: &Plan<'_>,
) -> Result<CaseResult, String> {
    let Plan {
        base_tag,
        nonce,
        runs,
        check,
    } = *plan;
    write_context(ctx, case, base_tag, nonce)?;
    let no_cache = case.kind == Kind::NoCache;
    if case.kind != Kind::NoCache {
        for t in tools.iter_mut() {
            t.build(sh, ctx, &tag_for(case, nonce, 0), false, case.network)?;
        }
    }
    let mut all: Vec<Vec<Duration>> = vec![Vec::new(); tools.len()];
    for i in 1..=runs + 1 {
        if case.kind == Kind::OneFileChanged {
            let p = ctx.join("src/main.txt");
            std::fs::write(&p, changed(nonce, i)).map_err(|e| format!("{}: {e}", p.display()))?;
        }
        // 번갈아 먼저: 머신의 부하와 캐시가 한쪽에만 몰리지 않게.
        let mut order: Vec<usize> = (0..tools.len()).collect();
        if i % 2 == 0 {
            order.reverse();
        }
        for k in order {
            let d = tools[k].build(sh, ctx, &tag_for(case, nonce, i), no_cache, case.network)?;
            all[k].push(d);
        }
    }
    let same = if check && tools.len() >= 2 {
        let script = files_script(case.paths);
        let last = tag_for(case, nonce, runs + 1);
        let a = tools[0].files(sh, &last, &script)?;
        let b = tools[1].files(sh, &last, &script)?;
        if !a.contains("workdir ") {
            return Err(format!(
                "{}: the file listing printed nothing: {a:?}",
                case.name
            ));
        }
        match first_difference(&a, &b) {
            None => Same::Yes,
            Some(d) => Same::No(d),
        }
    } else {
        Same::NotChecked
    };
    Ok(CaseResult {
        case: *case,
        sides: tools
            .iter()
            .zip(all)
            .map(|(t, s)| (t.side, Samples::from_runs(s)))
            .collect(),
        same,
    })
}

fn measure_all(
    sh: &mut dyn Shell,
    tools: &mut [Tool],
    o: &Options,
    dir: &Path,
    platform: &str,
    plan: &Plan<'_>,
) -> Result<Vec<CaseResult>, String> {
    for t in tools.iter_mut() {
        eprintln!("bench build: {} base {}", t.side.label(), o.image);
        t.setup(sh, &o.image, plan.base_tag, platform)?;
    }
    let mut results = Vec::new();
    for case in cases(o.network) {
        eprintln!(
            "bench build: {} (1 cold + {} warm builds per tool)",
            case.name, o.runs
        );
        results.push(measure_case(sh, tools, &case, &dir.join(case.name), plan)?);
    }
    Ok(results)
}

// ── 환경 ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
struct SbxInfo {
    version: String,
    executor: String,
    arch: String,
    /// 빌드 VM 크기.
    vm: String,
}

#[derive(Debug, Clone, Default)]
struct DockerInfo {
    client: String,
    server: String,
    product: String,
    platform: String,
    driver: String,
    cpus: String,
    memory: String,
}

fn sbx_info(sh: &mut dyn Shell, program: &str) -> Result<SbxInfo, String> {
    let out = must(sh, program, &strings(&["info", "--format", "json"]))?.stdout;
    let v: Value = serde_json::from_str(&out).map_err(|e| format!("sbx info: {e}"))?;
    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let mut executor = s(&v["executor"]["name"]);
    if let Some(a) = v["executor"]["adapter"].as_str() {
        executor.push_str(&format!(" (adapter {a})"));
    }
    Ok(SbxInfo {
        version: s(&v["version"]),
        executor,
        arch: s(&v["arch"]),
        vm: String::new(),
    })
}

/// Docker와 견줄 수 있는지. 아니면 그 까닭.
fn docker_info(sh: &mut dyn Shell, arch: &str) -> Result<DockerInfo, String> {
    let r = sh
        .run(DOCKER, &strings(&["version", "--format", "{{json .}}"]))
        .map_err(|_| "`docker` is not on PATH".to_owned())?;
    let v: Value = serde_json::from_str(r.stdout.trim()).unwrap_or(Value::Null);
    if !r.ok || v["Server"].is_null() {
        let why = r
            .stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        return Err(format!(
            "the Docker daemon is not reachable ({})",
            why.trim()
        ));
    }
    let s = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let platform = format!("{}/{}", s(&v["Server"]["Os"]), s(&v["Server"]["Arch"]));
    if platform != format!("linux/{arch}") {
        return Err(format!(
            "Docker builds for {platform}, sbx for linux/{arch}: not the same platform"
        ));
    }
    let mut d = DockerInfo {
        client: s(&v["Client"]["Version"]),
        server: s(&v["Server"]["Version"]),
        product: s(&v["Server"]["Platform"]["Name"]),
        platform,
        ..DockerInfo::default()
    };
    if let Ok(r) = sh.run(DOCKER, &strings(&["info", "--format", "{{json .}}"]))
        && let Ok(v) = serde_json::from_str::<Value>(r.stdout.trim())
    {
        d.cpus = v["NCPU"]
            .as_u64()
            .map(|n| n.to_string())
            .unwrap_or_default();
        d.memory = v["MemTotal"].as_u64().map(gib).unwrap_or_default();
    }
    if let Ok(r) = sh.run(DOCKER, &strings(&["buildx", "inspect"])) {
        d.driver = r
            .stdout
            .lines()
            .find_map(|l| l.strip_prefix("Driver:"))
            .map(|x| x.trim().to_owned())
            .unwrap_or_default();
    }
    Ok(d)
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64)
}

/// 호스트: OS, 아키텍처, CPU 모델과 수, 메모리. 못 읽은 것은 뺀다.
fn host() -> String {
    let read = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let mut parts = vec![
        read("uname", &["-srm"])
            .unwrap_or_else(|| format!("{} {}", std::env::consts::OS, std::env::consts::ARCH)),
    ];
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let model = read("sysctl", &["-n", "machdep.cpu.brand_string"]).or_else(|| {
        cpuinfo
            .lines()
            .find_map(|l| l.strip_prefix("model name"))
            .map(|l| l.trim_start_matches([' ', '\t', ':']).to_owned())
    });
    parts.extend(model);
    if let Ok(n) = std::thread::available_parallelism() {
        parts.push(format!("{n} CPUs"));
    }
    let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let mem = read("sysctl", &["-n", "hw.memsize"])
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| {
            meminfo
                .lines()
                .find_map(|l| l.strip_prefix("MemTotal:"))
                .and_then(|l| l.trim().trim_end_matches("kB").trim().parse::<u64>().ok())
                .map(|kb| kb * 1024)
        });
    parts.extend(mem.map(gib));
    parts.join(", ")
}

// ── 보고 ──────────────────────────────────────────────────────────────

#[derive(Debug)]
struct Report {
    host: String,
    sbx: SbxInfo,
    docker: Result<DockerInfo, String>,
    image: String,
    platform: String,
    runs: usize,
    cases: Vec<CaseResult>,
    /// 지우지 못한 것.
    leftovers: Vec<String>,
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn ms_value(d: Duration) -> f64 {
    (d.as_secs_f64() * 10_000.0).round() / 10.0
}

/// sbx p50 / docker p50.
fn ratio(r: &CaseResult) -> Option<f64> {
    let p50 = |side| {
        r.sides
            .iter()
            .find(|(s, _)| *s == side)
            .map(|(_, x)| x.stats().p50.as_secs_f64())
    };
    match (p50(Side::Sbx), p50(Side::Docker)) {
        (Some(a), Some(b)) if b > 0.0 => Some(a / b),
        _ => None,
    }
}

fn render_markdown(r: &Report) -> String {
    let mut out = String::from("### `sbx build` and `docker build`, same Dockerfiles\n\n");
    out.push_str(&format!("- host: {}\n", r.host));
    out.push_str(&format!(
        "- sbx: {}, executor {}, build VM {}\n",
        r.sbx.version, r.sbx.executor, r.sbx.vm
    ));
    match &r.docker {
        Ok(d) => {
            let mut s = format!("- docker: {} (server {}", d.client, d.server);
            if !d.product.is_empty() {
                s.push_str(&format!(", {}", d.product));
            }
            s.push_str(&format!("), {}", d.platform));
            if !d.driver.is_empty() {
                s.push_str(&format!(", builder driver {}", d.driver));
            }
            if !d.cpus.is_empty() {
                s.push_str(&format!(
                    ", {} CPUs and {} for its containers",
                    d.cpus, d.memory
                ));
            }
            out.push_str(&s);
            out.push('\n');
        }
        Err(why) => out.push_str(&format!("- docker: not compared: {why}\n")),
    }
    out.push_str(&format!(
        "- base: {} for {}, pulled and prepared on both sides before timing (not counted); `FROM` names a local tag \
         of it, so no build asks a registry\n",
        r.image, r.platform
    ));
    out.push_str(&format!(
        "- each case: cold is the first timed build, then {} warm builds (min, p50, p90, max); sbx and docker take \
         turns going first; each time is the tool's process from start to exit\n\n",
        r.runs
    ));
    out.push_str(
        "| case | tool | cold | warm runs | min | p50 | p90 | max | p50 sbx/docker | same files |\n\
         |---|---|---|---|---|---|---|---|---|---|\n",
    );
    for c in &r.cases {
        let same = match &c.same {
            Same::Yes => "yes".to_owned(),
            Same::No(_) => "**no**".to_owned(),
            Same::NotChecked => "–".to_owned(),
        };
        for (side, samples) in &c.sides {
            let s = samples.stats();
            let (ratio, same) = match side {
                Side::Sbx => (
                    ratio(c).map_or_else(|| "–".to_owned(), |x| format!("{x:.2}×")),
                    same.as_str(),
                ),
                Side::Docker => (String::new(), ""),
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {ratio} | {same} |\n",
                c.case.name,
                side.label(),
                ms(s.cold),
                s.n,
                ms(s.min),
                ms(s.p50),
                ms(s.p90),
                ms(s.max),
            ));
        }
    }
    out.push_str("\nCases:\n\n");
    for c in &r.cases {
        out.push_str(&format!("- {}: {}\n", c.case.name, c.case.what));
    }
    out.push_str(
        "\nWhat is not the same on both sides, and is not made the same:\n\n\
         - Isolation: sbx runs each stage's steps in a VM of its own; Docker runs RUN steps in containers (inside \
         Docker's VM on macOS and Windows, on the host kernel on Linux).\n\
         - Stages: Docker's builder runs independent stages at the same time; sbx builds them one after another \
         (multi-stage).\n\
         - Resources: the sbx build VM has the size above; Docker's builder uses all of its VM or host.\n\
         - Output: sbx keeps layers as disks for its VMs (`type=sbx`); Docker exports to its image store, with \
         whatever its builder adds by default.\n\
         - Same files: each case's last image is run once on each side (not timed) and lists the checked paths: \
         type, mode, owner, size, sha256 of each file, link targets, the working directory, user and `APP_MODE`. \
         Times and layer digests are not compared.\n\
         - Left in place: build cache entries on both sides (`sbx builder prune`, `docker builder prune`).\n",
    );
    for c in &r.cases {
        if let Same::No(d) = &c.same {
            out.push_str(&format!("\n**{}: the images differ**: {d}\n", c.case.name));
        }
    }
    if !r.leftovers.is_empty() {
        out.push_str("\nCould not remove:\n\n");
        for l in &r.leftovers {
            out.push_str(&format!("- {l}\n"));
        }
    }
    out
}

/// 기계가 읽을 출력. `format`이 1인 동안 필드를 지우거나 뜻을 바꾸지 않는다(더하기만 한다).
fn render_json(r: &Report) -> String {
    let cases: Vec<Value> = r
        .cases
        .iter()
        .map(|c| {
            let mut sides = serde_json::Map::new();
            for (side, samples) in &c.sides {
                let s = samples.stats();
                sides.insert(
                    side.label().into(),
                    json!({
                        "cold_ms": ms_value(s.cold),
                        "warm_ms": samples.warm.iter().copied().map(ms_value).collect::<Vec<_>>(),
                        "min_ms": ms_value(s.min),
                        "p50_ms": ms_value(s.p50),
                        "p90_ms": ms_value(s.p90),
                        "max_ms": ms_value(s.max),
                    }),
                );
            }
            json!({
                "name": c.case.name,
                "description": c.case.what,
                "network": c.case.network,
                "sbx": sides.get("sbx").cloned().unwrap_or(Value::Null),
                "docker": sides.get("docker").cloned().unwrap_or(Value::Null),
                "p50_sbx_over_docker": ratio(c),
                "same_files": match &c.same {
                    Same::Yes => json!(true),
                    Same::No(_) => json!(false),
                    Same::NotChecked => Value::Null,
                },
                "difference": match &c.same {
                    Same::No(d) => json!(d),
                    _ => Value::Null,
                },
            })
        })
        .collect();
    let docker = match &r.docker {
        Ok(d) => json!({
            "compared": true,
            "client": d.client,
            "server": d.server,
            "product": d.product,
            "platform": d.platform,
            "driver": d.driver,
            "cpus": d.cpus,
            "memory": d.memory,
        }),
        Err(why) => json!({ "compared": false, "reason": why }),
    };
    let v = json!({
        "kind": "sbx-bench-build",
        "format": 1,
        "host": r.host,
        "sbx": { "version": r.sbx.version, "executor": r.sbx.executor, "build_vm": r.sbx.vm },
        "docker": docker,
        "base": r.image,
        "platform": r.platform,
        "warm_runs": r.runs,
        "cases": cases,
        "leftovers": r.leftovers,
    });
    serde_json::to_string_pretty(&v).expect("JSON values serialize") + "\n"
}

// ── 들어가는 곳 ─────────────────────────────────────────────────────────

/// 이 실행만의 짧은 이름 조각(태그에 쓴다).
fn nonce() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{:08x}",
        (t.as_nanos() as u64 ^ (u64::from(std::process::id()) << 32)) as u32
    )
}

/// 재고 보고를 돌려준다. 두 번째 값은 모든 경우가 같은 파일이었는지(다르면 `false`, 보고는 그래도 쓴다).
pub fn run(root: &Path, args: &[String]) -> Result<(String, bool), String> {
    let o = parse(root, args)?;
    if o.dry_run {
        return Ok((dry_run(&o)?, true));
    }
    if !o.sbx.is_file() {
        return Err(format!(
            "{} not found (build it with `cargo xtask dist`, or give --sbx)",
            o.sbx.display()
        ));
    }
    let mut sh = Real;
    let report = measure(&mut sh, &o, host(), nonce())?;
    let all_same = !report.cases.iter().any(|c| matches!(c.same, Same::No(_)));
    let text = match o.format {
        Format::Markdown => render_markdown(&report),
        Format::Json => render_json(&report),
    };
    Ok((text, all_same))
}

fn dry_run(o: &Options) -> Result<String, String> {
    let nonce = nonce();
    let base_tag = format!("{REPO}/base:{nonce}");
    let mut out = String::from("dry run: nothing runs. The Dockerfiles:\n");
    let mut files: Vec<File> = Vec::new();
    for c in cases(o.network) {
        if !files.contains(&c.file) {
            files.push(c.file);
            out.push_str(&format!(
                "\n# {:?}\n{}",
                c.file,
                dockerfile(c.file, &base_tag, &nonce)
            ));
        }
    }
    out.push_str(&format!(
        "\nThe images are checked with: sh -c {}\n\nThe commands (with --runs {}; this list assumes sbx lacks the base \
         and Docker has it, and leaves out the checks and Docker's version probes):\n\n",
        quote(&files_script(cases(o.network)[0].paths)),
        o.runs
    ));
    let mut sh = Dry::default();
    measure(&mut sh, o, "(dry run)".into(), nonce)?;
    for l in sh.lines {
        out.push_str(&l);
        out.push('\n');
    }
    Ok(out)
}

/// 환경을 묻고, 재고, 이 실행이 만든 것을 지운다(실패해도).
fn measure(sh: &mut dyn Shell, o: &Options, host: String, nonce: String) -> Result<Report, String> {
    let sbx = o.sbx.to_string_lossy().into_owned();
    let mut info = if o.dry_run {
        SbxInfo {
            arch: host_arch().into(),
            ..SbxInfo::default()
        }
    } else {
        sbx_info(sh, &sbx)?
    };
    info.vm = format!(
        "{} vCPU, {}{}",
        o.cpus.as_deref().unwrap_or("1"),
        o.memory.as_deref().unwrap_or("512m"),
        if o.cpus.is_none() && o.memory.is_none() {
            " (sbx's default)"
        } else {
            ""
        }
    );
    let platform = format!("linux/{}", info.arch);
    let docker = if !o.docker {
        Err("--no-docker".to_owned())
    } else if o.dry_run {
        Ok(DockerInfo::default())
    } else {
        docker_info(sh, &info.arch)
    };
    if let Err(why) = &docker {
        eprintln!("bench build: not comparing with Docker: {why}");
    }

    let mut s = Tool::new(Side::Sbx, &sbx);
    if let Some(c) = &o.cpus {
        s.build_extra.extend(["--cpus".to_owned(), c.clone()]);
    }
    if let Some(m) = &o.memory {
        s.build_extra.extend(["-m".to_owned(), m.clone()]);
    }
    let dir = std::env::temp_dir().join(format!("sbx-bench-build-{nonce}"));
    let mut tools = vec![s];
    if docker.is_ok() {
        let mut d = Tool::new(Side::Docker, DOCKER);
        d.meta_dir = Some(dir.join("docker-metadata"));
        tools.push(d);
    }
    let base_tag = format!("{REPO}/base:{nonce}");
    let plan = Plan {
        base_tag: &base_tag,
        nonce: &nonce,
        runs: o.runs,
        check: !o.dry_run,
    };
    let result = std::fs::create_dir_all(dir.join("docker-metadata"))
        .map_err(|e| format!("{}: {e}", dir.display()))
        .and_then(|()| measure_all(sh, &mut tools, o, &dir, &platform, &plan));
    let mut leftovers = Vec::new();
    for t in &mut tools {
        leftovers.extend(t.cleanup(sh));
    }
    if let Err(e) = std::fs::remove_dir_all(&dir)
        && dir.exists()
    {
        leftovers.push(format!("{}: {e}", dir.display()));
    }
    for l in &leftovers {
        eprintln!("bench build: could not remove: {l}");
    }
    let cases = result?;
    Ok(Report {
        host,
        sbx: info,
        docker,
        image: o.image.clone(),
        platform,
        runs: o.runs,
        cases,
        leftovers,
    })
}

/// 호스트 아키텍처(OCI 이름).
fn host_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
}

#[cfg(test)]
mod tests;
