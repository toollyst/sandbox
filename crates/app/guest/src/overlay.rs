//! 빌드 층을 overlay로 쌓는다 (Linux). [`Layers`]의 게스트 구현이다.
//!
//! **처음 뿌리**: 부팅은 rootfs와 scratch를 합친 overlay 안으로 chroot한 뒤 연결 처리 프로세스를 띄운다(`boot`).
//! 그 뿌리를 처음 뿌리라 한다. 처음 뿌리에서는 rootfs(erofs)와 scratch(ext4)의 원래 마운트가 가려져 보이지 않는다.
//! 그래서 첫 층을 열 때 같은 장치를 처음 뿌리 안에 다시 마운트한다. 이미 마운트된 장치를 다시 마운트하면 커널은
//! 같은 superblock을 쓰므로 bind 마운트와 같다. 부팅과 커널 명령줄은 바뀌지 않고, 보통의 `run`에는 비용이 없다
//! (첫 `OpenLayer` 전에는 이 모듈이 시스템 호출을 하지 않는다). 처음 뿌리의 위층(`scratch/upper`)은 빌드 VM의
//! 작업 공간일 뿐 내보내지 않는다.
//!
//! ```text
//! /run/sbx/scratch           /dev/vdb(scratch, ext4) 다시 마운트. upper/, work/는 처음 뿌리의 것이다
//!   layers/                  층 디렉터리. 층을 열 때 작업 디렉터리를 여기로 두고 overlay 옵션에 상대 경로를 쓴다
//!     lower/0 … lower/k      이미지의 층 디스크들(erofs, `crate::disks`) 읽기 전용 다시 마운트. 0이 맨 아래
//!     1/ … n/                층마다의 위층
//!     1.work/ … n.work/      overlay 작업 디렉터리
//!     root/                  층 n: lowerdir=n-1:…:1:lower/k:…:lower/0, upperdir=n, workdir=n.work
//! ```
//!
//! **층 n 열기**
//!
//! 1. 연결 처리 프로세스의 뿌리를 처음 뿌리로 되돌린다(열어 둔 디렉터리로 `fchdir` 뒤 `chroot`).
//! 2. 층 n-1의 마운트(그 안의 `/proc` 등 포함)를 안쪽부터 내린다. 늦게 떼기(`MNT_DETACH`)를 쓰지 않는다.
//!    앞 단계의 프로세스가 남아 층을 쥐고 있으면 `EBUSY`로 실패한다. 위층이 아직 어딘가의 upper로 쓰이는 동안
//!    그것을 아래층으로 쓰면 overlay의 동작이 정의되지 않기 때문이다.
//! 3. 층 n을 `root/`에 마운트한다. 옵션 `redirect_dir=off,metacopy=off,index=off`: 앞의 둘은 위층에 OCI 층으로 옮길
//!    수 없는 표시(옮긴 디렉터리, 내용 없는 메타데이터 복사)를 남기므로 커널 설정의 기본값에 맡기지 않고 끈다.
//!    그래서 아래층 디렉터리의 이름 바꾸기는 `EXDEV`이고, `mv`는 복사와 지우기로 바꿔 한다. `index=off`이면 아래층의
//!    하드 링크는 위층으로 복사될 때 끊어진다(바꾼 이름만 위층에 생긴다).
//! 4. 그 안에 `/proc`, `/sys`(읽기 전용), `/dev`(`crate::devfs`의 링크와 함께), `/dev/pts`, `/dev/shm`을 마운트한다(부팅과 같은 옵션). 그 층에
//!    자리가 없으면 만들고, 내보낼 때 비어 있으면 빼고 보낸다(BuildKit과 같다). 자리가 디렉터리가 아니면(심볼릭
//!    링크 포함) 오류다. 링크를 따라 층 밖에 마운트하지 않는다.
//! 5. 투명 TCP가 켜져 있으면([`Overlay::with_resolver`]) 그 층의 `/etc/resolv.conf`를 보통의 샌드박스와 같은
//!    내용(`transparent::RESOLV_CONF`)으로 쓴다. 층 안에서는 처음 뿌리의 쓰기 층(부팅 뒤 sbx가 쓴 것)이 보이지
//!    않기 때문이다. 이 파일은 [`Planted`]로 내보내기에서 빠진다(단계가 바꾸지 않았으면). `/etc`가 없으면 만들고
//!    마운트 자리처럼 다룬다. `/etc`가 디렉터리가 아니면(링크 포함) 쓰지 않는다. 링크를 따라 층 밖에 쓰지 않는다.
//! 6. 그 안으로 chroot한다. 스레드는 뿌리를 함께 쓰므로 그 뒤의 요청과 그 요청이 띄운 프로세스는 층 n을 `/`로 본다.
//!
//! **상한**: 아래층은 상대 경로로 적어 마운트 옵션(한 페이지, 4096바이트) 안에 든다. 층 수는
//! [`MAX_LAYERS`]까지이고, 이미지의 층 디스크와 앞 층을 합친 아래층 수는 overlayfs `OVL_MAX_STACK`(500)까지다.
//! 넘으면 지금 층을 그대로 둔 채 오류다.
//!
//! **내보내기**: 위층 `layers/n`을 [`export_upper`]로 보낸다. 연결 처리 프로세스의 뿌리는 층 안이므로 열어 둔
//! `layers/`를 `/proc/self/fd/<fd>`로 찾아간다.
//!
//! **실패**: 열기가 중간에 실패하면 이미 한 마운트는 기록해 두고(다음 열기가 내린다) 파일 시스템 요청을 거부한다.
//! 다시 부팅하면(정지 뒤 시작) 남은 `layers/`를 지우고 1부터 다시 센다.

