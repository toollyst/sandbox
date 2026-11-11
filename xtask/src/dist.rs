//! `dist`: 배포 빌드. 이 호스트 아키텍처의 게스트 init을 만들고, 그것을 내장한 `sbx`와 `sbx-shim`을 release로 빌드한다.
//!
//! 1. `guest-initramfs --arch <호스트 아키텍처>` (`target/guest/<arch>/initramfs`)
//! 2. `SBX_EMBED_GUEST`에 그 절대 경로를 주고 `cargo build --release -p sbx-cli -p sbx-shim`
//!
//! 결과는 `target/release/sbx`와 `target/release/sbx-shim`이다. 둘을 같은 디렉터리에 두면 sbx가 shim을 찾는다.
//!
//! `--auditable`: `cargo auditable build`로 빌드해 실행 파일 안에 의존성 목록(이름, 버전, 출처)을 넣는다
//! ([cargo-auditable](https://github.com/rust-secure-code/cargo-auditable)). 그러면 `cargo audit bin`, syft, Trivy가
//! 받은 실행 파일만 보고 알려진 취약점을 찾는다. 릴리스가 쓴다. `cargo install cargo-auditable`이 있어야 한다.
//!
//! `--target <triple>`: 그 대상으로 빌드한다(결과는 `target/<triple>/release/`). 릴리스는 Linux에서
//! `<arch>-unknown-linux-musl`로 정적 실행 파일을 만든다(어느 배포판에서나 돈다). 게스트 init의 아키텍처는 대상의
//! 아키텍처다.
//! 실행기(`sbx-executor`와 VMM)는 따로 설치한다. 다른 아키텍처용 배포는 그 아키텍처 호스트에서 만든다.
//!
//! `install [--dir <디렉터리>]`: `dist`로 만든 `sbx`와 `sbx-shim`을 설치 스크립트와 같은 OS 표준 위치에 둔다
//! ([`default_dir`]: macOS·Linux `~/.local/bin`, Windows `%LOCALAPPDATA%\Programs\sbx`). 빌드 디렉터리를
//! `PATH`에 넣지 않아도 되고, 다시 빌드해도 설치한 것이 바뀌지 않는다. 파일마다 옆에 임시 파일로 쓰고 바꿔 넣는다.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::initramfs;

/// 실행한다. 만든 실행 파일 경로를 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut target: Option<String> = None;
    let mut auditable = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = Some(it.next().ok_or("--target needs a value")?.clone()),
            "--auditable" => auditable = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let arch = match &target {
        Some(t) => t.split('-').next().unwrap_or_default(),
        None => std::env::consts::ARCH,
    };
    let guest = initramfs::run(root, &["--arch".into(), arch.into()])?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let mut build = Command::new(cargo);
    build.current_dir(root);
    if auditable {
        build.arg("auditable");
    }
    build.args(["build", "--release", "-p", "sbx-cli", "-p", "sbx-shim"]);
    if let Some(t) = &target {
        build.args(["--target", t]);
    }
    let status = build
        .env("SBX_EMBED_GUEST", &guest)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err("building sbx failed".into());
    }
    // 다른 대상은 Unix(Linux)만 만든다.
    let exe = if target.is_some() {
        ""
    } else {
        std::env::consts::EXE_SUFFIX
    };
    let release = match &target {
        Some(t) => crate::paths::target_dir(root).join(t).join("release"),
        None => crate::paths::target_dir(root).join("release"),
    };
    Ok(vec![
        release.join(format!("sbx{exe}")),
        release.join(format!("sbx-shim{exe}")),
    ])
}

/// 설치 스크립트(`install.sh`, `install.ps1`)의 기본 위치. `home`은 홈 디렉터리, `local_app_data`는 Windows의
/// `%LOCALAPPDATA%`다.
pub fn default_dir(
    windows: bool,
    home: Option<&Path>,
    local_app_data: Option<&Path>,
) -> Result<PathBuf, String> {
    if windows {
        local_app_data
            .map(|d| d.join("Programs").join("sbx"))
            .ok_or_else(|| "LOCALAPPDATA is not set; give --dir".into())
    } else {
        home.map(|h| h.join(".local").join("bin"))
            .ok_or_else(|| "HOME is not set; give --dir".into())
    }
}

/// `files`를 `dir`에 복사한다. 파일마다 `<이름>.new`에 쓰고 rename하므로 실패해도 반쯤 쓴 실행 파일이 남지 않는다.
pub fn copy_into(files: &[PathBuf], dir: &Path) -> Result<Vec<PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for f in files {
        let name = f
            .file_name()
            .ok_or_else(|| format!("{} has no file name", f.display()))?;
        let to = dir.join(name);
        let tmp = dir.join(format!("{}.new", name.to_string_lossy()));
        std::fs::copy(f, &tmp).map_err(|e| format!("{} -> {}: {e}", f.display(), tmp.display()))?;
        std::fs::rename(&tmp, &to).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot replace {} (is it running?): {e}", to.display())
        })?;
        out.push(to);
    }
    Ok(out)
}

/// `install`: 만들고 설치한다. 설치한 경로를 돌려준다.
pub fn install(root: &Path, args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut dir: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dir" => dir = Some(PathBuf::from(it.next().ok_or("--dir needs a value")?)),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    let dir = match dir {
        Some(d) => d,
        None => default_dir(
            cfg!(windows),
            std::env::var_os("HOME").map(PathBuf::from).as_deref(),
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .as_deref(),
        )?,
    };
    let built = run(root, &[])?;
    let installed = copy_into(&built, &dir)?;
    let on_path =
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d == dir));
    if !on_path {
        eprintln!(
            "install: {} is not on your PATH; add it to run sbx by name",
            dir.display()
        );
    }
    Ok(installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installs_where_the_install_scripts_do() {
        let home = Path::new("/home/u");
        let lad = Path::new(r"C:\Users\u\AppData\Local");
        assert_eq!(
            default_dir(false, Some(home), None).unwrap(),
            home.join(".local").join("bin")
        );
        assert_eq!(
            default_dir(true, None, Some(lad)).unwrap(),
            lad.join("Programs").join("sbx")
        );
        assert!(
            default_dir(false, None, Some(lad))
                .unwrap_err()
                .contains("--dir")
        );
        assert!(
            default_dir(true, Some(home), None)
                .unwrap_err()
                .contains("--dir")
        );
    }

    #[test]
    fn copies_replace_what_is_there_and_leave_no_temporary_files() {
        let root = std::env::temp_dir().join(format!("xtask-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let (a, b) = (src.join("sbx"), src.join("sbx-shim"));
        std::fs::write(&a, "new sbx").unwrap();
        std::fs::write(&b, "new shim").unwrap();
        let dir = root.join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("sbx"), "old").unwrap();
        let got = copy_into(&[a, b], &dir).unwrap();
        assert_eq!(got, [dir.join("sbx"), dir.join("sbx-shim")]);
        assert_eq!(std::fs::read_to_string(dir.join("sbx")).unwrap(), "new sbx");
        let mut names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["sbx", "sbx-shim"]);
        assert!(copy_into(&[src.join("missing")], &dir).is_err());
        assert!(!dir.join("missing.new").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
