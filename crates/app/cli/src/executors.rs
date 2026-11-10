//! `sbx executor ...`: 설치한 실행기(`sbx-executor-install`)와 실행기 저장소(`sbx-executor-repo`).
//!
//! | 무엇 | 위치 |
//! |---|---|
//! | 설치한 실행기 | `<data_dir>/executors` |
//! | 저장소 목록 | `<data_dir>/executor-repositories` (한 줄에 `<이름> <URL>`, 줄 순서가 우선순위) |
//! | 받는 중인 묶음 | `<cache_dir>/executor-downloads/<임시>` (끝나면 지운다) |
//!
//! - `install NAME[@VERSION]`: 저장소에서 고르고(규칙은 `sbx-executor-repo` 문서), 받아서 확인하고 설치한다. 고른 버전이
//!   이미 설치돼 있으면 아무것도 하지 않고 성공한다 (표준 오류에 알린다). 받지 못한 저장소는 경고하고 건너뛴다.
//! - `install --from PATH NAME@VERSION`: 받지 않는다. 같은 것이 이미 있으면 거부한다(내용이 다를 수 있다).
//! - 설치 전 확인: 실행기의 `capabilities`가 규약 버전과 이 호스트 아키텍처에 맞아야 한다.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sbx_executor::Executor;
use sbx_executor_install::{Executors, InstallError, Installed, Spec};
use sbx_executor_process::ProcessExecutor;
use sbx_executor_repo::{Fetcher, Platform, Repository, Sources, index};
use sbx_model::Arch;

use crate::cli::{ExecutorCommand, RepoCommand};
use crate::output;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// 설치한 실행기들.
pub fn installed(data_dir: &Path) -> Executors {
    Executors::new(crate::dirs::executors_dir(data_dir))
}

/// 공식 실행기 저장소. 묶음은 GitHub Releases에, `index.json`은 GitHub Pages에 있다(CI가 올린다).
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
}

/// `executor ...`를 실행한다. 종료 코드를 돌려준다.
pub async fn run(cx: &Ctx<'_>, c: &ExecutorCommand) -> Result<u8> {
    let ex = installed(cx.data_dir);
    match c {
        ExecutorCommand::Ls => ls(cx, &ex),
        ExecutorCommand::Install {
            from: Some(from),
            spec,
        } => {
            let spec: Spec = spec.parse()?;
            print_executor(cx, &install_local(cx, &ex, &spec, from)?)?;
            Ok(0)
        }
        ExecutorCommand::Install { from: None, spec } => {
            install_remote(cx, &ex, &spec.parse()?).await?;
            Ok(0)
        }
        ExecutorCommand::Search { name } => search(cx, name.as_deref()).await,
        ExecutorCommand::Use { spec } => {
            print_executor(cx, &ex.set_default(&spec.parse()?)?)?;
            Ok(0)
        }
        ExecutorCommand::Rm { spec } => {
            print_executor(cx, &ex.remove(&spec.parse()?)?)?;
            Ok(0)
        }
        ExecutorCommand::Repo { command } => repo(cx, command),
    }
}

fn ls(cx: &Ctx<'_>, ex: &Executors) -> Result<u8> {
    let default = ex.default()?;
    let list = ex.list()?;
    if cx.json {
        let v: Vec<_> = list
            .iter()
            .map(|i| output::InstalledExecutor {
                name: i.name.clone(),
                version: i.version.clone(),
                default: default.as_ref() == Some(i),
                path: i.executable.display().to_string(),
            })
            .collect();
        output::print_value(&v)?;
        return Ok(0);
    }
    println!("{:<2}{:<20} {:<14} PATH", "", "NAME", "VERSION");
    for i in &list {
        let mark = if default.as_ref() == Some(i) { "*" } else { "" };
        println!(
            "{mark:<2}{:<20} {:<14} {}",
            i.name,
            i.version,
            i.executable.display()
        );
    }
    Ok(0)
}

/// 설치하기 전에 실행기를 확인한다: 규약을 말하고(`ProcessExecutor::new`가 규약 버전을 본다) 이 호스트 아키텍처를 돌린다.
fn verify(exe: &Path) -> std::result::Result<(), String> {
    let arch = Arch::host().ok_or("this host architecture is not supported")?;
    let p = ProcessExecutor::new(exe).map_err(|e| e.to_string())?;
    let caps = p.capabilities();
    if caps.arch != arch {
        return Err(format!(
            "executor {} runs {:?} guests; this host is {arch:?}",
            caps.name, caps.arch
        ));
    }
    Ok(())
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

fn install_local(cx: &Ctx<'_>, ex: &Executors, spec: &Spec, from: &Path) -> Result<Installed> {
    if !is_archive(from) {
        return Ok(ex.install(spec, from, None, |i| verify(&i.executable))?);
    }
    let work = work_dir(cx);
    let r =
        sbx_executor_repo::unpack::unpack(from, &work, sbx_executor_repo::unpack::Layout::Executor)
            .map_err(Box::<dyn Error>::from)
            .and_then(|dir| Ok(ex.install(spec, &dir, None, |i| verify(&i.executable))?));
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

async fn install_remote(cx: &Ctx<'_>, ex: &Executors, spec: &Spec) -> Result<()> {
    let (fetcher, idx) = indexes(cx).await?;
    let c = index::pick(&idx, spec, &Platform::host(), sbx_executor_proto::VERSION)?;
    let want = c.spec();
    if let Some(i) = ex.find(&want)? {
        eprintln!("{i} is already installed");
        return print_executor(cx, &i);
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
        let dir = sbx_executor_repo::fetch_bundle(&fetcher, &c, &work).await?;
        match ex.install(&want, &dir, None, |i| verify(&i.executable)) {
            Ok(i) => Ok(i),
            // 그사이 다른 쪽이 설치했다.
            Err(InstallError::AlreadyInstalled(_)) => Ok(ex
                .find(&want)?
                .ok_or_else(|| format!("{want:?} vanished during install"))?),
            Err(e) => Err(e.into()),
        }
    }
    .await;
    let _ = std::fs::remove_dir_all(&work);
    print_executor(cx, &r?)
}

/// 실행기 하나를 출력한다: `이름@버전`, JSON이면 [`output::ExecutorRef`].
fn print_executor(cx: &Ctx<'_>, i: &Installed) -> Result<()> {
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

async fn search(cx: &Ctx<'_>, name: Option<&str>) -> Result<u8> {
    let (_, idx) = indexes(cx).await?;
    let list = index::available(&idx, name, &Platform::host(), sbx_executor_proto::VERSION);
    if cx.json {
        let v: Vec<_> = list
            .iter()
            .map(|c| output::AvailableExecutor {
                name: c.name.clone(),
                version: c.version.clone(),
                protocol: c.protocol,
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
        let p = Platform::host();
        eprintln!(
            "sbx: no executor{} in the configured repositories runs on {}/{}{}",
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
    println!("{:<20} {:<14} {:<10} REPOSITORY", "NAME", "VERSION", "SIZE");
    for c in &list {
        println!(
            "{:<20} {:<14} {:<10} {}",
            c.name,
            c.version,
            crate::behavior::human_size(c.artifact.size),
            c.repository
        );
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
