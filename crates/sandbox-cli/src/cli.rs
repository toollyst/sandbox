//! clap 정의. 명령 이름과 설명은 [`commands`](crate::commands)가 정본이다.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use sandbox_model::RootfsFormat;

use crate::behavior::{DEFAULT_ROOTFS_FORMAT, PullPolicy};
use crate::commands::about;

/// 출력 형식.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Human-readable table.
    Text,
    /// JSON for scripts.
    Json,
}

/// `sbx`.
#[derive(Debug, Parser)]
#[command(
    name = "sbx",
    version,
    about = "Docker-style sandboxes, each in its own microVM",
    // 모든 명령에 붙는 옵션은 명령마다의 옵션과 섞이지 않게 따로 모은다(Docker의 Global Options와 같다).
    next_help_heading = "Global Options"
)]
pub struct Cli {
    /// Directory for sandbox state.
    #[arg(long, global = true, env = "SBX_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Directory for the image cache.
    #[arg(long, global = true, env = "SBX_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,
    /// Directory for guest connection sockets (must be a short path).
    #[arg(long, global = true, env = "SBX_SOCKET_DIR")]
    pub socket_dir: Option<PathBuf>,
    /// Guest init initramfs. Defaults to the one embedded in sbx.
    #[arg(long, global = true, env = "SBX_GUEST_INIT")]
    pub guest_init: Option<PathBuf>,
    /// Executor program (speaks the executor protocol). Looked up on PATH unless it is a path.
    /// Defaults to the installed default executor (`sbx executor use`), then `sbx-executor` on PATH.
    #[arg(long, global = true, env = "SBX_EXECUTOR")]
    pub executor: Option<PathBuf>,
    /// Shim program that owns detached sandboxes. Defaults to `sbx-shim` next to sbx, then on PATH.
    #[arg(long, global = true, env = "SBX_SHIM")]
    pub shim: Option<PathBuf>,
    /// Output format.
    #[arg(long, global = true, value_enum, default_value_t = Format::Text)]
    pub format: Format,
    /// Command.
    #[command(subcommand)]
    pub command: Command,
}

