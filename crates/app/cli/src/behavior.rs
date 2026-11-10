//! CLI 동작 규칙. Docker CLI를 기준으로 하고, 다른 점은 [`DOCKER_DIFFERENCES`]에 적는다.
//!
//! 여기 있는 함수는 입출력 없이 "무엇을 어떤 순서로 부를지"만 정한다. 테스트가 곧 규칙이다.

use std::fmt;
use std::str::FromStr;

use std::collections::BTreeMap;

use std::time::Duration;

use sbx_guest_proto::ErrorKind;
use sbx_model::{AllowRule, ImageId, NetworkPolicy, ParseError, RootfsFormat, SandboxId};

/// `run`과 `create`의 `--pull`. Docker와 같은 값과 같은 기본값(`missing`)이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullPolicy {
    /// 이미지가 없을 때만 받는다.
    #[default]
    Missing,
    /// 매번 받는다. digest가 같으면 내려받지 않는다.
    Always,
    /// 받지 않는다. 없으면 오류.
    Never,
}

impl FromStr for PullPolicy {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "missing" => Ok(PullPolicy::Missing),
            "always" => Ok(PullPolicy::Always),
            "never" => Ok(PullPolicy::Never),
            _ => Err(ParseError::new("pull policy", s)),
        }
    }
}

/// `run`과 `create`가 차례로 부르는 코어 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `Manager::pull`.
    Pull,
    /// `Manager::prepare`. 이미 있으면 아무것도 하지 않는다.
    Prepare,
    /// `Runtime::create`.
    Create,
}

/// `--pull=never`인데 준비가 안 된 경우.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReady {
    /// 이미지가 없음.
    Image,
    /// 이미지는 있지만 그 형식의 rootfs가 없음.
    Rootfs,
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotReady::Image => {
                f.write_str("image is not pulled (use `sbx pull` or --pull=missing)")
            }
            NotReady::Rootfs => {
                f.write_str("rootfs is not prepared (use `sbx prepare` or --pull=missing)")
            }
        }
    }
}

/// `run`과 `create`가 부를 동작 순서.
///
/// | `--pull` | 이미지 없음 | 이미지만 있음 | 둘 다 있음 |
/// |---|---|---|---|
/// | missing | pull, prepare, create | prepare, create | create |
/// | always | pull, prepare, create | pull, prepare, create | pull, prepare, create |
/// | never | 오류(이미지) | 오류(rootfs) | create |
pub fn create_steps(
    policy: PullPolicy,
    image_present: bool,
    rootfs_present: bool,
) -> Result<Vec<Step>, NotReady> {
    use Step::*;
    Ok(match (policy, image_present, rootfs_present) {
        (PullPolicy::Always, _, _) | (PullPolicy::Missing, false, _) => vec![Pull, Prepare, Create],
        (PullPolicy::Missing, true, false) => vec![Prepare, Create],
        (PullPolicy::Missing, true, true) | (PullPolicy::Never, true, true) => vec![Create],
        (PullPolicy::Never, false, _) => return Err(NotReady::Image),
        (PullPolicy::Never, true, false) => return Err(NotReady::Rootfs),
    })
}

/// `rmi`의 판단.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmiDecision {
    /// `Manager::remove`를 부른다. 다른 data_dir의 실행 중 샌드박스는 그 안의 잠금이 막는다.
    Remove,
    /// 실행 중인 샌드박스가 쓴다. `-f`로도 지울 수 없다.
    RefuseRunning(Vec<SandboxId>),
    /// 정지된 샌드박스가 쓴다. `-f`로 지울 수 있다.
    RefuseStopped(Vec<SandboxId>),
}

/// `rmi` 보호 규칙. Docker `rmi`와 같다.
///
/// | 이미지를 쓰는 샌드박스 (같은 data_dir) | 기본 | `-f` |
/// |---|---|---|
/// | 없음 | 삭제 | 삭제 |
/// | 정지된 샌드박스 | 거부 | 삭제 |
/// | 실행 중인 샌드박스 | 거부 | 거부 |
///
/// 다른 data_dir의 정지된 샌드박스는 알 수 없으므로 보호 범위 밖이다.
/// 강제로 지운 이미지를 쓰던 샌드박스를 `start`하면 이미지 없음 오류가 난다.
pub fn rmi_decision(running: Vec<SandboxId>, stopped: Vec<SandboxId>, force: bool) -> RmiDecision {
    if !running.is_empty() {
        RmiDecision::RefuseRunning(running)
    } else if !stopped.is_empty() && !force {
        RmiDecision::RefuseStopped(stopped)
    } else {
        RmiDecision::Remove
    }
}

/// `--rootfs-format`을 주지 않았을 때의 형식.
pub const DEFAULT_ROOTFS_FORMAT: RootfsFormat = RootfsFormat::Erofs;

