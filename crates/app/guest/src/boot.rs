//! 부팅 (Linux, PID 1). 맨 위 층이다.
//!
//! 디스크는 실행기가 붙인 순서대로 `/dev/vda`가 이미지의 맨 아래 층, `/dev/vdb`가 scratch, 그 뒤 EROFS 디스크들이
//! 위 층들이다. 규칙은 [`crate::disks`]에 있다.
//!
//! **PID 1** ([`pid1`])
//! 1. `/dev`(devtmpfs), `/proc`, `/sys`를 마운트하고 커널 명령줄(`cmdline`)을 읽는다.
//! 2. 층 디스크마다 erofs로 읽기 전용 마운트한다(`/run/sbx/lower/0`이 맨 아래).
//! 3. scratch에 파일 시스템이 없으면 포맷한다(`mkfs`). 있으면 그대로 쓴다(정지 뒤 다시 시작).
//! 4. 층들을 아래(lowerdir, 맨 위 층이 앞), scratch를 위로 overlay를 만들고, 그 안에 `/proc`, `/sys`, `/dev`,
//!    `/dev/pts`, `/dev/shm`을 마운트한다. `/dev`에는 OCI 런타임처럼 `/dev/fd`, `/dev/stdin` 등의 링크를 둔다
//!    (`crate::devfs`). 층이 하나면 지금까지와 같은 overlay다.
//! 5. overlay를 `/`로 옮기고 chroot한다. 루프백을 올리고 호스트 이름을 정한다.
//! 6. 자신을 `agent` 인자로 다시 실행해 연결 처리 프로세스를 띄운다.
//! 7. 이후로는 고아 프로세스를 거두기만 한다. 연결 처리 프로세스가 끝나면 VM을 끝낸다.
//!
//! 연결 처리를 따로 띄우는 이유: PID 1이 모든 자식을 거두면(`waitpid(-1)`) exec한 프로세스의 종료 상태를 빼앗는다.
//!
//! **연결 처리** ([`agent`]): vsock [`GUEST_PORT`]에서 연결마다 스레드 하나로 `server::serve`를 돌린다.
//! 루프백 DNS([`crate::dns`])도 띄운다: 이름 풀기가 시간 초과(5초)를 기다리지 않고 곧바로 실패하게 한다.
//! 시계는 호스트가 `ClockSync`를 보낼 때 커널 시계를 맞춘다([`crate::clock`]).
//! 이름 붙은 볼륨은 호스트가 시작할 때 `Volume`으로 알리면 붙인다([`crate::volume`]). 부팅은 볼륨을 모른다.
//! `Shutdown` 요청이면 sync 뒤 `sbx.halt` 방법으로 VM을 끝낸다.
//! 빌드 층(`OpenLayer`)은 이 프로세스의 뿌리를 층 안으로 옮긴다. 규칙은 `overlay` 모듈에 있다. PID 1은 처음 뿌리에 남는다.
//!
//! 어느 단계든 실패하면 콘솔에 이유를 쓰고 VM을 끝낸다. 호스트는 연결이 되지 않는 것으로 안다.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::process::Command;
use std::thread;

use sbx_guest_proto::GUEST_PORT;

use crate::cmdline::{self, BootConfig, Halt};
use crate::mkfs;
use crate::server::{self, Ctx};
use crate::sys::linux as sys;

const SCRATCH_DEV: &str = "/dev/vdb";
const LOWER: &str = "/run/sbx/lower";
const SCRATCH: &str = "/run/sbx/scratch";
const NEWROOT: &str = "/run/sbx/root";

