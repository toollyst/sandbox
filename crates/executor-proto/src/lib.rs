//! 실행기 통신 규약. sbx가 외부 실행기(예: fc-fork)를 띄우고 다루는 방법이다.
//!
//! 이 문서가 규격이다. 실행기는 이것만 지키면 된다. VMM, 기본 커널, 그 설정은 실행기의 책임이다.
//! sbx는 규약에 맞는 실행 파일을 그대로 실행한다.
//!
//! # 실행기 실행 파일
//!
//! VM 하나에 실행기 프로세스 하나다. sbx가 두 가지 방법으로 실행한다.
//!
//! - `<실행기> capabilities`: [`Capabilities`] JSON 한 줄을 표준 출력에 쓰고 0으로 끝난다.
//! - `<실행기> run`: 제어 세션. 아래 순서를 따른다.
//!
//! 표준 입력과 표준 출력은 줄 단위 JSON이다(UTF-8, 메시지 하나가 한 줄, 한 줄 최대 [`MAX_LINE`] 바이트).
//! 표준 오류는 자유 형식 진단 출력이고, sbx는 그대로 로그 파일에 남긴다.
//!
//! # 제어 세션 (`run`)
//!
//! 1. sbx가 [`Request::Launch`]를 보낸다. `version`이 [`VERSION`]과 다르면 실행기는 [`Event::Error`]를 보내고 끝난다.
//! 2. 실행기는 VM을 띄우고 [`Event::Started`]를 보낸다. 띄우지 못하면 [`Event::Error`]를 보내고 0이 아닌 코드로 끝난다.
//!    `Started`는 게스트가 연결을 받을 준비가 되었다는 뜻이 아니다. sbx는 연결을 다시 시도한다.
//! 3. sbx는 언제든 [`Request::Kill`]을 보낼 수 있다. 실행기는 VM을 즉시 끝낸다.
//! 4. VM이 어떤 이유로든 끝나면 실행기는 [`Event::Exited`]를 보내고 0으로 끝난다.
//! 5. 표준 입력이 닫히면(sbx가 끝났거나 놓았으면) 실행기는 VM을 즉시 끝내고 끝난다. 그래서 고아 VM이 남지 않는다.
//!
//! 모르는 메시지나 해석할 수 없는 줄은 오류다. 실행기는 [`Event::Error`]를 보내고 VM을 끝낸다.
//!
//! # 게스트 연결
//!
//! 실행기는 [`VmSpec::control_socket`] 경로에 Unix 소켓을 만든다. sbx는 여기에 연결해
//! `CONNECT <포트>\n`을 쓰고 `OK <아무 숫자>\n`을 받은 뒤부터 게스트 vsock 포트와의 바이트 스트림으로 쓴다
//! (Firecracker와 Cloud Hypervisor의 vsock 방식과 같다). 게스트가 아직 그 포트를 듣지 않으면 실행기는 연결을 닫는다.
//! 클라이언트 쪽은 [`vsock::connect`], 서버 쪽 읽기는 [`vsock::accept`]다.
//!
//! sbx는 연결의 반쯤 닫기(`shutdown(SHUT_WR)`)를 쓰지 않는다. 끝은 연결 위의 메시지로 알리고, 다 쓰면 연결 전체를 닫는다.
//! 그래서 한쪽이 EOF면 연결 전체를 닫는 vsock 구현(Firecracker가 그렇다)도 그대로 맞는다.
//! 게스트에서 호스트로 여는 연결은 쓰지 않는다(`<control_socket>_<포트>` 같은 방식이 필요 없다).
//!
//! # 게스트에 대한 약속
//!
//! 실행기가 띄운 VM은 다음을 지킨다. 게스트 init([`VmSpec::guest_init`])은 sbx가 준다.
//!
//! - 커널은 `guest_init`을 initrd로 받아 그 안의 `/init`을 PID 1로 실행한다. `root=`를 주지 않는다.
//! - 커널은 보통 실행기의 기본 커널이다. [`VmSpec::kernel`]이 있으면(이미지가 자기 커널을 담은 경우) 그 파일로
//!   부팅한다. 할 수 없는 실행기는 `custom_kernel`을 `false`로 알린다. 그러면 sbx는 커널을 보내지 않고, 그런 이미지는
//!   그 실행기로 띄우지 않는다. 받은 커널의 형식(VMM이 읽는 형식인지)과 아래 기능은 이미지를 만든 쪽의 책임이다.
//! - 커널 기능: virtio-blk, vsock(virtio), erofs, ext4(ext2 형식 포함), overlay, devtmpfs, proc, sysfs, tmpfs, devpts(Unix98 pty), 루프백 네트워크.
//!   커널은 4.7 이상이어야 한다(`newinstance`로 마운트한 `/dev/pts`를 `/dev/ptmx`가 찾는다).
//! - 디스크는 [`VmSpec::disks`] 순서대로 붙인다. 게스트에서 첫째가 `/dev/vda`(rootfs, 읽기 전용), 둘째가 `/dev/vdb`(scratch)다.
//!   디스크 파일은 받은 그대로 붙인다(내용을 바꾸지 않는다). 게스트는 처음 보는 scratch가 0으로 채워져 있다고 믿는다
//!   (sbx가 새 sparse 파일로 만든다).
//! - 커널 명령줄에 `sbx.hostname=<hostname>`과 `sbx.halt=poweroff|reboot`을 넣는다. `sbx.halt`는 그 VMM에서
//!   게스트가 VM을 끝내는 방법이다(Firecracker처럼 재시작 요청을 VM 종료로 다루면 `reboot`).
//! - 공유 폴더([`VmSpec::shares`])는 virtio-fs 장치 하나씩으로 붙이고, 그 태그를 [`ShareSpec::tag`]로 한다.
//!   게스트 init이 `mount -t virtiofs <tag> <경로>`로 마운트한다(커널 기능 virtio-fs). 읽기 전용이면 실행기도
//!   읽기 전용으로 내보낸다. 붙일 수 없는 실행기는 `live_share`를 `false`로 알린다. 그러면 sbx는 공유를 보내지 않는다.
//! - 네트워크 장치는 붙이지 않는다. 허용된 나가는 연결은 sbx가 vsock 위의 프록시로 처리한다.
//!
//! # 버전
//!
//! [`VERSION`]이 바뀌면 호환되지 않는다. 1.0 전까지 규칙은 "같아야 한다"이다.
//! 버전마다 모양이 다를 수 있으므로 `capabilities`를 읽는 쪽은 [`CapabilitiesHeader`]로 버전을 먼저 본다.
//!
//! | 버전 | 바뀐 것 |
//! |---|---|
//! | 1 | 처음 |
//! | 2 | `Capabilities::custom_kernel`, `VmSpec::kernel` (이미지가 담은 커널로 부팅) |
//!
//! - 의존: `serde`, `serde_json`만 (실행기 저장소가 이것만 가져다 쓸 수 있게)

