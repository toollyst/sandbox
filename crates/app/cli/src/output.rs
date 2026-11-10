//! `--format json` 출력. 출력의 모양은 스크립트, 에이전트, `sbx tui`가 기대는 공개 규약이다.
//!
//! - 명령마다 어떤 모양을 내는지는 [`JSON_OUTPUT`]이 정본이다. 모든 명령이 들어 있다(테스트가 확인한다).
//!   - [`Form::Value`]: stdout에 JSON 값 하나(보기 좋게 줄을 나눈다).
//!   - [`Form::Lines`]: 대상마다 한 줄에 JSON 객체 하나(JSON Lines). 여러 대상을 받는 명령이 쓴다.
//!     하나가 실패해도 나머지를 계속하므로(Docker와 같다) 한 줄씩 낸다.
//!   - [`Form::Empty`]: 성공하면 아무것도 내지 않는다.
//!   - [`Form::Unsupported`]: 출력이 게스트 프로세스의 바이트 그대로이거나 다른 규약이다. `--format json`이면 아무것도
//!     하지 않고 오류로 끝난다.
//!   - [`Form::Detached`]: 붙지 않을 때(`run -d`, `start`)는 `Value`, 붙을 때(`run`, `start -a`)는 `Unsupported`.
//! - `-q`가 있는 명령(`ps`, `images`)은 `-q`가 이긴다: 한 줄에 ID 하나.
//! - stdout에는 JSON만 나온다. 진행 표시와 안내는 stderr로 간다.
//! - 오류는 stderr에 [`ErrorLine`] 한 줄이다. 명령 전체가 실패하면 마지막 줄이고, 여러 대상 중 하나가 실패하면 그 대상의
//!   줄(`target`이 있다)이다. stderr의 그 밖의 줄은 사람이 읽는 안내이고 모양을 약속하지 않는다.
//! - 필드 이름은 snake_case, 시각은 RFC 3339(UTC), 값이 없으면 `null`이다.
//! - 자료형은 저장 형식(`sbx-model`의 기록)과 따로 둔다. 저장 형식이 바뀌어도 출력은 이 파일이 바뀔 때만 바뀐다.
//!   문서 주석은 영어다. clap 도움말처럼 사용자에게 보이는 스키마 설명이 되기 때문이다.
//! - 문서 `docs/developers/cli-output.md`와 JSON Schema `docs/developers/cli-output.schema.json`은 여기서 만든다.
//!   테스트가 파일이 지금 자료형과 같은지 확인하고, `SBX_BLESS=1 cargo test -p sbx-cli --lib output`이 다시 쓴다.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use sbx_model::{MountKind, NetworkPolicy};
use sbx_runtime::SandboxInfo;

use crate::behavior;
use crate::cli::Command;

