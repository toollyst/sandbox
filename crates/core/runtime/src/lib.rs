//! 실행 코어: 샌드박스 생명주기.
//!
//! 샌드박스를 만들고, 시작하고, 그 안에서 명령을 실행하고, 파일을 주고받고, 멈추고, 지운다.
//! 샌드박스 1개는 VM 1개, 이미지 1개다.
//!
//! **알지 않는 것**
//!
//! - 이미지를 받거나, rootfs를 만들거나, 캐시를 지우는 일. 그것은 관리(`sbx-manage`)의 일이다.
//!   이 크레이트는 `sbx-store`를 `write` feature 없이 쓰므로 캐시를 바꿀 수 없다.
//! - 특정 실행기. `sbx-executor` trait만 안다.
//! - CLI 같은 표현 계층.
//!
//! **명시적 동작**
//!
//! - [`Runtime::create`]는 이미지나 rootfs가 없으면 받거나 만들지 않고 바로 오류를 낸다.
//!   Docker처럼 "없으면 받아서 실행"하는 동작은 표현 계층이 관리 쪽을 먼저 부르는 방식으로 만든다.
//! - 요청한 기능을 실행기가 지원하지 않으면 다른 방식으로 바꾸지 않고 오류를 낸다([`validate`]).
//! - **기본 프로세스** (Docker와 같다): [`Sandbox::start`]는 VM을 띄우고 `spec.process`를 실행한다.
//!   기본 프로세스가 끝나면 샌드박스도 멈추고 그 종료 코드를 남긴다. 실행 중인 샌드박스에는 [`Sandbox::exec`]로 더 실행한다.
//! - 종료 코드: 종료하면 그 코드, 시그널로 끝나면 128 + 시그널 번호다(Docker와 같다).
//!   VM이 먼저 끝나면(정지, 강제 종료) 기본 프로세스는 137로 끝난 것으로 본다.
//! - [`Sandbox::stop`]은 게스트에 종료를 요청하고(sync 뒤 전원 끄기), `timeout`까지 VM이 끝나지 않으면 강제로 끝낸다.
//!   Docker와 달리 기본 프로세스에 SIGTERM을 먼저 보내지 않는다.
//! - `bind` 마운트: 실행기에 공유 폴더로 넘기고(태그 `sbx0`, `sbx1`, … 마운트 순서대로), 게스트가 준비되면 그 경로에 마운트하게 한다.
//!   실행기가 실시간 공유를 지원하지 않으면 만들 때 오류다([`validate`]).
//! - `copy` 마운트는 시작할 때 기본 프로세스보다 먼저 한 번 복사한다. 대상이 디렉터리로 있으면 그 안에 내용을, 없으면 그 경로로 만든다.
//!   순서: bind를 모두 마운트한 뒤 copy를 마운트 순서대로.
//! - 파일 복사([`Sandbox::copy_in`], [`Sandbox::copy_out`])의 대상 규칙은 Docker `cp`와 같다([`cpplan`]).
//!   Docker와 달리 실행 중인 샌드박스에서만 된다(디스크를 읽는 것이 게스트이기 때문이다).
//!
//! **수명 (attached)**: VM을 띄운 프로세스가 그 VM을 소유한다(`owner.lock`). 소유 프로세스가 끝나면 실행기 규약에 따라
//! VM도 끝나고, 다른 프로세스는 그 샌드박스를 `stopped`로 본다. 다른 프로세스도 같은 data_dir로 exec, cp, stop을 할 수 있다.
//! 수명은 시작할 때 정한다. 시작한 프로세스와 상관없이 남기려면(detached) `sbx-shim` 프로세스가 대신 시작해 소유한다.
//!
//! 모든 블로킹 입출력은 별도 스레드에서 한다([`sbx_model::blocking`]). 특정 비동기 런타임에 의존하지 않는다.
//!
//! **층**: `lib` → `egress` → `client` → `hostfs`, `records` → `process`, `validate`, `cpplan`, `chan` → `error`.
//!
//! - 의존: `sbx-model`, `sbx-guest-proto`, `sbx-executor`, `sbx-store`(읽기)

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sbx_executor::{ChannelSpec, Disk, DiskRole, Executor, GuestConn, VmConfig, VmHandle};
use sbx_guest_proto::{ExitStatus, GuestMsg, HostMsg, WinSize};
use sbx_model::{
    ExecSpec, Ident, MountKind, NetworkPolicy, ProcessDefaults, SandboxId, SandboxSpec, Signal,
    State, UserSpec, blocking,
};
use sbx_store::{LeaseError, RootfsLease, Store};

mod chan;
mod client;
pub mod cpplan;
pub mod egress;
mod error;
mod hostfs;
pub mod process;
mod records;
pub mod validate;

pub use error::RuntimeError;
pub use records::{OWNER_GONE, StateRecord};

use client::Guest;
use records::{OwnerLock, Paths};

/// 게스트가 연결을 받을 때까지 기다리는 시간.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// 시작할 때 소유 잠금을 얻으려고 기다리는 시간 (다른 프로세스가 잠깐 쥐는 경우).
pub const OWN_WAIT: Duration = Duration::from_secs(1);
/// 기본 프로세스가 끝난 뒤 VM이 스스로 꺼지기를 기다리는 시간. 넘으면 강제로 끝낸다.
pub const VM_EXIT_GRACE: Duration = Duration::from_secs(10);
/// VM이 먼저 끝났을 때 기본 프로세스의 종료 코드 (SIGKILL로 끝난 것과 같다).
pub const EXIT_VM_ENDED: i32 = 137;
/// Unix 소켓 경로의 최대 바이트 수 (macOS 기준, 가장 짧다).
pub const MAX_SOCKET_PATH: usize = 103;

