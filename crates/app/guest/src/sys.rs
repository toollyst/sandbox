//! 시스템 호출. 게스트에서 `unsafe`가 있는 곳은 여기뿐이다.
//!
//! 함수마다 안전 조건을 적고, 밖으로는 안전한 함수만 내보낸다.
#![allow(unsafe_code)]

use std::io;

fn check(ret: libc::c_int) -> io::Result<()> {
    if ret == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// 프로세스에 시그널을 보낸다.
pub fn kill(pid: u32, signal: i32) -> io::Result<()> {
    let pid =
        libc::pid_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    if pid <= 0 {
        // 0과 음수는 프로세스 그룹 전체를 뜻한다. 여기서는 프로세스 하나만 받는다.
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: kill은 메모리를 건드리지 않는다.
    check(unsafe { libc::kill(pid, signal) })
}

/// Linux 번호가 다른 시그널: (Linux 번호, 이 OS의 번호). Linux에서는 비어 있다.
#[cfg(target_os = "linux")]
const RENUMBERED: &[(i32, i32)] = &[];
#[cfg(not(target_os = "linux"))]
const RENUMBERED: &[(i32, i32)] = &[
    (7, libc::SIGBUS),
    (10, libc::SIGUSR1),
    (12, libc::SIGUSR2),
    (17, libc::SIGCHLD),
    (18, libc::SIGCONT),
    (19, libc::SIGSTOP),
    (20, libc::SIGTSTP),
    (21, libc::SIGTTIN),
    (22, libc::SIGTTOU),
    (23, libc::SIGURG),
    (24, libc::SIGXCPU),
    (25, libc::SIGXFSZ),
    (26, libc::SIGVTALRM),
    (27, libc::SIGPROF),
    (28, libc::SIGWINCH),
    (29, libc::SIGIO),
    (31, libc::SIGSYS),
];

/// 호스트가 보낸 Linux 시그널 번호를 이 OS의 번호로. 프로토콜의 시그널 번호는 모두 Linux 번호다.
/// 게스트(Linux)에서는 그대로다. 다른 OS(호스트에서 게스트 처리를 돌리는 fake 실행기)에서는 이름이 같은
/// 시그널로 바꾸고, 모르는 번호는 그대로 둔다.
pub fn from_linux_signal(n: i32) -> i32 {
    RENUMBERED
        .iter()
        .find(|(linux, _)| *linux == n)
        .map_or(n, |(_, here)| *here)
}

/// [`from_linux_signal`]의 반대. 프로세스를 끝낸 시그널을 호스트에 알릴 때 쓴다.
pub fn to_linux_signal(n: i32) -> i32 {
    RENUMBERED
        .iter()
        .find(|(_, here)| *here == n)
        .map_or(n, |(linux, _)| *linux)
}

/// `fd`에서 막히지 않고 읽을 수 있을 때까지(데이터가 있거나 닫힘) 최대 `timeout` 기다린다. 읽을 수 있으면 `true`.
pub fn poll_readable(
    fd: std::os::fd::BorrowedFd<'_>,
    timeout: std::time::Duration,
) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: p는 살아 있는 pollfd 하나이고 개수는 1이다. fd는 BorrowedFd라 그동안 열려 있다.
        let n = unsafe { libc::poll(&mut p, 1, ms) };
        if n == -1 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(n > 0);
    }
}

/// 소켓의 보내는 쪽(`both`면 양쪽)을 닫는다.
pub fn shutdown(fd: std::os::fd::BorrowedFd<'_>, both: bool) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let how = if both { libc::SHUT_RDWR } else { libc::SHUT_WR };
    // SAFETY: fd는 BorrowedFd라 열려 있다. shutdown은 메모리를 건드리지 않는다.
    check(unsafe { libc::shutdown(fd.as_raw_fd(), how) })
}

