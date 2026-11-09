//! 메시지 종류와 인코딩.
//!
//! 종류 번호는 방향마다 범위가 다르다: 호스트 → 게스트는 `0x01..`, 게스트 → 호스트는 `0x81..`.
//! 방향을 잘못 해석하면 모르는 종류로 오류가 난다.

use std::io::{self, Read, Write};

use crate::codec::{DecodeError, DecodeResult, Decoder, Encoder};
use crate::frame::{read_frame, write_frame};

/// 사용자나 그룹. 이름은 게스트가 rootfs의 `/etc/passwd`, `/etc/group`으로 숫자로 바꾼다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ident {
    /// 숫자 ID.
    Id(u32),
    /// 이름.
    Name(String),
}

/// 실행 사용자.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    /// 사용자.
    pub user: Ident,
    /// 그룹. 없으면 `/etc/passwd`의 기본 그룹이다.
    ///
    /// Docker와 같은 규칙: 숫자 사용자가 `/etc/passwd`에 없으면 gid 0, 이름이 없으면 오류.
    /// 보조 그룹은 `/etc/group`에서 그 사용자 이름이 들어 있는 그룹들이다.
    pub group: Option<Ident>,
}

/// 프로세스 실행 요청. 값은 호스트가 이미 합친 최종 값이다. 게스트는 기본값을 더하지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exec {
    /// argv. 첫 항목에 `/`가 없으면 `env`의 `PATH`로 찾는다.
    pub args: Vec<String>,
    /// 환경 변수 전체. 순서대로 설정한다.
    pub env: Vec<(String, String)>,
    /// 작업 디렉터리 (절대 경로).
    pub cwd: String,
    /// 실행 사용자.
    pub user: User,
    /// 터미널을 붙인다. 그러면 stdout과 stderr가 하나로 합쳐져 [`GuestMsg::Stdout`]으로만 온다.
    pub tty: bool,
    /// 표준 입력을 연다. 아니면 `/dev/null`이다.
    pub stdin: bool,
    /// 터미널 크기. `tty`일 때만 쓴다.
    pub size: Option<WinSize>,
    /// 출력을 언제까지 보내는지.
    ///
    /// - `false`: 출력 파이프가 닫힐 때까지 보낸다. 프로세스가 끝나도 남은 자식이 파이프를 쥐고 있으면 기다린다
    ///   (`cmd | cat`, `docker exec`와 같다).
    /// - `true`: 프로세스가 끝나면 이미 쓴 출력만 보내고 [`GuestMsg::Exited`]를 보낸다. 남은 자식은 기다리지 않는다.
    ///   샌드박스의 기본 프로세스에 쓴다. 기본 프로세스가 끝나면 샌드박스가 멈추기 때문이다
    ///   (Docker에서 컨테이너의 첫 프로세스가 끝나면 모두 끝나는 것과 같다).
    pub stop_output_at_exit: bool,
}

/// 터미널 크기.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    /// 줄 수.
    pub rows: u16,
    /// 열 수.
    pub cols: u16,
}

/// 파일 복사 항목의 종류.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileKind {
    /// 일반 파일. 이어서 [`Data`](HostMsg::Data)가 합쳐서 정확히 `size` 바이트 온다.
    File {
        /// 크기.
        size: u64,
    },
    /// 디렉터리.
    Dir,
    /// 심볼릭 링크.
    Symlink {
        /// 링크 대상 (해석하지 않고 그대로 둔다).
        target: String,
    },
}

/// 파일 복사 항목 하나. 디렉터리는 그 안의 항목보다 먼저 온다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 상대 경로. [`check_relative_path`]를 통과해야 한다.
    pub path: String,
    /// 종류.
    pub kind: FileKind,
    /// 권한 비트 (`0o7777` 안).
    pub mode: u32,
    /// 소유자.
    pub uid: u32,
    /// 그룹.
    pub gid: u32,
    /// 수정 시각 (Unix 초).
    pub mtime: i64,
}

/// 프로세스 종료 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitStatus {
    /// 종료 코드.
    Code(i32),
    /// 시그널로 끝남.
    Signal(i32),
}

