//! Firecracker 공식 jailer로 VM을 가둔다(Linux, root). 규칙은 크레이트 문서의 "jailer" 절에 있다.
//!
//! 이 모듈은 다른 모듈을 쓰지 않는다. 받은 [`VmSpec`]을 jail 안 경로로 바꾼 것([`Plan::spec`])을 돌려주고, 그것을
//! Firecracker 설정 파일로 만드는 일은 [`crate::session`]이 한다.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use sbx_executor_proto::VmSpec;

/// chroot들을 두는 디렉터리를 가리키는 환경 변수. 기본은 [`DEFAULT_CHROOT_BASE`].
pub const ENV_CHROOT_BASE: &str = "SBX_FIRECRACKER_CHROOT_BASE";
/// jailer의 기본값과 같다.
pub const DEFAULT_CHROOT_BASE: &str = "/srv/jailer";
/// jail이 쓰는 uid·gid의 시작을 가리키는 환경 변수. 기본은 [`DEFAULT_ID_BASE`].
pub const ENV_ID_BASE: &str = "SBX_FIRECRACKER_JAILER_ID_BASE";
/// gid는 이 값, uid는 이 값 + 1 + 슬롯. 시스템 계정·일반 사용자·흔한 subuid 범위와 겹치지 않는 높은 곳이다.
pub const DEFAULT_ID_BASE: u32 = 2_100_000_000;
/// 동시에 돌 수 있는 jail 수.
pub const SLOTS: u32 = 4096;
/// cgroup v2 상한을 끄는 환경 변수(`off`). cgroup을 쓸 수 없는 곳(일부 컨테이너)을 위해서다.
pub const ENV_CGROUPS: &str = "SBX_FIRECRACKER_CGROUPS";
/// cgroup v2의 뿌리.
pub const CGROUP_ROOT: &str = "/sys/fs/cgroup";
/// 게스트 메모리 위에 Firecracker 프로세스 몫으로 더 허락하는 메모리.
pub const MEMORY_OVERHEAD_MIB: u64 = 256;
/// jailer가 실행 파일 이름으로 만드는 cgroup 부모(`--parent-cgroup`의 기본값)와 chroot 아래 디렉터리.
const EXEC_NAME: &str = "firecracker";
/// 슬롯 잠금 파일을 두는 곳(chroot 기준 디렉터리 아래).
const LOCKS: &str = "sbx-slots";
/// jail 안의 경로들.
const SOCKET: &str = "v.sock";
const CONFIG: &str = "config.json";
const KERNEL: &str = "kernel";
const INIT: &str = "init";

/// jail 안에 둘 파일 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// 호스트의 원본.
    pub source: PathBuf,
    /// jail 뿌리 아래 이름.
    pub name: String,
    /// VM이 쓴다(쓰기 가능한 디스크).
    pub writable: bool,
    /// 다른 파일시스템이면 복사해도 된다(커널, 게스트 init처럼 작고 읽기만 하는 것).
    pub copy_ok: bool,
}

/// VM 하나의 jail 계획. 순수하다(파일을 건드리지 않는다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// jailer `--id`.
    pub id: String,
    /// jail 사용자(슬롯마다 다르다).
    pub uid: u32,
    /// jail 그룹(모든 jail이 같다).
    pub gid: u32,
    /// `--chroot-base-dir`.
    pub base: PathBuf,
    /// jail에 넣을 파일.
    pub files: Vec<Staged>,
    /// jail 안 경로로 바꾼 VM.
    pub spec: VmSpec,
    /// jail 안 커널 경로.
    pub kernel: PathBuf,
}

