//! clap 정의. 명령 이름과 설명은 [`commands`](crate::commands)가 정본이다.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use sandbox_model::RootfsFormat;

use crate::behavior::{DEFAULT_ROOTFS_FORMAT, PullPolicy};
use crate::commands::about;

/// 출력 형식.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// 사람이 읽는 표.
    Text,
    /// 스크립트용 JSON.
    Json,
}

/// `sbx`.
#[derive(Debug, Parser)]
#[command(name = "sbx", version, about = "microVM 샌드박스")]
pub struct Cli {
    /// 샌드박스 상태 디렉터리.
    #[arg(long, global = true, env = "SBX_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// 이미지 캐시 디렉터리.
    #[arg(long, global = true, env = "SBX_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,
    /// 게스트 연결 소켓 디렉터리 (짧은 경로여야 한다).
    #[arg(long, global = true, env = "SBX_SOCKET_DIR")]
    pub socket_dir: Option<PathBuf>,
    /// 게스트 init initramfs. 기본은 sbx에 내장된 것.
    #[arg(long, global = true, env = "SBX_GUEST_INIT")]
    pub guest_init: Option<PathBuf>,
    /// 실행기 실행 파일 (`executor-proto` 규약). 경로가 아니면 PATH에서 찾는다.
    /// 기본은 설치한 기본 실행기(`sbx executor use`), 없으면 PATH의 `sbx-executor`.
    #[arg(long, global = true, env = "SBX_EXECUTOR")]
    pub executor: Option<PathBuf>,
    /// detached 샌드박스를 소유할 shim 실행 파일. 기본은 sbx 옆의 `sbx-shim`, 없으면 PATH의 `sbx-shim`.
    #[arg(long, global = true, env = "SBX_SHIM")]
    pub shim: Option<PathBuf>,
    /// 출력 형식.
    #[arg(long, global = true, value_enum, default_value_t = Format::Text)]
    pub format: Format,
    /// 명령.
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
        /// linux/amd64 또는 linux/arm64. 기본은 호스트 아키텍처.
        #[arg(long)]
        platform: Option<String>,
        /// 이미지 참조.
        image: String,
    },
    #[command(about = about("prepare"))]
    Prepare {
        /// rootfs 형식.
        #[arg(long, default_value = DEFAULT_ROOTFS_FORMAT.as_str())]
        rootfs_format: RootfsFormat,
        /// 이미지 참조 또는 ID.
        image: String,
    },
    #[command(about = about("images"))]
    Images,
    #[command(about = about("rmi"))]
    Rmi {
        /// 정지된 샌드박스가 쓰는 이미지, 참조가 여럿인 이미지도 지운다. 실행 중인 샌드박스의 이미지는 지우지 않는다.
        #[arg(short, long)]
        force: bool,
        /// 이미지 참조 또는 ID.
        #[arg(required = true)]
        images: Vec<String>,
    },
    #[command(about = about("executor"))]
    Executor {
        #[command(subcommand)]
        command: ExecutorCommand,
    },
    #[command(about = about("run"))]
    Run {
        #[command(flatten)]
        attach: AttachArgs,
        #[command(flatten)]
        args: RunArgs,
    },
    #[command(about = about("create"))]
    Create(RunArgs),
    #[command(about = about("start"))]
    Start {
        /// 붙어서 출력을 받고 기본 프로세스가 끝날 때까지 기다린다. 없으면 shim으로 시작하고 ID를 출력한다.
        #[arg(short = 'a', long)]
        attach: bool,
        /// 표준 입력을 연다 (붙는다).
        #[arg(short = 'i', long)]
        interactive: bool,
        /// 샌드박스 ID.
        id: String,
    },
    #[command(about = about("exec"))]
    Exec(ExecArgs),
    #[command(about = about("cp"))]
    Cp {
        /// 원본 (호스트 경로 또는 ID:경로).
        src: String,
        /// 대상 (호스트 경로 또는 ID:경로).
        dst: String,
    },
    #[command(about = about("stop"))]
    Stop(StopArgs),
    #[command(about = about("restart"))]
    Restart(StopArgs),
    #[command(about = about("kill"))]
    Kill {
        /// 보낼 시그널 (이름 또는 번호).
        #[arg(short = 's', long, default_value = "KILL")]
        signal: String,
        /// 샌드박스 ID.
        #[arg(required = true)]
        ids: Vec<String>,
    },
    #[command(about = about("wait"))]
    Wait {
        /// 샌드박스 ID.
        #[arg(required = true)]
        ids: Vec<String>,
    },
    #[command(about = about("rm"))]
    Rm {
        /// 실행 중이면 멈추고 지운다.
        #[arg(short, long)]
        force: bool,
        /// 샌드박스 ID.
        #[arg(required = true)]
        ids: Vec<String>,
    },
    #[command(about = about("attach"))]
    Attach {
        /// 표준 입력을 전하지 않는다.
        #[arg(long)]
        no_stdin: bool,
        /// 터미널이 아닐 때 받은 시그널을 기본 프로세스에 전한다.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        sig_proxy: bool,
        /// 떨어지는 키 순서 (예: ctrl-p,ctrl-q).
        #[arg(long, default_value = crate::attach::DetachKeys::DEFAULT)]
        detach_keys: String,
        /// 샌드박스 ID.
        id: String,
    },
    #[command(about = about("ps"))]
    Ps {
        /// 멈춘 샌드박스도 보여준다.
        #[arg(short, long)]
        all: bool,
    },
    #[command(about = about("inspect"))]
    Inspect {
        /// 샌드박스 ID.
        id: String,
    },
    #[command(about = about("logs"))]
    Logs {
        /// 샌드박스가 멈출 때까지 새 출력을 계속 보여준다.
        #[arg(short, long)]
        follow: bool,
        /// 샌드박스 ID.
        id: String,
    },
}

