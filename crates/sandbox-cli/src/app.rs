//! 명령 처리. 구현체를 조립하고, 관리 코어와 실행 코어를 [`behavior`]의 규칙대로 차례로 부른다.
//!
//! 조립하는 구현체:
//!
//! | 자리 | 구현체 |
//! |---|---|
//! | 이미지 소스 | `sandbox-oci` (OCI 레지스트리) |
//! | rootfs writer | `sandbox-rootfs`의 erofs |
//! | 실행기 | `--executor firecracker`: `SBX_FIRECRACKER` 경로, 없으면 PATH의 `firecracker` |

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use executor_firecracker::FirecrackerExecutor;
use sandbox_executor::Executor;
use sandbox_image::Platform;
use sandbox_manage::Manager;
use sandbox_model::{
    Arch, ExecSpec, ImageId, Lifetime, NetworkPolicy, ProcessOverrides, Resources, RootfsFormat,
    SandboxId, SandboxSpec, State, UserSpec,
};
use sandbox_oci::{OciImageSource, Reference};
use sandbox_rootfs::erofs::ErofsBuilder;
use sandbox_runtime::{Runtime, RuntimeConfig, SandboxInfo};

use crate::behavior::{self, ImageMatch, LookupError, RmiAction, RmiDecision, Step};
use crate::cli::{Cli, Command, ExecArgs, Format, RunArgs};
use crate::dirs;
use crate::parse::{self, CpEndpoint};

/// 명령 결과. 성공하면 종료 코드.
pub type AppResult<T> = Result<T, Box<dyn Error>>;

#[derive(Debug)]
struct Msg(String);

impl fmt::Display for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for Msg {}

fn msg(s: impl Into<String>) -> Box<dyn Error> {
    Box::new(Msg(s.into()))
}

fn short(id: &ImageId) -> &str {
    &id.digest().hex()[..12]
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}B")
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

fn host_arch() -> AppResult<Arch> {
    Arch::host().ok_or_else(|| msg("this host architecture is not supported"))
}

/// 명령 하나를 실행한다.
pub async fn run(cli: Cli) -> AppResult<u8> {
    let ctx = Ctx::new(&cli)?;
    match &cli.command {
        Command::Pull { platform, image } => ctx.pull(platform.as_deref(), image).await,
        Command::Prepare {
            rootfs_format,
            image,
        } => ctx.prepare(*rootfs_format, image).await,
        Command::Images => ctx.images(),
        Command::Rmi { force, images } => ctx.rmi(*force, images).await,
        Command::Run(a) => ctx.run_sandbox(a).await,
        Command::Create(a) => {
            let id = ctx.create(a).await?;
            println!("{id}");
            Ok(0)
        }
        Command::Start { id } => {
            let rt = ctx.runtime()?;
            rt.get(&SandboxId::new(id)?).await?.start().await?;
            println!("{id}");
            Ok(0)
        }
        Command::Exec(a) => ctx.exec(a).await,
        Command::Cp { src, dst } => ctx.cp(src, dst).await,
        Command::Stop { time, ids } => ctx.stop(*time, ids).await,
        Command::Rm { force, ids } => ctx.rm(*force, ids).await,
        Command::Ps { all } => ctx.ps(*all).await,
        Command::Inspect { id } => ctx.inspect(id).await,
    }
}

struct Ctx {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    executor: String,
    format: Format,
}

impl Ctx {
    fn new(cli: &Cli) -> AppResult<Self> {
        Ok(Self {
            data_dir: cli
                .data_dir
                .clone()
                .or_else(dirs::data_dir)
                .ok_or_else(|| msg("cannot determine a data directory; use --data-dir"))?,
            cache_dir: cli
                .cache_dir
                .clone()
                .or_else(dirs::cache_dir)
                .ok_or_else(|| msg("cannot determine a cache directory; use --cache-dir"))?,
            executor: cli.executor.clone(),
            format: cli.format,
        })
    }

    fn json(&self) -> bool {
        self.format == Format::Json
    }

    fn manager(&self) -> AppResult<Manager> {
        Ok(Manager::new(
            &self.cache_dir,
            Arc::new(OciImageSource::new()?),
            vec![Arc::new(ErofsBuilder)],
        )?)
    }