/// 실행 코어 설정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// 샌드박스 상태. 같은 경로를 쓰는 프로세스끼리 샌드박스 목록이 공유된다.
    pub data_dir: PathBuf,
    /// 이미지 캐시. 읽기만 한다.
    pub cache_dir: PathBuf,
    /// 게스트 init initramfs. 실행기에 그대로 넘긴다.
    pub guest_init: PathBuf,
    /// 게스트 연결 소켓을 두는 디렉터리. Unix 소켓 경로 제한([`MAX_SOCKET_PATH`]) 때문에 짧은 경로여야 한다.
    /// 소켓 이름은 `<ID>.sock`이다. 없으면 만든다(Unix에서는 권한 0700).
    pub socket_dir: PathBuf,
}

struct Owned {
    vm: Box<dyn VmHandle>,
    control: PathBuf,
    _lease: RootfsLease,
    _lock: OwnerLock,
}

struct Inner {
    config: RuntimeConfig,
    store: Store,
    executor: Arc<dyn Executor>,
    owned: Mutex<HashMap<SandboxId, Owned>>,
}

/// 실행 코어. 복제하면 같은 것을 가리킨다.
#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

impl fmt::Debug for Runtime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Runtime")
            .field("config", &self.inner.config)
            .field("executor", &self.inner.executor.capabilities().name)
            .finish()
    }
}