/// 오류 종류. 호스트는 이것으로 사용자에게 보여줄 결과를 정한다(예: `exec`의 실행 파일이 없으면 127).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// 프로토콜 버전이 다름.
    Version,
    /// 메시지 순서나 내용이 잘못됨.
    Protocol,
    /// 경로나 실행 파일이 없음.
    NotFound,
    /// 권한 없음.
    PermissionDenied,
    /// 요청 값이 잘못됨 (예: 사용자 이름을 찾지 못함, 상대 경로가 아님).
    InvalidArgument,
    /// 그 밖의 입출력 오류.
    Io,
}

/// 오류.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// 종류.
    pub kind: ErrorKind,
    /// 사람이 읽을 설명.
    pub message: String,
}

/// 호스트 → 게스트.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMsg {
    /// 연결의 첫 메시지.
    Hello {
        /// [`PROTOCOL_VERSION`](crate::PROTOCOL_VERSION).
        version: u32,
    },
    /// 프로세스를 실행한다.
    ///
    /// 흐름: 게스트가 [`GuestMsg::Started`] → 호스트는 [`Stdin`](HostMsg::Stdin), [`StdinClose`](HostMsg::StdinClose),
    /// [`Signal`](HostMsg::Signal), [`Resize`](HostMsg::Resize)를 보내고 게스트는 [`GuestMsg::Stdout`],
    /// [`GuestMsg::Stderr`]를 보낸다 → 프로세스가 끝나고 출력을 모두 보내면 게스트가 [`GuestMsg::Exited`]를 보내고 닫는다.
    /// 시작하지 못하면 [`GuestMsg::Started`] 대신 [`GuestMsg::Error`]가 온다.
    /// 호스트가 연결을 닫으면 게스트는 프로세스를 SIGKILL로 끝낸다.
    Exec(Exec),
    /// 표준 입력.
    Stdin(Vec<u8>),
    /// 표준 입력 닫기.
    StdinClose,
    /// 시그널 보내기.
    Signal(i32),
    /// 터미널 크기 바꾸기.
    Resize(WinSize),
    /// 게스트의 디렉터리 `dest` 아래에 파일을 놓는다. `dest`는 이미 있어야 한다.
    ///
    /// 흐름: 호스트가 [`Entry`](HostMsg::Entry)와 [`Data`](HostMsg::Data)를 보내고 [`End`](HostMsg::End)로 끝낸다
    /// → 게스트가 [`GuestMsg::Done`]. 같은 경로가 있으면 덮어쓴다(디렉터리는 합친다).
    CopyIn {
        /// 절대 경로.
        dest: String,
    },
    /// 게스트의 `path`(파일이나 디렉터리)를 보내 달라.
    ///
    /// 흐름: 게스트가 [`GuestMsg::Entry`]와 [`GuestMsg::Data`]를 보내고 [`GuestMsg::End`]로 끝낸다.
    /// 첫 항목의 경로는 `path`의 마지막 이름이다. 심볼릭 링크는 따라가지 않는다.
    CopyOut {
        /// 절대 경로.
        path: String,
    },
    /// 복사 항목.
    Entry(Entry),
    /// 파일 내용 조각.
    Data(Vec<u8>),
    /// 복사 끝.
    End,
    /// 파일 시스템을 동기화하고 VM을 끈다. 게스트가 [`GuestMsg::Done`]을 보낸 뒤 끈다.
    Shutdown,
    /// 경로 정보. 심볼릭 링크는 따라간다(`stat(2)`). 흐름: 게스트가 [`GuestMsg::Stat`].
    Stat {
        /// 절대 경로.
        path: String,
    },
    /// 실행기가 붙인 공유 폴더(virtio-fs, 태그 `tag`)를 `target`에 마운트한다. `target`이 없으면 만든다.
    /// 흐름: 게스트가 [`GuestMsg::Done`].
    Share {
        /// 실행기에 준 태그.
        tag: String,
        /// 게스트 절대 경로.
        target: String,
        /// 읽기 전용으로 마운트한다.
        readonly: bool,
    },
    /// 게스트 안의 나가는 연결 프록시를 `127.0.0.1`의 빈 포트에서 듣게 한다(이미 듣고 있으면 그대로).
    /// 흐름: 게스트가 [`GuestMsg::Listening`].
    EgressListen,
    /// 이 연결을 나가는 연결 하나에 쓰라고 맡긴다. 게스트는 프록시 클라이언트가 올 때까지 쥐고 있다가
    /// [`GuestMsg::Connect`]를 보낸다. 호스트는 [`HostMsg::Connected`]나 [`HostMsg::Refused`]로 답하고,
    /// `Connected` 뒤로 이 연결은 프레임 없는 바이트 스트림이 된다(양쪽이 그대로 이어 붙인다).
    EgressSlot,
    /// [`GuestMsg::Connect`]의 답: 연결했다.
    Connected,
    /// [`GuestMsg::Connect`]의 답: 허용하지 않거나 연결하지 못했다. 연결의 마지막 메시지.
    Refused {
        /// 이유.
        reason: String,
    },
}

