//! 이름 붙은 볼륨: data_dir에 두는 디스크 하나와 그 기록. 샌드박스를 지워도 남는다.
//!
//! ```text
//! <data_dir>/volumes/<이름>/
//!   volume.json   VolumeRecord (만들 때 고정: 크기, 라벨, 만든 시각)
//!   disk.img      볼륨 디스크. 만들 때 0으로 채운 새 sparse 파일이다. 게스트가 처음 쓰기로 붙일 때 포맷한다
//!   use.lock      붙인 동안 VM을 소유한 프로세스가 쥐는 잠금. 쓰기는 배타, 읽기 전용은 공유
//! ```
//!
//! **규칙**
//!
//! - 이름은 [`sbx_model::check_volume_name`]을 따른다(경로 구분자, `..`가 없다). 그래서 디렉터리 이름으로 그대로 쓴다.
//! - 만들기는 임시 디렉터리에 다 쓴 뒤 rename한다. 반쯤 만든 볼륨은 목록에 없다. 같은 이름이 이미 있으면
//!   [`RuntimeError::VolumeExists`]다(rename이 실패한다. 동시에 만들어도 하나만 된다).
//! - **한 번에 쓰는 쪽 하나**: 블록 장치는 두 커널이 함께 쓸 수 없다. 샌드박스를 시작할 때 볼륨마다 `use.lock`을 쥔다
//!   ([`hold`]). 쓰기로 붙이면 배타 잠금, 읽기 전용이면 공유 잠금이다. 그래서 쓰는 쪽이 있으면 아무도 더 붙이지 못하고,
//!   읽는 쪽들이 있으면 쓰는 쪽이 붙지 못한다. 잠금은 OS의 파일 잠금(`flock`, Windows `LockFileEx`)이라 그 프로세스가
//!   죽으면 풀린다(`owner.lock`과 같다). 그래서 강제로 죽은 소유 프로세스가 볼륨을 붙잡아 두지 않는다.
//!   잠금은 다른 쪽이 잠깐 쥘 수 있으므로(지우기가 쓰는지 보는 순간, fork한 자식이 exec 전까지 물려받은 동안) 한 번
//!   실패로 판단하지 않고 [`HOLD_WAIT`]까지 다시 해 본다.
//! - **VM이 잠금보다 오래 사는 틈**: 소유 프로세스가 죽으면 잠금은 곧바로 풀리지만 VM은 실행기가 끌 때까지 조금 더
//!   산다. 그래서 잠금을 쥔 뒤, 같은 볼륨과 겹치는(둘 중 하나가 쓰는) 다른 샌드박스의 VM이 아직 사는지 기록의 제어
//!   소켓으로 보고, 살아 있으면 [`VM_GONE_WAIT`]까지 기다린 뒤에도 살아 있으면 거부한다(`Runtime::start`).
//! - **지우기**: 배타 잠금을 쥘 수 있을 때만(아무도 붙이지 않았을 때) 이름이 아닌 임시 이름으로 rename하고 지운다.
//!   샌드박스 기록이 가리키는 볼륨인지는 부르는 쪽(`Runtime::remove_volume`)이 먼저 본다.
//! - 호스트는 디스크 내용을 해석하지 않는다. 다만 1024바이트 자리에 EROFS 매직이 있는 디스크는 붙이지 않는다
//!   ([`looks_like_layer`]). 게스트는 EROFS가 아닌 첫 디스크에서 층 세기를 멈추므로, 그런 볼륨이 층으로 읽히면 안 된다
//!   (게스트가 볼륨 장치에 직접 쓸 수 있으므로 붙일 때마다 본다. 정상인 ext4는 그 자리가 inode 수다).
//! - 기록은 비밀일 수 있다(볼륨 내용). 디렉터리는 0700, 파일은 0600이다.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::RuntimeError;
use crate::records::{atomic_write, private_dir, private_file, read_json, temp, to_json};

/// 볼륨 디스크의 가장 작은 크기(MiB). 게스트가 그 위에 저널이 있는 ext4를 만든다.
pub const MIN_SIZE_MIB: u32 = 64;
/// 볼륨 디스크의 가장 큰 크기(MiB, 16 TiB 아래). 게스트의 포맷이 블록 번호를 32비트로 쓴다.
pub const MAX_SIZE_MIB: u32 = 16 * 1024 * 1024 - 1;
/// 시작할 때 볼륨 잠금을 얻으려고 기다리는 시간(다른 쪽이 잠깐 쥐는 경우).
pub const HOLD_WAIT: Duration = Duration::from_secs(1);

