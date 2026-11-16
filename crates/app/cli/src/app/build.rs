//! `build`: Dockerfile로 이미지를 만든다. 해석(`sbx-dockerfile`), 빌드 코어(`sbx-build`), 관리 코어(저장소), 실행
//! 코어(빌드 VM)를 잇는다. 규칙은 빌드 코어 문서와 사용자 안내서의 Build 절에 있다.
//!
//! - 계획은 두 번 푼다: 먼저 바탕 이미지의 `Env` 없이 풀어 `FROM` 이미지를 모으고, 그 이미지들을 찾거나 받은 뒤 그
//!   `Env`로 다시 푼다(`ENV PATH=/x:$PATH`).
//! - 빌드 VM은 그때까지의 층으로 쓴 뿌리 기록(`sbx_manage::BUILD_ROOT_SOURCE`)으로 띄운다. 기본 프로세스가 없고
//!   (`hold`), 라벨 [`BUILD_LABEL`]이 있다. 끝나면(실패해도) 지운다.
//! - 빌드 VM의 쓰기 디스크는 [`BUILD_SCRATCH_MIB`]다(희소 파일이라 쓴 만큼만 든다).
//! - 실패한 `RUN`: `--progress plain`이 아니면 그 출력의 끝을 보이고, VM이 거부한 이름마다 열 방법을 보인다
//!   (`blocked: NAME — add --allow NAME:PORT`). 포트는 짐작하지 않는다(거부된 것은 이름뿐이다).

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sbx_build::{
    Base, BuildError, Built, CacheRecord, Contents, Event, Machines, OutputSink, PutEntry, PutKind,
    RunSpec, StepStatus, Store, Stream, Vm,
};
use sbx_image::Platform;
use sbx_manage::Manager;
use sbx_model::build::{BuildOptions, BuildPlan, StageBase};
use sbx_model::{
    Arch, BoxFuture, ExecSpec, ImageId, Layer, NetworkPolicy, ProcessDefaults, Resources,
    RootfsFormat, SandboxSpec, Signal, StopSpec, UserSpec,
};
use sbx_oci::Reference;
use sbx_runtime::{IoMode, Placed, PlacedKind, Runtime};

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
        Ok(self
            .m
            .build_cache(key)
            .map_err(backend)?
            .map(|r| CacheRecord {
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

    fn add_layer(&self, tar: &Path) -> Result<Layer, BuildError> {
        let (layer, claim) = self.m.add_layer(tar).map_err(backend)?;
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
        if file.as_os_str() == "-" {
            return Err(msg("-f -: sbx build reads the Dockerfile from a file"));
        }
        let dockerfile =
            std::fs::read_to_string(&file).map_err(|e| msg(format!("{}: {e}", file.display())))?;
        // `<Dockerfile>.dockerignore`가 있으면 그것을, 없으면 컨텍스트의 `.dockerignore`(BuildKit과 같다).
        let mut ignore_file = file.clone().into_os_string();
        ignore_file.push(".dockerignore");
        let ignore_file = PathBuf::from(ignore_file);
        let ignore_file = if ignore_file.is_file() {
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
        let arch = host_arch()?;
        let options = BuildOptions {
            build_args: build_args(&a.build_args)?,
            target: a.target.clone(),
            arch,
        };
        let fname = file.display().to_string();
        let plan_err = |e: sbx_model::build::PlanError| msg(format!("{fname}: {e}"));
        let first = plan(&dockerfile, &options, &BTreeMap::new()).map_err(plan_err)?;
        let network = behavior::network_policy(&a.network, &a.allow).map_err(msg)?;
        let human = !self.json() && !a.quiet;
        if human {
            for w in behavior::proxy_only_warnings(&network) {
                eprintln!("warning: {w}");
            }
        }
        let chosen = self.choose(None)?;
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
            tags: tags.clone(),
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
        };
        let plain = a.progress == BuildProgress::Plain;
        let result = sbx_build::build(
            &req,
            &store,
            &machines,
            printer(self.json(), a.quiet, plain),
        )
        .await;
        drop(store);
        let _ = std::fs::remove_dir_all(&work);
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
                let mut m = format!(
                    "{fname}: line {line}: {text}: the command returned a non-zero code: {code}"
                );
                for b in &blocked {
                    m.push_str(&format!("\nblocked: {b} — add --allow {b}:PORT"));
                }
                return Err(failure(1, msg(m)));
            }
            Err(e @ BuildError::Step { .. }) => return Err(msg(format!("{fname}: {e}"))),
            Err(e) => return Err(msg(e.to_string())),
        };
        if self.json() {
            output::print_line(&output::BuildEvent::built(&out.id.to_string(), &tags))?;
        } else if a.quiet {
            println!("{}", out.id);
        } else {
            println!("Successfully built {}", short(&out.id));
            for t in &tags {
                println!("Successfully tagged {}", behavior::familiar(t));
            }
        }
        Ok(0)
    }
}
