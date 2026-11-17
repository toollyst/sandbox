//! `build`: Dockerfile로 이미지를 만든다. 해석(`sbx-dockerfile`), 빌드 코어(`sbx-build`), 관리 코어(저장소), 실행
//! 코어(빌드 VM)를 잇는다. 규칙은 빌드 코어 문서와 사용자 안내서의 Build 절에 있다.
//!
//! - 계획은 두 번 푼다: 먼저 바탕 이미지의 `Env` 없이 풀어 `FROM` 이미지를 모으고, 그 이미지들을 찾거나 받은 뒤 그
//!   `Env`로 다시 푼다(`ENV PATH=/x:$PATH`).
//! - 빌드 VM은 그때까지의 층으로 쓴 뿌리 기록(`sbx_manage::BUILD_ROOT_SOURCE`)으로 띄운다. 기본 프로세스가 없고
//!   (`hold`), 라벨 [`BUILD_LABEL`]이 있다. 끝나면(실패해도) 지운다.
//! - 빌드 VM의 쓰기 디스크는 [`BUILD_SCRATCH_MIB`]다(희소 파일이라 쓴 만큼만 든다).
//! - 실패한 `RUN`: `--progress plain`이 아니면 그 출력의 끝을 보이고, VM이 거부한 이름마다 열 방법을 보인다
//!   (`blocked: NAME — add --allow NAME:PORT`). 포트는 짐작하지 않는다(거부된 것은 이름뿐이다). 닫힌 네트워크
//!   (`--network none`, 기본)에서도 빌드 VM은 물은 이름을 거부로 기록한다(`SandboxSpec::record_names`). 그런 이름이
//!   있으면 네트워크가 닫혀 있다고 함께 알린다.
//! - 출력(`--output`, `--load`, `--push`): 규칙은 [`outputs`]. sbx 저장소가 아닌 출력은 만든 이미지를 내보낸다
//!   ([`Manager::export_built`]). 저장소에 두지 않아도 층과 캐시 기록은 남는다(다음 빌드가 쓴다).
//! - `SOURCE_DATE_EPOCH`: `--build-arg SOURCE_DATE_EPOCH=N`, 없으면 같은 이름의 환경 변수(그때는 빌드 인자로도
//!   넘긴다, buildx와 같다). 음이 아닌 정수(Unix 초)여야 한다. 빈 값은 없는 것이다. 쓰임은 빌드 코어 문서에 있다.
//! - 끝나면(실패해도) 빌드 캐시를 상한 밑으로 줄인다([`Manager::trim_build_cache`]). 상한은
//!   [`behavior::BUILD_CACHE_MAX_ENV`], 없으면 [`behavior::DEFAULT_BUILD_CACHE_MAX`].
//! - 저장하면(`type=sbx`) 이미지 기록에 어디서 만들었는지(`build_record`: 컨텍스트, Dockerfile, `--target`)를 남긴다.
//!   `images`가 보이고 `sbx tui`의 `R`이 그것으로 다시 빌드한다. 저장하지 않는 빌드는 남기지 않는다.

mod outputs;

use std::collections::BTreeMap;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sbx_build::{
    Base, BuildError, Built, CacheRecord, Contents, Event, Machines, OutputSink, PutEntry, PutKind,
    RunSpec, StepStatus, Store, Stream, Vm,
};
use sbx_image::Platform;
use sbx_manage::Manager;
use sbx_manage::export::Export;
use sbx_model::build::{BuildOptions, BuildPlan, StageBase};
use sbx_model::{
    Arch, BoxFuture, ExecSpec, ImageId, Layer, NetworkPolicy, ProcessDefaults, Resources,
    RootfsFormat, SandboxSpec, Signal, StopSpec, UserSpec,
};
use sbx_oci::Reference;
use sbx_runtime::{IoMode, Placed, PlacedKind, Runtime};
use sbx_store::BuildRecord;

use self::outputs::{Output, Target};
use super::{AppResult, Ctx, failure, host_arch, msg, short};
use crate::behavior;
use crate::cli::{BuildArgs, BuildProgress};
use crate::output;

/// 빌드 VM 샌드박스의 라벨.
pub const BUILD_LABEL: &str = "io.github.toollyst.sandbox.build";

/// 빌드 VM의 쓰기 디스크 크기(MiB).
pub const BUILD_SCRATCH_MIB: u32 = 16 * 1024;

/// 내보내는 층 하나의 파일 내용 합계 상한.
const LAYER_LIMIT: u64 = 1 << 40;

fn backend(e: impl std::fmt::Display) -> BuildError {
    BuildError::backend(e)
}

