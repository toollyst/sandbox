//! clap 정의. 명령 이름과 설명은 [`commands`](crate::commands)가 정본이다.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use sbx_model::RootfsFormat;

use crate::behavior::{DEFAULT_ROOTFS_FORMAT, PullPolicy};
use crate::commands::about;
use crate::complete;

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
    /// Executor to use: an installed `NAME[@VERSION]`, or a program that speaks the executor protocol (a path, or a
    /// name looked up on PATH). Without it: the executor a sandbox was created with (for start and restart), then
    /// `.sbx-executor` in this folder or the nearest one above, then the default (`sbx executor use`), then
    /// `sbx-executor` on PATH.
    #[arg(long, global = true, env = "SBX_EXECUTOR")]
    pub executor: Option<PathBuf>,
    /// Shim program that owns detached sandboxes. Defaults to `sbx-shim` next to sbx, then on PATH.
    #[arg(long, global = true, env = "SBX_SHIM")]
    pub shim: Option<PathBuf>,
    /// Output format.
    #[arg(long, global = true, value_enum, default_value_t = Format::Text)]
    pub format: Format,
    /// With --format json: run this jq expression on the output and print each result on its own line
    /// (strings without quotes). Built in (jaq); jq itself is not needed.
    #[arg(long, global = true, value_name = "EXPR")]
    pub jq: Option<String>,
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
        #[arg(add = complete::image())]
        image: String,
    },
    #[command(about = about("login"))]
    Login {
        /// User name. Asked for when it is missing and stdin is a terminal.
        #[arg(short = 'u', long)]
        username: Option<String>,
        /// Read the password or access token from stdin (one line). Without it, sbx asks on the terminal.
        #[arg(long)]
        password_stdin: bool,
        /// Registry, e.g. ghcr.io or localhost:5000. Defaults to Docker Hub.
        server: Option<String>,
    },
    #[command(about = about("logout"))]
    Logout {
        /// Registry, e.g. ghcr.io. Defaults to Docker Hub.
        server: Option<String>,
    },
    #[command(about = about("prepare"))]
    Prepare {
        /// Root filesystem format.
        #[arg(long, default_value = DEFAULT_ROOTFS_FORMAT.as_str())]
        rootfs_format: RootfsFormat,
        /// Image reference or ID.
        #[arg(add = complete::image())]
        image: String,
    },
    #[command(about = about("images"))]
    Images {
        /// Only show image IDs, one per image.
        #[arg(short, long)]
        quiet: bool,
        /// Filter: reference=PATTERN, dangling=true|false, label=KEY[=VALUE]. Different keys must all match, a repeated key any of its values.
        #[arg(short = 'f', long = "filter", value_name = "KEY=VALUE")]
        filter: Vec<String>,
    },
    #[command(about = about("rmi"))]
    Rmi {
        /// Also remove images used by stopped sandboxes or with several references. Never removes images of running sandboxes.
        #[arg(short, long)]
        force: bool,
        /// Image references or IDs.
        #[arg(required = true, add = complete::image())]
        images: Vec<String>,
    },
    #[command(about = about("executor"))]
    Executor {
        #[command(subcommand)]
        command: ExecutorCommand,
    },
    #[command(about = about("info"))]
    Info,
    #[command(about = about("system"))]
    System {
        #[command(subcommand)]
        command: SystemCommand,
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
        /// Sandbox name, ID, or unique ID prefix.
        #[arg(add = complete::sandbox())]
        id: String,
    },
    #[command(about = about("exec"), override_usage = "sbx exec [OPTIONS] SANDBOX COMMAND [ARG...]")]
    Exec(ExecArgs),
    #[command(about = about("cp"))]
    Cp {
        /// Source (host path, or SANDBOX:PATH where SANDBOX is a name or ID).
        #[arg(add = complete::cp_endpoint_arg())]
        src: String,
        /// Destination (host path, or SANDBOX:PATH where SANDBOX is a name or ID).
        #[arg(add = complete::cp_endpoint_arg())]
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
        /// Sandbox name, ID, or unique ID prefix.
        #[arg(required = true, add = complete::sandbox())]
        ids: Vec<String>,
    },
    #[command(about = about("wait"))]
    Wait {
        /// Sandbox name, ID, or unique ID prefix.
        #[arg(required = true, add = complete::sandbox())]
        ids: Vec<String>,
    },
    #[command(about = about("rm"))]
    Rm {
        /// Stop running sandboxes before removing them.
        #[arg(short, long)]
        force: bool,
        /// Sandbox name, ID, or unique ID prefix.
        #[arg(required = true, add = complete::sandbox())]
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
        /// Sandbox name, ID, or unique ID prefix.
        #[arg(add = complete::sandbox())]
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
        /// Filter: id=PREFIX, name=TEXT, label=KEY[=VALUE], status=STATE, exited=CODE, ancestor=IMAGE.
        /// Different keys must all match, a repeated key any of its values. status and exited imply -a.
        #[arg(short = 'f', long = "filter", value_name = "KEY=VALUE")]
        filter: Vec<String>,
    },
    #[command(about = about("stats"))]
    Stats {
        /// Print one sample and exit. Without a terminal, and with --format json, there is always one sample.
        #[arg(long)]
        no_stream: bool,
        /// Sandbox names, IDs or ID prefixes (default: every running sandbox).
        #[arg(add = complete::sandbox())]
        ids: Vec<String>,
    },
    #[command(about = about("inspect"))]
    Inspect {
        /// Sandbox name, ID, or unique ID prefix.
        #[arg(add = complete::sandbox())]
        id: String,
    },
    #[command(about = about("logs"))]
    Logs(LogsArgs),
    #[command(about = about("events"))]
    Events {
        /// Replay events since this time first: RFC 3339 (2026-11-10T13:20:14Z), Unix seconds, or a duration ago (10m).
        /// Without it, only new events are shown.
        #[arg(long)]
        since: Option<String>,
        /// Stop at this time (same forms as --since). Without it, keep streaming.
        #[arg(long)]
        until: Option<String>,
        /// Filter: type=sandbox|image, event=ACTION, sandbox=ID-PREFIX|NAME, image=IMAGE.
        /// Different keys must all match, a repeated key any of its values.
        #[arg(short = 'f', long = "filter", value_name = "KEY=VALUE")]
        filter: Vec<String>,
    },
    #[command(about = about("tui"), after_help = crate::tui::keys::help_text())]
    Tui,
    #[command(about = about("mcp"))]
    Mcp {
        /// Let tools use every sandbox in the data directory, not only the ones created through MCP.
        #[arg(long)]
        all_sandboxes: bool,
        /// At most this many sandboxes created through MCP may exist at once (running or stopped, until removed).
        #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u32).range(1..))]
        max_sandboxes: u32,
        /// The most vCPUs one tool call may ask for.
        #[arg(long, default_value_t = 4, value_parser = clap::value_parser!(u8).range(1..))]
        max_cpus: u8,
        /// The most memory one tool call may ask for (e.g. 2g).
        #[arg(long, default_value = "4g", value_parser = crate::parse::memory_mib)]
        max_memory: u32,
        /// The largest writable disk one tool call may ask for (e.g. 20g).
        #[arg(long, default_value = "16g", value_parser = crate::parse::disk_mib)]
        max_disk: u32,
        /// Stop every sandbox created through MCP at most this long after it starts (e.g. 1h); agents may ask for
        /// less with `lifetime`. By default they run until stopped or removed.
        #[arg(long, value_name = "DURATION", value_parser = crate::parse::lifetime_secs)]
        max_lifetime: Option<u64>,
        /// Let agents allow loopback, private and link-local IP addresses and `localhost` (by default they can
        /// only allow public hosts, so an agent can't reach services on this machine, the LAN or cloud metadata).
        #[arg(long)]
        allow_private_network: bool,
    },
    #[command(
        about = about("completion"),
        long_about = "Print the shell script that enables tab completion. Besides commands and options, it completes \
sandbox names and IDs, image references, and installed executors: on each TAB the shell runs \
`COMPLETE=<shell> sbx -- <words>`, which reads the data and cache directories (--data-dir and --cache-dir \
typed on the line, then SBX_DATA_DIR and SBX_CACHE_DIR, then the defaults) and never changes anything. \
The script is the same as `COMPLETE=<shell> sbx` prints. Load it at shell startup instead of saving it to a file, \
so it always matches the installed sbx.",
        after_help = "Examples:\n  echo 'source <(sbx completion bash)' >> ~/.bashrc\n  echo 'source <(sbx completion zsh)' >> ~/.zshrc\n  echo 'sbx completion fish | source' > ~/.config/fish/completions/sbx.fish\n  echo 'sbx completion powershell | Out-String | Invoke-Expression' >> $PROFILE"
    )]
    Completion {
        /// Shell to enable completion for.
        shell: clap_complete::Shell,
    },
}

