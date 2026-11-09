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
//! - `copy` 마운트는 시작할 때 기본 프로세스보다 먼저 한 번 복사한다. 대상이 디렉터리로 있으면 그 안에 내용을, 없으면 그 경로로 만든다.
//! - 파일 복사([`Sandbox::copy_in`], [`Sandbox::copy_out`])의 대상 규칙은 Docker `cp`와 같다([`cpplan`]).
//!   Docker와 달리 실행 중인 샌드박스에서만 된다(디스크를 읽는 것이 게스트이기 때문이다).
//!
//! **수명 (attached)**: VM을 띄운 프로세스가 그 VM을 소유한다(`owner.lock`). 소유 프로세스가 끝나면 실행기 규약에 따라
//! VM도 끝나고, 다른 프로세스는 그 샌드박스를 `stopped`로 본다. 다른 프로세스도 같은 data_dir로 exec, cp, stop을 할 수 있다.
//! detached(소유 프로세스와 상관없이 사는 VM)는 `sandbox-shim`이 맡는다(로드맵 M3).
//!
//! 모든 블로킹 입출력은 별도 스레드에서 한다([`sandbox_model::blocking`]). 특정 비동기 런타임에 의존하지 않는다.
//!
//! **층**: `lib` → `client` → `hostfs`, `records` → `process`, `validate`, `cpplan`, `chan` → `error`.
//!
//! - 의존: `sandbox-model`, `sandbox-proto`, `sandbox-executor`, `sandbox-store`(읽기)

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sandbox_executor::{ChannelSpec, Disk, DiskRole, Executor, GuestConn, VmConfig, VmHandle};
use sandbox_model::{
    ExecSpec, Ident, MountKind, ProcessDefaults, SandboxId, SandboxSpec, State, UserSpec, blocking,
};
use sandbox_proto::{ExitStatus, GuestMsg, HostMsg, WinSize};
use sandbox_store::{LeaseError, RootfsLease, Store};

mod chan;
mod client;
pub mod cpplan;
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
                guest_port: sandbox_proto::GUEST_PORT,
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
        records::create(
            &self.inner.config.data_dir,
            &id,
            &spec,
            spec.resources.scratch_disk_mib,
        )?;
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

    fn read_info(&self, id: &SandboxId) -> Result<Option<SandboxInfo>, RuntimeError> {
        Ok(
            records::read(&self.paths(id))?.map(|(spec, state)| SandboxInfo {
                id: id.clone(),
                spec,
                state: state.state,
                error: state.error,
                exit_code: state.exit_code,
                control: state.control,
            }),
        )
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
        };
        let _ = records::write_state(&self.paths(id), &state);
        // 상태를 쓴 뒤에 잠금을 놓는다. 그래야 다른 프로세스가 "소유자 사라짐"으로 잘못 보지 않는다.
        drop(owned);
    }
}

fn to_proto_user(u: &UserSpec) -> sandbox_proto::User {
    let ident = |i: &Ident| match i {
        Ident::Id(n) => sandbox_proto::Ident::Id(*n),
        Ident::Name(s) => sandbox_proto::Ident::Name(s.clone()),
    };
    sandbox_proto::User {
        user: ident(&u.user),
        group: u.group.as_ref().map(ident),
    }
}