/// 어디서 만들었는지(다시 빌드하려고): 컨텍스트와 Dockerfile의 절대 경로(심볼릭 링크는 풀지 않는다: 준 그대로),
/// `--target`. Dockerfile을 표준 입력에서 읽었으면 `file`이 없다. 경로가 UTF-8이 아니거나 절대 경로로 만들 수 없으면
/// 기록하지 않는다(빌드는 그대로 한다).
fn build_record(context: &Path, file: Option<&Path>, target: Option<&str>) -> Option<BuildRecord> {
    let text = |p: &Path| Some(std::path::absolute(p).ok()?.to_str()?.to_owned());
    Some(BuildRecord {
        context: text(context)?,
        file: match file {
            Some(f) => Some(text(f)?),
            None => None,
        },
        target: target.map(str::to_owned),
    })
}

/// `.dockerignore`.
struct Ignore(sbx_dockerfile::DockerIgnore);

impl sbx_build::Ignore for Ignore {
    fn is_excluded(&self, path: &str) -> bool {
        self.0.is_excluded(path)
    }
    fn has_exceptions(&self) -> bool {
        self.0.has_exceptions()
    }
}

/// 층 내용(관리 코어의 작업 공간을 쥔다).
struct Held(sbx_manage::LayerContents);

impl Contents for Held {
    fn open(&self, c: &sbx_rootfs::ContentRef) -> io::Result<Box<dyn Read + Send>> {
        self.0.open_owned(c)
    }
}

/// 저장소 쪽: 관리 코어.
struct BuildStore<'a> {
    ctx: &'a Ctx,
    m: Arc<Manager>,
    arch: Arch,
    work: PathBuf,
    tags: Vec<String>,
    /// 어디서 만들었는지. 저장할 때(`type=sbx`) 이미지 기록에만 남는다.
    build: Option<BuildRecord>,
    /// 빌드가 쓰는 층을 끝날 때까지 쥔다(정리가 지우지 않게).
    claims: Mutex<Vec<sbx_manage::BlobClaim>>,
    quiet: bool,
}

impl BuildStore<'_> {
    fn hold(&self, c: sbx_manage::BlobClaim) {
        self.claims
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(c);
    }
}

impl Store for BuildStore<'_> {
    fn image<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Result<Base, BuildError>> {
        Box::pin(async move {
            let base = |message: String| BuildError::Base {
                reference: reference.to_owned(),
                message,
            };
            let found = self
                .ctx
                .lookup(&self.m, reference)
                .map_err(|e| base(e.to_string()))?;
            let id = match found {
                Some(f) => f.id().clone(),
                None => {
                    if !self.quiet {
                        eprintln!("pulling {reference}");
                    }
                    self.m
                        .pull(reference, Platform { arch: self.arch })
                        .await
                        .map_err(|e| base(e.to_string()))?
                        .record
                        .id
                }
            };
            let record = self.m.store().image(&id).map_err(backend)?;
            self.hold(self.m.claim_layers(&record.layers).map_err(backend)?);
            Ok(Base {
                config: self.m.config_document(&id).map_err(backend)?,
                id,
                layers: record.layers,
                fallback: record.config,
            })
        })
    }

    fn cache_get(&self, key: &str) -> Result<Option<CacheRecord>, BuildError> {
        let Some(r) = self.m.build_cache(key).map_err(backend)? else {
            return Ok(None);
        };
        // 쓰는 동안 다른 빌드의 캐시 줄이기나 정리가 지우지 않게 쥔다.
        self.hold(
            self.m
                .claim_layers(std::slice::from_ref(&r.layer))
                .map_err(backend)?,
        );
        Ok(Some(CacheRecord {
            layer: r.layer,
            created: r.created,
        }))
    }

    fn cache_put(&self, key: &str, record: &CacheRecord) -> Result<(), BuildError> {
        self.m
            .put_build_cache(
                key,
                &sbx_store::BuildCacheRecord {
                    layer: record.layer.clone(),
                    created: record.created.clone(),
                },
            )
            .map_err(backend)
    }

    fn add_layer(&self, tar: &Path, clamp: Option<i64>) -> Result<Layer, BuildError> {
        let (layer, claim) = self.m.add_layer(tar, clamp).map_err(backend)?;
        self.hold(claim);
        Ok(layer)
    }

    fn read_layer(
        &self,
        layer: &Layer,
        index: usize,
    ) -> Result<(Vec<sbx_rootfs::LayerEntry>, Arc<dyn Contents>), BuildError> {
        let (entries, contents) = self.m.layer_entries(layer, index).map_err(backend)?;
        Ok((entries, Arc::new(Held(contents))))
    }

    fn work_dir(&self) -> &Path {
        &self.work
    }

    fn commit(&self, image: &Built) -> Result<ImageId, BuildError> {
        self.m
            .commit_built(
                &image.config_json,
                image.config.clone(),
                image.layers.clone(),
                image.arch,
                &self.tags,
                self.build.clone(),
            )
            .map_err(backend)
    }
}