use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub mod vsock;

/// 규약 버전.
pub const VERSION: u32 = 2;

/// 한 줄 최대 길이(바이트, 줄바꿈 포함).
pub const MAX_LINE: usize = 1024 * 1024;

/// 게스트 아키텍처.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// x86_64.
    Amd64,
    /// aarch64.
    Arm64,
}

/// `<실행기> capabilities`의 출력.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// 실행기가 구현한 규약 버전.
    pub version: u32,
    /// 실행기 이름. 오류 메시지와 `inspect`에 쓴다.
    pub name: String,
    /// 게스트 아키텍처. 호스트와 같아야 한다.
    pub arch: Arch,
    /// 커널이 읽을 수 있는 rootfs 형식 이름(예: `"erofs"`). sbx가 모르는 이름은 무시한다.
    pub rootfs_formats: Vec<String>,
    /// 실시간 폴더 공유([`VmSpec::shares`]) 지원 여부.
    pub live_share: bool,
    /// [`VmSpec::kernel`]로 부팅할 수 있는지.
    pub custom_kernel: bool,
    /// 스냅샷 지원 여부.
    pub snapshot: bool,
    /// VM 하나에 붙일 수 있는 디스크 개수.
    pub max_disks: u32,
}

/// 모든 버전의 [`Capabilities`]에 있는 앞부분. 읽는 쪽은 이것으로 `version`을 먼저 보고, 같을 때만
/// [`Capabilities`]로 읽는다. 그래야 버전이 다른 실행기에 "규약 버전이 다르다"고 알릴 수 있다(모양이 달라도).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CapabilitiesHeader {
    /// 실행기가 구현한 규약 버전.
    pub version: u32,
    /// 실행기 이름 (없으면 빈 문자열).
    #[serde(default)]
    pub name: String,
}

