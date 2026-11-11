//! 호스트와 게스트 사이의 메시지.
//!
//! **연결 단위**: 작업 하나에 연결 하나다. `exec` 한 번, 파일 복사 한 번, 종료 요청 한 번이 각각 새 연결을 연다.
//! 그래서 한 연결 안에서 여러 작업을 섞지 않고, 다중화가 없다.
//!
//! **연결 순서**
//!
//! 1. 호스트가 [`HostMsg::Hello`]를 보낸다. 게스트는 버전이 같으면 [`GuestMsg::HelloAck`],
//!    다르면 [`GuestMsg::Error`]를 보내고 연결을 닫는다. 1.0 전까지 호환 규칙은 "같아야 한다"이다.
//! 2. 호스트가 요청 하나를 보낸다: [`HostMsg::Exec`], [`HostMsg::ExecDetached`], [`HostMsg::CopyIn`], [`HostMsg::CopyOut`], [`HostMsg::Stat`], [`HostMsg::Share`], [`HostMsg::EgressListen`], [`HostMsg::EgressSlot`], [`HostMsg::SignalMain`], [`HostMsg::Usage`], [`HostMsg::Shutdown`].
//! 3. 요청별 흐름은 각 메시지 문서에 있다. 실패는 언제든 [`GuestMsg::Error`] 하나로 끝난다.
//!
//! **인코딩**: 프레임([`frame`]) 하나에 메시지 하나. 본문의 첫 바이트가 메시지 종류이고,
//! 필드는 정해진 순서로 이어진다. 정수는 big-endian, 바이트열과 문자열은 길이(u32) 다음에 내용,
//! 목록은 개수(u32) 다음에 항목, 선택 값은 0 또는 1 한 바이트 다음에 값이다. 규칙은 [`codec`]에 있다.
//!
//! **신뢰 경계**: 게스트는 사용자 코드가 도는 곳이므로 공격자 쪽으로 본다.
//! 호스트는 게스트가 보낸 것을 신뢰하지 않는다. 길이가 상한을 넘는 프레임은 본문을 읽기 전에 거부하고,
//! 해석은 남은 바이트보다 많이 읽거나 미리 할당하지 않는다. 끝에 남는 바이트, 모르는 종류, 잘못된 UTF-8은 오류다.
//!
//! - 의존: 표준 라이브러리만 (게스트 바이너리 크기와 호환성 때문)

pub mod codec;
pub mod frame;
mod msg;

pub use frame::{MAX_FRAME_LEN, read_frame, write_frame};
pub use msg::*;

/// 프로토콜 버전. [`HostMsg::Hello`]로 확인한다.
pub const PROTOCOL_VERSION: u32 = 2;

/// 게스트가 듣는 vsock 포트.
pub const GUEST_PORT: u32 = 1024;

/// [`HostMsg::Stdin`], [`GuestMsg::Stdout`], 파일 [`Data`](GuestMsg::Data) 한 조각의 권장 크기.
/// 받는 쪽은 [`MAX_FRAME_LEN`]까지 받는다.
pub const CHUNK_LEN: usize = 64 * 1024;