/// 빌드 VM을 띄우는 쪽: 뿌리 기록, prepare, 실행 코어.
struct BuildMachines {
    m: Arc<Manager>,
    rt: Runtime,
    arch: Arch,
    format: RootfsFormat,
    network: NetworkPolicy,
    resources: Resources,
    labels: BTreeMap<String, String>,
    data_dir: PathBuf,
}

impl Machines for BuildMachines {
    fn start<'a>(&'a self, layers: &'a [Layer]) -> BoxFuture<'a, Result<Box<dyn Vm>, BuildError>> {
        Box::pin(async move {
            let root = self.m.put_build_root(layers, self.arch).map_err(backend)?;
            let result = async {
                self.m
                    .prepare(&root, self.format, self.rt.layer_disks())
                    .await
                    .map_err(backend)?;
                let spec = SandboxSpec {
                    image: root.clone(),
                    rootfs_format: self.format,
                    process: ProcessDefaults {
                        args: Vec::new(),
                        env: Vec::new(),
                        cwd: "/".into(),
                        user: UserSpec::root(),
                    },
                    resources: self.resources,
                    mounts: Vec::new(),
                    volumes: Vec::new(),
                    network: self.network.clone(),
                    hostname: None,
                    labels: self.labels.clone(),
                    stop: StopSpec {
                        signal: Signal::TERM,
                        timeout_secs: Some(0),
                    },
                    open_stdin: false,
                    tty: false,
                    name: None,
                    lifetime_secs: None,
                    ports: Vec::new(),
                    egress_limit_bytes: None,
                    hold: true,
                    connect: Vec::new(),
                    connect_scope: Default::default(),
                    record_names: true,
                };
                let sb = self.rt.create(spec).await.map_err(backend)?;
                match sb.start(IoMode::default()).await {
                    Ok(main) => Ok((sb, main)),
                    Err(e) => {
                        let _ = sb.delete().await;
                        Err(backend(super::explain(e)))
                    }
                }
            }
            .await;
            match result {
                Ok((sb, main)) => Ok(Box::new(BuildVm {
                    sb: Some(sb),
                    _main: Some(main),
                    m: self.m.clone(),
                    root,
                    data_dir: self.data_dir.clone(),
                }) as Box<dyn Vm>),
                Err(e) => {
                    let _ = self.m.remove_build_root(&root);
                    Err(e)
                }
            }
        })
    }
}

struct BuildVm {
    sb: Option<sbx_runtime::Sandbox>,
    _main: Option<sbx_runtime::Process>,
    m: Arc<Manager>,
    root: ImageId,
    data_dir: PathBuf,
}

impl BuildVm {
    fn sb(&self) -> Result<&sbx_runtime::Sandbox, BuildError> {
        self.sb
            .as_ref()
            .ok_or_else(|| BuildError::Backend("the build VM is gone".into()))
    }
}

impl Vm for BuildVm {
    fn open_layer(&mut self) -> BoxFuture<'_, Result<(), BuildError>> {
        Box::pin(async move {
            self.sb()?.open_layer().await.map_err(backend)?;
            Ok(())
        })
    }

    fn run(&mut self, spec: RunSpec, output: OutputSink) -> BoxFuture<'_, Result<i32, BuildError>> {
        Box::pin(async move {
            let user = if spec.user.is_empty() {
                None
            } else {
                Some(
                    spec.user
                        .parse::<UserSpec>()
                        .map_err(|e| BuildError::Backend(format!("USER {}: {e}", spec.user)))?,
                )
            };
            let mut p = self
                .sb()?
                .exec_step(ExecSpec {
                    args: spec.args,
                    env: spec.env,
                    cwd: Some(spec.cwd),
                    user,
                    tty: false,
                })
                .await
                .map_err(backend)?;
            while let Some(o) = p.next().await {
                match o {
                    sbx_runtime::Output::Stdout(d) => output(Stream::Stdout, &d),
                    sbx_runtime::Output::Stderr(d) => output(Stream::Stderr, &d),
                }
            }
            p.wait().await.map_err(backend)
        })
    }

    fn export<'a>(&'a mut self, tar: &'a Path) -> BoxFuture<'a, Result<(), BuildError>> {
        Box::pin(async move {
            let f = std::fs::File::create(tar)?;
            let (f, _) = self
                .sb()?
                .export_layer(io::BufWriter::new(f), LAYER_LIMIT)
                .await
                .map_err(backend)?;
            f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
            Ok(())
        })
    }

    fn put(&mut self, entries: Vec<PutEntry>) -> BoxFuture<'_, Result<(), BuildError>> {
        Box::pin(async move {
            let placed = entries
                .into_iter()
                .map(|e| Placed {
                    path: e.path,
                    kind: match e.kind {
                        PutKind::Dir => PlacedKind::Dir,
                        PutKind::File { size, open } => PlacedKind::File { size, open },
                        PutKind::Symlink(t) => PlacedKind::Symlink(t),
                    },
                    mode: e.mode,
                    uid: e.uid,
                    gid: e.gid,
                    mtime: e.mtime,
                })
                .collect();
            self.sb()?.copy_entries("/", placed).await.map_err(backend)
        })
    }

    fn blocked(&mut self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(async move {
            let Some(sb) = &self.sb else {
                return Vec::new();
            };
            refused(&self.data_dir, sb.id().as_str())
        })
    }

    fn finish(mut self: Box<Self>) -> BoxFuture<'static, Result<(), BuildError>> {
        Box::pin(async move {
            let sb = self.sb.take();
            let result = match sb {
                Some(sb) => {
                    let stopped = sb.stop(None, Duration::ZERO).await;
                    let deleted = sb.delete().await;
                    stopped.and(deleted).map_err(backend)
                }
                None => Ok(()),
            };
            drop(self._main.take());
            let _ = self.m.remove_build_root(&self.root);
            result
        })
    }
}

