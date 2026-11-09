//! 실행 코어: 샌드박스 생명주기.
//!
//! 샌드박스를 만들고, 시작하고, 그 안에서 명령을 실행하고, 파일을 주고받고, 멈추고, 지운다.
//! 샌드박스 1개는 VM 1개, 이미지 1개다.
//!
//! **알지 않는 것**
//!
//! - 이미지를 받거나, rootfs를 만들거나, 캐시를 지우는 일. 그것은 관리(`sandbox-manage`)의 일이다.
//!   이 크레이트는 `sandbox-store`를 `write` feature 없이 쓰므로 캐시를 바꿀 수 없다.
//! - 특정 실행기. `sandbox-executor` trait만 안다.
//! - CLI 같은 표현 계층.
//!
//! **명시적 동작**: [`Runtime::create`]는 이미지나 rootfs가 없으면 받거나 만들지 않고 바로 오류를 낸다.
//! Docker처럼 "없으면 받아서 실행"하는 동작은 표현 계층이 관리 쪽을 먼저 부르는 방식으로 만든다.
//! 요청한 기능을 실행기가 지원하지 않으면 다른 방식으로 바꾸지 않고 오류를 낸다([`validate`]).
//!
//! **수명**: attached와 detached의 감시 로직은 이 크레이트 안에 하나만 둔다.
//! detached는 `sandbox-shim` 바이너리가 같은 로직을 별도 프로세스에서 돌린다.
//!
//! data_dir은 이 크레이트만 쓴다.
//!
//! ```text
//! <data_dir>/
//!   sandboxes/<id>/spec.json     SandboxSpec (생성 시 고정)
//!   sandboxes/<id>/state.json    상태, 실행기 프로세스 pid, 수명 모드
//!   sandboxes/<id>/scratch.img   샌드박스별 쓰기 가능 디스크
//!   sandboxes/<id>/control.sock  detached일 때
//!   sandboxes/<id>/logs/
//! ```
//!
//! - 의존: `sandbox-model`, `sandbox-proto`, `sandbox-executor`, `sandbox-store`(읽기)

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sandbox_executor::{Executor, ExecutorError};
use sandbox_model::{ExecSpec, ImageId, ParseError, RootfsFormat, SandboxId, SandboxSpec, State};
use sandbox_store::Store;

pub mod process;
pub mod validate;

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
            RuntimeError::NotFound(id) => write!(f, "no such sandbox: {id}"),
            RuntimeError::InvalidState { current, action } => {
                write!(f, "cannot {action} a sandbox in state {}", current.as_str())
            }
            RuntimeError::Executor(e) => e.fmt(f),
            RuntimeError::Io(e) => write!(f, "io: {e}"),
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

/// 실행 코어 설정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// 샌드박스 상태. 같은 경로를 쓰는 프로세스끼리 샌드박스 목록이 공유된다.
    pub data_dir: PathBuf,
    /// 이미지 캐시. 읽기만 한다.
    pub cache_dir: PathBuf,
}

/// 실행 코어.
pub struct Runtime {
    config: RuntimeConfig,
    store: Store,
    executor: Arc<dyn Executor>,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("config", &self.config)
            .field("executor", &self.executor.capabilities().name)
            .finish()
    }
}

/// 샌드박스 목록의 한 줄.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxInfo {
    /// ID.
    pub id: SandboxId,
    /// 생성 시 고정된 설정.
    pub spec: SandboxSpec,
    /// 상태.
    pub state: State,
    /// `stopped`에 딸린 오류 정보.
    pub error: Option<String>,
}

impl Runtime {
    /// 만든다. data_dir이 없으면 만든다. cache_dir은 만들지 않는다(관리 쪽의 일).
    pub fn new(config: RuntimeConfig, executor: Arc<dyn Executor>) -> Result<Self, RuntimeError> {
        std::fs::create_dir_all(config.data_dir.join("sandboxes"))?;
        let store = Store::open(&config.cache_dir);
        Ok(Self {
            config,
            store,
            executor,
        })
    }

    /// 설정.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// 샌드박스를 만든다.
    ///
    /// 검사 순서: 실행기 기능([`validate::check`]) → 이미지 존재 → rootfs 존재.
    /// 어느 것이든 없으면 받거나 만들지 않고 오류를 낸다.
    pub async fn create(&self, spec: SandboxSpec) -> Result<Sandbox, RuntimeError> {
        validate::check(&spec, &self.executor.capabilities())?;
        if !self.store.has_image(&spec.image) {
            return Err(RuntimeError::ImageNotPulled(spec.image));
        }
        if !self.store.has_rootfs(&spec.image, spec.rootfs_format) {
            return Err(RuntimeError::RootfsNotPrepared {
                image: spec.image,
                format: spec.rootfs_format,
            });
        }
        Err(RuntimeError::Unimplemented("sandbox state persistence"))
    }

    /// ID로 샌드박스를 찾는다.
    pub async fn get(&self, id: &SandboxId) -> Result<Sandbox, RuntimeError> {
        Err(RuntimeError::NotFound(id.clone()))
    }

    /// data_dir의 모든 샌드박스.
    pub async fn list(&self) -> Result<Vec<SandboxInfo>, RuntimeError> {
        Err(RuntimeError::Unimplemented("sandbox listing"))
    }
}

/// 샌드박스 하나.
#[derive(Debug)]
pub struct Sandbox {
    id: SandboxId,
    state: State,
}

