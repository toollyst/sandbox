//! 이름 붙은 볼륨을 붙인다 (`HostMsg::Volume`).
//!
//! 볼륨은 호스트의 sparse 디스크 파일 하나이고, 실행기가 층 디스크 뒤에 virtio-blk로 붙인다(`sbx-executor-proto`
//! 규약 3, 역할 `volume`). 부팅은 볼륨을 모른다. 층은 EROFS인 디스크만 세고 처음으로 EROFS가 아닌 디스크에서
//! 멈추므로(`crate::disks`) 볼륨(ext4)은 층으로 읽히지 않는다. 어느 디스크가 어느 볼륨이고 어디에 마운트할지는
//! 호스트가 부팅 뒤 이 메시지로 알린다. 기본 프로세스보다 먼저다.
//!
//! **붙이기** ([`Attach::attach`], Linux)
//!
//! 1. 디스크 차례가 층 디스크와 scratch 뒤인지 본다([`first_volume_disk`]). 아니면 오류다. 장치가 EROFS여도
//!    오류다(호스트는 그런 볼륨을 붙이지 않는다. 게스트가 볼륨 장치에 직접 쓸 수 있으므로 붙일 때마다 본다).
//! 2. 파일 시스템이 없으면(superblock 매직이 없으면) 처음 쓰는 볼륨이다. 호스트는 볼륨을 0으로 채운 새 sparse 파일로
//!    만든다. 그래서 scratch처럼 0이라고 믿고 저널이 있는 ext4로 포맷한다(`mkfs::Journal::Internal`). 읽기 전용이면
//!    포맷할 수 없으므로 오류다.
//! 3. 잠깐 쓰는 자리([`STAGING`] 아래 디스크 번호)에 ext4로 마운트한다. 쓰기면 `errors=remount-ro`. 읽기 전용이면
//!    읽기 전용으로 마운트하고, 저널을 다시 적용해야 하는 볼륨(강제로 꺼진 VM이 쓴 것)은 장치에 쓸 수 없으므로
//!    저널을 읽지 않고(`noload`) 마운트한다. 그때 보이는 것은 마지막으로 저널에서 디스크로 옮긴 상태다.
//! 4. 쓰기이고 볼륨이 비어 있으면(뿌리에 `lost+found`만 있거나 아무것도 없으면) `lost+found`를 지운다. Docker 볼륨처럼
//!    빈 디렉터리로 보여야 한다(postgres의 `initdb`는 `lost+found`가 있으면 거부한다). 커널은 그것이 없어도 된다.
//!    `copy`이면 이어서 대상 경로의 지금 내용(이미지에 있던 것)을 볼륨으로 복사한다([`copy_up`]). Docker와 같다:
//!    빈 볼륨이면 처음이 아니어도 채운다. 채운 뒤 `sync`한다(강제로 꺼져도 채운 내용이 남게).
//! 5. 대상 경로가 없으면 디렉터리로 만들고(쓰기 층에), 자리를 대상으로 옮긴다(`MS_MOVE`). 대상이 디렉터리가 아니면
//!    오류다.
//!
//! **채우기 규칙** ([`copy_up`]): 디렉터리, 일반 파일, 심볼릭 링크, FIFO, 장치를 복사한다. 소유자(게스트가 root일 때),
//! 권한, 수정 시각을 그대로 둔다. 대상 디렉터리 자체의 소유자와 권한도 볼륨의 뿌리에 옮긴다(Docker와 같다:
//! postgres 이미지의 `/var/lib/postgresql/data`는 postgres 사용자 소유여야 한다). 소켓은 건너뛴다. 하드 링크는 각각
//! 파일로 복사한다. xattr은 옮기지 않는다. 링크는 따라가지 않는다.
//!
//! 이 모듈의 채우기와 빈 볼륨 판단은 파일 시스템 위에서만 돌므로 호스트(Unix)에서 그대로 테스트한다. 마운트는
//! `SANDBOX_PRIVILEGED_TESTS=1`(root와 loop 장치)일 때 실제 커널로 테스트한다.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, lchown, symlink};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::sys;

/// 볼륨을 대상에 옮기기 전에 잠깐 마운트하는 자리(디스크 번호마다 하나). devtmpfs라 쓰기 층에 남지 않는다.
pub const STAGING: &str = "/dev/.sbx-volumes";

/// 층 디스크가 `layers`개일 때 첫 볼륨 디스크의 차례: 맨 아래 층(0), scratch(1), 위 층들(2..=layers) 뒤.
pub fn first_volume_disk(layers: usize) -> u32 {
    u32::try_from(layers.max(1) + 1).unwrap_or(u32::MAX)
}

