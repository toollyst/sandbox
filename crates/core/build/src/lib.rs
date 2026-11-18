//! 빌드 코어: 빌드 계획(`sbx_model::build::BuildPlan`)을 실행해 이미지를 만든다(`sbx build`).
//!
//! 하는 일: 스테이지와 단계를 차례로 실행하고, 단계마다 캐시 키를 만들고(기획 4.4, [`key`]), 층을 쌓고, 이미지 config
//! (OCI image config, [`config`])를 조립해 저장소에 넘긴다. 진행은 [`Event`]로 알린다.
//!
//! 알면 안 되는 것: VM을 띄우는 방법과 캐시 디렉터리의 모양. 그것은 [`Store`], [`Machines`], [`Vm`] 뒤에 있고
//! 바이너리가 실행 코어와 관리 코어로 채운다. Dockerfile 문법(해석은 `sbx-dockerfile`이 끝냈다), 레지스트리, 압축.
//!
//! **규칙**
//!
//! - 스테이지마다 VM은 많아야 하나다. 처음으로 캐시에 없는 `RUN`을 만날 때 그때까지의 층을 뿌리로 띄운다. 그래서
//!   모든 단계가 캐시에서 오면 VM을 띄우지 않는다. 스테이지는 차례로 하나씩 만든다(동시에 만들지 않는다).
//! - `RUN`은 VM 안에서 새 층을 열고 실행한 뒤 그 층의 변경을 내보낸다. 환경은 그때의 config `Env`에 그 자리의 `ARG`를
//!   덮은 것, 작업 디렉터리와 사용자는 config의 것이다. 0이 아닌 코드면 출력의 끝과 VM이 거부한 연결 대상을 담아
//!   실패한다([`BuildError::RunFailed`]). 끝나면 그 단계가 남긴 프로세스도 끝난다(게스트가 한다).
//! - `COPY`, `ADD`, `WORKDIR`의 층은 호스트에서 tar로 만든다([`copy`], [`tarw`]). 대상의 모양(이미 있는 디렉터리,
//!   링크, `/etc/passwd`)은 지금까지의 층을 합친 트리로 본다. VM이 떠 있으면 같은 항목을 VM에도 놓는다. 그래서
//!   `FROM scratch`와 `COPY`뿐인 스테이지는 VM 없이 만든다.
//! - `RUN`, `COPY`, `ADD`, `WORKDIR`은 언제나 층 하나다(바뀐 것이 없으면 빈 층). BuildKit과 같다(Docker 29로 확인).
//!   나머지 단계는 config만 바꾼다.
//! - `ADD`의 원본이 압축 파일이나 tar이면 Docker는 푼다. sbx는 아직 풀지 않으므로 거부한다(조용히 복사하지 않는다).
//! - `COPY --from=이미지`, `FROM 이미지`는 저장소가 찾고 없으면 받는다.
//! - 캐시: 층을 만드는 단계는 키 → 층 기록을 남긴다([`Store::cache_put`]). `no_cache`면 기록을 읽지 않고(새로 만든다)
//!   새 기록은 남긴다(Docker와 같다).
//! - 만든 이미지의 ID는 config 문서의 sha256이다(manifest가 없다. 층 tar를 다시 만들어 manifest를 쓰는 일은 내보내기,
//!   `sbx-manage`의 `export`의 몫이다). 저장소에 두는 것(`type=sbx`)은 [`Request::store`]일 때만이다. 아니면 만든 것을
//!   돌려주기만 한다(`type=docker`, `type=oci`, `type=registry`만 고른 빌드).
//! - `SOURCE_DATE_EPOCH`([`Request::source_date_epoch`]): 이 빌드가 더한 history의 시각과 이미지의 `created`가 그 값이고,
//!   새 층의 파일 시각을 그 값으로 누른다(저장소가 한다, [`Store::add_layer`]). 캐시 키에도 들어간다([`key::epoch`]).
//!   그래서 같은 입력과 같은 캐시면 같은 이미지 ID다. `RUN`이 매번 다른 내용을 쓰면 다르다(sbx가 만드는 차이는 없다).
//!
//! - 의존: `sbx-model`, `sbx-rootfs`(합친 트리와 층 항목)

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use sbx_model::build::{BuildPlan, CopyKind, CopySource, Stage, StageBase, Step, StepKind};
use sbx_model::{Arch, ImageId, Layer};

