//! `sbx` 진입점. 종료 코드는 [`sbx_cli::behavior`]의 종료 코드 표를 따른다.
//! `--format json`이면 오류도 JSON 한 줄이다([`sbx_cli::output`]).

use std::process::ExitCode;

use clap::Parser;

use sbx_cli::cli::{Cli, Format};
use sbx_cli::{app, output};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
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