/// `--cpus`를 주지 않았을 때의 vCPU 개수.
pub const DEFAULT_VCPUS: u8 = 1;

/// `--memory`를 주지 않았을 때의 메모리(MiB).
pub const DEFAULT_MEMORY_MIB: u32 = 512;

/// 샌드박스별 쓰기 가능 디스크 크기(MiB).
pub const DEFAULT_SCRATCH_MIB: u32 = 1024;

/// 이미지 ID 앞부분으로 찾을 때 필요한 최소 길이. `images`의 IMAGE ID 열과 같은 12자리다.
/// 짧은 문자열이 우연히 ID 앞부분과 맞아 엉뚱한 이미지를 지우는 일을 막는다.
pub const MIN_ID_PREFIX: usize = 12;

/// 종료 코드 (Docker와 같다).
///
/// | 경우 | 코드 |
/// |---|---|
/// | 붙어서 실행한 프로세스가 끝남 (`run`, `start`, `exec`) | 그 프로세스의 코드. 시그널이면 128 + 번호 |
/// | `run`에서 sbx 쪽 실패 (이미지, 실행기, VM, 게스트) | [`EXIT_RUN_FAILED`] 125 |
/// | `run`, `exec`에서 명령을 시작하지 못함: 실행 권한 없음 | [`EXIT_CANNOT_INVOKE`] 126 |
/// | `run`, `exec`에서 명령을 시작하지 못함: 실행 파일 없음 | [`EXIT_NOT_FOUND`] 127 |
/// | 그 밖의 명령의 실패 | 1 |
/// | 사용법 오류 | 2 (clap) |
pub const EXIT_RUN_FAILED: u8 = 125;

/// 명령을 찾았지만 실행할 수 없음.
pub const EXIT_CANNOT_INVOKE: u8 = 126;

/// 명령을 찾지 못함.
pub const EXIT_NOT_FOUND: u8 = 127;

/// `run`, `exec`에서 명령을 시작하지 못했을 때의 종료 코드.
pub fn command_failure_code(kind: ErrorKind) -> u8 {
    match kind {
        ErrorKind::NotFound => EXIT_NOT_FOUND,
        ErrorKind::PermissionDenied => EXIT_CANNOT_INVOKE,
        _ => EXIT_RUN_FAILED,
    }
}

/// 프로세스 종료 코드를 sbx의 종료 코드로. 0..=255 밖이면 255.
pub fn exit_byte(code: i32) -> u8 {
    u8::try_from(code).unwrap_or(255)
}

/// 붙어서 실행하는 동안 받으면 프로세스에 그대로 전하는 시그널 (Docker `--sig-proxy` 기본과 같다).
/// (이름, 게스트 Linux 번호). 호스트 번호와 다를 수 있어(macOS의 USR1은 30) 게스트 번호로 보낸다.
/// sbx는 이 시그널로 끝나지 않는다. 프로세스가 끝나야 끝난다.
/// Windows에서는 Ctrl+C만 받아 INT로 보낸다.
pub const FORWARDED_SIGNALS: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("USR1", 10),
    ("USR2", 12),
    ("TERM", 15),
];

/// `stop`, `restart`에서 시그널 뒤 기다리는 시간: `-t` > 샌드박스의 `--stop-timeout` > 10초 (Docker와 같다).
pub fn stop_timeout(given: Option<u64>, recorded: Option<u32>) -> Duration {
    Duration::from_secs(given.or(recorded.map(u64::from)).unwrap_or(10))
}

/// `--network`와 `--allow`로 네트워크 정책을 정한다.
///
/// | `--network` | `--allow` | 결과 |
/// |---|---|---|
/// | `none` | 없음 | 차단 |
/// | `none` | 있음 | 오류 (`--allow`는 `--network allow`와 함께) |
/// | `allow` | 있음 | 그 목록만 허용 |
/// | `allow` | 없음 | 오류 (허용할 것이 없다) |
/// | 그 밖 | | 오류 |
pub fn network_policy(mode: &str, allow: &[String]) -> Result<NetworkPolicy, String> {
    match (mode, allow.is_empty()) {
        ("none", true) => Ok(NetworkPolicy::Disabled),
        ("none", false) => Err("--allow requires --network allow".into()),
        ("allow", true) => Err("--network allow needs at least one --allow HOST:PORT".into()),
        ("allow", false) => allow
            .iter()
            .map(|r| r.parse::<AllowRule>().map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()
            .map(NetworkPolicy::Allow),
        (other, _) => Err(format!(
            "--network {other} is not supported (none or allow)"
        )),
    }
}

/// 이미지 이름이 무엇으로 맞았는지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageMatch {
    /// 참조(태그 또는 digest 참조)로 맞음.
    Ref {
        /// 정규화된 참조.
        reference: String,
        /// 이미지.
        id: ImageId,
    },
    /// 이미지 ID(전체 또는 앞부분)로 맞음.
    Id(ImageId),
}