pub mod config;
pub mod copy;
mod error;
pub mod key;
mod ports;
pub mod source;
pub mod tarw;
pub mod time;
pub mod users;

pub use error::BuildError;
pub use ports::{
    Base, Built, CacheRecord, Contents, Ignore, Machines, Open, OutputSink, PutEntry, PutKind,
    RunSpec, Store, Stream, Vm,
};

use config::{Image, go_plain, go_quoted};
use copy::{ItemKind, Placement};
use source::{Context, Src, SrcKind, TreeView};

/// 실패한 `RUN`에서 보여 줄 출력의 끝(바이트).
pub const OUTPUT_TAIL: usize = 16 << 10;

/// 빌드 요청.
pub struct Request {
    /// 실행할 계획.
    pub plan: BuildPlan,
    /// 빌드 컨텍스트(디렉터리).
    pub context: PathBuf,
    /// `.dockerignore`.
    pub ignore: Option<Arc<dyn Ignore>>,
    /// 캐시 기록을 읽지 않는다.
    pub no_cache: bool,
    /// 만들 이미지의 아키텍처.
    pub arch: Arch,
    /// `SOURCE_DATE_EPOCH`(Unix 초). 규칙은 크레이트 문서에 있다.
    pub source_date_epoch: Option<i64>,
    /// 만든 이미지를 저장소에 둔다([`Store::commit`], `type=sbx`).
    pub store: bool,
}

/// 빌드 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// 이미지 ID(config 문서의 sha256). 저장했으면 저장소의 ID다.
    pub id: ImageId,
    /// 저장소에 두었는지([`Request::store`]).
    pub stored: bool,
    /// 만든 이미지.
    pub image: Built,
    /// 띄운 빌드 VM 수.
    pub vms: usize,
    /// 캐시에서 온 단계 수.
    pub cached: usize,
}

/// 단계의 진행.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepStatus {
    /// 시작했다.
    Started,
    /// 캐시에서 왔다.
    Cached,
    /// 끝났다.
    Done {
        /// 걸린 시간(밀리초).
        millis: u64,
    },
}

/// 단계 하나의 진행 알림. `FROM`은 스테이지의 첫 단계(1번)다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepEvent {
    /// 스테이지 번호(계획 안, 0부터).
    pub stage: usize,
    /// 스테이지 이름(`AS`). 없으면 `None`.
    pub stage_name: Option<String>,
    /// 계획의 스테이지 수.
    pub stages: usize,
    /// 단계 번호(1부터, `FROM`이 1).
    pub index: usize,
    /// 스테이지의 단계 수(`FROM` 포함).
    pub steps: usize,
    /// Dockerfile의 줄.
    pub line: usize,
    /// 풀린 명령.
    pub text: String,
    /// 진행.
    pub status: StepStatus,
}

/// 진행 알림.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 단계의 진행.
    Step(StepEvent),
    /// `RUN`의 출력.
    Output {
        /// 흐름.
        stream: Stream,
        /// 바이트.
        data: Vec<u8>,
    },
}

/// 알림을 받는 쪽.
pub type Events = Arc<dyn Fn(Event) + Send + Sync>;

/// 끝난 스테이지.
#[derive(Clone)]
struct Finished {
    layers: Vec<Layer>,
    image: Image,
    key: String,
}

struct Builder<'a> {
    req: &'a Request,
    store: &'a dyn Store,
    machines: &'a dyn Machines,
    events: Events,
    context: Context,
    done: Vec<Option<Finished>>,
    views: BTreeMap<String, TreeView>,
    vms: usize,
    cached: usize,
    seq: usize,
    /// `SOURCE_DATE_EPOCH`의 RFC 3339 꼴.
    epoch: Option<String>,
}

/// 만드는 중인 스테이지.
struct Run {
    layers: Vec<Layer>,
    image: Image,
    key: String,
    cmd_set: bool,
    view: Option<TreeView>,
    applied: usize,
    vm: Option<Box<dyn Vm>>,
}

