//! `sbx` 진입점. 종료 코드는 [`sbx_cli::behavior`]의 종료 코드 표를 따른다.
//! `--format json`이면 오류도 JSON 한 줄이다([`sbx_cli::output`]).
//!
//! 맨 먼저 셸 자동완성 요청인지 본다: 환경 변수 `COMPLETE`(비어 있지 않고 `0`이 아님)가 있으면 명령을 실행하지 않고
//! 후보(또는 등록 스크립트)를 찍고 끝낸다([`sbx_cli::complete`]). 비동기 런타임도 그 뒤에 만든다(탭마다 빨라야 한다).
//!
//! 모든 일은 스택 크기를 정한 스레드([`MAIN_STACK`])에서 한다. 주 스레드의 스택은 OS가 정해서 Windows는 1 MiB뿐이다
//! (Linux와 macOS는 8 MiB). 디버그 빌드는 명령 트리(clap)를 만드는 데만 1 MiB 가까이 써서 Windows에서 모든 명령이
//! `has overflowed its stack`으로 끝났다.

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};

use sbx_cli::cli::{Cli, Format, MAIN_STACK};
use sbx_cli::{app, complete, output, update};

fn main() -> ExitCode {
    let main = std::thread::Builder::new()
        .name("main".to_owned())
        .stack_size(MAIN_STACK)
        .spawn(sbx);
    match main.map(std::thread::JoinHandle::join) {
        Ok(Ok(code)) => code,
        // 패닉: 메시지는 패닉 훅이 이미 찍었다. 주 스레드에서 패닉한 것처럼 101로 끝낸다.
        Ok(Err(_)) => ExitCode::from(101),
        Err(e) => {
            eprintln!("sbx: cannot start the main thread: {e}");
            ExitCode::FAILURE
        }
    }
}

fn sbx() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(Cli::command)
        .var(complete::ENV_VAR)
        .complete();
    run(parse())
}

/// 인자를 읽는다. `--version`이면 새 버전 알림 기록을 보고 `0.2.1 (0.2.2 available)`처럼 찍는다([`update::version_text`]).
fn parse() -> Cli {
    let asks_version = std::env::args_os()
        .skip(1)
        .take_while(|a| a != "--")
        .any(|a| a == "--version" || a == "-V");
    if !asks_version {
        return Cli::parse();
    }
    let cache = std::env::var_os("SBX_CACHE_DIR")
        .map(std::path::PathBuf::from)
        .or_else(sbx_cli::dirs::cache_dir);
    // clap은 'static 글만 받는다. 한 번만 만든다.
    let version: &'static str = Box::leak(update::version_text(cache.as_deref()).into_boxed_str());
    let mut matches = Cli::command().version(version).get_matches();
    Cli::from_arg_matches_mut(&mut matches).unwrap_or_else(|e| e.exit())
}

/// 명령을 비동기 런타임에서 돌린다. 끝나면 런타임을 기다리지 않고 닫는다([`SHUTDOWN_WAIT`]): 버린 새 버전 확인이
/// 이름 풀이(`getaddrinfo`, 막는 스레드)에 걸려 있으면 런타임을 그냥 버릴 때 그것이 끝날 때까지 종료가 늦어진다.
fn run(cli: Cli) -> ExitCode {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("sbx: cannot start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let code = rt.block_on(command(cli));
    rt.shutdown_timeout(SHUTDOWN_WAIT);
    ExitCode::from(code)
}

/// 명령이 끝난 뒤 남은 막는 작업을 기다리는 한도. 명령이 기다려야 하는 것은 이미 다 기다렸다.
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_millis(200);

async fn command(cli: Cli) -> u8 {
    let json = cli.format == Format::Json;
    let check = update::start(&cli);
    let code = match sbx_cli::run(cli).await {
        Ok(code) => code,
        Err(e) => {
            let code = e.downcast_ref::<app::Failure>().map_or(1, |f| f.code);
            if json {
                output::print_error(None, &e.to_string(), Some(code));
            } else {
                eprintln!("sbx: {e}");
            }
            code
        }
    };
    update::finish(check).await;
    code
}
