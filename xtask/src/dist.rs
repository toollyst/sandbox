//! `dist`: 배포 빌드. 이 호스트 아키텍처의 게스트 init을 만들고, 그것을 내장한 `sbx`와 `sbx-shim`을 release로 빌드한다.
//!
//! 1. `guest-initramfs --arch <호스트 아키텍처>` (`target/guest/<arch>/initramfs`)
//! 2. `SBX_EMBED_GUEST`에 그 절대 경로를 주고 `cargo build --release -p sandbox-cli -p sandbox-shim`
//!
//! 결과는 `target/release/sbx`와 `target/release/sbx-shim`이다. 둘을 같은 디렉터리에 두면 sbx가 shim을 찾는다.
//! 실행기(`sbx-executor`, fc-fork)는 따로 설치한다. 다른 아키텍처용 배포는 그 아키텍처 호스트에서 만든다.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::initramfs;

/// 실행한다. 만든 실행 파일 경로를 돌려준다.
pub fn run(root: &Path, args: &[String]) -> Result<Vec<PathBuf>, String> {
    if let Some(a) = args.first() {
        return Err(format!("unknown argument {a:?}"));
    }
    let arch = std::env::consts::ARCH;
    let guest = initramfs::run(root, &["--arch".into(), arch.into()])?;
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let status = Command::new(cargo)
        .current_dir(root)
        .args([
            "build",
            "--release",
            "-p",
            "sandbox-cli",
            "-p",
            "sandbox-shim",
        ])
        .env("SBX_EMBED_GUEST", &guest)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err("building sbx failed".into());
    }
    let exe = std::env::consts::EXE_SUFFIX;
    let release = root.join("target").join("release");
    Ok(vec![
        release.join(format!("sbx{exe}")),
        release.join(format!("sbx-shim{exe}")),
    ])
}