/// 샌드박스 하나의 기록.
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
    /// 기본 프로세스의 종료 코드 (끝났을 때).
    pub exit_code: Option<i32>,
    /// 게스트 연결 소켓 (실행 중일 때).
    pub control: Option<PathBuf>,
    /// 게스트 안 나가는 연결 프록시의 포트 (실행 중이고 네트워크를 허용했을 때).
    pub egress_port: Option<u16>,
    /// 만든 시각. 만든 뒤 바뀌지 않는 설정 파일의 수정 시각이다. 파일 시스템이 주지 않으면 `None`.
    pub created: Option<std::time::SystemTime>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Runtime {
    /// 만든다. data_dir이 없으면 만든다. cache_dir은 만들지 않는다(관리 쪽의 일).
    pub fn new(config: RuntimeConfig, executor: Arc<dyn Executor>) -> Result<Self, RuntimeError> {
        std::fs::create_dir_all(config.data_dir.join("sandboxes"))?;
        let store = Store::open(&config.cache_dir);
        Ok(Self {
            inner: Arc::new(Inner {
                config,
                store,
                executor,
                owned: Mutex::default(),
            }),
        })
    }

    /// 설정.
    pub fn config(&self) -> &RuntimeConfig {
        &self.inner.config
    }

    fn paths(&self, id: &SandboxId) -> Paths {
        Paths::new(&self.inner.config.data_dir, id)
    }

    fn guest(&self, control: &Path) -> Guest {
        Guest::new(
            self.inner.executor.clone(),
            ChannelSpec {
                host_endpoint: control.to_owned(),
                guest_port: sbx_guest_proto::GUEST_PORT,
            },
        )
    }

    fn socket_path(&self, id: &SandboxId) -> Result<PathBuf, RuntimeError> {
        let dir = &self.inner.config.socket_dir;
        let path = dir.join(format!("{id}.sock"));
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(RuntimeError::SocketPathTooLong(path));
        }
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(path)
    }

    /// 샌드박스를 만든다. 아직 시작하지 않는다.
    ///
    /// 검사 순서: 실행기 기능([`validate::check`]) → 이미지 존재 → rootfs 존재.
    /// 어느 것이든 없으면 받거나 만들지 않고 오류를 낸다.
    pub async fn create(&self, spec: SandboxSpec) -> Result<Sandbox, RuntimeError> {
        validate::check(&spec, &self.inner.executor.capabilities())?;
        let data_dir = &self.inner.config.data_dir;
        if let Some(name) = &spec.name {
            sbx_model::check_sandbox_name(name).map_err(RuntimeError::InvalidName)?;
            if let Some(id) = records::name_owner(data_dir, name)? {
                return Err(RuntimeError::NameInUse {
                    name: name.clone(),
                    id,
                });
            }
        }
        let store = &self.inner.store;
        if !store.has_image(&spec.image) {
            return Err(RuntimeError::ImageNotPulled(spec.image));
        }
        if !store.has_rootfs(&spec.image, spec.rootfs_format) {
            return Err(RuntimeError::RootfsNotPrepared {
                image: spec.image,
                format: spec.rootfs_format,
            });
        }
        let id = records::new_id();
        records::create(data_dir, &id, &spec, spec.resources.scratch_disk_mib)?;
        // 기록을 만든 뒤 이름을 잡는다. 그사이 다른 쪽이 잡았으면 만든 것을 지운다.
        if let Some(name) = &spec.name
            && let Err(e) = records::claim_name(data_dir, name, &id)
        {
            let _ = records::remove(&self.paths(&id));
            return Err(e);
        }
        self.get(&id).await
    }

    /// ID로 샌드박스를 찾는다.
    pub async fn get(&self, id: &SandboxId) -> Result<Sandbox, RuntimeError> {
        let info = self
            .read_info(id)?
            .ok_or_else(|| RuntimeError::NotFound(id.clone()))?;
        Ok(Sandbox {
            rt: self.clone(),
            info,
        })
    }

    /// ID, 이름, ID 앞부분으로 샌드박스를 찾는다(Docker와 같은 순서다). 정확히 같은 ID가 있으면 그것,
    /// 그 이름의 샌드박스가 있으면 그것, 아니면 그것으로 시작하는 ID가 하나뿐일 때 그것이다.
    /// 없으면 [`RuntimeError::NoMatch`], 여럿이면 [`RuntimeError::Ambiguous`].
    pub async fn resolve(&self, query: &str) -> Result<SandboxId, RuntimeError> {
        if let Ok(id) = SandboxId::new(query)
            && self.read_info(&id)?.is_some()
        {
            return Ok(id);
        }
        if sbx_model::check_sandbox_name(query).is_ok()
            && let Some(id) = records::name_owner(&self.inner.config.data_dir, query)?
        {
            return Ok(id);
        }
        let mut found = records::ids(&self.inner.config.data_dir)?
            .into_iter()
            .filter(|id| !query.is_empty() && id.as_str().starts_with(query));
        match (found.next(), found.count()) {
            (Some(id), 0) => Ok(id),
            (Some(_), more) => Err(RuntimeError::Ambiguous {
                prefix: query.to_owned(),
                matches: more + 1,
            }),
            (None, _) => Err(RuntimeError::NoMatch(query.to_owned())),
        }
    }

    /// [`Runtime::resolve`]로 찾아 [`Runtime::get`]한다.
    pub async fn find(&self, query: &str) -> Result<Sandbox, RuntimeError> {
        let id = self.resolve(query).await?;
        self.get(&id).await
    }

    fn read_info(&self, id: &SandboxId) -> Result<Option<SandboxInfo>, RuntimeError> {
        let paths = self.paths(id);
        Ok(records::read(&paths)?.map(|(spec, state)| SandboxInfo {
            id: id.clone(),
            spec,
            state: state.state,
            error: state.error,
            exit_code: state.exit_code,
            control: state.control,
            egress_port: state.egress_port,
            created: std::fs::metadata(paths.spec())
                .and_then(|m| m.modified())
                .ok(),
        }))
    }

    /// data_dir의 모든 샌드박스. ID 순서.
    pub async fn list(&self) -> Result<Vec<SandboxInfo>, RuntimeError> {
        let mut out = Vec::new();
        for id in records::ids(&self.inner.config.data_dir)? {
            if let Some(info) = self.read_info(&id)? {
                out.push(info);
            }
        }
        Ok(out)
    }

    /// `timeout`까지 실행 중이 아니게 되기를(지워진 것도) 기다린다. 그렇게 되면 `true`.
    /// `stopped`가 기록된 뒤 소유 잠금이 풀릴 때까지도 기다린다. 그래야 돌아온 뒤 곧바로 다시 시작할 수 있다.
    fn wait_not_running(&self, id: &SandboxId, timeout: Duration) -> Result<bool, RuntimeError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.read_info(id)? {
                None => return Ok(true),
                Some(i) if i.state == State::Running => {}
                Some(_) => match records::is_owned(&self.paths(id)) {
                    Ok(false) => return Ok(true),
                    Ok(true) => {}
                    // 그사이 지워졌다.
                    Err(RuntimeError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(true);
                    }
                    Err(e) => return Err(e),
                },
            }
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(client::RETRY);
        }
    }

    /// 소유한 VM을 정리하고 `stopped`를 쓴다. 이미 정리했으면 아무것도 하지 않는다.
    /// 블로킹이다(작업 스레드에서 부른다).
    fn finish(
        &self,
        id: &SandboxId,
        exit_code: Option<i32>,
        error: Option<String>,
        grace: Duration,
    ) {
        let Some(mut owned) = lock(&self.inner.owned).remove(id) else {
            return;
        };
        let _ = self.guest(&owned.control).shutdown();
        if !chan::block_on(owned.vm.wait(grace)).unwrap_or(false) {
            let _ = owned.vm.kill();
        }
        let state = StateRecord {
            state: State::Stopped,
            error,
            pid: None,
            exit_code,
            control: None,
            egress_port: None,
        };
        let _ = records::write_state(&self.paths(id), &state);
        // 상태를 쓴 뒤에 잠금을 놓는다. 그래야 다른 프로세스가 "소유자 사라짐"으로 잘못 보지 않는다.
        drop(owned);
    }
}

fn to_proto_user(u: &UserSpec) -> sbx_guest_proto::User {
    let ident = |i: &Ident| match i {
        Ident::Id(n) => sbx_guest_proto::Ident::Id(*n),
        Ident::Name(s) => sbx_guest_proto::Ident::Name(s.clone()),
    };
    sbx_guest_proto::User {
        user: ident(&u.user),
        group: u.group.as_ref().map(ident),
    }
}

/// bind 마운트 `i`번째(0부터)의 공유 태그.
fn share_tag(i: usize) -> String {
    format!("sbx{i}")
}

/// `main`이면 기본 프로세스다: 끝나면 남은 자식의 출력을 기다리지 않는다(샌드박스가 곧 멈춘다).
fn to_proto_exec(p: &ProcessDefaults, io: IoMode, main: bool) -> sbx_guest_proto::Exec {
    sbx_guest_proto::Exec {
        args: p.args.clone(),
        env: p.env.clone(),
        cwd: p.cwd.clone(),
        user: to_proto_user(&p.user),
        tty: io.tty.is_some(),
        stdin: io.stdin,
        size: io.tty.map(|t| WinSize {
            rows: t.rows,
            cols: t.cols,
        }),
        main,
    }
}

fn exit_code(s: ExitStatus) -> i32 {
    match s {
        ExitStatus::Code(c) => c,
        ExitStatus::Signal(n) => 128 + n,
    }
}