impl ImageMatch {
    /// 이미지 ID.
    pub fn id(&self) -> &ImageId {
        match self {
            ImageMatch::Ref { id, .. } | ImageMatch::Id(id) => id,
        }
    }
}

/// 이미지를 찾지 못한 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// 없음.
    NotFound(String),
    /// ID 앞부분이 여러 이미지와 맞음.
    Ambiguous(String),
}

impl std::error::Error for LookupError {}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LookupError::NotFound(n) => write!(f, "no such image: {n}"),
            LookupError::Ambiguous(n) => write!(f, "image ID prefix {n} matches several images"),
        }
    }
}

/// 이미지 이름을 찾는다. Docker와 같은 순서다.
///
/// 1. 참조로 해석해 태그 기록에서 찾는다(`normalized`는 정규화된 참조, 해석할 수 없으면 `None`).
/// 2. `sha256:`으로 시작하거나 hex만으로 된 이름은 이미지 ID로 본다. 전체 또는
///    [`MIN_ID_PREFIX`]자리 이상의 앞부분이 정확히 하나와 맞아야 한다.
pub fn match_image(
    name: &str,
    normalized: Option<&str>,
    refs: &BTreeMap<String, ImageId>,
    ids: &[ImageId],
) -> Result<ImageMatch, LookupError> {
    if let Some((reference, id)) = normalized.and_then(|n| refs.get_key_value(n)) {
        return Ok(ImageMatch::Ref {
            reference: reference.clone(),
            id: id.clone(),
        });
    }
    let hex = name.strip_prefix("sha256:").unwrap_or(name);
    let is_hex = !hex.is_empty()
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if is_hex && hex.len() >= MIN_ID_PREFIX {
        let found: Vec<&ImageId> = ids
            .iter()
            .filter(|id| id.digest().hex().starts_with(hex))
            .collect();
        return match found.as_slice() {
            [one] => Ok(ImageMatch::Id((*one).clone())),
            [] => Err(LookupError::NotFound(name.to_owned())),
            _ => Err(LookupError::Ambiguous(name.to_owned())),
        };
    }
    Err(LookupError::NotFound(name.to_owned()))
}

/// `rmi` 이름 하나에 대해 할 일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmiAction {
    /// 참조만 지운다. 이미지는 남는다.
    Untag(String),
    /// 이미지를 지운다. [`rmi_decision`]의 보호 규칙을 거친다.
    Delete(ImageId),
    /// ID로 지정했는데 참조가 여럿이다. `-f`가 필요하다.
    RefuseMultipleRefs(ImageId),
}

/// `rmi` 태그 규칙. Docker `rmi`와 같다.
///
/// | 이름이 맞은 방식 | 이미지를 가리키는 참조 수 | 기본 | `-f` |
/// |---|---|---|---|
/// | 참조 | 2개 이상 | 그 참조만 지움 | 같음 |
/// | 참조 | 1개 | 이미지 삭제 | 같음 |
/// | ID | 1개 이하 | 이미지 삭제 | 같음 |
/// | ID | 2개 이상 | 거부 | 이미지 삭제 |
///
/// 이미지 삭제는 [`rmi_decision`]의 샌드박스 보호 규칙을 거친다.
pub fn rmi_action(m: &ImageMatch, ref_count: usize, force: bool) -> RmiAction {
    match m {
        ImageMatch::Ref { reference, .. } if ref_count >= 2 => RmiAction::Untag(reference.clone()),
        ImageMatch::Ref { id, .. } => RmiAction::Delete(id.clone()),
        ImageMatch::Id(id) if ref_count >= 2 && !force => RmiAction::RefuseMultipleRefs(id.clone()),
        ImageMatch::Id(id) => RmiAction::Delete(id.clone()),
    }
}