/// 게스트 → 호스트.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestMsg {
    /// 버전이 같다.
    HelloAck {
        /// 게스트의 버전.
        version: u32,
    },
    /// 프로세스가 시작됨.
    Started {
        /// 게스트 안의 PID.
        pid: u32,
    },
    /// 표준 출력 (tty면 터미널 출력 전체).
    Stdout(Vec<u8>),
    /// 표준 오류.
    Stderr(Vec<u8>),
    /// 프로세스가 끝남. 연결의 마지막 메시지.
    Exited(ExitStatus),
    /// 복사 항목.
    Entry(Entry),
    /// 파일 내용 조각.
    Data(Vec<u8>),
    /// 복사 끝.
    End,
    /// 요청을 마침.
    Done,
    /// 실패. 연결의 마지막 메시지.
    Error(Error),
    /// [`HostMsg::Stat`]의 답. 없으면 `None`. 있으면 `path`가 마지막 이름인 항목이다.
    Stat(Option<Entry>),
    /// [`HostMsg::EgressListen`]의 답: 프록시가 듣는 포트.
    Listening {
        /// `127.0.0.1`의 포트.
        port: u16,
    },
    /// [`HostMsg::EgressSlot`]으로 맡긴 연결로 이 목적지에 연결해 달라는 요청.
    Connect {
        /// 프록시 클라이언트가 요청한 호스트(이름 또는 IP).
        host: String,
        /// 포트.
        port: u16,
    },
}

/// 복사 항목의 경로가 안전한 상대 경로인지 본다.
///
/// 비어 있지 않고, `/`로 시작하지 않고, 구성 요소가 비거나 `.`이나 `..`이 아니고, NUL이 없어야 한다.
/// 받는 쪽은 이것을 통과한 경로만 대상 디렉터리에 붙인다. 그래서 대상 밖으로 나갈 수 없다.
/// (대상 안의 심볼릭 링크를 따라 나가는 것은 받는 쪽이 링크를 따라가지 않고 만들어 막는다.)
pub fn check_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() {
        return Err("empty path".into());
    }
    if path.starts_with('/') {
        return Err(format!("absolute path: {path:?}"));
    }
    if path.contains('\0') {
        return Err(format!("NUL in path: {path:?}"));
    }
    for c in path.split('/') {
        if c.is_empty() || c == "." || c == ".." {
            return Err(format!("invalid component in {path:?}"));
        }
    }
    Ok(())
}

// --- 인코딩 ---

mod tag {
    pub const HELLO: u8 = 0x01;
    pub const EXEC: u8 = 0x02;
    pub const STDIN: u8 = 0x03;
    pub const STDIN_CLOSE: u8 = 0x04;
    pub const SIGNAL: u8 = 0x05;
    pub const RESIZE: u8 = 0x06;
    pub const COPY_IN: u8 = 0x07;
    pub const COPY_OUT: u8 = 0x08;
    pub const ENTRY: u8 = 0x09;
    pub const DATA: u8 = 0x0A;
    pub const END: u8 = 0x0B;
    pub const SHUTDOWN: u8 = 0x0C;
    pub const STAT: u8 = 0x0D;
    pub const SHARE: u8 = 0x0E;
    pub const EGRESS_LISTEN: u8 = 0x0F;
    pub const EGRESS_SLOT: u8 = 0x10;
    pub const CONNECTED: u8 = 0x11;
    pub const REFUSED: u8 = 0x12;