fn to_proto_exec(p: &ProcessDefaults, tty: bool, stdin: bool) -> sandbox_proto::Exec {
    sandbox_proto::Exec {
        args: p.args.clone(),
        env: p.env.clone(),
        cwd: p.cwd.clone(),
        user: to_proto_user(&p.user),
        tty,
        stdin,
        size: None,
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
    /// `stdin`이면 기본 프로세스의 표준 입력을 연다.
    pub async fn start(&self, stdin: bool) -> Result<Process, RuntimeError> {
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
        let owner =
            records::try_own(&paths)?.ok_or_else(|| RuntimeError::OwnedElsewhere(id.clone()))?;
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
                LeaseError::Io(e) => RuntimeError::Io(e),
            })?;
        let vm_config = VmConfig {
            guest_init,
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
            shares: vec![],
            control_channel: ChannelSpec {
                host_endpoint: control.clone(),
                guest_port: sandbox_proto::GUEST_PORT,
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
        let main = to_proto_exec(&spec.process, false, stdin);
        let mounts = spec.mounts.clone();
        let (vm, started) = blocking(move || {
            let mut vm = vm;
            let r = (|| {
                let mut alive = || !chan::block_on(vm.wait(Duration::ZERO)).unwrap_or(true);
                guest.wait_ready(START_TIMEOUT, &mut alive)?;
                for m in mounts.iter().filter(|m| m.kind == MountKind::Copy) {
                    let target = guest.stat(&m.destination)?;
                    match target {
                        Some(e) if e.kind == sandbox_proto::FileKind::Dir => {
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
                guest.exec(main)
            })();
            if r.is_err() {
                let _ = vm.kill();
            }
            (vm, r)
        })
        .await;
        let (conn, pid) = started.map_err(record_failure)?;

        let state = StateRecord {
            state: State::Running,
            error: None,
            pid: vm.pid(),
            exit_code: None,
            control: Some(control.clone()),
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
    pub async fn exec(&self, spec: ExecSpec) -> Result<Process, RuntimeError> {
        self.exec_with(spec, false).await
    }

    /// [`Sandbox::exec`]와 같고, `stdin`이면 표준 입력을 연다.
    pub async fn exec_with(&self, spec: ExecSpec, stdin: bool) -> Result<Process, RuntimeError> {
        let (info, guest) = self.require_running("exec")?;
        let p = process::for_exec(&info.spec.process, &spec)?;
        let x = to_proto_exec(&p, spec.tty, stdin);
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
                Some(e) if e.kind == sandbox_proto::FileKind::Dir => cpplan::Target::Dir,
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
                RuntimeError::Guest(sandbox_proto::Error::new(
                    sandbox_proto::ErrorKind::NotFound,
                    format!("no such file or directory in the sandbox: {src}"),
                ))
            })?;
            let src_is_dir = src_entry.kind == sandbox_proto::FileKind::Dir;
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
                cpplan::Placement::Inside => guest.copy_out(src, &host, Some(&src_entry.path)),
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

    /// 멈춘다. 게스트에 종료를 요청하고 `timeout`까지 VM이 끝나기를 기다린다. 넘으면 강제로 끝낸다.
    ///
    /// 다른 프로세스가 소유한 VM이면 종료를 요청하고 `timeout`까지 소유가 풀리기를 기다린다.
    /// 그래도 끝나지 않으면 [`RuntimeError::OwnedElsewhere`]다(강제로 끝낼 수 있는 것은 소유 프로세스뿐이다).
    pub async fn stop(&self, timeout: Duration) -> Result<(), RuntimeError> {
        let (info, guest) = self.require_running("stop")?;
        let rt = self.rt.clone();
        let id = info.id.clone();
        blocking(move || {
            if lock(&rt.inner.owned).contains_key(&id) {
                rt.finish(&id, Some(EXIT_VM_ENDED), None, timeout);
                return Ok(());
            }
            let _ = guest.shutdown();
            let paths = rt.paths(&id);
            let deadline = std::time::Instant::now() + timeout;
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

    /// 지운다. 실행 중이면 오류다(강제 삭제는 표현 계층이 `stop` 후 `delete`로 만든다).
    pub async fn delete(self) -> Result<(), RuntimeError> {
        let info = self.fresh()?;
        if info.state == State::Running {
            return Err(RuntimeError::InvalidState {
                current: info.state,
                action: "delete",
            });
        }
        records::remove(&self.rt.paths(&info.id))
    }
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
        for chunk in data.chunks(sandbox_proto::CHUNK_LEN) {
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
