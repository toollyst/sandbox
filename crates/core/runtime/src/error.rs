//! 실행 코어 오류. 크레이트의 가장 아래 모듈이다. 다른 모듈은 여기에 의존하고, 여기는 아무 모듈에도 의존하지 않는다.

use std::fmt;
use std::io;
use std::path::PathBuf;

use sbx_executor::ExecutorError;
use sbx_model::{ImageId, ParseError, RootfsFormat, SandboxId, State};

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
    /// 이미지가 자기 커널을 담고 있는데 꺼내 두지 않음 (`prepare`를 다시 해야 한다).
    KernelNotPrepared(ImageId),
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
        /// 호스트 경로.
        source: std::path::PathBuf,
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
    /// 그 앞부분으로 시작하는 샌드박스가 없음 ([`Runtime::resolve`](crate::Runtime::resolve)).
    NoMatch(String),
    /// 그 앞부분으로 시작하는 샌드박스가 여럿.
    Ambiguous {
        /// 받은 앞부분.
        prefix: String,
        /// 맞는 개수.
        matches: usize,
    },
    /// 이름이 잘못됨([`sbx_model::check_sandbox_name`]).
    InvalidName(ParseError),
    /// 그 이름을 다른 샌드박스가 쓰고 있음.
    NameInUse {
        /// 이름.
        name: String,
        /// 쓰고 있는 샌드박스.
        id: SandboxId,
    },
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
    Guest(sbx_guest_proto::Error),
    /// 실행할 명령을 시작하지 못함 (실행 파일이 없음, 실행 권한이 없음 등). 종류는 오류의 `kind`다.
    CommandFailed(sbx_guest_proto::Error),
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
    /// 그 이름의 볼륨이 없음.
    VolumeNotFound(String),
    /// 그 이름의 볼륨이 이미 있음.
    VolumeExists(String),
    /// 볼륨을 다른 샌드박스가 붙이고 있음(쓰기는 하나만, 읽기 전용은 쓰는 쪽이 없을 때만).
    VolumeBusy {
        /// 볼륨 이름.
        name: String,
        /// 붙이고 있는 샌드박스(이름이나 짧은 ID, 알면).
        holder: Option<String>,
    },
    /// 소유 프로세스는 죽었지만 그 VM이 아직 꺼지지 않아 볼륨을 붙이고 있음.
    VolumeStillAttached {
        /// 볼륨 이름.
        name: String,
        /// 그 VM의 샌드박스(이름이나 짧은 ID).
        holder: String,
    },
    /// 샌드박스 기록이 가리키는 볼륨은 지울 수 없음.
    VolumeReferenced {
        /// 볼륨 이름.
        name: String,
        /// 그 볼륨을 쓰는 샌드박스(이름이나 짧은 ID).
        sandboxes: Vec<String>,
    },
    /// 실행기가 볼륨 디스크를 붙이지 못함(규약 3보다 옛 실행기).
    VolumesUnsupported {
        /// 실행기 이름.
        executor: &'static str,
    },
    /// 볼륨이 실행기의 디스크 수를 넘음.
    TooManyVolumes {
        /// 요청한 볼륨 수.
        volumes: usize,
        /// 실행기 이름.
        executor: &'static str,
        /// 실행기가 붙일 수 있는 디스크 수.
        max_disks: usize,
    },
    /// 입출력 오류.
    Io(io::Error),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::ImageNotPulled(id) => write!(f, "image {id} is not pulled"),
            RuntimeError::RootfsNotPrepared { image, format } => {
                write!(f, "image {image} has no {format} rootfs prepared")
            }
            RuntimeError::KernelNotPrepared(image) => {
                write!(
                    f,
                    "image {image} carries its own kernel, but it is not prepared"
                )
            }
            RuntimeError::FormatUnsupported { executor, format } => {
                write!(f, "executor {executor} does not support {format} rootfs")
            }
            // 다른 길 둘을 알린다: 시작할 때 복사해 넣기(그대로 붙여 쓸 수 있게), 실시간 공유가 있는 실행기.
            RuntimeError::BindUnsupported {
                executor,
                source,
                destination,
            } => write!(
                f,
                "executor {executor} cannot share host folders live (-v {src}:{destination} needs \
                 virtio-fs); copy the folder in at start instead with \
                 --mount type=copy,source={src},target={destination} (take results out with \
                 `sbx cp`), or use an executor that announces live sharing (`sbx info` shows \
                 \"live share\")",
                src = source.display()
            ),
            RuntimeError::NoCommand => f.write_str("no command specified"),
            RuntimeError::InvalidUser(e) => e.fmt(f),
            RuntimeError::InvalidStopSignal(e) => write!(f, "image StopSignal: {e}"),
            RuntimeError::NotFound(id) => write!(f, "no such sandbox: {id}"),
            RuntimeError::NoMatch(p) => write!(f, "no such sandbox: {p}"),
            RuntimeError::Ambiguous { prefix, matches } => write!(
                f,
                "{matches} sandboxes start with {prefix:?}; give more of the ID"
            ),
            RuntimeError::InvalidName(e) => e.fmt(f),
            RuntimeError::NameInUse { name, id } => write!(
                f,
                "the name {name:?} is already in use by sandbox {}; remove it or choose another name",
                &id.as_str()[..12.min(id.as_str().len())]
            ),
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
            RuntimeError::VolumeNotFound(n) => write!(f, "no such volume: {n}"),
            RuntimeError::VolumeExists(n) => write!(f, "volume {n} already exists"),
            RuntimeError::VolumeBusy { name, holder } => match holder {
                Some(h) => write!(
                    f,
                    "volume {name} is in use by sandbox {h}: a volume attached read-write is used by one \
                     running sandbox at a time (read-only attaches can share it when nobody writes); stop {h} first"
                ),
                None => write!(
                    f,
                    "volume {name} is in use by another sandbox that is starting or running; stop it first"
                ),
            },
            RuntimeError::VolumeStillAttached { name, holder } => write!(
                f,
                "volume {name} is still attached to sandbox {holder}: its owner exited but its VM is still shutting \
                 down; try again in a moment"
            ),
            RuntimeError::VolumeReferenced { name, sandboxes } => write!(
                f,
                "volume {name} is in use by sandbox{} {}; remove {} first",
                if sandboxes.len() == 1 { "" } else { "es" },
                sandboxes.join(", "),
                if sandboxes.len() == 1 { "it" } else { "them" }
            ),
            RuntimeError::VolumesUnsupported { executor } => write!(
                f,
                "executor {executor} cannot attach volumes: it speaks an executor protocol older than 3; \
                 install a newer version of it"
            ),
            RuntimeError::TooManyVolumes {
                volumes,
                executor,
                max_disks,
            } => write!(
                f,
                "{volumes} volumes need {} disks (one per volume, the writable disk and at least one image \
                 layer), but executor {executor} attaches at most {max_disks}, so at most {} volumes",
                volumes + 2,
                max_disks.saturating_sub(2)
            ),
            RuntimeError::OwnedElsewhere(id) => write!(
                f,
                "sandbox {id} was started by another process; only that process can do this"
            ),
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
