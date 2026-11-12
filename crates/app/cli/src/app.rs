//! 명령 처리. 구현체를 조립하고, 관리 코어와 실행 코어를 [`behavior`]의 규칙대로 차례로 부른다.
//!
//! 조립하는 구현체:
//!
//! | 자리 | 구현체 |
//! |---|---|
//! | 이미지 소스 | `sbx-oci` (OCI 레지스트리) |
//! | rootfs writer | `sbx-rootfs`의 erofs |
//! | 실행기 | [`crate::choose`]의 순서(`--executor`, 샌드박스 고정, `.sbx-executor`, 기본, PATH)로 고른 외부 실행기: `sbx-executor-proto` 규약을 말한다 |
//!
//! | detached 소유 | `--shim <실행 파일>`(`SBX_SHIM`, 기본 sbx 옆의 `sbx-shim`, 없으면 PATH): `sbx-shim` |
//!
//! 실행기 실행 파일은 VM을 띄우는 명령(`run`, `create`, `start`)에만 필요하다. 나머지는 떠 있는 VM에 규약대로 연결만 한다
//! ([`sbx_executor_process::ConnectOnly`]).

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use sbx_executor::Executor;
use sbx_executor_process::{ConnectOnly, ProcessExecutor};
use sbx_image::Platform;
use sbx_manage::Manager;
use sbx_model::{
    Arch, ExecSpec, ImageId, ProcessOverrides, Resources, RootfsFormat, SandboxId, SandboxSpec,
    Signal, State, StopSpec, UserSpec,
};
use sbx_oci::{OciImageSource, Reference};
use sbx_rootfs::erofs::ErofsBuilder;
use sbx_runtime::{Runtime, RuntimeConfig, RuntimeError, SandboxInfo};
use sbx_shim::{LogReader, MAIN_LOG, ShimConfig, Stream};
use sbx_store::CacheLayout;

use crate::attach::{self, attach};
use crate::behavior::{self, ImageMatch, LookupError, RmiAction, RmiDecision, Step};
use crate::cli::{
    AttachArgs, Cli, Command, ExecArgs, ExecutorCommand, Format, LogsArgs, RunArgs, StopArgs,
    SystemCommand,
};
use crate::filter::{self, Filters};
use crate::output;
use crate::parse::{self, CpEndpoint};
use crate::progress::Status;
use crate::{choose, dirs, executors, guest, host};

mod load;
mod login;
mod stats;
mod system;
mod upgrade;

/// `logs -f`, `events`가 새 기록을 보러 가는 간격.
const FOLLOW_POLL: Duration = Duration::from_millis(100);

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

/// 1이 아닌 종료 코드로 끝나야 하는 오류. 코드는 [`behavior`]의 종료 코드 표를 따른다.
#[derive(Debug)]
pub struct Failure {
    /// 종료 코드.
    pub code: u8,
    /// 오류.
    pub error: Box<dyn Error>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl Error for Failure {}

fn failure(code: u8, error: impl Into<Box<dyn Error>>) -> Box<dyn Error> {
    Box::new(Failure {
        code,
        error: error.into(),
    })
}

/// `run`, `exec`에서 명령을 시작하지 못했으면 126/127, 아니면 `otherwise`.
fn start_failure(e: RuntimeError, otherwise: u8) -> Box<dyn Error> {
    let code = match &e {
        RuntimeError::CommandFailed(g) => behavior::command_failure_code(g.kind),
        _ => otherwise,
    };
    failure(code, explain(e))
}

/// 고칠 방법을 덧붙인다. 게스트 init이 없으면 이 빌드에 내장되지 않은 것이다.
fn explain(e: RuntimeError) -> Box<dyn Error> {
    match &e {
        RuntimeError::GuestInitMissing(_) => msg(format!(
            "{e}; this sbx build has no embedded guest init: build one with `cargo xtask guest-initramfs --arch <x86_64|aarch64>` \
             and pass it with --guest-init, or use a release build (`cargo xtask dist`)"
        )),
        _ => Box::new(e),
    }
}

fn short(id: &ImageId) -> &str {
    &id.digest().hex()[..12]
}

fn host_arch() -> AppResult<Arch> {
    Arch::host().ok_or_else(|| msg("this host architecture is not supported"))
}

/// 명령 하나를 실행한다.
pub async fn run(cli: Cli) -> AppResult<u8> {
    let ctx = Ctx::new(&cli)?;
    match &cli.command {
        Command::Pull { platform, image } => ctx.pull(platform.as_deref(), image).await,
        Command::Executor { command } => ctx.executor_cmd(command).await,
        Command::Login {
            username,
            password_stdin,
            server,
        } => {
            ctx.login(username.as_deref(), *password_stdin, server.as_deref())
                .await
        }
        Command::Logout { server } => ctx.logout(server.as_deref()),
        Command::Stats { no_stream, ids } => ctx.stats(*no_stream, ids).await,
        Command::System { command } => match command {
            SystemCommand::Df => ctx.system_df().await,
            SystemCommand::Prune { all, force, filter } => {
                ctx.system_prune(*all, *force, filter).await
            }
        },
        Command::Prepare {
            rootfs_format,
            image,
        } => ctx.prepare(*rootfs_format, image).await,
        Command::Images { quiet, filter } => ctx.images(*quiet, filter),
        Command::Info => ctx.info().await,
        Command::Upgrade(a) => ctx.upgrade(a).await,
        Command::Rmi { force, images } => ctx.rmi(*force, images).await,
        Command::Load {
            input,
            tag,
            platform,
        } => {
            ctx.load(input.as_deref(), tag.as_deref(), platform.as_deref())
                .await
        }
        Command::Save { output, image } => ctx.save(output.as_deref(), image),
        Command::Run { attach, args } => ctx.run_sandbox(attach, args).await,
        Command::Create(a) => {
            if a.rm {
                // 끝난 뒤 지우기는 붙어 있는 run만 할 수 있다. 기록해 두었다가 start에서 지우지 않는다.
                return Err(msg("--rm is only supported with run"));
            }
            let id = ctx.create(a).await?;
            ctx.print_sandbox_ref(&id, a.name.clone())?;
            Ok(0)
        }
        Command::Start {
            attach,
            interactive,
            id,
        } => {
            if *attach || *interactive {
                ctx.start(*interactive, id).await
            } else {
                let resolved = ctx.runtime()?.resolve(id).await?;
                ctx.start_detached(&resolved, false).await?;
                if ctx.json() {
                    let name = ctx
                        .runtime()?
                        .get(&resolved)
                        .await?
                        .info()
                        .spec
                        .name
                        .clone();
                    ctx.print_sandbox_ref(&resolved, name)?;
                } else {
                    println!("{id}");
                }
                Ok(0)
            }
        }
        Command::Logs(a) => ctx.logs(a).await,
        Command::Exec(a) => ctx.exec(a).await,
        Command::Cp { src, dst } => ctx.cp(src, dst).await,
        Command::Stop(a) => ctx.stop(a).await,
        Command::Restart(a) => ctx.restart(a).await,
        Command::Kill { signal, ids } => ctx.kill(signal, ids).await,
        Command::Wait { ids } => ctx.wait(ids).await,
        Command::Rm { force, ids } => ctx.rm(*force, ids).await,
        Command::Attach {
            no_stdin,
            sig_proxy,
            detach_keys,
            id,
        } => ctx.attach_cmd(*no_stdin, *sig_proxy, detach_keys, id).await,
        Command::Ps { all, quiet, filter } => ctx.ps(*all, *quiet, filter).await,
        Command::Inspect { id } => ctx.inspect(id).await,
        Command::Events {
            since,
            until,
            filter,
        } => ctx.events(since.as_deref(), until.as_deref(), filter).await,
        // `mcp`는 이 위(`sbx_cli::run`)에서 [`crate::mcp`]로 간다.
        Command::Mcp { .. } | Command::Completion { .. } | Command::Tui => {
            Err(msg("internal: served by sbx_cli::run"))
        }
    }
}

/// 지금 유닉스 시각(나노초).
/// 지금 시각(유닉스 시각, 나노초).
pub(crate) fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// `logs`가 기록을 쓰는 곳. 명시적 동작:
///
/// - 텍스트: 흐름 그대로(표준 출력은 stdout, 표준 오류는 stderr). `-t`면 줄마다 앞에 `RFC 3339 나노초 + 공백`
///   (Docker `logs -t`와 같은 모양)을 붙인다([`behavior::prefix_lines`]).
/// - `--format json`: 두 흐름 모두 stdout에 [`output::LogLine`] 한 줄씩. 기록을 줄로 나눈 조각마다 하나이고, 끝나지 않은
///   줄은 기다리지 않고 그때까지 온 만큼 낸다. 시각은 늘 들어 있으므로 `-t`는 바꾸는 것이 없다.
/// - stdout이 닫히면(파이프를 읽는 쪽이 끝났다) 조용히 끝난다. stderr에 쓰지 못한 것은 버린다.
struct LogSink {
    json: bool,
    prefix: bool,
    /// 흐름마다(표준 출력, 표준 오류) 지금 줄 처음인지.
    at_line_start: [bool; 2],
    out: std::io::Stdout,
    err: std::io::Stderr,
}

impl LogSink {
    fn new(json: bool, prefix: bool) -> Self {
        Self {
            json,
            prefix,
            at_line_start: [true; 2],
            out: std::io::stdout(),
            err: std::io::stderr(),
        }
    }