/// 의사 터미널 한 쌍 (주, 종)을 그 크기로 연다. 둘 다 exec할 때 닫히게(`FD_CLOEXEC`) 해 둔다.
pub fn openpty(rows: u16, cols: u16) -> io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use std::os::fd::{FromRawFd, OwnedFd};
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let mut ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: 출력 인자는 살아 있는 지역 변수다. 이름 버퍼는 넘기지 않고(null), termios는 기본값(null)이다.
    // 창 크기는 Linux에서 `*const`, macOS에서 `*mut`이라 원시 포인터로 넘긴다(어느 쪽이든 읽기만 한다).
    check(unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut ws,
        )
    })?;
    // SAFETY: openpty가 성공했으므로 둘 다 새로 열린, 이 프로세스만 가진 fd다.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    for fd in [&master, &slave] {
        use std::os::fd::AsRawFd;
        // SAFETY: fd는 열려 있다. F_SETFD는 fd 플래그만 바꾼다.
        check(unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) })?;
    }
    Ok((master, slave))
}

/// 터미널 크기를 바꾼다. 그 터미널의 전경 프로세스 그룹은 SIGWINCH를 받는다.
pub fn set_winsize(fd: std::os::fd::BorrowedFd<'_>, rows: u16, cols: u16) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: fd는 BorrowedFd라 열려 있고, TIOCSWINSZ는 winsize를 읽기만 한다.
    // 요청 번호의 타입은 libc마다 달라 `as _`로 맞춘다.
    check(unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) })
}

/// 자식이 새 세션을 만들고 표준 입력(터미널)을 제어 터미널로 삼게 한다. 표준 입출력은 이미 터미널이어야 한다.
pub fn controlling_tty_on_stdin(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: 닫힘은 fork 뒤 exec 전에 자식에서 돈다. 그 안에서는 async-signal-safe 함수(setsid, ioctl)만 부르고
    // 메모리를 할당하지 않는다. std는 이 닫힘보다 먼저 표준 입출력을 맞춰 둔다.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// 자식 프로세스가 끝날 때까지 기다리되 거두지 않는다(`WNOWAIT`).
///
/// 이것이 돌아온 뒤에도 PID는 아직 재사용되지 않는다. 그래서 그 사이에 보내는 시그널이
/// 다른 프로세스에 가지 않는다. 거두는 것은 `Child::wait`가 한다.
pub fn wait_exited_nowait(pid: u32) -> io::Result<()> {
    let id = libc::id_t::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    loop {
        // SAFETY: siginfo_t는 정수 필드만 있는 C 구조체라 0으로 채워도 된다.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: info는 이 호출 동안 살아 있는 올바른 포인터다.
        let ret =
            unsafe { libc::waitid(libc::P_PID, id, &mut info, libc::WEXITED | libc::WNOWAIT) };
        match check(ret) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            r => return r,
        }
    }
}

/// 유효 사용자가 root인지.
pub fn is_root() -> bool {
    // SAFETY: geteuid는 인자가 없고 실패하지 않는다.
    unsafe { libc::geteuid() == 0 }
}

/// 버퍼에 있는 파일 시스템 쓰기를 디스크로 내린다.
pub fn sync() {
    // SAFETY: sync는 인자가 없고 실패하지 않는다.
    unsafe { libc::sync() }
}

/// `path`가 있는 파일 시스템의 (전체, 쓴 것) 바이트.
// statvfs 필드의 C 형은 OS마다 다르다(Linux는 u64, macOS는 u32도 있다).
#[allow(clippy::unnecessary_cast)]
pub fn disk_usage(path: &str) -> io::Result<(u64, u64)> {
    let c =
        std::ffi::CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: statvfs는 정수 필드만 있는 C 구조체라 0으로 채워도 되고, c는 NUL로 끝나는 문자열이다.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    check(unsafe { libc::statvfs(c.as_ptr(), &mut st) })?;
    let frsize = st.f_frsize as u64;
    let total = (st.f_blocks as u64).saturating_mul(frsize);
    let free = (st.f_bfree as u64).saturating_mul(frsize);
    Ok((total, total.saturating_sub(free)))
}