fn guest_parent(path: &str) -> (String, String) {
    let t = path.trim_end_matches('/');
    match t.rsplit_once('/') {
        Some(("", name)) => ("/".into(), name.into()),
        Some((parent, name)) => (parent.into(), name.into()),
        None => ("/".into(), t.into()),
    }
}

/// [`Sandbox::read_file`]이 따라가는 심볼릭 링크 수 (Linux `MAXSYMLINKS`와 같다).
pub const MAX_READ_LINKS: usize = 40;

/// 게스트 경로의 마지막 이름. `/`처럼 이름이 없으면 `None`.
fn guest_basename(path: &str) -> Option<&str> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
}

fn require_absolute(path: &str) -> Result<(), RuntimeError> {
    if path.starts_with('/') {
        Ok(())
    } else {
        Err(RuntimeError::Invalid(format!(
            "guest path must be absolute: {path:?}"
        )))
    }
}

/// 샌드박스 하나. 값은 얻은 시점의 기록이다. 동작은 매번 기록을 다시 읽고 판단한다.
#[derive(Debug, Clone)]
pub struct Sandbox {
    rt: Runtime,
    info: SandboxInfo,
}

impl Sandbox {
    /// ID.
    pub fn id(&self) -> &SandboxId {
        &self.info.id
    }

    /// 얻은 시점의 상태.
    pub fn state(&self) -> State {
        self.info.state
    }

    /// 얻은 시점의 기록.
    pub fn info(&self) -> &SandboxInfo {
        &self.info
    }

    /// 로그 디렉터리. 호출자도 여기에 로그를 둘 수 있다(`executor.log`는 실행 코어가 쓴다).
    /// 샌드박스를 지우면 함께 지워진다. 아직 없을 수 있다.
    pub fn log_dir(&self) -> PathBuf {
        self.rt.paths(&self.info.id).log_dir()
    }

    fn fresh(&self) -> Result<SandboxInfo, RuntimeError> {
        self.rt
            .read_info(&self.info.id)?
            .ok_or_else(|| RuntimeError::NotFound(self.info.id.clone()))
    }

    /// 실행 중이어야 한다. 기록과 게스트 연결을 돌려준다.
    fn require_running(&self, action: &'static str) -> Result<(SandboxInfo, Guest), RuntimeError> {
        let info = self.fresh()?;
        let control = match (info.state, &info.control) {
            (State::Running, Some(c)) => c.clone(),
            (current, _) => return Err(RuntimeError::InvalidState { current, action }),
        };
        let guest = self.rt.guest(&control);
        Ok((info, guest))
    }

