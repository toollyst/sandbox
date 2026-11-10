//! `sbx executor ...`: 설치한 실행기와 어댑터(`sbx-executor-install`), 실행기 저장소(`sbx-executor-repo`).
//!
//! | 무엇 | 위치 |
//! |---|---|
//! | 설치한 실행기(자체 완결 실행기와 VMM 묶음) | `<data_dir>/executors` |
//! | 설치한 어댑터 | `<data_dir>/adapters` |
//! | 저장소 목록 | `<data_dir>/executor-repositories` (한 줄에 `<이름> <URL>`, 줄 순서가 우선순위) |
//! | 받는 중인 묶음 | `<cache_dir>/executor-downloads/<임시>` (끝나면 지운다) |
//!
//! sbx만 설치하면 실행기도 어댑터도 없다(`executor ls`가 비어 있다). 둘은 따로 설치한다.
//!
//! - `install NAME[@VERSION]`: 저장소에서 고르고(규칙은 `sbx-executor-repo` 문서), 받아서 확인하고 설치한다. 고른 버전이
//!   이미 설치돼 있으면 아무것도 하지 않고 성공한다 (표준 오류에 알린다). 받지 못한 저장소는 경고하고 건너뛴다.
//!   VMM 묶음이면 그것이 가리키는 어댑터가 설치돼 있는지 먼저 보고, 없으면 어댑터부터 받아 설치한다.
//! - `install --from PATH NAME@VERSION [--adapter A]`: 받지 않는다. 같은 것이 이미 있으면 거부한다(내용이 다를 수 있다).
//!   `--adapter`면 VMM 묶음이고, 어댑터 A가 설치돼 있어야 한다.
//! - 설치 전 확인: 실행기(VMM 묶음이면 그 디렉터리를 받은 어댑터)의 `capabilities`가 규약 버전과 이 호스트 아키텍처에
//!   맞아야 한다. 어댑터만 설치할 때는 확인하지 않는다(VMM 없이는 답하지 않을 수 있다).
//! - `rm`은 샌드박스가 고정한 실행기([`crate::choose::PIN_LABEL`])를, `adapter rm`은 설치한 VMM 묶음이 쓰는 어댑터의
//!   마지막 버전을 지우지 않는다(`-f`로 지운다).
//! - `prune`: 기본도 아니고, 그 이름의 가장 높은 버전도 아니고, 샌드박스가 고정하지도 않은 실행기 버전과, 가장 높은
//!   버전이 아닌 어댑터 버전을 지운다.

use std::error::Error;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sbx_executor::Executor;
use sbx_executor_install::{Executors, InstallError, Installed, Spec};
use sbx_executor_process::ProcessExecutor;
use sbx_executor_repo::{Candidate, Fetcher, Platform, Repository, Sources, index};
use sbx_model::Arch;

use crate::choose;
use crate::cli::{AdapterCommand, ExecutorCommand, RepoCommand};
use crate::output;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// 설치한 실행기들.
pub fn installed(data_dir: &Path) -> Executors {
    Executors::new(crate::dirs::executors_dir(data_dir))
}

/// 설치한 어댑터들.
pub fn adapters(data_dir: &Path) -> Executors {
    Executors::new(data_dir.join("adapters"))
}

/// 공식 실행기 저장소. 묶음은 GitHub Releases에, 색인은 GitHub Pages에 있다(CI가 올린다).
pub const OFFICIAL_REPOSITORY: (&str, &str) =
    ("official", "https://toollyst.github.io/sandbox/executors/");

/// 저장소 목록 파일. 파일이 없으면 공식 저장소 하나다(`sbx executor repo rm official`로 뺄 수 있다).
pub fn sources(data_dir: &Path) -> Sources {
    let (name, url) = OFFICIAL_REPOSITORY;
    Sources::new(data_dir.join("executor-repositories"))
        .with_defaults(vec![Repository::new(name, url).expect("valid")])
}

/// `executor` 명령에 필요한 것.
pub struct Ctx<'a> {
    /// `<data_dir>`.
    pub data_dir: &'a Path,
    /// `<cache_dir>`.
    pub cache_dir: &'a Path,
    /// `--format json`.
    pub json: bool,
    /// `--executor`.
    pub flag: Option<&'a Path>,
    /// 샌드박스(이름이나 ID)와 그것이 고정한 실행기. `rm`과 `prune`만 쓴다.
    pub pins: &'a [(String, String)],
}

