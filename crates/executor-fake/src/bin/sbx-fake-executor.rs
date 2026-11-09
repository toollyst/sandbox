//! 테스트용 가짜 실행기 실행 파일. 동작은 `executor_fake::process`에 있다.

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    executor_fake::process::main()
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("sbx-fake-executor needs Unix sockets");
    std::process::ExitCode::from(1)
}