    /// 기록(또는 `--tail`의 줄) 하나를 쓴다. stdout이 닫혔으면 `false`.
    fn write(&mut self, stream: Stream, time_ns: u64, data: &[u8]) -> AppResult<bool> {
        if self.json {
            for piece in data.split_inclusive(|&b| b == b'\n') {
                // `--jq`도 여기서 적용한다(`render_line`).
                for line in output::render_line(&output::LogLine::new(stream, time_ns, piece))? {
                    if writeln!(self.out, "{line}")
                        .and_then(|()| self.out.flush())
                        .is_err()
                    {
                        return Ok(false);
                    }
                }
            }
            return Ok(true);
        }
        let i = usize::from(stream == Stream::Stderr);
        let prefixed;
        let data = if self.prefix {
            let prefix = format!("{} ", behavior::rfc3339_ns(time_ns));
            prefixed = behavior::prefix_lines(data, &prefix, &mut self.at_line_start[i]);
            &prefixed[..]
        } else {
            data
        };
        Ok(match stream {
            Stream::Stdout => self
                .out
                .write_all(data)
                .and_then(|()| self.out.flush())
                .is_ok(),
            Stream::Stderr => {
                let _ = self.err.write_all(data).and_then(|()| self.err.flush());
                true
            }
        })
    }
}

/// 명령들이 함께 쓰는 설정과 코어. `mcp`도 이것으로 코어를 부른다.
pub(crate) struct Ctx {
    data_dir: PathBuf,
    cache_dir: PathBuf,
    socket_dir: PathBuf,
    guest_init: Option<PathBuf>,
    /// `--executor`로 준 것.
    executor: Option<PathBuf>,
    shim: Option<PathBuf>,
    format: Format,
    /// 고른 실행기마다 실행 코어를 한 번만 만든다(실행기 `capabilities`와 게스트 init 확인을 되풀이하지 않는다).
    launchers: std::sync::Mutex<Vec<(choose::Chosen, Runtime)>>,
    /// 받을 이미지의 크기 상한(`sbx mcp --max-image-size`).
    pub(crate) max_image_bytes: Option<u64>,
}

impl Ctx {
    pub(crate) fn new(cli: &Cli) -> AppResult<Self> {
        let cache_dir = cli
            .cache_dir
            .clone()
            .or_else(dirs::cache_dir)
            .ok_or_else(|| msg("cannot determine a cache directory; use --cache-dir"))?;
        let data_dir = cli
            .data_dir
            .clone()
            .or_else(dirs::data_dir)
            .ok_or_else(|| msg("cannot determine a data directory; use --data-dir"))?;
        // 더 새 sbx가 바꾼 배치는 짐작하지 않고 거부한다(COMPATIBILITY.md).
        crate::layout::check(&data_dir, "data directory").map_err(msg)?;
        crate::layout::check(&cache_dir, "cache directory").map_err(msg)?;
        Ok(Self {
            socket_dir: cli
                .socket_dir
                .clone()
                .or_else(dirs::socket_dir)
                .ok_or_else(|| msg("cannot determine a socket directory; use --socket-dir"))?,
            guest_init: cli.guest_init.clone(),
            data_dir,
            cache_dir,
            executor: cli.executor.clone(),
            shim: cli.shim.clone(),
            format: cli.format,
            launchers: std::sync::Mutex::new(Vec::new()),
            max_image_bytes: None,
        })
    }

    fn json(&self) -> bool {
        self.format == Format::Json
    }

    /// 만들거나 시작한 샌드박스: ID, JSON이면 [`output::SandboxRef`].
    fn print_sandbox_ref(&self, id: &SandboxId, name: Option<String>) -> AppResult<()> {
        if self.json() {
            output::print_value(&output::SandboxRef {
                id: id.to_string(),
                name,
            })?;
        } else {
            println!("{id}");
        }
        Ok(())
    }

    fn manager(&self) -> AppResult<Manager> {
        Ok(Manager::new(
            &self.cache_dir,
            Arc::new(
                OciImageSource::with_auth(crate::dirs::registry_auth())?
                    .with_registries(crate::dirs::registries_conf())
                    .with_policy(crate::dirs::signature_policy()),
            ),
            vec![Arc::new(ErofsBuilder)],
        )?
        .with_max_image_bytes(self.max_image_bytes))
    }