/// 샌드박스 `id`의 네트워크 기록에서 거부된 이름(이름으로 거부된 것, 중복 없이 처음 차례로).
fn refused(data_dir: &Path, id: &str) -> Vec<String> {
    let log = sbx_runtime::network_log(data_dir);
    let Ok(mut r) = log.reader(sbx_events::Start::Beginning) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    while let Ok(Some(e)) = r.next_event() {
        if e.id != id || e.action != "refuse" {
            continue;
        }
        if let Some(h) = e.attributes.get("host")
            && !out.contains(h)
        {
            out.push(h.clone());
        }
    }
    out
}

/// `--build-arg`: `K=V`, 또는 `K`(환경 변수의 값, 없으면 넘기지 않는다. Docker와 같다).
fn build_args(given: &[String]) -> AppResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    for a in given {
        match a.split_once('=') {
            Some((k, v)) if !k.is_empty() => out.push((k.to_owned(), v.to_owned())),
            Some(_) => return Err(msg(format!("--build-arg {a:?}: the name is empty"))),
            None => {
                if let Ok(v) = std::env::var(a) {
                    out.push((a.clone(), v));
                }
            }
        }
    }
    Ok(out)
}

/// `SOURCE_DATE_EPOCH`(모듈 문서): `args`(풀어 둔 `--build-arg`)에 있으면 그것, 없으면 `env`(그때는 `args`에도
/// 더한다).
fn source_date_epoch(
    args: &mut Vec<(String, String)>,
    env: Option<String>,
) -> AppResult<Option<i64>> {
    const NAME: &str = "SOURCE_DATE_EPOCH";
    let value = match args.iter().rev().find(|(k, _)| k == NAME) {
        Some((_, v)) => v.clone(),
        None => match env {
            Some(v) if !v.is_empty() => {
                args.push((NAME.to_owned(), v.clone()));
                v
            }
            _ => return Ok(None),
        },
    };
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<i64>() {
        Ok(n) if n >= 0 => Ok(Some(n)),
        _ => Err(msg(format!(
            "{NAME}={value}: expected a Unix time in seconds (a non-negative integer)"
        ))),
    }
}

/// 빌드 캐시 상한(모듈 문서). `None`이면 상한이 없다.
fn build_cache_max(env: Option<&str>) -> Result<Option<u64>, String> {
    match env.map(str::trim) {
        None | Some("") => Ok(Some(behavior::DEFAULT_BUILD_CACHE_MAX)),
        Some(v) if v.eq_ignore_ascii_case("none") => Ok(None),
        Some(v) => crate::parse::size_or_zero(v)
            .map(Some)
            .map_err(|e| format!("{}={v}: {e}", behavior::BUILD_CACHE_MAX_ENV)),
    }
}

/// 실패한 `RUN`의 오류 끝에 붙일 줄들: 거부된 이름마다 열 방법, 닫힌 네트워크였으면 그 사실.
fn blocked_lines(blocked: &[String], closed: bool) -> String {
    let mut m = String::new();
    let open = if closed { "--network allow " } else { "" };
    for b in blocked {
        m.push_str(&format!("\nblocked: {b} — add {open}--allow {b}:PORT"));
    }
    if closed && !blocked.is_empty() {
        m.push_str(
            "\nthe build network is closed (--network none, the default): RUN steps reach only the hosts \
             given with --network allow --allow HOST:PORT",
        );
    }
    m
}

/// 만든 이미지를 `docker load`에 흘린다. docker의 출력은 stderr로 보낸다(`quiet`면 버린다).
fn docker_load(m: &Manager, e: &Export, names: &[String], quiet: bool) -> AppResult<()> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("docker")
        .arg("load")
        .stdin(Stdio::piped())
        .stdout(if quiet {
            Stdio::null()
        } else {
            Stdio::from(io::stderr())
        })
        .spawn()
        .map_err(|e| msg(format!("docker load: {e}")))?;
    let stdin = child.stdin.take().expect("stdin is piped");
    let mut w = io::BufWriter::new(stdin);
    let wrote: AppResult<()> = m
        .write_archive(e, names, &mut w)
        .map_err(Into::into)
        .and_then(|()| w.flush().map_err(Into::into));
    drop(w);
    let status = child.wait().map_err(|e| msg(format!("docker load: {e}")))?;
    if !status.success() {
        return Err(msg(format!("docker load failed ({status})")));
    }
    wrote
}