    /// 시작한다. `created`나 `stopped`에서만 할 수 있다. 기본 프로세스를 돌려준다.
    ///
    /// 순서: 소유 잠금 → 게스트 init 확인 → rootfs 잠금(이미지 삭제 방지) → VM 띄우기 → 게스트 준비 대기
    /// → copy 마운트 복사 → 기본 프로세스 실행 → `running` 기록.
    /// 중간에 실패하면 VM을 끝내고, 상태는 그대로 두고 오류를 기록한다.
    /// 기본 프로세스의 입출력은 `io`대로 연다.
    pub async fn start(&self, io: IoMode) -> Result<Process, RuntimeError> {
        let info = self.fresh()?;
        if !info.state.can_transition_to(State::Running) {
            return Err(RuntimeError::InvalidState {
                current: info.state,
                action: "start",
            });
        }
        let rt = &self.rt;
        let id = info.id.clone();
        let paths = rt.paths(&id);
        let control = rt.socket_path(&id)?;
        // 기록이 실행 중이 아니므로 소유자는 없어야 한다. 잠깐 쥐는 쪽이 있을 수 있어 조금 기다린다.
        let owner = records::own_within(&paths, OWN_WAIT)?
            .ok_or_else(|| RuntimeError::OwnedElsewhere(id.clone()))?;
        let guest_init = rt.inner.config.guest_init.clone();
        if !guest_init.is_file() {
            return Err(RuntimeError::GuestInitMissing(guest_init));
        }
        let spec = info.spec.clone();
        let lease = rt
            .inner
            .store
            .lease(&spec.image, spec.rootfs_format)
            .map_err(|e| match e {
                LeaseError::ImageNotFound => RuntimeError::ImageNotPulled(spec.image.clone()),
                LeaseError::RootfsNotFound => RuntimeError::RootfsNotPrepared {
                    image: spec.image.clone(),
                    format: spec.rootfs_format,
                },
                LeaseError::KernelNotFound => RuntimeError::KernelNotPrepared(spec.image.clone()),
                LeaseError::Io(e) => RuntimeError::Io(e),
            })?;
        let vm_config = VmConfig {
            guest_init,
            kernel: lease.kernel().map(Path::to_path_buf),
            disks: vec![
                Disk {
                    path: lease.rootfs().to_owned(),
                    role: DiskRole::Rootfs,
                    readonly: true,
                },
                Disk {
                    path: paths.scratch(),
                    role: DiskRole::Scratch,
                    readonly: false,
                },
            ],
            vcpus: spec.resources.vcpus,
            memory_mib: spec.resources.memory_mib,
            shares: spec
                .mounts
                .iter()
                .filter(|m| m.kind == MountKind::Bind)
                .enumerate()
                .map(|(i, m)| sbx_executor::Share {
                    host_path: m.source.clone(),
                    tag: share_tag(i),
                    readonly: m.readonly,
                })
                .collect(),
            control_channel: ChannelSpec {
                host_endpoint: control.clone(),
                guest_port: sbx_guest_proto::GUEST_PORT,
            },
            hostname: spec
                .hostname
                .clone()
                .unwrap_or_else(|| id.as_str().chars().take(12).collect()),
            log: paths.executor_log(),
        };
        let record_failure = |e: RuntimeError| {
            let mut state = StateRecord::new(info.state);
            // 기록만 고친다. 소유 잠금은 아래에서 함께 놓인다.
            state.error = Some(e.to_string());
            state.exit_code = info.exit_code;
            let _ = records::write_state(&paths, &state);
            e
        };
        let vm = rt
            .inner
            .executor
            .launch(vm_config)
            .await
            .map_err(|e| record_failure(e.into()))?;

        let guest = rt.guest(&control);
        let mut main = to_proto_exec(&spec.process, io, true);
        let mounts = spec.mounts.clone();
        let network = spec.network.clone();
        let (vm, started) = blocking(move || {
            let mut vm = vm;
            let r = (|| {
                let mut alive = || !chan::block_on(vm.wait(Duration::ZERO)).unwrap_or(true);
                guest.wait_ready(START_TIMEOUT, &mut alive)?;
                let binds = mounts.iter().filter(|m| m.kind == MountKind::Bind);
                for (i, m) in binds.enumerate() {
                    guest.share(&share_tag(i), &m.destination, m.readonly)?;
                }
                for m in mounts.iter().filter(|m| m.kind == MountKind::Copy) {
                    let target = guest.stat(&m.destination)?;
                    match target {
                        Some(e) if e.kind == sbx_guest_proto::FileKind::Dir => {
                            guest.copy_in(&m.source, &m.destination, None)?
                        }
                        Some(_) => {
                            return Err(RuntimeError::Invalid(format!(
                                "copy mount target {} is not a directory",
                                m.destination
                            )));
                        }
                        None => {
                            let (parent, name) = guest_parent(&m.destination);
                            guest.copy_in(&m.source, &parent, Some(&name))?
                        }
                    }
                }
                // 나가는 연결: 프록시를 띄우고, 맡길 연결을 돌리고, 기본 프로세스에 프록시 환경 변수를 준다.
                let egress_port = match network {
                    NetworkPolicy::Disabled => None,
                    NetworkPolicy::Allow(rules) => {
                        let port = guest.egress_listen()?;
                        egress::start(guest.clone(), rules);
                        process::add_proxy_env(&mut main.env, port);
                        Some(port)
                    }
                };
                guest.exec(main).map(|(c, pid)| (c, pid, egress_port))
            })();
            if r.is_err() {
                let _ = vm.kill();
            }
            (vm, r)
        })
        .await;
        let (conn, pid, egress_port) = started.map_err(record_failure)?;

        let state = StateRecord {
            state: State::Running,
            error: None,
            pid: vm.pid(),
            exit_code: None,
            control: Some(control.clone()),
            egress_port,
        };
        records::write_state(&paths, &state)?;
        lock(&rt.inner.owned).insert(
            id.clone(),
            Owned {
                vm,
                control,
                _lease: lease,
                _lock: owner,
            },
        );
        let rt2 = rt.clone();
        Ok(Process::spawn(
            conn,
            pid,
            Some(Box::new(move |code: i32, error: Option<String>| {
                rt2.finish(&id, Some(code), error, VM_EXIT_GRACE)
            })),
        ))
    }

    /// 안에서 프로세스를 하나 더 실행한다. `running`에서만 할 수 있다. 규칙은 [`process::for_exec`].
    /// 입출력은 [`IoMode::default`]에 `spec.tty`를 더한 것이다.
    pub async fn exec(&self, spec: ExecSpec) -> Result<Process, RuntimeError> {
        self.exec_with(spec, IoMode::default()).await
    }

    /// [`Sandbox::exec`]와 같고, 입출력은 `io`대로 연다. `spec.tty`이고 `io.tty`가 없으면 터미널 크기는 게스트 기본값이다.
    pub async fn exec_with(&self, spec: ExecSpec, io: IoMode) -> Result<Process, RuntimeError> {
        let (info, guest) = self.require_running("exec")?;
        let p = process::for_exec(&info.spec.process, &spec)?;
        let io = IoMode {
            tty: io.tty.or(spec.tty.then_some(TtySize::GUEST_DEFAULT)),
            ..io
        };
        let mut x = to_proto_exec(&p, io, false);
        if let Some(port) = info.egress_port {
            process::add_proxy_env(&mut x.env, port);
        }
        let (conn, pid) = blocking(move || guest.exec(x)).await?;
        Ok(Process::spawn(conn, pid, None))
    }

    /// 호스트 파일이나 디렉터리를 게스트로 복사한다. 대상 규칙은 [`cpplan`].
    pub async fn copy_in(&self, host: &Path, guest_path: &str) -> Result<(), RuntimeError> {
        let (_, guest) = self.require_running("copy")?;
        require_absolute(guest_path)?;
        let host = host.to_owned();
        let guest_path = guest_path.to_owned();
        blocking(move || {
            // Path는 끝의 `.`을 정규화하므로 문자열로 본다.
            let raw = host.as_os_str().to_string_lossy();
            let contents = raw.ends_with("/.") || raw.ends_with("\\.");
            let meta = std::fs::symlink_metadata(&host)?;
            let target = match guest.stat(&guest_path)? {
                None => cpplan::Target::Missing,
                Some(e) if e.kind == sbx_guest_proto::FileKind::Dir => cpplan::Target::Dir,
                Some(_) => cpplan::Target::File,
            };
            let placement =
                cpplan::plan(meta.is_dir(), contents, target, guest_path.ends_with('/'))
                    .map_err(|m| RuntimeError::Invalid(format!("{m}: {guest_path}")))?;
            let src_name = host
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .ok_or_else(|| RuntimeError::Invalid(format!("cannot copy {}", host.display())))?;
            match placement {
                cpplan::Placement::Inside => guest.copy_in(&host, &guest_path, Some(&src_name)),
                cpplan::Placement::ContentsInside => guest.copy_in(&host, &guest_path, None),
                cpplan::Placement::AsTarget => {
                    let (parent, name) = guest_parent(&guest_path);
                    guest.copy_in(&host, &parent, Some(&name))
                }
            }
        })
        .await
    }

