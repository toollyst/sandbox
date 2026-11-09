//! 게스트 init. VM 안에서 PID 1로 돈다 (Linux 전용, 정적 링크).
//!
//! 하는 일: rootfs(읽기 전용)와 scratch(쓰기 가능)를 overlay로 합친다, 제어 채널로 명령을 받아
//! 프로세스를 실행한다, 파일을 주고받는다, 사용자 이름을 rootfs 기준으로 숫자로 바꾼다.
//!
//! 사용자 코드와 같은 VM에 있으므로 호스트는 이 프로세스가 보내는 것을 신뢰하지 않는다.
//!
//! - 의존: `sandbox-proto`만 (바이너리 크기 때문)

fn main() {
    eprintln!(
        "sandbox-guest: not implemented yet (protocol v{})",
        sandbox_proto::PROTOCOL_VERSION
    );
    std::process::exit(1);
}