use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sbx_guest_proto::{Error, ErrorKind, MAX_LAYERS};

use crate::layer::{Kernel, Layers, Planted, export_upper};
use crate::sys::linux as sys;
use crate::wire::{Out, io_error};

/// scratch를 다시 마운트하는 곳(처음 뿌리 안).
pub const SCRATCH: &str = "/run/sbx/scratch";
/// 층 디렉터리.
pub const LAYERS: &str = "/run/sbx/scratch/layers";
/// 층마다 만드는 마운트 자리(층의 맨 위 이름). 없으면 만들고 내보낼 때 뺀다.
const STUBS: [&str; 3] = ["proc", "sys", "dev"];

/// 처음 열 때 층 디렉터리를 준비한다: 빈 디렉터리로 만들고 그 안에 이미지 rootfs(아래층들, [`Overlay::with_base`])를
/// 보이게 한다.
pub type Prepare = Box<dyn Fn(&Path) -> io::Result<()> + Send + Sync>;

/// overlay 아래층 수의 상한(Linux `OVL_MAX_STACK`).
const OVL_MAX_STACK: usize = 500;

/// overlay 층.
pub struct Overlay {
    dir: PathBuf,
    prepare: Prepare,
    /// 이미지 rootfs의 아래층들. 층 디렉터리 기준 상대 경로, 위가 앞이다. 기본은 `lower` 하나.
    base: Vec<String>,
    /// 연 층 안으로 연결 처리 프로세스의 뿌리를 옮긴다. 테스트는 옮기지 않고 `dir/root`를 본다.
    enter: bool,
    /// 층에 `/etc/resolv.conf`를 쓸지(투명 TCP가 켜졌는지). 없으면 쓰지 않는다.
    resolver: Option<Resolver>,
    state: Mutex<State>,
}

/// 층을 열 때마다 묻는다: 루프백 DNS를 가리키는 `/etc/resolv.conf`를 층에 둘지.
pub type Resolver = Box<dyn Fn() -> bool + Send + Sync>;

#[derive(Default, PartialEq, Eq)]
enum Phase {
    /// 빌드 전.
    #[default]
    Idle,
    /// 층 `count`가 열려 있다.
    Open,
    /// 열기가 실패했다.
    Broken,
}

#[derive(Default)]
struct State {
    phase: Phase,
    /// 처음 뿌리(`enter`일 때만).
    first_root: Option<File>,
    /// 층 디렉터리. 준비를 마쳤으면 있다.
    dir: Option<File>,
    /// 마지막으로 연 층 번호.
    count: u32,
    /// 층 디렉터리 기준으로 한 마운트, 한 순서대로.
    mounted: Vec<String>,
    /// 지금 층에서 만든 마운트 자리.
    stubs: Vec<String>,
    /// 지금 층에 sbx가 둔 파일.
    planted: Vec<Planted>,
}

fn failed(what: &str, e: &io::Error) -> Error {
    io_error(what, e)
}

impl Overlay {
    /// 만든다. 시스템 호출은 하지 않는다.
    pub fn new(dir: PathBuf, enter: bool, prepare: Prepare) -> Self {
        Self {
            dir,
            prepare,
            base: vec!["lower".into()],
            enter,
            resolver: None,
            state: Mutex::default(),
        }
    }

    /// 이미지 rootfs의 아래층들(층 디렉터리 기준 상대 경로, 위가 앞). `prepare`가 그 자리를 채운다.
    pub fn with_base(mut self, base: Vec<String>) -> Self {
        self.base = base;
        self
    }

