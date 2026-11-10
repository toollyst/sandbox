//! `sbx` 진입점. 종료 코드는 [`sbx_cli::behavior`]의 종료 코드 표를 따른다.
//! `--format json`이면 오류도 JSON 한 줄이다([`sbx_cli::output`]).
//!
//! 맨 먼저 셸 자동완성 요청인지 본다: 환경 변수 `COMPLETE`(비어 있지 않고 `0`이 아님)가 있으면 명령을 실행하지 않고
//! 후보(또는 등록 스크립트)를 찍고 끝낸다([`sbx_cli::complete`]). 비동기 런타임도 그 뒤에 만든다(탭마다 빨라야 한다).

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use sbx_cli::cli::{Cli, Format};
use sbx_cli::{app, complete, output};

fn main() -> ExitCode {
    clap_complete::CompleteEnv::with_factory(Cli::command)
        .var(complete::ENV_VAR)
        .complete();
    run(Cli::parse())
}

#[tokio::main]
async fn run(cli: Cli) -> ExitCode {
    let json = cli.format == Format::Json;
    match sbx_cli::run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let code = e.downcast_ref::<app::Failure>().map_or(1, |f| f.code);
            if json {
                output::print_error(None, &e.to_string(), Some(code));
            } else {
                eprintln!("sbx: {e}");
            }
            ExitCode::from(code)
        }
    }
}