/// 명령 하나가 `--format json`에서 내는 모양. 값은 자료형 이름이다(`[이름]`은 그 배열).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// JSON 값 하나.
    Value(&'static str),
    /// 대상마다 한 줄에 JSON 객체 하나.
    Lines(&'static str),
    /// 출력 없음.
    Empty,
    /// 받지 않는다. 값은 이유다.
    Unsupported(&'static str),
    /// 붙지 않으면 `Value`, 붙으면 `Unsupported`.
    Detached(&'static str),
}

const PROCESS_BYTES: &str = "its output is the process's own bytes";

/// 명령(하위 명령은 공백으로 잇는다)마다 `--format json`의 모양.
pub const JSON_OUTPUT: &[(&str, Form)] = &[
    ("pull", Form::Value("Pulled")),
    ("prepare", Form::Value("Prepared")),
    ("images", Form::Value("[Image]")),
    ("rmi", Form::Lines("Removed")),
    ("executor ls", Form::Value("[InstalledExecutor]")),
    ("executor install", Form::Value("ExecutorRef")),
    ("executor search", Form::Value("[AvailableExecutor]")),
    ("executor use", Form::Value("ExecutorRef")),
    ("executor rm", Form::Value("ExecutorRef")),
    ("executor repo ls", Form::Value("[Repository]")),
    ("executor repo add", Form::Value("Repository")),
    ("executor repo rm", Form::Value("Repository")),
    ("info", Form::Value("Info")),
    ("run", Form::Detached("SandboxRef")),
    ("create", Form::Value("SandboxRef")),
    ("start", Form::Detached("SandboxRef")),
    ("exec", Form::Unsupported(PROCESS_BYTES)),
    ("cp", Form::Empty),
    ("stop", Form::Lines("Done")),
    ("restart", Form::Lines("Done")),
    ("kill", Form::Lines("Done")),
    ("wait", Form::Lines("Exited")),
    ("rm", Form::Lines("Done")),
    ("attach", Form::Unsupported(PROCESS_BYTES)),
    ("ps", Form::Value("[Sandbox]")),
    ("inspect", Form::Value("Sandbox")),
    (
        "logs",
        Form::Unsupported("its output is the main process's own bytes"),
    ),
    ("events", Form::Lines("Event")),
    (
        "mcp",
        Form::Unsupported("it speaks the Model Context Protocol (JSON-RPC) on stdio"),
    ),
    (
        "completion",
        Form::Unsupported("its output is a shell script"),
    ),
];

/// `command`(하위 명령은 공백으로 잇는다)의 모양.
pub fn form(command: &str) -> Option<Form> {
    JSON_OUTPUT
        .iter()
        .find(|(c, _)| *c == command)
        .map(|(_, f)| *f)
}

/// `--format json`을 받지 않으면 그 이유를 담은 오류 문구. `attached`는 붙어서 실행하는지(`run`, `start -a`).
pub fn refuse(command: &str, attached: bool) -> Option<String> {
    let why = match form(command)? {
        Form::Unsupported(why) => why,
        Form::Detached(_) if attached => PROCESS_BYTES,
        _ => return None,
    };
    let hint = match command {
        "run" => " (use -d to get the sandbox as JSON)",
        "start" => " (without -a it prints the sandbox as JSON)",
        _ => "",
    };
    Some(format!(
        "--format json is not supported by `{command}`{}: {why}{hint}",
        if attached && matches!(command, "run" | "start") {
            " when attached"
        } else {
            ""
        }
    ))
}

/// `--format json`을 받지 않는 명령이면 그 이유. 하위 명령과 붙는지(`run`, `start -a`)도 본다.
pub fn refusal(command: &Command) -> Option<String> {
    let (name, attached) = match command {
        Command::Run { attach, .. } => ("run", !attach.detach),
        Command::Start {
            attach,
            interactive,
            ..
        } => ("start", *attach || *interactive),
        Command::Exec(_) => ("exec", true),
        Command::Attach { .. } => ("attach", true),
        Command::Logs { .. } => ("logs", false),
        Command::Mcp { .. } => ("mcp", false),
        Command::Completion { .. } => ("completion", false),
        _ => return None,
    };
    refuse(name, attached)
}

/// 값 하나를 stdout에 쓴다([`Form::Value`]).
pub fn print_value<T: Serialize + ?Sized>(v: &T) -> serde_json::Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}

/// 한 줄을 stdout에 쓴다([`Form::Lines`]).
pub fn print_line<T: Serialize + ?Sized>(v: &T) -> serde_json::Result<()> {
    println!("{}", serde_json::to_string(v)?);
    Ok(())
}

/// 오류 한 줄을 stderr에 쓴다.
pub fn print_error(target: Option<&str>, message: &str, exit_code: Option<u8>) {
    let line = ErrorLine {
        error: ErrorBody {
            message: message.to_owned(),
            target: target.map(str::to_owned),
            exit_code,
        },
    };
    // 직렬화는 실패하지 않는다(문자열과 숫자뿐).
    if let Ok(s) = serde_json::to_string(&line) {
        eprintln!("{s}");
    }
}

/// An error, written to stderr as one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ErrorLine {
    /// The error.
    pub error: ErrorBody,
}

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ErrorBody {
    /// Human-readable message. Its wording may change.
    pub message: String,
    /// The argument (sandbox or image) this error is about, when a command takes several and this one failed.
    /// The others still ran.
    pub target: Option<String>,
    /// The exit code sbx ends with. Only on the last line, when the whole command failed.
    pub exit_code: Option<u8>,
}

/// An image that was downloaded (`pull`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Pulled {
    /// Normalized reference, e.g. `docker.io/library/alpine:3.20`.
    pub reference: String,
    /// Image ID (`sha256:…` of the image config).
    pub id: String,
    /// Architecture in OCI terms (`amd64`, `arm64`).
    pub arch: String,
}

