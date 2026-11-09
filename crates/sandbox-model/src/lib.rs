//! 여러 크레이트가 함께 쓰는 자료형.
//!
//! 동작(파일, 프로세스, 네트워크)은 두지 않는다. 값과 값의 검증만 둔다.
//!
//! - 의존: 없음
//! - 쓰지 않는 곳: `sandbox-proto`, `sandbox-guest` (게스트 바이너리를 작게 유지하기 위해)

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// `Arc<dyn Trait>`으로 갈아 끼우는 인터페이스(실행기, 이미지 소스)에 쓰는 비동기 반환 타입.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 블로킹 작업을 새 스레드에서 돌리고 그 결과를 기다리는 future.
///
/// 특정 비동기 런타임에 의존하지 않는다. 파일과 소켓을 블로킹으로 다루는 구현이
/// 호출자의 비동기 스레드를 막지 않게 할 때 쓴다.
pub fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> BoxFuture<'static, T> {
    use std::sync::{Arc, Mutex};
    use std::task::{Poll, Waker};

    struct Slot<T> {
        value: Option<T>,
        waker: Option<Waker>,
    }
    let slot = Arc::new(Mutex::new(Slot {
        value: None,
        waker: None,
    }));
    let inner = slot.clone();
    std::thread::spawn(move || {
        let v = f();
        let mut s = inner.lock().unwrap_or_else(|e| e.into_inner());
        s.value = Some(v);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    });
    Box::pin(std::future::poll_fn(move |cx| {
        let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
        match s.value.take() {
            Some(v) => Poll::Ready(v),
            None => {
                s.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }))
}

/// 값 해석 실패.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 무엇을 해석하다 실패했는지.
    pub what: &'static str,
    /// 입력값.
    pub input: String,
}

impl ParseError {
    /// 만든다.
    pub fn new(what: &'static str, input: &str) -> Self {
        Self {
            what,
            input: input.to_owned(),
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid {}: {:?}", self.what, self.input)
    }
}

impl std::error::Error for ParseError {}

/// 샌드박스 ID.
///
/// 영문 소문자, 숫자, `-`만 쓰고 길이는 1~64자다.
/// data_dir 아래 디렉터리 이름으로 그대로 쓰므로 모든 OS에서 안전한 문자만 허용한다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SandboxId(String);

impl SandboxId {
    /// 규칙을 검사해 만든다.
    pub fn new(id: &str) -> Result<Self, ParseError> {
        let ok = (1..=64).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if ok {
            Ok(Self(id.to_owned()))
        } else {
            Err(ParseError::new("sandbox id", id))
        }
    }

    /// 문자열 값.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SandboxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for SandboxId {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<SandboxId> for String {
    fn from(v: SandboxId) -> String {
        v.0
    }
}

/// 내용 주소. `sha256:<64자리 소문자 hex>` 형식이다. 지금은 sha256만 받는다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest(String);

impl Digest {
    const PREFIX: &'static str = "sha256:";

    /// 해석한다.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let hex = s
            .strip_prefix(Self::PREFIX)
            .ok_or_else(|| ParseError::new("digest", s))?;
        let ok = hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if ok {
            Ok(Self(s.to_owned()))
        } else {
            Err(ParseError::new("digest", s))
        }
    }

