//! Firecracker용 실행기 (`sbx-executor-firecracker`). sbx의 실행기 규약(`sbx-executor-proto`)과 Firecracker의
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
//! | firecracker 실행 파일 | `SBX_FIRECRACKER` > sbx가 준 VMM 묶음(`SBX_EXECUTOR_VMM_DIR`) 안의 `firecracker` > 이 실행 파일 옆의 `firecracker` > PATH의 `firecracker` |
//! | 기본 게스트 커널 | `SBX_FIRECRACKER_KERNEL` > 이 실행 파일 옆의 `kernel` |
//! | jailer (root일 때만) | `SBX_FIRECRACKER_JAILER` > 고른 firecracker와 같은 디렉터리의 `jailer`. `SBX_FIRECRACKER_JAILER=off`면 쓰지 않는다 |
//!
//! 커널은 sbx 게스트 약속(`sbx-executor-proto` 문서)의 기능을 갖춰야 한다. 특히 erofs, virtio-vsock, overlay.
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
//! - 커널 명령줄: `console=ttyS0 quiet reboot=k panic=1`(x86_64는 `pci=off`도), `sbx.hostname=`, `sbx.halt=reboot`
//!   (Firecracker는 게스트의 재시작 요청을 VM 종료로 다룬다). `quiet`는 부팅 메시지를 시리얼 콘솔에 쓰는 시간
//!   (약 100ms)을 없앤다. 경고와 오류는 그대로 나온다.
//! - 네트워크 장치와 공유 폴더(virtio-fs)는 없다(`live_share: false`). 스냅샷도 아직 알리지 않는다.
//!
//! # jailer ([`jail`], Linux)
//!
//! root로 돌면 firecracker를 Firecracker 공식 [jailer](https://github.com/firecracker-microvm/firecracker/blob/main/docs/jailer.md)로
//! 가둔다. jailer를 찾지 못하면 root인 채로 firecracker를 돌리지 않고 `capabilities`와 `run`이 실패한다
//! (`SBX_FIRECRACKER_JAILER=off`로 끌 수 있다). root가 아니면 jailer를 쓰지 않는다(jailer가 root를 요구한다). 그때
//! firecracker는 sbx를 돌린 사용자로 돈다.
//!
//! - VM마다 슬롯 하나(`<기준>/sbx-slots/<n>.lock`을 잠근다, 최대 [`jail::SLOTS`]), uid는 `기준 id + 1 + n`, gid는
//!   `기준 id`(기본 [`jail::DEFAULT_ID_BASE`], `SBX_FIRECRACKER_JAILER_ID_BASE`). 두 VM이 같은 uid를 쓰지 않는다.
//! - chroot는 `<기준>/firecracker/sbx-<n>/root`(기준은 `SBX_FIRECRACKER_CHROOT_BASE`, 기본 `/srv/jailer`. root
//!   소유이고 root만 쓸 수 있어야 한다). 커널·게스트 init·디스크는 하드 링크로 넣는다(같은 파일시스템이어야 한다.
//!   커널과 게스트 init은 아니면 복사한다). 쓰기 가능한 디스크는 VM이 도는 동안 jail 사용자의 것이고 끝나면 되돌린다.
//!   읽기 전용 파일은 다른 사용자가 쓸 수 있으면 거부하고, 읽을 수 없으면 jail 그룹에 읽기를 준다.
//! - 제어 소켓 자리에는 jail 안 vsock 소켓을 가리키는 링크를 둔다.
//! - cgroup v2가 있으면 `firecracker/sbx-<n>`에 상한을 둔다: `cpu.max`는 vCPU + 1 코어, `memory.max`는 게스트
//!   메모리 + [`jail::MEMORY_OVERHEAD_MIB`] MiB, `pids.max`는 vCPU + 32. 뿌리에 없는 컨트롤러는 건너뛴다.
//!   `SBX_FIRECRACKER_CGROUPS=off`면 cgroup을 만들지 않는다.
//! - VM이 끝나면 jail 디렉터리와 cgroup을 지운다. 실행기가 죽어 남은 것은 같은 슬롯을 다음에 쓸 때 지운다.
//!
//! # 프로세스 ([`session`])
//!
//! - `run`: `Launch`를 받아 설정 파일을 쓰고 firecracker를 띄운다(root면 jailer로). vsock 소켓 파일이 생기면 `Started`,
//!   그 전에 firecracker가 끝나면 `Error`다. firecracker의 출력(게스트 시리얼 콘솔 포함)은 표준 오류(실행기 로그)로 간다.
//! - firecracker는 이 실행 파일의 `vmm` 모드(감시 프로세스) 아래에서 돈다. 감시 프로세스는 표준 입력이 닫히면
//!   firecracker를 죽인다. 표준 입력은 `run` 세션이 쥐고 있으므로 `Kill`, 세션의 입력 끝, 세션 프로세스가
//!   강제로 죽는 경우 모두 VM이 끝난다(고아 VM이 남지 않는다).
//! - VM이 끝나면 `Exited`: firecracker가 0으로 끝나면 `guest powered off`, 죽였으면 `killed`.

pub mod config;
#[cfg(unix)]
pub mod jail;
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