impl Plan {
    /// 슬롯 `slot`에서 `vm`(커널은 `kernel`)을 돌리는 계획.
    pub fn new(vm: &VmSpec, kernel: &Path, base: &Path, id_base: u32, slot: u32) -> Self {
        let mut files = vec![
            Staged {
                source: kernel.to_owned(),
                name: KERNEL.into(),
                writable: false,
                copy_ok: true,
            },
            Staged {
                source: vm.guest_init.clone(),
                name: INIT.into(),
                writable: false,
                copy_ok: true,
            },
        ];
        let mut spec = vm.clone();
        for (i, d) in spec.disks.iter_mut().enumerate() {
            let name = format!("disk{i}");
            files.push(Staged {
                source: d.path.clone(),
                name: name.clone(),
                writable: !d.readonly,
                copy_ok: false,
            });
            d.path = Path::new("/").join(name);
        }
        spec.guest_init = Path::new("/").join(INIT);
        spec.kernel = None;
        spec.control_socket = Path::new("/").join(SOCKET);
        Plan {
            id: format!("sbx-{slot}"),
            uid: id_base + 1 + slot,
            gid: id_base,
            base: base.to_owned(),
            files,
            spec,
            kernel: Path::new("/").join(KERNEL),
        }
    }

    /// `<base>/firecracker/<id>`: 끝나면 통째로 지운다.
    pub fn dir(&self) -> PathBuf {
        self.base.join(EXEC_NAME).join(&self.id)
    }

    /// jail의 뿌리(Firecracker에게는 `/`).
    pub fn root(&self) -> PathBuf {
        self.dir().join("root")
    }

    /// 호스트에서 본 vsock 소켓. sbx가 준 제어 소켓 자리에 이것을 가리키는 링크를 둔다.
    pub fn socket(&self) -> PathBuf {
        self.root().join(SOCKET)
    }

    /// 호스트에서 본 설정 파일.
    pub fn config(&self) -> PathBuf {
        self.root().join(CONFIG)
    }

    /// 이 VM의 cgroup(jailer가 만든다).
    pub fn cgroup(&self) -> PathBuf {
        Path::new(CGROUP_ROOT).join(EXEC_NAME).join(&self.id)
    }

    /// jailer 인자. `cgroups`는 `파일=값` 목록(비면 cgroup을 만들지 않는다).
    pub fn args(&self, firecracker: &Path, cgroups: &[String]) -> Vec<OsString> {
        let mut a: Vec<OsString> = vec![
            "--id".into(),
            self.id.clone().into(),
            "--exec-file".into(),
            firecracker.into(),
            "--uid".into(),
            self.uid.to_string().into(),
            "--gid".into(),
            self.gid.to_string().into(),
            "--chroot-base-dir".into(),
            self.base.clone().into(),
        ];
        if !cgroups.is_empty() {
            a.extend(["--cgroup-version".into(), "2".into()]);
            for c in cgroups {
                a.extend(["--cgroup".into(), c.into()]);
            }
        }
        a.extend([
            "--".into(),
            "--no-api".into(),
            "--config-file".into(),
            Path::new("/").join(CONFIG).into(),
        ]);
        a
    }
}

/// cgroup v2 상한. `available`은 뿌리의 `cgroup.controllers` 내용이다. 그 안에 있는 컨트롤러만 쓴다.
pub fn cgroup_limits(vm: &VmSpec, available: &str) -> Vec<String> {
    let has = |c: &str| available.split_whitespace().any(|x| x == c);
    let mut v = Vec::new();
    if has("cpu") {
        // vCPU마다 한 코어, Firecracker 자신의 스레드 몫으로 한 코어 더.
        v.push(format!(
            "cpu.max={} 100000",
            (u64::from(vm.vcpus) + 1) * 100_000
        ));
    }
    if has("memory") {
        v.push(format!(
            "memory.max={}",
            (u64::from(vm.memory_mib) + MEMORY_OVERHEAD_MIB) << 20
        ));
    }
    if has("pids") {
        v.push(format!("pids.max={}", u32::from(vm.vcpus) + 32));
    }
    v
}

/// 잡은 슬롯. 놓으면(drop) 다른 VM이 쓸 수 있다. 프로세스가 죽어도 커널이 잠금을 푼다.
#[derive(Debug)]
pub struct Slot {
    /// 슬롯 번호.
    pub n: u32,
    _lock: File,
}