    /// 전체 문자열(`sha256:...`).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `sha256:` 뒤의 hex.
    ///
    /// 파일 이름에는 이 값만 쓴다. `:`는 Windows 파일 이름에 쓸 수 없기 때문이다.
    pub fn hex(&self) -> &str {
        &self.0[Self::PREFIX.len()..]
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for Digest {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<Digest> for String {
    fn from(v: Digest) -> String {
        v.0
    }
}

/// 이미지 ID. 한 플랫폼의 manifest digest다.
///
/// 태그는 받는 순간 이 값으로 고정된다. 샌드박스는 항상 이 값으로 이미지를 가리킨다.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ImageId(Digest);

impl ImageId {
    /// manifest digest로 만든다.
    pub fn new(manifest: Digest) -> Self {
        Self(manifest)
    }

    /// manifest digest.
    pub fn digest(&self) -> &Digest {
        &self.0
    }
}

impl fmt::Display for ImageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// 레이어 압축 형식.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Compression {
    /// 압축 없음 (tar).
    None,
    /// gzip (tar+gzip).
    Gzip,
}

/// 이미지 레이어 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layer {
    /// 압축된 blob의 digest. 내려받을 때 검증한다.
    pub digest: Digest,
    /// 압축을 푼 tar의 digest. rootfs를 만들 때 검증한다.
    pub diff_id: Digest,
    /// 압축된 크기(바이트).
    pub size: u64,
    /// 압축 형식.
    pub compression: Compression,
}

/// CPU 아키텍처.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Arch {
    /// x86_64 (OCI 표기 `amd64`).
    #[serde(rename = "amd64")]
    X86_64,
    /// aarch64 (OCI 표기 `arm64`).
    #[serde(rename = "arm64")]
    Aarch64,
}

impl Arch {
    /// 이 프로그램이 빌드된 아키텍처. 지원하지 않는 아키텍처면 `None`.
    pub const fn host() -> Option<Arch> {
        if cfg!(target_arch = "x86_64") {
            Some(Arch::X86_64)
        } else if cfg!(target_arch = "aarch64") {
            Some(Arch::Aarch64)
        } else {
            None
        }
    }

    /// OCI 플랫폼 표기.
    pub const fn oci_name(self) -> &'static str {
        match self {
            Arch::X86_64 => "amd64",
            Arch::Aarch64 => "arm64",
        }
    }
}

/// rootfs 디스크의 파일시스템 형식.
///
/// 구현이 생길 때마다 항목을 하나씩 추가한다. 항목이 있다는 것은
/// `sandbox-rootfs`에 그 형식의 writer가 있다는 뜻이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RootfsFormat {
    /// 읽기 전용 이미지용 파일시스템. 첫 번째 구현.
    Erofs,
}

impl RootfsFormat {
    /// 모든 형식.
    pub const ALL: &'static [RootfsFormat] = &[RootfsFormat::Erofs];

    /// 이름. CLI 옵션 값과 파일 확장자로 쓴다.
    pub const fn as_str(self) -> &'static str {
        match self {
            RootfsFormat::Erofs => "erofs",
        }
    }
}

impl FromStr for RootfsFormat {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|f| f.as_str() == s)
            .ok_or_else(|| ParseError::new("rootfs format", s))
    }
}

impl fmt::Display for RootfsFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 이미지가 정한 실행 기본값. `sandbox-oci`가 OCI image config에서 변환해 채운다.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImageConfig {
    /// `Entrypoint`.
    pub entrypoint: Vec<String>,
    /// `Cmd`.
    pub cmd: Vec<String>,
    /// `Env`. 순서를 지킨다.
    pub env: Vec<(String, String)>,
    /// `WorkingDir`.
    pub working_dir: Option<String>,
    /// `User`. 해석 전 원문.
    pub user: Option<String>,
    /// `StopSignal`. 해석 전 원문.
    pub stop_signal: Option<String>,
    /// `Labels`.
    pub labels: BTreeMap<String, String>,
}

/// 사용자나 그룹을 가리키는 값.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ident {
    /// 숫자 ID.
    Id(u32),
    /// 이름. 게스트에서 rootfs의 `/etc/passwd`, `/etc/group`으로 숫자로 바꾼다.
    Name(String),
}

impl Ident {
    fn parse(s: &str, whole: &str) -> Result<Self, ParseError> {
        if s.is_empty() {
            return Err(ParseError::new("user", whole));
        }
        if s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse()
                .map(Ident::Id)
                .map_err(|_| ParseError::new("user", whole))
        } else {
            Ok(Ident::Name(s.to_owned()))
        }
    }
}

impl fmt::Display for Ident {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ident::Id(id) => write!(f, "{id}"),
            Ident::Name(name) => f.write_str(name),
        }
    }
}