/// 단계의 머리: `[2/7]`, 여러 스테이지면 `[build 2/7]`(이름이 없으면 `stage-N`).
fn step_head(s: &sbx_build::StepEvent) -> String {
    if s.stages > 1 {
        let name = s
            .stage_name
            .clone()
            .unwrap_or_else(|| format!("stage-{}", s.stage));
        format!("[{name} {}/{}]", s.index, s.steps)
    } else {
        format!("[{}/{}]", s.index, s.steps)
    }
}

fn stream_name(s: Stream) -> &'static str {
    match s {
        Stream::Stdout => "stdout",
        Stream::Stderr => "stderr",
    }
}

/// 진행 알림을 사람이나 스크립트에게 보인다.
fn printer(json: bool, quiet: bool, plain: bool) -> sbx_build::Events {
    Arc::new(move |e: Event| {
        if json {
            let line = match &e {
                Event::Step(s) => output::BuildEvent::step(
                    s.stage_name.clone(),
                    s.stage,
                    s.index,
                    s.steps,
                    s.line,
                    s.text.clone(),
                    &s.status,
                ),
                Event::Output { stream, data } => {
                    output::BuildEvent::output(stream_name(*stream), data)
                }
            };
            let _ = output::print_line(&line);
            return;
        }
        if quiet {
            return;
        }
        match e {
            Event::Step(s) => {
                let head = step_head(&s);
                match s.status {
                    StepStatus::Started => eprintln!("{head} {}", s.text),
                    StepStatus::Cached => eprintln!("{head} CACHED"),
                    StepStatus::Done { millis } => {
                        eprintln!("{head} DONE {:.1}s", millis as f64 / 1000.0)
                    }
                }
            }
            Event::Output { data, .. } if plain => {
                use std::io::Write as _;
                let mut err = io::stderr().lock();
                let _ = err.write_all(&data);
                let _ = err.flush();
            }
            Event::Output { .. } => {}
        }
    })
}

/// 빌드가 끝난 뒤에 쓰는 것들([`Ctx::finish_build`]).
struct Done<'a> {
    /// 오류에 보일 Dockerfile 이름.
    fname: &'a str,
    outputs: &'a [Output],
    /// `-t`(정규화한 것).
    tags: &'a [String],
    /// 저장소에 붙인 이름들(`-t`와 `type=sbx,name=`).
    store_tags: &'a [String],
    /// 빌드 네트워크가 닫혀 있었는지.
    closed: bool,
}

/// 계획을 푼다. `envs`는 바탕 이미지 참조 → config `Env`.
fn plan(
    dockerfile: &str,
    options: &BuildOptions,
    envs: &BTreeMap<String, Vec<(String, String)>>,
) -> Result<BuildPlan, sbx_model::build::PlanError> {
    let mut env = |r: &str| Ok(envs.get(r).cloned().unwrap_or_default());
    sbx_dockerfile::plan(dockerfile, options, &mut env)
}