fn log(msg: &str) {
    eprintln!("sbx-guest: {msg}");
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
        // 포맷되지 않은 scratch는 sbx가 새로 만든 0으로 채운 파일이다(실행기 규약의 약속).
        mkfs::format(&mut dev, size, uuid, mkfs::Disk::Zeroed).map_err(ctx("format scratch"))?;
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
    let layers = crate::disks::layer_devices(std::path::Path::new("/dev"))?;
    let mut lowers = Vec::with_capacity(layers.len());
    for (i, dev) in layers.iter().enumerate() {
        let target = format!("{LOWER}/{i}");
        mount_dir(
            &dev.to_string_lossy(),
            &target,
            "erofs",
            libc::MS_RDONLY,
            "",
        )?;
        lowers.push(target);
    }
    // overlay는 lowerdir의 앞을 위로 본다.
    lowers.reverse();
    prepare_scratch()?;
    mount_dir(
        "overlay",
        NEWROOT,
        "overlay",
        0,
        &format!(
            "lowerdir={},upperdir={SCRATCH}/upper,workdir={SCRATCH}/work",
            lowers.join(":")
        ),
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
    // `/dev/fd`, `/dev/stdin` 등(OCI 런타임과 같다). 없어도 부팅은 계속한다.
    if let Err(e) = crate::devfs::add_links(std::path::Path::new(&format!("{NEWROOT}/dev"))) {
        log(&format!("/dev links: {e}"));
    }
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

/// 실행기가 붙인 virtio-fs 공유 폴더를 마운트한다. 대상 디렉터리가 없으면 만든다.
fn mount_share(tag: &str, target: &str, readonly: bool) -> io::Result<()> {
    let mut flags = libc::MS_NOSUID | libc::MS_NODEV;
    if readonly {
        flags |= libc::MS_RDONLY;
    }
    mount_dir(tag, target, "virtiofs", flags, "")
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
    // 나가는 연결과 투명 TCP. 루프백 DNS는 이것에게 답을 받는다(투명 TCP가 켜지기 전에는 모두 NXDOMAIN).
    let egress = std::sync::Arc::new(crate::egress::Egress::with_net(Box::new(
        crate::transparent::KernelNet,
    )));
    // 루프백 DNS. 띄우지 못해도(이미 누가 듣는다) 부팅은 계속한다. 그때는 이름 풀기가 느리게 실패할 뿐이다.
    for addr in ["127.0.0.1:53", "[::1]:53"] {
        match std::net::UdpSocket::bind(addr) {
            Ok(sock) => {
                let resolver = egress.resolver();
                thread::spawn(move || crate::dns::serve(&sock, resolver));
            }
            Err(e) => log(&format!("dns on {addr}: {e}")),
        }
    }
    let halt_how = cfg.halt;
    // 층 디스크는 부팅 때와 같은 규칙으로 다시 찾는다(디스크 차례와 내용만 본다). 찾지 못하면 수를 모른다고 본다.
    let found = crate::disks::layer_devices(std::path::Path::new("/dev")).ok();
    let layer_disks = found.as_ref().and_then(|d| u32::try_from(d.len()).ok());
    let layer_devs =
        found.unwrap_or_else(|| vec![std::path::Path::new("/dev").join(crate::disks::device(0))]);
    // 볼륨은 층 디스크와 scratch 뒤다. 층 수를 몰라도 장치가 EROFS이면 붙이기가 거부한다.
    let volumes = crate::volume::linux::Attach {
        dev: "/dev".into(),
        staging: crate::volume::STAGING.into(),
        first: crate::volume::first_volume_disk(layer_devs.len()),
    };
    let transparent_on = {
        let e = egress.clone();
        move || e.transparent_on()
    };
    let ctx: &'static Ctx = Box::leak(Box::new(Ctx {
        root: "/".into(),
        on_shutdown: Box::new(move || halt(halt_how)),
        share: Some(Box::new(mount_share)),
        egress,
        main: Default::default(),
        // 첫 OpenLayer 전에는 아무것도 하지 않는다(보통의 run에는 비용이 없다).
        layers: Some(Box::new(
            crate::overlay::Overlay::guest(layer_devs, SCRATCH_DEV)
                .with_resolver(Box::new(transparent_on)),
        )),
        sweep: crate::exec::Sweep::Everything,
        procs: None,
        layer_disks,
        // 시계 맞추기: 호스트가 보낼 때만 한다. 규칙은 clock 모듈에 있다.
        clock: Some(Box::new(crate::clock::Kernel)),
        volume: Some(Box::new(move |disk, target, readonly, copy| {
            volumes.attach(disk, target, readonly, copy)
        })),
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