/// 프로세스를 실행할 사용자.
///
/// `name`, `uid`, `name:group`, `uid:gid` 형식을 받는다. Docker `--user`와 같다.
/// 이름은 rootfs를 봐야 풀 수 있으므로 게스트에서 실행할 때 숫자로 바꾼다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UserSpec {
    /// 사용자.
    pub user: Ident,
    /// 그룹. 없으면 사용자의 기본 그룹.
    pub group: Option<Ident>,
}

impl UserSpec {
    /// root (uid 0).
    pub fn root() -> Self {
        Self {
            user: Ident::Id(0),
            group: None,
        }
    }
}

impl FromStr for UserSpec {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once(':') {
            Some((user, group)) => Ok(Self {
                user: Ident::parse(user, s)?,
                group: Some(Ident::parse(group, s)?),
            }),
            None => Ok(Self {
                user: Ident::parse(s, s)?,
                group: None,
            }),
        }
    }
}

impl fmt::Display for UserSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.group {
            Some(group) => write!(f, "{}:{}", self.user, group),
            None => write!(f, "{}", self.user),
        }
    }
}

impl TryFrom<String> for UserSpec {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<UserSpec> for String {
    fn from(v: UserSpec) -> String {
        v.to_string()
    }
}

/// 샌드박스의 기본 프로세스. 이미지 기본값에 호출자 값을 덮어쓴 결과다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessDefaults {
    /// 실행할 argv. 비어 있을 수 없다.
    pub args: Vec<String>,
    /// 환경 변수.
    pub env: Vec<(String, String)>,
    /// 작업 디렉터리 (게스트 경로).
    pub cwd: String,
    /// 실행 사용자.
    pub user: UserSpec,
}

/// 호출자가 이미지 기본값을 덮어쓰는 값. Docker `run`의 인자와 옵션에 대응한다.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessOverrides {
    /// `--entrypoint`. `Some(빈 목록)`은 이미지 `Entrypoint`를 지운다.
    pub entrypoint: Option<Vec<String>>,
    /// 이미지 이름 뒤의 인자. 비어 있으면 주지 않은 것이다.
    pub args: Vec<String>,
    /// `-e`.
    pub env: Vec<(String, String)>,
    /// `-w`.
    pub cwd: Option<String>,
    /// `-u`.
    pub user: Option<UserSpec>,
}

/// 실행 중인 샌드박스 안에서 추가로 실행할 프로세스. Docker `exec`에 대응한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecSpec {
    /// 실행할 argv.
    pub args: Vec<String>,
    /// 기본 프로세스 환경에 더할 환경 변수.
    pub env: Vec<(String, String)>,
    /// 작업 디렉터리. 없으면 기본 프로세스의 값.
    pub cwd: Option<String>,
    /// 실행 사용자. 없으면 기본 프로세스의 값.
    pub user: Option<UserSpec>,
    /// 터미널 할당 여부.
    pub tty: bool,
}

/// VM 자원.
///
/// 기본값은 이 크레이트가 정하지 않는다. 표현 계층(CLI 설정 등)이 정해서 넘긴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    /// vCPU 개수.
    pub vcpus: u8,
    /// 메모리(MiB).
    pub memory_mib: u32,
    /// 샌드박스별 쓰기 가능 디스크 크기(MiB).
    pub scratch_disk_mib: u32,
}

/// 폴더 공유 방식. 호출자가 고른다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountKind {
    /// 실시간 공유.
    ///
    /// 실행기 capability `live_share`가 있어야 한다. 없으면 생성이 실패한다. `Copy`로 바꾸지 않는다.
    Bind,
    /// 시작할 때 복사해 넣는다. 결과는 `copy_out`으로 명시적으로 꺼낸다.
    Copy,
}

/// 폴더 공유 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    /// 호스트 경로.
    pub source: PathBuf,
    /// 게스트 경로.
    pub destination: String,
    /// 공유 방식.
    pub kind: MountKind,
    /// 게스트에서 읽기 전용인지.
    pub readonly: bool,
}

