//! 개발용 작업. `cargo xtask <명령>`으로 실행한다.
//!
//! - `check-deps`: 의존 방향을 검사한다.
//!   - 크레이트 사이: [`RULES`]와 [`FORBIDDEN_FEATURES`]가 정본이다.
//!   - 크레이트 안 모듈 사이: 순환 금지. 규칙은 [`modules`]에 있다.
//! - `guest-initramfs --arch <x86_64|aarch64> [--binary <파일>] [--out <파일>]`: 게스트 init initramfs를 만든다.
//!   내용과 규칙은 [`initramfs`]에 있다.
//! - `dist`: 게스트 init을 내장한 배포용 `sbx`, `sbx-shim`을 만든다. 규칙은 [`dist`]에 있다.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, ExitCode};

mod dist;
mod initramfs;
mod modules;

/// 크레이트가 의존해도 되는 것.
enum Allowed {
    /// 이 workspace 크레이트들만. 외부 크레이트는 따로 제한하지 않는다.
    Workspace(&'static [&'static str]),
    /// 여기 적힌 크레이트만 (workspace든 외부든). 게스트 바이너리 크기와 호환성 때문.
    Strict(&'static [&'static str]),
    /// 제한 없음. 구현체를 고르고 조립하는 바이너리.
    Any,
}

/// 의존 규칙. 직접 의존과 간접 의존 모두에 적용한다.
///
/// - 실행(`sandbox-runtime`)과 관리(`sandbox-manage`)는 서로 의존하지 않는다.
/// - 코어는 trait 크레이트에만 의존한다. 구현체(`sandbox-oci`, `executor-*`)를 고르는 것은 바이너리뿐이다.
/// - `sandbox-proto`는 외부 크레이트를 쓰지 않는다. `sandbox-guest`는 시스템 호출 선언(`libc`)만 쓴다.
/// - 대상 OS별 의존도 모두 센다(`cargo tree --target all`).
const RULES: &[(&str, Allowed)] = &[
    ("sandbox-model", Allowed::Workspace(&[])),
    ("sandbox-proto", Allowed::Strict(&[])),
    ("sandbox-executor", Allowed::Workspace(&["sandbox-model"])),
    ("sandbox-image", Allowed::Workspace(&["sandbox-model"])),
    ("sandbox-rootfs", Allowed::Workspace(&["sandbox-model"])),
    ("sandbox-store", Allowed::Workspace(&["sandbox-model"])),
    (
        "sandbox-runtime",
        Allowed::Workspace(&[
            "sandbox-model",
            "sandbox-proto",
            "sandbox-executor",
            "sandbox-store",
        ]),
    ),
    (
        "sandbox-manage",
        Allowed::Workspace(&[
            "sandbox-model",
            "sandbox-image",
            "sandbox-rootfs",
            "sandbox-store",
        ]),
    ),
    (
        "sandbox-oci",
        Allowed::Workspace(&["sandbox-model", "sandbox-image"]),
    ),
    (
        "executor-fake",
        // 게스트 쪽 연결 처리를 호스트에서 그대로 돌리고, 실행기 규약을 말한다.
        Allowed::Workspace(&[
            "sandbox-model",
            "sandbox-executor",
            "executor-proto",
            "sandbox-proto",
            "sandbox-guest",
        ]),
    ),
    // 실행기 통신 규약. 외부 실행기 저장소가 가져다 쓰므로 workspace 크레이트에 의존하지 않는다.
    ("executor-proto", Allowed::Workspace(&[])),
    (
        "executor-process",
        Allowed::Workspace(&["sandbox-model", "sandbox-executor", "executor-proto"]),
    ),
    ("sandbox-guest", Allowed::Strict(&["sandbox-proto", "libc"])),
    (
        "sandbox-shim",
        Allowed::Workspace(&[
            "sandbox-model",
            "sandbox-proto",
            "sandbox-executor",
            "sandbox-store",
            "sandbox-runtime",
            "executor-process",
            "executor-proto",
        ]),
    ),
    ("sandbox-cli", Allowed::Any),
];

