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

/// 버퍼에 있는 파일 시스템 쓰기를 디스크로 내린다.
pub fn sync() {
    // SAFETY: sync는 인자가 없고 실패하지 않는다.
    unsafe { libc::sync() }
}