/// 볼륨을 붙이기 전에, 소유 프로세스가 죽은 다른 샌드박스의 VM이 꺼지기를 기다리는 시간. 실행기는 표준 입력이 닫히면
/// VM을 끈다(실행기 규약 5). 보통 바로 끝나지만 그사이 두 VM이 한 디스크에 쓰면 안 된다.
pub const VM_GONE_WAIT: Duration = Duration::from_secs(3);

/// 볼륨 이벤트의 종류. 동작은 `create`, `remove`이고 ID는 볼륨 이름이다.
pub const VOLUME_EVENT_KIND: &str = "volume";

/// `data_dir`의 볼륨 이벤트 기록.
pub fn volume_events_log(data_dir: &Path) -> sbx_events::Log {
    sbx_events::Log::new(data_dir.join("events").join("volumes"))
}

/// `volume.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRecord {
    /// 디스크 크기(MiB).
    pub size_mib: u32,
    /// 라벨.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    /// 만든 시각(Unix 시각, 나노초).
    pub created_ns: u64,
}

/// 볼륨 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// 이름.
    pub name: String,
    /// 기록.
    pub record: VolumeRecord,
    /// 디렉터리.
    pub dir: PathBuf,
    /// 호스트에서 디스크가 실제로 차지하는 바이트(sparse 파일의 할당된 블록).
    pub allocated_bytes: u64,
}

impl VolumeInfo {
    /// 디스크 파일.
    pub fn disk(&self) -> PathBuf {
        self.dir.join(DISK)
    }

    /// 만든 시각.
    pub fn created(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.record.created_ns)
    }
}

const RECORD: &str = "volume.json";
const DISK: &str = "disk.img";
const LOCK: &str = "use.lock";

fn root(data_dir: &Path) -> PathBuf {
    data_dir.join("volumes")
}

fn dir(data_dir: &Path, name: &str) -> PathBuf {
    root(data_dir).join(name)
}

fn check_name(name: &str) -> Result<(), RuntimeError> {
    sbx_model::check_volume_name(name).map_err(RuntimeError::InvalidName)
}

