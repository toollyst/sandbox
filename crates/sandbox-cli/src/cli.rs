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
    /// 실행기.
    #[arg(
        long,
        global = true,
        default_value = "firecracker",
        env = "SBX_EXECUTOR"
    )]
    pub executor: String,
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
    #[command(about = about("run"))]
    Run(RunArgs),
    #[command(about = about("create"))]
    Create(RunArgs),
    #[command(about = about("start"))]
    Start {
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
    Stop {
        /// 종료를 기다리는 시간(초). 넘으면 강제로 끝낸다.
        #[arg(short = 't', long = "time", default_value_t = 10)]
        time: u64,
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
    /// 호출한 프로세스가 끝나도 남긴다.
    #[arg(short = 'd', long)]
    pub detach: bool,
    /// 끝나면 지운다.
    #[arg(long)]
    pub rm: bool,
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
    /// 네트워크. 지금은 none만.
    #[arg(long, default_value = "none")]
    pub network: String,
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
        let Command::Run(r) = parse(&["run", "-e", "A=1", "alpine", "ls", "-l", "-e"]).command
        else {
            panic!("expected run");
        };
        assert_eq!(r.image, "alpine");
        assert_eq!(r.env, vec!["A=1"]);
        assert_eq!(r.command, vec!["ls", "-l", "-e"]);
        assert_eq!(r.pull, PullPolicy::Missing);
        assert_eq!(r.rootfs_format, RootfsFormat::Erofs);
        assert_eq!(r.network, "none");
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
