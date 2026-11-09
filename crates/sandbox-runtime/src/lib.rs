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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use sandbox_executor::Executor;
use sandbox_model::{ExecSpec, SandboxId, SandboxSpec, State};
use sandbox_store::Store;
use serde::{Deserialize, Serialize};

mod error;
pub mod process;
pub mod validate;
pub use error::RuntimeError;

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

/// `state.json`에 저장하는 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRecord {
    /// 상태.
    pub state: State,
    /// `stopped`에 딸린 오류 정보.
    pub error: Option<String>,
    /// 실행기 프로세스 ID (실행 중일 때).
    pub pid: Option<u32>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, RuntimeError> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| RuntimeError::Corrupt {
        path: path.to_owned(),
        reason: e.to_string(),
    })
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
        let info = self
            .read_info(id)?
            .ok_or_else(|| RuntimeError::NotFound(id.clone()))?;
        Ok(Sandbox {
            id: info.id,
            state: info.state,
        })
    }

    fn sandboxes_dir(&self) -> PathBuf {
        self.config.data_dir.join("sandboxes")
    }

    /// `spec.json`과 `state.json`이 모두 있어야 샌드박스로 본다. 하나라도 없으면 만드는 중이거나 지우는 중이다.
    fn read_info(&self, id: &SandboxId) -> Result<Option<SandboxInfo>, RuntimeError> {
        let dir = self.sandboxes_dir().join(id.as_str());
        let (spec, state) = (dir.join("spec.json"), dir.join("state.json"));
        if !spec.is_file() || !state.is_file() {
            return Ok(None);
        }
        let spec: SandboxSpec = read_json(&spec)?;
        let state: StateRecord = read_json(&state)?;
        Ok(Some(SandboxInfo {
            id: id.clone(),
            spec,
            state: state.state,
            error: state.error,
        }))
    }

    /// data_dir의 모든 샌드박스.
    pub async fn list(&self) -> Result<Vec<SandboxInfo>, RuntimeError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.sandboxes_dir())? {
            let name = entry?.file_name();
            // 규칙에 맞지 않는 이름은 이 크레이트가 만든 것이 아니다.
            let Some(id) = name.to_str().and_then(|n| SandboxId::new(n).ok()) else {
                continue;
            };
            if let Some(info) = self.read_info(&id)? {
                out.push(info);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
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
    use sandbox_model::{
        Digest, ImageId, Lifetime, NetworkPolicy, ProcessDefaults, Resources, RootfsFormat,
        UserSpec,
    };
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
    fn list_and_get_read_records() {
        let (rt, root) = runtime("list");
        assert!(ready(rt.list()).unwrap().is_empty());

        let dir = root.join("data/sandboxes/abc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("spec.json"), serde_json::to_vec(&spec()).unwrap()).unwrap();
        // state.json이 아직 없으면 목록에 나오지 않는다.
        assert!(ready(rt.list()).unwrap().is_empty());
        let state = StateRecord {
            state: State::Stopped,
            error: Some("boom".into()),
            pid: None,
        };
        std::fs::write(dir.join("state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        std::fs::create_dir_all(root.join("data/sandboxes/Not-Valid")).unwrap();

        let list = ready(rt.list()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].spec, spec());
        assert_eq!(list[0].error.as_deref(), Some("boom"));
        let id = SandboxId::new("abc").unwrap();
        assert_eq!(ready(rt.get(&id)).unwrap().state(), State::Stopped);
        assert!(matches!(
            ready(rt.get(&SandboxId::new("nope").unwrap())),
            Err(RuntimeError::NotFound(_))
        ));

        std::fs::write(dir.join("state.json"), b"{").unwrap();
        assert!(matches!(
            ready(rt.list()),
            Err(RuntimeError::Corrupt { .. })
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