/// A root filesystem that is ready (`prepare`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Prepared {
    /// Image ID.
    pub id: String,
    /// Root filesystem format, e.g. `erofs`.
    pub rootfs_format: String,
    /// Whether it was built now (`false`: it already existed).
    pub built: bool,
}

/// A downloaded image (`images`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Image {
    /// Image ID.
    pub id: String,
    /// Normalized references that point to this image. Empty if none does.
    pub references: Vec<String>,
    /// Architecture in OCI terms.
    pub arch: String,
    /// Root filesystem formats that are prepared.
    pub prepared: Vec<String>,
    /// Sum of the compressed layer sizes, in bytes.
    pub compressed_size: u64,
    /// Image config fields sbx does not apply.
    pub ignored_fields: Vec<String>,
}

/// The result for one image given to `rmi`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Removed {
    /// The argument as given.
    pub target: String,
    /// References removed.
    pub untagged: Vec<String>,
    /// Image ID and layer digests deleted. Empty if only a reference was removed.
    pub deleted: Vec<String>,
}

/// An installed executor (`executor ls`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InstalledExecutor {
    /// Name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Whether it is the default (`executor use`).
    pub default: bool,
    /// Path of its executable.
    pub path: String,
}

/// An executor by name and version (`executor install`, `use`, `rm`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExecutorRef {
    /// Name.
    pub name: String,
    /// Version.
    pub version: String,
}

/// An executor available from a repository for this host (`executor search`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AvailableExecutor {
    /// Name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Executor protocol version it speaks.
    pub protocol: u32,
    /// Repository name.
    pub repository: String,
    /// Operating system of the bundle.
    pub os: String,
    /// Architecture of the bundle (OCI terms).
    pub architecture: String,
    /// Bundle URL.
    pub url: String,
    /// SHA-256 of the bundle, hex.
    pub sha256: String,
    /// Bundle size in bytes.
    pub size: u64,
}

/// An executor repository (`executor repo ls`, `add`, `rm`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Repository {
    /// Name.
    pub name: String,
    /// URL. The index is `<url>/index.json`.
    pub url: String,
}

/// Host and installation diagnostics (`info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Info {
    /// sbx version.
    pub version: String,
    /// Host operating system, human-readable.
    pub os: String,
    /// Host architecture (OCI terms).
    pub arch: String,
    /// Whether this host can run VMs.
    pub hypervisor: Hypervisor,
    /// The executor sbx would use.
    pub executor: ExecutorInfo,
    /// Guest init in use: a path, or `embedded in sbx`.
    pub guest_init: String,
    /// Data directory.
    pub data_dir: String,
    /// Cache directory.
    pub cache_dir: String,
    /// Number of sandboxes.
    pub sandboxes: usize,
    /// Number of running sandboxes.
    pub running: usize,
    /// Number of images.
    pub images: usize,
}

/// Virtualization support.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Hypervisor {
    /// Whether VMs can run.
    pub usable: bool,
    /// What was found, human-readable.
    pub detail: String,
}

/// The executor sbx would use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ExecutorInfo {
    /// `name@version` of the installed default, or the `--executor` path.
    pub name: String,
    /// Executable that would run.
    pub program: String,
    /// What it supports. Null if it could not be asked (see `error`).
    pub capabilities: Option<Capabilities>,
    /// Why it could not be asked.
    pub error: Option<String>,
}

/// What an executor supports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Executor name.
    pub name: String,
    /// Guest architecture (OCI terms).
    pub arch: String,
    /// Root filesystem formats it can attach.
    pub rootfs_formats: Vec<String>,
    /// Whether it can share host folders live (`-v`, bind).
    pub live_share: bool,
    /// Whether it can boot a kernel shipped in the image.
    pub custom_kernel: bool,
}

/// A sandbox that was created or started (`create`, `run -d`, `start`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SandboxRef {
    /// Full sandbox ID.
    pub id: String,
    /// Name, if it has one.
    pub name: Option<String>,
}