/// Docker와 다른 점: (항목, 이 프로젝트의 동작, 이유).
pub const DOCKER_DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "--network",
        "none(기본, Docker는 bridge)과 allow만 있다. allow는 --allow 목록의 HOST:PORT로 가는 TCP만 게스트 안 HTTP 프록시(HTTP_PROXY 등)를 거쳐 호스트가 연다. VM에는 네트워크 장치가 없다",
        "기획서 원칙: 네트워크는 기본 차단, 허용한 것만 연다. 이름 단위 허용 목록을 실행기 기능 없이 모든 OS에서 강제한다",
    ),
    (
        "--cpus, --memory 기본값",
        "1 vCPU, 512 MiB (Docker는 제한 없음). --cpus는 정수만 받는다",
        "VM은 크기를 정해서 띄워야 한다",
    ),
    (
        "--name",
        "주지 않으면 이름이 없다 (Docker는 이름을 지어 붙인다). 이름이 없는 샌드박스는 ID로 찾는다",
        "지어 붙인 이름은 쓰지 않는 사람에게도 보이고 겹침만 늘린다. 필요하면 호출한 쪽이 짓는다",
    ),
    (
        "이미지 ID 앞부분으로 찾기",
        "12자리 이상만 받는다 (Docker는 더 짧아도 받는다)",
        "짧은 문자열이 우연히 맞아 엉뚱한 이미지를 지우는 일을 막는다",
    ),
    (
        "--format",
        "text와 json만. Go 템플릿(--format '{{.ID}}')은 없고, 대신 --jq EXPR로 JSON 출력을 고른다(gh와 같다). \
         json의 모양은 crate::output이 정본이다(docs/developers/cli-output.md)",
        "Go 템플릿을 Rust로 똑같이 만들 수 없고, 반쯤 같은 것은 명시적 동작에 어긋난다. jq는 사실상 표준 언어다",
    ),
    (
        "--filter",
        "ps, images, events만. 키는 crate::filter의 표(PS, IMAGES, EVENTS)가 정본이다. name은 정규식이 아니라 부분 문자열, \
         reference는 *와 ?만 있는 패턴. ps의 before/since/health 등, images의 before/since, events의 container/label은 없다. \
         모르는 키와 값은 오류다(Docker는 일부를 무시한다)",
        "명시적 동작: 받는 것을 표로 정하고, 정규식 엔진을 들이지 않는다",
    ),
    (
        "sbx events",
        "종류는 sandbox와 image, 동작은 create, start, stop, remove와 pull, prepare, untag, delete다 \
         (Docker는 container의 die, destroy 등). 시각은 RFC 3339 나노초. --since/--until은 시간대 없는 날짜를 받지 않는다. \
         기록은 1 MiB 두 세대까지 남는다(Docker는 최근 256개)",
        "데몬이 없어서 상태를 바꾸는 프로세스가 data_dir과 cache_dir에 직접 남긴다(sbx-events). 그래서 소유 프로세스가 \
         갑자기 죽어 아무도 stopped를 기록하지 못하면 stop 이벤트도 없다",
    ),
    (
        "sbx completion",
        "등록 스크립트를 찍는다. 탭마다 셸이 `COMPLETE=<셸> sbx -- <단어들>`로 sbx를 다시 불러 샌드박스 이름·ID, 이미지 참조, \
         설치한 실행기를 후보로 받는다(crate::complete). 그래서 환경 변수 COMPLETE가 있으면(비어 있지 않고 0이 아니면) \
         sbx는 명령을 실행하지 않고 자동완성만 한다. 샌드박스를 상태로 거르지 않는다(Docker는 stop에 실행 중인 것만 낸다)",
        "데몬이 없어서 sbx가 data_dir과 cache_dir을 직접 읽는다. 상태를 알려면 소유 잠금을 건드려야 하는데, 자동완성은 \
         아무것도 바꾸지 않는다",
    ),
    (
        "sbx tui",
        "Docker CLI에 없는 명령(Docker Desktop이나 lazydocker 같은 화면). 전체 화면에서 ps, images, events를 보고 \
         키로 sbx 명령을 낸다. 코어를 직접 부르지 않고 같은 sbx를 자식으로 부른다(crate::tui)",
        "CLI가 주인이고 화면은 그 위의 도구다. 화면이 하는 일은 모두 상태 줄에 보이는 sbx 명령 줄과 같다",
    ),
    (
        "sbx prepare",
        "Docker에 없는 명령",
        "이미지를 VM용 rootfs 디스크로 만드는 단계가 따로 있다",
    ),
    (
        "--rootfs-format",
        "Docker에 없는 옵션. 생략하면 erofs ([`DEFAULT_ROOTFS_FORMAT`])",
        "rootfs 형식을 여러 개 지원한다",
    ),
    (
        "--mount type=copy",
        "Docker에 없는 마운트 종류",
        "시작할 때 복사해 넣고 결과는 cp로 꺼내는 공유 방식",
    ),
    (
        "-v, --mount type=bind",
        "실행기가 실시간 공유를 지원하지 않으면 오류",
        "실행기마다 기능이 다르다. 다른 방식으로 바꾸지 않는다",
    ),
    (
        "rmi의 정지 샌드박스 보호",
        "같은 data_dir 안에서만",
        "다른 data_dir은 알 수 없다",
    ),
    (
        "stopped 상태",
        "Docker처럼 다시 start할 수 있다 (OCI Runtime Spec과 다름)",
        "CLI가 Docker를 기준으로 한다",
    ),
    (
        "run -d, start",
        "Docker와 같다. 붙지 않고 시작한 샌드박스는 데몬 대신 샌드박스마다 shim 프로세스(`sbx-shim`)가 소유한다",
        "데몬이 없다. VM은 시작한 프로세스가 소유한다",
    ),
    (
        "logs",
        "붙지 않고(run -d, start) 시작했을 때만 남는다. 붙어서 실행한 출력은 sbx가 바로 보여주고 남기지 않는다. 샌드박스마다 256 MiB까지 남기고 그 뒤는 버린다(한 줄로 알린다. Docker json-file은 한도가 없다)",
        "출력을 받아 적을 데몬이 없다. 적는 것은 shim이다",
    ),
    (
        "logs -t, --since, --until",
        "시각은 줄마다가 아니라 shim이 출력 조각을 받을 때마다 하나다(한 조각 안의 줄들은 같은 시각). \
         시각을 남기기 전의 sbx가 쓴 로그(그 샌드박스를 다시 시작해도 그대로)에는 이 옵션들이 오류다. \
         --since/--until은 시간대 없는 날짜를 받지 않는다",
        "로그는 shim이 받은 조각 그대로 적는다(sbx_shim::log). 시각 없는 줄을 범위에 넣을지 뺄지 알 수 없으므로 조용히 틀리지 않는다",
    ),
    (
        "attach",
        "붙지 않고(run -d, start) 시작한 샌드박스에만 붙는다. 다른 sbx가 붙어서 실행 중인 샌드박스에는 붙지 않는다. Windows에서는 아직 없다",
        "붙기는 기본 프로세스의 입출력을 쥔 shim이 받는다. 데몬이 없다",
    ),
    (
        "cp",
        "실행 중인 샌드박스에서만 된다",
        "디스크를 읽고 쓰는 것이 게스트이기 때문이다",
    ),
    (
        "stop, restart, rm -f",
        "시그널은 Docker와 같다. -t가 지나면 SIGKILL 대신 게스트를 끈다 (sync 뒤 전원 끄기, 종료 코드 137). rm -f는 시그널 없이 끈다",
        "샌드박스 하나가 VM 하나다. 정지 = VM 종료",
    ),
    (
        "기본 프로세스와 시그널",
        "기본 프로세스는 PID 1이 아니다(게스트 init이 PID 1). 그래서 핸들러가 없는 프로세스도 SIGTERM으로 끝난다",
        "VM 안의 PID 1은 sbx의 게스트 init이다",
    ),
    (
        "stop -t, --stop-timeout",
        "음수(무한히 기다림)를 받지 않는다",
        "기다림은 호출한 쪽이 끊을 수 있다(Ctrl+C 뒤 kill)",
    ),
    (
        "다른 터미널에서 실행 중인 샌드박스의 stop, rm -f",
        "게스트를 끄고 기다린다. 꺼지지 않으면 오류 (VM을 강제로 끝낼 수 있는 것은 VM을 띄운 프로세스뿐이다)",
        "VM은 띄운 프로세스가 소유한다",
    ),
];