    pub const HELLO_ACK: u8 = 0x81;
    pub const STARTED: u8 = 0x82;
    pub const STDOUT: u8 = 0x83;
    pub const STDERR: u8 = 0x84;
    pub const EXITED: u8 = 0x85;
    pub const G_ENTRY: u8 = 0x86;
    pub const G_DATA: u8 = 0x87;
    pub const G_END: u8 = 0x88;
    pub const DONE: u8 = 0x89;
    pub const ERROR: u8 = 0x8A;
    pub const G_STAT: u8 = 0x8B;
    pub const LISTENING: u8 = 0x8C;
    pub const CONNECT: u8 = 0x8D;
}

fn bad<T>(m: impl Into<String>) -> DecodeResult<T> {
    Err(DecodeError(m.into()))
}

fn enc_ident(e: &mut Encoder, v: &Ident) {
    match v {
        Ident::Id(n) => e.u8(0).u32(*n),
        Ident::Name(s) => e.u8(1).str(s),
    };
}

fn dec_ident(d: &mut Decoder<'_>) -> DecodeResult<Ident> {
    match d.u8()? {
        0 => Ok(Ident::Id(d.u32()?)),
        1 => Ok(Ident::Name(d.string()?)),
        v => bad(format!("ident kind {v}")),
    }
}

fn enc_size(e: &mut Encoder, s: &WinSize) {
    e.u16(s.rows).u16(s.cols);
}

fn dec_size(d: &mut Decoder<'_>) -> DecodeResult<WinSize> {
    Ok(WinSize {
        rows: d.u16()?,
        cols: d.u16()?,
    })
}

fn enc_entry(e: &mut Encoder, v: &Entry) {
    e.str(&v.path);
    match &v.kind {
        FileKind::File { size } => e.u8(0).u64(*size),
        FileKind::Dir => e.u8(1),
        FileKind::Symlink { target } => e.u8(2).str(target),
    };
    e.u32(v.mode).u32(v.uid).u32(v.gid).i64(v.mtime);
}

fn dec_entry(d: &mut Decoder<'_>) -> DecodeResult<Entry> {
    let path = d.string()?;
    let kind = match d.u8()? {
        0 => FileKind::File { size: d.u64()? },
        1 => FileKind::Dir,
        2 => FileKind::Symlink {
            target: d.string()?,
        },
        v => return bad(format!("file kind {v}")),
    };
    let mode = d.u32()?;
    if mode & !0o7777 != 0 {
        return bad(format!("mode {mode:o} has bits outside 0o7777"));
    }
    Ok(Entry {
        path,
        kind,
        mode,
        uid: d.u32()?,
        gid: d.u32()?,
        mtime: d.i64()?,
    })
}

impl ErrorKind {
    fn code(self) -> u8 {
        match self {
            ErrorKind::Version => 0,
            ErrorKind::Protocol => 1,
            ErrorKind::NotFound => 2,
            ErrorKind::PermissionDenied => 3,
            ErrorKind::InvalidArgument => 4,
            ErrorKind::Io => 5,
        }
    }

    fn from_code(v: u8) -> DecodeResult<Self> {
        Ok(match v {
            0 => ErrorKind::Version,
            1 => ErrorKind::Protocol,
            2 => ErrorKind::NotFound,
            3 => ErrorKind::PermissionDenied,
            4 => ErrorKind::InvalidArgument,
            5 => ErrorKind::Io,
            v => return bad(format!("error kind {v}")),
        })
    }

    /// 입출력 오류 종류에서.
    pub fn from_io(kind: io::ErrorKind) -> Self {
        match kind {
            io::ErrorKind::NotFound => ErrorKind::NotFound,
            io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidInput => ErrorKind::InvalidArgument,
            _ => ErrorKind::Io,
        }
    }
}

