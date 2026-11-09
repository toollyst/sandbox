//! Docker형 CLI `sbx`.
//!
//! CLI는 코어를 표현하는 방식 중 하나일 뿐이다. 도메인 로직을 두지 않고, 다음만 한다.
//!
//! - 인자 해석과 출력 형식 ([`cli`], [`parse`])
//! - 구현체 조립과 코어 호출 ([`app`])
//! - 관리 코어(`sandbox-manage`)와 실행 코어(`sandbox-runtime`)를 차례로 불러 Docker와 같은 동작을 만든다
//!
//! 동작은 Docker CLI를 기준으로 한다. 사람들이 이미 익숙하기 때문이다.
//! 명령 목록은 [`commands`], 동작 규칙은 [`behavior`], Docker와 다른 점은
//! [`behavior::DOCKER_DIFFERENCES`]에 있다. 기본 디렉터리는 [`dirs`]에 있다.

pub mod app;
pub mod behavior;
pub mod cli;
pub mod commands;
pub mod dirs;
pub mod parse;