/// 명령. 순서와 설명은 `COMMANDS`와 같아야 한다(테스트가 확인한다).
///
/// 항목에 문서 주석을 달지 않는다. 문서 주석은 clap의 긴 도움말이 되어 `COMMANDS`와 갈라지기 때문이다.
#[allow(missing_docs)]
#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = about("pull"))]
    Pull {
        /// linux/amd64 or linux/arm64. Defaults to the host architecture.
        #[arg(long)]
        platform: Option<String>,
        /// Image reference.
        image: String,
    },
    #[command(about = about("prepare"))]
    Prepare {
        /// Root filesystem format.
        #[arg(long, default_value = DEFAULT_ROOTFS_FORMAT.as_str())]
        rootfs_format: RootfsFormat,
        /// Image reference or ID.
        image: String,
    },
    #[command(about = about("images"))]
    Images {
        /// Only show image IDs, one per image.
        #[arg(short, long)]
        quiet: bool,
    },
    #[command(about = about("rmi"))]
    Rmi {
        /// Also remove images used by stopped sandboxes or with several references. Never removes images of running sandboxes.
        #[arg(short, long)]
        force: bool,
        /// Image references or IDs.
        #[arg(required = true)]
        images: Vec<String>,
    },
    #[command(about = about("executor"))]
    Executor {
        #[command(subcommand)]
        command: ExecutorCommand,
    },
    #[command(about = about("run"), override_usage = "sbx run [OPTIONS] IMAGE [COMMAND] [ARG...]")]
    Run {
        #[command(flatten)]
        attach: AttachArgs,
        #[command(flatten)]
        args: RunArgs,
    },
    #[command(about = about("create"), override_usage = "sbx create [OPTIONS] IMAGE [COMMAND] [ARG...]")]
    Create(RunArgs),
    #[command(about = about("start"))]
    Start {
        /// Attach: stream output and wait for the main process. Without it, start detached and print the ID.
        #[arg(short = 'a', long)]
        attach: bool,
        /// Attach stdin.
        #[arg(short = 'i', long)]
        interactive: bool,
        /// Sandbox ID (or a unique prefix).
        id: String,
    },
    #[command(about = about("exec"), override_usage = "sbx exec [OPTIONS] ID COMMAND [ARG...]")]
    Exec(ExecArgs),
    #[command(about = about("cp"))]
    Cp {
        /// Source (host path or ID:PATH).
        src: String,
        /// Destination (host path or ID:PATH).
        dst: String,
    },
    #[command(about = about("stop"))]
    Stop(StopArgs),
    #[command(about = about("restart"))]
    Restart(StopArgs),
    #[command(about = about("kill"))]
    Kill {
        /// Signal to send (name or number).
        #[arg(short = 's', long, default_value = "KILL")]
        signal: String,
        /// Sandbox ID (or a unique prefix).
        #[arg(required = true)]
        ids: Vec<String>,
    },
    #[command(about = about("wait"))]
    Wait {
        /// Sandbox ID (or a unique prefix).
        #[arg(required = true)]
        ids: Vec<String>,
    },
    #[command(about = about("rm"))]
    Rm {
        /// Stop running sandboxes before removing them.
        #[arg(short, long)]
        force: bool,
        /// Sandbox ID (or a unique prefix).
        #[arg(required = true)]
        ids: Vec<String>,
    },
    #[command(about = about("attach"))]
    Attach {
        /// Do not forward stdin.
        #[arg(long)]
        no_stdin: bool,
        /// Forward received signals to the main process (when it has no TTY).
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        sig_proxy: bool,
        /// Key sequence for detaching (e.g. ctrl-p,ctrl-q).
        #[arg(long, default_value = crate::attach::DetachKeys::DEFAULT)]
        detach_keys: String,
        /// Sandbox ID (or a unique prefix).
        id: String,
    },
    #[command(about = about("ps"))]
    Ps {
        /// Show stopped sandboxes too.
        #[arg(short, long)]
        all: bool,
        /// Only show sandbox IDs (e.g. `sbx rm -f $(sbx ps -aq)`).
        #[arg(short, long)]
        quiet: bool,
    },
    #[command(about = about("inspect"))]
    Inspect {
        /// Sandbox ID (or a unique prefix).
        id: String,
    },
    #[command(about = about("logs"))]
    Logs {
        /// Keep streaming new output until the sandbox stops.
        #[arg(short, long)]
        follow: bool,
        /// Sandbox ID (or a unique prefix).
        id: String,
    },
    #[command(about = about("mcp"))]
    Mcp,
}