    fn executor(&self) -> AppResult<Arc<dyn Executor>> {
        match self.executor.as_str() {
            "firecracker" => {
                let bin = std::env::var_os("SBX_FIRECRACKER")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| "firecracker".into());
                Ok(Arc::new(FirecrackerExecutor::new(bin)?))
            }
            other => Err(msg(format!(
                "unknown executor {other:?} (available: firecracker)"
            ))),
        }
    }

    fn runtime(&self) -> AppResult<Runtime> {
        Ok(Runtime::new(
            RuntimeConfig {
                data_dir: self.data_dir.clone(),
                cache_dir: self.cache_dir.clone(),
            },
            self.executor()?,
        )?)
    }

    /// 이미지 이름을 찾는다. 없으면 `None`, 그 밖의 문제는 오류.
    fn lookup(&self, m: &Manager, name: &str) -> AppResult<Option<ImageMatch>> {
        let normalized = Reference::parse(name).ok().map(|r| r.to_string());
        let refs = m.store().refs()?;
        let ids: Vec<ImageId> = m.store().images()?.into_iter().map(|r| r.id).collect();
        match behavior::match_image(name, normalized.as_deref(), &refs, &ids) {
            Ok(found) => Ok(Some(found)),
            Err(LookupError::NotFound(_)) => Ok(None),
            Err(e) => Err(msg(e.to_string())),
        }
    }

    fn require(&self, m: &Manager, name: &str) -> AppResult<ImageMatch> {
        self.lookup(m, name)?
            .ok_or_else(|| msg(LookupError::NotFound(name.to_owned()).to_string()))
    }

    async fn pull(&self, platform: Option<&str>, image: &str) -> AppResult<u8> {
        let arch = match platform {
            Some(p) => parse::platform(p)?,
            None => host_arch()?,
        };
        let info = self.manager()?.pull(image, Platform { arch }).await?;
        if self.json() {
            println!(
                "{}",
                json!({"reference": info.record.reference, "id": info.record.id, "arch": info.record.arch})
            );
        } else {
            println!("{}: pulled {}", info.record.reference, info.record.id);
        }
        Ok(0)
    }

    async fn prepare(&self, format: RootfsFormat, image: &str) -> AppResult<u8> {
        let m = self.manager()?;
        let found = self.require(&m, image)?;
        let built = m.prepare(found.id(), format).await?;
        if self.json() {
            println!(
                "{}",
                json!({"id": found.id(), "format": format, "built": built})
            );
        } else if built {
            println!("{}: {format} rootfs prepared", short(found.id()));
        } else {
            println!("{}: {format} rootfs already prepared", short(found.id()));
        }
        Ok(0)
    }

    fn images(&self) -> AppResult<u8> {
        let images = self.manager()?.images()?;
        if self.json() {
            let list: Vec<_> = images
                .iter()
                .map(|i| {
                    json!({
                        "id": i.record.id,
                        "refs": i.refs,
                        "arch": i.record.arch,
                        "prepared": i.prepared,
                        "compressed_size": i.record.compressed_size(),
                        "ignored_fields": i.record.ignored_fields,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&list)?);
            return Ok(0);
        }
        println!(
            "{:<48} {:<14} {:<12} {:<10} SIZE",
            "REFERENCE", "IMAGE ID", "PLATFORM", "ROOTFS"
        );
        for i in &images {
            let rootfs: Vec<&str> = i.prepared.iter().map(|f| f.as_str()).collect();
            let rootfs = if rootfs.is_empty() {
                "-".to_owned()
            } else {
                rootfs.join(",")
            };
            let refs: Vec<&str> = if i.refs.is_empty() {
                vec!["<none>"]
            } else {
                i.refs.iter().map(String::as_str).collect()
            };
            for r in refs {
                println!(
                    "{:<48} {:<14} {:<12} {:<10} {}",
                    r,
                    short(&i.record.id),
                    format!("linux/{}", i.record.arch.oci_name()),
                    rootfs,
                    human_size(i.record.compressed_size())
                );
            }
        }
        Ok(0)
    }

    async fn rmi(&self, force: bool, names: &[String]) -> AppResult<u8> {
        let m = self.manager()?;
        let sandboxes = self.runtime()?.list().await?;
        let mut failed = 0;
        for name in names {
            match self.rmi_one(&m, &sandboxes, name, force) {
                Ok(lines) => lines.iter().for_each(|l| println!("{l}")),
                Err(e) => {
                    eprintln!("sbx: {name}: {e}");
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            return Err(msg(format!(
                "{failed} of {} image(s) not removed",
                names.len()
            )));
        }
        Ok(0)
    }

    fn rmi_one(
        &self,
        m: &Manager,
        sandboxes: &[SandboxInfo],
        name: &str,
        force: bool,
    ) -> AppResult<Vec<String>> {
        let found = self.require(m, name)?;
        let ref_count = m
            .store()
            .refs()?
            .values()
            .filter(|v| *v == found.id())
            .count();
        match behavior::rmi_action(&found, ref_count, force) {
            RmiAction::Untag(r) => {
                m.untag(&r)?;
                Ok(vec![format!("Untagged: {r}")])
            }
            RmiAction::RefuseMultipleRefs(id) => Err(msg(format!(
                "image {} is referenced by {ref_count} references; remove them or use -f",
                short(&id)
            ))),
            RmiAction::Delete(id) => {
                let users = sandboxes.iter().filter(|s| s.spec.image == id);
                let (running, stopped): (Vec<_>, Vec<_>) =
                    users.partition(|s| s.state == State::Running);
                let ids = |v: Vec<&SandboxInfo>| v.into_iter().map(|s| s.id.clone()).collect();
                match behavior::rmi_decision(ids(running), ids(stopped), force) {
                    RmiDecision::Remove => {
                        let r = m.remove(&id)?;
                        let mut lines: Vec<String> = r
                            .untagged
                            .iter()
                            .map(|u| format!("Untagged: {u}"))
                            .collect();
                        lines.push(format!("Deleted: {id}"));
                        Ok(lines)
                    }
                    RmiDecision::RefuseRunning(ids) => Err(msg(format!(
                        "image is used by running sandbox(es): {}",
                        join_ids(&ids)
                    ))),
                    RmiDecision::RefuseStopped(ids) => Err(msg(format!(
                        "image is used by stopped sandbox(es): {} (use -f)",
                        join_ids(&ids)
                    ))),
                }
            }
        }
    }

    /// `run`과 `create`: 옵션을 먼저 모두 해석하고, [`behavior::create_steps`] 순서로 코어를 부른다.
    async fn create(&self, a: &RunArgs) -> AppResult<SandboxId> {
        if a.network != "none" {
            return Err(msg(format!(
                "--network {} is not supported yet (only none)",
                a.network
            )));
        }
        let user = a.user.as_deref().map(str::parse::<UserSpec>).transpose()?;
        let overrides = ProcessOverrides {
            entrypoint: a.entrypoint.as_ref().map(|e| {
                if e.is_empty() {
                    Vec::new()
                } else {
                    vec![e.clone()]
                }
            }),
            args: a.command.clone(),
            env: parse::env(&a.env, |k| std::env::var(k).ok()),
            cwd: a.workdir.clone(),
            user,
        };
        let resources = Resources {
            vcpus: a
                .cpus
                .as_deref()
                .map(parse::cpus)
                .transpose()?
                .unwrap_or(behavior::DEFAULT_VCPUS),
            memory_mib: a
                .memory
                .as_deref()
                .map(parse::memory_mib)
                .transpose()?
                .unwrap_or(behavior::DEFAULT_MEMORY_MIB),
            scratch_disk_mib: behavior::DEFAULT_SCRATCH_MIB,
        };
        let mut mounts = a
            .volumes
            .iter()
            .map(|v| parse::volume(v))
            .collect::<Result<Vec<_>, _>>()?;
        for m in &a.mounts {
            mounts.push(parse::mount(m)?);
        }
        let labels: BTreeMap<String, String> = a.labels.iter().map(|l| parse::label(l)).collect();

        let m = self.manager()?;
        let format = a.rootfs_format;
        let found = self.lookup(&m, &a.image)?;
        let rootfs_present = found
            .as_ref()
            .is_some_and(|f| m.store().has_rootfs(f.id(), format));
        let steps = behavior::create_steps(a.pull, found.is_some(), rootfs_present)
            .map_err(|e| msg(format!("{}: {e}", a.image)))?;
        let mut image = found.map(|f| f.id().clone());
        for step in steps {
            match step {
                Step::Pull => {
                    let info = m.pull(&a.image, Platform { arch: host_arch()? }).await?;
                    eprintln!("{}: pulled {}", info.record.reference, info.record.id);
                    image = Some(info.record.id);
                }
                Step::Prepare => {
                    let id = image
                        .as_ref()
                        .ok_or_else(|| msg("internal: prepare before pull"))?;
                    if m.prepare(id, format).await? {
                        eprintln!("{}: {format} rootfs prepared", short(id));
                    }
                }
                Step::Create => {
                    let id = image
                        .clone()
                        .ok_or_else(|| msg("internal: create without image"))?;
                    let record = m.store().image(&id)?;
                    let process = sandbox_runtime::process::resolve(&record.config, &overrides)?;
                    let spec = SandboxSpec {
                        image: id,
                        rootfs_format: format,
                        process,
                        resources,
                        mounts: mounts.clone(),
                        network: NetworkPolicy::Disabled,
                        lifetime: if a.detach {
                            Lifetime::Detached
                        } else {
                            Lifetime::Attached
                        },
                        hostname: a.hostname.clone(),
                        labels: labels.clone(),
                    };
                    let sandbox = self.runtime()?.create(spec).await?;
                    return Ok(sandbox.id().clone());
                }
            }
        }
        Err(msg("internal: create step missing"))
    }

    async fn run_sandbox(&self, a: &RunArgs) -> AppResult<u8> {
        let id = self.create(a).await?;
        let rt = self.runtime()?;
        rt.get(&id).await?.start().await?;
        if a.detach {
            println!("{id}");
            Ok(0)
        } else {
            Err(msg("attaching to a sandbox is not implemented yet; use -d"))
        }
    }

    async fn exec(&self, a: &ExecArgs) -> AppResult<u8> {
        let rt = self.runtime()?;
        let sandbox = rt.get(&SandboxId::new(&a.id)?).await?;
        let spec = ExecSpec {
            args: a.command.clone(),
            env: parse::env(&a.env, |k| std::env::var(k).ok()),
            cwd: a.workdir.clone(),
            user: a.user.as_deref().map(str::parse::<UserSpec>).transpose()?,
            tty: a.tty,
        };
        let code = sandbox.exec(spec).await?.wait().await?;
        Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(255))
    }

    async fn cp(&self, src: &str, dst: &str) -> AppResult<u8> {
        let rt = self.runtime()?;
        match (parse::cp_endpoint(src), parse::cp_endpoint(dst)) {
            (CpEndpoint::Host(h), CpEndpoint::Sandbox { id, path }) => {
                rt.get(&id).await?.copy_in(&h, &path).await?;
            }
            (CpEndpoint::Sandbox { id, path }, CpEndpoint::Host(h)) => {
                rt.get(&id).await?.copy_out(&path, &h).await?;
            }
            (CpEndpoint::Host(_), CpEndpoint::Host(_)) => {
                return Err(msg("one of SRC or DST must be ID:path"));
            }
            (CpEndpoint::Sandbox { .. }, CpEndpoint::Sandbox { .. }) => {
                return Err(msg("copying between sandboxes is not supported"));
            }
        }
        Ok(0)
    }

    async fn stop(&self, secs: u64, ids: &[String]) -> AppResult<u8> {
        let rt = self.runtime()?;
        each(ids, async |id| {
            rt.get(&id).await?.stop(Duration::from_secs(secs)).await?;
            Ok(())
        })
        .await
    }

    async fn rm(&self, force: bool, ids: &[String]) -> AppResult<u8> {
        let rt = self.runtime()?;
        each(ids, async |id| {
            let sandbox = rt.get(&id).await?;
            if sandbox.state() == State::Running {
                if !force {
                    return Err(msg("sandbox is running; stop it first or use -f"));
                }
                sandbox.stop(Duration::ZERO).await?;
                rt.get(&id).await?.delete().await?;
            } else {
                sandbox.delete().await?;
            }
            Ok(())
        })
        .await
    }

    async fn ps(&self, all: bool) -> AppResult<u8> {
        let list: Vec<SandboxInfo> = self
            .runtime()?
            .list()
            .await?
            .into_iter()
            .filter(|s| all || s.state == State::Running)
            .collect();
        if self.json() {
            let v: Vec<_> = list.iter().map(info_json).collect();
            println!("{}", serde_json::to_string_pretty(&v)?);
            return Ok(0);
        }
        println!(
            "{:<24} {:<14} {:<30} STATUS",
            "SANDBOX ID", "IMAGE", "COMMAND"
        );
        for s in &list {
            let mut command = s.spec.process.args.join(" ");
            if command.chars().count() > 28 {
                command = command.chars().take(27).collect::<String>() + "…";
            }
            let status = match &s.error {
                Some(e) => format!("{} ({e})", s.state.as_str()),
                None => s.state.as_str().to_owned(),
            };
            println!(
                "{:<24} {:<14} {:<30} {}",
                s.id,
                short(&s.spec.image),
                command,
                status
            );
        }
        Ok(0)
    }

    async fn inspect(&self, id: &str) -> AppResult<u8> {
        let id = SandboxId::new(id)?;
        let info = self
            .runtime()?
            .list()
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| msg(format!("no such sandbox: {id}")))?;
        println!("{}", serde_json::to_string_pretty(&info_json(&info))?);
        Ok(0)
    }
}

fn info_json(s: &SandboxInfo) -> serde_json::Value {
    json!({"id": s.id, "state": s.state, "error": s.error, "spec": s.spec})
}

fn join_ids(ids: &[SandboxId]) -> String {
    ids.iter()
        .map(SandboxId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// ID 여러 개에 같은 동작을 한다. 하나가 실패해도 나머지를 계속하고, 성공한 ID를 출력한다(Docker와 같다).
async fn each(ids: &[String], f: impl AsyncFn(SandboxId) -> AppResult<()>) -> AppResult<u8> {
    let mut failed = 0;
    for raw in ids {
        let result = match SandboxId::new(raw) {
            Ok(id) => f(id).await,
            Err(e) => Err(e.into()),
        };
        match result {
            Ok(()) => println!("{raw}"),
            Err(e) => {
                eprintln!("sbx: {raw}: {e}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        return Err(msg(format!("{failed} of {} sandbox(es) failed", ids.len())));
    }
    Ok(0)
}
