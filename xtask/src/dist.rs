//! `dist`: 배포 빌드. 이 호스트 아키텍처의 게스트 init을 만들고, 그것을 내장한 `sbx`와 `sbx-shim`을 release로 빌드한다.
//!
//! 1. `guest-initramfs --arch <호스트 아키텍처>` (`target/guest/<arch>/initramfs`)
//! 2. `SBX_EMBED_GUEST`에 그 절대 경로를 주고 `cargo build --release -p sandbox-cli -p sandbox-shim`
//!
//! 결과는 `target/release/sbx`와 `target/release/sbx-shim`이다. 둘을 같은 디렉터리에 두면 sbx가 shim을 찾는다.
//!
//! `--target <triple>`: 그 대상으로 빌드한다(결과는 `target/<triple>/release/`). 릴리스는 Linux에서
//! `<arch>-unknown-linux-musl`로 정적 실행 파일을 만든다(어느 배포판에서나 돈다). 게스트 init의 아키텍처는 대상의
//! 아키텍처다.
//! 실행기(`sbx-executor`, fc-fork)는 따로 설치한다. 다른 아키텍처용 배포는 그 아키텍처 호스트에서 만든다.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::initramfs;

/// 실행한다. 만든 실행 파일 경로를 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<Vec<PathBuf>, String> {
    let mut target: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--target" => target = Some(it.next().ok_or("--target needs a value")?.clone()),
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
    build.current_dir(root).args([
        "build",
        "--release",
        "-p",
        "sandbox-cli",
        "-p",
        "sandbox-shim",
    ]);
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