    /// 게스트 init. 순서: `--guest-init` > sbx에 내장된 것([`guest::embedded`], VM을 띄울 때 관리 코어가 캐시에 꺼낸다)
    /// > 캐시의 `guest/<게스트 프로토콜 버전>/<arch>/initramfs`(직접 둔 것).
    fn guest_init(&self, launch: bool) -> AppResult<PathBuf> {
        if let Some(p) = &self.guest_init {
            return Ok(p.clone());
        }
        let arch = host_arch()?;
        if launch && let Some(bytes) = guest::embedded(arch) {
            return Ok(self.manager()?.install_guest_init(arch, bytes)?);
        }
        Ok(CacheLayout::new(&self.cache_dir)
            .guest_init(&sbx_guest_proto::PROTOCOL_VERSION.to_string(), arch))
    }

    /// `launch`면 VM을 띄울 때 쓰는 설정이다(내장 게스트 init을 꺼낸다).
    fn runtime_config(&self, launch: bool) -> AppResult<RuntimeConfig> {
        Ok(RuntimeConfig {
            data_dir: self.data_dir.clone(),
            cache_dir: self.cache_dir.clone(),
            guest_init: self.guest_init(launch)?,
            socket_dir: self.socket_dir.clone(),
        })
    }

    fn runtime_with(&self, executor: Arc<dyn Executor>, launch: bool) -> AppResult<Runtime> {
        Ok(Runtime::new(self.runtime_config(launch)?, executor)?)
    }

    /// shim 실행 파일: `--shim`, 아니면 sbx 옆의 `sbx-shim`, 아니면 PATH의 `sbx-shim`.
    fn shim_program(&self) -> PathBuf {
        if let Some(p) = &self.shim {
            return p.clone();
        }
        let name = format!("sbx-shim{}", std::env::consts::EXE_SUFFIX);
        std::env::current_exe()
            .ok()
            .map(|exe| exe.with_file_name(&name))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from(name))
    }

    /// shim으로 시작한다(붙지 않는다). 시작이 끝나면 돌아온다.
    pub(crate) async fn start_detached(
        &self,
        id: &SandboxId,
        remove_on_exit: bool,
    ) -> AppResult<()> {
        // 실행기와 이미지를 먼저 확인한다(shim 안에서 실패하면 이유가 덜 분명하다).
        let chosen = self.choose_for(id).await?;
        let sandbox = self.launcher_with(&chosen)?.get(id).await?;
        let config = ShimConfig {
            runtime: self.runtime_config(true)?,
            executor: chosen.program.clone(),
            vmm_dir: chosen.vmm_dir().map(ToOwned::to_owned),
            remove_on_exit,
        };
        if !config.runtime.guest_init.is_file() {
            return Err(explain(RuntimeError::GuestInitMissing(
                config.runtime.guest_init.clone(),
            )));
        }
        sbx_shim::start_detached(&self.shim_program(), &config, id, &sandbox.log_dir())
            .await
            .map_err(msg)
    }