/// 디스크 용도.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskRole {
    /// 이미지 rootfs.
    Rootfs,
    /// 샌드박스별 쓰기 가능 디스크.
    Scratch,
}

/// 디스크 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskSpec {
    /// 호스트의 디스크 이미지 파일.
    pub path: PathBuf,
    /// 용도.
    pub role: DiskRole,
    /// 읽기 전용으로 붙인다.
    pub readonly: bool,
}

/// 실시간 폴더 공유 하나(virtio-fs). `live_share`가 없는 실행기에는 보내지 않는다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareSpec {
    /// 호스트 디렉터리.
    pub host_path: PathBuf,
    /// 게스트가 이 공유를 찾는 이름.
    pub tag: String,
    /// 읽기 전용.
    pub readonly: bool,
}

/// 띄울 VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmSpec {
    /// 게스트 init initramfs (sbx가 준다).
    pub guest_init: PathBuf,
    /// 이 커널로 부팅한다. 없으면(필드가 없으면) 실행기의 기본 커널이다. `custom_kernel`인 실행기에만 보낸다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel: Option<PathBuf>,
    /// 붙일 디스크. 순서가 게스트 장치 이름을 정한다.
    pub disks: Vec<DiskSpec>,
    /// vCPU 개수 (1 이상).
    pub vcpus: u8,
    /// 메모리 MiB (1 이상).
    pub memory_mib: u32,
    /// 실시간 폴더 공유.
    pub shares: Vec<ShareSpec>,
    /// 게스트 연결용 Unix 소켓 경로. 실행기가 만든다. 이미 있으면 지우고 만든다.
    pub control_socket: PathBuf,
    /// 게스트 호스트 이름.
    pub hostname: String,
}

/// sbx → 실행기.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// VM을 띄운다. 세션의 첫 메시지.
    Launch {
        /// [`VERSION`].
        version: u32,
        /// 띄울 VM.
        vm: VmSpec,
    },
    /// VM을 즉시 끝낸다. (필드가 없어도 struct 모양이어야 모르는 필드를 거부한다.)
    Kill {},
}

/// 실행기 → sbx.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Event {
    /// VM을 띄웠다.
    Started {
        /// VMM 프로세스 ID (있으면).
        pid: Option<u32>,
    },
    /// VM이 끝났다. 세션의 마지막 메시지.
    Exited {
        /// 사람이 읽을 이유 (예: `"guest powered off"`, `"killed"`).
        reason: String,
    },
    /// 실패. 세션의 마지막 메시지.
    Error {
        /// 사람이 읽을 설명.
        message: String,
    },
}

/// 메시지 하나를 한 줄로 쓴다.
pub fn write_line<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(io::Error::other)?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "message exceeds MAX_LINE",
        ));
    }
    w.write_all(&line)?;
    w.flush()
}

/// 한 줄을 읽어 해석한다. 입력이 끝났으면 `None`.
/// [`MAX_LINE`]을 넘는 줄은 끝까지 읽지 않고 오류다.
pub fn read_line<T: DeserializeOwned>(r: &mut impl BufRead) -> io::Result<Option<T>> {
    let mut line = Vec::new();
    let n = r
        .by_ref()
        .take(MAX_LINE as u64)
        .read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        let why = if n >= MAX_LINE {
            "line exceeds MAX_LINE"
        } else {
            "unterminated line"
        };
        return Err(io::Error::new(io::ErrorKind::InvalidData, why));
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests;