    /// 게스트 파일 하나의 내용을 읽는다. 호스트 파일시스템에는 아무것도 쓰지 않는다.
    ///
    /// 심볼릭 링크는 게스트 안의 경로로 따라간다([`MAX_READ_LINKS`]번까지). 호스트의 같은 이름은 보지 않는다.
    /// 크기가 `limit`을 넘거나 일반 파일이 아니면 내용을 받기 전에 오류다.
    pub async fn read_file(&self, guest_path: &str, limit: u64) -> Result<Vec<u8>, RuntimeError> {
        let (_, guest) = self.require_running("read")?;
        require_absolute(guest_path)?;
        let asked = guest_path.to_owned();
        let mut path = asked.clone();
        blocking(move || {
            for _ in 0..=MAX_READ_LINKS {
                match guest.read_bytes(&path, limit)? {
                    hostfs::Received::File(b) => return Ok(b),
                    hostfs::Received::Symlink(target) => {
                        path = if target.starts_with('/') {
                            target
                        } else {
                            let (parent, _) = guest_parent(&path);
                            format!("{}/{target}", parent.trim_end_matches('/'))
                        };
                    }
                }
            }
            Err(RuntimeError::Invalid(format!(
                "{asked}: more than {MAX_READ_LINKS} symbolic links"
            )))
        })
        .await
    }

    /// 게스트 파일이나 디렉터리를 호스트로 복사한다. 대상 규칙은 [`cpplan`].
    pub async fn copy_out(&self, guest_path: &str, host: &Path) -> Result<(), RuntimeError> {
        let (_, guest) = self.require_running("copy")?;
        require_absolute(guest_path)?;
        let host = host.to_owned();
        let guest_path = guest_path.to_owned();
        blocking(move || {
            let contents = guest_path.ends_with("/.");
            let src = guest_path.strip_suffix("/.").unwrap_or(&guest_path);
            let src = if src.is_empty() { "/" } else { src };
            let src_entry = guest.stat(src)?.ok_or_else(|| {
                RuntimeError::Guest(sbx_guest_proto::Error::new(
                    sbx_guest_proto::ErrorKind::NotFound,
                    format!("no such file or directory in the sandbox: {src}"),
                ))
            })?;
            let src_is_dir = src_entry.kind == sbx_guest_proto::FileKind::Dir;
            let target = match std::fs::metadata(&host) {
                Ok(m) if m.is_dir() => cpplan::Target::Dir,
                Ok(_) => cpplan::Target::File,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => cpplan::Target::Missing,
                Err(e) => return Err(e.into()),
            };
            let slash = host
                .as_os_str()
                .to_string_lossy()
                .ends_with(std::path::MAIN_SEPARATOR)
                || host.as_os_str().to_string_lossy().ends_with('/');
            let placement = cpplan::plan(src_is_dir, contents, target, slash)
                .map_err(|m| RuntimeError::Invalid(format!("{m}: {}", host.display())))?;
            match placement {
                // 이름은 호스트가 요청한 경로에서 뽑는다. 게스트의 Stat 답(`src_entry.path`)은 믿지 않는다.
                cpplan::Placement::Inside => {
                    let name = guest_basename(src).ok_or_else(|| {
                        RuntimeError::Invalid(format!("cannot copy {src:?} into a directory"))
                    })?;
                    guest.copy_out(src, &host, Some(name))
                }
                cpplan::Placement::ContentsInside => guest.copy_out(src, &host, None),
                cpplan::Placement::AsTarget => {
                    let parent = host
                        .parent()
                        .filter(|p| !p.as_os_str().is_empty())
                        .unwrap_or(Path::new("."));
                    let name = host
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .ok_or_else(|| {
                            RuntimeError::Invalid(format!("cannot copy to {}", host.display()))
                        })?;
                    guest.copy_out(src, parent, Some(&name))
                }
            }
        })
        .await
    }