/// One sandbox that `stop`, `restart`, `kill` or `rm` acted on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Done {
    /// The argument as given (name, ID or ID prefix).
    pub target: String,
    /// Full sandbox ID.
    pub id: String,
}

/// One sandbox that `wait` waited for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Exited {
    /// The argument as given.
    pub target: String,
    /// Full sandbox ID.
    pub id: String,
    /// Exit code of the main process.
    pub exit_code: i32,
}

/// Something that changed (`events`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Event {
    /// When it happened (RFC 3339, nanoseconds).
    pub time: String,
    /// `sandbox` or `image`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Sandbox: `create`, `start`, `stop`, `remove`. Image: `pull`, `prepare`, `untag`, `delete`.
    pub action: String,
    /// Sandbox ID or image ID.
    pub id: String,
    /// Details. Sandbox: `image`, `name`; on `stop` also `exit_code` and `error`. Image: `reference` (`pull`,
    /// `untag`), `rootfs_format` (`prepare`).
    pub attributes: BTreeMap<String, String>,
}

impl Event {
    /// 기록 형식에서.
    pub fn new(e: &sbx_events::Event) -> Self {
        Self {
            time: behavior::rfc3339_ns(e.time_ns),
            kind: e.kind.clone(),
            action: e.action.clone(),
            id: e.id.clone(),
            attributes: e.attributes.clone(),
        }
    }
}

/// A sandbox (`ps`, `inspect`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Sandbox {
    /// Full sandbox ID.
    pub id: String,
    /// Name given with `--name`.
    pub name: Option<String>,
    /// When it was created. Null if the file system does not record it.
    pub created: Option<String>,
    /// Lifecycle state.
    pub state: SandboxState,
    /// Exit code of the main process, once it has exited.
    pub exit_code: Option<i32>,
    /// Why it stopped, if an error stopped it.
    pub error: Option<String>,
    /// The image it runs.
    pub image: SandboxImage,
    /// Root filesystem format.
    pub rootfs_format: String,
    /// The main process.
    pub process: Process,
    /// VM size.
    pub resources: Resources,
    /// Network access.
    pub network: Network,
    /// Folders shared with the host.
    pub mounts: Vec<Mount>,
    /// Guest hostname, if set.
    pub hostname: Option<String>,
    /// Labels (from the image, then `-l`).
    pub labels: BTreeMap<String, String>,
    /// How `stop` stops it.
    pub stop: Stop,
    /// Whether the main process's stdin is open (`-i`).
    pub interactive: bool,
    /// Whether the main process has a terminal (`-t`).
    pub tty: bool,
}

/// Sandbox lifecycle state (OCI names).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SandboxState {
    /// Being created.
    Creating,
    /// Created, not started yet.
    Created,
    /// Running.
    Running,
    /// Stopped.
    Stopped,
}

/// The image a sandbox runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SandboxImage {
    /// Image ID.
    pub id: String,
    /// The reference it was pulled as. Null if the image has since been removed.
    pub reference: Option<String>,
}

/// A process to run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Process {
    /// argv.
    pub args: Vec<String>,
    /// Environment, `NAME=value`.
    pub env: Vec<String>,
    /// Working directory in the guest.
    pub cwd: String,
    /// User (and group), as `-u` takes it.
    pub user: String,
}

/// VM size.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Resources {
    /// vCPUs.
    pub vcpus: u8,
    /// Memory in MiB.
    pub memory_mib: u32,
    /// Writable disk in MiB.
    pub scratch_disk_mib: u32,
}

/// Network access.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Network {
    /// `none` or `allow`.
    pub mode: NetworkMode,
    /// `HOST:PORT` rules (`--allow`). Empty for `none`.
    pub allow: Vec<String>,
}

/// Network mode (`--network`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network.
    None,
    /// Only TCP to the allowed `HOST:PORT`s.
    Allow,
}

/// A folder shared with the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Mount {
    /// Host path.
    pub source: String,
    /// Guest path.
    pub destination: String,
    /// `bind` (live) or `copy` (copied in at start).
    pub kind: MountType,
    /// Read-only in the guest.
    pub readonly: bool,
}

/// How a folder is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    /// Live share.
    Bind,
    /// Copied in when the sandbox starts.
    Copy,
}