impl Error {
    /// 만든다.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Error {}

impl HostMsg {
    /// 프레임 본문으로.
    pub fn encode(&self) -> Vec<u8> {
        let mut e;
        match self {
            HostMsg::Hello { version } => {
                e = Encoder::new(tag::HELLO);
                e.u32(*version);
            }
            HostMsg::Exec(x) => {
                e = Encoder::new(tag::EXEC);
                e.list(&x.args, |e, a| {
                    e.str(a);
                })
                .list(&x.env, |e, (k, v)| {
                    e.str(k).str(v);
                })
                .str(&x.cwd);
                enc_ident(&mut e, &x.user.user);
                e.opt(x.user.group.as_ref(), enc_ident)
                    .bool(x.tty)
                    .bool(x.stdin)
                    .opt(x.size.as_ref(), enc_size)
                    .bool(x.stop_output_at_exit);
            }
            HostMsg::Stdin(b) => {
                e = Encoder::new(tag::STDIN);
                e.bytes(b);
            }
            HostMsg::StdinClose => e = Encoder::new(tag::STDIN_CLOSE),
            HostMsg::Signal(n) => {
                e = Encoder::new(tag::SIGNAL);
                e.i32(*n);
            }
            HostMsg::Resize(s) => {
                e = Encoder::new(tag::RESIZE);
                enc_size(&mut e, s);
            }
            HostMsg::CopyIn { dest } => {
                e = Encoder::new(tag::COPY_IN);
                e.str(dest);
            }
            HostMsg::CopyOut { path } => {
                e = Encoder::new(tag::COPY_OUT);
                e.str(path);
            }
            HostMsg::Entry(x) => {
                e = Encoder::new(tag::ENTRY);
                enc_entry(&mut e, x);
            }
            HostMsg::Data(b) => {
                e = Encoder::new(tag::DATA);
                e.bytes(b);
            }
            HostMsg::End => e = Encoder::new(tag::END),
            HostMsg::Shutdown => e = Encoder::new(tag::SHUTDOWN),
            HostMsg::Stat { path } => {
                e = Encoder::new(tag::STAT);
                e.str(path);
            }
            HostMsg::Share {
                tag,
                target,
                readonly,
            } => {
                e = Encoder::new(tag::SHARE);
                e.str(tag).str(target).bool(*readonly);
            }
            HostMsg::EgressListen => e = Encoder::new(tag::EGRESS_LISTEN),
            HostMsg::EgressSlot => e = Encoder::new(tag::EGRESS_SLOT),
            HostMsg::Connected => e = Encoder::new(tag::CONNECTED),
            HostMsg::Refused { reason } => {
                e = Encoder::new(tag::REFUSED);
                e.str(reason);
            }
        }
        e.finish()
    }

    /// 프레임 본문에서.
    pub fn decode(body: &[u8]) -> DecodeResult<Self> {
        let mut d = Decoder::new(body);
        let msg = match d.u8()? {
            tag::HELLO => HostMsg::Hello { version: d.u32()? },
            tag::EXEC => {
                let args = d.list(|d| d.string())?;
                let env = d.list(|d| Ok((d.string()?, d.string()?)))?;
                let cwd = d.string()?;
                let user = User {
                    user: dec_ident(&mut d)?,
                    group: d.opt(dec_ident)?,
                };
                HostMsg::Exec(Exec {
                    args,
                    env,
                    cwd,
                    user,
                    tty: d.bool()?,
                    stdin: d.bool()?,
                    size: d.opt(dec_size)?,
                    stop_output_at_exit: d.bool()?,
                })
            }
            tag::STDIN => HostMsg::Stdin(d.bytes()?.to_vec()),
            tag::STDIN_CLOSE => HostMsg::StdinClose,
            tag::SIGNAL => HostMsg::Signal(d.i32()?),
            tag::RESIZE => HostMsg::Resize(dec_size(&mut d)?),
            tag::COPY_IN => HostMsg::CopyIn { dest: d.string()? },
            tag::COPY_OUT => HostMsg::CopyOut { path: d.string()? },
            tag::ENTRY => HostMsg::Entry(dec_entry(&mut d)?),
            tag::DATA => HostMsg::Data(d.bytes()?.to_vec()),
            tag::END => HostMsg::End,
            tag::SHUTDOWN => HostMsg::Shutdown,
            tag::STAT => HostMsg::Stat { path: d.string()? },
            tag::SHARE => HostMsg::Share {
                tag: d.string()?,
                target: d.string()?,
                readonly: d.bool()?,
            },
            tag::EGRESS_LISTEN => HostMsg::EgressListen,
            tag::EGRESS_SLOT => HostMsg::EgressSlot,
            tag::CONNECTED => HostMsg::Connected,
            tag::REFUSED => HostMsg::Refused {
                reason: d.string()?,
            },
            t => return bad(format!("unknown host message {t:#04x}")),
        };
        d.end()?;
        Ok(msg)
    }