    /// 멈춘다. Docker `stop`과 같다.
    ///
    /// 1. 기본 프로세스에 시그널(`signal`, 없으면 [`StopSpec::signal`](sbx_model::StopSpec::signal))을
    ///    보내고 `timeout`까지 샌드박스가 멈추기를 기다린다. 기본 프로세스가 끝나면 샌드박스가 멈추고,
    ///    종료 코드는 그 프로세스의 것이다(SIGTERM으로 끝나면 143). `timeout`이 0이면 이 단계를 건너뛴다.
    /// 2. 그래도 실행 중이면 게스트를 끈다(sync 뒤 전원 끄기). [`VM_EXIT_GRACE`] 안에 꺼지지 않으면
    ///    VM을 강제로 끝낸다. 종료 코드는 [`EXIT_VM_ENDED`]다(Docker에서 SIGKILL로 끝난 것과 같다).
    ///
    /// 다른 프로세스가 소유한 VM이면 2에서 게스트를 끄고 [`VM_EXIT_GRACE`]까지 소유가 풀리기를 기다린다.
    /// 그래도 끝나지 않으면 [`RuntimeError::OwnedElsewhere`]다(VM을 강제로 끝낼 수 있는 것은 소유 프로세스뿐이다).
    pub async fn stop(
        &self,
        signal: Option<Signal>,
        timeout: Duration,
    ) -> Result<(), RuntimeError> {
        let (info, guest) = self.require_running("stop")?;
        let signal = signal.unwrap_or(info.spec.stop.signal);
        let rt = self.rt.clone();
        let id = info.id.clone();
        blocking(move || {
            if !timeout.is_zero() {
                // 기본 프로세스가 없으면 이미 끝나는 중이다. 그래도 기다린다.
                let _ = guest.signal_main(signal.number());
                if rt.wait_not_running(&id, timeout)? {
                    return Ok(());
                }
            }
            if lock(&rt.inner.owned).contains_key(&id) {
                rt.finish(&id, Some(EXIT_VM_ENDED), None, VM_EXIT_GRACE);
                return Ok(());
            }
            let _ = guest.shutdown();
            let paths = rt.paths(&id);
            let deadline = std::time::Instant::now() + VM_EXIT_GRACE;
            while records::is_owned(&paths)? {
                if std::time::Instant::now() >= deadline {
                    return Err(RuntimeError::OwnedElsewhere(id));
                }
                std::thread::sleep(client::RETRY);
            }
            Ok(())
        })
        .await
    }

    /// 기본 프로세스에 시그널을 보낸다. Docker `kill`과 같다: 프로세스가 그 시그널로 끝나면 샌드박스가 멈춘다.
    /// 기본 프로세스가 이미 끝나 샌드박스가 멈추는 중이면 아무것도 하지 않는다.
    pub async fn kill(&self, signal: Signal) -> Result<(), RuntimeError> {
        let (_, guest) = self.require_running("kill")?;
        blocking(move || guest.signal_main(signal.number()).map(|_| ())).await
    }

    /// 멈출 때까지 기다리고 기본 프로세스의 종료 코드를 돌려준다. Docker `wait`과 같다.
    /// 이미 멈췄으면 곧바로 돌아온다. 아직 시작하지 않았으면 시작해서 멈출 때까지 기다린다.
    /// 종료 코드가 기록되지 않았으면(소유 프로세스가 사라짐) [`EXIT_VM_ENDED`]다. 기다리는 동안 지워지면
    /// [`RuntimeError::NotFound`]다.
    pub async fn wait(&self) -> Result<i32, RuntimeError> {
        let rt = self.rt.clone();
        let id = self.info.id.clone();
        blocking(move || {
            loop {
                let info = rt
                    .read_info(&id)?
                    .ok_or_else(|| RuntimeError::NotFound(id.clone()))?;
                if info.state == State::Stopped {
                    return Ok(info.exit_code.unwrap_or(EXIT_VM_ENDED));
                }
                std::thread::sleep(client::RETRY);
            }
        })
        .await
    }

    /// 지운다. 실행 중이면 오류다(강제 삭제는 표현 계층이 `stop` 후 `delete`로 만든다).
    pub async fn delete(self) -> Result<(), RuntimeError> {
        let info = self.fresh()?;
        if info.state == State::Running {
            return Err(RuntimeError::InvalidState {
                current: info.state,
                action: "delete",
            });
        }
        // 이름을 먼저 놓는다(이름 파일이 없는 샌드박스를 가리키지 않게).
        if let Some(name) = &info.spec.name {
            records::release_name(&self.rt.inner.config.data_dir, name, &info.id)?;
        }
        records::remove(&self.rt.paths(&info.id))
    }
}

/// 프로세스의 입출력 방식. 기본값은 표준 입력을 닫고 터미널 없이(출력은 stdout, stderr 따로)다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IoMode {
    /// 표준 입력을 연다. 아니면 처음부터 닫혀 있다.
    pub stdin: bool,
    /// 터미널을 붙인다. 값은 처음 크기다. 그러면 출력은 하나로 합쳐져 [`Output::Stdout`]으로만 온다.
    /// 크기는 [`ProcessInput::resize`]로 바꾼다.
    pub tty: Option<TtySize>,
}

/// 터미널 크기.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TtySize {
    /// 줄 수.
    pub rows: u16,
    /// 열 수.
    pub cols: u16,
}

impl TtySize {
    /// 크기를 모를 때 게스트가 쓰는 크기 (24x80).
    pub const GUEST_DEFAULT: TtySize = TtySize { rows: 24, cols: 80 };
}

/// 프로세스 출력 한 조각.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    /// 표준 출력.
    Stdout(Vec<u8>),
    /// 표준 오류.
    Stderr(Vec<u8>),
}

enum Event {
    Out(Output),
    Exit(i32),
    Lost(String),
}

type OnEnd = Box<dyn FnOnce(i32, Option<String>) + Send>;

/// 샌드박스 안의 프로세스. 버려도 프로세스는 계속 돈다(출력은 버린다).
///
/// 기본 프로세스([`Sandbox::start`])의 끝([`Process::next`]가 `None`)을 받았으면 샌드박스는 이미 `stopped`로 기록되어 있다.
pub struct Process {
    pid: u32,
    events: chan::Receiver<Event>,
    input: ProcessInput,
    exit: Option<Result<i32, String>>,
}

/// 프로세스에 보내는 쪽. 복제해서 다른 스레드에서 쓸 수 있다(예: 표준 입력을 읽는 스레드, 시그널 처리).
/// 보내기는 별도 스레드가 하므로 막히지 않는다. 프로세스가 끝났으면 버린다.
#[derive(Debug, Clone)]
pub struct ProcessInput(std::sync::mpsc::Sender<HostMsg>);