/// 빈 슬롯을 하나 잡는다. 잠금 파일은 `<base>/sbx-slots/<n>.lock`.
pub fn take_slot(base: &Path) -> Result<Slot, String> {
    let dir = base.join(LOCKS);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for n in 0..SLOTS {
        let p = dir.join(format!("{n}.lock"));
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&p)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        match f.try_lock() {
            Ok(()) => return Ok(Slot { n, _lock: f }),
            Err(std::fs::TryLockError::WouldBlock) => continue,
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(format!("{}: {e}", p.display()));
            }
        }
    }
    Err(format!(
        "all {SLOTS} jail slots under {} are in use",
        base.display()
    ))
}

/// 기준 디렉터리는 root만 쓸 수 있어야 한다(jailer는 그 아래를 믿는다). 없으면 0700으로 만든다.
#[cfg(unix)]
pub fn check_base(base: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if !base.is_absolute() {
        return Err(format!(
            "{ENV_CHROOT_BASE} must be an absolute path: {}",
            base.display()
        ));
    }
    if !base.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(base)
            .map_err(|e| format!("{}: {e}", base.display()))?;
    }
    let m = std::fs::symlink_metadata(base).map_err(|e| format!("{}: {e}", base.display()))?;
    if !m.is_dir() || m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(format!(
            "{} must be a directory owned by root that only root can write to",
            base.display()
        ));
    }
    Ok(())
}

/// 바꿔 둔 소유자. 끝나면 되돌린다.
#[derive(Debug)]
pub struct Owner {
    path: PathBuf,
    uid: u32,
    gid: u32,
}

/// 지난번에 죽은 VM이 남긴 jail을 지우고, jail 뿌리를 만들고 파일을 넣는다. 쓰기 가능한 파일은 jail 사용자에게
/// 주고(돌려줄 소유자를 반환한다), 읽기만 하는 파일은 jail 사용자가 읽을 수 있게 한다.
#[cfg(unix)]
pub fn stage(plan: &Plan) -> Result<Vec<Owner>, String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let err = |p: &Path, e: io::Error| format!("{}: {e}", p.display());
    remove(plan);
    let root = plan.root();
    std::fs::create_dir_all(&root).map_err(|e| err(&root, e))?;
    let mut owners = Vec::new();
    for f in &plan.files {
        let dst = root.join(&f.name);
        let m = std::fs::metadata(&f.source).map_err(|e| err(&f.source, e))?;
        if !m.is_file() {
            return Err(format!("{} is not a regular file", f.source.display()));
        }
        match std::fs::hard_link(&f.source, &dst) {
            Ok(()) => {}
            Err(e) if e.raw_os_error() == Some(EXDEV) && f.copy_ok => {
                std::fs::copy(&f.source, &dst).map_err(|e| err(&dst, e))?;
                std::os::unix::fs::chown(&dst, Some(plan.uid), Some(plan.gid))
                    .map_err(|e| err(&dst, e))?;
                std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o400))
                    .map_err(|e| err(&dst, e))?;
                continue;
            }
            Err(e) if e.raw_os_error() == Some(EXDEV) => {
                return Err(format!(
                    "{} is on another filesystem than {}; set {ENV_CHROOT_BASE} to a directory on the same \
                     filesystem as sbx's data and cache directories",
                    f.source.display(),
                    plan.base.display()
                ));
            }
            Err(e) => return Err(err(&dst, e)),
        }
        if f.writable {
            owners.push(Owner {
                path: f.source.clone(),
                uid: m.uid(),
                gid: m.gid(),
            });
            std::os::unix::fs::chown(&dst, Some(plan.uid), Some(plan.gid))
                .map_err(|e| err(&dst, e))?;
        } else {
            // 하드 링크는 원본과 같은 파일이다. 다른 사용자가 쓸 수 있으면 jail 하나가 함께 쓰는 이미지를 바꿀 수 있다.
            if m.mode() & 0o002 != 0 {
                return Err(format!(
                    "{} is writable by other users; refusing to give it to a jailed VM",
                    f.source.display()
                ));
            }
            // 다른 사용자가 읽을 수 없으면(umask 027 같은 곳) 그룹을 jail 그룹으로 하고 그룹 읽기를 더한다.
            if m.mode() & 0o004 == 0 {
                std::os::unix::fs::chown(&dst, None, Some(plan.gid)).map_err(|e| err(&dst, e))?;
                std::fs::set_permissions(
                    &dst,
                    std::fs::Permissions::from_mode((m.mode() | 0o040) & 0o7777),
                )
                .map_err(|e| err(&dst, e))?;
            }
        }
    }
    Ok(owners)
}

