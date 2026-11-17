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
//!   볼륨 디스크(규약 3, 역할 `volume`)는 `cache_type: "Writeback"`으로 붙인다. 그래야 게스트가 flush 기능을 보고,
//!   게스트의 flush가 호스트 파일의 fsync로 간다(기본값 `Unsafe`는 flush를 보이지 않는다). 규약 2와 3을 말한다.
//! - vsock: guest CID 3, `uds_path`는 규약의 `control_socket`. 남아 있는 파일은 지운다(Firecracker는 있으면 실패한다).
//!   경로는 [`config::MAX_SOCKET_PATH`] 바이트 이하여야 한다(macOS Unix 소켓 한도).
//! - 커널 명령줄: `console=ttyS0 quiet reboot=k panic=1`(x86_64는 `pci=off`도), `sbx.hostname=`, `sbx.halt=reboot`
//!   (Firecracker는 게스트의 재시작 요청을 VM 종료로 다룬다). `quiet`는 부팅 메시지를 시리얼 콘솔에 쓰는 시간
//!   (약 100ms)을 없앤다. 경고와 오류는 그대로 나온다.
//! - 네트워크 장치는 없다. 스냅샷도 아직 알리지 않는다. 공유 폴더는 아래 "확장"이 있을 때만이다.
//!
//! # 확장 ([`features`])
//!
//! upstream Firecracker에는 virtio-fs가 없다. 같은 인터페이스를 구현한 빌드가 더 가진 장치는 이렇게 알아내고 쓴다.
//!
//! - `capabilities`와 (공유가 있는) `run`은 `firecracker --features`를 돌린다. 5초 안에 0으로 끝나고 표준 출력이 JSON
//!   객체이며 그 `extensions`(이름 목록이나 이름을 키로 한 객체)에 `fs`가 있을 때만 `live_share: true`다. 그 밖(upstream은
//!   그 인자를 모르고 오류로 끝난다)은 확장 없음이고 upstream과 똑같이 돈다.
//! - `fs`가 있고 `VmSpec::shares`가 있으면 설정 파일 최상위에 `extensions.fs`를 넣는다:
//!   `{"shares": [{"id": "fs0", "tag": <태그>, "path": <호스트 디렉터리>, "read_only": <bool>}, …]}`. 태그는 36바이트
//!   이하(virtio-fs 명세). 공유가 없으면 `extensions`를 넣지 않는다. `fs`를 알리지 않은 VMM에 공유가 오면 `Error`다.
//! - 이 인자와 키는 wick(macOS HVF Firecracker 빌드)이 제안했다. 어느 VMM인지는 보지 않는다: 같은 모양으로 답하는
//!   VMM이면 무엇이든 그대로 쓴다.
//!
//! # 프로세스 ([`session`])
//!
//! - `run`: `Launch`를 받아 설정 파일을 쓰고 firecracker를 띄운다. vsock 소켓 파일이 생기면 `Started`,
//!   그 전에 firecracker가 끝나면 `Error`다. firecracker의 출력(게스트 시리얼 콘솔 포함)은 표준 오류(실행기 로그)로 간다.
//! - firecracker는 이 실행 파일의 `vmm` 모드(감시 프로세스) 아래에서 돈다. 감시 프로세스는 표준 입력이 닫히면
//!   firecracker를 죽인다. 표준 입력은 `run` 세션이 쥐고 있으므로 `Kill`, 세션의 입력 끝, 세션 프로세스가
//!   강제로 죽는 경우 모두 VM이 끝난다(고아 VM이 남지 않는다).
//! - 거꾸로 감시 프로세스만 강제로 죽으면(SIGKILL, OOM) firecracker를 죽일 쪽이 없다. 그래서 Unix에서는 감시 프로세스를
//!   새 프로세스 그룹으로 띄우고(firecracker가 물려받는다), 세션이 감시 프로세스의 끝을 보면 거두기 전에 그 그룹을
//!   SIGKILL로 끝낸 뒤 `Exited`를 보낸다. 남는 경우: 세션과 감시 프로세스가 함께 강제로 죽으면 firecracker는 남는다
//!   (둘을 이어 주는 것이 없다. Linux의 PR_SET_PDEATHSIG는 fork와 exec 사이에 unsafe가 필요해 쓰지 않는다).
//! - VM이 끝나면 `Exited`: firecracker가 0으로 끝나면 `guest powered off`, 죽였으면 `killed`.

pub mod config;
pub mod features;
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