/// `executor` 아래 명령.
#[derive(Debug, Subcommand)]
pub enum ExecutorCommand {
    /// 설치한 실행기를 보여준다 (기본 실행기에 *).
    Ls,
    /// 실행기를 설치한다. 설치한 실행기는 규약 버전과 아키텍처를 확인한다.
    Install {
        /// 묶음 디렉터리(맨 위에 sbx-executor) 또는 실행 파일 하나.
        #[arg(long)]
        from: PathBuf,
        /// NAME@VERSION.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
    /// 기본 실행기를 정한다. 버전을 빼면 가장 높은 버전.
    Use {
        /// `NAME[@VERSION]`.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
    /// 실행기를 지운다.
    Rm {
        /// NAME@VERSION.
        #[arg(value_name = "EXECUTOR")]
        spec: String,
    },
}

/// `stop`과 `restart`의 옵션.
#[derive(Debug, Args)]
pub struct StopArgs {
    /// 먼저 보낼 시그널 (이름 또는 번호). 기본은 샌드박스의 `--stop-signal`, 이미지의 `StopSignal`, SIGTERM 순.
    #[arg(short = 's', long)]
    pub signal: Option<String>,
    /// 기다리는 시간(초). 넘으면 게스트를 끈다. 기본은 샌드박스의 `--stop-timeout`, 없으면 10.
    #[arg(short = 't', long = "time")]
    pub time: Option<u64>,
    /// 샌드박스 ID.
    #[arg(required = true)]
    pub ids: Vec<String>,
}

/// `run`에만 있는 옵션: 붙어서 실행하는지.
#[derive(Debug, Args)]
pub struct AttachArgs {
    /// 붙지 않고 시작해 ID를 출력한다. 샌드박스는 sbx가 끝나도 남는다(`sandbox-shim`). `sbx attach`로 붙는다.
    #[arg(short = 'd', long)]
    pub detach: bool,
}

/// `run`과 `create`의 옵션. 이미지 이름 뒤의 인자는 모두 명령으로 넘어간다(Docker와 같다).
#[derive(Debug, Args)]
pub struct RunArgs {
    /// 이미지 준비: missing, always, never.
    #[arg(long, default_value = "missing")]
    pub pull: PullPolicy,
    /// rootfs 형식.
    #[arg(long, default_value = DEFAULT_ROOTFS_FORMAT.as_str())]
    pub rootfs_format: RootfsFormat,
    /// 끝나면 지운다. run에서만 된다(create에서는 오류).
    #[arg(long)]
    pub rm: bool,
    /// 기본 프로세스의 표준 입력을 연다. 샌드박스 설정으로 남아 시작할 때마다 연다.
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// 기본 프로세스에 터미널을 붙인다. 샌드박스 설정으로 남는다.
    #[arg(short = 't', long)]
    pub tty: bool,
    /// 환경 변수 K=V.
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    /// 작업 디렉터리.
    #[arg(short = 'w', long = "workdir")]
    pub workdir: Option<String>,
    /// 사용자 (name, uid, name:group, uid:gid).
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    /// 이미지 ENTRYPOINT를 대체한다. 빈 문자열이면 지운다.
    #[arg(long)]
    pub entrypoint: Option<String>,
    /// vCPU 개수.
    #[arg(long)]
    pub cpus: Option<String>,
    /// 메모리 (예: 512m, 1g).
    #[arg(short = 'm', long)]
    pub memory: Option<String>,
    /// hostname.
    #[arg(long)]
    pub hostname: Option<String>,
    /// 라벨 k=v.
    #[arg(short = 'l', long = "label")]
    pub labels: Vec<String>,
    /// `SRC:DST[:ro]` (type=bind의 줄임말).
    #[arg(short = 'v', long = "volume")]
    pub volumes: Vec<String>,
    /// `type=bind|copy,source=...,target=...[,readonly]`
    #[arg(long = "mount")]
    pub mounts: Vec<String>,
    /// 네트워크: none(차단) 또는 allow(--allow 목록만, 프록시로).
    #[arg(long, default_value = "none")]
    pub network: String,
    /// 멈출 때 기본 프로세스에 먼저 보낼 시그널. 기본은 이미지의 `StopSignal`, 없으면 SIGTERM.
    #[arg(long)]
    pub stop_signal: Option<String>,
    /// 멈출 때 시그널 뒤 기다리는 시간(초). 기본은 10.
    #[arg(long)]
    pub stop_timeout: Option<u32>,
    /// `--network allow`에서 허용할 `HOST:PORT` (`*.example.com:443`, `10.0.0.5:*`, `[::1]:80`). 여러 번 준다.
    #[arg(long = "allow")]
    pub allow: Vec<String>,
    /// 이미지 참조 또는 ID.
    pub image: String,
    /// 실행할 명령과 인자.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

/// `exec`의 옵션.
#[derive(Debug, Args)]
pub struct ExecArgs {
    /// 표준 입력을 연다.
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// 터미널을 할당한다.
    #[arg(short = 't', long)]
    pub tty: bool,
    /// 환경 변수 K=V.
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    /// 작업 디렉터리.
    #[arg(short = 'w', long = "workdir")]
    pub workdir: Option<String>,
    /// 사용자.
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    /// 샌드박스 ID.
    pub id: String,
    /// 실행할 명령과 인자.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
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
        assert_eq!(r.image, "alpine");
        assert_eq!(r.env, vec!["A=1"]);
        assert_eq!(r.command, vec!["ls", "-l", "-e", "-i"]);
        assert_eq!(r.pull, PullPolicy::Missing);
        assert_eq!(r.rootfs_format, RootfsFormat::Erofs);
        assert_eq!(r.network, "none");
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
