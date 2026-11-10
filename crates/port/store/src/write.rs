//! 캐시를 바꾸는 API. `write` feature가 켜졌을 때만 있다. 관리(`sbx-manage`)만 쓴다.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sbx_model::{Digest, ImageId};

use crate::digest::HashingWriter;
use crate::{CacheLayout, ImageRecord, Store, StoreError};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 같은 디렉터리 안의 임시 파일 경로. 이름이 겹치지 않는다.
pub fn temp_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 이미지 기록을 쓴다. 이미지 디렉터리와 잠금 파일도 만든다.
pub fn write_image_record(layout: &CacheLayout, record: &ImageRecord) -> io::Result<()> {
    create_image_dir(layout, &record.id)?;
    let json = serde_json::to_vec_pretty(record).map_err(io::Error::other)?;
    atomic_write(&layout.image_record(&record.id), &json)
}

fn with_refs<T>(
    layout: &CacheLayout,
    f: impl FnOnce(&mut BTreeMap<String, ImageId>) -> T,
) -> Result<T, StoreError> {
    fs::create_dir_all(layout.root())?;
    let lock = open_lock_file(&layout.cache_lock())?;
    lock.lock()?;
    let mut refs = Store::open(layout.root()).refs()?;
    let out = f(&mut refs);
    let json = serde_json::to_vec_pretty(&refs).map_err(io::Error::other)?;
    atomic_write(&layout.refs(), &json)?;
    Ok(out)
}

/// 참조가 이미지를 가리키게 한다. 같은 참조가 다른 이미지를 가리키고 있었으면 바꾼다(Docker와 같다).
pub fn set_ref(layout: &CacheLayout, reference: &str, id: &ImageId) -> Result<(), StoreError> {
    with_refs(layout, |refs| {
        refs.insert(reference.to_owned(), id.clone());
    })
}

/// 참조 하나를 지운다. 지웠으면 그 참조가 가리키던 이미지를 돌려준다.
pub fn remove_ref(layout: &CacheLayout, reference: &str) -> Result<Option<ImageId>, StoreError> {
    with_refs(layout, |refs| refs.remove(reference))
}

/// 이미지를 가리키는 참조를 모두 지운다. 지운 참조를 돌려준다.
pub fn remove_refs_to(layout: &CacheLayout, id: &ImageId) -> Result<Vec<String>, StoreError> {
    with_refs(layout, |refs| {
        let removed: Vec<String> = refs
            .iter()
            .filter(|(_, v)| *v == id)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &removed {
            refs.remove(k);
        }
        removed
    })
}

/// blob을 받는 쓰기. 다 쓴 뒤 [`BlobWriter::commit`]이 digest를 대조하고, 맞을 때만 제자리에 놓는다.
#[derive(Debug)]
pub struct BlobWriter {
    temp: PathBuf,
    file: Option<HashingWriter<File>>,
}

impl BlobWriter {
    /// blobs 디렉터리에 임시 파일을 만든다.
    pub fn create(layout: &CacheLayout) -> io::Result<Self> {
        let dir = layout.blobs_dir();
        fs::create_dir_all(&dir)?;
        let temp = temp_path(&dir, "blob");
        let file = File::create(&temp)?;
        Ok(Self {
            temp,
            file: Some(HashingWriter::new(file)),
        })
    }

    /// 쓴 데이터의 digest가 `expected`와 같으면 제자리에 놓는다. 다르면 지우고 `InvalidData` 오류.
    pub fn commit(mut self, layout: &CacheLayout, expected: &Digest) -> io::Result<u64> {
        let file = self
            .file
            .take()
            .ok_or_else(|| io::Error::other("blob writer already finished"))?;
        let actual = file.digest();
        let bytes = file.byte_count();
        let file = file.into_inner();
        file.sync_all()?;
        drop(file);
        if &actual != expected {
            let _ = fs::remove_file(&self.temp);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("blob digest mismatch: expected {expected}, got {actual}"),
            ));
        }
        fs::rename(&self.temp, layout.blob(expected))?;
        Ok(bytes)
    }
}

impl Write for BlobWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.file {
            Some(f) => f.write(buf),
            None => Err(io::Error::other("blob writer already finished")),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.file {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for BlobWriter {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::remove_file(&self.temp);
        }
    }
}

