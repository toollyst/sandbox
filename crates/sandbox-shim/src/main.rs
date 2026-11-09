//! detached 샌드박스 하나를 붙잡고 있는 감시 프로세스.
//!
//! 호출한 프로세스(CLI, 앱)가 끝나도 샌드박스가 남아야 할 때 쓴다. 데몬 없이 샌드박스마다 하나씩 뜬다.
//! 감시 로직 자체는 `sandbox-runtime`에 있고, 이 바이너리는 그것을 별도 프로세스에서 돌리기만 한다.
//!
//! 하는 일: 실행기 프로세스를 띄우고 붙잡는다, rootfs lease를 쥔다,
//! `data_dir/sandboxes/<id>/control.sock`으로 제어 요청을 받는다.

fn main() {
    eprintln!("sandbox-shim: not implemented yet");
    std::process::exit(1);
}