/// 네트워크 정책.
///
/// 무엇을 허용할지는 호출자가 정하고, 샌드박스는 강제만 한다. 어느 경우든 VM에는 네트워크 장치가 없다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkPolicy {
    /// 네트워크 없음.
    Disabled,
    /// 허용 목록에 맞는 TCP 연결만 호스트가 대신 연다(게스트 안의 HTTP 프록시를 거친다).
    /// 목록은 비어 있지 않다. 규칙은 [`AllowRule`].
    Allow(Vec<AllowRule>),
}

/// 허용 규칙 하나: `호스트:포트`.
///
/// - 호스트: 이름(대소문자 무시), `*.접미사`(그 아래 이름만, 접미사 자체는 아님), IP 주소(IPv6은 `[...]`로 감싼다).
/// - 포트: 숫자, 또는 `*`(모든 포트). 생략할 수 없다.
///
/// 이름이 루프백, 사설, 링크 로컬 주소로 풀리면 거부한다(규칙이 그 주소를 IP로 직접 적었거나 이름이 `localhost`면 허용).
/// 이것은 실행 코어가 연결할 때 강제한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowRule {
    /// 소문자 이름, `*.접미사`, 또는 IP 주소 문자열(IPv6은 괄호 없이).
    pub host: String,
    /// 포트. `None`이면 모든 포트.
    pub port: Option<u16>,
}

impl AllowRule {
    /// 규칙이 IP 주소를 직접 적었는지.
    pub fn is_ip(&self) -> bool {
        self.host.parse::<std::net::IpAddr>().is_ok()
    }

    /// 요청한 호스트(이름 또는 IP 문자열)와 포트가 이 규칙에 맞는지. 호스트는 대소문자를 무시한다.
    pub fn matches(&self, host: &str, port: u16) -> bool {
        if self.port.is_some_and(|p| p != port) {
            return false;
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        match self.host.strip_prefix("*.") {
            Some(suffix) => host
                .strip_suffix(suffix)
                .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.')),
            None => host == self.host,
        }
    }
}

impl fmt::Display for AllowRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]", self.host)?;
        } else {
            f.write_str(&self.host)?;
        }
        match self.port {
            Some(p) => write!(f, ":{p}"),
            None => f.write_str(":*"),
        }
    }
}

impl FromStr for AllowRule {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseError::new("allow rule (HOST:PORT or HOST:*)", s);
        let (host, port) = if let Some(rest) = s.strip_prefix('[') {
            let (h, p) = rest.split_once("]:").ok_or_else(bad)?;
            h.parse::<std::net::Ipv6Addr>().map_err(|_| bad())?;
            (h.to_owned(), p)
        } else {
            let (h, p) = s.rsplit_once(':').ok_or_else(bad)?;
            if h.contains(':') {
                return Err(bad());
            }
            (h.trim_end_matches('.').to_ascii_lowercase(), p)
        };
        let name = host.strip_prefix("*.").unwrap_or(&host);
        let label_ok = |l: &str| {
            !l.is_empty()
                && l.len() <= 63
                && l.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        let valid = host.parse::<std::net::IpAddr>().is_ok()
            || (!name.is_empty() && name.len() <= 253 && name.split('.').all(label_ok));
        if !valid {
            return Err(bad());
        }
        let port = match port {
            "*" => None,
            p => Some(p.parse::<u16>().ok().filter(|&n| n != 0).ok_or_else(bad)?),
        };
        Ok(Self { host, port })
    }
}

/// 시그널. 게스트가 Linux이므로 Linux 번호로 담는다(호스트 OS의 번호와 다를 수 있다).
///
/// 받는 형식(Docker `--signal`, image config `StopSignal`과 같다): 이름(`TERM`, `SIGTERM`, 대소문자 무관),
/// 번호(`15`), 실시간 시그널(`RTMIN`, `RTMIN+3`, `RTMAX-1`). 번호는 1..=64다.
/// 쓸 때는 이름(`SIGTERM`)으로, 이름이 없는 번호는 숫자로 쓴다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Signal(i32);

