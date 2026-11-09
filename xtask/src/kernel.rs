//! `kernel`: 실행기 묶음에 넣을 기본 게스트 커널을 만든다.
//!
//! ```text
//! cargo xtask kernel --arch <x86_64|aarch64> [--out <파일>]
//! ```
//!
//! - 소스: kernel.org의 Linux [`VERSION`] (호스트에서 내려받아 `target/kernel/`에 두고, [`SHA256`]으로 확인한다).
//! - 설정: upstream Firecracker의 CI 커널 설정(`xtask/kernel/microvm-kernel-ci-<arch>-6.1.config`, 출처는 파일 머리에
//!   적었다) 위에 `xtask/kernel/sbx.config`를 합친다(`merge_config.sh`, `olddefconfig`). `sbx.config`의 항목이
//!   하나라도 `=y`가 아니면 실패한다.
//! - 빌드: Docker의 Linux 컨테이너(`debian:bookworm`) 안에서 한다. 소스는 이름 있는 볼륨 `sbx-kernel`에 풀어 둔다
//!   (macOS의 파일 시스템은 대소문자를 구분하지 않아 커널 소스를 풀 수 없다). 아키텍처마다 빌드 디렉터리가
//!   따로 있어(`O=`) 다음 빌드는 증분이다. 다른 아키텍처는 교차 컴파일한다.
//! - 결과: Firecracker가 읽는 형식. x86_64는 ELF `vmlinux`, aarch64는 `Image`. 기본 위치는
//!   `target/kernel/<arch>/kernel`이고, 쓴 설정은 그 옆의 `config`다.

use std::path::{Path, PathBuf};
use std::process::Command;

/// 커널 버전.
pub const VERSION: &str = "6.1.155";
/// `linux-<VERSION>.tar.xz`의 SHA-256 (kernel.org `sha256sums.asc`와 같다).
pub const SHA256: &str = "c29387aeee085fbcbd91236224b9df805063bac43615e75cea2c6b29604a5c73";
/// 빌드에 쓰는 이미지.
const BUILDER: &str = "debian:bookworm";
/// 소스와 빌드 트리를 두는 Docker 볼륨.
const VOLUME: &str = "sbx-kernel";

fn tarball_url() -> String {
    format!("https://cdn.kernel.org/pub/linux/kernel/v6.x/linux-{VERSION}.tar.xz")
}

struct Target {
    /// `uname -m` 이름.
    arch: &'static str,
    /// 커널의 `ARCH=`.
    kernel_arch: &'static str,
    /// 빌드할 대상과 결과 파일(빌드 트리 기준).
    make_target: &'static str,
    image: &'static str,
    /// 교차 컴파일러 패키지와 접두사.
    cross_package: &'static str,
    cross_prefix: &'static str,
}

fn target(arch: &str) -> Result<Target, String> {
    match arch {
        "x86_64" => Ok(Target {
            arch: "x86_64",
            kernel_arch: "x86_64",
            make_target: "vmlinux",
            image: "vmlinux",
            cross_package: "gcc-x86-64-linux-gnu",
            cross_prefix: "x86_64-linux-gnu-",
        }),
        "aarch64" => Ok(Target {
            arch: "aarch64",
            kernel_arch: "arm64",
            make_target: "Image",
            image: "arch/arm64/boot/Image",
            cross_package: "gcc-aarch64-linux-gnu",
            cross_prefix: "aarch64-linux-gnu-",
        }),
        other => Err(format!("unknown --arch {other} (x86_64 or aarch64)")),
    }
}