    /// 층을 열 때 `resolver()`가 참이면 그 층에 `/etc/resolv.conf`를 둔다(모듈 문서의 5).
    pub fn with_resolver(mut self, resolver: Resolver) -> Self {
        self.resolver = Some(resolver);
        self
    }

    /// 게스트: scratch 장치와 이미지의 층 장치들(아래부터, `crate::disks`)을 처음 뿌리 안에 다시 마운트한다.
    pub fn guest(layer_devs: Vec<PathBuf>, scratch_dev: &'static str) -> Self {
        let base = (0..layer_devs.len())
            .rev()
            .map(|i| format!("lower/{i}"))
            .collect();
        Self::new(
            LAYERS.into(),
            true,
            Box::new(move |dir| {
                fs::create_dir_all(SCRATCH)?;
                sys::mount(scratch_dev, SCRATCH, "ext4", libc::MS_NOATIME, "")
                    .map_err(|e| io::Error::new(e.kind(), format!("mount scratch: {e}")))?;
                fresh_dir(dir)?;
                for (i, dev) in layer_devs.iter().enumerate() {
                    let lower = dir.join("lower").join(i.to_string());
                    fs::create_dir_all(&lower)?;
                    sys::mount(
                        &dev.to_string_lossy(),
                        &lower.to_string_lossy(),
                        "erofs",
                        libc::MS_RDONLY,
                        "",
                    )
                    .map_err(|e| {
                        io::Error::new(e.kind(), format!("mount {}: {e}", dev.display()))
                    })?;
                }
                Ok(())
            }),
        )
        .with_base(base)
    }

    fn open_locked(&self, st: &mut State, n: u32) -> Result<u32, Error> {
        if let Some(root) = &st.first_root {
            sys::fchdir(root).map_err(|e| failed("back to the first root", &e))?;
            std::os::unix::fs::chroot(".").map_err(|e| failed("back to the first root", &e))?;
        }
        if st.dir.is_none() {
            (self.prepare)(&self.dir).map_err(|e| failed("prepare build layers", &e))?;
            if self.enter {
                st.first_root = Some(File::open("/").map_err(|e| failed("/", &e))?);
            }
            st.dir = Some(File::open(&self.dir).map_err(|e| failed("build layers", &e))?);
        }
        let back = std::env::current_dir().ok();
        std::env::set_current_dir(&self.dir).map_err(|e| failed("build layers", &e))?;
        let r = self.mount_layer(st, n);
        if !self.enter
            && let Some(b) = back
        {
            let _ = std::env::set_current_dir(b);
        }
        r?;
        if self.enter {
            std::env::set_current_dir("root").map_err(|e| failed("enter layer", &e))?;
            std::os::unix::fs::chroot(".").map_err(|e| failed("enter layer", &e))?;
            std::env::set_current_dir("/").map_err(|e| failed("enter layer", &e))?;
        }
        st.count = n;
        Ok(n)
    }

    /// 작업 디렉터리가 층 디렉터리일 때 부른다.
    fn mount_layer(&self, st: &mut State, n: u32) -> Result<(), Error> {
        while let Some(m) = st.mounted.last() {
            sys::umount(m).map_err(|e| {
                if e.raw_os_error() == Some(libc::EBUSY) {
                    Error::new(
                        ErrorKind::Io,
                        format!(
                            "layer {}: still in use ({m}); a process from an earlier step is still running",
                            st.count
                        ),
                    )
                } else {
                    failed(&format!("unmount {m}"), &e)
                }
            })?;
            st.mounted.pop();
        }
        st.stubs.clear();
        st.planted.clear();
        let (upper, work) = (n.to_string(), format!("{n}.work"));
        fresh_dir(Path::new(&upper)).map_err(|e| failed(&upper, &e))?;
        fresh_dir(Path::new(&work)).map_err(|e| failed(&work, &e))?;
        fs::create_dir_all("root").map_err(|e| failed("root", &e))?;
        let lower: Vec<String> = (1..n)
            .rev()
            .map(|i| i.to_string())
            .chain(self.base.iter().cloned())
            .collect();
        let opts = format!(
            "lowerdir={},upperdir={upper},workdir={work},redirect_dir=off,metacopy=off,index=off",
            lower.join(":")
        );
        sys::mount("overlay", "root", "overlay", 0, &opts)
            .map_err(|e| failed(&format!("mount layer {n}"), &e))?;
        st.mounted.push("root".into());
        for name in STUBS {
            let p = format!("root/{name}");
            match fs::symlink_metadata(&p) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    return Err(Error::new(
                        ErrorKind::InvalidArgument,
                        format!("/{name} in the image is not a directory"),
                    ));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir(&p).map_err(|e| failed(&p, &e))?;
                    st.stubs.push(name.into());
                }
                Err(e) => return Err(failed(&p, &e)),
            }
        }
        if self.resolver.as_ref().is_some_and(|f| f()) {
            plant_resolver(Path::new("root"), st)?;
        }
        let nosuid = libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC;
        let mounts: [(&str, &str, &str, libc::c_ulong, &str); 5] = [
            ("proc", "root/proc", "proc", nosuid, ""),
            ("sysfs", "root/sys", "sysfs", nosuid | libc::MS_RDONLY, ""),
            (
                "devtmpfs",
                "root/dev",
                "devtmpfs",
                libc::MS_NOSUID,
                "mode=0755",
            ),
            (
                "devpts",
                "root/dev/pts",
                "devpts",
                libc::MS_NOSUID | libc::MS_NOEXEC,
                "newinstance,ptmxmode=0666,mode=0620",
            ),
            ("shm", "root/dev/shm", "tmpfs", nosuid, "mode=1777"),
        ];
        for (source, target, fstype, flags, data) in mounts {
            if target.starts_with("root/dev/") {
                // devtmpfs 안이다. 층에 쓰지 않는다.
                fs::create_dir_all(target).map_err(|e| failed(target, &e))?;
            }
            sys::mount(source, target, fstype, flags, data)
                .map_err(|e| failed(&format!("mount {fstype} on /{}", &target[5..]), &e))?;
            st.mounted.push(target.into());
            if target == "root/dev" {
                // 보통의 샌드박스와 같은 `/dev/fd`, `/dev/stdin` 등. devtmpfs 안이라 층에 쓰지 않는다.
                crate::devfs::add_links(Path::new(target)).map_err(|e| failed("/dev links", &e))?;
            }
        }
        Ok(())
    }
}