/// (이름, Linux 번호). x86_64와 aarch64에서 같다.
const SIGNALS: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("TRAP", 5),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("STKFLT", 16),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
    ("URG", 23),
    ("XCPU", 24),
    ("XFSZ", 25),
    ("VTALRM", 26),
    ("PROF", 27),
    ("WINCH", 28),
    ("IO", 29),
    ("PWR", 30),
    ("SYS", 31),
];

/// 다른 이름.
const SIGNAL_ALIASES: &[(&str, i32)] = &[("IOT", 6), ("CLD", 17), ("POLL", 29)];

impl Signal {
    /// SIGTERM.
    pub const TERM: Signal = Signal(15);
    /// SIGKILL.
    pub const KILL: Signal = Signal(9);
    /// 실시간 시그널의 처음 (C 라이브러리가 쓰는 둘을 뺀 값, Docker와 같다).
    pub const RTMIN: i32 = 34;
    /// 실시간 시그널의 끝.
    pub const RTMAX: i32 = 64;

    /// 번호로 만든다. 1..=64가 아니면 `None`.
    pub fn new(n: i32) -> Option<Self> {
        (1..=Self::RTMAX).contains(&n).then_some(Self(n))
    }

    /// Linux 번호.
    pub fn number(self) -> i32 {
        self.0
    }
}

impl FromStr for Signal {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || ParseError::new("signal", s);
        if let Ok(n) = s.parse::<i32>() {
            return Self::new(n).ok_or_else(bad);
        }
        let upper = s.to_ascii_uppercase();
        let name = upper.strip_prefix("SIG").unwrap_or(&upper);
        if let Some(&(_, n)) = SIGNALS
            .iter()
            .chain(SIGNAL_ALIASES)
            .find(|(k, _)| *k == name)
        {
            return Ok(Self(n));
        }
        let rt = |rest: &str, base: i32, sign: i32| -> Option<Signal> {
            if rest.is_empty() {
                return Some(Self(base));
            }
            let off = rest
                .strip_prefix(if sign > 0 { '+' } else { '-' })?
                .parse::<i32>()
                .ok()?;
            let n = base + sign * off;
            (Self::RTMIN..=Self::RTMAX).contains(&n).then_some(Self(n))
        };
        let found = if let Some(rest) = name.strip_prefix("RTMIN") {
            rt(rest, Self::RTMIN, 1)
        } else if let Some(rest) = name.strip_prefix("RTMAX") {
            rt(rest, Self::RTMAX, -1)
        } else {
            None
        };
        found.ok_or_else(bad)
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match SIGNALS.iter().find(|(_, n)| *n == self.0) {
            Some((name, _)) => write!(f, "SIG{name}"),
            None if self.0 >= Self::RTMIN => write!(f, "SIGRTMIN+{}", self.0 - Self::RTMIN),
            None => write!(f, "{}", self.0),
        }
    }
}

impl TryFrom<String> for Signal {
    type Error = ParseError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl From<Signal> for String {
    fn from(v: Signal) -> String {
        v.to_string()
    }
}

/// 멈추는 방법. Docker의 `StopSignal`, `StopTimeout`과 같다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StopSpec {
    /// 멈출 때 기본 프로세스에 먼저 보내는 시그널.
    pub signal: Signal,
    /// 그 뒤 기다리는 시간(초). 없으면 호출자(표현 계층)가 정한다.
    pub timeout_secs: Option<u32>,
}

impl Default for StopSpec {
    fn default() -> Self {
        Self {
            signal: Signal::TERM,
            timeout_secs: None,
        }
    }
}