/// How `stop` stops a sandbox.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Stop {
    /// Signal sent first, e.g. `SIGTERM`.
    pub signal: String,
    /// Seconds to wait before powering off. Null: the command's default.
    pub timeout_secs: Option<u32>,
}

impl Sandbox {
    /// `reference`는 이미지를 받을 때 쓴 참조(이미지가 지워졌으면 `None`).
    pub fn new(s: &SandboxInfo, reference: Option<String>) -> Self {
        let spec = &s.spec;
        let (mode, allow) = match &spec.network {
            NetworkPolicy::Disabled => (NetworkMode::None, Vec::new()),
            NetworkPolicy::Allow(rules) => (
                NetworkMode::Allow,
                rules.iter().map(ToString::to_string).collect(),
            ),
        };
        Self {
            id: s.id.to_string(),
            name: spec.name.clone(),
            created: s.created.map(behavior::rfc3339),
            state: s.state.into(),
            exit_code: s.exit_code,
            error: s.error.clone(),
            image: SandboxImage {
                id: spec.image.to_string(),
                reference,
            },
            rootfs_format: spec.rootfs_format.to_string(),
            process: Process {
                args: spec.process.args.clone(),
                env: spec
                    .process
                    .env
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect(),
                cwd: spec.process.cwd.clone(),
                user: spec.process.user.to_string(),
            },
            resources: Resources {
                vcpus: spec.resources.vcpus,
                memory_mib: spec.resources.memory_mib,
                scratch_disk_mib: spec.resources.scratch_disk_mib,
            },
            network: Network { mode, allow },
            mounts: spec
                .mounts
                .iter()
                .map(|m| Mount {
                    source: m.source.display().to_string(),
                    destination: m.destination.clone(),
                    kind: match m.kind {
                        MountKind::Bind => MountType::Bind,
                        MountKind::Copy => MountType::Copy,
                    },
                    readonly: m.readonly,
                })
                .collect(),
            hostname: spec.hostname.clone(),
            labels: spec.labels.clone(),
            stop: Stop {
                signal: spec.stop.signal.to_string(),
                timeout_secs: spec.stop.timeout_secs,
            },
            interactive: spec.open_stdin,
            tty: spec.tty,
        }
    }
}