/// 설정 파일을 jail 안에 쓴다(jail 사용자만 읽는다).
#[cfg(unix)]
pub fn write_config(plan: &Plan, text: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let p = plan.config();
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&p)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    io::Write::write_all(&mut f, text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))?;
    std::os::unix::fs::chown(&p, Some(plan.uid), Some(plan.gid))
        .map_err(|e| format!("{}: {e}", p.display()))
}

/// 끝난 VM의 흔적을 지운다: 소유자를 되돌리고, jail 디렉터리와 cgroup을 지운다. 실패는 무시한다(다음에 같은
/// 슬롯을 쓰는 VM이 [`stage`]에서 다시 지운다).
#[cfg(unix)]
pub fn finish(plan: &Plan, owners: &[Owner]) {
    for o in owners {
        let _ = std::os::unix::fs::chown(&o.path, Some(o.uid), Some(o.gid));
    }
    remove(plan);
}

#[cfg(unix)]
fn remove(plan: &Plan) {
    // jail 디렉터리 안은 하드 링크·복사본·장치 파일뿐이다. 지워도 원본은 그대로다.
    let _ = std::fs::remove_dir_all(plan.dir());
    // 프로세스가 막 끝났으면 cgroup이 잠시 비지 않았을 수 있다.
    let cg = plan.cgroup();
    for _ in 0..50 {
        match std::fs::remove_dir(&cg) {
            Err(e) if e.raw_os_error() == Some(EBUSY) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => break,
        }
    }
}

/// Linux의 `EXDEV`, `EBUSY` (libc 없이).
const EXDEV: i32 = 18;
const EBUSY: i32 = 16;

#[cfg(test)]
mod tests {
    use super::*;
    use sbx_executor_proto::{DiskRole, DiskSpec};

    fn vm() -> VmSpec {
        VmSpec {
            guest_init: "/c/initramfs".into(),
            kernel: Some("/c/img/kernel".into()),
            disks: vec![
                DiskSpec {
                    path: "/c/rootfs.erofs".into(),
                    role: DiskRole::Rootfs,
                    readonly: true,
                },
                DiskSpec {
                    path: "/d/scratch.img".into(),
                    role: DiskRole::Scratch,
                    readonly: false,
                },
            ],
            vcpus: 2,
            memory_mib: 512,
            shares: vec![],
            control_socket: "/run/sbx/abc.sock".into(),
            hostname: "box".into(),
        }
    }

    #[test]
    fn the_vm_sees_only_paths_inside_its_jail() {
        let p = Plan::new(
            &vm(),
            Path::new("/c/img/kernel"),
            Path::new("/srv/jailer"),
            1000,
            7,
        );
        assert_eq!((p.id.as_str(), p.uid, p.gid), ("sbx-7", 1008, 1000));
        assert_eq!(p.root(), Path::new("/srv/jailer/firecracker/sbx-7/root"));
        assert_eq!(p.socket(), p.root().join("v.sock"));
        assert_eq!(p.spec.control_socket, Path::new("/v.sock"));
        assert_eq!(p.spec.guest_init, Path::new("/init"));
        assert_eq!(
            (p.spec.kernel.as_ref(), p.kernel.as_path()),
            (None, Path::new("/kernel"))
        );
        let disks: Vec<_> = p
            .spec
            .disks
            .iter()
            .map(|d| d.path.to_str().unwrap())
            .collect();
        assert_eq!(disks, ["/disk0", "/disk1"]);
        let f: Vec<_> = p
            .files
            .iter()
            .map(|f| {
                (
                    f.source.to_str().unwrap(),
                    f.name.as_str(),
                    f.writable,
                    f.copy_ok,
                )
            })
            .collect();
        assert_eq!(
            f,
            [
                ("/c/img/kernel", "kernel", false, true),
                ("/c/initramfs", "init", false, true),
                ("/c/rootfs.erofs", "disk0", false, false),
                ("/d/scratch.img", "disk1", true, false),
            ]
        );
        let a: Vec<String> = p
            .args(Path::new("/v/firecracker"), &["pids.max=34".into()])
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        assert_eq!(
            a.join(" "),
            "--id sbx-7 --exec-file /v/firecracker --uid 1008 --gid 1000 --chroot-base-dir /srv/jailer \
             --cgroup-version 2 --cgroup pids.max=34 -- --no-api --config-file /config.json"
        );
        let a = p.args(Path::new("/v/firecracker"), &[]);
        assert!(!a.iter().any(|s| s == "--cgroup-version"));
    }

