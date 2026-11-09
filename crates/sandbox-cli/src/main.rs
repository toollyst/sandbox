//! `sbx` 진입점. 종료 코드: 0 성공, 1 실행 오류, 2 사용법 오류(clap), `exec`는 안에서 실행한 프로세스의 코드.

use std::process::ExitCode;

use clap::Parser;

use sandbox_cli::{app, cli::Cli};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match app::run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("sbx: {e}");
            ExitCode::from(1)
        }
    }
}