/// `pins`가 필요한 명령인지. 필요할 때만 샌드박스 목록을 읽는다.
pub fn needs_pins(c: &ExecutorCommand) -> bool {
    matches!(
        c,
        ExecutorCommand::Rm { force: false, .. } | ExecutorCommand::Prune { .. }
    )
}

/// `executor ...`를 실행한다. 종료 코드를 돌려준다.
pub async fn run(cx: &Ctx<'_>, c: &ExecutorCommand) -> Result<u8> {
    let ex = installed(cx.data_dir);
    let ad = adapters(cx.data_dir);
    match c {
        ExecutorCommand::Ls => ls(cx, &ex),
        ExecutorCommand::Install {
            from: Some(from),
            adapter,
            spec,
        } => {
            let spec: Spec = spec.parse()?;
            let i = install_local(cx, &ex, &ad, &spec, from, adapter.as_deref())?;
            print_ref(cx, &i)?;
            Ok(0)
        }
        ExecutorCommand::Install {
            from: None, spec, ..
        } => {
            install_remote(cx, &ex, &ad, &spec.parse()?).await?;
            Ok(0)
        }
        ExecutorCommand::Search { name } => search(cx, name.as_deref(), Kind::Executor).await,
        ExecutorCommand::Use { spec } => {
            print_ref(cx, &ex.set_default(&spec.parse()?)?)?;
            Ok(0)
        }
        ExecutorCommand::Current => {
            let c = chosen(cx, &ex, &ad, None)?
                .ok_or("no executor is installed; install one with: sbx executor install wick")?;
            print_chosen(cx, &c, false)
        }
        ExecutorCommand::Which { spec } => {
            let flag = spec.as_deref().map(Path::new);
            if let Some(s) = spec {
                // 경로가 아니라 설치한 것만.
                let spec: Spec = s.parse()?;
                if ex.find(&spec)?.is_none() {
                    return Err(format!("{spec} is not installed").into());
                }
            }
            let c = chosen(cx, &ex, &ad, flag)?.ok_or("no executor is installed")?;
            print_chosen(cx, &c, true)
        }
        ExecutorCommand::Rm { force, spec } => {
            let spec: Spec = spec.parse()?;
            if !force && let Some(found) = ex.find(&spec)?.filter(|_| spec.version.is_some()) {
                let users = pinned_by(cx, &found);
                if !users.is_empty() {
                    return Err(format!(
                        "{found} is pinned by sandbox{} {}; remove {} first, or use -f",
                        if users.len() > 1 { "es" } else { "" },
                        users.join(", "),
                        if users.len() > 1 { "them" } else { "it" },
                    )
                    .into());
                }
            }
            print_ref(cx, &ex.remove(&spec)?)?;
            Ok(0)
        }
        ExecutorCommand::Prune { dry_run } => prune(cx, &ex, &ad, *dry_run),
        ExecutorCommand::Adapter { command } => adapter(cx, &ex, &ad, command).await,
        ExecutorCommand::Repo { command } => repo(cx, command),
    }
}

fn chosen(
    cx: &Ctx<'_>,
    ex: &Executors,
    ad: &Executors,
    flag: Option<&Path>,
) -> Result<Option<choose::Chosen>> {
    let cwd = std::env::current_dir().ok();
    Ok(choose::choose(&choose::Inputs {
        executors: ex,
        adapters: ad,
        flag: flag.or(cx.flag),
        pinned: None,
        cwd: cwd.as_deref(),
        on_path: &choose::on_path,
    })?)
}

fn print_chosen(cx: &Ctx<'_>, c: &choose::Chosen, program_only: bool) -> Result<u8> {
    if cx.json {
        output::print_value(&output::ChosenExecutor {
            name: c.installed.as_ref().map(|i| i.name.clone()),
            version: c.installed.as_ref().map(|i| i.version.clone()),
            source: c.source.to_string(),
            program: c.program.display().to_string(),
            adapter: c.adapter.as_ref().map(ToString::to_string),
            vmm_dir: c.vmm_dir().map(|d| d.display().to_string()),
        })?;
    } else if program_only {
        println!("{}", c.program.display());
    } else {
        println!("{} ({})", c.label(), c.source);
    }
    Ok(0)
}

