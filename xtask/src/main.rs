//! 개발용 작업. `cargo xtask <명령>`으로 실행한다.
//!
//! - `check-deps`: 의존 방향을 검사한다.
//!   - 크레이트 사이: [`RULES`]와 [`FORBIDDEN_FEATURES`]가 정본이다.
//!   - 계층 폴더(`crates/<계층>/<이름>`)와 계층 사이 방향, `docs/developers/crates.md`의 의존 표: [`layout`].
//!   - 크레이트 안 모듈 사이: 순환 금지. 규칙은 [`modules`]에 있다.
//! - `deps-doc`: `docs/developers/crates.md`의 의존 표와 그림을 지금 크레이트로 다시 쓴다. [`layout`]에 있다.
//! - `guest-initramfs --arch <x86_64|aarch64> [--binary <파일>] [--out <파일>]`: 게스트 init initramfs를 만든다.
//!   내용과 규칙은 [`initramfs`]에 있다.
//! - `dist`: 게스트 init을 내장한 배포용 `sbx`, `sbx-shim`을 만든다. 규칙은 [`dist`]에 있다.
//! - `install [--dir <디렉터리>]`: `dist`로 만들어 OS 표준 위치(설치 스크립트와 같은 곳)에 설치한다. [`dist`]에 있다.
//! - `kernel --arch <x86_64|aarch64> [--out <파일>]`: 실행기 묶음에 넣을 기본 게스트 커널을 만든다. [`kernel`]에 있다.
//! - `smoke [--sbx <파일>] [--no-internet]`: 설치한 실행기로 실제 VM의 기본 동작을 확인한다. [`smoke`]에 있다.
//! - `repo-add --repo <디렉터리> --bundle <디렉터리> --name <이름> --version <버전>`: 실행기 묶음을 정적 저장소에
//!   더한다. [`repo`]에 있다.
//! - `repo-keygen --out <접두사>`, `repo-sign --repo <디렉터리> --key <파일>`: 저장소 색인 서명 키를 만들고, 색인에
//!   서명한다(minisign, `serial`·`expires`). [`sign`]에 있다.
//! - `release-index`, `release-yank`, `release-sign`, `sign-files`: sbx 릴리스 색인(`releases.json`)과 릴리스 묶음의
//!   minisign 서명. [`release`]에 있다.
//! - `demo [--sbx <파일>] [--out <SVG>]`: README의 데모를 실제로 돌려 만든다. [`demo`]에 있다.
//! - `bench [--sbx <파일>] [--image <참조>] [--runs <N>] [--parallel <K>]`: `sbx`로 시간을 잰다. [`bench`](mod@bench)에 있다.
//! - `fuzz [--time <초>] [대상...] [-- <libFuzzer 옵션>]`, `fuzz --seeds`: `fuzz/`의 퍼징 대상을 돌린다
//!   (nightly와 cargo-fuzz가 필요하다). [`fuzz`](mod@fuzz)에 있다.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, ExitCode};