/// 정규화된 참조를 Docker처럼 줄인다: `docker.io/library/alpine:3.20` → `alpine:3.20`,
/// `docker.io/user/app:1` → `user/app:1`. 다른 레지스트리는 그대로.
pub fn familiar(reference: &str) -> &str {
    reference
        .strip_prefix("docker.io/library/")
        .or_else(|| reference.strip_prefix("docker.io/"))
        .unwrap_or(reference)
}

/// 지난 시간을 Docker `ps`의 CREATED처럼 쓴다 ("3 minutes ago"). 기준은 Docker(go-units `HumanDuration`)와 같다.
pub fn human_ago(elapsed: std::time::Duration) -> String {
    let s = elapsed.as_secs();
    let (m, h) = (s / 60, (s as f64 / 3600.0).round() as u64);
    let plural = |n: u64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    let d = if s < 1 {
        "Less than a second".to_owned()
    } else if s < 60 {
        plural(s, "second")
    } else if m == 1 {
        "About a minute".to_owned()
    } else if m < 60 {
        plural(m, "minute")
    } else if h == 1 {
        "About an hour".to_owned()
    } else if h < 48 {
        plural(h, "hour")
    } else if h < 24 * 7 * 2 {
        plural(h / 24, "day")
    } else if h < 24 * 30 * 2 {
        plural(h / 24 / 7, "week")
    } else if h < 24 * 365 * 2 {
        plural(h / 24 / 30, "month")
    } else {
        plural(h / 24 / 365, "year")
    };
    format!("{d} ago")
}

/// UTC RFC 3339 (`2026-11-10T03:04:05Z`). JSON 출력에 쓴다.
pub fn rfc3339(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    format!("{}Z", civil(secs))
}

/// [`rfc3339`]과 같고 나노초 9자리까지 쓴다(이벤트 시각). 입력은 유닉스 시각 나노초.
pub fn rfc3339_ns(ns: u64) -> String {
    let secs = (ns / 1_000_000_000) as i64;
    format!("{}.{:09}Z", civil(secs), ns % 1_000_000_000)
}