impl From<sbx_model::State> for SandboxState {
    fn from(s: sbx_model::State) -> Self {
        match s {
            sbx_model::State::Creating => SandboxState::Creating,
            sbx_model::State::Created => SandboxState::Created,
            sbx_model::State::Running => SandboxState::Running,
            sbx_model::State::Stopped => SandboxState::Stopped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::path::{Path, PathBuf};

    use crate::commands::COMMANDS;

    #[test]
    fn every_command_has_a_form() {
        use clap::CommandFactory;
        // 하위 명령이 있는 명령은 하위 명령마다 적는다.
        let mut expected = Vec::new();
        let root = crate::cli::Cli::command();
        fn walk(prefix: &str, c: &clap::Command, out: &mut Vec<String>) {
            let subs: Vec<_> = c
                .get_subcommands()
                .filter(|s| s.get_name() != "help")
                .collect();
            if subs.is_empty() {
                out.push(prefix.to_owned());
            }
            for s in subs {
                walk(&format!("{prefix} {}", s.get_name()), s, out);
            }
        }
        for c in COMMANDS {
            let sub = root
                .find_subcommand(c.name)
                .unwrap_or_else(|| panic!("{} is not a clap command", c.name));
            walk(c.name, sub, &mut expected);
        }
        let listed: Vec<String> = JSON_OUTPUT.iter().map(|(c, _)| c.to_string()).collect();
        assert_eq!(
            listed, expected,
            "JSON_OUTPUT must list every command in order"
        );
    }

    #[test]
    fn refusals_name_the_command() {
        assert!(refuse("exec", false).unwrap().contains("`exec`"));
        assert!(refuse("run", false).is_none());
        let attached = refuse("run", true).unwrap();
        assert!(attached.contains("when attached") && attached.contains("-d"));
        assert!(refuse("start", true).unwrap().contains("-a"));
        assert!(refuse("ps", false).is_none());
        assert!(refuse("cp", false).is_none());
    }

    /// 문서에 넣는 자료형과 그것들이 쓰는 자료형의 스키마(`$defs`). 생성기 하나로 모아 `$ref`가 한 문서 안에서 풀린다.
    /// [`JSON_OUTPUT`]이 가리키는 이름은 모두 여기 있어야 한다.
    fn schemas() -> serde_json::Map<String, serde_json::Value> {
        let mut g = schemars::generate::SchemaSettings::draft2020_12().into_generator();
        g.subschema_for::<ErrorLine>();
        g.subschema_for::<Pulled>();
        g.subschema_for::<Prepared>();
        g.subschema_for::<Image>();
        g.subschema_for::<Removed>();
        g.subschema_for::<InstalledExecutor>();
        g.subschema_for::<ExecutorRef>();
        g.subschema_for::<AvailableExecutor>();
        g.subschema_for::<Repository>();
        g.subschema_for::<Info>();
        g.subschema_for::<SandboxRef>();
        g.subschema_for::<Done>();
        g.subschema_for::<Exited>();
        g.subschema_for::<Sandbox>();
        g.subschema_for::<Event>();
        g.take_definitions(true)
    }

    #[test]
    fn forms_name_documented_types() {
        let defs = schemas();
        for (cmd, f) in JSON_OUTPUT {
            let ty = match f {
                Form::Value(t) | Form::Lines(t) | Form::Detached(t) => *t,
                Form::Empty | Form::Unsupported(_) => continue,
            };
            let ty = ty.trim_start_matches('[').trim_end_matches(']');
            assert!(defs.contains_key(ty), "{cmd}: {ty} has no schema");
        }
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
    }

    /// 사람이 읽는 문서. 명령마다 모양과 자료형을 표로 둔다.
    fn markdown() -> String {
        let mut md = String::from(
            "# CLI JSON output\n\n\
             <!-- Generated from crates/app/cli/src/output.rs. Do not edit: run \
             `SBX_BLESS=1 cargo test -p sbx-cli --lib output`. -->\n\n\
             With `--format json`, sbx prints only JSON on stdout. This is a public contract: \
             scripts, agents and `sbx tui` rely on it. sbx is alpha, so it may still change, \
             but only together with this file.\n\n\
             - **value**: one JSON value (pretty-printed).\n\
             - **lines**: one JSON object per line (JSON Lines), one per argument. \
             If one argument fails, the others still run.\n\
             - **empty**: nothing on success.\n\
             - **not supported**: sbx refuses `--format json` and exits with an error.\n\
             - `-q` (`ps`, `images`) wins over `--format`: one ID per line.\n\
             - Errors go to stderr as one `ErrorLine` per line. Other stderr lines are for humans.\n\
             - Field names are snake_case, times are RFC 3339 (UTC), missing values are `null`.\n\n\
             The JSON Schema (draft 2020-12) of every type is in \
             [cli-output.schema.json](cli-output.schema.json).\n\n\
             | Command | Output |\n|---|---|\n",
        );
        for (cmd, f) in JSON_OUTPUT {
            let out = match f {
                Form::Value(t) => format!("value: `{t}`"),
                Form::Lines(t) => format!("lines: `{t}`"),
                Form::Empty => "empty".to_owned(),
                Form::Unsupported(why) => format!("not supported: {why}"),
                Form::Detached(t) => {
                    format!("value: `{t}` when detached; not supported when attached")
                }
            };
            md.push_str(&format!("| `{cmd}` | {out} |\n"));
        }
        md
    }

    fn schema_file() -> String {
        let v = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "sbx --format json output types",
            "$defs": schemas(),
        });
        serde_json::to_string_pretty(&v).unwrap() + "\n"
    }

    #[test]
    fn docs_are_current() {
        let dir = repo_root().join("docs/developers");
        let files = [
            ("cli-output.md", markdown()),
            ("cli-output.schema.json", schema_file()),
        ];
        let bless = std::env::var_os("SBX_BLESS").is_some();
        for (name, want) in files {
            let path = dir.join(name);
            if bless {
                std::fs::write(&path, &want).unwrap();
                continue;
            }
            let have = std::fs::read_to_string(&path).unwrap_or_default();
            assert!(
                have == want,
                "docs/developers/{name} is out of date: run `SBX_BLESS=1 cargo test -p sbx-cli --lib output`"
            );
        }
    }
}
