//! Firecracker용 실행기 (`sbx-executor-firecracker`). sbx의 실행기 규약(`executor-proto`)과 Firecracker의
//! 공개 인터페이스(설정 파일, vsock Unix 소켓) 사이를 잇는 adapter다.
//!
//! Firecracker 쪽은 표준 그대로 쓴다: `firecracker --no-api --config-file <파일>`, 설정 파일 형식,
//! vsock의 `CONNECT <포트>` 방식. 그래서 upstream Firecracker(Linux KVM)와, 같은 인터페이스를 구현한
//! 다른 빌드(예: macOS HVF 포트)에서 똑같이 돈다. sbx만을 위한 Firecracker 기능은 쓰지 않는다.
//!
//! # 찾는 것 ([`settings`])
//!
//! | 무엇 | 순서 |
//! |---|---|
//! | firecracker 실행 파일 | `SBX_FIRECRACKER` > 이 실행 파일 옆의 `firecracker` > PATH의 `firecracker` |
//! | 기본 게스트 커널 | `SBX_FIRECRACKER_KERNEL` > 이 실행 파일 옆의 `kernel` |
//!
//! 커널은 sbx 게스트 약속(`executor-proto` 문서)의 기능을 갖춰야 한다. 특히 erofs, virtio-vsock, overlay.
//! firecracker와 기본 커널 중 하나라도 없으면 `capabilities`가 실패하고 이유를 쓴다.
//!
//! 이미지가 자기 커널을 담고 있으면 sbx가 그것을 `VmSpec::kernel`로 주고(`custom_kernel: true`), 그 커널로 부팅한다.
//! Firecracker가 읽는 형식이어야 한다(x86_64는 ELF `vmlinux`, aarch64는 `Image`).
//!
//! # VM ([`config`])
//!
//! - initrd는 sbx가 준 게스트 init, `root=`는 없다. 디스크는 받은 순서대로(rootfs `vda` 읽기 전용, scratch `vdb`).
//! - vsock: guest CID 3, `uds_path`는 규약의 `control_socket`. 남아 있는 파일은 지운다(Firecracker는 있으면 실패한다).
//!   경로는 [`config::MAX_SOCKET_PATH`] 바이트 이하여야 한다(macOS Unix 소켓 한도).
//! - 커널 명령줄: `console=ttyS0 reboot=k panic=1`(x86_64는 `pci=off`도), `sbx.hostname=`, `sbx.halt=reboot`
//!   (Firecracker는 게스트의 재시작 요청을 VM 종료로 다룬다).
//! - 네트워크 장치와 공유 폴더(virtio-fs)는 없다(`live_share: false`). 스냅샷도 아직 알리지 않는다.
//!
//! # 프로세스 ([`session`])
//!
//! - `run`: `Launch`를 받아 설정 파일을 쓰고 firecracker를 띄운다. vsock 소켓 파일이 생기면 `Started`,
//!   그 전에 firecracker가 끝나면 `Error`다. firecracker의 출력(게스트 시리얼 콘솔 포함)은 표준 오류(실행기 로그)로 간다.
//! - firecracker는 이 실행 파일의 `vmm` 모드(감시 프로세스) 아래에서 돈다. 감시 프로세스는 표준 입력이 닫히면
//!   firecracker를 죽인다. 표준 입력은 `run` 세션이 쥐고 있으므로 `Kill`, 세션의 입력 끝, 세션 프로세스가
//!   강제로 죽는 경우 모두 VM이 끝난다(고아 VM이 남지 않는다).
//! - VM이 끝나면 `Exited`: firecracker가 0으로 끝나면 `guest powered off`, 죽였으면 `killed`.

pub mod config;
pub mod session;
pub mod settings;

use std::process::ExitCode;

/// 진입점. `capabilities`, `run`, `vmm`(내부용).
pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("capabilities") => session::capabilities(),
        Some("run") => session::run(),
        Some(session::VMM) => session::vmm(&args[1..]),
        _ => {
            eprintln!("usage: sbx-executor-firecracker capabilities|run");
            ExitCode::from(2)
        }
    }
}