impl Ctx {
    pub(super) async fn build(&self, a: &BuildArgs) -> AppResult<u8> {
        let context = a.path.as_path();
        if a.path.as_os_str() == "-" || a.path.to_string_lossy().contains("://") {
            return Err(msg(format!(
                "{}: sbx build takes a local directory as the build context",
                a.path.display()
            )));
        }
        if !context.is_dir() {
            return Err(msg(format!(
                "{}: the build context is not a directory",
                context.display()
            )));
        }
        let file = a.file.clone().unwrap_or_else(|| context.join("Dockerfile"));
        // `-f -`: Dockerfile을 표준 입력에서 읽는다(컨텍스트는 그대로 디렉터리다).
        let stdin_file = file.as_os_str() == "-";
        let dockerfile = if stdin_file {
            let mut text = String::new();
            io::stdin()
                .lock()
                .read_to_string(&mut text)
                .map_err(|e| msg(format!("-f -: {e}")))?;
            text
        } else {
            std::fs::read_to_string(&file).map_err(|e| msg(format!("{}: {e}", file.display())))?
        };
        // `<Dockerfile>.dockerignore`가 있으면 그것을, 없으면 컨텍스트의 `.dockerignore`(BuildKit과 같다).
        let mut ignore_file = file.clone().into_os_string();
        ignore_file.push(".dockerignore");
        let ignore_file = PathBuf::from(ignore_file);
        let ignore_file = if !stdin_file && ignore_file.is_file() {
            ignore_file
        } else {
            context.join(".dockerignore")
        };
        let ignore = match std::fs::read_to_string(&ignore_file) {
            Ok(text) => Some(Arc::new(Ignore(
                sbx_dockerfile::DockerIgnore::parse(&text)
                    .map_err(|e| msg(format!("{}: {e}", ignore_file.display())))?,
            )) as Arc<dyn sbx_build::Ignore>),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(msg(format!("{}: {e}", ignore_file.display()))),
        };
        let mut tags = Vec::new();
        for t in &a.tags {
            let r = Reference::parse(t).map_err(|e| msg(format!("-t {t}: {e}")))?;
            if r.digest.is_some() {
                return Err(msg(format!("-t {t}: a tag cannot have a digest")));
            }
            tags.push(r.to_string());
        }
        let outputs =
            outputs::parse(&a.outputs, a.load, a.push, &tags, &|p| p.is_dir()).map_err(msg)?;
        let to_stdout = outputs.iter().any(|o| o.target == Target::Archive(None));
        if to_stdout && io::stdout().is_terminal() {
            return Err(msg(
                "refusing to write a tar to the terminal; give dest=FILE or redirect",
            ));
        }
        if to_stdout && self.json() {
            return Err(msg(
                "--format json writes to stdout, where the tar goes (dest=-); give dest=FILE",
            ));
        }
        if outputs.iter().any(|o| o.target == Target::DockerLoad)
            && !crate::choose::on_path(Path::new("docker"))
        {
            return Err(msg(
                "--output type=docker (--load) streams the image into `docker load`, but docker is not on PATH; \
                 write a tar instead with --output type=docker,dest=FILE and load it where docker is",
            ));
        }
        let store_it = outputs.iter().any(|o| o.target == Target::Store);
        // 저장소의 이름: `-t`와 `type=sbx,name=`.
        let mut store_tags = tags.clone();
        for o in outputs.iter().filter(|o| o.target == Target::Store) {
            for n in &o.names {
                if !store_tags.contains(n) {
                    store_tags.push(n.clone());
                }
            }
        }
        let arch = host_arch()?;
        let mut args = build_args(&a.build_args)?;
        let epoch = source_date_epoch(&mut args, std::env::var("SOURCE_DATE_EPOCH").ok())?;
        let options = BuildOptions {
            build_args: args,
            target: a.target.clone(),
            arch,
        };
        let fname = if stdin_file {
            "Dockerfile (stdin)".to_owned()
        } else {
            file.display().to_string()
        };
        let plan_err = |e: sbx_model::build::PlanError| msg(format!("{fname}: {e}"));
        let first = plan(&dockerfile, &options, &BTreeMap::new()).map_err(plan_err)?;
        let network = behavior::network_policy(&a.network, &a.allow).map_err(msg)?;
        let closed = matches!(network, NetworkPolicy::Disabled);
        let human = !self.json() && !a.quiet;
        if human {
            for w in behavior::proxy_only_warnings(&network) {
                eprintln!("warning: {w}");
            }
        }
        let chosen = self.choose(None).await?;
        let rt = self.launcher_with(&chosen)?;
        let m = Arc::new(self.manager()?);
        let work = self.cache_dir.join("tmp").join(format!(
            ".build-{}-{}",
            std::process::id(),
            super::now_ns()
        ));
        std::fs::create_dir_all(&work)?;
        let store = BuildStore {
            ctx: self,
            m: m.clone(),
            arch,
            work: work.clone(),
            tags: store_tags.clone(),
            build: build_record(
                context,
                (!stdin_file).then_some(file.as_path()),
                a.target.as_deref(),
            ),
            claims: Mutex::default(),
            quiet: !human,
        };
        // 바탕 이미지의 Env를 모아 다시 푼다.
        let mut envs = BTreeMap::new();
        for s in &first.stages {
            if let StageBase::Image(r) = &s.base
                && !envs.contains_key(r)
            {
                let base = store
                    .image(r)
                    .await
                    .map_err(|e| msg(format!("{fname}: line {}: {e}", s.line)))?;
                let env = match &base.config {
                    Some(doc) => sbx_build::config::Image::from_document(doc)
                        .map(|i| i.env())
                        .unwrap_or_default(),
                    None => base.fallback.env.clone(),
                };
                envs.insert(r.clone(), env);
            }
        }
        let plan = plan(&dockerfile, &options, &envs).map_err(plan_err)?;
        if human {
            for w in &plan.warnings {
                eprintln!("warning: {fname}: {w}");
            }
        }
        let mut labels = BTreeMap::from([(BUILD_LABEL.to_owned(), "1".to_owned())]);
        if let Some(pin) = chosen.pin() {
            labels.insert(crate::choose::PIN_LABEL.to_owned(), pin);
        }
        let machines = BuildMachines {
            m: m.clone(),
            rt,
            arch,
            format: crate::behavior::DEFAULT_ROOTFS_FORMAT,
            network,
            resources: Resources {
                vcpus: a
                    .cpus
                    .as_deref()
                    .map(crate::parse::cpus)
                    .transpose()?
                    .unwrap_or(behavior::DEFAULT_VCPUS),
                memory_mib: a
                    .memory
                    .as_deref()
                    .map(crate::parse::memory_mib)
                    .transpose()?
                    .unwrap_or(behavior::DEFAULT_MEMORY_MIB),
                scratch_disk_mib: BUILD_SCRATCH_MIB,
            },
            labels,
            data_dir: self.data_dir.clone(),
        };
        let req = sbx_build::Request {
            plan,
            context: context.to_owned(),
            ignore,
            no_cache: a.no_cache,
            arch,
            source_date_epoch: epoch,
            store: store_it,
        };
        let plain = a.progress == BuildProgress::Plain;
        let result = sbx_build::build(
            &req,
            &store,
            &machines,
            printer(self.json(), a.quiet, plain),
        )
        .await;
        // 내보낼 것이 있으면 빌드가 쥔 층을 놓기 전에 내보내기를 준비한다(그것이 층을 쥔다).
        let export = match &result {
            Ok(o) if outputs.iter().any(|x| x.target != Target::Store) => Some(
                m.export_built(&o.image.config_json, &o.image.layers)
                    .map_err(|e| msg(e.to_string())),
            ),
            _ => None,
        };
        drop(store);
        let _ = std::fs::remove_dir_all(&work);
        let done = Done {
            fname: &fname,
            outputs: &outputs,
            tags: &tags,
            store_tags: &store_tags,
            closed,
        };
        let r = self
            .finish_build(a, &m, &done, result, export.transpose())
            .await;
        self.trim_build_cache(&m, human);
        r
    }