/// `executor` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum ExecutorCommand {
    /// List installed executors (* marks the default).
    Ls,
    /// Install an executor. Without --from, download it from the executor repositories (latest version unless given).
    /// The protocol version and architecture are checked before installing.
    Install {
        /// Install this instead of downloading: a bundle directory (sbx-executor at the top), a .tar.gz bundle,
        /// or a single executable. Requires NAME@VERSION.
        #[arg(long)]
        from: Option<PathBuf>,
        /// `NAME[@VERSION]`.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
    /// List executors in the repositories that can be installed on this host.
    Search {
        /// Only this name.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Set the default executor (latest installed version unless given).
    Use {
        /// `NAME[@VERSION]`.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
    /// Remove an installed executor.
    Rm {
        /// NAME@VERSION.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
    /// Manage executor repositories (earlier ones take precedence). The list starts with `official`.
    Repo {
        /// Subcommand.
        #[command(subcommand)]
        command: RepoCommand,
    },
}

/// `executor repo` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// List repositories in priority order.
    Ls,
    /// Add a repository at the end of the list. Its index is fetched when used.
    Add {
        /// Repository name.
        name: String,
        /// Repository URL (https, file, or http on localhost). The index is `<URL>/index.json`.
        url: String,
    },
    /// Remove a repository from the list (including `official`; the removal is kept).
    Rm {
        /// Repository name.
        name: String,
    },
}

/// `stop`과 `restart`의 옵션.
#[derive(Debug, Args)]
pub struct StopArgs {
    /// Signal to send first (name or number). Defaults to the sandbox's --stop-signal, the image's StopSignal, then SIGTERM.
    #[arg(short = 's', long)]
    pub signal: Option<String>,
    /// Seconds to wait before powering the guest off. Defaults to the sandbox's --stop-timeout, then 10.
    #[arg(short = 't', long = "time")]
    pub time: Option<u64>,
    /// Sandbox ID (or a unique prefix).
    #[arg(required = true)]
    pub ids: Vec<String>,
}

/// `run`에만 있는 옵션: 붙어서 실행하는지.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// Run in the background and print the ID. The sandbox outlives sbx; use `sbx attach` to attach.
    #[arg(short = 'd', long)]
    pub detach: bool,
}

/// `run`과 `create`의 옵션. 이미지 이름 뒤의 인자는 모두 명령으로 넘어간다(Docker와 같다).
#[derive(Debug, Args)]
pub struct RunArgs {
    /// Pull policy: missing, always, never.
    #[arg(long, default_value = "missing")]
    pub pull: PullPolicy,
    /// Root filesystem format.
    #[arg(long, default_value = DEFAULT_ROOTFS_FORMAT.as_str())]
    pub rootfs_format: RootfsFormat,
    /// Remove the sandbox when it exits (run only).
    #[arg(long)]
    pub rm: bool,
    /// Keep stdin open for the main process (kept in the sandbox's settings for every start).
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Allocate a TTY for the main process (kept in the sandbox's settings).
    #[arg(short = 't', long)]
    pub tty: bool,
    /// Environment variable K=V (K alone takes the host's value).
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    /// Read environment variables from a file (K=V or K per line, # comments). -e wins.
    #[arg(long)]
    pub env_file: Vec<PathBuf>,
    /// Working directory.
    #[arg(short = 'w', long = "workdir")]
    pub workdir: Option<String>,
    /// User (name, uid, name:group, uid:gid).
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    /// Override the image's ENTRYPOINT (an empty string clears it).
    #[arg(long)]
    pub entrypoint: Option<String>,
    /// Number of vCPUs (default 1).
    #[arg(long)]
    pub cpus: Option<String>,
    /// Memory (e.g. 512m, 1g; default 512m).
    #[arg(short = 'm', long)]
    pub memory: Option<String>,
    /// Name the sandbox; other commands accept it in place of the ID.
    #[arg(long)]
    pub name: Option<String>,
    /// Hostname (defaults to the short sandbox ID).
    #[arg(long)]
    pub hostname: Option<String>,
    /// Label k=v.
    #[arg(short = 'l', long = "label")]
    pub labels: Vec<String>,
    /// Live-share a host directory: `SRC:DST[:ro]` (short for --mount type=bind; needs executor support).
    #[arg(short = 'v', long = "volume")]
    pub volumes: Vec<String>,
    /// `type=bind|copy,source=...,target=...[,readonly]` (copy: copied in at start).
    #[arg(long = "mount")]
    pub mounts: Vec<String>,
    /// Network: none (no access) or allow (only --allow destinations, through a proxy).
    #[arg(long, default_value = "none")]
    pub network: String,
    /// Signal sent to the main process on stop. Defaults to the image's StopSignal, then SIGTERM.
    #[arg(long)]
    pub stop_signal: Option<String>,
    /// Seconds to wait after the stop signal before powering off (default 10).
    #[arg(long)]
    pub stop_timeout: Option<u32>,
    /// Destination allowed with --network allow: `HOST:PORT` (`*.example.com:443`, `10.0.0.5:*`, `[::1]:80`). Repeatable.
    #[arg(long = "allow")]
    pub allow: Vec<String>,
    /// Image reference or ID, then the command and its arguments. Everything after the image belongs to the
    /// command, even values that look like sbx options.
    #[arg(
        value_name = "IMAGE [COMMAND [ARG...]]",
        required = true,
        num_args = 1..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub image_and_command: Vec<String>,
}

impl RunArgs {
    /// 이미지 참조나 ID.
    pub fn image(&self) -> &str {
        &self.image_and_command[0]
    }

    /// 이미지 뒤의 명령과 인자. 비었으면 이미지의 기본 명령이다.
    pub fn command(&self) -> &[String] {
        &self.image_and_command[1..]
    }
}

/// `exec`의 옵션.
#[derive(Debug, Args)]
pub struct ExecArgs {
    /// Keep stdin open.
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Allocate a TTY.
    #[arg(short = 't', long)]
    pub tty: bool,
    /// Environment variable K=V (K alone takes the host's value).
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    /// Read environment variables from a file (K=V or K per line, # comments). -e wins.
    #[arg(long)]
    pub env_file: Vec<PathBuf>,
    /// Working directory.
    #[arg(short = 'w', long = "workdir")]
    pub workdir: Option<String>,
    /// User.
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    /// Sandbox ID (or a unique prefix), then the command and its arguments. Everything after the ID belongs to
    /// the command.
    #[arg(
        value_name = "ID COMMAND [ARG...]",
        required = true,
        num_args = 2..,
        trailing_var_arg = true,
        allow_hyphen_values = true
    )]
    pub id_and_command: Vec<String>,
}