/// 샌드박스를 만드는 데 필요한 전부. 생성 후에는 바뀌지 않는다.
///
/// 수명(VM을 띄운 프로세스와 함께 끝나는지, 남는지)은 여기에 없다. Docker처럼 시작할 때 정한다:
/// 시작한 프로세스가 VM을 소유하고, 남기려면 `sandbox-shim`이 시작한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxSpec {
    /// digest로 고정된 이미지.
    pub image: ImageId,
    /// rootfs 형식. 항상 명시한다. 기본값은 표현 계층이 정한다.
    pub rootfs_format: RootfsFormat,
    /// 기본 프로세스.
    pub process: ProcessDefaults,
    /// VM 자원.
    pub resources: Resources,
    /// 폴더 공유.
    pub mounts: Vec<Mount>,
    /// 네트워크 정책.
    pub network: NetworkPolicy,
    /// 게스트 hostname.
    pub hostname: Option<String>,
    /// 호출자가 붙이는 라벨.
    pub labels: BTreeMap<String, String>,
    /// 멈추는 방법. 이 필드가 없던 기록은 기본값(SIGTERM)으로 읽는다.
    #[serde(default)]
    pub stop: StopSpec,
    /// 기본 프로세스의 표준 입력을 연다(Docker `-i`, `OpenStdin`). 시작할 때마다 이대로 연다.
    #[serde(default)]
    pub open_stdin: bool,
    /// 기본 프로세스에 터미널을 붙인다(Docker `-t`, `Tty`). 시작할 때마다 이대로 붙인다.
    #[serde(default)]
    pub tty: bool,
}

/// 샌드박스 상태. 이름은 OCI Runtime Spec의 상태를 따른다.
///
/// OCI와 다른 점: OCI에서 `stopped`는 끝 상태지만, 여기서는 Docker처럼
/// `stopped`에서 다시 `running`으로 갈 수 있다(`start`로 재시작).
/// 실패는 별도 상태가 아니라 `stopped`에 딸린 오류 정보로 둔다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// 만드는 중.
    Creating,
    /// 만들어졌고 아직 시작하지 않음.
    Created,
    /// 실행 중.
    Running,
    /// 멈춤.
    Stopped,
}

impl State {
    /// OCI 표기.
    pub const fn as_str(self) -> &'static str {
        match self {
            State::Creating => "creating",
            State::Created => "created",
            State::Running => "running",
            State::Stopped => "stopped",
        }
    }

    /// 허용되는 상태 전이.
    ///
    /// | 지금 | 갈 수 있는 상태 |
    /// |---|---|
    /// | creating | created, stopped (생성 실패) |
    /// | created | running, stopped |
    /// | running | stopped |
    /// | stopped | running (재시작) |
    pub const fn can_transition_to(self, next: State) -> bool {
        matches!(
            (self, next),
            (State::Creating, State::Created)
                | (State::Creating, State::Stopped)
                | (State::Created, State::Running)
                | (State::Created, State::Stopped)
                | (State::Running, State::Stopped)
                | (State::Stopped, State::Running)
        )
    }
}

#[cfg(test)]
mod signal_tests {
    use super::*;

    #[test]
    fn signals_parse_like_docker() {
        let n = |s: &str| s.parse::<Signal>().map(Signal::number);
        assert_eq!(n("TERM"), Ok(15));
        assert_eq!(n("sigkill"), Ok(9));
        assert_eq!(n("SIGUSR1"), Ok(10));
        assert_eq!(n("9"), Ok(9));
        assert_eq!(n("IOT"), Ok(6));
        assert_eq!(n("RTMIN"), Ok(34));
        assert_eq!(n("SIGRTMIN+3"), Ok(37));
        assert_eq!(n("RTMAX-1"), Ok(63));
        for bad in [
            "", "0", "65", "-1", "NOPE", "RTMIN+31", "RTMAX+1", "RTMIN3", "SIG",
        ] {
            assert!(bad.parse::<Signal>().is_err(), "{bad}");
        }
        assert_eq!(Signal::TERM.to_string(), "SIGTERM");
        assert_eq!(Signal::new(37).unwrap().to_string(), "SIGRTMIN+3");
        assert_eq!(Signal::new(32).unwrap().to_string(), "32");
        // 쓴 것을 다시 읽으면 같다.
        for i in 1..=64 {
            let s = Signal::new(i).unwrap();
            assert_eq!(s.to_string().parse::<Signal>(), Ok(s), "{i}");
        }
    }

