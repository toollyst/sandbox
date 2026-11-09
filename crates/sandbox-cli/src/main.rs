//! `sbx` 진입점.

use std::process::ExitCode;

use sandbox_cli::commands;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            print!("{}", commands::help_text());
            ExitCode::SUCCESS
        }
        Some(name) => match commands::find(name) {
            Some(cmd) => {
                eprintln!("sbx {}: 아직 구현되지 않았습니다", cmd.name);
                ExitCode::from(2)
            }
            None => {
                eprintln!("sbx: 알 수 없는 명령 {name:?}. `sbx help`를 보세요");
                ExitCode::from(2)
            }
        },
    }
}