/// 작업 공간 디렉터리 하나. 버려지면 지워진다.
#[derive(Debug)]
pub struct WorkDir {
    path: PathBuf,
}

impl WorkDir {
    /// cache_dir의 `tmp/` 아래에 새로 만든다.
    pub fn create(layout: &CacheLayout, name: &str) -> io::Result<Self> {
        let tmp = layout.tmp_dir();
        fs::create_dir_all(&tmp)?;
        let path = temp_path(&tmp, name);
        fs::create_dir(&path)?;
        Ok(Self { path })
    }

    /// 경로.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// 임시 파일에 다 쓴 뒤 rename한다. 읽는 쪽은 완성된 파일만 본다.
///
/// 같은 경로를 여러 프로세스가 동시에 쓰면 마지막 rename이 남는다. 내용 주소 파일은 내용이 같으므로 문제없다.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = temp_path(dir, &name.to_string_lossy());
    let result = (|| {
        let mut f = File::create(&temp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// 이미지 디렉터리와 잠금 파일을 만든다. 이미 있으면 그대로 둔다.
pub fn create_image_dir(layout: &CacheLayout, id: &ImageId) -> io::Result<()> {
    fs::create_dir_all(layout.image_dir(id))?;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(layout.image_lock(id))?;
    Ok(())
}

/// 이미지 배타 잠금. 살아 있는 동안 새 lease가 기다린다.
#[derive(Debug)]
pub struct ExclusiveImageLock {
    file: File,
}

/// 배타 잠금을 시도한다. 누가 lease를 쥐고 있으면(실행 중인 샌드박스가 있으면) `None`.
pub fn try_lock_image(
    layout: &CacheLayout,
    id: &ImageId,
) -> io::Result<Option<ExclusiveImageLock>> {
    let file = File::open(layout.image_lock(id))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(ExclusiveImageLock { file })),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// 이미지를 지운다. 배타 잠금을 쥔 상태여야 한다.
///
/// 순서: 잠금을 쥔 채 잠금 파일 외의 내용을 지운다 → 잠금을 놓는다 → 잠금 파일과 디렉터리를 지운다.
/// 그사이 lease를 기다리던 쪽은 잠금을 잡은 뒤 이미지 기록이 없음을 보고 `ImageNotFound`를 받는다.
pub fn remove_image(
    layout: &CacheLayout,
    id: &ImageId,
    lock: ExclusiveImageLock,
) -> io::Result<()> {
    let dir = layout.image_dir(id);
    let lock_path = layout.image_lock(id);
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path == lock_path {
            continue;
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    drop(lock.file);
    fs::remove_file(&lock_path)?;
    fs::remove_dir(&dir)
}

/// 잠금 파일을 연다(없으면 만든다). 읽기와 쓰기로 연다: Windows의 `LockFileEx`는 읽기나 쓰기 권한이 있는 핸들을
/// 요구하고, 덧붙이기 전용 핸들(`append`)에는 쓰기 권한이 없어 "Access is denied"가 난다. 내용은 쓰지 않는다.
fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// 진행 중인 pull이 쓸 blob 목록. 살아 있는 동안 [`collect_blobs`]가 이 blob들을 지우지 않는다.
#[derive(Debug)]
pub struct BlobClaim {
    file: Option<File>,
    path: PathBuf,
}

impl Drop for BlobClaim {
    fn drop(&mut self) {
        // 잠금을 먼저 놓고 지운다. 그 사이 정리가 표시를 오래된 것으로 보고 지워도 된다.
        // 그때는 이미 이미지 기록이 blob을 가리키고 있다.
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

/// pull을 시작할 때 부른다. 돌려받은 표시를 이미지 기록을 쓴 뒤까지 쥐고 있어야 한다.
///
/// 순서가 보장하는 것: blob이 있는지 확인하는 일은 이 함수가 끝난 뒤에 한다. 그러면 정리는
/// (1) 표시가 생기기 전에 끝났거나(그러면 blob이 없다고 보고 다시 받는다), (2) 표시를 보고 그 blob을 남긴다.
/// 표시를 쓰고 공유 잠금을 거는 동안 `blobs/.lock`을 공유로 쥐므로 정리가 그 사이에 끼지 않는다.
/// 다운로드 동안에는 `blobs/.lock`을 쥐지 않는다. 그래서 오래 걸리는 pull이 `rmi`를 막지 않는다.
///
/// 프로세스가 죽으면 표시 파일이 남지만 잠금은 풀린다. 정리가 그런 표시를 지운다.
pub fn claim_blobs(layout: &CacheLayout, blobs: &[Digest]) -> io::Result<BlobClaim> {
    let gate = open_lock_file(&layout.blobs_lock())?;
    gate.lock_shared()?;
    let dir = layout.pulls_dir();
    fs::create_dir_all(&dir)?;
    let path = temp_path(&dir, "pull");
    let json = serde_json::to_vec(blobs).map_err(io::Error::other)?;
    // Windows에서는 공유 잠금이 쓰기를 막으므로 먼저 다 쓰고 잠근다.
    fs::write(&path, &json)?;
    let file = File::open(&path)?;
    file.lock_shared()?;
    drop(gate);
    Ok(BlobClaim {
        file: Some(file),
        path,
    })
}

/// 어떤 이미지 기록도, 살아 있는 pull 표시도 가리키지 않는 blob을 지운다. 지운 blob을 돌려준다.
///
/// - `blobs/.lock`을 배타로 쥔 채 참조를 모으고 지운다.
/// - 이미지 기록을 하나라도 읽지 못하면 아무것도 지우지 않고 오류를 낸다. 그 기록이 가리키는 blob을 모르기 때문이다.
/// - 배타 잠금을 잡을 수 있는 pull 표시는 죽은 pull이 남긴 것이다. 지우고 참조로 세지 않는다.
/// - 이름이 `.`으로 시작하는 파일(받는 중인 임시 파일)은 건드리지 않는다.
/// - prepare 중인 이미지는 이미지 기록이 있으므로 그 blob이 남는다.
pub fn collect_blobs(layout: &CacheLayout) -> Result<Vec<Digest>, StoreError> {
    let gate = open_lock_file(&layout.blobs_lock())?;
    gate.lock()?;

    let mut keep = std::collections::BTreeSet::new();
    for record in Store::open(layout.root()).images()? {
        keep.extend(record.layers.into_iter().map(|l| l.digest));
    }
    if let Some(entries) = read_dir_if_exists(&layout.pulls_dir())? {
        for entry in entries {
            let path = entry?.path();
            let file = match File::open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            match file.try_lock() {
                Ok(()) => {
                    drop(file);
                    let _ = fs::remove_file(&path);
                }
                Err(TryLockError::WouldBlock) => {
                    let bytes = fs::read(&path)?;
                    let claimed: Vec<Digest> =
                        serde_json::from_slice(&bytes).map_err(|e| StoreError::Corrupt {
                            path: path.clone(),
                            reason: e.to_string(),
                        })?;
                    keep.extend(claimed);
                }
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }

    let mut removed = Vec::new();
    if let Some(entries) = read_dir_if_exists(&layout.blobs_dir())? {
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(digest) = Digest::parse(&format!("sha256:{name}")) else {
                continue;
            };
            if !keep.contains(&digest) {
                fs::remove_file(entry.path())?;
                removed.push(digest);
            }
        }
    }
    removed.sort();
    Ok(removed)
}

fn read_dir_if_exists(dir: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::read_dir(dir) {
        Ok(e) => Ok(Some(e)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LeaseError, Store};
    use sbx_model::{Digest, RootfsFormat};
    use std::path::PathBuf;

    fn temp_root(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("sandbox-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    fn image() -> ImageId {
        ImageId::new(Digest::parse(&format!("sha256:{}", "cd".repeat(32))).unwrap())
    }

    fn prepared(root: &Path) -> (Store, ImageId) {
        let store = Store::open(root);
        let id = image();
        create_image_dir(store.layout(), &id).unwrap();
        write_image_record(store.layout(), &record_with(&[])).unwrap();
        atomic_write(&store.layout().rootfs(&id, RootfsFormat::Erofs), b"fs").unwrap();
        (store, id)
    }

    #[test]
    fn blob_is_placed_only_when_digest_matches() {
        let root = temp_root("blob");
        let layout = CacheLayout::new(&root);
        let good = crate::digest::sha256(b"layer");

        let mut w = BlobWriter::create(&layout).unwrap();
        w.write_all(b"tampered").unwrap();
        let err = w.commit(&layout, &good).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!layout.blob(&good).exists());

        let mut w = BlobWriter::create(&layout).unwrap();
        w.write_all(b"layer").unwrap();
        assert_eq!(w.commit(&layout, &good).unwrap(), 5);
        assert_eq!(fs::read(layout.blob(&good)).unwrap(), b"layer");

        // 임시 파일이 남지 않는다.
        assert_eq!(fs::read_dir(layout.blobs_dir()).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn dropped_blob_writer_leaves_nothing() {
        let root = temp_root("blobdrop");
        let layout = CacheLayout::new(&root);
        let mut w = BlobWriter::create(&layout).unwrap();
        w.write_all(b"partial").unwrap();
        drop(w);
        assert_eq!(fs::read_dir(root.join("blobs/sha256")).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn refs_set_replace_and_remove() {
        let root = temp_root("refs");
        let layout = CacheLayout::new(&root);
        let store = Store::open(&root);
        let a = image();
        let b = ImageId::new(Digest::parse(&format!("sha256:{}", "ee".repeat(32))).unwrap());

        set_ref(&layout, "docker.io/library/alpine:latest", &a).unwrap();
        set_ref(&layout, "docker.io/library/alpine:3", &a).unwrap();
        set_ref(&layout, "docker.io/library/alpine:latest", &b).unwrap();
        let refs = store.refs().unwrap();
        assert_eq!(refs["docker.io/library/alpine:latest"], b);
        assert_eq!(refs["docker.io/library/alpine:3"], a);

        assert_eq!(
            remove_refs_to(&layout, &a).unwrap(),
            vec!["docker.io/library/alpine:3"]
        );
        assert_eq!(
            remove_ref(&layout, "docker.io/library/alpine:latest").unwrap(),
            Some(b)
        );
        assert!(store.refs().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn work_dir_is_removed_on_drop() {
        let root = temp_root("work");
        let layout = CacheLayout::new(&root);
        let work = WorkDir::create(&layout, "prepare").unwrap();
        fs::write(work.path().join("x"), b"1").unwrap();
        let path = work.path().to_owned();
        drop(work);
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let root = temp_root("atomic");
        let path = root.join("a/b.txt");
        atomic_write(&path, b"one").unwrap();
        atomic_write(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert_eq!(fs::read_dir(root.join("a")).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lease_blocks_exclusive_lock() {
        let root = temp_root("lease");
        let (store, id) = prepared(&root);
        let lease = store.lease(&id, RootfsFormat::Erofs).unwrap();
        assert!(try_lock_image(store.layout(), &id).unwrap().is_none());
        drop(lease);
        assert!(try_lock_image(store.layout(), &id).unwrap().is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lease_needs_the_image_kernel_when_the_image_has_one() {
        let root = temp_root("kernel");
        let (store, id) = prepared(&root);
        assert_eq!(
            store.lease(&id, RootfsFormat::Erofs).unwrap().kernel(),
            None
        );
        let mut r = record_with(&[]);
        r.config.labels.insert(
            sbx_model::ImageConfig::KERNEL_LABEL.into(),
            "/boot/vmlinux".into(),
        );
        write_image_record(store.layout(), &r).unwrap();
        assert!(matches!(
            store.lease(&id, RootfsFormat::Erofs),
            Err(LeaseError::KernelNotFound)
        ));
        atomic_write(&store.layout().kernel(&id), b"k").unwrap();
        let lease = store.lease(&id, RootfsFormat::Erofs).unwrap();
        assert_eq!(lease.kernel(), Some(store.layout().kernel(&id).as_path()));
        drop(lease);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removed_image_cannot_be_leased() {
        let root = temp_root("remove");
        let (store, id) = prepared(&root);
        let lock = try_lock_image(store.layout(), &id).unwrap().unwrap();
        remove_image(store.layout(), &id, lock).unwrap();
        assert!(!store.layout().image_dir(&id).exists());
        assert!(matches!(
            store.lease(&id, RootfsFormat::Erofs),
            Err(LeaseError::ImageNotFound)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    fn put_blob(layout: &CacheLayout, data: &[u8]) -> Digest {
        let d = crate::digest::sha256(data);
        let mut w = BlobWriter::create(layout).unwrap();
        w.write_all(data).unwrap();
        w.commit(layout, &d).unwrap();
        d
    }

    fn record_with(layers: &[&Digest]) -> ImageRecord {
        use sbx_model::{Arch, Compression, ImageConfig, Layer};
        ImageRecord {
            id: image(),
            reference: "docker.io/library/x:latest".into(),
            source: "registry-1.docker.io/library/x".into(),
            arch: Arch::X86_64,
            config: ImageConfig::default(),
            layers: layers
                .iter()
                .map(|d| Layer {
                    digest: (*d).clone(),
                    diff_id: (*d).clone(),
                    size: 1,
                    compression: Compression::None,
                })
                .collect(),
            ignored_fields: Vec::new(),
        }
    }

    #[test]
    fn collect_keeps_blobs_referenced_by_records() {
        let root = temp_root("gc-records");
        let layout = CacheLayout::new(&root);
        let used = put_blob(&layout, b"used");
        let unused = put_blob(&layout, b"unused");
        write_image_record(&layout, &record_with(&[&used])).unwrap();

        assert_eq!(collect_blobs(&layout).unwrap(), vec![unused.clone()]);
        assert!(layout.blob(&used).is_file());
        assert!(!layout.blob(&unused).exists());
        assert!(collect_blobs(&layout).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn live_claim_protects_blobs_until_dropped() {
        let root = temp_root("gc-claim");
        let layout = CacheLayout::new(&root);
        let blob = put_blob(&layout, b"shared layer");

        // pull이 "이미 있다"고 보고 받기를 건너뛴 상태. 아직 이미지 기록은 없다.
        let claim = claim_blobs(&layout, std::slice::from_ref(&blob)).unwrap();
        assert!(collect_blobs(&layout).unwrap().is_empty());
        assert!(layout.blob(&blob).is_file());

        drop(claim);
        assert_eq!(fs::read_dir(layout.pulls_dir()).unwrap().count(), 0);
        assert_eq!(collect_blobs(&layout).unwrap(), vec![blob]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn claims_from_dead_pulls_are_discarded() {
        let root = temp_root("gc-stale");
        let layout = CacheLayout::new(&root);
        let blob = put_blob(&layout, b"orphan");
        // 죽은 pull이 남긴 표시: 내용은 있지만 아무도 잠그지 않았다.
        fs::create_dir_all(layout.pulls_dir()).unwrap();
        let stale = layout.pulls_dir().join(".pull.tmp-1-0");
        fs::write(&stale, serde_json::to_vec(&[&blob]).unwrap()).unwrap();

        assert_eq!(collect_blobs(&layout).unwrap(), vec![blob]);
        assert!(!stale.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collect_leaves_partial_downloads_alone() {
        let root = temp_root("gc-partial");
        let layout = CacheLayout::new(&root);
        let mut w = BlobWriter::create(&layout).unwrap();
        w.write_all(b"half").unwrap();
        assert!(collect_blobs(&layout).unwrap().is_empty());
        assert_eq!(fs::read_dir(layout.blobs_dir()).unwrap().count(), 1);
        drop(w);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn collect_refuses_when_a_record_is_unreadable() {
        let root = temp_root("gc-corrupt");
        let layout = CacheLayout::new(&root);
        let blob = put_blob(&layout, b"maybe used");
        create_image_dir(&layout, &image()).unwrap();
        atomic_write(&layout.image_record(&image()), b"not json").unwrap();

        assert!(matches!(
            collect_blobs(&layout),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(layout.blob(&blob).is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lease_reports_missing_rootfs_format() {
        let root = temp_root("noformat");
        let store = Store::open(&root);
        let id = image();
        create_image_dir(store.layout(), &id).unwrap();
        atomic_write(&store.layout().image_record(&id), b"{}").unwrap();
        assert!(matches!(
            store.lease(&id, RootfsFormat::Erofs),
            Err(LeaseError::RootfsNotFound)
        ));
        fs::remove_dir_all(root).unwrap();
    }
}