    /// `logs`: shim이 남긴 기본 프로세스 출력을 순서대로 표준 출력과 표준 오류에 쓴다.
    /// `-f`면 샌드박스가 실행 중인 동안(`--until`이 있으면 그때까지) 새 출력을 기다린다.
    /// 시각 조건(`-t`, `--since`, `--until`)은 [`behavior::LogTimes`], 쓰는 모양은 [`LogSink`]가 정한다.
    async fn logs(&self, a: &LogsArgs) -> AppResult<u8> {
        let tail = match a.tail.as_str() {
            "all" => None,
            n => Some(
                n.parse::<usize>()
                    .map_err(|_| msg(format!("--tail: expected a number or `all`, got {n:?}")))?,
            ),
        };
        let time = |flag: &str, v: &Option<String>| {
            v.as_deref()
                .map(|s| parse::time_ns(s, now_ns()).map_err(|e| msg(format!("{flag}: {e}"))))
                .transpose()
        };
        let times = behavior::LogTimes {
            since: time("--since", &a.since)?,
            until: time("--until", &a.until)?,
            prefix: a.timestamps && !self.json(),
        };
        let rt = self.runtime()?;
        let sandbox = rt.find(&a.id).await?;
        let path = sandbox.log_dir().join(MAIN_LOG);
        let mut reader = match LogReader::open(&path) {
            Ok(r) => Some(r),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let mut sink = LogSink::new(self.json(), times.prefix);
        // --tail: 이미 있는 것은 모아서 마지막 N줄만 보인다. 그 뒤(-f)는 그대로 흘린다.
        if let (Some(n), Some(r)) = (tail, &mut reader) {
            let mut records = Vec::new();
            while let Some(rec) = r.next_record()? {
                if times.wants(rec.time_ns) {
                    records.push((rec.stream, rec.time_ns, rec.data));
                }
            }
            for (stream, time_ns, line) in behavior::last_lines(records, n) {
                if !sink.write(stream, time_ns, &line)? {
                    return Ok(0);
                }
            }
        }
        loop {
            // 끝낼 때인지 먼저 정하고 읽는다. 그때까지 쓰인 것은 이번에 모두 읽힌다.
            let done = !a.follow
                || times.until.is_some_and(|u| now_ns() > u)
                || rt.get(sandbox.id()).await?.state() != State::Running;
            if reader.is_none() {
                reader = LogReader::open(&path).ok();
            }
            if let Some(r) = &mut reader {
                while let Some(rec) = r.next_record()? {
                    if times.wants(rec.time_ns)
                        && !sink.write(rec.stream, rec.time_ns, &rec.data)?
                    {
                        return Ok(0);
                    }
                }
            }
            if done {
                return Ok(0);
            }
            tokio::time::sleep(FOLLOW_POLL).await;
        }
    }

    /// VM을 띄우지 않는 일에 쓰는 실행 코어. 실행기 실행 파일을 실행하지 않는다.
    pub(crate) fn runtime(&self) -> AppResult<Runtime> {
        self.runtime_with(Arc::new(ConnectOnly), false)
    }

    /// 있는 샌드박스를 띄우는 일(`start`)에 쓰는 실행 코어. 그 샌드박스에 고정된 실행기를 따른다.
    pub(crate) async fn launcher_for(&self, id: &SandboxId) -> AppResult<Runtime> {
        let chosen = self.choose_for(id).await?;
        self.launcher_with(&chosen)
    }

    fn launcher_with(&self, chosen: &choose::Chosen) -> AppResult<Runtime> {
        let mut cache = self.launchers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, rt)) = cache.iter().find(|(c, _)| c == chosen) {
            return Ok(rt.clone());
        }
        let executor = ProcessExecutor::with_env(&chosen.program, chosen.env())?;
        let rt = self.runtime_with(Arc::new(executor), true)?;
        cache.push((chosen.clone(), rt.clone()));
        Ok(rt)
    }

    /// 있는 샌드박스에 쓸 실행기.
    async fn choose_for(&self, id: &SandboxId) -> AppResult<choose::Chosen> {
        let pinned = self
            .runtime()?
            .get(id)
            .await?
            .info()
            .spec
            .labels
            .get(choose::PIN_LABEL)
            .cloned();
        self.choose(pinned.as_deref())
    }

    /// 쓸 실행기. 순서는 [`crate::choose`]에 있다. `pinned`는 샌드박스에 고정된 것.
    pub(crate) fn choose(&self, pinned: Option<&str>) -> AppResult<choose::Chosen> {
        self.try_choose(pinned)?.ok_or_else(|| {
            // 무엇을 하면 되는지 알린다. 실행기가 없는 OS(지금은 Windows)에서는 설치할 것도 없다.
            msg(match std::env::consts::OS {
                "linux" | "macos" => choose::NO_EXECUTOR.to_owned(),
                os => format!(
                    "sandboxes cannot run on {os} yet: there is no executor for this platform \
                     (pull, prepare and images work)"
                ),
            })
        })
    }

    /// [`Ctx::choose`]와 같고, 아무것도 없으면 `None`.
    pub(crate) fn try_choose(&self, pinned: Option<&str>) -> AppResult<Option<choose::Chosen>> {
        let cwd = std::env::current_dir().ok();
        choose::choose(&choose::Inputs {
            executors: &executors::installed(&self.data_dir),
            adapters: &executors::adapters(&self.data_dir),
            flag: self.executor.as_deref(),
            pinned,
            cwd: cwd.as_deref(),
            on_path: &choose::on_path,
        })
        .map_err(msg)
    }

    /// `executor ...`.
    async fn executor_cmd(&self, c: &ExecutorCommand) -> AppResult<u8> {
        let pins: Vec<(String, String)> = if executors::needs_pins(c) {
            self.runtime()?
                .list()
                .await?
                .into_iter()
                .filter_map(|i| {
                    let pin = i.spec.labels.get(choose::PIN_LABEL)?.clone();
                    Some((i.spec.name.clone().unwrap_or_else(|| i.id.to_string()), pin))
                })
                .collect()
        } else {
            Vec::new()
        };
        executors::run(
            &executors::Ctx {
                data_dir: &self.data_dir,
                cache_dir: &self.cache_dir,
                json: self.json(),
                flag: self.executor.as_deref(),
                pins: &pins,
            },
            c,
        )
        .await
    }

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
        let status = Status::new();
        let info = self
            .manager()?
            .pull_with(image, Platform { arch }, &|p| status.pull(image, p))
            .await?;
        status.clear();
        if self.json() {
            output::print_value(&output::Pulled {
                reference: info.record.reference.clone(),
                id: info.record.id.to_string(),
                arch: info.record.arch.oci_name().to_owned(),
            })?;
        } else {
            println!("{}: pulled {}", info.record.reference, info.record.id);
        }
        Ok(0)
    }

    async fn prepare(&self, format: RootfsFormat, image: &str) -> AppResult<u8> {
        let m = self.manager()?;
        let found = self.require(&m, image)?;
        let status = Status::new();
        status.text(&format!("Preparing {format} rootfs for {image}…"));
        let built = m.prepare(found.id(), format).await?;
        status.clear();
        if self.json() {
            output::print_value(&output::Prepared {
                id: found.id().to_string(),
                rootfs_format: format.to_string(),
                built,
            })?;
        } else if built {
            println!("{}: {format} rootfs prepared", short(found.id()));
        } else {
            println!("{}: {format} rootfs already prepared", short(found.id()));
        }
        Ok(0)
    }

    /// `info`: 버그 보고와 설치 확인에 쓰는 진단. 실행기를 부를 수 없어도 나머지는 보여준다.
    async fn info(&self) -> AppResult<u8> {
        let arch = host_arch()?;
        let (vm_ok, hypervisor) = host::hypervisor();
        let chosen = self.try_choose(None).map_err(|e| e.to_string());
        let caps = match &chosen {
            Ok(Some(c)) => ProcessExecutor::with_env(&c.program, c.env())
                .map(|e| e.capabilities())
                .map_err(|e| match e {
                    sbx_executor::ExecutorError::Launch(m) => m,
                    e => e.to_string(),
                }),
            Ok(None) => Err(choose::NO_EXECUTOR.to_owned()),
            Err(e) => Err(e.clone()),
        };
        let chosen = chosen.ok().flatten();
        let guest = match &self.guest_init {
            Some(p) => p.display().to_string(),
            None if guest::embedded(arch).is_some() => "embedded in sbx".into(),
            None => self.guest_init(false)?.display().to_string(),
        };
        let sandboxes = self.runtime()?.list().await?;
        let running = sandboxes
            .iter()
            .filter(|s| s.state == State::Running)
            .count();
        let images = self.manager()?.images()?.len();
        let executor_name = chosen
            .as_ref()
            .map_or_else(|| "none".to_owned(), choose::Chosen::label);
        let program = chosen.as_ref().map(|c| c.program.display().to_string());
        let source = chosen.as_ref().map(|c| c.source.to_string());
        let adapter = chosen
            .as_ref()
            .and_then(|c| c.adapter.as_ref())
            .map(ToString::to_string);
        if self.json() {
            let (capabilities, error) = match &caps {
                Ok(c) => (
                    Some(output::Capabilities {
                        name: c.name.to_owned(),
                        arch: c.arch.oci_name().to_owned(),
                        rootfs_formats: c.rootfs_formats.iter().map(|f| f.to_string()).collect(),
                        live_share: c.live_share,
                        custom_kernel: c.custom_kernel,
                    }),
                    None,
                ),
                Err(e) => (None, Some(e.clone())),
            };
            output::print_value(&output::Info {
                version: env!("CARGO_PKG_VERSION").to_owned(),
                os: host::os(),
                arch: arch.oci_name().to_owned(),
                hypervisor: output::Hypervisor {
                    usable: vm_ok,
                    detail: hypervisor,
                },
                executor: output::ExecutorInfo {
                    name: executor_name,
                    source,
                    program,
                    adapter,
                    capabilities,
                    error,
                },
                guest_init: guest,
                data_dir: self.data_dir.display().to_string(),
                cache_dir: self.cache_dir.display().to_string(),
                sandboxes: sandboxes.len(),
                running,
                images,
                update: crate::update::cached_info(&self.cache_dir),
            })?;
            return Ok(0);
        }
        match crate::update::cached_summary(&self.cache_dir) {
            Some(s) => println!("{s}"),
            None => println!("sbx {}", env!("CARGO_PKG_VERSION")),
        }
        println!(
            " Host:        {} ({}/{})",
            host::os(),
            std::env::consts::OS,
            arch.oci_name()
        );
        println!(" Hypervisor:  {hypervisor}");
        match &source {
            Some(s) => println!(" Executor:    {executor_name} ({s})"),
            None => println!(" Executor:    {executor_name}"),
        }
        if let Some(a) = &adapter {
            println!(" Adapter:     {a}");
        }
        if let Some(p) = &program {
            println!(" Program:     {p}");
        }
        match &caps {
            Ok(c) => {
                let formats: Vec<&str> = c.rootfs_formats.iter().map(|f| f.as_str()).collect();
                let yes = |b: bool| if b { "yes" } else { "no" };
                println!(
                    " Supports:    {} ({}), rootfs {}, live share {}, custom kernel {}",
                    c.name,
                    c.arch.oci_name(),
                    formats.join(","),
                    yes(c.live_share),
                    yes(c.custom_kernel)
                );
            }
            Err(e) => println!(" Supports:    unknown ({e})"),
        }
        println!(" Guest init:  {guest}");
        println!(" Data dir:    {}", self.data_dir.display());
        println!(" Cache dir:   {}", self.cache_dir.display());
        println!(" Sandboxes:   {} ({running} running)", sandboxes.len());
        println!(" Images:      {images}");
        Ok(0)
    }

    fn images(&self, quiet: bool, filters: &[String]) -> AppResult<u8> {
        let f = Filters::parse(filters, filter::IMAGES).map_err(msg)?;
        let images: Vec<_> = self
            .manager()?
            .images()?
            .into_iter()
            .filter(|i| {
                f.allows("reference", |p| {
                    i.refs
                        .iter()
                        .any(|r| filter::glob(p, r) || filter::glob(p, behavior::familiar(r)))
                }) && f.allows("dangling", |v| (v == "true") == i.refs.is_empty())
                    && f.allows("label", |v| filter::label(&i.record.config.labels, v))
            })
            .collect();
        if quiet {
            for i in &images {
                println!("{}", short(&i.record.id));
            }
            return Ok(0);
        }
        if self.json() {
            let list: Vec<_> = images
                .iter()
                .map(|i| output::Image {
                    id: i.record.id.to_string(),
                    references: i.refs.clone(),
                    arch: i.record.arch.oci_name().to_owned(),
                    prepared: i.prepared.iter().map(|f| f.to_string()).collect(),
                    compressed_size: i.record.compressed_size(),
                    ignored_fields: i.record.ignored_fields.clone(),
                    exposed_ports: i.record.config.exposed_ports.clone(),
                    volumes: i.record.config.volumes.clone(),
                })
                .collect();
            output::print_value(&list)?;
            return Ok(0);
        }
        println!(
            "{:<40} {:<14} {:<12} {:<10} SIZE",
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
                i.refs.iter().map(|r| behavior::familiar(r)).collect()
            };
            for r in refs {
                println!(
                    "{:<40} {:<14} {:<12} {:<10} {}",
                    r,
                    short(&i.record.id),
                    format!("linux/{}", i.record.arch.oci_name()),
                    rootfs,
                    behavior::human_size(i.record.compressed_size())
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
                Ok(r) if self.json() => output::print_line(&r)?,
                Ok(r) => {
                    r.untagged.iter().for_each(|u| println!("Untagged: {u}"));
                    r.deleted.iter().for_each(|d| println!("Deleted: {d}"));
                }
                Err(e) => {
                    self.target_error(name, &e);
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
    ) -> AppResult<output::Removed> {
        let found = self.require(m, name)?;
        let removed = |untagged: Vec<String>, deleted: Vec<String>| output::Removed {
            target: name.to_owned(),
            untagged,
            deleted,
        };
        let ref_count = m
            .store()
            .refs()?
            .values()
            .filter(|v| *v == found.id())
            .count();
        match behavior::rmi_action(&found, ref_count, force) {
            RmiAction::Untag(r) => {
                m.untag(&r)?;
                Ok(removed(vec![r.to_string()], Vec::new()))
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
                        let mut deleted = vec![id.to_string()];
                        deleted.extend(r.deleted_layers.iter().map(|d| d.to_string()));
                        Ok(removed(
                            r.untagged.iter().map(|u| u.to_string()).collect(),
                            deleted,
                        ))
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
    pub(crate) async fn create(&self, a: &RunArgs) -> AppResult<SandboxId> {
        self.create_with(a, &|_| {}).await
    }

    /// [`create`](Self::create)과 같고, 이미지를 받는 동안 진행을 `on_pull`에도 알린다(`sbx mcp`가 쓴다).
    pub(crate) async fn create_with(
        &self,
        a: &RunArgs,
        on_pull: &(dyn Fn(sbx_manage::PullProgress) + Sync),
    ) -> AppResult<SandboxId> {
        // 실행기를 먼저 고른다. 없으면 이미지를 받기 전에 실패한다.
        let chosen = self.choose(None)?;
        let network = behavior::network_policy(&a.network, &a.allow)?;
        let user = a.user.as_deref().map(str::parse::<UserSpec>).transpose()?;
        let stop_signal = a
            .stop_signal
            .as_deref()
            .map(str::parse::<Signal>)
            .transpose()?;
        let overrides = ProcessOverrides {
            entrypoint: a.entrypoint.as_ref().map(|e| {
                if e.is_empty() {
                    Vec::new()
                } else {
                    vec![e.clone()]
                }
            }),
            args: a.command().to_vec(),
            env: env_values(&a.env_file, &a.env)?,
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
            scratch_disk_mib: a.storage_opt.unwrap_or(behavior::DEFAULT_SCRATCH_MIB),
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
        let image_name = a.image();
        if image_name.starts_with('-') {
            return Err(msg(format!(
                "unknown option {image_name:?} (options go before the image)"
            )));
        }
        let found = self.lookup(&m, image_name)?;
        let rootfs_present = found
            .as_ref()
            .is_some_and(|f| m.is_prepared(f.id(), format));
        let steps = behavior::create_steps(a.pull, found.is_some(), rootfs_present)
            .map_err(|e| msg(format!("{image_name}: {e}")))?;
        let mut image = found.map(|f| f.id().clone());
        for step in steps {
            match step {
                Step::Pull => {
                    let arch = host_arch()?;
                    let status = Status::new();
                    let info = m
                        .pull_with(image_name, Platform { arch }, &|p| {
                            status.pull(image_name, p);
                            on_pull(p);
                        })
                        .await?;
                    status.clear();
                    eprintln!("{}: pulled {}", info.record.reference, info.record.id);
                    image = Some(info.record.id);
                }
                Step::Prepare => {
                    let id = image
                        .as_ref()
                        .ok_or_else(|| msg("internal: prepare before pull"))?;
                    let status = Status::new();
                    status.text(&format!("Preparing {format} rootfs for {image_name}…"));
                    let built = m.prepare(id, format).await?;
                    status.clear();
                    if built {
                        eprintln!("{}: {format} rootfs prepared", short(id));
                    }
                }
                Step::Create => {
                    let id = image
                        .clone()
                        .ok_or_else(|| msg("internal: create without image"))?;
                    let record = m.store().image(&id)?;
                    let process = sbx_runtime::process::resolve(&record.config, &overrides)?;
                    let stop = StopSpec {
                        signal: sbx_runtime::process::stop_signal(&record.config, stop_signal)?,
                        timeout_secs: a.stop_timeout,
                    };
                    let mut labels = sbx_runtime::process::labels(&record.config, &labels);
                    if let Some(pin) = chosen.pin() {
                        labels.insert(choose::PIN_LABEL.to_owned(), pin);
                    }
                    let spec = SandboxSpec {
                        image: id,
                        rootfs_format: format,
                        process,
                        resources,
                        mounts: mounts.clone(),
                        network: network.clone(),
                        hostname: a.hostname.clone(),
                        labels,
                        stop,
                        open_stdin: a.interactive,
                        tty: a.tty,
                        name: a.name.clone(),
                        lifetime_secs: a.timeout,
                        ports: a.publish.clone(),
                        egress_limit_bytes: a.egress_limit,
                    };
                    let sandbox = self.launcher_with(&chosen)?.create(spec).await?;
                    return Ok(sandbox.id().clone());
                }
            }
        }
        Err(msg("internal: create step missing"))
    }

    /// `run`: 만들고, 시작하고, 붙어서 기본 프로세스가 끝날 때까지 기다린다. 종료 코드는 그 프로세스의 코드다.
    /// `--rm`이면 끝난 뒤(시작에 실패해도) 지운다.
    async fn run_sandbox(&self, attach_args: &AttachArgs, a: &RunArgs) -> AppResult<u8> {
        let fail = |e: Box<dyn Error>| failure(behavior::EXIT_RUN_FAILED, e);
        if attach_args.detach {
            let id = self.create(a).await.map_err(fail)?;
            if let Err(e) = self.start_detached(&id, a.rm).await {
                if a.rm
                    && let Ok(s) = self.runtime()?.get(&id).await
                {
                    let _ = s.delete().await;
                }
                return Err(fail(e));
            }
            self.print_sandbox_ref(&id, a.name.clone())?;
            return Ok(0);
        }
        let io = attach::io_mode(a.interactive, a.tty).map_err(|e| fail(msg(e)))?;
        let id = self.create(a).await.map_err(fail)?;
        let rt = self.launcher_for(&id).await.map_err(fail)?;
        let sandbox = rt.get(&id).await.map_err(|e| fail(e.into()))?;
        let started = sandbox.start(io).await;
        let result = match started {
            Ok(p) => attach(p, io, a.interactive)
                .await
                .map(behavior::exit_byte)
                .map_err(|e| fail(e.into())),
            Err(e) => Err(start_failure(e, behavior::EXIT_RUN_FAILED)),
        };
        if a.rm {
            let removed = match rt.get(&id).await {
                Ok(s) => s.delete().await,
                Err(e) => Err(e),
            };
            if let Err(e) = removed {
                eprintln!("sbx: {id}: cannot remove: {e}");
            }
        }
        result
    }

    /// `start -a`: 시작하고 붙어서 기본 프로세스가 끝날 때까지 기다린다. 표준 입력은 샌드박스가 `-i`이거나
    /// `start -i`이면 열고, `start -i`일 때만 sbx의 입력을 보낸다. 터미널은 샌드박스 설정대로다(Docker와 같다).
    async fn start(&self, interactive: bool, id: &str) -> AppResult<u8> {
        let id = self.runtime()?.resolve(id).await?;
        let rt = self.launcher_for(&id).await?;
        let sandbox = rt.get(&id).await?;
        let spec = &sandbox.info().spec;
        let mut io = attach::io_mode(interactive, spec.tty)?;
        io.stdin |= spec.open_stdin;
        let p = sandbox.start(io).await.map_err(explain)?;
        Ok(behavior::exit_byte(attach(p, io, interactive).await?))
    }

    /// `attach`: shim이 소유한 샌드박스의 기본 프로세스에 붙는다. 종료 코드는 그 프로세스의 코드다.
    /// 떼기 키로 떨어지면 0이다. 연결이 끊기면 샌드박스가 멈출 때까지 기다려 기록된 코드를 쓴다.
    async fn attach_cmd(
        &self,
        no_stdin: bool,
        sig_proxy: bool,
        detach_keys: &str,
        id: &str,
    ) -> AppResult<u8> {
        let detach_keys = attach::DetachKeys::parse(detach_keys)?;
        let rt = self.runtime()?;
        let sandbox = rt.find(id).await?;
        if sandbox.state() != State::Running {
            return Err(msg(
                "cannot attach to a sandbox that is not running; start it first",
            ));
        }
        #[cfg(unix)]
        {
            let path = sbx_shim::attach::socket_path(&self.socket_dir, sandbox.id());
            let stream = tokio::net::UnixStream::connect(&path).await.map_err(|_| {
                msg("this sandbox was not started detached (run -d, start); only those can be attached")
            })?;
            let opts = attach::ShimAttach {
                stdin: !no_stdin,
                sig_proxy,
                detach_keys,
            };
            match attach::attach_shim(stream, opts).await? {
                attach::ShimEnd::Exit(c) => Ok(behavior::exit_byte(c)),
                attach::ShimEnd::Detached => Ok(0),
                attach::ShimEnd::Lost => Ok(behavior::exit_byte(sandbox.wait().await?)),
            }
        }
        #[cfg(not(unix))]
        {
            let _ = (no_stdin, sig_proxy, detach_keys, sandbox);
            Err(msg("attach is not supported on this platform yet"))
        }
    }

    /// `exec`: 붙어서 실행한다. 종료 코드는 그 프로세스의 코드다.
    async fn exec(&self, a: &ExecArgs) -> AppResult<u8> {
        let io = attach::io_mode(a.interactive, a.tty)?;
        let rt = self.runtime()?;
        let sandbox = rt.find(a.id()).await?;
        let spec = ExecSpec {
            args: a.command().to_vec(),
            env: env_values(&a.env_file, &a.env)?,
            cwd: a.workdir.clone(),
            user: a.user.as_deref().map(str::parse::<UserSpec>).transpose()?,
            tty: a.tty,
        };
        if a.detach {
            sandbox
                .exec_detached(spec)
                .await
                .map_err(|e| start_failure(e, 1))?;
            return Ok(0);
        }
        let p = sandbox
            .exec_with(spec, io)
            .await
            .map_err(|e| start_failure(e, 1))?;
        let Some(secs) = a.timeout else {
            return Ok(behavior::exit_byte(attach(p, io, a.interactive).await?));
        };
        let input = p.input();
        let timer = tokio::task::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            input.signal(behavior::SIGKILL);
        });
        let code = attach(p, io, a.interactive).await?;
        let fired = timer.is_finished();
        timer.abort();
        if fired && code == behavior::EXIT_KILLED {
            eprintln!("sbx: the command was killed after its --timeout of {secs}s");
            return Ok(behavior::EXIT_TIMED_OUT);
        }
        Ok(behavior::exit_byte(code))
    }

    async fn cp(&self, src: &str, dst: &str) -> AppResult<u8> {
        let rt = self.runtime()?;
        match (parse::cp_endpoint(src), parse::cp_endpoint(dst)) {
            (CpEndpoint::Host(h), CpEndpoint::Sandbox { id, path }) => {
                rt.find(&id).await?.copy_in(&h, &path).await?;
            }
            (CpEndpoint::Sandbox { id, path }, CpEndpoint::Host(h)) => {
                rt.find(&id).await?.copy_out(&path, &h).await?;
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

    async fn stop(&self, a: &StopArgs) -> AppResult<u8> {
        let rt = self.runtime()?;
        let signal = a.signal.as_deref().map(str::parse::<Signal>).transpose()?;
        self.each(&rt, &a.ids, async |id| {
            let s = rt.get(&id).await?;
            let timeout = behavior::stop_timeout(a.time, s.info().spec.stop.timeout_secs);
            s.stop(signal, timeout).await?;
            Ok(())
        })
        .await
    }

    /// `restart`: 실행 중이면 `stop`과 같이 멈추고, 붙지 않고 다시 시작한다(Docker와 같다).
    async fn restart(&self, a: &StopArgs) -> AppResult<u8> {
        let rt = self.runtime()?;
        let signal = a.signal.as_deref().map(str::parse::<Signal>).transpose()?;
        self.each(&rt, &a.ids, async |id| {
            let s = rt.get(&id).await?;
            if s.state() == State::Running {
                let timeout = behavior::stop_timeout(a.time, s.info().spec.stop.timeout_secs);
                s.stop(signal, timeout).await?;
            }
            self.start_detached(&id, false).await
        })
        .await
    }

    async fn kill(&self, signal: &str, ids: &[String]) -> AppResult<u8> {
        let rt = self.runtime()?;
        let signal: Signal = signal.parse()?;
        self.each(&rt, ids, async |id| {
            rt.get(&id).await?.kill(signal).await?;
            Ok(())
        })
        .await
    }

    /// `wait`: 샌드박스마다 종료 코드를 한 줄씩 출력한다.
    async fn wait(&self, ids: &[String]) -> AppResult<u8> {
        let rt = self.runtime()?;
        self.each_with(&rt, ids, async |id| {
            Ok(Some(rt.get(&id).await?.wait().await?))
        })
        .await
    }

    async fn rm(&self, force: bool, ids: &[String]) -> AppResult<u8> {
        let rt = self.runtime()?;
        self.each(&rt, ids, async |id| {
            let sandbox = rt.get(&id).await?;
            if sandbox.state() == State::Running {
                if !force {
                    return Err(msg("sandbox is running; stop it first or use -f"));
                }
                // Docker의 SIGKILL처럼 기다리지 않고 끈다.
                sandbox.stop(None, Duration::ZERO).await?;
                rt.get(&id).await?.delete().await?;
            } else {
                sandbox.delete().await?;
            }
            Ok(())
        })
        .await
    }

    async fn ps(&self, all: bool, quiet: bool, filters: &[String]) -> AppResult<u8> {
        let f = Filters::parse(filters, filter::PS).map_err(msg)?;
        // 상태로 거르면 멈춘 것도 본다(Docker와 같다).
        let all = all || f.has("status") || f.has("exited");
        let store = sbx_store::Store::open(&self.cache_dir);
        let list: Vec<SandboxInfo> = self
            .runtime()?
            .list()
            .await?
            .into_iter()
            .filter(|s| all || s.state == State::Running)
            .filter(|s| {
                f.allows("id", |v| s.id.as_str().starts_with(v))
                    && f.allows("name", |v| {
                        s.spec.name.as_deref().is_some_and(|n| n.contains(v))
                    })
                    && f.allows("label", |v| filter::label(&s.spec.labels, v))
                    && f.allows("status", |v| v == s.state.as_str())
                    && f.allows("exited", |v| {
                        s.state == State::Stopped && s.exit_code == v.parse().ok()
                    })
                    && f.allows("ancestor", |v| {
                        let reference = store.image(&s.spec.image).ok().map(|r| r.reference);
                        filter::image(
                            &s.spec.image.to_string(),
                            reference.as_deref(),
                            v,
                            normalized(v).as_deref(),
                        )
                    })
            })
            .collect();
        // 새로 만든 것이 위로 (Docker와 같다).
        let mut list = list;
        list.sort_by_key(|s| std::cmp::Reverse(s.created));
        if quiet {
            for s in &list {
                println!("{}", short_id(&s.id));
            }
            return Ok(0);
        }
        if self.json() {
            let v: Vec<_> = list.iter().map(|s| self.sandbox_json(s)).collect();
            output::print_value(&v)?;
            return Ok(0);
        }
        println!(
            "{:<14} {:<24} {:<30} {:<20} {:<20} NAMES",
            "SANDBOX ID", "IMAGE", "COMMAND", "CREATED", "STATUS"
        );
        let now = std::time::SystemTime::now();
        for s in &list {
            let image = self.image_name(&s.spec.image);
            let mut command = s.spec.process.args.join(" ");
            if command.chars().count() > 28 {
                command = command.chars().take(27).collect::<String>() + "…";
            }
            let mut status = s.state.as_str().to_owned();
            if let (State::Stopped, Some(c)) = (s.state, s.exit_code) {
                status = format!("{status} ({c})");
            }
            if let Some(e) = &s.error {
                status = format!("{status}: {e}");
            }
            let created = s
                .created
                .map(|t| behavior::human_ago(now.duration_since(t).unwrap_or_default()))
                .unwrap_or_default();
            println!(
                "{:<14} {:<24} {:<30} {:<20} {:<20} {}",
                short_id(&s.id),
                image,
                command,
                created,
                status,
                s.spec.name.as_deref().unwrap_or("")
            );
        }
        Ok(0)
    }

    /// 이미지를 사람이 읽는 이름으로: 받을 때 쓴 참조를 Docker처럼 줄인다. 이미지가 지워졌으면 ID 앞부분.
    pub(crate) fn image_name(&self, image: &ImageId) -> String {
        sbx_store::Store::open(&self.cache_dir)
            .image(image)
            .map(|r| behavior::familiar(&r.reference).to_owned())
            .unwrap_or_else(|_| short(image).to_owned())
    }

    async fn inspect(&self, id: &str) -> AppResult<u8> {
        let info = self.runtime()?.find(id).await?.info().clone();
        output::print_value(&self.sandbox_json(&info))?;
        Ok(0)
    }

    /// `events`: data_dir의 샌드박스 이벤트와 cache_dir의 이미지 이벤트를 시각 순으로 합쳐 흘린다(Docker `events`).
    ///
    /// - `--since`가 없으면 지금부터 생기는 것만, 있으면 남아 있는 것 중 그 시각 뒤의 것부터.
    /// - `--until`이 없으면 끝나지 않는다. 있으면 그 시각이 지나면 남은 것을 마저 읽고 끝난다.
    /// - [`FOLLOW_POLL`]마다 두 기록을 읽어 모은 것을 시각 순으로 낸다. 그래서 두 기록 사이의 순서는 그 간격 안에서만 맞춘다.
    /// - stdout이 닫히면(예: `| head`) 조용히 끝난다.
    async fn events(
        &self,
        since: Option<&str>,
        until: Option<&str>,
        filters: &[String],
    ) -> AppResult<u8> {
        use std::io::Write as _;
        let f = Filters::parse(filters, filter::EVENTS).map_err(msg)?;
        let store = sbx_store::Store::open(&self.cache_dir);
        let wanted = |e: &sbx_events::Event| {
            let attr = |k: &str| e.attributes.get(k).map(String::as_str);
            f.allows("type", |v| v == e.kind)
                && f.allows("event", |v| v == e.action)
                && f.allows("sandbox", |v| {
                    (e.kind == sbx_runtime::EVENT_KIND || e.kind == sbx_runtime::NETWORK_EVENT_KIND)
                        && (e.id.starts_with(v) || attr("name") == Some(v))
                })
                && f.allows("image", |v| {
                    let id = if e.kind == sbx_manage::EVENT_KIND {
                        Some(e.id.as_str())
                    } else {
                        attr("image")
                    };
                    let Some(id) = id else { return false };
                    // 이벤트에 참조가 없으면(샌드박스 이벤트, delete) 캐시에서 찾는다. 지워졌으면 ID로만 맞는다.
                    let reference = attr("reference").map(str::to_owned).or_else(|| {
                        let id = ImageId::new(sbx_model::Digest::parse(id).ok()?);
                        store.image(&id).ok().map(|r| r.reference)
                    });
                    filter::image(id, reference.as_deref(), v, normalized(v).as_deref())
                })
        };
        let now = now_ns;
        let since = since
            .map(|s| parse::time_ns(s, now()).map_err(|e| msg(format!("--since: {e}"))))
            .transpose()?;
        let until = until
            .map(|s| parse::time_ns(s, now()).map_err(|e| msg(format!("--until: {e}"))))
            .transpose()?;
        let start = if since.is_some() {
            sbx_events::Start::Beginning
        } else {
            sbx_events::Start::End
        };
        let mut readers = [
            sbx_runtime::events_log(&self.data_dir).reader(start)?,
            sbx_runtime::network_log(&self.data_dir).reader(start)?,
            sbx_manage::events_log(&self.cache_dir).reader(start)?,
        ];
        // 표준 출력은 줄마다 비운다. 묶음마다 한 번 비우게 감싼다.
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        loop {
            // 끝낼 때인지 먼저 정하고 읽는다. 그 시각까지 덧붙은 것은 이번에 모두 읽힌다.
            let done = until.is_some_and(|u| now() > u);
            let mut batch = Vec::new();
            for r in &mut readers {
                while let Some(e) = r.next_event()? {
                    batch.push(e);
                }
            }
            batch.sort_by_key(|e| e.time_ns);
            for e in batch {
                if since.is_some_and(|s| e.time_ns < s)
                    || until.is_some_and(|u| e.time_ns > u)
                    || !wanted(&e)
                {
                    continue;
                }
                let lines = if self.json() {
                    output::render_line(&output::Event::new(&e))?
                } else {
                    let attrs: Vec<String> = e
                        .attributes
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect();
                    let attrs = if attrs.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", attrs.join(", "))
                    };
                    vec![format!(
                        "{} {} {} {}{attrs}",
                        behavior::rfc3339_ns(e.time_ns),
                        e.kind,
                        e.action,
                        e.id
                    )]
                };
                for line in lines {
                    if writeln!(out, "{line}").is_err() {
                        return Ok(0);
                    }
                }
            }
            // 묶음마다 한 번(줄마다 하면 많은 기록을 읽을 때 줄마다 쓰기 호출이 된다).
            if out.flush().is_err() {
                return Ok(0);
            }
            if done {
                return Ok(0);
            }
            tokio::time::sleep(FOLLOW_POLL).await;
        }
    }

    /// [`output::Sandbox`]. 이미지 참조는 캐시에서 찾는다(지워졌으면 `null`).
    fn sandbox_json(&self, s: &SandboxInfo) -> output::Sandbox {
        let reference = sbx_store::Store::open(&self.cache_dir)
            .image(&s.spec.image)
            .ok()
            .map(|r| r.reference);
        output::Sandbox::new(s, reference)
    }

    /// 여러 대상 중 하나의 오류: `sbx: 대상: 오류`, JSON이면 [`output::ErrorLine`].
    fn target_error(&self, target: &str, e: &dyn fmt::Display) {
        if self.json() {
            output::print_error(Some(target), &e.to_string(), None);
        } else {
            eprintln!("sbx: {target}: {e}");
        }
    }

    /// ID(또는 그 앞부분, 이름) 여러 개에 같은 동작을 한다. 하나가 실패해도 나머지를 계속하고,
    /// 성공한 것은 받은 그대로 출력한다(Docker와 같다). JSON이면 [`output::Done`] 한 줄씩.
    async fn each(
        &self,
        rt: &Runtime,
        ids: &[String],
        f: impl AsyncFn(SandboxId) -> AppResult<()>,
    ) -> AppResult<u8> {
        self.each_with(rt, ids, async |id| f(id).await.map(|()| None))
            .await
    }

    /// [`each`](Self::each)와 같고, `f`가 종료 코드를 돌려주면 받은 ID 대신 그 코드를 출력한다
    /// (JSON이면 [`output::Exited`]).
    async fn each_with(
        &self,
        rt: &Runtime,
        ids: &[String],
        f: impl AsyncFn(SandboxId) -> AppResult<Option<i32>>,
    ) -> AppResult<u8> {
        let mut failed = 0;
        for raw in ids {
            let result = match rt.resolve(raw).await {
                Ok(id) => f(id.clone()).await.map(|code| (id, code)),
                Err(e) => Err(e.into()),
            };
            match result {
                Ok((id, code)) if self.json() => {
                    let (target, id) = (raw.clone(), id.to_string());
                    match code {
                        Some(exit_code) => output::print_line(&output::Exited {
                            target,
                            id,
                            exit_code,
                        })?,
                        None => output::print_line(&output::Done { target, id })?,
                    }
                }
                Ok((_, Some(code))) => println!("{code}"),
                Ok((_, None)) => println!("{raw}"),
                Err(e) => {
                    self.target_error(raw, &e);
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            return Err(msg(format!("{failed} of {} sandbox(es) failed", ids.len())));
        }
        Ok(0)
    }
}

/// `--env-file`의 값들 뒤에 `-e`를 붙여 해석한다. 같은 이름은 뒤의 것(-e)이 이긴다(Docker와 같다).
fn env_values(files: &[PathBuf], env: &[String]) -> AppResult<Vec<(String, String)>> {
    let mut all = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f)
            .map_err(|e| msg(format!("--env-file {}: {e}", f.display())))?;
        all.extend(
            parse::env_file(&text).map_err(|e| msg(format!("--env-file {}: {e}", f.display())))?,
        );
    }
    all.extend_from_slice(env);
    Ok(parse::env(&all, |k| std::env::var(k).ok()))
}

/// 필터 값을 이미지 참조로 정규화한다(`alpine` → `docker.io/library/alpine:latest`). 참조가 아니면 `None`.
fn normalized(value: &str) -> Option<String> {
    Reference::parse(value).ok().map(|r| r.to_string())
}

fn join_ids(ids: &[SandboxId]) -> String {
    ids.iter()
        .map(SandboxId::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ps`에 보이는 ID (앞 12자, Docker와 같다). 명령에는 이것이나 더 짧은 앞부분을 줘도 된다.
fn short_id(id: &SandboxId) -> &str {
    let s = id.as_str();
    &s[..s.len().min(12)]
}