/// `found`를 고정한 샌드박스들.
fn pinned_by(cx: &Ctx<'_>, found: &Installed) -> Vec<String> {
    let want = found.to_string();
    cx.pins
        .iter()
        .filter(|(_, pin)| *pin == want)
        .map(|(sandbox, _)| sandbox.clone())
        .collect()
}

fn ls(cx: &Ctx<'_>, ex: &Executors) -> Result<u8> {
    let default = ex.default()?;
    let list = ex.list()?;
    let path = |i: &Installed| match i.adapter {
        Some(_) => i.dir.display().to_string(),
        None => i.executable.display().to_string(),
    };
    if cx.json {
        let v: Vec<_> = list
            .iter()
            .map(|i| output::InstalledExecutor {
                name: i.name.clone(),
                version: i.version.clone(),
                default: default.as_ref() == Some(i),
                path: path(i),
                adapter: i.adapter.clone(),
            })
            .collect();
        output::print_value(&v)?;
        return Ok(0);
    }
    println!(
        "{:<2}{:<20} {:<14} {:<14} PATH",
        "", "NAME", "VERSION", "ADAPTER"
    );
    for i in &list {
        let mark = if default.as_ref() == Some(i) { "*" } else { "" };
        println!(
            "{mark:<2}{:<20} {:<14} {:<14} {}",
            i.name,
            i.version,
            i.adapter.as_deref().unwrap_or("-"),
            path(i)
        );
    }
    Ok(0)
}

/// 설치하기 전에 실행기를 확인한다: 규약을 말하고(`ProcessExecutor`가 규약 버전을 본다) 이 호스트 아키텍처를 돌린다.
/// `env`는 실행기에 더할 환경 변수(VMM 묶음이면 그 디렉터리).
fn verify(exe: &Path, env: Vec<(OsString, OsString)>) -> std::result::Result<(), String> {
    let arch = Arch::host().ok_or("this host architecture is not supported")?;
    let p = ProcessExecutor::with_env(exe, env).map_err(|e| e.to_string())?;
    let caps = p.capabilities();
    if caps.arch != arch {
        return Err(format!(
            "executor {} runs {:?} guests; this host is {arch:?}",
            caps.name, caps.arch
        ));
    }
    Ok(())
}

/// 어댑터는 혼자서는 확인하지 않는다. VMM 묶음 없이는 `capabilities`에 답하지 않을 수 있다(firecracker 어댑터는
/// VMM과 커널을 찾을 수 있어야 답한다). 규약 버전과 아키텍처는 VMM 묶음을 설치할 때 그 디렉터리와 함께 확인한다.
fn adapter_check(_: &Installed) -> std::result::Result<(), String> {
    Ok(())
}

fn install_adapter_local(
    cx: &Ctx<'_>,
    ad: &Executors,
    spec: &Spec,
    from: &Path,
) -> Result<Installed> {
    if !is_archive(from) {
        return Ok(ad.install(spec, from, None, adapter_check)?);
    }
    let work = work_dir(cx);
    let r =
        sbx_executor_repo::unpack::unpack(from, &work, sbx_executor_repo::unpack::Layout::Executor)
            .map_err(Box::<dyn Error>::from)
            .and_then(|dir| Ok(ad.install(spec, &dir, None, adapter_check)?));
    let _ = std::fs::remove_dir_all(&work);
    r
}

/// 설치하는 것(임시 위치)을 확인한다. VMM 묶음이면 설치한 어댑터에 그 디렉터리를 주고 묻는다.
fn verify_installed(ad: &Executors, i: &Installed) -> std::result::Result<(), String> {
    match &i.adapter {
        None => verify(&i.executable, Vec::new()),
        Some(name) => {
            let spec = Spec {
                name: name.clone(),
                version: None,
            };
            let adapter = ad
                .find(&spec)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| {
                    format!(
                        "adapter {name} is not installed; install it with: sbx executor adapter install {name}"
                    )
                })?;
            verify(
                &adapter.executable,
                vec![(
                    sbx_executor_proto::VMM_DIR_ENV.into(),
                    i.dir.as_os_str().to_owned(),
                )],
            )
        }
    }
}

/// 받는 중인 묶음을 둘 새 디렉터리. 돌려받은 쪽이 지운다.
fn work_dir(cx: &Ctx<'_>) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    cx.cache_dir.join("executor-downloads").join(format!(
        "{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn is_archive(p: &Path) -> bool {
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    p.is_file() && (name.ends_with(".tar.gz") || name.ends_with(".tgz"))
}

