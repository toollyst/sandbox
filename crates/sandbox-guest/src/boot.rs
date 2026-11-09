//! 부팅 (Linux, PID 1). 맨 위 층이다.
//!
//! 디스크는 실행기가 붙인 순서대로 `/dev/vda`가 rootfs(읽기 전용), `/dev/vdb`가 scratch다.
//!
//! **PID 1** ([`pid1`])
//! 1. `/dev`(devtmpfs), `/proc`, `/sys`를 마운트하고 커널 명령줄(`cmdline`)을 읽는다.
//! 2. rootfs를 erofs로 읽기 전용 마운트한다.
//! 3. scratch에 파일 시스템이 없으면 포맷한다(`mkfs`). 있으면 그대로 쓴다(정지 뒤 다시 시작).
//! 4. rootfs를 아래, scratch를 위로 overlay를 만들고, 그 안에 `/proc`, `/sys`, `/dev`, `/dev/pts`, `/dev/shm`을 마운트한다.
//! 5. overlay를 `/`로 옮기고 chroot한다. 루프백을 올리고 호스트 이름을 정한다.
//! 6. 자신을 `agent` 인자로 다시 실행해 연결 처리 프로세스를 띄운다.
//! 7. 이후로는 고아 프로세스를 거두기만 한다. 연결 처리 프로세스가 끝나면 VM을 끝낸다.
//!
//! 연결 처리를 따로 띄우는 이유: PID 1이 모든 자식을 거두면(`waitpid(-1)`) exec한 프로세스의 종료 상태를 빼앗는다.
//!
//! **연결 처리** ([`agent`]): vsock [`GUEST_PORT`]에서 연결마다 스레드 하나로 `server::serve`를 돌린다.
//! `Shutdown` 요청이면 sync 뒤 `sbx.halt` 방법으로 VM을 끝낸다.
//!
//! 어느 단계든 실패하면 콘솔에 이유를 쓰고 VM을 끝낸다. 호스트는 연결이 되지 않는 것으로 안다.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::process::Command;
use std::thread;

use sandbox_proto::GUEST_PORT;

use crate::cmdline::{self, BootConfig, Halt};
use crate::mkfs;
use crate::server::{self, Ctx};
use crate::sys::linux as sys;

const ROOTFS_DEV: &str = "/dev/vda";
const SCRATCH_DEV: &str = "/dev/vdb";
const LOWER: &str = "/run/sbx/lower";
const SCRATCH: &str = "/run/sbx/scratch";
const NEWROOT: &str = "/run/sbx/root";

fn log(msg: &str) {
    eprintln!("sandbox-guest: {msg}");
}

fn ctx(what: &str) -> impl Fn(io::Error) -> io::Error + '_ {
    move |e| io::Error::new(e.kind(), format!("{what}: {e}"))
}

fn halt(how: Halt) -> ! {
    crate::sys::sync();
    let cmd = match how {
        Halt::PowerOff => libc::RB_POWER_OFF,
        Halt::Reboot => libc::RB_AUTOBOOT,
    };
    let e = sys::reboot(cmd);
    log(&format!("reboot failed: {e}"));
    loop {
        thread::park();
    }
}

fn mount_dir(
    source: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: &str,
) -> io::Result<()> {
    fs::create_dir_all(target).map_err(ctx(target))?;
    sys::mount(source, target, fstype, flags, data)
        .map_err(ctx(&format!("mount {fstype} on {target}")))
}

fn read_config() -> BootConfig {
    let raw = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    match cmdline::parse(&raw) {
        Ok(c) => c,
        Err(e) => {
            log(&e);
            halt(Halt::PowerOff)
        }
    }
}

fn prepare_scratch() -> io::Result<()> {
    let mut dev = OpenOptions::new()
        .read(true)
        .write(true)
        .open(SCRATCH_DEV)
        .map_err(ctx(SCRATCH_DEV))?;
    if !mkfs::is_formatted(&mut dev)? {
        let size = dev.seek(SeekFrom::End(0))?;
        let mut uuid = [0u8; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut uuid)?;
        mkfs::format(&mut dev, size, uuid).map_err(ctx("format scratch"))?;
        dev.sync_all()?;
    }
    drop(dev);
    mount_dir(SCRATCH_DEV, SCRATCH, "ext4", libc::MS_NOATIME, "")?;
    fs::create_dir_all(format!("{SCRATCH}/upper"))?;
    fs::create_dir_all(format!("{SCRATCH}/work"))?;
    Ok(())
}