/// 컨테이너 안에서 도는 스크립트. `/src`(내려받은 소스, 읽기 전용), `/cfg`(설정, 읽기 전용), `/out`(결과).
fn script(t: &Target) -> String {
    format!(
        r#"set -euo pipefail
cross=""
if [ "$(uname -m)" != "{arch}" ]; then cross="{cross_package}"; fi
apt-get update -qq
apt-get install -y -qq --no-install-recommends build-essential flex bison bc libelf-dev libssl-dev xz-utils cpio $cross >/dev/null
echo "{sha}  /src/linux-{v}.tar.xz" | sha256sum -c --quiet
mkdir -p /build && cd /build
if [ ! -f linux-{v}/.sbx-unpacked ]; then rm -rf linux-{v}; tar xJf /src/linux-{v}.tar.xz && touch linux-{v}/.sbx-unpacked; fi
cd linux-{v}
obj=/build/out-{arch}
mkdir -p $obj
make_args="ARCH={karch} O=$obj"
if [ -n "$cross" ]; then make_args="$make_args CROSS_COMPILE={prefix}"; fi
rm -f $obj/.config
ARCH={karch} ./scripts/kconfig/merge_config.sh -m -O $obj /cfg/microvm-kernel-ci-{arch}-6.1.config /cfg/sbx.config >/dev/null
make $make_args olddefconfig >/dev/null
missing=""
for o in $(grep -E '^CONFIG_[A-Z0-9_]+=y' /cfg/sbx.config | cut -d= -f1); do grep -qx "$o=y" $obj/.config || missing="$missing $o"; done
if [ -n "$missing" ]; then echo "kernel config is missing:$missing" >&2; exit 1; fi
make $make_args -j"$(nproc)" {mtarget} >/out/make.log 2>&1 || {{ tail -40 /out/make.log >&2; exit 1; }}
cp $obj/{image} /out/kernel
cp $obj/.config /out/config
"#,
        arch = t.arch,
        cross_package = t.cross_package,
        sha = SHA256,
        v = VERSION,
        karch = t.kernel_arch,
        prefix = t.cross_prefix,
        mtarget = t.make_target,
        image = t.image,
    )
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("cannot run {what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{what} failed ({status})"))
    }
}

/// 만들고 결과 파일 경로를 돌려준다.
pub fn build(root: &Path, args: &[String]) -> Result<PathBuf, String> {
    let mut arch = None;
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--arch" => arch = it.next().cloned(),
            "--out" => out = it.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let t = target(&arch.ok_or("--arch is required (x86_64 or aarch64)")?)?;
    let src = root.join("target/kernel");
    let work = src.join(t.arch);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let tarball = src.join(format!("linux-{VERSION}.tar.xz"));
    if !tarball.is_file() {
        eprintln!("kernel: downloading {}", tarball_url());
        let partial = tarball.with_extension("partial");
        run(
            Command::new("curl")
                .args(["-fL", "--retry", "3", "-o"])
                .arg(&partial)
                .arg(tarball_url()),
            "curl",
        )?;
        std::fs::rename(&partial, &tarball).map_err(|e| e.to_string())?;
    }
    eprintln!(
        "kernel: building Linux {VERSION} for {} in {BUILDER}",
        t.arch
    );
    let abs = |p: &Path| -> Result<String, String> {
        Ok(p.canonicalize()
            .map_err(|e| format!("{}: {e}", p.display()))?
            .to_string_lossy()
            .into_owned())
    };
    run(
        Command::new("docker")
            .args(["run", "--rm", "-v"])
            .arg(format!("{VOLUME}:/build"))
            .arg("-v")
            .arg(format!("{}:/src:ro", abs(&src)?))
            .arg("-v")
            .arg(format!("{}:/cfg:ro", abs(&root.join("xtask/kernel"))?))
            .arg("-v")
            .arg(format!("{}:/out", abs(&work)?))
            .args([BUILDER, "bash", "-c"])
            .arg(script(&t)),
        "docker",
    )?;
    let built = work.join("kernel");
    match out {
        Some(o) => {
            std::fs::copy(&built, &o).map_err(|e| format!("{}: {e}", o.display()))?;
            Ok(o)
        }
        None => Ok(built),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_option_is_checked_and_targets_are_known() {
        let cfg = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("kernel/sbx.config"),
        )
        .unwrap();
        let required: Vec<&str> = cfg.lines().filter(|l| l.starts_with("CONFIG_")).collect();
        assert!(required.contains(&"CONFIG_EROFS_FS=y"));
        assert!(required.iter().all(|l| l.ends_with("=y")), "{required:?}");
        for arch in ["x86_64", "aarch64"] {
            let t = target(arch).unwrap();
            assert!(script(&t).contains(&format!("microvm-kernel-ci-{arch}-6.1.config")));
            assert!(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("kernel/microvm-kernel-ci-{arch}-6.1.config"))
                    .is_file()
            );
        }
        assert!(target("riscv64").is_err());
    }
}