fn install_local(
    cx: &Ctx<'_>,
    ex: &Executors,
    ad: &Executors,
    spec: &Spec,
    from: &Path,
    adapter: Option<&str>,
) -> Result<Installed> {
    if !is_archive(from) {
        return Ok(ex.install(spec, from, adapter, |i| verify_installed(ad, i))?);
    }
    let layout = match adapter {
        Some(_) => sbx_executor_repo::unpack::Layout::Vmm,
        None => sbx_executor_repo::unpack::Layout::Executor,
    };
    let work = work_dir(cx);
    let r = sbx_executor_repo::unpack::unpack(from, &work, layout)
        .map_err(Box::<dyn Error>::from)
        .and_then(|dir| Ok(ex.install(spec, &dir, adapter, |i| verify_installed(ad, i))?));
    let _ = std::fs::remove_dir_all(&work);
    r
}

async fn indexes(cx: &Ctx<'_>) -> Result<(Fetcher, Vec<(String, index::Index)>)> {
    let repos = sources(cx.data_dir).list()?;
    if repos.is_empty() {
        return Err(
            "no executor repositories; add one with `sbx executor repo add NAME URL`".into(),
        );
    }
    let fetcher = Fetcher::new()?;
    let (ok, failed) = sbx_executor_repo::indexes(&fetcher, &repos).await;
    for e in &failed {
        eprintln!("warning: {e}");
    }
    if ok.is_empty() {
        return Err("no executor repository could be read".into());
    }
    Ok((fetcher, ok))
}

async fn install_remote(cx: &Ctx<'_>, ex: &Executors, ad: &Executors, spec: &Spec) -> Result<()> {
    let (fetcher, idx) = indexes(cx).await?;
    let platform = Platform::host();
    let c = index::pick(&idx, spec, &platform, sbx_executor_proto::VERSION)?;
    // VMM 묶음이면 어댑터부터. 이미 있으면(어느 버전이든) 그것을 쓴다.
    if let Some(name) = &c.adapter {
        let a = Spec {
            name: name.clone(),
            version: None,
        };
        if ad.find(&a)?.is_none() {
            let ac = index::pick_adapter(&idx, &a, &platform, sbx_executor_proto::VERSION)
                .map_err(|e| format!("{}@{} needs adapter {name}: {e}", c.name, c.version))?;
            eprintln!("{}@{} needs adapter {name}", c.name, c.version);
            fetch_and_install(cx, &fetcher, ad, &ac, adapter_check).await?;
        }
    }
    let i = fetch_and_install(cx, &fetcher, ex, &c, |i| verify_installed(ad, i)).await?;
    print_ref(cx, &i)
}

/// `c`를 받아 `into`에 설치한다. 이미 있으면 받지 않는다.
async fn fetch_and_install(
    cx: &Ctx<'_>,
    fetcher: &Fetcher,
    into: &Executors,
    c: &Candidate,
    check: impl FnOnce(&Installed) -> std::result::Result<(), String>,
) -> Result<Installed> {
    let want = c.spec();
    if let Some(i) = into.find(&want)? {
        eprintln!("{i} is already installed");
        return Ok(i);
    }
    eprintln!(
        "downloading {}@{} ({}) from {}",
        c.name,
        c.version,
        crate::behavior::human_size(c.artifact.size),
        c.repository
    );
    let work = work_dir(cx);
    let r: Result<Installed> = async {
        let dir = sbx_executor_repo::fetch_bundle(fetcher, c, &work).await?;
        match into.install(&want, &dir, c.adapter.as_deref(), check) {
            Ok(i) => Ok(i),
            // 그사이 다른 쪽이 설치했다.
            Err(InstallError::AlreadyInstalled(_)) => Ok(into
                .find(&want)?
                .ok_or_else(|| format!("{want} vanished during install"))?),
            Err(e) => Err(e.into()),
        }
    }
    .await;
    let _ = std::fs::remove_dir_all(&work);
    r
}