    /// 빌드 결과를 알리고 출력에 내보낸다.
    async fn finish_build(
        &self,
        a: &BuildArgs,
        m: &Manager,
        d: &Done<'_>,
        result: Result<sbx_build::Outcome, BuildError>,
        export: AppResult<Option<Export>>,
    ) -> AppResult<u8> {
        let human = !self.json() && !a.quiet;
        let plain = a.progress == BuildProgress::Plain;
        let out = match result {
            Ok(out) => out,
            Err(BuildError::RunFailed {
                line,
                text,
                code,
                output,
                blocked,
            }) => {
                if human && !plain && !output.is_empty() {
                    eprint!("{output}");
                    if !output.ends_with('\n') {
                        eprintln!();
                    }
                }
                let m = format!(
                    "{}: line {line}: {text}: the command returned a non-zero code: {code}{}",
                    d.fname,
                    blocked_lines(&blocked, d.closed)
                );
                return Err(failure(1, msg(m)));
            }
            Err(e @ BuildError::Step { .. }) => return Err(msg(format!("{}: {e}", d.fname))),
            Err(e) => return Err(msg(e.to_string())),
        };
        let export = export?;
        // tar가 stdout으로 가면 사람에게 하는 말은 stderr로.
        let to_stdout = d.outputs.iter().any(|o| o.target == Target::Archive(None));
        let say = |line: &str| {
            if to_stdout {
                eprintln!("{line}");
            } else {
                println!("{line}");
            }
        };
        let id = out.id.to_string();
        if self.json() {
            output::print_line(&output::BuildEvent::built(&id, d.store_tags, out.stored))?;
        } else if a.quiet {
            say(&id);
        } else if out.stored {
            say(&format!("Successfully built {}", short(&out.id)));
            for t in d.store_tags {
                say(&format!("Successfully tagged {}", behavior::familiar(t)));
            }
        }
        let Some(e) = export else {
            return Ok(0);
        };
        let exported = |o: &Output, dest: &str| -> AppResult<()> {
            if self.json() {
                output::print_line(&output::BuildEvent::exported(
                    o.kind,
                    dest,
                    &e.id.to_string(),
                ))?;
            } else if human {
                eprintln!("Exported {} to {dest}", e.id);
            }
            Ok(())
        };
        for o in d.outputs {
            let names = o.names_or(d.tags);
            match &o.target {
                Target::Store => {}
                Target::DockerLoad => {
                    docker_load(m, &e, names, !human)?;
                    exported(o, "docker")?;
                }
                Target::Archive(None) => {
                    let mut w = io::BufWriter::new(io::stdout().lock());
                    m.write_archive(&e, names, &mut w)?;
                    w.flush()?;
                    exported(o, "-")?;
                }
                Target::Archive(Some(path)) => {
                    super::load::write_file(path, &mut |w| {
                        m.write_archive(&e, names, w)?;
                        Ok(())
                    })?;
                    exported(o, &path.display().to_string())?;
                }
                Target::Layout(dir) => {
                    m.write_layout(&e, names, dir)?;
                    exported(o, &dir.display().to_string())?;
                }
                Target::Registry => self.push_built(m, &e, o, names, a.quiet, &say).await?,
            }
        }
        Ok(0)
    }