/// `system` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum SystemCommand {
    /// Show how much disk images, sandboxes and executors take, and how much prune could free.
    Df,
    /// Remove stopped sandboxes, unused layers and leftovers (-a: also images no sandbox uses).
    Prune {
        /// Also remove every image no remaining sandbox uses.
        #[arg(short = 'a', long)]
        all: bool,
        /// Don't ask for confirmation.
        #[arg(short = 'f', long)]
        force: bool,
        /// Only sandboxes matching: until=TIME (created before), label=KEY[=VALUE].
        #[arg(long = "filter", value_name = "KEY=VALUE")]
        filter: Vec<String>,
    },
}

/// `executor` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum ExecutorCommand {
    /// List installed executors (* marks the default).
    Ls,
    /// Install an executor. Without --from, download it from the executor repositories (latest version unless given).
    /// If it is a VMM package, the adapter it needs is installed first. The protocol version and architecture are
    /// checked before installing.
    Install {
        /// Install this instead of downloading: a bundle directory (sbx-executor at the top), a .tar.gz bundle,
        /// or a single executable. Requires NAME@VERSION.
        #[arg(long)]
        from: Option<PathBuf>,
        /// With --from: install it as a VMM package run by this installed adapter (it has no sbx-executor).
        #[arg(long, requires = "from")]
        adapter: Option<String>,
        /// Take it from this repository. Without it, a name comes from the first repository (in `repo ls` order)
        /// that publishes it, and a name already installed only comes from where it came from before.
        #[arg(long, conflicts_with = "from", add = complete::repository())]
        repo: Option<String>,
        /// `NAME[@VERSION]`. A main version (`wick@1.18.0`) installs its newest patch (`1.18.0+wick.2`);
        /// no version or `@latest` installs the highest version.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
    /// List executors in the repositories that can be installed on this host: the newest patch of each main version.
    #[command(visible_alias = "ls-remote")]
    Search {
        /// Every patch, not just the newest of each main version.
        #[arg(short, long)]
        all: bool,
        /// Only this name.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Set the default executor (latest installed version unless given).
    Use {
        /// `NAME[@VERSION]`.
        #[arg(value_name = "EXECUTOR", add = complete::executor())]
        spec: String,
    },
    /// Show the executor a new sandbox would use here, and why (--executor, .sbx-executor, default, or PATH).
    Current,
    /// Print the program that runs an installed executor (for a VMM package, its adapter).
    Which {
        /// `NAME[@VERSION]`. Defaults to the current executor.
        #[arg(value_name = "EXECUTOR", add = complete::executor())]
        spec: Option<String>,
    },
    /// Remove an installed executor. Refused while a sandbox is pinned to it, unless --force.
    Rm {
        /// Remove even if sandboxes are pinned to it (they will not start until it is installed again).
        #[arg(short, long)]
        force: bool,
        /// NAME@VERSION.
        #[arg(value_name = "EXECUTOR", add = complete::executor())]
        spec: String,
    },
    /// Remove versions nothing uses: executors that are not the default, not the highest version of their name, and
    /// not pinned by a sandbox; adapters that are not the highest version of their name.
    Prune {
        /// Only print what would be removed.
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
    /// Manage adapters: programs that speak the executor protocol and run a VMM package.
    Adapter {
        /// Subcommand.
        #[command(subcommand)]
        command: AdapterCommand,
    },
    /// Manage executor repositories (earlier ones take precedence). The list starts with `official`.
    Repo {
        /// Subcommand.
        #[command(subcommand)]
        command: RepoCommand,
    },
}

/// `executor adapter` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum AdapterCommand {
    /// List installed adapters and the VMM packages that use them.
    Ls,
    /// Install an adapter (latest version unless given). Without --from, download it from the repositories.
    Install {
        /// Install this instead of downloading: a bundle directory, a .tar.gz bundle, or a single executable.
        /// Requires NAME@VERSION.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Take it from this repository (see `executor install --repo`).
        #[arg(long, conflicts_with = "from", add = complete::repository())]
        repo: Option<String>,
        /// `NAME[@VERSION]`.
        #[arg(value_name = "ADAPTER")]
        spec: String,
    },
    /// List adapters in the repositories that can be installed on this host: the newest patch of each main version.
    #[command(visible_alias = "ls-remote")]
    Search {
        /// Every patch, not just the newest of each main version.
        #[arg(short, long)]
        all: bool,
        /// Only this name.
        #[arg(value_name = "NAME")]
        name: Option<String>,
    },
    /// Remove an installed adapter. Refused if it is the last version an installed VMM package needs, unless --force.
    Rm {
        /// Remove even if VMM packages need it.
        #[arg(short, long)]
        force: bool,
        /// NAME@VERSION.
        #[arg(value_name = "ADAPTER", add = complete::adapter())]
        spec: String,
    },
}

/// `executor repo` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum RepoCommand {
    /// List repositories in priority order.
    Ls,
    /// Add a repository at the end of the list. Its index is fetched when used. Give the repository's minisign
    /// public key with --key (its index must then be signed and fresh), or --unsigned to rely on TLS alone.
    Add {
        /// Repository name.
        name: String,
        /// Repository URL (https, file, or http on localhost). The index is `<URL>/v2/index.json`, or
        /// `<URL>/index.json` if there is none (unsigned repositories only).
        url: String,
        /// A minisign public key (the one-line `RW…` base64) that signs the index. Repeat it while the repository
        /// rotates keys: any one of them may sign.
        #[arg(long = "key", value_name = "PUBLIC_KEY")]
        keys: Vec<String>,
        /// Add it without a key: whoever controls the repository host or its TLS controls what you install.
        #[arg(long, conflicts_with = "keys")]
        unsigned: bool,
    },
    /// Remove a repository from the list (including `official`; the removal is kept).
    Rm {
        /// Repository name.
        name: String,
    },
}