/// 유닉스 시각(초)을 `YYYY-MM-DDTHH:MM:SS`(UTC)로.
fn civil(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // 일 수를 그레고리력 날짜로 (Howard Hinnant의 civil_from_days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// 로그 기록(출력 조각)들의 마지막 `n`줄. 조각을 줄로 나누고, 같은 스트림에서 이어지는 조각은 한 줄로 붙인다.
/// 끝에 줄바꿈이 없는 마지막 조각도 한 줄로 센다(Docker `logs --tail`과 같다). 붙인 줄의 시각(`T`)은 첫 조각의 것이다.
pub fn last_lines<S: Copy + PartialEq, T: Copy>(
    records: Vec<(S, T, Vec<u8>)>,
    n: usize,
) -> Vec<(S, T, Vec<u8>)> {
    let mut lines: Vec<(S, T, Vec<u8>)> = Vec::new();
    for (stream, time, data) in records {
        for piece in data.split_inclusive(|&b| b == b'\n') {
            match lines.last_mut() {
                Some((s, _, line)) if *s == stream && line.last() != Some(&b'\n') => {
                    line.extend_from_slice(piece)
                }
                _ => lines.push((stream, time, piece.to_vec())),
            }
        }
    }
    let skip = lines.len().saturating_sub(n);
    lines.split_off(skip)
}

/// `logs`의 시각 조건. 명시적 동작:
///
/// - `--since`, `--until`은 그 시각을 넣는다(Docker와 같다). 시각은 shim이 출력 조각을 받은 때다.
/// - `--tail`은 이 조건에 맞는 기록에서 마지막 N줄을 센다.
/// - 시각이 필요한데(`-t`의 텍스트 출력, `--since`, `--until`) 기록에 없으면(이 판 전의 sbx가 쓴 로그,
///   [`sbx_shim::log`]의 판 1) 아무것도 고르지 않고 오류다. 범위 안인지 알 수 없는 줄을 넣거나 빼서 조용히 틀리지 않는다.
///   그런 로그도 시각 없이는(`--format json`이면 `time: null`로) 다 보인다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogTimes {
    /// 이 시각(나노초)부터.
    pub since: Option<u64>,
    /// 이 시각(나노초)까지.
    pub until: Option<u64>,
    /// 줄마다 시각을 붙인다(`-t`, 텍스트 출력).
    pub prefix: bool,
}

/// 시각이 없는 로그에 시각이 필요한 옵션을 쓴 오류.
pub const LOG_WITHOUT_TIMES: &str = "this sandbox's log was written by an older sbx that did not record times, \
     so -t, --since and --until cannot be used (logs without them still shows it); recreate the sandbox to get times";

impl LogTimes {
    /// 시각이 `time_ns`인 기록을 보일지.
    pub fn wants(&self, time_ns: Option<u64>) -> Result<bool, &'static str> {
        match time_ns {
            Some(t) => Ok(self.since.is_none_or(|s| t >= s) && self.until.is_none_or(|u| t <= u)),
            None if self.prefix || self.since.is_some() || self.until.is_some() => {
                Err(LOG_WITHOUT_TIMES)
            }
            None => Ok(true),
        }
    }
}

/// `logs -t`: 출력 조각 `data`의 줄마다 앞에 `prefix`를 붙인다. `at_line_start`는 그 흐름이 줄 처음인지이고,
/// 조각 사이에 이어진다(흐름마다 하나). 줄이 조각 여럿에 걸치면 첫 조각의 것만 붙는다.
pub fn prefix_lines(data: &[u8], prefix: &str, at_line_start: &mut bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + prefix.len());
    for piece in data.split_inclusive(|&b| b == b'\n') {
        if *at_line_start {
            out.extend_from_slice(prefix.as_bytes());
        }
        out.extend_from_slice(piece);
        *at_line_start = piece.ends_with(b"\n");
    }
    out
}

