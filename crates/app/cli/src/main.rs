//! `sbx` 진입점. 종료 코드는 [`sbx_cli::behavior`]의 종료 코드 표를 따른다.

use std::process::ExitCode;

use clap::Parser;

use sbx_cli::{app, cli::Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match sbx_cli::run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("sbx: {e}");
            match e.downcast_ref::<app::Failure>() {
                Some(f) => ExitCode::from(f.code),
                None => ExitCode::from(1),
            }
        }
    }
}