    #[test]
    fn old_specs_without_stop_read_as_sigterm() {
        let v: StopSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(v, StopSpec::default());
        let v: StopSpec = serde_json::from_str(r#"{"signal":"SIGINT","timeout_secs":3}"#).unwrap();
        assert_eq!((v.signal.number(), v.timeout_secs), (2, Some(3)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_rules_parse_and_match() {
        let r = |s: &str| s.parse::<AllowRule>().unwrap();
        assert_eq!(
            r("PyPI.org:443"),
            AllowRule {
                host: "pypi.org".into(),
                port: Some(443)
            }
        );
        assert_eq!(r("*.github.com:*").port, None);
        assert_eq!(r("[::1]:8080").host, "::1");
        assert_eq!(r("10.0.0.1:22").to_string(), "10.0.0.1:22");
        assert_eq!(r("[::1]:*").to_string(), "[::1]:*");
        for bad in [
            "pypi.org",
            "pypi.org:0",
            "pypi.org:x",
            ":443",
            "a b:1",
            "::1:80",
            "[zz]:1",
            "*.:1",
        ] {
            assert!(bad.parse::<AllowRule>().is_err(), "{bad}");
        }

        let exact = r("pypi.org:443");
        assert!(exact.matches("PYPI.ORG", 443));
        assert!(exact.matches("pypi.org.", 443));
        assert!(!exact.matches("pypi.org", 80));
        assert!(!exact.matches("files.pypi.org", 443));
        let wild = r("*.github.com:*");
        assert!(wild.matches("api.github.com", 1));
        assert!(wild.matches("a.b.github.com", 443));
        assert!(!wild.matches("github.com", 443));
        assert!(!wild.matches("evilgithub.com", 443));
        assert!(r("10.0.0.1:22").is_ip() && !exact.is_ip());
    }

    #[test]
    fn sandbox_id_rules() {
        assert!(SandboxId::new("abc-123").is_ok());
        assert!(SandboxId::new("").is_err());
        assert!(SandboxId::new("ABC").is_err());
        assert!(SandboxId::new("a/b").is_err());
        assert!(SandboxId::new(&"a".repeat(65)).is_err());
    }

    #[test]
    fn digest_rules() {
        let hex = "a".repeat(64);
        let d = Digest::parse(&format!("sha256:{hex}")).unwrap();
        assert_eq!(d.hex(), hex);
        assert!(Digest::parse(&hex).is_err());
        assert!(Digest::parse(&format!("sha256:{}", "A".repeat(64))).is_err());
        assert!(Digest::parse("sha256:abc").is_err());
        assert!(Digest::parse(&format!("sha512:{hex}")).is_err());
    }

    #[test]
    fn rootfs_format_names() {
        for f in RootfsFormat::ALL {
            assert_eq!(f.as_str().parse::<RootfsFormat>().unwrap(), *f);
        }
        assert!("ext4".parse::<RootfsFormat>().is_err());
    }

    #[test]
    fn user_spec_forms() {
        let parse = |s: &str| s.parse::<UserSpec>().unwrap();
        assert_eq!(parse("1000").user, Ident::Id(1000));
        assert_eq!(parse("app").user, Ident::Name("app".into()));
        assert_eq!(parse("1000:1000").group, Some(Ident::Id(1000)));
        assert_eq!(parse("app:staff").group, Some(Ident::Name("staff".into())));
        assert!("".parse::<UserSpec>().is_err());
        assert!(":1".parse::<UserSpec>().is_err());
        assert!("1:".parse::<UserSpec>().is_err());
        assert!("99999999999".parse::<UserSpec>().is_err());
        assert_eq!(parse("app:staff").to_string(), "app:staff");
    }

    #[test]
    fn state_transitions() {
        use State::*;
        let all = [Creating, Created, Running, Stopped];
        let allowed = [
            (Creating, Created),
            (Creating, Stopped),
            (Created, Running),
            (Created, Stopped),
            (Running, Stopped),
            (Stopped, Running),
        ];
        for from in all {
            for to in all {
                assert_eq!(
                    from.can_transition_to(to),
                    allowed.contains(&(from, to)),
                    "{} -> {}",
                    from.as_str(),
                    to.as_str()
                );
            }
        }
    }
}