fn switch_root(cfg: &BootConfig) -> io::Result<()> {
    mount_dir("devtmpfs", "/dev", "devtmpfs", libc::MS_NOSUID, "mode=0755")?;
    mount_dir(
        "proc",
        "/proc",
        "proc",
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        "",
    )?;
    mount_dir(
        "sysfs",
        "/sys",
        "sysfs",
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        "",
    )?;
    mount_dir(ROOTFS_DEV, LOWER, "erofs", libc::MS_RDONLY, "")?;
    prepare_scratch()?;
    mount_dir(
        "overlay",
        NEWROOT,
        "overlay",
        0,
        &format!("lowerdir={LOWER},upperdir={SCRATCH}/upper,workdir={SCRATCH}/work"),
    )?;
    let nosuid = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
    mount_dir("proc", &format!("{NEWROOT}/proc"), "proc", nosuid, "")?;
    mount_dir(
        "sysfs",
        &format!("{NEWROOT}/sys"),
        "sysfs",
        nosuid | libc::MS_RDONLY,
        "",
    )?;
    mount_dir(
        "devtmpfs",
        &format!("{NEWROOT}/dev"),
        "devtmpfs",
        libc::MS_NOSUID,
        "mode=0755",
    )?;
    mount_dir(
        "devpts",
        &format!("{NEWROOT}/dev/pts"),
        "devpts",
        libc::MS_NOSUID | libc::MS_NOEXEC,
        "newinstance,ptmxmode=0666,mode=0620",
    )?;
    mount_dir(
        "shm",
        &format!("{NEWROOT}/dev/shm"),
        "tmpfs",
        nosuid,
        "mode=1777",
    )?;

    std::env::set_current_dir(NEWROOT)?;
    sys::move_mount(".", "/").map_err(ctx("move root"))?;
    std::os::unix::fs::chroot(".").map_err(ctx("chroot"))?;
    std::env::set_current_dir("/")?;

    sys::loopback_up().map_err(ctx("loopback"))?;
    sys::sethostname(&cfg.hostname).map_err(ctx("hostname"))?;
    Ok(())
}

/// PID 1. 돌아오지 않는다.
pub fn pid1() -> ! {
    if std::process::id() != 1 {
        log("must run as PID 1");
        std::process::exit(1);
    }
    // /proc가 있어야 명령줄을 읽는다. 마운트 실패는 switch_root가 다시 보고한다.
    let _ = mount_dir("proc", "/proc", "proc", 0, "");
    let cfg = read_config();
    if let Err(e) = switch_root(&cfg) {
        log(&format!("boot failed: {e}"));
        halt(cfg.halt);
    }
    let agent = match Command::new("/proc/self/exe").arg("agent").spawn() {
        Ok(c) => c.id(),
        Err(e) => {
            log(&format!("cannot start agent: {e}"));
            halt(cfg.halt);
        }
    };
    loop {
        match sys::wait_any() {
            Ok((pid, status)) if pid == agent => {
                log(&format!("agent exited (status {status:#x})"));
                halt(cfg.halt);
            }
            Ok(_) => {}
            Err(e) => {
                log(&format!("wait failed: {e}"));
                halt(cfg.halt);
            }
        }
    }
}

/// 연결 처리 프로세스. 돌아오지 않는다.
pub fn agent() -> ! {
    let cfg = read_config();
    let listener = match sys::vsock_listen(GUEST_PORT) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("vsock listen on port {GUEST_PORT}: {e}"));
            halt(cfg.halt);
        }
    };
    let halt_how = cfg.halt;
    let ctx: &'static Ctx = Box::leak(Box::new(Ctx {
        root: "/".into(),
        on_shutdown: Box::new(move || halt(halt_how)),
    }));
    loop {
        match sys::accept(&listener) {
            Ok(conn) => {
                thread::spawn(move || server::serve(conn, ctx));
            }
            Err(e) => log(&format!("accept: {e}")),
        }
    }
}