    /// 만든 이미지를 이름마다 올린다(`push`와 같은 길, 같은 줄).
    async fn push_built(
        &self,
        m: &Manager,
        e: &Export,
        o: &Output,
        names: &[String],
        quiet: bool,
        say: &(dyn Fn(&str) + Sync),
    ) -> AppResult<()> {
        let dest = sbx_oci::OciImageSource::with_auth(crate::dirs::registry_auth())?
            .with_registries(crate::dirs::registries_conf());
        let human = !self.json() && !quiet;
        for reference in names {
            if human {
                let r = Reference::parse(reference).map_err(|e| msg(e.to_string()))?;
                say(&format!(
                    "The push refers to repository [{}/{}]",
                    r.registry, r.repository
                ));
            }
            let status = if quiet {
                None
            } else {
                Some(crate::progress::Status::new())
            };
            let shown = behavior::familiar(reference);
            let p = m
                .push_export(&dest, e, reference, &|p| {
                    if let Some(s) = &status {
                        s.push(shown, p)
                    }
                })
                .await;
            drop(status);
            let p = p?;
            if self.json() {
                output::print_line(&output::BuildEvent::exported(
                    o.kind,
                    &p.reference,
                    &p.id.to_string(),
                ))?;
            } else if human {
                super::push::report(&p).iter().for_each(|l| say(l));
            }
        }
        Ok(())
    }

    /// 빌드 캐시를 상한 밑으로 줄인다(모듈 문서). 실패는 경고일 뿐이다(빌드는 끝났다).
    fn trim_build_cache(&self, m: &Manager, human: bool) {
        let max =
            match build_cache_max(std::env::var(behavior::BUILD_CACHE_MAX_ENV).ok().as_deref()) {
                Ok(Some(max)) => max,
                Ok(None) => return,
                Err(e) => {
                    eprintln!("warning: {e}");
                    return;
                }
            };
        match m.trim_build_cache(max, &super::system::disk_usage) {
            Ok(t) if t.records > 0 && human => eprintln!(
                "build cache: removed {} old step(s) to stay under {} ({} -> {})",
                t.records,
                behavior::human_size(max),
                behavior::human_size(t.before),
                behavior::human_size(t.after),
            ),
            Ok(_) => {}
            Err(e) => eprintln!("warning: could not trim the build cache: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_date_epoch_comes_from_the_build_arg_then_the_environment() {
        let kv = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        let mut args = vec![kv("SOURCE_DATE_EPOCH", "100")];
        assert_eq!(
            source_date_epoch(&mut args, Some("200".into())).unwrap(),
            Some(100)
        );
        assert_eq!(args.len(), 1);
        // 환경 변수는 빌드 인자로도 넘긴다(buildx와 같다).
        let mut args = Vec::new();
        assert_eq!(
            source_date_epoch(&mut args, Some("200".into())).unwrap(),
            Some(200)
        );
        assert_eq!(args, [kv("SOURCE_DATE_EPOCH", "200")]);
        assert_eq!(source_date_epoch(&mut Vec::new(), None).unwrap(), None);
        assert_eq!(
            source_date_epoch(&mut Vec::new(), Some(String::new())).unwrap(),
            None
        );
        assert_eq!(
            source_date_epoch(&mut vec![kv("SOURCE_DATE_EPOCH", "")], None).unwrap(),
            None
        );
        for bad in ["-1", "soon", "1.5"] {
            let e = source_date_epoch(&mut vec![kv("SOURCE_DATE_EPOCH", bad)], None)
                .unwrap_err()
                .to_string();
            assert!(e.contains("Unix time"), "{e}");
        }
    }

    #[test]
    fn the_build_cache_cap_has_a_default_and_can_be_turned_off() {
        assert_eq!(
            build_cache_max(None).unwrap(),
            Some(behavior::DEFAULT_BUILD_CACHE_MAX)
        );
        assert_eq!(build_cache_max(Some("2g")).unwrap(), Some(2 << 30));
        assert_eq!(build_cache_max(Some("0")).unwrap(), Some(0));
        assert_eq!(build_cache_max(Some("none")).unwrap(), None);
        assert!(
            build_cache_max(Some("lots"))
                .unwrap_err()
                .contains(behavior::BUILD_CACHE_MAX_ENV)
        );
    }

    #[test]
    fn a_closed_network_says_how_to_open_it() {
        let names = vec!["deb.debian.org".to_owned()];
        let closed = blocked_lines(&names, true);
        assert!(
            closed.contains(
                "blocked: deb.debian.org — add --network allow --allow deb.debian.org:PORT"
            ),
            "{closed}"
        );
        assert!(closed.contains("the build network is closed"), "{closed}");
        let open = blocked_lines(&names, false);
        assert!(
            open.contains("blocked: deb.debian.org — add --allow deb.debian.org:PORT")
                && !open.contains("closed"),
            "{open}"
        );
        assert_eq!(blocked_lines(&[], true), "");
    }
}