/// 파일이 호스트에서 차지하는 바이트. Unix는 할당된 블록, 다른 곳은 길이다.
fn allocated(path: &Path) -> u64 {
    match fs::metadata(path) {
        #[cfg(unix)]
        Ok(m) => std::os::unix::fs::MetadataExt::blocks(&m).saturating_mul(512),
        #[cfg(not(unix))]
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// 이름의 볼륨. 없거나 만드는 중이면 `None`.
pub fn get(data_dir: &Path, name: &str) -> Result<Option<VolumeInfo>, RuntimeError> {
    check_name(name)?;
    let d = dir(data_dir, name);
    let record: VolumeRecord = match read_json(&d.join(RECORD)) {
        Ok(r) => r,
        Err(RuntimeError::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    Ok(Some(VolumeInfo {
        name: name.to_owned(),
        record,
        allocated_bytes: allocated(&d.join(DISK)),
        dir: d,
    }))
}

/// 모든 볼륨. 이름 순서. 만드는 중이거나 지우는 중인 것(이름 규칙에 맞지 않는 디렉터리), 읽을 수 없는 기록은 건너뛴다.
pub fn list(data_dir: &Path) -> Result<Vec<VolumeInfo>, RuntimeError> {
    let entries = match fs::read_dir(root(data_dir)) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        if let Some(n) = entry?.file_name().to_str()
            && sbx_model::check_volume_name(n).is_ok()
        {
            names.push(n.to_owned());
        }
    }
    names.sort();
    let mut out = Vec::new();
    for n in names {
        if let Ok(Some(v)) = get(data_dir, &n) {
            out.push(v);
        }
    }
    Ok(out)
}

/// 만든다. 디스크는 `size_mib` 크기의 0으로 채운 새 sparse 파일이다. 이미 있으면 [`RuntimeError::VolumeExists`].
pub fn create(
    data_dir: &Path,
    name: &str,
    size_mib: u32,
    labels: BTreeMap<String, String>,
) -> Result<VolumeInfo, RuntimeError> {
    check_name(name)?;
    if !(MIN_SIZE_MIB..=MAX_SIZE_MIB).contains(&size_mib) {
        return Err(RuntimeError::Invalid(format!(
            "volume size must be between {MIN_SIZE_MIB}m and {}g",
            MAX_SIZE_MIB / 1024
        )));
    }
    let parent = root(data_dir);
    private_dir(&parent)?;
    let final_dir = dir(data_dir, name);
    if final_dir.exists() {
        return Err(RuntimeError::VolumeExists(name.to_owned()));
    }
    let staging = temp(&parent, name);
    let created_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let record = VolumeRecord {
        size_mib,
        labels,
        created_ns,
    };
    let r = (|| -> Result<(), RuntimeError> {
        private_dir(&staging)?;
        atomic_write(&staging.join(RECORD), &to_json(&record)?)?;
        private_file(&staging.join(DISK))?.set_len(u64::from(size_mib) << 20)?;
        private_file(&staging.join(LOCK))?;
        match fs::rename(&staging, &final_dir) {
            Ok(()) => Ok(()),
            Err(_) if final_dir.exists() => Err(RuntimeError::VolumeExists(name.to_owned())),
            Err(e) => Err(e.into()),
        }
    })();
    if r.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    r?;
    get(data_dir, name)?.ok_or_else(|| RuntimeError::VolumeNotFound(name.to_owned()))
}

/// 붙인 동안 쥐는 잠금. 버리면(소유 프로세스가 끝나도) 풀린다.
#[derive(Debug)]
pub struct Hold {
    _file: File,
}

fn open_lock(data_dir: &Path, name: &str) -> Result<File, RuntimeError> {
    // 읽기와 쓰기로 연다: Windows의 `LockFileEx`는 덧붙이기 전용 핸들을 거부한다.
    let path = dir(data_dir, name).join(LOCK);
    match OpenOptions::new()
        .read(true)
        .write(true)
        .create(false)
        .open(&path)
    {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Err(RuntimeError::VolumeNotFound(name.to_owned()))
        }
        Err(e) => Err(e.into()),
    }
}

/// 잠금을 한 번 시도한다. `readonly`면 공유, 아니면 배타. 누가 맞지 않게 쥐고 있으면 `None`.
pub fn try_hold(data_dir: &Path, name: &str, readonly: bool) -> Result<Option<Hold>, RuntimeError> {
    let file = open_lock(data_dir, name)?;
    let r = if readonly {
        file.try_lock_shared()
    } else {
        file.try_lock()
    };
    match r {
        Ok(()) => Ok(Some(Hold { _file: file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e.into()),
    }
}

/// [`HOLD_WAIT`]까지 잠금을 시도한다(모듈 문서의 "한 번에 쓰는 쪽 하나").
pub fn hold(data_dir: &Path, name: &str, readonly: bool) -> Result<Option<Hold>, RuntimeError> {
    hold_within(data_dir, name, readonly, HOLD_WAIT)
}

fn hold_within(
    data_dir: &Path,
    name: &str,
    readonly: bool,
    wait: Duration,
) -> Result<Option<Hold>, RuntimeError> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        if let Some(h) = try_hold(data_dir, name, readonly)? {
            return Ok(Some(h));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// 지금 어느 샌드박스가 붙이고 있는지(배타 잠금을 잠깐 쥐어 본다). 보여 주기에만 쓴다. fork한 자식이 잠깐 물려받은
/// 잠금으로 틀리지 않게 조금(100ms) 다시 해 본다.
pub fn in_use(data_dir: &Path, name: &str) -> Result<bool, RuntimeError> {
    Ok(hold_within(data_dir, name, false, Duration::from_millis(100))?.is_none())
}

/// 디스크가 층(EROFS)처럼 보이는지: 1024바이트 자리의 4바이트가 EROFS 매직이다. 그런 볼륨은 붙이지 않는다(모듈 문서).
pub fn looks_like_layer(disk: &Path) -> io::Result<bool> {
    const EROFS_MAGIC: u32 = 0xE0F5_E1E2;
    let mut f = File::open(disk)?;
    let mut m = [0u8; 4];
    f.seek(SeekFrom::Start(1024))?;
    match f.read_exact(&mut m) {
        Ok(()) => Ok(u32::from_le_bytes(m) == EROFS_MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// 지운다. 붙인 샌드박스가 있으면 [`RuntimeError::VolumeBusy`]. 샌드박스 기록이 가리키는지는 부르는 쪽이 본다.
pub fn remove(data_dir: &Path, name: &str) -> Result<(), RuntimeError> {
    check_name(name)?;
    let d = dir(data_dir, name);
    let Some(held) = hold(data_dir, name, false)? else {
        return Err(RuntimeError::VolumeBusy {
            name: name.to_owned(),
            holder: None,
        });
    };
    let doomed = temp(&root(data_dir), &format!("{name}.rm"));
    let r = fs::rename(&d, &doomed);
    // 이름으로는 이제 찾을 수 없다. Windows는 열린 파일을 지울 수 없으므로 잠금을 놓고 지운다.
    drop(held);
    r?;
    fs::remove_dir_all(&doomed)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sbx-volume-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn create_list_get_and_remove() {
        let d = data_dir("crud");
        assert!(list(&d).unwrap().is_empty());
        let labels = BTreeMap::from([("app".to_owned(), "db".to_owned())]);
        let v = create(&d, "pgdata", 128, labels.clone()).unwrap();
        assert_eq!((v.record.size_mib, &v.record.labels), (128, &labels));
        assert_eq!(fs::metadata(v.disk()).unwrap().len(), 128 << 20);
        // sparse: 호스트에서는 거의 차지하지 않는다.
        assert!(v.allocated_bytes < 1 << 20, "{}", v.allocated_bytes);
        assert!(matches!(
            create(&d, "pgdata", 128, BTreeMap::new()),
            Err(RuntimeError::VolumeExists(n)) if n == "pgdata"
        ));
        create(&d, "cache", 64, BTreeMap::new()).unwrap();
        let names: Vec<String> = list(&d).unwrap().into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["cache", "pgdata"]);
        assert!(!looks_like_layer(&v.disk()).unwrap());
        remove(&d, "pgdata").unwrap();
        assert!(get(&d, "pgdata").unwrap().is_none());
        assert!(matches!(
            remove(&d, "pgdata"),
            Err(RuntimeError::VolumeNotFound(_))
        ));
        // 지우다 남은 디렉터리와 만드는 중인 디렉터리는 목록에 없다.
        fs::create_dir_all(root(&d).join(".x.tmp-1-2")).unwrap();
        assert_eq!(list(&d).unwrap().len(), 1);
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn names_and_sizes_are_checked() {
        let d = data_dir("names");
        for bad in ["..", "a/b", "x", "-v"] {
            assert!(matches!(
                create(&d, bad, 64, BTreeMap::new()),
                Err(RuntimeError::InvalidName(_))
            ));
        }
        assert!(create(&d, "small", 63, BTreeMap::new()).is_err());
        let _ = fs::remove_dir_all(d);
    }

    /// 쓰기는 하나만, 읽기 전용은 쓰는 쪽이 없을 때 여럿. 잠금을 놓으면(소유 프로세스가 끝나면) 다시 된다.
    #[test]
    fn one_writer_or_many_readers() {
        let d = data_dir("locks");
        create(&d, "v1", 64, BTreeMap::new()).unwrap();
        let w = try_hold(&d, "v1", false).unwrap().expect("first writer");
        assert!(try_hold(&d, "v1", false).unwrap().is_none());
        assert!(try_hold(&d, "v1", true).unwrap().is_none());
        assert!(in_use(&d, "v1").unwrap());
        assert!(matches!(
            remove(&d, "v1"),
            Err(RuntimeError::VolumeBusy { .. })
        ));
        drop(w);
        // 다른 테스트가 fork한 자식이 잠깐 물려받을 수 있으므로 되는 쪽은 기다리며 본다(`hold`).
        assert!(!in_use(&d, "v1").unwrap());
        let r1 = hold(&d, "v1", true).unwrap().expect("reader");
        let r2 = hold(&d, "v1", true).unwrap().expect("second reader");
        assert!(try_hold(&d, "v1", false).unwrap().is_none());
        drop((r1, r2));
        assert!(hold(&d, "v1", false).unwrap().is_some());
        assert!(matches!(
            try_hold(&d, "nope", false),
            Err(RuntimeError::VolumeNotFound(_))
        ));
        fs::remove_dir_all(d).unwrap();
    }

    #[test]
    fn an_erofs_disk_is_never_a_volume() {
        let d = data_dir("erofs");
        let v = create(&d, "v1", 64, BTreeMap::new()).unwrap();
        let mut bytes = vec![0u8; 4096];
        bytes[1024..1028].copy_from_slice(&0xE0F5_E1E2u32.to_le_bytes());
        fs::write(v.disk(), &bytes).unwrap();
        assert!(looks_like_layer(&v.disk()).unwrap());
        fs::remove_dir_all(d).unwrap();
    }
}