/// 장치 번호(`st_rdev`)를 (major, minor)로 나눈다. 인코딩은 OS마다 다르다.
pub fn dev_split(rdev: u64) -> (u32, u32) {
    #[cfg(target_os = "linux")]
    {
        // glibc `gnu_dev_major`, `gnu_dev_minor`와 같다.
        let major = ((rdev >> 8) & 0xfff) | ((rdev >> 32) & !0xfff);
        let minor = (rdev & 0xff) | ((rdev >> 12) & !0xff);
        (major as u32, minor as u32)
    }
    #[cfg(not(target_os = "linux"))]
    {
        // BSD와 macOS: 위 8비트가 major, 아래 24비트가 minor.
        (((rdev >> 24) & 0xff) as u32, (rdev & 0xff_ffff) as u32)
    }
}

/// netlink `RTM_NEWROUTE` 메시지 하나: `ip route replace local <dst>/<prefix> dev <ifindex>`와 같다
/// (표 local, 종류 local, 범위 host). 응답(ack)을 달라고 한다. 헤더는 이 CPU의 바이트 순서, 주소는 네트워크 순서다.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn local_route_message(dst: std::net::Ipv4Addr, prefix: u8, ifindex: u32, seq: u32) -> Vec<u8> {
    const RTM_NEWROUTE: u16 = 24;
    const NLM_F_REQUEST: u16 = 0x1;
    const NLM_F_ACK: u16 = 0x4;
    const NLM_F_REPLACE: u16 = 0x100;
    const NLM_F_CREATE: u16 = 0x400;
    const AF_INET: u8 = 2;
    const RT_TABLE_LOCAL: u8 = 255;
    const RTPROT_STATIC: u8 = 4;
    const RT_SCOPE_HOST: u8 = 254;
    const RTN_LOCAL: u8 = 2;
    const RTA_DST: u16 = 1;
    const RTA_OIF: u16 = 4;
    let mut m = Vec::with_capacity(44);
    m.extend_from_slice(&44u32.to_ne_bytes());
    m.extend_from_slice(&RTM_NEWROUTE.to_ne_bytes());
    m.extend_from_slice(&(NLM_F_REQUEST | NLM_F_ACK | NLM_F_REPLACE | NLM_F_CREATE).to_ne_bytes());
    m.extend_from_slice(&seq.to_ne_bytes());
    m.extend_from_slice(&0u32.to_ne_bytes());
    // rtmsg: family, dst_len, src_len, tos, table, protocol, scope, type, flags.
    m.extend_from_slice(&[
        AF_INET,
        prefix,
        0,
        0,
        RT_TABLE_LOCAL,
        RTPROT_STATIC,
        RT_SCOPE_HOST,
        RTN_LOCAL,
    ]);
    m.extend_from_slice(&0u32.to_ne_bytes());
    // rtattr: 길이, 종류, 값.
    m.extend_from_slice(&8u16.to_ne_bytes());
    m.extend_from_slice(&RTA_DST.to_ne_bytes());
    m.extend_from_slice(&dst.octets());
    m.extend_from_slice(&8u16.to_ne_bytes());
    m.extend_from_slice(&RTA_OIF.to_ne_bytes());
    m.extend_from_slice(&ifindex.to_ne_bytes());
    m
}

/// netlink 응답에서 `seq`의 ack를 찾는다: `Some(0)`이면 성공, `Some(errno)`이면 실패, 없으면 `None`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn netlink_ack(buf: &[u8], seq: u32) -> Option<i32> {
    const NLMSG_ERROR: u16 = 2;
    let mut at = 0;
    while at + 16 <= buf.len() {
        let word = |o: usize| u32::from_ne_bytes(buf[o..o + 4].try_into().unwrap_or_default());
        let len = word(at) as usize;
        if len < 16 || at + len > buf.len() {
            return None;
        }
        let kind = u16::from_ne_bytes([buf[at + 4], buf[at + 5]]);
        if kind == NLMSG_ERROR && word(at + 8) == seq && len >= 20 {
            let err = i32::from_ne_bytes(buf[at + 16..at + 20].try_into().unwrap_or_default());
            return Some(-err);
        }
        // 다음 메시지는 4바이트 경계에서 시작한다.
        at += (len + 3) & !3;
    }
    None
}