/// 계획을 실행해 이미지를 만들고 저장한다.
pub async fn build(
    req: &Request,
    store: &dyn Store,
    machines: &dyn Machines,
    events: Events,
) -> Result<Outcome, BuildError> {
    let mut b = Builder {
        req,
        store,
        machines,
        events,
        context: Context::new(req.context.clone(), req.ignore.clone()),
        done: vec![None; req.plan.stages.len()],
        views: BTreeMap::new(),
        vms: 0,
        cached: 0,
        seq: 0,
        epoch: req.source_date_epoch.map(time::rfc3339),
    };
    for (i, stage) in req.plan.stages.iter().enumerate() {
        let f = b.stage(i, stage).await?;
        b.done[i] = Some(f);
    }
    let mut last = b
        .done
        .pop()
        .flatten()
        .ok_or_else(|| BuildError::Backend("the build plan has no stages".into()))?;
    if let Some(e) = &b.epoch {
        last.image.set_created(e);
    }
    let diff_ids: Vec<String> = last.layers.iter().map(|l| l.diff_id.to_string()).collect();
    let image = Built {
        config_json: last.image.document(&diff_ids),
        config: last.image.image_config(),
        layers: last.layers,
        arch: req.arch,
    };
    let id = if req.store {
        store.commit(&image)?
    } else {
        ImageId::new(digest(&image.config_json))
    };
    Ok(Outcome {
        id,
        stored: req.store,
        image,
        vms: b.vms,
        cached: b.cached,
    })
}

/// sha256 digest.
fn digest(bytes: &[u8]) -> sbx_model::Digest {
    use sha2::Digest as _;
    let hex = source::hex(&sha2::Sha256::digest(bytes));
    sbx_model::Digest::parse(&format!("sha256:{hex}")).expect("a sha256 digest")
}

fn now_secs() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(_) => 0,
    }
}

/// 절대 경로로: 상대 경로면 작업 디렉터리에 잇는다. 끝의 `/`는 남긴다.
fn absolute(workdir: &str, path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("{}/{path}", workdir.trim_end_matches('/'))
    }
}

/// 압축 파일이나 tar로 보이는지(`ADD`가 풀 것인지). 첫 512바이트로 본다.
fn looks_like_archive(head: &[u8]) -> bool {
    let compressed = head.starts_with(&[0x1f, 0x8b])
        || head.starts_with(b"BZh")
        || head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0])
        || head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]);
    compressed || head.get(257..262) == Some(b"ustar")
}