/// 층 뿌리 `root`에 루프백 DNS를 가리키는 `etc/resolv.conf`를 쓰고 [`Planted`]로 적는다(모듈 문서의 5).
fn plant_resolver(root: &Path, st: &mut State) -> Result<(), Error> {
    let etc = root.join("etc");
    match fs::symlink_metadata(&etc) {
        Ok(m) if m.is_dir() => {}
        // 링크나 파일이면 쓰지 않는다(따라가면 층 밖일 수 있다).
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&etc).map_err(|e| failed("/etc", &e))?;
            st.stubs.push("etc".into());
        }
        Err(e) => return Err(failed("/etc", &e)),
    }
    crate::transparent::write_resolv_conf(root).map_err(|e| failed("/etc/resolv.conf", &e))?;
    st.planted.push(Planted {
        path: "etc/resolv.conf".into(),
        content: crate::transparent::RESOLV_CONF.as_bytes().to_vec(),
    });
    Ok(())
}

/// 비어 있는 새 디렉터리로 만든다. 있던 것은 지운다(실패한 열기나 이전 부팅이 남긴 것).
fn fresh_dir(p: &Path) -> io::Result<()> {
    match fs::remove_dir_all(p) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    fs::create_dir(p)
}

impl Layers for Overlay {
    fn open(&self) -> Result<u32, Error> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let n = st.count + 1;
        // 지금 층을 건드리기 전에 본다. 열린 층은 그대로 쓸 수 있다.
        if n > MAX_LAYERS {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!("a build can have at most {MAX_LAYERS} layers"),
            ));
        }
        if (n as usize - 1) + self.base.len() > OVL_MAX_STACK {
            return Err(Error::new(
                ErrorKind::InvalidArgument,
                format!(
                    "the image has {} layer disks; with the build layers that is more than the \
                     {OVL_MAX_STACK} lower layers overlayfs takes",
                    self.base.len()
                ),
            ));
        }
        match self.open_locked(&mut st, n) {
            Ok(n) => {
                st.phase = Phase::Open;
                Ok(n)
            }
            Err(e) => {
                st.phase = Phase::Broken;
                Err(e)
            }
        }
    }

    fn export(&self, out: &Out) -> Result<(), Error> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (Phase::Open, Some(dir)) = (&st.phase, &st.dir) else {
            return Err(Error::new(ErrorKind::InvalidArgument, "no layer is open"));
        };
        let upper = if self.enter {
            PathBuf::from(format!("/proc/self/fd/{}/{}", dir.as_raw_fd(), st.count))
        } else {
            self.dir.join(st.count.to_string())
        };
        export_upper(&upper, &st.stubs, &st.planted, &Kernel, out)
    }

    fn is_open(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).phase == Phase::Open
    }

    fn check_ready(&self) -> Result<(), Error> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.phase == Phase::Broken {
            return Err(Error::new(
                ErrorKind::Io,
                "opening the last build layer failed; open a layer again",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