/// 사람이 읽는 크기. Docker처럼 1000 단위다.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u + 1 < UNITS.len() {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes}B")
    } else {
        format!("{v:.1}{}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LOG_WITHOUT_TIMES, LogTimes, familiar, human_ago, human_size, last_lines, prefix_lines,
        rfc3339,
    };

    #[test]
    fn tail_counts_lines_across_records() {
        let r = |s: u8, t: u32, d: &str| (s, t, d.as_bytes().to_vec());
        let records = vec![
            r(1, 10, "a\nb"),
            r(1, 20, "c\n"),
            r(2, 30, "e1\n"),
            r(1, 40, "d\n"),
            r(1, 50, "no-newline"),
        ];
        let got = |n| {
            last_lines(records.clone(), n)
                .into_iter()
                .map(|(s, t, l)| (s, t, String::from_utf8(l).unwrap()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            got(2),
            [(1, 40, "d\n".into()), (1, 50, "no-newline".into())]
        );
        // 조각 둘에 걸친 줄은 첫 조각의 시각이다.
        assert_eq!(
            got(4),
            [
                (1, 10, "bc\n".into()),
                (2, 30, "e1\n".into()),
                (1, 40, "d\n".into()),
                (1, 50, "no-newline".into())
            ]
        );
        assert_eq!(got(0), []);
        assert_eq!(got(99).len(), 5);
    }

    #[test]
    fn log_times_select_a_closed_range_and_refuse_logs_without_times() {
        let range = LogTimes {
            since: Some(10),
            until: Some(20),
            prefix: false,
        };
        assert_eq!(range.wants(Some(9)), Ok(false));
        assert_eq!(range.wants(Some(10)), Ok(true));
        assert_eq!(range.wants(Some(20)), Ok(true));
        assert_eq!(range.wants(Some(21)), Ok(false));
        assert_eq!(range.wants(None), Err(LOG_WITHOUT_TIMES));
        let prefix = LogTimes {
            prefix: true,
            ..LogTimes::default()
        };
        assert_eq!(prefix.wants(None), Err(LOG_WITHOUT_TIMES));
        // 시각이 필요 없으면 시각 없는 기록도 보인다.
        assert_eq!(LogTimes::default().wants(None), Ok(true));
        assert_eq!(LogTimes::default().wants(Some(0)), Ok(true));
    }

    #[test]
    fn prefix_goes_before_each_line_once() {
        let mut start = true;
        assert_eq!(prefix_lines(b"a\nb", "T ", &mut start), b"T a\nT b");
        assert!(!start);
        // 이어지는 조각은 줄 처음이 아니다.
        assert_eq!(prefix_lines(b"c\n", "U ", &mut start), b"c\n");
        assert!(start);
        assert_eq!(prefix_lines(b"\n\n", "V ", &mut start), b"V \nV \n");
    }
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn times_read_like_docker() {
        let ago = |s: u64| human_ago(Duration::from_secs(s));
        assert_eq!(ago(0), "Less than a second ago");
        assert_eq!(ago(1), "1 second ago");
        assert_eq!(ago(59), "59 seconds ago");
        assert_eq!(ago(90), "About a minute ago");
        assert_eq!(ago(45 * 60), "45 minutes ago");
        assert_eq!(ago(3600), "About an hour ago");
        assert_eq!(ago(5 * 3600), "5 hours ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(ago(20 * 86_400), "2 weeks ago");
        assert_eq!(ago(90 * 86_400), "3 months ago");
        assert_eq!(ago(800 * 86_400), "2 years ago");
        let at = |s: u64| rfc3339(UNIX_EPOCH + Duration::from_secs(s));
        assert_eq!(at(0), "1970-01-01T00:00:00Z");
        assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(at(1_793_574_245), "2026-11-01T23:04:05Z");
        assert_eq!(
            rfc3339_ns(1_794_316_814_000_000_007),
            "2026-11-10T13:20:14.000000007Z"
        );
    }

    #[test]
    fn familiar_names_and_sizes() {
        assert_eq!(familiar("docker.io/library/alpine:3.20"), "alpine:3.20");
        assert_eq!(familiar("docker.io/user/app:1"), "user/app:1");
        assert_eq!(familiar("ghcr.io/o/r:t"), "ghcr.io/o/r:t");
        assert_eq!(familiar("localhost:5055/x:1"), "localhost:5055/x:1");
        assert_eq!(human_size(999), "999B");
        assert_eq!(human_size(3_456_789), "3.5MB");
    }

    use super::*;
    use Step::*;

    #[test]
    fn pull_policy_default_matches_docker() {
        assert_eq!(PullPolicy::default(), PullPolicy::Missing);
        assert_eq!("never".parse::<PullPolicy>().unwrap(), PullPolicy::Never);
        assert!("sometimes".parse::<PullPolicy>().is_err());
    }

    #[test]
    fn create_steps_table() {
        use PullPolicy::*;
        let cases = [
            (Missing, false, false, Ok(vec![Pull, Prepare, Create])),
            (Missing, true, false, Ok(vec![Prepare, Create])),
            (Missing, true, true, Ok(vec![Create])),
            (Always, false, false, Ok(vec![Pull, Prepare, Create])),
            (Always, true, false, Ok(vec![Pull, Prepare, Create])),
            (Always, true, true, Ok(vec![Pull, Prepare, Create])),
            (Never, false, false, Err(NotReady::Image)),
            (Never, true, false, Err(NotReady::Rootfs)),
            (Never, true, true, Ok(vec![Create])),
        ];
        for (policy, image, rootfs, expected) in cases {
            assert_eq!(
                create_steps(policy, image, rootfs),
                expected,
                "{policy:?} image={image} rootfs={rootfs}"
            );
        }
    }

    #[test]
    fn network_policy_table() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(network_policy("none", &[]), Ok(NetworkPolicy::Disabled));
        assert!(network_policy("none", &v(&["a.com:443"])).is_err());
        assert!(network_policy("allow", &[]).is_err());
        assert!(network_policy("bridge", &[]).is_err());
        assert!(network_policy("allow", &v(&["a.com"])).is_err());
        assert_eq!(
            network_policy("allow", &v(&["A.com:443", "*.b.org:*"])),
            Ok(NetworkPolicy::Allow(vec![
                "a.com:443".parse().unwrap(),
                "*.b.org:*".parse().unwrap()
            ]))
        );
    }

    #[test]
    fn exit_codes_match_docker() {
        assert_eq!(command_failure_code(ErrorKind::NotFound), 127);
        assert_eq!(command_failure_code(ErrorKind::PermissionDenied), 126);
        assert_eq!(command_failure_code(ErrorKind::InvalidArgument), 125);
        assert_eq!(exit_byte(0), 0);
        assert_eq!(exit_byte(137), 137);
        assert_eq!(exit_byte(-1), 255);
        assert_eq!(exit_byte(256), 255);
    }

    #[test]
    fn forwarded_signals_use_linux_numbers() {
        let n = |name| FORWARDED_SIGNALS.iter().find(|s| s.0 == name).unwrap().1;
        assert_eq!((n("INT"), n("TERM"), n("USR1"), n("USR2")), (2, 15, 10, 12));
    }

    #[test]
    fn rmi_table() {
        let id = || vec![SandboxId::new("a").unwrap()];
        assert_eq!(rmi_decision(vec![], vec![], false), RmiDecision::Remove);
        assert_eq!(rmi_decision(vec![], vec![], true), RmiDecision::Remove);
        assert_eq!(
            rmi_decision(vec![], id(), false),
            RmiDecision::RefuseStopped(id())
        );
        assert_eq!(rmi_decision(vec![], id(), true), RmiDecision::Remove);
        assert_eq!(
            rmi_decision(id(), vec![], false),
            RmiDecision::RefuseRunning(id())
        );
        assert_eq!(
            rmi_decision(id(), vec![], true),
            RmiDecision::RefuseRunning(id())
        );
    }
    fn id(c: char) -> ImageId {
        ImageId::new(
            sbx_model::Digest::parse(&format!("sha256:{}", c.to_string().repeat(64))).unwrap(),
        )
    }

    #[test]
    fn match_image_order() {
        let mut refs = BTreeMap::new();
        refs.insert("docker.io/library/alpine:latest".to_string(), id('a'));
        let ids = [id('a'), id('b')];
        assert_eq!(
            match_image(
                "alpine",
                Some("docker.io/library/alpine:latest"),
                &refs,
                &ids
            ),
            Ok(ImageMatch::Ref {
                reference: "docker.io/library/alpine:latest".into(),
                id: id('a')
            })
        );
        assert_eq!(
            match_image("bbbbbbbbbbbb", None, &refs, &ids),
            Ok(ImageMatch::Id(id('b')))
        );
        assert_eq!(
            match_image(&format!("sha256:{}", "b".repeat(64)), None, &refs, &ids),
            Ok(ImageMatch::Id(id('b')))
        );
        // 12자리보다 짧은 앞부분은 받지 않는다.
        assert!(matches!(
            match_image("bbbb", None, &refs, &ids),
            Err(LookupError::NotFound(_))
        ));
        assert!(matches!(
            match_image(
                "ubuntu",
                Some("docker.io/library/ubuntu:latest"),
                &refs,
                &ids
            ),
            Err(LookupError::NotFound(_))
        ));
    }

    #[test]
    fn ambiguous_id_prefix() {
        let x = ImageId::new(
            sbx_model::Digest::parse(&format!("sha256:{}{}", "c".repeat(12), "0".repeat(52)))
                .unwrap(),
        );
        let y = ImageId::new(
            sbx_model::Digest::parse(&format!("sha256:{}{}", "c".repeat(12), "1".repeat(52)))
                .unwrap(),
        );
        assert!(matches!(
            match_image(&"c".repeat(12), None, &BTreeMap::new(), &[x, y]),
            Err(LookupError::Ambiguous(_))
        ));
    }

    #[test]
    fn rmi_action_table() {
        let by_ref = ImageMatch::Ref {
            reference: "r".into(),
            id: id('a'),
        };
        let by_id = ImageMatch::Id(id('a'));
        assert_eq!(rmi_action(&by_ref, 2, false), RmiAction::Untag("r".into()));
        assert_eq!(rmi_action(&by_ref, 2, true), RmiAction::Untag("r".into()));
        assert_eq!(rmi_action(&by_ref, 1, false), RmiAction::Delete(id('a')));
        assert_eq!(rmi_action(&by_id, 1, false), RmiAction::Delete(id('a')));
        assert_eq!(rmi_action(&by_id, 0, false), RmiAction::Delete(id('a')));
        assert_eq!(
            rmi_action(&by_id, 2, false),
            RmiAction::RefuseMultipleRefs(id('a'))
        );
        assert_eq!(rmi_action(&by_id, 2, true), RmiAction::Delete(id('a')));
    }
}