    /// 프레임 하나로 쓴다.
    pub fn write_to<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        write_frame(w, &self.encode())
    }

    /// 프레임 하나를 읽어 해석한다.
    pub fn read_from<R: Read + ?Sized>(r: &mut R) -> io::Result<Self> {
        Ok(Self::decode(&read_frame(r)?)?)
    }
}

impl GuestMsg {
    /// 프레임 본문으로.
    pub fn encode(&self) -> Vec<u8> {
        let mut e;
        match self {
            GuestMsg::HelloAck { version } => {
                e = Encoder::new(tag::HELLO_ACK);
                e.u32(*version);
            }
            GuestMsg::Started { pid } => {
                e = Encoder::new(tag::STARTED);
                e.u32(*pid);
            }
            GuestMsg::Stdout(b) => {
                e = Encoder::new(tag::STDOUT);
                e.bytes(b);
            }
            GuestMsg::Stderr(b) => {
                e = Encoder::new(tag::STDERR);
                e.bytes(b);
            }
            GuestMsg::Exited(s) => {
                e = Encoder::new(tag::EXITED);
                match s {
                    ExitStatus::Code(c) => e.u8(0).i32(*c),
                    ExitStatus::Signal(n) => e.u8(1).i32(*n),
                };
            }
            GuestMsg::Entry(x) => {
                e = Encoder::new(tag::G_ENTRY);
                enc_entry(&mut e, x);
            }
            GuestMsg::Data(b) => {
                e = Encoder::new(tag::G_DATA);
                e.bytes(b);
            }
            GuestMsg::End => e = Encoder::new(tag::G_END),
            GuestMsg::Done => e = Encoder::new(tag::DONE),
            GuestMsg::Error(x) => {
                e = Encoder::new(tag::ERROR);
                e.u8(x.kind.code()).str(&x.message);
            }
            GuestMsg::Stat(x) => {
                e = Encoder::new(tag::G_STAT);
                e.opt(x.as_ref(), enc_entry);
            }
            GuestMsg::Listening { port } => {
                e = Encoder::new(tag::LISTENING);
                e.u16(*port);
            }
            GuestMsg::Connect { host, port } => {
                e = Encoder::new(tag::CONNECT);
                e.str(host).u16(*port);
            }
        }
        e.finish()
    }

    /// 프레임 본문에서.
    pub fn decode(body: &[u8]) -> DecodeResult<Self> {
        let mut d = Decoder::new(body);
        let msg = match d.u8()? {
            tag::HELLO_ACK => GuestMsg::HelloAck { version: d.u32()? },
            tag::STARTED => GuestMsg::Started { pid: d.u32()? },
            tag::STDOUT => GuestMsg::Stdout(d.bytes()?.to_vec()),
            tag::STDERR => GuestMsg::Stderr(d.bytes()?.to_vec()),
            tag::EXITED => GuestMsg::Exited(match d.u8()? {
                0 => ExitStatus::Code(d.i32()?),
                1 => ExitStatus::Signal(d.i32()?),
                v => return bad(format!("exit kind {v}")),
            }),
            tag::G_ENTRY => GuestMsg::Entry(dec_entry(&mut d)?),
            tag::G_DATA => GuestMsg::Data(d.bytes()?.to_vec()),
            tag::G_END => GuestMsg::End,
            tag::DONE => GuestMsg::Done,
            tag::ERROR => GuestMsg::Error(Error {
                kind: ErrorKind::from_code(d.u8()?)?,
                message: d.string()?,
            }),
            tag::G_STAT => GuestMsg::Stat(d.opt(dec_entry)?),
            tag::LISTENING => GuestMsg::Listening { port: d.u16()? },
            tag::CONNECT => GuestMsg::Connect {
                host: d.string()?,
                port: d.u16()?,
            },
            t => return bad(format!("unknown guest message {t:#04x}")),
        };
        d.end()?;
        Ok(msg)
    }

    /// 프레임 하나로 쓴다.
    pub fn write_to<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        write_frame(w, &self.encode())
    }

    /// 프레임 하나를 읽어 해석한다.
    pub fn read_from<R: Read + ?Sized>(r: &mut R) -> io::Result<Self> {
        Ok(Self::decode(&read_frame(r)?)?)
    }
}

#[cfg(test)]
mod tests;