mod bench;
mod demo;
mod dist;
mod fuzz;
mod initramfs;
mod kernel;
mod layout;
mod modules;
mod paths;
mod release;
mod repo;
mod sign;
mod smoke;

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
/// - 실행(`sbx-runtime`)과 관리(`sbx-manage`)는 서로 의존하지 않는다.
/// - 코어는 trait 크레이트에만 의존한다. 구현체(`sbx-oci`, `sbx-executor-*`)를 고르는 것은 바이너리뿐이다.
/// - `sbx-guest-proto`는 외부 크레이트를 쓰지 않는다. `sbx-guest`는 시스템 호출 선언(`libc`)만 쓴다.
/// - 대상 OS별 의존도 모두 센다(`cargo tree --target all`).
const RULES: &[(&str, Allowed)] = &[
    ("sbx-model", Allowed::Workspace(&[])),
    ("sbx-guest-proto", Allowed::Strict(&[])),
    ("sbx-executor", Allowed::Workspace(&["sbx-model"])),
    ("sbx-image", Allowed::Workspace(&["sbx-model"])),
    ("sbx-rootfs", Allowed::Workspace(&["sbx-model"])),
    ("sbx-store", Allowed::Workspace(&["sbx-model"])),
    // 이벤트 기록 장치. 무엇을 기록할지 모른다. 실행과 관리가 각자 자기 디렉터리에 쓴다.
    ("sbx-events", Allowed::Workspace(&[])),
    (
        "sbx-runtime",
        Allowed::Workspace(&[
            "sbx-model",
            "sbx-guest-proto",
            "sbx-executor",
            "sbx-store",
            "sbx-events",
        ]),
    ),
    (
        "sbx-manage",
        Allowed::Workspace(&[
            "sbx-model",
            "sbx-image",
            "sbx-rootfs",
            "sbx-store",
            "sbx-events",
        ]),
    ),
    ("sbx-oci", Allowed::Workspace(&["sbx-model", "sbx-image"])),
    (
        "sbx-executor-fake",
        // 게스트 쪽 연결 처리를 호스트에서 그대로 돌리고, 실행기 규약을 말한다.
        Allowed::Workspace(&[
            "sbx-model",
            "sbx-executor",
            "sbx-executor-proto",
            "sbx-guest-proto",
            "sbx-guest",
        ]),
    ),
    // 실행기 통신 규약. 외부 실행기 저장소가 가져다 쓰므로 workspace 크레이트에 의존하지 않는다.
    ("sbx-executor-proto", Allowed::Workspace(&[])),
    (
        "sbx-executor-process",
        Allowed::Workspace(&["sbx-model", "sbx-executor", "sbx-executor-proto"]),
    ),
    ("sbx-guest", Allowed::Strict(&["sbx-guest-proto", "libc"])),
    // Firecracker adapter: 실행기 규약과 Firecracker 공개 인터페이스만 안다. sbx 코어를 모른다.
    (
        "sbx-executor-firecracker",
        Allowed::Workspace(&["sbx-executor-proto"]),
    ),
    // 설치된 실행기 관리. std만 쓴다.
    ("sbx-executor-install", Allowed::Strict(&[])),
    // 실행기 저장소 규약과 클라이언트. 실행기 이름 규칙은 sbx-executor-install을 따른다. sbx 코어를 모른다.
    (
        "sbx-executor-repo",
        Allowed::Workspace(&["sbx-executor-install"]),
    ),
    // sbx 릴리스 색인(새 버전 알림과 `sbx upgrade`). 받기·서명·풀기는 실행기 저장소의 것을 쓴다. sbx 코어를 모른다.
    (
        "sbx-release-index",
        Allowed::Workspace(&["sbx-executor-repo", "sbx-executor-install"]),
    ),
    (
        "sbx-shim",
        Allowed::Workspace(&[
            "sbx-model",
            "sbx-guest-proto",
            "sbx-executor",
            "sbx-store",
            "sbx-events",
            "sbx-runtime",
            "sbx-executor-process",
            "sbx-executor-proto",
        ]),
    ),
    ("sbx-cli", Allowed::Any),
];

