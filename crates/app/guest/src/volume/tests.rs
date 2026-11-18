use super::*;

use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sbx-guest-volume-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn volumes_come_after_the_layers_and_scratch() {
    // 맨 아래 층(0), scratch(1), 위 층들(2..).
    assert_eq!(first_volume_disk(1), 2);
    assert_eq!(first_volume_disk(4), 5);
    // 층 수를 모르면(0) 적어도 scratch 뒤다.
    assert_eq!(first_volume_disk(0), 2);
}

#[test]
fn a_volume_with_only_lost_and_found_is_empty() {
    let d = temp("empty");
    assert!(is_empty(&d).unwrap());
    fs::create_dir(d.join("lost+found")).unwrap();
    assert!(is_empty(&d).unwrap());
    fs::write(d.join("PG_VERSION"), "17").unwrap();
    assert!(!is_empty(&d).unwrap());
    fs::remove_dir_all(d).unwrap();
}

/// 이미지의 경로 내용이 권한, 수정 시각, 링크째로 볼륨에 들어가고, 디렉터리 자체의 권한도 뿌리에 옮겨진다.
#[test]
fn copy_up_keeps_modes_times_and_links() {
    let d = temp("copy");
    let (src, dst) = (d.join("image/data"), d.join("volume"));
    fs::create_dir_all(src.join("sub/deep")).unwrap();
    fs::create_dir(&dst).unwrap();
    fs::write(src.join("conf"), "listen=*\n").unwrap();
    fs::set_permissions(src.join("conf"), fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(src.join("sub/deep/f"), "x").unwrap();
    symlink("../conf", src.join("sub/link")).unwrap();
    let old = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    File::options()
        .write(true)
        .open(src.join("conf"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    fs::set_permissions(src.join("sub"), fs::Permissions::from_mode(0o750)).unwrap();
    fs::set_permissions(&src, fs::Permissions::from_mode(0o1777)).unwrap();

    copy_up(&src, &dst).unwrap();
    let mode = |p: &Path| fs::symlink_metadata(p).unwrap().mode() & 0o7777;
    assert_eq!(fs::read_to_string(dst.join("conf")).unwrap(), "listen=*\n");
    assert_eq!(mode(&dst.join("conf")), 0o640);
    assert_eq!(
        fs::metadata(dst.join("conf")).unwrap().modified().unwrap(),
        old
    );
    assert_eq!(mode(&dst.join("sub")), 0o750);
    assert_eq!(fs::read_to_string(dst.join("sub/deep/f")).unwrap(), "x");
    assert_eq!(
        fs::read_link(dst.join("sub/link")).unwrap(),
        Path::new("../conf")
    );
    // 대상 디렉터리 자체의 권한이 볼륨의 뿌리로 간다.
    assert_eq!(mode(&dst), 0o1777);
    // 없는 경로는 채울 것이 없다. 파일은 오류다.
    copy_up(&d.join("missing"), &dst).unwrap();
    assert!(copy_up(&src.join("conf"), &d.join("volume2")).is_err());
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(d).unwrap();
}

/// 실제 커널로 볼륨을 붙인다: 처음에는 포맷하고 이미지 내용으로 채우고, `fsync`한 쓰기는 강제로 끈 것과 같은 상태
/// (마운트된 채 뜬 디스크 사본)에서도 남고, 다시 붙이면 채우지 않고, 읽기 전용은 쓸 수 없다.
/// root와 loop 장치가 필요하므로 `SANDBOX_PRIVILEGED_TESTS=1`일 때만 돈다(privileged 컨테이너).
#[cfg(target_os = "linux")]
#[test]
fn privileged_volumes_format_fill_survive_a_crash_and_mount_read_only() {
    use std::io::Write;
    use std::process::Command;
    if std::env::var_os("SANDBOX_PRIVILEGED_TESTS").is_none() {
        eprintln!("SANDBOX_PRIVILEGED_TESTS not set, skipping");
        return;
    }
    let sh = |script: &str| {
        let out = Command::new("sh").args(["-ec", script]).output().unwrap();
        assert!(
            out.status.success(),
            "{script}\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    let d = temp("kernel");
    let dev = d.join("dev");
    fs::create_dir_all(&dev).unwrap();
    let img = d.join("volume.img");
    File::create(&img).unwrap().set_len(64 << 20).unwrap();
    // 붙인 장치는 쓰기로 둔다(앞서 누가 남긴 읽기 전용 표시가 있어도. 아래 `Detach`).
    let attach_rw = |img: &Path| {
        sh(&format!(
            "d=$(losetup -f --show {}) && blockdev --setrw $d && echo $d",
            img.display()
        ))
    };
    let loop_dev = attach_rw(&img);
    // 볼륨은 셋째 디스크(vdc): 층 하나, scratch 다음.
    symlink(&loop_dev, dev.join("vdc")).unwrap();
    let attach = linux::Attach {
        dev: dev.clone(),
        staging: d.join("stage"),
        first: first_volume_disk(1),
    };
    // 이미지에 있던 내용: postgres처럼 다른 사용자 소유의 1777 디렉터리와 파일.
    let root = d.join("root");
    let target = root.join("var/lib/data");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("seed"), "from image").unwrap();
    std::os::unix::fs::chown(target.join("seed"), Some(999), Some(999)).unwrap();
    std::os::unix::fs::chown(&target, Some(999), Some(999)).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o1777)).unwrap();
    let t = target.to_str().unwrap();

    // 층이나 scratch 자리는 볼륨이 아니다.
    assert!(attach.attach(1, t, false, true).is_err());
    // 처음 쓰는 볼륨은 읽기 전용으로 붙일 수 없다.
    let e = attach.attach(2, t, true, false).unwrap_err().to_string();
    assert!(e.contains("never been used"), "{e}");

    attach.attach(2, t, false, true).unwrap();
    assert_eq!(
        fs::read_to_string(target.join("seed")).unwrap(),
        "from image"
    );
    let m = fs::metadata(&target).unwrap();
    assert_eq!((m.uid(), m.mode() & 0o7777), (999, 0o1777));
    assert_eq!(fs::metadata(target.join("seed")).unwrap().uid(), 999);
    assert!(!target.join("lost+found").exists());
    let mut f = File::create(target.join("committed")).unwrap();
    f.write_all(b"fsynced").unwrap();
    f.sync_all().unwrap();
    drop(f);
    // 강제로 끈 VM: 마운트를 풀지 않은 채 그 순간의 디스크를 떠 둔다(loop 장치의 flush는 뒤 파일까지 간다).
    let crash = d.join("crash.img");
    sh(&format!(
        "cp --sparse=always {} {}",
        img.display(),
        crash.display()
    ));
    sh(&format!("umount {t}"));
    sh(&format!("losetup -d {loop_dev}"));
    let fsck = Command::new("e2fsck")
        .args(["-f", "-n"])
        .arg(&img)
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stdout)
    );

    // 떠 둔 디스크를 다시 붙인다: 커널이 저널을 다시 적용하고, fsync한 파일이 있다. 비어 있지 않으니 채우지 않는다.
    fs::write(target.join("seed"), "changed image").unwrap();
    let crash_dev = attach_rw(&crash);
    // 아래에서 장치를 읽기 전용으로 한다. 그 표시(BLKROSET)는 loop 장치를 떼어도 남아, 다음에 그 장치를 받는 마운트가
    // 조용히 읽기 전용이 된다(같은 커널의 다른 테스트가 "Read-only file system"으로 실패했다). 패닉해도 되돌리고 뗀다.
    struct Detach(String);
    impl Drop for Detach {
        fn drop(&mut self) {
            let _ = Command::new("blockdev").args(["--setrw", &self.0]).status();
            let _ = Command::new("losetup").args(["-d", &self.0]).status();
        }
    }
    let crash_guard = Detach(crash_dev.clone());
    fs::remove_file(dev.join("vdc")).unwrap();
    symlink(&crash_dev, dev.join("vdc")).unwrap();
    attach.attach(2, t, false, true).unwrap();
    assert_eq!(
        fs::read_to_string(target.join("committed")).unwrap(),
        "fsynced"
    );
    assert_eq!(
        fs::read_to_string(target.join("seed")).unwrap(),
        "from image"
    );
    sh(&format!("umount {t}"));

    // 읽기 전용: 읽히고 쓸 수 없다. 장치도 읽기 전용이다(실행기가 읽기 전용으로 붙인 것과 같다).
    sh(&format!("blockdev --setro {crash_dev}"));
    attach.attach(2, t, true, true).unwrap();
    assert_eq!(
        fs::read_to_string(target.join("committed")).unwrap(),
        "fsynced"
    );
    assert!(fs::write(target.join("nope"), "x").is_err());
    sh(&format!("umount {t}"));
    drop(crash_guard);
    assert_eq!(
        sh(&format!("blockdev --getro {crash_dev} || echo gone")),
        "0"
    );
    fs::remove_dir_all(d).unwrap();
}