    #[test]
    fn cgroup_limits_follow_the_vm_and_what_the_host_has() {
        assert_eq!(
            cgroup_limits(&vm(), "cpuset cpu io memory hugetlb pids rdma"),
            [
                "cpu.max=300000 100000",
                "memory.max=805306368",
                "pids.max=34"
            ]
        );
        assert_eq!(cgroup_limits(&vm(), "cpuset io pids"), ["pids.max=34"]);
        assert!(cgroup_limits(&vm(), "").is_empty());
    }

    #[test]
    fn slots_are_taken_one_at_a_time_and_freed_on_drop() {
        let base = std::env::temp_dir().join(format!("sbx-fc-slots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let a = take_slot(&base).unwrap();
        let b = take_slot(&base).unwrap();
        assert_eq!((a.n, b.n), (0, 1));
        drop(a);
        assert_eq!(take_slot(&base).unwrap().n, 0);
        std::fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn staging_links_files_and_finishing_leaves_the_originals() {
        use std::os::unix::fs::MetadataExt;
        let t = std::env::temp_dir().join(format!("sbx-fc-stage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("c")).unwrap();
        for f in ["kernel", "init", "rootfs", "scratch"] {
            std::fs::write(t.join("c").join(f), f).unwrap();
        }
        let mut v = vm();
        v.guest_init = t.join("c/init");
        v.disks[0].path = t.join("c/rootfs");
        v.disks[1].path = t.join("c/scratch");
        // 이 테스트는 root가 아니어도 돈다: jail 사용자를 지금 사용자로 한다(소유자 바꾸기가 그대로 된다).
        let me = std::fs::metadata(&t).unwrap();
        let mut p = Plan::new(&v, &t.join("c/kernel"), &t.join("jail"), 0, 0);
        (p.uid, p.gid) = (me.uid(), me.gid());
        // 지난번에 남은 jail은 지운다.
        std::fs::create_dir_all(p.root().join("stale")).unwrap();
        let owners = stage(&p).unwrap();
        assert!(!p.root().join("stale").exists());
        for (name, want) in [
            ("kernel", "kernel"),
            ("init", "init"),
            ("disk0", "rootfs"),
            ("disk1", "scratch"),
        ] {
            assert_eq!(std::fs::read_to_string(p.root().join(name)).unwrap(), want);
        }
        // 하드 링크다(복사하지 않는다).
        assert_eq!(
            std::fs::metadata(p.root().join("disk1")).unwrap().ino(),
            std::fs::metadata(t.join("c/scratch")).unwrap().ino()
        );
        assert_eq!(owners.len(), 1);
        write_config(&p, "{}").unwrap();
        assert_eq!(std::fs::read_to_string(p.config()).unwrap(), "{}");
        finish(&p, &owners);
        assert!(!p.dir().exists());
        assert_eq!(
            std::fs::read_to_string(t.join("c/scratch")).unwrap(),
            "scratch"
        );
        // 다른 사용자가 쓸 수 있는 읽기 전용 이미지는 jail에 주지 않는다.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(t.join("c/rootfs"), std::fs::Permissions::from_mode(0o666))
            .unwrap();
        assert!(stage(&p).unwrap_err().contains("writable by other users"));
        std::fs::remove_dir_all(t).unwrap();
    }
}