/// 실행기나 어댑터 하나를 출력한다: `이름@버전`, JSON이면 [`output::ExecutorRef`].
fn print_ref(cx: &Ctx<'_>, i: &Installed) -> Result<()> {
    if cx.json {
        output::print_value(&output::ExecutorRef {
            name: i.name.clone(),
            version: i.version.clone(),
        })?;
    } else {
        println!("{i}");
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Executor,
    Adapter,
}

async fn search(cx: &Ctx<'_>, name: Option<&str>, kind: Kind) -> Result<u8> {
    let (_, idx) = indexes(cx).await?;
    let (p, v) = (Platform::host(), sbx_executor_proto::VERSION);
    let list = match kind {
        Kind::Executor => index::available(&idx, name, &p, v),
        Kind::Adapter => index::available_adapters(&idx, name, &p, v),
    };
    if cx.json {
        let v: Vec<_> = list
            .iter()
            .map(|c| output::AvailableExecutor {
                name: c.name.clone(),
                version: c.version.clone(),
                protocol: c.protocol,
                adapter: c.adapter.clone(),
                repository: c.repository.clone(),
                os: c.artifact.platform.os.clone(),
                architecture: c.artifact.platform.architecture.clone(),
                url: c.artifact.url.to_string(),
                sha256: c.artifact.sha256.clone(),
                size: c.artifact.size,
            })
            .collect();
        output::print_value(&v)?;
        return Ok(0);
    }
    if list.is_empty() {
        eprintln!(
            "sbx: no {}{} in the configured repositories runs on {}/{}{}",
            if kind == Kind::Adapter {
                "adapter"
            } else {
                "executor"
            },
            name.map(|n| format!(" named {n:?}")).unwrap_or_default(),
            p.os,
            p.architecture,
            if matches!(std::env::consts::OS, "linux" | "macos") {
                ""
            } else {
                " (sbx cannot run sandboxes on this OS yet)"
            }
        );
    }
    if kind == Kind::Executor {
        println!(
            "{:<20} {:<14} {:<14} {:<10} REPOSITORY",
            "NAME", "VERSION", "ADAPTER", "SIZE"
        );
    } else {
        println!("{:<20} {:<14} {:<10} REPOSITORY", "NAME", "VERSION", "SIZE");
    }
    for c in &list {
        let size = crate::behavior::human_size(c.artifact.size);
        if kind == Kind::Executor {
            println!(
                "{:<20} {:<14} {:<14} {:<10} {}",
                c.name,
                c.version,
                c.adapter.as_deref().unwrap_or("-"),
                size,
                c.repository
            );
        } else {
            println!(
                "{:<20} {:<14} {:<10} {}",
                c.name, c.version, size, c.repository
            );
        }
    }
    Ok(0)
}

/// 설치한 VMM 묶음들이 쓰는 어댑터 이름 → 그 묶음들.
fn adapter_users(ex: &Executors) -> Result<Vec<(String, String)>> {
    Ok(ex
        .list()?
        .into_iter()
        .filter_map(|i| i.adapter.clone().map(|a| (a, i.to_string())))
        .collect())
}

async fn adapter(cx: &Ctx<'_>, ex: &Executors, ad: &Executors, c: &AdapterCommand) -> Result<u8> {
    match c {
        AdapterCommand::Ls => {
            let users = adapter_users(ex)?;
            let list = ad.list()?;
            let used = |i: &Installed| -> Vec<String> {
                users
                    .iter()
                    .filter(|(a, _)| *a == i.name)
                    .map(|(_, u)| u.clone())
                    .collect()
            };
            if cx.json {
                let v: Vec<_> = list
                    .iter()
                    .map(|i| output::InstalledAdapter {
                        name: i.name.clone(),
                        version: i.version.clone(),
                        path: i.executable.display().to_string(),
                        used_by: used(i),
                    })
                    .collect();
                output::print_value(&v)?;
                return Ok(0);
            }
            println!("{:<20} {:<14} {:<24} PATH", "NAME", "VERSION", "USED BY");
            for i in &list {
                let u = used(i);
                println!(
                    "{:<20} {:<14} {:<24} {}",
                    i.name,
                    i.version,
                    if u.is_empty() {
                        "-".into()
                    } else {
                        u.join(",")
                    },
                    i.executable.display()
                );
            }
            Ok(0)
        }
        AdapterCommand::Install {
            from: Some(from),
            spec,
        } => {
            let spec: Spec = spec.parse()?;
            let i = install_adapter_local(cx, ad, &spec, from)?;
            print_ref(cx, &i)?;
            Ok(0)
        }
        AdapterCommand::Install { from: None, spec } => {
            let (fetcher, idx) = indexes(cx).await?;
            let c = index::pick_adapter(
                &idx,
                &spec.parse()?,
                &Platform::host(),
                sbx_executor_proto::VERSION,
            )?;
            let i = fetch_and_install(cx, &fetcher, ad, &c, adapter_check).await?;
            print_ref(cx, &i)?;
            Ok(0)
        }
        AdapterCommand::Search { name } => search(cx, name.as_deref(), Kind::Adapter).await,
        AdapterCommand::Rm { force, spec } => {
            let spec: Spec = spec.parse()?;
            if !force && let Some(found) = ad.find(&spec)?.filter(|_| spec.version.is_some()) {
                let others = ad
                    .list()?
                    .iter()
                    .any(|i| i.name == found.name && *i != found);
                let users: Vec<String> = adapter_users(ex)?
                    .into_iter()
                    .filter(|(a, _)| *a == found.name)
                    .map(|(_, u)| u)
                    .collect();
                if !others && !users.is_empty() {
                    return Err(format!(
                        "{found} is the last {} adapter and {} needs it; remove {} first, or use -f",
                        found.name,
                        users.join(", "),
                        if users.len() > 1 { "them" } else { "it" },
                    )
                    .into());
                }
            }
            print_ref(cx, &ad.remove(&spec)?)?;
            Ok(0)
        }
    }
}

/// 지울 것: 각 이름의 가장 높은 버전과 `keep`이 아닌 것.
fn stale(list: &[Installed], keep: &dyn Fn(&Installed) -> bool) -> Vec<Installed> {
    // `list`는 이름 순, 같은 이름은 버전 순이다.
    list.iter()
        .enumerate()
        .filter(|(n, i)| {
            let newest = list.get(n + 1).is_none_or(|next| next.name != i.name);
            !newest && !keep(i)
        })
        .map(|(_, i)| i.clone())
        .collect()
}

fn prune(cx: &Ctx<'_>, ex: &Executors, ad: &Executors, dry_run: bool) -> Result<u8> {
    let default = ex.default()?;
    let pins: Vec<&str> = cx.pins.iter().map(|(_, p)| p.as_str()).collect();
    let executors = stale(&ex.list()?, &|i| {
        default.as_ref() == Some(i) || pins.contains(&i.to_string().as_str())
    });
    let adapters = stale(&ad.list()?, &|_| false);
    let mut out = Vec::new();
    for (kind, from, list) in [("executor", ex, executors), ("adapter", ad, adapters)] {
        for i in list {
            if !dry_run {
                from.remove(&i.spec())?;
            }
            if !cx.json {
                println!("{}{kind} {i}", if dry_run { "would remove " } else { "" });
            }
            out.push(output::Pruned {
                kind: kind.to_owned(),
                name: i.name.clone(),
                version: i.version.clone(),
                removed: !dry_run,
            });
        }
    }
    if cx.json {
        output::print_value(&out)?;
    }
    Ok(0)
}

fn repo(cx: &Ctx<'_>, c: &RepoCommand) -> Result<u8> {
    let s = sources(cx.data_dir);
    match c {
        RepoCommand::Ls => {
            let list = s.list()?;
            if cx.json {
                let v: Vec<_> = list.iter().map(repository).collect();
                output::print_value(&v)?;
            } else {
                println!("{:<20} URL", "NAME");
                for r in &list {
                    println!("{:<20} {}", r.name, r.url);
                }
            }
        }
        RepoCommand::Add { name, url } => {
            let r = Repository::new(name, url)?;
            s.add(r.clone())?;
            print_repository(cx, &r)?;
        }
        RepoCommand::Rm { name } => {
            print_repository(cx, &s.remove(name)?)?;
        }
    }
    Ok(0)
}

fn repository(r: &Repository) -> output::Repository {
    output::Repository {
        name: r.name.clone(),
        url: r.url.to_string(),
    }
}

/// 저장소 하나를 출력한다: 이름, JSON이면 [`output::Repository`].
fn print_repository(cx: &Ctx<'_>, r: &Repository) -> Result<()> {
    if cx.json {
        output::print_value(&repository(r))?;
    } else {
        println!("{}", r.name);
    }
    Ok(())
}