/// 볼륨이 비었는지: 뿌리에 `lost+found` 말고 아무것도 없다.
pub fn is_empty(root: &Path) -> io::Result<bool> {
    for e in fs::read_dir(root)? {
        if e?.file_name() != "lost+found" {
            return Ok(false);
        }
    }
    Ok(true)
}

fn time(meta: &fs::Metadata) -> SystemTime {
    let (s, n) = (meta.mtime(), meta.mtime_nsec().clamp(0, 999_999_999) as u32);
    if s >= 0 {
        UNIX_EPOCH + Duration::new(s as u64, n)
    } else {
        UNIX_EPOCH - Duration::from_secs(s.unsigned_abs()) + Duration::from_nanos(u64::from(n))
    }
}

/// 소유자를 옮긴다. root가 아니면 옮길 수 없으므로 그대로 둔다(테스트).
fn own(path: &Path, meta: &fs::Metadata) -> io::Result<()> {
    if sys::is_root() {
        lchown(path, Some(meta.uid()), Some(meta.gid()))?;
    }
    Ok(())
}

/// `src` 디렉터리의 내용을 `dst`(빈 볼륨의 뿌리)로 복사하고, `src`의 소유자, 권한, 수정 시각을 `dst`에 옮긴다.
/// 규칙은 모듈 문서의 "채우기 규칙"이다. `src`가 없으면 아무것도 하지 않는다. 디렉터리가 아니면 오류다.
pub fn copy_up(src: &Path, dst: &Path) -> io::Result<()> {
    let meta = match fs::symlink_metadata(src) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", src.display()),
        ));
    }
    copy_children(src, dst)?;
    finish_dir(dst, &meta)
}

/// 디렉터리의 소유자, 권한, 수정 시각을 맞춘다. 안을 다 채운 뒤에 부른다(안에 쓰면 수정 시각이 바뀐다).
/// 볼륨의 뿌리는 마운트한 파일 시스템의 것이라 수정 시각은 가장 나중에 바꾼다.
fn finish_dir(dst: &Path, meta: &fs::Metadata) -> io::Result<()> {
    own(dst, meta)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(meta.mode() & 0o7777))?;
    File::open(dst)?.set_modified(time(meta))
}

fn copy_children(src: &Path, dst: &Path) -> io::Result<()> {
    let mut names: Vec<_> = fs::read_dir(src)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<_>>()?;
    names.sort();
    for name in names {
        let (from, to) = (src.join(&name), dst.join(&name));
        let ctx = |e: io::Error| io::Error::new(e.kind(), format!("{}: {e}", from.display()));
        copy_entry(&from, &to).map_err(ctx)?;
    }
    Ok(())
}

fn copy_entry(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    let ft = meta.file_type();
    if ft.is_dir() {
        // 안을 채우는 동안 쓸 수 있게 먼저 0700으로 만들고, 끝에 권한을 맞춘다.
        fs::create_dir(to)?;
        fs::set_permissions(to, fs::Permissions::from_mode(0o700))?;
        copy_children(from, to)?;
        return finish_dir(to, &meta);
    }
    if ft.is_symlink() {
        symlink(fs::read_link(from)?, to)?;
        return own(to, &meta);
    }
    if ft.is_file() {
        let mut r = File::open(from)?;
        let mut w = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(to)?;
        io::copy(&mut r, &mut w)?;
        w.set_modified(time(&meta))?;
        drop(w);
    } else if ft.is_fifo() || ft.is_char_device() || ft.is_block_device() {
        // 수정 시각은 두지 않는다(FIFO를 열면 막힌다).
        sys::mknod(to, meta.mode(), meta.rdev())?;
    } else {
        // 소켓: 듣는 프로세스 없이는 뜻이 없다.
        return Ok(());
    }
    own(to, &meta)?;
    // chown은 setuid 비트를 지우므로 권한은 그 뒤에 둔다.
    fs::set_permissions(to, fs::Permissions::from_mode(meta.mode() & 0o7777))
}

