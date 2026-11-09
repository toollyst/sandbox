//! 실행 코어 오류. 크레이트의 가장 아래 모듈이다. 다른 모듈은 여기에 의존하고, 여기는 아무 모듈에도 의존하지 않는다.

use std::fmt;
use std::io;
use std::path::PathBuf;

use sandbox_executor::ExecutorError;
use sandbox_model::{ImageId, ParseError, RootfsFormat, SandboxId, State};

/// 실행 코어 오류.
#[derive(Debug)]
pub enum RuntimeError {
    /// 이미지를 받아 두지 않음.
    ImageNotPulled(ImageId),
    /// 요청한 형식의 rootfs를 준비하지 않음.
    RootfsNotPrepared {
        /// 이미지.
        image: ImageId,
        /// 형식.
        format: RootfsFormat,
    },
    /// 실행기가 그 rootfs 형식을 받지 않음.
    FormatUnsupported {
        /// 실행기 이름.
        executor: &'static str,
        /// 형식.
        format: RootfsFormat,
    },
    /// 실행기가 실시간 공유를 지원하지 않는데 `Bind` 마운트를 요청함.
    BindUnsupported {
        /// 실행기 이름.
        executor: &'static str,
        /// 게스트 경로.
        destination: String,
    },
    /// 실행할 명령이 없음 (이미지에도 호출자 값에도 없음).
    NoCommand,
    /// 사용자 값이 잘못됨.
    InvalidUser(ParseError),
    /// 이미지의 `StopSignal`이 잘못됨.
    InvalidStopSignal(ParseError),
    /// 그 ID의 샌드박스가 없음.
    NotFound(SandboxId),
    /// 지금 상태에서는 할 수 없는 동작.
    InvalidState {
        /// 지금 상태.
        current: State,
        /// 요청한 동작.
        action: &'static str,
    },
    /// 실행기 오류.
    Executor(ExecutorError),
    /// 기록 파일이 깨짐.
    Corrupt {
        /// 파일.
        path: PathBuf,
        /// 이유.
        reason: String,
    },
    /// 게스트가 오류를 알림 (예: 실행 파일이 없음, 경로가 없음).
    Guest(sandbox_proto::Error),
    /// 실행할 명령을 시작하지 못함 (실행 파일이 없음, 실행 권한이 없음 등). 종류는 오류의 `kind`다.
    CommandFailed(sandbox_proto::Error),
    /// 게스트가 규약에 맞지 않는 것을 보냄.
    Protocol(String),
    /// 게스트에 연결할 수 없음.
    GuestUnreachable(String),
    /// 게스트 연결이 중간에 끊김 (VM이 끝났거나 멈춤).
    Lost(String),
    /// 요청 값이 잘못됨.
    Invalid(String),
    /// 게스트 init 파일이 없음.
    GuestInitMissing(PathBuf),
    /// 제어 소켓 경로가 Unix 소켓 길이 제한을 넘음.
    SocketPathTooLong(PathBuf),
    /// 다른 프로세스가 이 샌드박스의 VM을 소유하고 있어 할 수 없음.
    OwnedElsewhere(SandboxId),
    /// 입출력 오류.
    Io(io::Error),
    /// 아직 구현하지 않은 기능.
    Unimplemented(&'static str),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::ImageNotPulled(id) => write!(f, "image {id} is not pulled"),
            RuntimeError::RootfsNotPrepared { image, format } => {
                write!(f, "image {image} has no {format} rootfs prepared")
            }
            RuntimeError::FormatUnsupported { executor, format } => {
                write!(f, "executor {executor} does not support {format} rootfs")
            }
            RuntimeError::BindUnsupported {
                executor,
                destination,
            } => write!(
                f,
                "executor {executor} does not support live sharing (bind mount to {destination})"
            ),
            RuntimeError::NoCommand => f.write_str("no command specified"),
            RuntimeError::InvalidUser(e) => e.fmt(f),
            RuntimeError::InvalidStopSignal(e) => write!(f, "image StopSignal: {e}"),
            RuntimeError::NotFound(id) => write!(f, "no such sandbox: {id}"),
            RuntimeError::InvalidState { current, action } => {
                write!(f, "cannot {action} a sandbox in state {}", current.as_str())
            }
            RuntimeError::Executor(e) => e.fmt(f),
            RuntimeError::Corrupt { path, reason } => {
                write!(f, "corrupt record {}: {reason}", path.display())
            }
            RuntimeError::Io(e) => write!(f, "io: {e}"),
            RuntimeError::Guest(e) => write!(f, "{}", e.message),
            RuntimeError::CommandFailed(e) => write!(f, "cannot start the command: {}", e.message),
            RuntimeError::Protocol(m) => write!(f, "guest protocol violation: {m}"),
            RuntimeError::GuestUnreachable(m) => write!(f, "cannot reach the guest: {m}"),
            RuntimeError::Lost(m) => write!(f, "lost the guest connection: {m}"),
            RuntimeError::Invalid(m) => f.write_str(m),
            RuntimeError::GuestInitMissing(p) => {
                write!(f, "guest init {} not found", p.display())
            }
            RuntimeError::SocketPathTooLong(p) => write!(
                f,
                "control socket path is too long for a Unix socket: {} (use a shorter socket_dir)",
                p.display()
            ),
            RuntimeError::OwnedElsewhere(id) => write!(
                f,
                "sandbox {id} was started by another process; only that process can do this"
            ),
            RuntimeError::Unimplemented(what) => write!(f, "not implemented yet: {what}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<io::Error> for RuntimeError {
    fn from(e: io::Error) -> Self {
        RuntimeError::Io(e)
    }
}

impl From<ExecutorError> for RuntimeError {
    fn from(e: ExecutorError) -> Self {
        RuntimeError::Executor(e)
    }
}
