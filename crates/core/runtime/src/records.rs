//! data_dir의 샌드박스 기록.
//!
//! ```text
//! <data_dir>/sandboxes/<id>/
//!   spec.json          SandboxSpec (생성 시 고정)
//!   state.json         StateRecord
//!   scratch.img        쓰기 가능 디스크 (생성 시 0으로 채운 새 sparse 파일. 게스트가 첫 부팅 때 포맷하며 0이라고 믿는다)
//!   owner.lock         VM을 띄운 프로세스가 실행 중 배타 잠금을 쥔다
//!   logs/              로그. 샌드박스를 지우면 함께 지워진다
//!     executor.log     실행기의 진단 출력
//!     (그 밖)          호출자가 쓰는 로그 (예: `sbx-shim`의 기본 프로세스 출력)
//! ```
//!
//! - 만들기는 임시 디렉터리에 다 쓴 뒤 rename한다. 그래서 반쯤 만든 샌드박스는 목록에 나오지 않는다.
//! - 지우기는 먼저 ID가 아닌 이름으로 rename하고 지운다. 그래서 반쯤 지운 샌드박스도 목록에 나오지 않고,
//!   읽는 도중에 지워지면 없는 것으로 본다. 지우다 프로세스가 죽으면 그 이름의 디렉터리가 남는다(목록에는 없다).
//!   Windows에서는 다른 쪽이 안의 파일을 잠깐 열고 있으면 rename이, 지워지는 중인 파일을 열면 읽기가
//!   "Access is denied"로 실패한다. 둘 다 잠깐이라 Windows에서만 잠시 다시 해 본다([`transient`]).
//! - 상태 쓰기는 임시 파일에 쓴 뒤 rename한다. rename 전에 `fsync(2)`한다([`sync`]). macOS에서는 `F_FULLFSYNC`
//!   (디스크 캐시까지 비우기, 한 번에 약 4ms)를 하지 않는다: 기록은 커널이 죽어도 남지만, 전원이 나가는 경우까지는
//!   보장하지 않는다. 실행 상태는 전원과 함께 어차피 사라지고, 한 번 실행에 기록을 네 번 쓰기 때문이다.
//! - 게스트 연결 소켓은 여기 두지 않는다. Unix 소켓 경로 길이 제한(약 100바이트) 때문에 짧은 `socket_dir`에 두고,
//!   실행 중일 때 그 경로를 `state.json`의 `control`에 적는다. 다른 프로세스는 적힌 경로로 연결한다.
//! - **소유자**: `running` 기록은 `owner.lock`을 누군가 쥐고 있을 때만 믿는다. 잠금이 풀려 있으면
//!   소유 프로세스가 죽은 것이다(VM도 실행기 규약에 따라 끝났다). 그때는 `stopped`로 보고 이유를 붙인다.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sbx_model::{SandboxId, SandboxSpec, State};
use serde::{Deserialize, Serialize};

use crate::error::RuntimeError;

/// `state.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateRecord {
    /// 상태.
    pub state: State,
    /// `stopped`에 딸린 오류 정보.
    #[serde(default)]
    pub error: Option<String>,
    /// 실행기 프로세스 ID (실행 중일 때, 알면).
    #[serde(default)]
    pub pid: Option<u32>,
    /// 기본 프로세스의 종료 코드 (끝났을 때).
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// 게스트 연결 소켓 (실행 중일 때).
    #[serde(default)]
    pub control: Option<PathBuf>,
    /// 게스트 안 나가는 연결 프록시의 포트 (실행 중이고 네트워크를 허용했을 때).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub egress_port: Option<u16>,
}

impl StateRecord {
    /// 그 상태만 있는 기록.
    pub fn new(state: State) -> Self {
        Self {
            state,
            error: None,
            pid: None,
            exit_code: None,
            control: None,
            egress_port: None,
        }
    }
}

/// 소유 프로세스가 사라졌을 때 붙이는 이유.
pub const OWNER_GONE: &str = "the process that started this sandbox exited";

/// 샌드박스 하나의 경로.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// 디렉터리.
    pub dir: PathBuf,
}

impl Paths {
    /// data_dir과 ID로.
    pub fn new(data_dir: &Path, id: &SandboxId) -> Self {
        Self {
            dir: data_dir.join("sandboxes").join(id.as_str()),
        }
    }

    /// `spec.json`.
    pub fn spec(&self) -> PathBuf {
        self.dir.join("spec.json")
    }
    /// `state.json`.
    pub fn state(&self) -> PathBuf {
        self.dir.join("state.json")
    }
    /// `scratch.img`.
    pub fn scratch(&self) -> PathBuf {
        self.dir.join("scratch.img")
    }
    /// `owner.lock`.
    pub fn owner_lock(&self) -> PathBuf {
        self.dir.join("owner.lock")
    }
    /// `logs/executor.log`.
    pub fn executor_log(&self) -> PathBuf {
        self.log_dir().join("executor.log")
    }