#[cfg(target_os = "linux")]
pub use linux::Attach;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs::{self, OpenOptions};
    use std::io::{self, Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};

    use crate::sys::linux as sys;
    use crate::{disks, mkfs};

    /// 붙이는 방법. 게스트는 `/dev`와 [`super::STAGING`]을 쓴다. 테스트는 다른 디렉터리를 준다(장치 이름에 loop
    /// 장치를 가리키는 링크를 둔다).
    #[derive(Debug, Clone)]
    pub struct Attach {
        /// 장치 디렉터리(`/dev`). n번째 디스크가 그 안의 `disks::device(n)`이다.
        pub dev: PathBuf,
        /// 잠깐 마운트하는 자리의 부모.
        pub staging: PathBuf,
        /// 첫 볼륨 디스크의 차례([`super::first_volume_disk`]). 그 앞은 층과 scratch다.
        pub first: u32,
    }

    fn bad(m: String) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidInput, m)
    }

    impl Attach {
        /// `disk`번째 디스크를 `target`에 붙인다. 규칙은 모듈 문서의 "붙이기"다.
        pub fn attach(
            &self,
            disk: u32,
            target: &str,
            readonly: bool,
            copy: bool,
        ) -> io::Result<()> {
            if disk < self.first {
                return Err(bad(format!(
                    "disk {disk} is not a volume: the image layers and the writable disk come first"
                )));
            }
            if !target.starts_with('/') {
                return Err(bad(format!("volume target must be absolute: {target:?}")));
            }
            let dev = self.dev.join(disks::device(disk as usize));
            let mut f = OpenOptions::new()
                .read(true)
                .write(!readonly)
                .open(&dev)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", dev.display())))?;
            if disks::is_erofs(&mut f)? {
                return Err(bad(format!(
                    "{} holds an image layer, not a volume",
                    dev.display()
                )));
            }
            if !mkfs::is_formatted(&mut f)? {
                if readonly {
                    return Err(bad(
                        "the volume has never been used, so there is nothing to read; attach it \
                         writable once first"
                            .into(),
                    ));
                }
                let size = f.seek(SeekFrom::End(0))?;
                let mut uuid = [0u8; 16];
                fs::File::open("/dev/urandom")?.read_exact(&mut uuid)?;
                // 파일 시스템이 없는 볼륨은 sbx가 새로 만든 0으로 채운 파일이다(모듈 문서).
                mkfs::format_with(
                    &mut f,
                    size,
                    uuid,
                    mkfs::Disk::Zeroed,
                    mkfs::Journal::Internal,
                )
                .map_err(|e| io::Error::new(e.kind(), format!("format volume: {e}")))?;
                f.sync_all()?;
            }
            drop(f);
            let stage = self.staging.join(disk.to_string());
            fs::create_dir_all(&stage)?;
            let r = self.mount_and_move(&dev, &stage, target, readonly, copy);
            if r.is_err() {
                let _ = sys::umount(&stage.to_string_lossy());
            }
            let _ = fs::remove_dir(&stage);
            r
        }

        fn mount_and_move(
            &self,
            dev: &Path,
            stage: &Path,
            target: &str,
            readonly: bool,
            copy: bool,
        ) -> io::Result<()> {
            let (src, at) = (dev.to_string_lossy(), stage.to_string_lossy());
            let ctx = |what: &str| {
                let what = what.to_owned();
                move |e: io::Error| io::Error::new(e.kind(), format!("{what}: {e}"))
            };
            if readonly {
                // 다시 적용할 저널이 있으면 읽기 전용 장치에는 마운트되지 않는다. 그때는 저널 없이 읽는다.
                sys::mount(&src, &at, "ext4", libc::MS_RDONLY, "")
                    .or_else(|_| sys::mount(&src, &at, "ext4", libc::MS_RDONLY, "noload"))
                    .map_err(ctx("mount volume"))?;
            } else {
                sys::mount(&src, &at, "ext4", 0, "errors=remount-ro")
                    .map_err(ctx("mount volume"))?;
            }
            let target_path = Path::new(target);
            if !readonly && super::is_empty(stage)? {
                // 빈 볼륨에는 `lost+found`도 두지 않는다: 빈 디렉터리를 바라는 프로그램(postgres의 initdb)이 있다.
                match fs::remove_dir(stage.join("lost+found")) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
                if copy {
                    super::copy_up(target_path, stage)
                        .map_err(ctx("fill volume from the image"))?;
                }
                // 채운 내용도 VM이 강제로 꺼지면 남아야 한다(기본 프로세스는 그것이 있다고 본다).
                crate::sys::sync();
            }
            match fs::metadata(target_path) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(bad(format!("volume target {target} is not a directory"))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(target_path).map_err(ctx(target))?;
                }
                Err(e) => return Err(e),
            }
            sys::move_mount(&at, target).map_err(ctx(&format!("move volume to {target}")))
        }
    }
}

#[cfg(test)]
mod tests;
