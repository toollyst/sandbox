//! 게스트 init. VM 안에서 PID 1로 돈다 (Linux 전용, 정적 링크).
//!
//! 하는 일: rootfs(읽기 전용)와 scratch(쓰기 가능)를 overlay로 합친다, 제어 채널로 명령을 받아
//! 프로세스를 실행한다, 파일을 주고받는다, 사용자 이름을 rootfs 기준으로 숫자로 바꾼다.
//!
//! 사용자 코드와 같은 VM에 있으므로 호스트는 이 프로세스가 보내는 것을 신뢰하지 않는다.
//!
//! **층** (위가 아래를 쓴다. 아래는 위를 모른다. `cargo xtask check-deps`가 순환을 막는다.)
//!
//! ```text
//! server            연결 하나: 버전 확인, 요청을 나눠 보냄
//! exec   copy       요청별 처리
//! wire   passwd     메시지 주고받기, 사용자 해석
//! sys               시스템 호출 (unsafe는 여기만)
//! ```
//!
//! 연결 처리는 일반 스트림 위에서 돌므로 호스트(Unix)에서 그대로 테스트한다.
//!
//! - 의존: `sandbox-proto`, `libc`(시스템 호출 선언)
#![cfg(unix)]

pub mod copy;
pub mod exec;
pub mod passwd;
pub mod server;
mod sys;
pub mod wire;

#[cfg(test)]
mod tests;