/// Linux 전용 시스템 호출. 부팅(`boot`)과 빌드 층(`overlay`, `layer`)과 투명 TCP(`transparent`)가 쓴다.
#[cfg(target_os = "linux")]
pub mod linux {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::check;

    fn cstr(s: &str) -> io::Result<CString> {
        CString::new(s).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    fn cpath(p: &Path) -> io::Result<CString> {
        CString::new(p.as_os_str().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
    }

    /// `umount2(2)`, 플래그 없이. 쓰는 프로세스가 있으면 `EBUSY`로 실패한다(늦게 떼기를 쓰지 않는다).
    pub fn umount(target: &str) -> io::Result<()> {
        let t = cstr(target)?;
        // SAFETY: t는 이 호출 동안 살아 있는 NUL 종료 문자열이다.
        check(unsafe { libc::umount2(t.as_ptr(), 0) })
    }

    /// 작업 디렉터리를 열린 디렉터리로 바꾼다(`fchdir(2)`). chroot 밖의 디렉터리여도 된다.
    pub fn fchdir(dir: &File) -> io::Result<()> {
        // SAFETY: fchdir은 열린 fd 하나만 받는다. dir이 이 호출 동안 살아 있다.
        check(unsafe { libc::fchdir(dir.as_raw_fd()) })
    }

    /// 확장 속성 이름 목록. 심볼릭 링크를 따라가지 않는다(`llistxattr(2)`). xattr이 없는 파일 시스템이면 빈 목록이다.
    pub fn list_xattrs(path: &Path) -> io::Result<Vec<Vec<u8>>> {
        let p = cpath(path)?;
        loop {
            // SAFETY: 크기 0이면 버퍼를 쓰지 않고 필요한 크기만 돌려준다.
            let need = unsafe { libc::llistxattr(p.as_ptr(), std::ptr::null_mut(), 0) };
            if need < 0 {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::ENOTSUP) => Ok(Vec::new()),
                    _ => Err(e),
                };
            }
            if need == 0 {
                return Ok(Vec::new());
            }
            let mut buf = vec![0u8; need as usize];
            // SAFETY: buf는 buf.len() 바이트를 쓸 수 있다.
            let got = unsafe { libc::llistxattr(p.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
            if got < 0 {
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ERANGE) {
                    continue; // 그 사이에 늘었다.
                }
                return Err(e);
            }
            buf.truncate(got as usize);
            return Ok(buf
                .split(|b| *b == 0)
                .filter(|n| !n.is_empty())
                .map(<[u8]>::to_vec)
                .collect());
        }
    }

    /// 확장 속성 값 하나. 심볼릭 링크를 따라가지 않는다(`lgetxattr(2)`). 없으면 `None`.
    pub fn get_xattr(path: &Path, name: &[u8]) -> io::Result<Option<Vec<u8>>> {
        let p = cpath(path)?;
        let n = CString::new(name).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        loop {
            // SAFETY: 크기 0이면 버퍼를 쓰지 않는다.
            let need = unsafe { libc::lgetxattr(p.as_ptr(), n.as_ptr(), std::ptr::null_mut(), 0) };
            if need < 0 {
                let e = io::Error::last_os_error();
                return match e.raw_os_error() {
                    Some(libc::ENODATA) | Some(libc::ENOTSUP) => Ok(None),
                    _ => Err(e),
                };
            }
            let mut buf = vec![0u8; need as usize];
            // SAFETY: buf는 buf.len() 바이트를 쓸 수 있다.
            let got = unsafe {
                libc::lgetxattr(p.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len())
            };
            if got < 0 {
                let e = io::Error::last_os_error();
                match e.raw_os_error() {
                    Some(libc::ERANGE) => continue,
                    Some(libc::ENODATA) => return Ok(None),
                    _ => return Err(e),
                }
            }
            buf.truncate(got as usize);
            return Ok(Some(buf));
        }
    }

    /// `mount(2)`.
    pub fn mount(
        source: &str,
        target: &str,
        fstype: &str,
        flags: libc::c_ulong,
        data: &str,
    ) -> io::Result<()> {
        let (s, t, f, d) = (cstr(source)?, cstr(target)?, cstr(fstype)?, cstr(data)?);
        // SAFETY: 모든 포인터는 이 호출 동안 살아 있는 NUL 종료 문자열이다.
        check(unsafe { libc::mount(s.as_ptr(), t.as_ptr(), f.as_ptr(), flags, d.as_ptr().cast()) })
    }

    /// 마운트 하나를 옮긴다(`MS_MOVE`).
    pub fn move_mount(source: &str, target: &str) -> io::Result<()> {
        let (s, t) = (cstr(source)?, cstr(target)?);
        // SAFETY: 위와 같다. MS_MOVE에서는 fstype과 data를 쓰지 않으므로 null이어도 된다.
        check(unsafe {
            libc::mount(
                s.as_ptr(),
                t.as_ptr(),
                std::ptr::null(),
                libc::MS_MOVE,
                std::ptr::null(),
            )
        })
    }

    /// VM을 끝낸다. 돌아오면 실패한 것이다.
    pub fn reboot(cmd: libc::c_int) -> io::Error {
        // SAFETY: reboot은 메모리를 건드리지 않는다.
        unsafe { libc::reboot(cmd) };
        io::Error::last_os_error()
    }

    /// 호스트 이름.
    pub fn sethostname(name: &str) -> io::Result<()> {
        // SAFETY: 포인터와 길이가 같은 바이트열을 가리킨다.
        check(unsafe { libc::sethostname(name.as_ptr().cast(), name.len()) })
    }

    /// 벽시계(`CLOCK_REALTIME`)에 `secs`초 + `nanos`나노초를 바로 더한다(`ADJ_SETOFFSET`). `nanos`는 `0..1e9`여야 한다
    /// (음수 차이는 초를 내려 나타낸다). 읽고 쓰는 사이의 틈이 없다(커널이 한 번에 더한다).
    pub fn clock_step(secs: i64, nanos: i64) -> io::Result<()> {
        // SAFETY: timex는 정수 필드뿐이라 0으로 채운 값이 올바르다.
        let mut t: libc::timex = unsafe { std::mem::zeroed() };
        t.modes = libc::ADJ_SETOFFSET | libc::ADJ_NANO;
        // 게스트는 64비트뿐이라(x86_64, aarch64) 필드가 모두 i64다. 다른 크기면 여기서 컴파일되지 않는다.
        t.time.tv_sec = secs;
        // ADJ_NANO에서는 tv_usec이 나노초다.
        t.time.tv_usec = nanos;
        // SAFETY: t는 이 호출 동안 살아 있는 timex다.
        check(unsafe { libc::adjtimex(&mut t) })
    }

    /// 벽시계를 `micros` 마이크로초만큼 천천히 옮긴다(`ADJ_OFFSET_SINGLESHOT`, `adjtime(3)`과 같다). 커널은 1초에
    /// 0.5ms까지 고친다. 남은 몫이 있으면 이 값으로 바꾼다. 0이면 진행 중인 것을 취소한다.
    pub fn clock_slew_micros(micros: i64) -> io::Result<()> {
        // SAFETY: timex는 정수 필드뿐이라 0으로 채운 값이 올바르다.
        let mut t: libc::timex = unsafe { std::mem::zeroed() };
        // 이 방식은 다른 비트와 함께 쓰지 않는다(커널이 EINVAL).
        t.modes = libc::ADJ_OFFSET_SINGLESHOT;
        t.offset = micros;
        // SAFETY: t는 이 호출 동안 살아 있는 timex다.
        check(unsafe { libc::adjtimex(&mut t) })
    }

    /// 아무 자식이나 끝나면 거둔다. (pid, 종료 상태). 자식이 없으면 `ECHILD` 오류.
    pub fn wait_any() -> io::Result<(u32, libc::c_int)> {
        let mut status = 0;
        loop {
            // SAFETY: status는 이 호출 동안 살아 있다.
            let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
            if pid >= 0 {
                return Ok((pid as u32, status));
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }

    /// 루프백 인터페이스를 올린다.
    // ioctl 요청 번호의 타입이 glibc(c_ulong)와 musl(c_int)에서 다르다. 한쪽에서는 같은 타입으로의 변환이다.
    #[allow(clippy::unnecessary_cast)]
    pub fn loopback_up() -> io::Result<()> {
        // SAFETY: socket은 새 fd를 돌려주거나 -1이다.
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        check(fd)?;
        // SAFETY: fd는 방금 만든, 아무도 소유하지 않은 fd다.
        let sock = unsafe { OwnedFd::from_raw_fd(fd) };
        use std::os::fd::AsRawFd;
        // SAFETY: ifreq는 정수와 바이트 배열로 된 C 구조체라 0으로 채워도 된다.
        let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
        for (d, s) in req.ifr_name.iter_mut().zip(b"lo\0") {
            *d = *s as libc::c_char;
        }
        // SAFETY: req는 살아 있는 ifreq이고, 이 ioctl들은 ifreq를 읽고 쓴다.
        check(unsafe {
            libc::ioctl(
                sock.as_raw_fd(),
                libc::SIOCGIFFLAGS as libc::Ioctl,
                &mut req,
            )
        })?;
        // SAFETY: SIOCGIFFLAGS가 채운 공용체의 flags 필드를 읽고 쓴다.
        unsafe {
            req.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short;
        }
        // SAFETY: 위와 같다.
        check(unsafe { libc::ioctl(sock.as_raw_fd(), libc::SIOCSIFFLAGS as libc::Ioctl, &req) })
    }

    /// `dst/prefix`를 루프백의 local 경로로 잡는다(`ip route replace local dst/prefix dev lo`). 이미 있으면 그대로다.
    /// 그 뒤로 범위 안의 어느 주소에서나 듣고 연결할 수 있다.
    pub fn add_local_route(dst: std::net::Ipv4Addr, prefix: u8) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: 이름은 NUL로 끝나는 정적 문자열이다.
        let lo = unsafe { libc::if_nametoindex(c"lo".as_ptr()) };
        if lo == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socket은 새 fd를 돌려주거나 -1이다.
        let fd = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        check(fd)?;
        // SAFETY: fd는 방금 만든, 아무도 소유하지 않은 fd다.
        let sock = unsafe { OwnedFd::from_raw_fd(fd) };
        let seq = 1;
        let msg = super::local_route_message(dst, prefix, lo, seq);
        // SAFETY: sockaddr_nl은 정수 필드만 있어 0으로 채워도 된다(nl_pid 0은 커널).
        let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        // SAFETY: msg와 kernel은 이 호출 동안 살아 있고 길이는 그 크기다.
        let sent = unsafe {
            libc::sendto(
                sock.as_raw_fd(),
                msg.as_ptr().cast(),
                msg.len(),
                0,
                (&kernel as *const libc::sockaddr_nl).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut buf = vec![0u8; 8192];
        loop {
            // SAFETY: buf는 buf.len() 바이트를 쓸 수 있다.
            let n = unsafe { libc::recv(sock.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            match super::netlink_ack(&buf[..n as usize], seq) {
                Some(0) => return Ok(()),
                Some(errno) => return Err(io::Error::from_raw_os_error(errno)),
                None if n == 0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                None => {}
            }
        }
    }

    /// vsock 포트에서 듣는다.
    pub fn vsock_listen(port: u32) -> io::Result<OwnedFd> {
        // SAFETY: socket은 새 fd를 돌려주거나 -1이다.
        let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
        check(fd)?;
        // SAFETY: fd는 방금 만든, 아무도 소유하지 않은 fd다.
        let sock = unsafe { OwnedFd::from_raw_fd(fd) };
        use std::os::fd::AsRawFd;
        // SAFETY: sockaddr_vm은 정수 필드만 있어 0으로 채워도 된다.
        let mut addr: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
        addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
        addr.svm_port = port;
        addr.svm_cid = libc::VMADDR_CID_ANY;
        // SAFETY: addr는 살아 있고, 길이는 그 구조체의 크기다.
        check(unsafe {
            libc::bind(
                sock.as_raw_fd(),
                (&addr as *const libc::sockaddr_vm).cast(),
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            )
        })?;
        // SAFETY: listen은 메모리를 건드리지 않는다.
        check(unsafe { libc::listen(sock.as_raw_fd(), 16) })?;
        Ok(sock)
    }

    /// 연결 하나를 받는다. 읽기와 쓰기는 일반 파일처럼 한다.
    pub fn accept(listener: &OwnedFd) -> io::Result<File> {
        use std::os::fd::AsRawFd;
        loop {
            // SAFETY: 주소를 받지 않으므로 null을 넘긴다.
            let fd = unsafe {
                libc::accept4(
                    listener.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            if fd >= 0 {
                // SAFETY: accept4가 돌려준 새 fd다.
                return Ok(File::from(unsafe { OwnedFd::from_raw_fd(fd) }));
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_route_message_is_laid_out_as_the_kernel_reads_it() {
        let m = local_route_message(std::net::Ipv4Addr::new(198, 18, 0, 0), 15, 1, 7);
        assert_eq!(m.len(), 44);
        assert_eq!(u32::from_ne_bytes(m[0..4].try_into().unwrap()), 44);
        assert_eq!(u16::from_ne_bytes([m[4], m[5]]), 24);
        assert_eq!(u16::from_ne_bytes([m[6], m[7]]), 0x505);
        assert_eq!(u32::from_ne_bytes(m[8..12].try_into().unwrap()), 7);
        assert_eq!(&m[16..24], &[2, 15, 0, 0, 255, 4, 254, 2]);
        assert_eq!(
            &m[28..32],
            &[8u16.to_ne_bytes(), 1u16.to_ne_bytes()].concat()[..]
        );
        assert_eq!(&m[32..36], &[198, 18, 0, 0]);
        assert_eq!(
            &m[36..40],
            &[8u16.to_ne_bytes(), 4u16.to_ne_bytes()].concat()[..]
        );
        assert_eq!(u32::from_ne_bytes(m[40..44].try_into().unwrap()), 1);
    }

    #[test]
    fn netlink_acks_are_found_by_sequence() {
        let ack = |seq: u32, err: i32| {
            let mut m = Vec::new();
            m.extend_from_slice(&36u32.to_ne_bytes());
            m.extend_from_slice(&2u16.to_ne_bytes());
            m.extend_from_slice(&0u16.to_ne_bytes());
            m.extend_from_slice(&seq.to_ne_bytes());
            m.extend_from_slice(&0u32.to_ne_bytes());
            m.extend_from_slice(&err.to_ne_bytes());
            m.extend_from_slice(&[0; 16]);
            m
        };
        assert_eq!(netlink_ack(&ack(1, 0), 1), Some(0));
        assert_eq!(netlink_ack(&ack(1, -17), 1), Some(17));
        assert_eq!(netlink_ack(&[ack(9, -1), ack(1, 0)].concat(), 1), Some(0));
        assert_eq!(netlink_ack(&ack(2, 0), 1), None);
        assert_eq!(netlink_ack(&ack(1, 0)[..10], 1), None);
        let mut lying = ack(1, 0);
        lying[0] = 200;
        assert_eq!(netlink_ack(&lying, 1), None);
        assert_eq!(netlink_ack(&[], 1), None);
    }
}