/// 켜면 안 되는 feature: (크레이트, 의존 크레이트, feature).
///
/// 실행 쪽이 캐시 쓰기 API를 켜면 "실행은 캐시를 읽기만 한다"가 깨진다.
const FORBIDDEN_FEATURES: &[(&str, &str, &str)] = &[
    ("sandbox-runtime", "sandbox-store", "write"),
    ("sandbox-shim", "sandbox-store", "write"),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("check-deps") => match check_deps() {
            Ok(()) => {
                println!("check-deps: ok");
                ExitCode::SUCCESS
            }
            Err(problems) => {
                for p in problems {
                    eprintln!("check-deps: {p}");
                }
                ExitCode::FAILURE
            }
        },
        Some("guest-initramfs") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match initramfs::run(root, &args[1..]) {
                Ok(out) => {
                    println!("{}", out.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("guest-initramfs: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("dist") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match dist::run(root, &args[1..]) {
                Ok(outs) => {
                    for o in outs {
                        println!("{}", o.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("dist: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: cargo xtask check-deps\n       cargo xtask guest-initramfs --arch <x86_64|aarch64> [--binary <file>] [--out <file>]\n       cargo xtask dist"
            );
            ExitCode::from(2)
        }
    }
}

fn check_deps() -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives inside the workspace");

    let rule_names: BTreeSet<&str> = RULES.iter().map(|(n, _)| *n).collect();
    let crate_dirs: BTreeSet<String> = std::fs::read_dir(root.join("crates"))
        .map_err(|e| vec![format!("cannot read crates/: {e}")])?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("Cargo.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for dir in &crate_dirs {
        if !rule_names.contains(dir.as_str()) {
            problems.push(format!("crates/{dir} has no rule in xtask RULES"));
        }
    }
    for name in &rule_names {
        if !crate_dirs.contains(*name) {
            problems.push(format!("rule for {name} but crates/{name} does not exist"));
        }
    }

    for (name, allowed) in RULES {
        let deps = match tree(root, name, &["-e", "normal"]) {
            Ok(lines) => lines
                .iter()
                .skip(1) // 첫 줄은 크레이트 자신이다.
                .filter_map(|l| l.split_whitespace().next())
                .map(str::to_owned)
                .collect::<BTreeSet<_>>(),
            Err(e) => {
                problems.push(e);
                continue;
            }
        };
        for dep in &deps {
            let is_workspace = rule_names.contains(dep.as_str());
            let ok = match allowed {
                Allowed::Any => true,
                Allowed::Workspace(list) => !is_workspace || list.contains(&dep.as_str()),
                Allowed::Strict(list) => list.contains(&dep.as_str()),
            };
            if !ok {
                problems.push(format!("{name} must not depend on {dep}"));
            }
        }
    }

    for dir in crate_dirs
        .iter()
        .map(|d| root.join("crates").join(d))
        .chain([root.join("xtask")])
    {
        let name = dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        problems.extend(modules::check(&name, &dir.join("src")));
    }

    for (name, dep, feature) in FORBIDDEN_FEATURES {
        match tree(root, name, &["-e", "features"]) {
            Ok(lines) => {
                let needle = format!("{dep} feature \"{feature}\"");
                if lines.iter().any(|l| l.starts_with(&needle)) {
                    problems.push(format!("{name} must not enable {dep}/{feature}"));
                }
            }
            Err(e) => problems.push(e),
        }
    }

    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// `cargo tree -p <name> --prefix none`의 줄 목록. 그 크레이트만 빌드할 때의 의존과 feature가 나온다.
fn tree(root: &Path, name: &str, extra: &[&str]) -> Result<Vec<String>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(root)
        .args(["tree", "-p", name, "--prefix", "none", "--target", "all"])
        .args(extra)
        .output()
        .map_err(|e| format!("cannot run cargo tree: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo tree -p {name} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect())
}