/// `logs`의 옵션.
#[derive(Debug, Args)]
pub struct LogsArgs {
    /// Keep streaming new output until the sandbox stops (or until --until).
    #[arg(short, long)]
    pub follow: bool,
    /// Show only the last N lines (`all` for everything). With --since/--until, the last N lines in that range.
    #[arg(short = 'n', long, default_value = "all")]
    pub tail: String,
    /// Prefix each line with the time sbx received it (RFC 3339, nanoseconds).
    #[arg(short, long)]
    pub timestamps: bool,
    /// Show only output since this time: RFC 3339 (2026-11-10T13:20:14Z), Unix seconds, or a duration ago (10m).
    #[arg(long)]
    pub since: Option<String>,
    /// Show only output until this time (same forms as --since). With -f, stop following then.
    #[arg(long)]
    pub until: Option<String>,
    /// Sandbox name, ID, or unique ID prefix.
    #[arg(add = complete::sandbox())]
    pub id: String,
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
    /// Sandbox name, ID, or unique ID prefix.
    #[arg(required = true, add = complete::sandbox())]
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
    /// Stop the sandbox this long after each start (e.g. 30m, 1h; like podman --timeout): stop signal, then power off
    /// after the stop timeout. Without it a sandbox runs until its main process ends or it is stopped.
    #[arg(long, value_name = "DURATION", value_parser = crate::parse::lifetime_secs)]
    pub timeout: Option<u64>,
    /// Writable disk: `size=SIZE` (e.g. size=20g; default 1g, at least 64m). Sparse, so the host only spends what
    /// the sandbox writes.
    #[arg(long = "storage-opt", value_name = "size=SIZE", value_parser = crate::parse::storage_opt)]
    pub storage_opt: Option<u32>,
    /// Publish a sandbox TCP port on the host: [IP:][HOST_PORT:]PORT (e.g. 8080:80). IP defaults to 127.0.0.1, so
    /// only this machine can connect (Docker's default is every address; write 0.0.0.0: for that). Without
    /// HOST_PORT a free port is picked; `sbx inspect` shows it.
    #[arg(short = 'p', long = "publish", value_name = "[IP:][HOST_PORT:]PORT", value_parser = crate::parse::port_mapping)]
    pub publish: Vec<sbx_model::PortMapping>,
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
        allow_hyphen_values = true,
        add = complete::first_image(<Cli as clap::CommandFactory>::command, "image_and_command")
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
    /// Start the command in the background and return at once. Its output is discarded; it runs until it ends or
    /// the sandbox stops.
    #[arg(short = 'd', long, conflicts_with_all = ["interactive", "tty", "timeout"])]
    pub detach: bool,
    /// Kill the command (SIGKILL) if it is still running after DURATION (e.g. 30s, 5m); sbx then exits with 124.
    #[arg(long, value_name = "DURATION", value_parser = crate::parse::lifetime_secs)]
    pub timeout: Option<u64>,
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
    /// Sandbox name, ID, or unique ID prefix, then the command and its arguments. Everything after the sandbox belongs to
    /// the command.
    #[arg(
        value_name = "SANDBOX COMMAND [ARG...]",
        required = true,
        num_args = 2..,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        add = complete::first_sandbox(<Cli as clap::CommandFactory>::command, "id_and_command")
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
    fn completion_finds_the_first_value_by_parsing_the_words_before() {
        use clap::CommandFactory;
        let n = |id: &str, words: &[&str]| {
            let words: Vec<std::ffi::OsString> = words.iter().map(Into::into).collect();
            complete::given_values(Cli::command(), id, &words)
        };
        let image = "image_and_command";
        assert_eq!(n(image, &["sbx", "run"]), 0);
        assert_eq!(
            n(
                image,
                &["sbx", "--data-dir", "/d", "run", "-e", "A=1", "--rm"]
            ),
            0
        );
        assert_eq!(n(image, &["sbx", "run", "-e"]), 0);
        assert_eq!(n(image, &["sbx", "run", "alpine"]), 1);
        assert_eq!(n(image, &["sbx", "create", "alpine", "sh", "-c"]), 3);
        assert_eq!(n(image, &["sbx", "ps"]), 0);
        assert_eq!(n("id_and_command", &["sbx", "exec", "-i", "web"]), 1);
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