impl Builder<'_> {
    /// 지금(Unix 초). `SOURCE_DATE_EPOCH`가 더 이르면 그것.
    fn now(&self) -> i64 {
        let now = now_secs();
        self.req.source_date_epoch.map_or(now, |e| now.min(e))
    }

    fn emit(&self, e: Event) {
        (self.events)(e)
    }

    fn step_event(
        &self,
        si: usize,
        stage: &Stage,
        index: usize,
        line: usize,
        text: &str,
        status: StepStatus,
    ) {
        self.emit(Event::Step(StepEvent {
            stage: si,
            stage_name: stage.name.clone(),
            stages: self.req.plan.stages.len(),
            index,
            steps: stage.steps.len() + 1,
            line,
            text: text.to_owned(),
            status,
        }));
    }

    /// 이미지 하나를 찾는다(없으면 저장소가 받는다).
    async fn base(&self, reference: &str) -> Result<(Base, Image), BuildError> {
        let base = self.store.image(reference).await?;
        let image = match &base.config {
            Some(doc) => Image::from_document(doc).map_err(|e| BuildError::Base {
                reference: reference.to_owned(),
                message: e.0,
            })?,
            None => Image::from_record(self.req.arch, &base.fallback),
        };
        Ok((base, image))
    }

    async fn stage(&mut self, si: usize, stage: &Stage) -> Result<Finished, BuildError> {
        let t = Instant::now();
        self.step_event(si, stage, 1, stage.line, &stage.text, StepStatus::Started);
        let (layers, image, key) = match &stage.base {
            StageBase::Image(r) => {
                let (base, image) = self.base(r).await?;
                (base.layers, image, key::image(&base.id.to_string()))
            }
            StageBase::Stage(i) => {
                let f = self.done.get(*i).cloned().flatten().ok_or_else(|| {
                    BuildError::Backend(format!("internal: stage {i} was not built"))
                })?;
                (f.layers, f.image, f.key)
            }
            StageBase::Scratch => (Vec::new(), Image::scratch(self.req.arch), key::scratch()),
        };
        let key = match self.req.source_date_epoch {
            Some(e) => key::epoch(&key, e),
            None => key,
        };
        self.step_event(
            si,
            stage,
            1,
            stage.line,
            &stage.text,
            StepStatus::Done {
                millis: t.elapsed().as_millis() as u64,
            },
        );
        let mut run = Run {
            layers,
            image,
            key,
            cmd_set: false,
            view: None,
            applied: 0,
            vm: None,
        };
        let result = self.steps(si, stage, &mut run).await;
        if let Some(vm) = run.vm.take() {
            let finished = vm.finish().await;
            if result.is_ok() {
                finished?;
            }
        }
        result?;
        Ok(Finished {
            layers: run.layers,
            image: run.image,
            key: run.key,
        })
    }

    async fn steps(&mut self, si: usize, stage: &Stage, run: &mut Run) -> Result<(), BuildError> {
        for (n, step) in stage.steps.iter().enumerate() {
            let index = n + 2;
            let t = Instant::now();
            self.step_event(si, stage, index, step.line, &step.text, StepStatus::Started);
            let cached = self.step(step, run).await?;
            let status = if cached {
                self.cached += 1;
                StepStatus::Cached
            } else {
                StepStatus::Done {
                    millis: t.elapsed().as_millis() as u64,
                }
            };
            self.step_event(si, stage, index, step.line, &step.text, status);
        }
        Ok(())
    }

    /// 단계 하나. 캐시에서 왔으면 `true`.
    async fn step(&mut self, step: &Step, run: &mut Run) -> Result<bool, BuildError> {
        let epoch = self.epoch.clone();
        let now = self.now();
        let img = &mut run.image;
        // config만 바꾸는 단계의 history. `SOURCE_DATE_EPOCH`면 그 시각이다.
        let record = |img: &mut Image, text: &str| img.record_layer(text, epoch.as_deref(), true);
        match &step.kind {
            StepKind::Env(pairs) => {
                img.set_env(pairs);
                let text: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                record(img, &format!("ENV {}", text.join(" ")));
            }
            StepKind::User(u) => {
                img.set("User", u.clone());
                record(img, &format!("USER {u}"));
            }
            StepKind::Label(pairs) => {
                img.add_labels(pairs);
                let text: Vec<String> = pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
                record(img, &format!("LABEL {}", text.join(" ")));
            }
            StepKind::Expose(ports) => {
                img.add_keys("ExposedPorts", ports);
                record(img, &format!("EXPOSE {}", go_plain(ports)));
            }
            StepKind::Volume(v) => {
                img.add_keys("Volumes", v);
                record(img, &format!("VOLUME {}", go_plain(v)));
            }
            StepKind::Entrypoint(c) => {
                let argv = c.argv();
                img.set_entrypoint(&argv, !run.cmd_set);
                record(img, &format!("ENTRYPOINT {}", go_quoted(&argv)));
            }
            StepKind::Cmd(c) => {
                let argv = c.argv();
                img.set_cmd(&argv);
                run.cmd_set = true;
                record(img, &format!("CMD {}", go_quoted(&argv)));
            }
            StepKind::StopSignal(s) => {
                img.set("StopSignal", s.clone());
                record(img, &format!("STOPSIGNAL {s}"));
            }
            StepKind::Shell(s) => {
                img.set_shell(s);
                record(img, &format!("SHELL {}", go_plain(s)));
            }
            StepKind::Healthcheck(h) => {
                img.set_healthcheck(h);
                record(img, &format!("HEALTHCHECK {}", go_quoted(&h.test)));
            }
            StepKind::Maintainer(m) => {
                img.set_author(m);
                record(img, &format!("MAINTAINER {m}"));
            }
            StepKind::Workdir(raw) => {
                // 만들 디렉터리는 정리한 경로다. config에는 Docker처럼 절대 경로면 쓴 그대로, 상대 경로면 앞의
                // 작업 디렉터리에 이어 정리한 것을 둔다(Docker 29.9.3: `WORKDIR /x/./y/../z/`는 그대로 남는다).
                let dir = format!(
                    "/{}",
                    source::clean(&absolute(&img.working_dir(), raw)).join("/")
                );
                let config = if raw.starts_with('/') {
                    raw.clone()
                } else {
                    dir.clone()
                };
                img.set("WorkingDir", config);
                let k = key::step(&run.key, &step.text, &[]);
                return self
                    .layer_step(step, run, k, step.text.clone(), |b, run| {
                        let user = run.image.user();
                        let owner = b
                            .with_root(run, |read| users::user(read, &user))?
                            .map_err(|m| BuildError::step(step, m))?;
                        let view = b.view(run)?;
                        copy::workdir(
                            &view.tree,
                            &dir,
                            copy::Options {
                                owner: Some(owner),
                                chmod: None,
                                now,
                            },
                        )
                        .map_err(|m| BuildError::step(step, m))
                    })
                    .await;
            }
            StepKind::Run(r) => {
                let mut extra = Vec::new();
                for (k, v) in &r.args {
                    extra.push(format!("{k}={v}"));
                }
                let k = key::step(&run.key, &step.text, &extra);
                let argv = r.command.argv();
                let mut created_by = String::from("RUN ");
                if !r.args.is_empty() {
                    created_by.push_str(&format!("|{} {} ", r.args.len(), extra.join(" ")));
                }
                created_by.push_str(&argv.join(" "));
                created_by.push_str(" # buildkit");
                if let Some(hit) = self.cache(&k)? {
                    self.push(run, k, hit, &created_by);
                    return Ok(true);
                }
                let mut env = run.image.env();
                for (k, v) in &r.args {
                    match env.iter_mut().find(|(n, _)| n == k) {
                        Some(e) => e.1 = v.clone(),
                        None => env.push((k.clone(), v.clone())),
                    }
                }
                let spec = RunSpec {
                    args: argv,
                    env,
                    cwd: run.image.working_dir(),
                    user: run.image.user(),
                };
                self.run_step(step, run, spec, k, &created_by).await?;
                return Ok(false);
            }
            StepKind::Copy(c) => {
                let (srcs, extra) = self.sources(step, c).await?;
                let k = key::step(&run.key, &step.text, &[extra]);
                let mut created_by = step.text.clone();
                if let CopySource::Stage(_) | CopySource::Image(_) = c.from {
                    created_by = strip_from(&created_by);
                }
                created_by.push_str(" # buildkit");
                let dest = copy::dest(&run.image.working_dir(), &c.dest);
                let chown = c.chown.clone();
                let chmod = c.chmod;
                return self
                    .layer_step(step, run, k, created_by, |b, run| {
                        let owner = match &chown {
                            Some(spec) => Some(
                                b.with_root(run, |read| users::chown(read, spec))?
                                    .map_err(|m| BuildError::step(step, m))?,
                            ),
                            None => None,
                        };
                        let view = b.view(run)?;
                        copy::place(
                            &view.tree,
                            &srcs,
                            &dest,
                            copy::Options { owner, chmod, now },
                        )
                        .map_err(|m| BuildError::step(step, m))
                    })
                    .await;
            }
        }
        // config만 바꾼 단계: 키만 잇는다.
        run.key = key::step(&run.key, &step.text, &[]);
        Ok(false)
    }

    fn cache(&self, key: &str) -> Result<Option<CacheRecord>, BuildError> {
        if self.req.no_cache {
            return Ok(None);
        }
        self.store.cache_get(key)
    }

    fn push(&self, run: &mut Run, key: String, rec: CacheRecord, created_by: &str) {
        run.layers.push(rec.layer);
        let created = self.epoch.clone().unwrap_or(rec.created);
        run.image.record(created_by, Some(&created));
        run.key = key;
    }

    fn tar_path(&mut self) -> PathBuf {
        self.seq += 1;
        self.store
            .work_dir()
            .join(format!("layer-{}-{}.tar", std::process::id(), self.seq))
    }

    /// 새 층을 저장하고 캐시 기록을 남긴다.
    fn store_layer(&self, tar: &std::path::Path, key: &str) -> Result<CacheRecord, BuildError> {
        let added = self.store.add_layer(tar, self.req.source_date_epoch);
        let _ = std::fs::remove_file(tar);
        let rec = CacheRecord {
            layer: added?,
            created: time::rfc3339(now_secs()),
        };
        self.store.cache_put(key, &rec)?;
        Ok(rec)
    }

    /// 호스트에서 만드는 층(`COPY`, `ADD`, `WORKDIR`). `plan`은 캐시에 없을 때만 부른다.
    async fn layer_step(
        &mut self,
        step: &Step,
        run: &mut Run,
        key: String,
        created_by: String,
        plan: impl FnOnce(&mut Self, &mut Run) -> Result<Placement, BuildError>,
    ) -> Result<bool, BuildError> {
        let hit = self.cache(&key)?;
        if let Some(hit) = &hit
            && run.vm.is_none()
        {
            self.push(run, key, hit.clone(), &created_by);
            return Ok(true);
        }
        let items = plan(self, run)?;
        if let Some(hit) = hit {
            // VM이 떠 있으면 캐시의 층과 같은 항목을 VM에도 놓는다(뒤의 RUN이 본다).
            let vm = run.vm.as_mut().expect("checked above");
            vm.put(put_entries(&items).map_err(|m| BuildError::step(step, m))?)
                .await?;
            self.push(run, key, hit, &created_by);
            return Ok(true);
        }
        let tar = self.tar_path();
        {
            let f = std::fs::File::create(&tar)?;
            let mut w = std::io::BufWriter::new(f);
            tarw::write(&items, &mut w).map_err(|e| BuildError::step(step, e.to_string()))?;
            use std::io::Write as _;
            w.flush()?;
        }
        let rec = self.store_layer(&tar, &key)?;
        if let Some(vm) = run.vm.as_mut() {
            vm.put(put_entries(&items).map_err(|m| BuildError::step(step, m))?)
                .await?;
        }
        self.push(run, key, rec, &created_by);
        Ok(false)
    }

    async fn run_step(
        &mut self,
        step: &Step,
        run: &mut Run,
        spec: RunSpec,
        key: String,
        created_by: &str,
    ) -> Result<(), BuildError> {
        if run.vm.is_none() {
            let vm = self.machines.start(&run.layers).await?;
            self.vms += 1;
            run.vm = Some(vm);
        }
        let tar = self.tar_path();
        let vm = run.vm.as_mut().expect("the VM was just started");
        let at = |e: BuildError| BuildError::step(step, e.to_string());
        vm.open_layer().await.map_err(at)?;
        let tail = Arc::new(Mutex::new(Vec::<u8>::new()));
        let events = self.events.clone();
        let t2 = tail.clone();
        let sink: OutputSink = Arc::new(move |stream, data: &[u8]| {
            events(Event::Output {
                stream,
                data: data.to_vec(),
            });
            let mut t = t2.lock().unwrap_or_else(|e| e.into_inner());
            t.extend_from_slice(data);
            if t.len() > OUTPUT_TAIL * 2 {
                let cut = t.len() - OUTPUT_TAIL;
                t.drain(..cut);
            }
        });
        let code = vm.run(spec, sink).await.map_err(at)?;
        if code != 0 {
            let blocked = vm.blocked().await;
            let t = tail.lock().unwrap_or_else(|e| e.into_inner());
            let start = t.len().saturating_sub(OUTPUT_TAIL);
            return Err(BuildError::RunFailed {
                line: step.line,
                text: step.text.clone(),
                code,
                output: String::from_utf8_lossy(&t[start..]).into_owned(),
                blocked,
            });
        }
        vm.export(&tar).await.map_err(at)?;
        let rec = self.store_layer(&tar, &key)?;
        self.push(run, key, rec, created_by);
        Ok(())
    }

    /// 지금 스테이지의 합친 트리. 아직 읽지 않은 층만 더 읽는다.
    fn view<'r>(&self, run: &'r mut Run) -> Result<&'r TreeView, BuildError> {
        let view = run.view.get_or_insert_with(TreeView::default);
        while run.applied < run.layers.len() {
            let i = run.applied;
            let (entries, contents) = self.store.read_layer(&run.layers[i], i)?;
            view.tree.apply_layer(entries)?;
            view.contents.insert(i, contents);
            run.applied += 1;
        }
        Ok(view)
    }

    /// 지금 뿌리의 파일을 읽는 함수로 `f`를 부른다.
    fn with_root<T>(
        &self,
        run: &mut Run,
        f: impl FnOnce(users::ReadFile<'_>) -> T,
    ) -> Result<T, BuildError> {
        let view = self.view(run)?;
        let mut read = |p: &str| view.read_file(p, 1 << 20);
        Ok(f(&mut read))
    }

    /// 층 목록의 트리(원본 스테이지·이미지). 같은 층 목록은 한 번만 읽는다.
    fn tree_of(&mut self, layers: &[Layer]) -> Result<TreeView, BuildError> {
        let id: Vec<&str> = layers.iter().map(|l| l.diff_id.as_str()).collect();
        let id = id.join(",");
        if let Some(v) = self.views.get(&id) {
            return Ok(v.clone());
        }
        let mut v = TreeView::default();
        for (i, l) in layers.iter().enumerate() {
            let (entries, contents) = self.store.read_layer(l, i)?;
            v.tree.apply_layer(entries)?;
            v.contents.insert(i, contents);
        }
        self.views.insert(id, v.clone());
        Ok(v)
    }

    /// `COPY`, `ADD`의 원본과 캐시 키 덧붙임.
    async fn sources(
        &mut self,
        step: &Step,
        c: &sbx_model::build::CopyStep,
    ) -> Result<(Vec<Src>, String), BuildError> {
        let mut srcs = Vec::new();
        let extra = match &c.from {
            CopySource::Context => {
                for s in &c.sources {
                    srcs.extend(
                        self.context
                            .find(s)
                            .map_err(|m| BuildError::step(step, m))?,
                    );
                }
                source::content_hash(&srcs).map_err(|e| BuildError::step(step, e.to_string()))?
            }
            CopySource::Stage(i) => {
                let f = self.done.get(*i).cloned().flatten().ok_or_else(|| {
                    BuildError::Backend(format!("internal: stage {i} was not built"))
                })?;
                let view = self.tree_of(&f.layers)?;
                for s in &c.sources {
                    srcs.extend(view.find(s).map_err(|m| BuildError::step(step, m))?);
                }
                format!("stage {}", f.key)
            }
            CopySource::Image(r) => {
                let (base, _) = self.base(r).await?;
                let view = self.tree_of(&base.layers)?;
                for s in &c.sources {
                    srcs.extend(view.find(s).map_err(|m| BuildError::step(step, m))?);
                }
                format!("image {}", base.id)
            }
        };
        if c.kind == CopyKind::Add {
            for s in &srcs {
                if let Some(e) = s.entries.first()
                    && let SrcKind::File { content, .. } = &e.kind
                {
                    let mut head = Vec::new();
                    use std::io::Read as _;
                    content
                        .open()
                        .and_then(|r| r.take(512).read_to_end(&mut head))
                        .map_err(|e| BuildError::step(step, e.to_string()))?;
                    if looks_like_archive(&head) {
                        return Err(BuildError::step(
                            step,
                            format!(
                                "{:?} looks like an archive, which ADD would extract; sbx build does not extract archives yet (use COPY and RUN tar -x)",
                                s.name
                            ),
                        ));
                    }
                }
            }
        }
        Ok((srcs, extra))
    }
}

