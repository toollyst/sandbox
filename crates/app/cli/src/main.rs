//! `sbx` 진입점. 종료 코드는 [`sbx_cli::behavior`]의 종료 코드 표를 따른다.
//! `--format json`이면 오류도 JSON 한 줄이다([`sbx_cli::output`]).
//!
//! 맨 먼저 셸 자동완성 요청인지 본다: 환경 변수 `COMPLETE`(비어 있지 않고 `0`이 아님)가 있으면 명령을 실행하지 않고
//! 후보(또는 등록 스크립트)를 찍고 끝낸다([`sbx_cli::complete`]). 비동기 런타임도 그 뒤에 만든다(탭마다 빨라야 한다).

use std::process::ExitCode;

use clap::{CommandFactory, FromArgMatches, Parser};

use sbx_cli::cli::{Cli, Format};
use sbx_cli::{app, complete, output, update};

fn main() -> ExitCode {
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

#[tokio::main]
async fn run(cli: Cli) -> ExitCode {
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
    ExitCode::from(code)
}
