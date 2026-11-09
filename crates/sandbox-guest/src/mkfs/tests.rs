use super::*;

use std::io::Cursor;
use std::process::Command;

const UUID: [u8; 16] = *b"sbx-scratch-test";

fn image(bytes: u64) -> Vec<u8> {
    // 0이 아닌 값으로 채워 둔다. 포맷이 디스크가 0이라고 가정하지 않는지 본다.
    let mut c = Cursor::new(vec![0xA5u8; bytes as usize]);
    let size = format(&mut c, bytes, UUID, Disk::Unknown).unwrap();
    assert!(size <= bytes);
    c.into_inner()
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[test]
fn zeroed_disk_gets_the_same_bytes() {
    let bytes = 64 << 20;
    let run = |disk| {
        let mut c = Cursor::new(vec![0u8; bytes as usize]);
        format(&mut c, bytes, UUID, disk).unwrap();
        c.into_inner()
    };
    assert!(run(Disk::Zeroed) == run(Disk::Unknown));
}

#[test]
fn superblock_fields() {
    let img = image(64 << 20);
    let sb = &img[1024..2048];
    assert_eq!(le16(sb, 56), MAGIC);
    assert_eq!(le32(sb, 4), 16384); // 64MiB / 4KiB
    assert_eq!(le32(sb, 24), 2);
    assert_eq!(le32(sb, 0), 4096); // 64MiB / 16KiB
    assert_eq!(le32(sb, 84), 11);
    assert_eq!(le16(sb, 88), 256);
    assert!(is_formatted(&mut Cursor::new(&img)).unwrap());
    assert!(!is_formatted(&mut Cursor::new(vec![0u8; 4096])).unwrap());
    assert!(!is_formatted(&mut Cursor::new(vec![0u8; 10])).unwrap());
}

#[test]
fn group_layout_and_backups() {
    // 3 그룹을 넘게: 그룹 0, 1에는 superblock 사본이 있고 2에는 없다.
    let l = Layout::plan(400 << 20).unwrap();
    assert_eq!(l.groups.len(), 4);
    let sup: Vec<bool> = l.groups.iter().map(|g| g.has_super).collect();
    assert_eq!(sup, vec![true, true, false, true]);
    for g in &l.groups {
        assert!(g.data <= g.start + g.blocks);
        assert_eq!(g.inode_table + l.itable_blocks, g.data);
    }
    let img = image(400 << 20);
    let backup = (BLOCKS_PER_GROUP * BLOCK) as usize;
    assert_eq!(le16(&img, backup + 56), MAGIC);
    assert_eq!(le16(&img, backup + 90), 1); // block_group_nr
}

#[test]
fn tiny_trailing_group_is_dropped() {
    // 그룹 하나 + 4블록: 마지막 그룹은 메타데이터도 담지 못한다.
    let l = Layout::plan((BLOCKS_PER_GROUP + 4) * BLOCK).unwrap();
    assert_eq!(l.groups.len(), 1);
    assert_eq!(l.blocks, BLOCKS_PER_GROUP);
}

#[test]
fn too_small_is_an_error() {
    assert!(Layout::plan(MIN_BYTES - 1).is_err());
}

#[test]
fn output_is_deterministic() {
    assert_eq!(image(8 << 20), image(8 << 20));
}

#[test]
fn has_super_follows_sparse_super() {
    let g: Vec<u64> = (0..60).filter(|g| has_super(*g)).collect();
    assert_eq!(g, vec![0, 1, 3, 5, 7, 9, 25, 27, 49]);
}

/// e2fsprogs가 있으면 공식 검사기로 확인한다. CI Linux 작업은 `SANDBOX_REQUIRE_E2FSCK=1`로 필수로 돌린다.
#[test]
fn e2fsck_accepts_image() {
    let have = Command::new("e2fsck").arg("-V").output().is_ok();
    if !have {
        assert!(
            std::env::var_os("SANDBOX_REQUIRE_E2FSCK").is_none(),
            "SANDBOX_REQUIRE_E2FSCK is set but e2fsck is missing"
        );
        eprintln!("e2fsck not found, skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sandbox-guest-mkfs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for size in [MIN_BYTES, 8 << 20, 400 << 20, (1 << 30) + (5 << 20)] {
        let path = dir.join(format!("scratch-{size}.img"));
        std::fs::write(&path, image(size)).unwrap();
        let out = Command::new("e2fsck")
            .args(["-f", "-n"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "e2fsck failed for {size} bytes:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::remove_file(&path).unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// 실제 커널이 ext4 드라이버로 마운트하고, 그 위에서 overlay upper가 동작하는지 본다.
/// root와 loop 장치가 필요하므로 `SANDBOX_PRIVILEGED_TESTS=1`일 때만 돈다(CI는 privileged 컨테이너에서 돌린다).
#[cfg(target_os = "linux")]
#[test]
fn privileged_kernel_mounts_scratch_as_overlay_upper() {
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
        String::from_utf8(out.stdout).unwrap()
    };
    let dir = std::env::temp_dir().join(format!("sandbox-guest-mount-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = dir.join("scratch.img");
    std::fs::write(&img, image(64 << 20)).unwrap();
    let d = dir.display();
    let out = sh(&format!(
        r#"
        mkdir -p {d}/s {d}/lower/keep {d}/lower/opaque {d}/merged
        echo base > {d}/lower/keep/f
        echo old > {d}/lower/opaque/old
        mount -t ext4 -o loop {d}/scratch.img {d}/s
        mkdir {d}/s/upper {d}/s/work
        mount -t overlay overlay -o lowerdir={d}/lower,upperdir={d}/s/upper,workdir={d}/s/work {d}/merged
        echo new > {d}/merged/keep/g
        rm {d}/merged/keep/f
        rm -r {d}/merged/opaque && mkdir {d}/merged/opaque
        ls -A {d}/merged/keep {d}/merged/opaque
        umount {d}/merged
        umount {d}/s
        "#
    ));
    assert_eq!(out, format!("{d}/merged/keep:\ng\n\n{d}/merged/opaque:\n"));
    let check = Command::new("e2fsck")
        .args(["-f", "-n"])
        .arg(&img)
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    std::fs::remove_dir_all(dir).unwrap();
}