    /// `logs/`.
    pub fn log_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 파일 내용을 커널 밖(장치)으로 내보낸다. macOS에서는 `fsync(2)`만 한다(모듈 문서). 다른 곳은 `sync_all`.
fn sync(f: &File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    return rustix::fs::fsync(f).map_err(io::Error::from);
    #[cfg(not(target_vendor = "apple"))]
    f.sync_all()
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let t = temp(dir, &name);
    let r = (|| {
        let mut f = File::create(&t)?;
        f.write_all(bytes)?;
        sync(&f)?;
        fs::rename(&t, path)
    })();
    if r.is_err() {
        let _ = fs::remove_file(&t);
    }
    r
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, RuntimeError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| RuntimeError::Corrupt {
        path: path.to_owned(),
        reason: e.to_string(),
    })
}

fn to_json<T: Serialize>(v: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(v).map_err(io::Error::other)
}

/// 새 ID. 64자리 16진수 (Docker와 같은 모양).
pub fn new_id() -> SandboxId {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(64);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    for i in 0..4u64 {
        // RandomState는 프로세스마다 무작위 키를 쓴다. 시각, PID, 순번을 더해 섞는다.
        let mut h = RandomState::new().build_hasher();
        h.write_u128(now);
        h.write_u32(std::process::id());
        h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        h.write_u64(i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    SandboxId::new(&out).expect("hex id is valid")
}

/// 만든다. 다 쓴 뒤에 제자리로 옮긴다.
pub fn create(
    data_dir: &Path,
    id: &SandboxId,
    spec: &SandboxSpec,
    scratch_mib: u32,
) -> Result<(), RuntimeError> {
    let parent = data_dir.join("sandboxes");
    fs::create_dir_all(&parent)?;
    let staging = temp(&parent, id.as_str());
    let r = (|| -> Result<(), RuntimeError> {
        fs::create_dir(&staging)?;
        let p = Paths {
            dir: staging.clone(),
        };
        atomic_write(&p.spec(), &to_json(spec)?)?;
        atomic_write(&p.state(), &to_json(&StateRecord::new(State::Created))?)?;
        File::create(p.scratch())?.set_len(u64::from(scratch_mib) << 20)?;
        File::create(p.owner_lock())?;
        fs::create_dir(p.dir.join("logs"))?;
        fs::rename(&staging, Paths::new(data_dir, id).dir)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    r
}

/// 상태를 쓴다.
pub fn write_state(paths: &Paths, state: &StateRecord) -> Result<(), RuntimeError> {
    Ok(atomic_write(&paths.state(), &to_json(state)?)?)
}

/// Windows에서 "Access is denied"가 잠깐의 경합(다른 쪽이 파일을 열고 있거나 지우는 중)일 때 다시 해 본다.
/// 다른 OS에서는 한 번만 한다(그곳의 권한 오류는 진짜 권한 문제다).
fn transient<T, E>(
    mut f: impl FnMut() -> Result<T, E>,
    denied: impl Fn(&E) -> bool,
) -> Result<T, E> {
    #[cfg(windows)]
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match f() {
                Err(e) if denied(&e) && std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                r => return r,
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = denied;
        f()
    }
}

fn io_denied(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied
}

/// 읽는다. 없거나 만드는 중이면 `None`. 소유자가 사라진 `running`은 `stopped`로 고쳐 돌려준다(파일은 바꾸지 않는다).
pub fn read(paths: &Paths) -> Result<Option<(SandboxSpec, StateRecord)>, RuntimeError> {
    let r = transient(
        || read_present(paths),
        |e| matches!(e, RuntimeError::Io(e) if io_denied(e)),
    );
    match r {
        // 읽는 도중에 지워졌다.
        Err(RuntimeError::Io(e)) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        r => r,
    }
}

fn read_present(paths: &Paths) -> Result<Option<(SandboxSpec, StateRecord)>, RuntimeError> {
    if !paths.spec().is_file() || !paths.state().is_file() {
        return Ok(None);
    }
    let spec: SandboxSpec = read_json(&paths.spec())?;
    let mut state: StateRecord = read_json(&paths.state())?;
    if state.state == State::Running && !is_owned(paths)? {
        state = StateRecord {
            state: State::Stopped,
            error: Some(OWNER_GONE.into()),
            pid: None,
            exit_code: state.exit_code,
            control: None,
            egress_port: None,
        };
    }
    Ok(Some((spec, state)))
}

/// 지운다.
pub fn remove(paths: &Paths) -> Result<(), RuntimeError> {
    let (Some(parent), Some(name)) = (paths.dir.parent(), paths.dir.file_name()) else {
        return Ok(fs::remove_dir_all(&paths.dir)?);
    };
    let doomed = temp(parent, &format!("{}.rm", name.to_string_lossy()));
    transient(|| fs::rename(&paths.dir, &doomed), io_denied)?;
    Ok(transient(|| fs::remove_dir_all(&doomed), io_denied)?)
}

/// 소유자 잠금. 살아 있는 동안 이 프로세스가 VM을 소유한다.
#[derive(Debug)]
pub struct OwnerLock {
    _file: File,
}

/// 소유 잠금 파일. 읽기와 쓰기로 연다: Windows의 `LockFileEx`는 덧붙이기 전용 핸들을 거부한다("Access is denied").
fn open_lock(paths: &Paths) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(paths.owner_lock())
}

/// 소유를 시도한다. 누가 쥐고 있으면 `None`.
pub fn try_own(paths: &Paths) -> Result<Option<OwnerLock>, RuntimeError> {
    let file = open_lock(paths)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(OwnerLock { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// `wait`까지 소유를 시도한다. 잠금은 잠깐씩 다른 쪽이 쥘 수 있다: 다른 프로세스가 [`is_owned`]로
/// 확인하는 순간, 또는 fork한 자식이 exec 전까지 파일을 물려받은 동안. 그래서 한 번 실패로 판단하지 않는다.
pub fn own_within(
    paths: &Paths,
    wait: std::time::Duration,
) -> Result<Option<OwnerLock>, RuntimeError> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if let Some(l) = try_own(paths)? {
            return Ok(Some(l));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 누가 소유하고 있는지.
pub fn is_owned(paths: &Paths) -> Result<bool, RuntimeError> {
    Ok(try_own(paths)?.is_none())
}

/// 이름 파일: `<data_dir>/names/<이름>`. 내용은 그 이름을 가진 샌드박스의 ID다.
///
/// - 만들기는 기록을 먼저 만들고 이름을 잡는다([`claim_name`]). 지우기는 이름을 먼저 놓고([`release_name`]) 기록을
///   지운다. 그래서 이름 파일은 있는 샌드박스를 가리킨다. 그렇지 않은 것(중간에 죽은 경우)은 남은 것으로 보고
///   다음에 잡는 쪽이 넘겨받는다.
/// - 잡기는 임시 파일에 ID를 다 쓴 뒤 `hard_link`로 놓는다. 이름이 이미 있으면 실패하므로 동시에 잡아도 하나만
///   잡고, 읽는 쪽은 반쯤 쓴 파일을 보지 않는다.
fn name_path(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("names").join(name)
}

/// 이름을 가진 샌드박스. 이름 파일이 없거나 없는 샌드박스를 가리키면 `None`.
pub fn name_owner(data_dir: &Path, name: &str) -> Result<Option<SandboxId>, RuntimeError> {
    let text = match transient(|| fs::read_to_string(name_path(data_dir, name)), io_denied) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(SandboxId::new(text.trim())
        .ok()
        .filter(|id| Paths::new(data_dir, id).spec().is_file()))
}

/// 이름을 `id`에게 준다. 다른 샌드박스가 쓰고 있으면 [`RuntimeError::NameInUse`].
pub fn claim_name(data_dir: &Path, name: &str, id: &SandboxId) -> Result<(), RuntimeError> {
    let dir = data_dir.join("names");
    fs::create_dir_all(&dir)?;
    let path = name_path(data_dir, name);
    let staged = temp(&dir, name);
    atomic_write(&staged, id.as_str().as_bytes())?;
    let r = (|| {
        // 남은 이름 파일을 한 번 넘겨받을 수 있다.
        for _ in 0..2 {
            match fs::hard_link(&staged, &path) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    match name_owner(data_dir, name)? {
                        Some(owner) if owner == *id => return Ok(()),
                        Some(owner) => {
                            return Err(RuntimeError::NameInUse {
                                name: name.to_owned(),
                                id: owner,
                            });
                        }
                        None => {
                            let _ = fs::remove_file(&path);
                        }
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(RuntimeError::Io(io::Error::other(format!(
            "cannot claim the name {name:?}"
        ))))
    })();
    let _ = fs::remove_file(&staged);
    r
}

/// `id`가 가진 이름을 놓는다. 다른 샌드박스의 것이면 건드리지 않는다.
pub fn release_name(data_dir: &Path, name: &str, id: &SandboxId) -> Result<(), RuntimeError> {
    let path = name_path(data_dir, name);
    match fs::read_to_string(&path) {
        Ok(t) if t.trim() == id.as_str() => match transient(|| fs::remove_file(&path), io_denied) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        },
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// data_dir의 모든 샌드박스 ID.
pub fn ids(data_dir: &Path) -> Result<Vec<SandboxId>, RuntimeError> {
    let dir = data_dir.join("sandboxes");
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        // 규칙에 맞지 않는 이름(만드는 중인 임시 디렉터리 등)은 건너뛴다.
        if let Some(id) = name.to_str().and_then(|n| SandboxId::new(n).ok()) {
            out.push(id);
        }
    }
    out.sort();
    Ok(out)
}