/// `COPY --from=...`의 history는 `--from`을 뺀다(BuildKit과 같다).
fn strip_from(text: &str) -> String {
    text.split(' ')
        .filter(|w| !w.starts_with("--from="))
        .collect::<Vec<_>>()
        .join(" ")
}

/// VM에 놓을 항목: 새로 놓는 것만(이미 있는 조상은 뺀다). 장치와 FIFO는 VM에 놓지 못하므로 오류다.
fn put_entries(items: &Placement) -> Result<Vec<PutEntry>, String> {
    let mut out = Vec::new();
    for (path, i) in items.iter().filter(|(_, i)| !i.existing) {
        copy::check_path(path)?;
        let kind = match &i.kind {
            ItemKind::Dir => PutKind::Dir,
            ItemKind::File { size, content } => {
                let c = content.clone();
                PutKind::File {
                    size: *size,
                    open: Box::new(move || c.open().map(|r| r as Box<dyn std::io::Read>)),
                }
            }
            ItemKind::Symlink(t) => PutKind::Symlink(t.clone()),
            _ => {
                return Err(format!(
                    "/{path} is a device or FIFO, which sbx build cannot copy into a running build VM yet"
                ));
            }
        };
        out.push(PutEntry {
            path: path.clone(),
            kind,
            mode: i.mode,
            uid: i.uid,
            gid: i.gid,
            mtime: i.mtime,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
