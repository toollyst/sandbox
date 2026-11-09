//! 게스트 init 진입점. 인자 없이는 PID 1, `agent`면 연결 처리 프로세스다. 규칙은 `boot` 모듈에 있다.

#[cfg(target_os = "linux")]
fn main() {
    match std::env::args().nth(1).as_deref() {
        None => sandbox_guest::boot::pid1(),
        Some("agent") => sandbox_guest::boot::agent(),
        Some(other) => {
            eprintln!("sandbox-guest: unknown mode {other:?}");
            std::process::exit(2);
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("sandbox-guest runs only inside a Linux guest");
    std::process::exit(1);
}
