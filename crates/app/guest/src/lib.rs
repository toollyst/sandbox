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
//! boot                          PID 1과 연결 처리 프로세스 (Linux)
//! server                        연결 하나: 버전 확인, 요청을 나눠 보냄
//! exec   copy   egress          요청별 처리 (egress: 나가는 연결 프록시)
//! wire   passwd  mkfs  cmdline  메시지 주고받기, 사용자 해석, scratch 포맷, 부팅 인자
//! sys                           시스템 호출 (unsafe는 여기만)
//! ```
//!
//! `boot` 아래는 일반 스트림과 파일 위에서 돌므로 호스트(Unix)에서 그대로 테스트한다.
//!
//! - 의존: `sbx-guest-proto`, `libc`(시스템 호출 선언)
#![cfg(unix)]

#[cfg(target_os = "linux")]
pub mod boot;
pub mod cmdline;
pub mod copy;
pub mod dns;
pub mod egress;
pub mod exec;
pub mod mkfs;
pub mod passwd;
pub mod server;
mod sys;
pub mod usage;
pub mod wire;

#[cfg(test)]
mod tests;