impl ExecArgs {
    /// 샌드박스 ID(나 앞부분).
    pub fn id(&self) -> &str {
        &self.id_and_command[0]
    }

    /// 실행할 명령과 인자(비어 있지 않다).
    pub fn command(&self) -> &[String] {
        &self.id_and_command[1..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("sbx").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn run_stops_option_parsing_at_image() {
        let Command::Run { attach, args: r } =
            parse(&["run", "-i", "-e", "A=1", "alpine", "ls", "-l", "-e", "-i"]).command
        else {
            panic!("expected run");
        };
        assert!(r.interactive && !r.tty && !attach.detach);
        assert_eq!(r.image(), "alpine");
        assert_eq!(r.env, vec!["A=1"]);
        assert_eq!(r.command(), ["ls", "-l", "-e", "-i"]);
        assert_eq!(r.pull, PullPolicy::Missing);
        assert_eq!(r.rootfs_format, RootfsFormat::Erofs);
        assert_eq!(r.network, "none");
    }

    #[test]
    fn everything_after_the_image_or_id_is_the_command() {
        // 명령의 첫 인자가 sbx 옵션처럼 생겨도(curl -v, -i) 명령이다. Docker와 같다.
        let Command::Run { args: r, .. } =
            parse(&["run", "--rm", "curlimages/curl", "-v", "-i", "https://x"]).command
        else {
            panic!("expected run");
        };
        assert_eq!(r.image(), "curlimages/curl");
        assert_eq!(r.command(), ["-v", "-i", "https://x"]);
        assert!(r.volumes.is_empty() && !r.interactive && r.rm);
        let Command::Run { args: r, .. } = parse(&["run", "alpine"]).command else {
            panic!("expected run");
        };
        assert!(r.command().is_empty());
        let Command::Exec(x) = parse(&["exec", "-i", "abc", "-e", "X", "-w"]).command else {
            panic!("expected exec");
        };
        assert_eq!(
            (x.id(), x.command()),
            ("abc", &["-e".to_owned(), "X".into(), "-w".into()][..])
        );
        assert!(x.interactive && x.env.is_empty());
        // exec에는 명령이 있어야 한다.
        assert!(Cli::try_parse_from(["sbx", "exec", "abc"]).is_err());
    }

    #[test]
    fn definition_is_consistent() {
        // 인자 이름 충돌 같은 정의 오류를 실행하기 전에 잡는다.
        <Cli as clap::CommandFactory>::command().debug_assert();
    }

    #[test]
    fn global_options_anywhere() {
        let cli = parse(&["images", "--format", "json", "--cache-dir", "/c"]);
        assert_eq!(cli.format, Format::Json);
        assert_eq!(cli.cache_dir, Some("/c".into()));
    }

    #[test]
    fn rejects_unknown_values() {
        assert!(Cli::try_parse_from(["sbx", "run", "--pull", "sometimes", "alpine"]).is_err());
        assert!(
            Cli::try_parse_from(["sbx", "prepare", "--rootfs-format", "ext4", "alpine"]).is_err()
        );
        assert!(Cli::try_parse_from(["sbx", "rmi"]).is_err());
    }
}