impl Sandbox {
    /// ID.
    pub fn id(&self) -> &SandboxId {
        &self.id
    }

    /// 상태.
    pub fn state(&self) -> State {
        self.state
    }

    /// 시작한다. `created`나 `stopped`에서만 할 수 있다.
    pub async fn start(&self) -> Result<(), RuntimeError> {
        self.require(State::Running, "start")?;
        Err(RuntimeError::Unimplemented("start"))
    }

    /// 안에서 프로세스를 하나 더 실행한다. `running`에서만 할 수 있다.
    pub async fn exec(&self, _spec: ExecSpec) -> Result<Process, RuntimeError> {
        if self.state != State::Running {
            return Err(RuntimeError::InvalidState {
                current: self.state,
                action: "exec",
            });
        }
        Err(RuntimeError::Unimplemented("exec"))
    }

    /// 호스트 파일이나 디렉터리를 게스트로 복사한다.
    pub async fn copy_in(&self, _host: &Path, _guest: &str) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unimplemented("copy_in"))
    }

    /// 게스트 파일이나 디렉터리를 호스트로 복사한다.
    pub async fn copy_out(&self, _guest: &str, _host: &Path) -> Result<(), RuntimeError> {
        Err(RuntimeError::Unimplemented("copy_out"))
    }

    /// 게스트에 종료를 요청하고 `timeout`까지 기다린다. 넘으면 강제로 끝낸다.
    pub async fn stop(&self, _timeout: Duration) -> Result<(), RuntimeError> {
        self.require(State::Stopped, "stop")?;
        Err(RuntimeError::Unimplemented("stop"))
    }

    /// 지운다. 실행 중이면 오류다(강제 삭제는 표현 계층이 `stop` 후 `delete`로 만든다).
    pub async fn delete(self) -> Result<(), RuntimeError> {
        if self.state == State::Running {
            return Err(RuntimeError::InvalidState {
                current: self.state,
                action: "delete",
            });
        }
        Err(RuntimeError::Unimplemented("delete"))
    }

    fn require(&self, next: State, action: &'static str) -> Result<(), RuntimeError> {
        if self.state.can_transition_to(next) {
            Ok(())
        } else {
            Err(RuntimeError::InvalidState {
                current: self.state,
                action,
            })
        }
    }
}

/// `exec`로 실행한 프로세스.
#[derive(Debug)]
pub struct Process {
    _private: (),
}

impl Process {
    /// 끝날 때까지 기다려 종료 코드를 돌려준다.
    pub async fn wait(self) -> Result<i32, RuntimeError> {
        Err(RuntimeError::Unimplemented("process wait"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use executor_fake::FakeExecutor;
    use sandbox_model::{Digest, Lifetime, NetworkPolicy, ProcessDefaults, Resources, UserSpec};
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// 바로 끝나는 future만 돌린다. 이 테스트들은 기다리는 일이 없다.
    fn ready<F: Future>(f: F) -> F::Output {
        let mut f = pin!(f);
        match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("future did not complete immediately"),
        }
    }

    fn spec() -> SandboxSpec {
        SandboxSpec {
            image: ImageId::new(Digest::parse(&format!("sha256:{}", "ef".repeat(32))).unwrap()),
            rootfs_format: RootfsFormat::Erofs,
            process: ProcessDefaults {
                args: vec!["sh".into()],
                env: vec![],
                cwd: "/".into(),
                user: UserSpec::root(),
            },
            resources: Resources {
                vcpus: 1,
                memory_mib: 256,
                scratch_disk_mib: 64,
            },
            mounts: vec![],
            network: NetworkPolicy::Disabled,
            lifetime: Lifetime::Attached,
            hostname: None,
            labels: BTreeMap::new(),
        }
    }

    fn runtime(name: &str) -> (Runtime, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("sandbox-runtime-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let rt = Runtime::new(
            RuntimeConfig {
                data_dir: root.join("data"),
                cache_dir: root.join("cache"),
            },
            Arc::new(FakeExecutor::basic()),
        )
        .unwrap();
        (rt, root)
    }

    #[test]
    fn create_does_not_pull_missing_image() {
        let (rt, root) = runtime("nopull");
        assert!(matches!(
            ready(rt.create(spec())),
            Err(RuntimeError::ImageNotPulled(_))
        ));
        // 캐시를 만들지도 않는다.
        assert!(!root.join("cache").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_does_not_prepare_missing_rootfs() {
        let (rt, root) = runtime("noprepare");
        let s = spec();
        let record = rt.store.layout().image_record(&s.image);
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        std::fs::write(&record, b"{}").unwrap();
        assert!(matches!(
            ready(rt.create(s)),
            Err(RuntimeError::RootfsNotPrepared { .. })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sandbox_state_guards() {
        let created = Sandbox {
            id: SandboxId::new("a").unwrap(),
            state: State::Created,
        };
        let exec = ExecSpec {
            args: vec!["ls".into()],
            env: vec![],
            cwd: None,
            user: None,
            tty: false,
        };
        assert!(matches!(
            ready(created.exec(exec)),
            Err(RuntimeError::InvalidState { .. })
        ));
        let running = Sandbox {
            id: SandboxId::new("b").unwrap(),
            state: State::Running,
        };
        assert!(matches!(
            ready(running.start()),
            Err(RuntimeError::InvalidState { .. })
        ));
        assert!(matches!(
            ready(running.delete()),
            Err(RuntimeError::InvalidState { .. })
        ));
    }
}
