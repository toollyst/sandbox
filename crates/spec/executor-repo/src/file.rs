//! 목록 파일(저장소 목록, 본 serial)을 고쳐 쓰는 법. 가장 아래 모듈이다.
//!
//! - 읽고 고쳐 쓰는 동안 `<파일>.lock`을 잠근다([`locked`]). 프로세스 둘이 동시에 고치면 한쪽의 기록이 사라질 수
//!   있는데(본 serial이면 롤백 창이 다시 열린다), 잠금이 그것을 막는다. 프로세스가 죽으면 커널이 잠금을 푼다.
//! - 새 내용은 같은 디렉터리의 임시 파일(`.<이름>.tmp-<pid>-<n>`, 파일마다 다른 이름)에 쓰고 rename한다
//!   ([`replace`]). 읽는 쪽은 늘 옛 내용이나 새 내용 전체를 본다.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP: AtomicU64 = AtomicU64::new(0);

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(".{name}{suffix}"))
}

/// `path`를 고치는 동안 잠근다. 디렉터리가 없으면 만든다.
pub fn locked<T, E: From<io::Error>>(
    path: &Path,
    f: impl FnOnce() -> Result<T, E>,
) -> Result<T, E> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(sibling(path, ".lock"))?;
    lock.lock()?;
    let r = f();
    drop(lock);
    r
}

/// `path`의 내용을 `text`로 바꾼다(임시 파일에 쓰고 rename).
pub fn replace(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = sibling(
        path,
        &format!(
            ".tmp-{}-{}",
            std::process::id(),
            TMP.fetch_add(1, Ordering::Relaxed)
        ),
    );
    fs::write(&tmp, text)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}
