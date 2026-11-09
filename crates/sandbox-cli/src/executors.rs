//! `sbx executor ...`: 설치한 실행기(`executor-install`)와 실행기 저장소(`executor-repo`).
//!
//! | 무엇 | 위치 |
//! |---|---|
//! | 설치한 실행기 | `<data_dir>/executors` |
//! | 저장소 목록 | `<data_dir>/executor-repositories` (한 줄에 `<이름> <URL>`, 줄 순서가 우선순위) |
//! | 받는 중인 묶음 | `<cache_dir>/executor-downloads/<임시>` (끝나면 지운다) |
//!
//! - `install NAME[@VERSION]`: 저장소에서 고르고(규칙은 `executor-repo` 문서), 받아서 확인하고 설치한다. 고른 버전이
//!   이미 설치돼 있으면 아무것도 하지 않고 성공한다 (표준 오류에 알린다). 받지 못한 저장소는 경고하고 건너뛴다.
//! - `install --from PATH NAME@VERSION`: 받지 않는다. 같은 것이 이미 있으면 거부한다(내용이 다를 수 있다).
//! - 설치 전 확인: 실행기의 `capabilities`가 규약 버전과 이 호스트 아키텍처에 맞아야 한다.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use executor_install::{Executors, InstallError, Installed, Spec};
use executor_process::ProcessExecutor;
use executor_repo::{Fetcher, Platform, Repository, Sources, index};
use sandbox_executor::Executor;
use sandbox_model::Arch;

use crate::cli::{ExecutorCommand, RepoCommand};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// 설치한 실행기들.
pub fn installed(data_dir: &Path) -> Executors {
    Executors::new(data_dir.join("executors"))
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
            println!("{}", install_local(cx, &ex, &spec, from)?);
            Ok(0)
        }
        ExecutorCommand::Install { from: None, spec } => {
            install_remote(cx, &ex, &spec.parse()?).await?;
            Ok(0)
        }
        ExecutorCommand::Search { name } => search(cx, name.as_deref()).await,
        ExecutorCommand::Use { spec } => {
            println!("{}", ex.set_default(&spec.parse()?)?);
            Ok(0)
        }
        ExecutorCommand::Rm { spec } => {
            println!("{}", ex.remove(&spec.parse()?)?);
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
            .map(|i| {
                serde_json::json!({
                    "name": i.name,
                    "version": i.version,
                    "default": default.as_ref() == Some(i),
                    "path": i.executable,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
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
        return Ok(ex.install(spec, from, verify)?);
    }
    let work = work_dir(cx);
    let r = executor_repo::unpack::unpack(from, &work)
        .map_err(Box::<dyn Error>::from)
        .and_then(|dir| Ok(ex.install(spec, &dir, verify)?));
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
    let (ok, failed) = executor_repo::indexes(&fetcher, &repos).await;
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
    let c = index::pick(&idx, spec, &Platform::host(), executor_proto::VERSION)?;
    let want = c.spec();
    if let Some(i) = ex.find(&want)? {
        eprintln!("{i} is already installed");
        println!("{i}");
        return Ok(());
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
        let dir = executor_repo::fetch_bundle(&fetcher, &c, &work).await?;
        match ex.install(&want, &dir, verify) {
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
    let installed = r?;
    println!("{installed}");
    Ok(())
}

async fn search(cx: &Ctx<'_>, name: Option<&str>) -> Result<u8> {
    let (_, idx) = indexes(cx).await?;
    let list = index::available(&idx, name, &Platform::host(), executor_proto::VERSION);
    if cx.json {
        let v: Vec<_> = list
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "version": c.version,
                    "protocol": c.protocol,
                    "repository": c.repository,
                    "os": c.artifact.platform.os,
                    "architecture": c.artifact.platform.architecture,
                    "url": c.artifact.url.as_str(),
                    "sha256": c.artifact.sha256,
                    "size": c.artifact.size,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(0);
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
                let v: Vec<_> = list
                    .iter()
                    .map(|r| serde_json::json!({"name": r.name, "url": r.url.as_str()}))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{:<20} URL", "NAME");
                for r in &list {
                    println!("{:<20} {}", r.name, r.url);
                }
            }
        }
        RepoCommand::Add { name, url } => {
            s.add(Repository::new(name, url)?)?;
            println!("{name}");
        }
        RepoCommand::Rm { name } => {
            println!("{}", s.remove(name)?.name);
        }
    }
    Ok(0)
}