impl ProcessInput {
    /// 표준 입력에 쓴다.
    pub fn write_stdin(&self, data: &[u8]) {
        for chunk in data.chunks(sbx_guest_proto::CHUNK_LEN) {
            let _ = self.0.send(HostMsg::Stdin(chunk.to_vec()));
        }
    }

    /// 표준 입력을 닫는다.
    pub fn close_stdin(&self) {
        let _ = self.0.send(HostMsg::StdinClose);
    }

    /// 시그널을 보낸다. 번호는 게스트(Linux)의 번호다.
    pub fn signal(&self, signal: i32) {
        let _ = self.0.send(HostMsg::Signal(signal));
    }

    /// 터미널 크기를 바꾼다.
    pub fn resize(&self, rows: u16, cols: u16) {
        let _ = self.0.send(HostMsg::Resize(WinSize { rows, cols }));
    }
}

impl fmt::Debug for Process {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Process")
            .field("pid", &self.pid)
            .field("exit", &self.exit)
            .finish()
    }
}

impl Process {
    /// `on_end`가 있으면 기본 프로세스다: VM이 먼저 끝나면 [`EXIT_VM_ENDED`]로 끝난 것으로 보고, 끝나면 `on_end`를 부른다.
    fn spawn(conn: Box<dyn GuestConn>, pid: u32, on_end: Option<OnEnd>) -> Self {
        let (tx, events) = chan::channel();
        let (input, rx) = std::sync::mpsc::channel::<HostMsg>();
        match conn.try_clone_conn() {
            Ok(mut writer) => {
                std::thread::spawn(move || {
                    for m in rx {
                        if m.write_to(&mut *writer).is_err() {
                            break;
                        }
                    }
                });
            }
            Err(e) => tx.send(Event::Lost(e.to_string())),
        }
        let is_main = on_end.is_some();
        std::thread::spawn(move || {
            let mut conn = conn;
            let (code, error) = loop {
                match GuestMsg::read_from(&mut *conn) {
                    Ok(GuestMsg::Stdout(b)) => tx.send(Event::Out(Output::Stdout(b))),
                    Ok(GuestMsg::Stderr(b)) => tx.send(Event::Out(Output::Stderr(b))),
                    Ok(GuestMsg::Exited(s)) => break (Some(exit_code(s)), None),
                    Ok(GuestMsg::Error(e)) => break (None, Some(e.message)),
                    Ok(other) => break (None, Some(format!("unexpected {other:?}"))),
                    Err(e) => break (None, Some(e.to_string())),
                }
            };
            let code = match (code, is_main) {
                (Some(c), _) => Some(c),
                (None, true) => Some(EXIT_VM_ENDED),
                (None, false) => None,
            };
            let event = match (code, &error) {
                (Some(c), _) => Event::Exit(c),
                (None, Some(e)) => Event::Lost(e.clone()),
                (None, None) => Event::Lost("connection closed".into()),
            };
            // 기본 프로세스는 샌드박스를 멈추고 기록한 뒤에 끝을 알린다.
            // 그래서 끝을 받은 호출자는 곧바로 그 샌드박스를 지울 수 있다.
            if let Some(f) = on_end {
                let lost = if code == Some(EXIT_VM_ENDED) {
                    error
                } else {
                    None
                };
                f(code.unwrap_or(EXIT_VM_ENDED), lost);
            }
            tx.send(event);
        });
        Self {
            pid,
            events,
            input: ProcessInput(input),
            exit: None,
        }
    }

    /// 게스트 안의 PID.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// 다음 출력. 끝났으면 `None`이고, 그 뒤 [`Process::exit_code`]가 있다.
    pub async fn next(&mut self) -> Option<Output> {
        if self.exit.is_some() {
            return None;
        }
        match self.events.recv().await {
            Some(Event::Out(o)) => Some(o),
            Some(Event::Exit(c)) => {
                self.exit = Some(Ok(c));
                None
            }
            Some(Event::Lost(e)) => {
                self.exit = Some(Err(e));
                None
            }
            None => {
                self.exit = Some(Err("connection closed".into()));
                None
            }
        }
    }

    /// 종료 코드. 아직 끝나지 않았거나 연결이 끊겼으면 `None`.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit.as_ref().and_then(|e| e.as_ref().ok().copied())
    }

    /// 보내는 쪽. 표준 입력, 시그널, 터미널 크기.
    pub fn input(&self) -> ProcessInput {
        self.input.clone()
    }

    /// 표준 입력에 쓴다. [`ProcessInput::write_stdin`]과 같다.
    pub fn write_stdin(&self, data: &[u8]) {
        self.input.write_stdin(data)
    }

    /// 표준 입력을 닫는다.
    pub fn close_stdin(&self) {
        self.input.close_stdin()
    }

    /// 시그널을 보낸다. 번호는 게스트(Linux)의 번호다.
    pub fn signal(&self, signal: i32) {
        self.input.signal(signal)
    }

    /// 터미널 크기를 바꾼다.
    pub fn resize(&self, rows: u16, cols: u16) {
        self.input.resize(rows, cols)
    }

    /// 끝날 때까지 기다려 종료 코드를 돌려준다. 출력은 버린다.
    pub async fn wait(mut self) -> Result<i32, RuntimeError> {
        while self.next().await.is_some() {}
        match self.exit.take() {
            Some(Ok(c)) => Ok(c),
            Some(Err(e)) => Err(RuntimeError::Lost(e)),
            None => Err(RuntimeError::Lost("connection closed".into())),
        }
    }
}

#[cfg(test)]
mod tests;