/// 켜면 안 되는 feature: (크레이트, 의존 크레이트, feature).
///
/// - 실행 쪽이 캐시 쓰기 API를 켜면 "실행은 캐시를 읽기만 한다"가 깨진다.
/// - `fuzzing`은 퍼징(`fuzz/`, workspace 밖)에 내부 함수를 여는 feature다. 배포하는 바이너리에는 들어가지 않는다.
const FORBIDDEN_FEATURES: &[(&str, &str, &str)] = &[
    ("sbx-runtime", "sbx-store", "write"),
    ("sbx-shim", "sbx-store", "write"),
    ("sbx-cli", "sbx-runtime", "fuzzing"),
    ("sbx-cli", "sbx-manage", "fuzzing"),
    ("sbx-shim", "sbx-runtime", "fuzzing"),
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
        Some("deps-doc") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match layout::write_doc(root) {
                Ok(path) => {
                    println!("{}", path.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("deps-doc: {e}");
                    ExitCode::FAILURE
                }
            }
        }
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
        Some("install") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match dist::install(root, &args[1..]) {
                Ok(outs) => {
                    for o in outs {
                        println!("{}", o.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("install: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("kernel") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match kernel::build(root, &args[1..]) {
                Ok(out) => {
                    println!("{}", out.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("kernel: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("smoke") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match smoke::run(root, &args[1..]) {
                Ok(n) => {
                    println!("smoke: {n} checks passed");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("smoke: FAILED {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("repo-add") => match repo::add(&args[1..]) {
            Ok(file) => {
                println!("{}", file.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("repo-add: {e}");
                ExitCode::FAILURE
            }
        },
        Some("repo-yank") => match repo::yank(&args[1..]) {
            Ok(file) => {
                println!("{}", file.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("repo-yank: {e}");
                ExitCode::FAILURE
            }
        },
        Some("repo-keygen") => match sign::keygen(&args[1..]) {
            Ok(pk) => {
                println!("{pk}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("repo-keygen: {e}");
                ExitCode::FAILURE
            }
        },
        Some("repo-sign") => match sign::sign(&args[1..]) {
            Ok(file) => {
                println!("{}", file.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("repo-sign: {e}");
                ExitCode::FAILURE
            }
        },
        Some(cmd @ ("release-index" | "release-yank" | "release-sign" | "sign-files")) => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            let r = match cmd {
                "release-index" => release::index(root, &args[1..]).map(|p| p.join(" ")),
                "release-yank" => release::yank(&args[1..]).map(|()| "ok".to_owned()),
                "release-sign" => release::sign_index(&args[1..]).map(|s| format!("serial {s}")),
                _ => release::sign_files(&args[1..]).map(|f| {
                    f.iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
            };
            match r {
                Ok(out) => {
                    println!("{out}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{cmd}: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("demo") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match demo::run(root, &args[1..]) {
                Ok(files) => {
                    for f in files {
                        println!("{}", f.display());
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("demo: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("fuzz") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match fuzz::run(root, &args[1..]) {
                Ok(out) => {
                    print!("{out}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("fuzz: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("bench") => {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .expect("xtask lives inside the workspace");
            match bench::run(root, &args[1..]) {
                Ok(table) => {
                    print!("{table}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("bench: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: cargo xtask check-deps\n       cargo xtask guest-initramfs --arch <x86_64|aarch64> [--binary <file>] [--out <file>]\n       cargo xtask dist [--target <triple>] [--auditable]\n       cargo xtask install [--dir <dir>]\n       cargo xtask kernel --arch <x86_64|aarch64> [--out <file>]\n       cargo xtask smoke [--sbx <file>] [--no-internet]\n       cargo xtask repo-add --repo <dir> --bundle <dir> --name <name> --version <version> [--os <os>] [--capabilities <file>] [--url <prefix>] [--description <text>] [--schema 1|2] [--adapter-package | --vmm-for <adapter> --arch <amd64|arm64>]\n       cargo xtask repo-yank --repo <dir> --name <name> --version <version> [--schema 1|2] [--adapter] [--advisory <text>] [--undo]\n       cargo xtask repo-keygen --out <prefix>\n       cargo xtask repo-sign --repo <dir> (--key <file> | --key-env <var>) [--serial <n>] [--expires-days <d>]\n       cargo xtask release-index --index <file> --version <version> --assets <dir> --base-url <url> [--security]\n       cargo xtask release-yank --index <file> --version <version> [--advisory <text>] [--undo]\n       cargo xtask release-sign --index <file> (--key <file> | --key-env <var>) [--expires-days <d>]\n       cargo xtask sign-files (--key <file> | --key-env <var>) <file>...\n       cargo xtask demo [--sbx <file>] [--out <svg>]\n       cargo xtask bench [--sbx <file>] [--image <ref>] [--runs <n>] [--parallel <k>] [--docker]\n       cargo xtask fuzz [--time <secs>] [<target>...] [-- <libFuzzer args>] | --seeds"
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
    let crates = layout::crates(root).map_err(|e| vec![e])?;
    for c in &crates {
        if !rule_names.contains(c.name.as_str()) {
            problems.push(format!(
                "{} (crates/{}) has no rule in xtask RULES",
                c.name, c.folder
            ));
        }
    }
    for name in &rule_names {
        if !crates.iter().any(|c| c.name == *name) {
            problems.push(format!(
                "rule for {name} but there is no such crate under crates/"
            ));
        }
    }
    problems.extend(layout::check(&crates));
    problems.extend(layout::check_doc(root, &crates));

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

    for (name, dir) in crates
        .iter()
        .map(|c| (c.name.clone(), c.dir.clone()))
        .chain([("xtask".to_owned(), root.join("xtask"))])
    {
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
