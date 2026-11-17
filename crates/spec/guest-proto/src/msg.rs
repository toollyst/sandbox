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
    /// 샌드박스의 기본 프로세스인지. 두 가지가 달라진다.
    ///
    /// - 출력: `false`면 출력 파이프가 닫힐 때까지 보낸다. 프로세스가 끝나도 남은 자식이 파이프를 쥐고 있으면
    ///   기다린다(`cmd | cat`, `docker exec`와 같다). `true`면 프로세스가 끝나면 이미 쓴 출력만 보내고
    ///   [`GuestMsg::Exited`]를 보낸다. 남은 자식은 기다리지 않는다. 기본 프로세스가 끝나면 샌드박스가 멈추기
    ///   때문이다(Docker에서 컨테이너의 첫 프로세스가 끝나면 모두 끝나는 것과 같다).
    /// - [`HostMsg::SignalMain`]이 이 프로세스에 시그널을 보낸다. 나중에 시작한 기본 프로세스가 앞의 것을 대신한다.
    pub main: bool,
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

/// 빌드 층 항목의 종류. [`HostMsg::ExportLayer`]의 답에 쓴다.
///
/// overlay 위층의 내용을 OCI 층의 변경으로 옮긴 것이다. whiteout과 opaque는 `.wh.` 이름이 아니라 종류로 보낸다.
/// 그래서 받는 쪽은 사용자가 만든 `.wh.`로 시작하는 이름을 지우기 표시와 헷갈리지 않고 거부할 수 있다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayerKind {
    /// 일반 파일. 이어서 [`GuestMsg::Data`]가 합쳐서 정확히 `size` 바이트 온다.
    File {
        /// 크기.
        size: u64,
    },
    /// 디렉터리.
    Dir {
        /// 아래층의 내용을 가린다(overlay의 opaque 디렉터리, OCI의 `.wh..wh..opq`).
        opaque: bool,
    },
    /// 심볼릭 링크.
    Symlink {
        /// 링크 대상 (해석하지 않고 그대로 둔다).
        target: String,
    },
    /// 하드 링크. 대상은 같은 층에서 앞서 [`LayerKind::File`]로 보낸 항목의 경로다.
    Hardlink {
        /// 대상 경로 (층 안 상대 경로).
        target: String,
    },
    /// 문자 장치.
    CharDevice {
        /// major 번호.
        major: u32,
        /// minor 번호.
        minor: u32,
    },
    /// 블록 장치.
    BlockDevice {
        /// major 번호.
        major: u32,
        /// minor 번호.
        minor: u32,
    },
    /// FIFO.
    Fifo,
    /// 아래층의 이 경로와 그 아래 전체를 지운다(overlay의 whiteout, OCI의 `.wh.<이름>`).
    Whiteout,
}

/// 빌드 층 항목 하나. 디렉터리는 그 안의 항목보다 먼저 온다. 층의 맨 위(`/`)는 보내지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerEntry {
    /// 층 안의 상대 경로. [`check_relative_path`]를 통과해야 한다.
    pub path: String,
    /// 종류.
    pub kind: LayerKind,
    /// 권한 비트 (`0o7777` 안).
    pub mode: u32,
    /// 소유자.
    pub uid: u32,
    /// 그룹.
    pub gid: u32,
    /// 수정 시각 (Unix 초).
    pub mtime: i64,
    /// 확장 속성 (이름, 값). overlay 내부용(`trusted.overlay.*`)은 넣지 않는다.
    pub xattrs: Vec<(String, Vec<u8>)>,
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
    /// [`GuestMsg::Connect`]를 보낸다. 호스트는 [`HostMsg::Connected`]나 [`HostMsg::Refused`]로 답한다.
    ///
    /// `Connected` 뒤로 양쪽은 목적지와 주고받는 바이트를 `Data`([`HostMsg::Data`], [`GuestMsg::Data`])로 보내고,
    /// 보내기를 끝내면 `End`를 보낸다(TCP 반쯤 닫기). 양쪽 다 `End`를 보내면 연결을 닫는다.
    /// 끝을 전송의 반쯤 닫기에 맡기지 않는다. 반쯤 닫기가 없는 vsock 구현이 있기 때문이다
    /// (Firecracker는 한쪽이 EOF면 연결 전체를 닫는다).
    EgressSlot,
    /// [`GuestMsg::Connect`]의 답: 연결했다.
    Connected,
    /// [`GuestMsg::Connect`]의 답: 허용하지 않거나 연결하지 못했다. 연결의 마지막 메시지.
    /// ([`GuestMsg::Lookup`]의 답은 [`HostMsg::Resolved`]다.)
    Refused {
        /// 이유.
        reason: String,
    },
    /// 기본 프로세스([`Exec::main`])에 시그널을 보낸다. 그 프로세스의 연결이 없는 쪽(다른 호스트 프로세스)도 보낼 수 있다.
    /// 번호는 Linux 번호다. 흐름: 게스트가 [`GuestMsg::Done`]. 실행 중인 기본 프로세스가 없으면 `NotFound` 오류.
    SignalMain(i32),
    /// 프로세스를 떼어 놓고 실행한다(`sbx exec -d`). [`Exec`]의 `tty`, `stdin`, `main`은 거짓이어야 한다.
    /// 표준 입출력은 `/dev/null`이다.
    ///
    /// 흐름: 게스트가 [`GuestMsg::Started`]를 보내고 닫는다. 연결이 닫혀도 프로세스는 계속 돈다.
    /// 시작하지 못하면 [`GuestMsg::Error`]. 이 메시지를 모르는 게스트(0.2.0)는 `Protocol` 오류를 보낸다.
    ExecDetached(Exec),
    /// 자원 사용량을 묻는다(`sbx stats`). 흐름: 게스트가 [`GuestMsg::Usage`]. 모르는 게스트(0.2.0)는 답 없이 닫는다.
    Usage,
    /// 호스트에 온 TCP 연결 하나를 게스트 `127.0.0.1:port`로 잇는다(`-p`). 흐름: 게스트가 연결하면 [`GuestMsg::Done`],
    /// 못 하면 [`GuestMsg::Error`]. `Done` 뒤로는 나가는 연결과 같다: 호스트는 [`HostMsg::Data`]·[`HostMsg::End`],
    /// 게스트는 [`GuestMsg::Data`]·[`GuestMsg::End`]. 모르는 게스트(0.2.0)는 답 없이 닫는다.
    Forward {
        /// 게스트 포트.
        port: u16,
    },
    /// 빌드 층을 하나 새로 연다. 이미지 rootfs와 앞서 연 층들을 읽기 전용 아래층으로, 빈 쓰기 층을 위로 쌓는다.
    /// 그 뒤의 [`Exec`](HostMsg::Exec), [`ExecDetached`](HostMsg::ExecDetached), [`CopyIn`](HostMsg::CopyIn),
    /// [`CopyOut`](HostMsg::CopyOut), [`Stat`](HostMsg::Stat)은 이 층으로 본 파일 시스템에서 한다.
    /// 규칙(차례, 앞 층 내리기, 상한)은 `sbx-guest`의 `layer` 모듈에 있다.
    ///
    /// 흐름: 게스트가 [`GuestMsg::LayerOpened`]. 층을 쌓을 수 없는 게스트이거나 [`MAX_LAYERS`](crate::MAX_LAYERS)를
    /// 넘거나 앞 층을 쓰는 프로세스가 남아 있으면 [`GuestMsg::Error`].
    OpenLayer,
    /// 지금 열린 층의 변경을 보내 달라. 층은 열린 채로 둔다.
    ///
    /// 흐름: 게스트가 [`GuestMsg::LayerEntry`]와 (일반 파일 뒤에) [`GuestMsg::Data`]를 보내고 [`GuestMsg::End`]로
    /// 끝낸다. 열린 층이 없으면 [`GuestMsg::Error`].
    ExportLayer,
    /// 투명 TCP를 켠다(`--network allow`일 때 시작에서 한 번). 프록시를 모르는 프로그램도 허용된 `HOST:PORT`에
    /// 닿게 한다. 규칙은 `sbx-guest`의 `transparent` 모듈에 있다.
    ///
    /// 게스트는 가짜 주소 범위를 local로 잡고, 루프백 DNS가 이름을 [`GuestMsg::Lookup`]으로 호스트에 묻게 하고,
    /// `/etc/resolv.conf`가 루프백 DNS를 가리키게 쓰고(쓰기 층에), `literals`의 주소마다 듣는다.
    /// `literals`는 IP로 적은 규칙 가운데 게스트가 들을 수 있는 IPv4 주소와 포트다(어느 것인지는 호스트가 고른다).
    ///
    /// 흐름: 게스트가 [`GuestMsg::Done`]. 일부나 전부를 하지 못했으면 [`GuestMsg::Error`]이고, 한 것은 남는다
    /// (프록시는 어느 쪽이든 그대로다). 모르는 게스트는 `Protocol` 오류로 답한다.
    Transparent {
        /// 듣는 IP 규칙 주소.
        literals: Vec<std::net::SocketAddrV4>,
    },
    /// [`GuestMsg::Lookup`]의 답. 이름에 맞는 허용 규칙의 포트들(작은 것부터, 겹치지 않게)과, 포트가 `*`인 규칙이
    /// 맞았는지. 둘 다 비었으면 그 이름은 허용되지 않았다. 호스트는 이 답을 위해 이름을 풀지 않는다.
    /// 이 답 뒤로 그 연결은 다시 맡긴 연결이다(다음 `Lookup`이나 `Connect`에 쓴다).
    Resolved {
        /// 열린 포트들.
        ports: Vec<u16>,
        /// 포트가 `*`인 규칙이 맞았다(프록시로만 닿는다).
        any_port: bool,
    },
    /// 빌드 단계 하나를 실행한다(`RUN`). [`Exec`](HostMsg::Exec)와 같은 흐름이고 다른 점은 끝이다: 프로세스가 끝나면
    /// 게스트는 그 단계가 남긴 프로세스를 모두 끝내고(SIGKILL), 사라진 뒤에 남은 출력과 [`GuestMsg::Exited`]를 보낸다.
    /// 그래서 백그라운드로 띄운 프로세스가 출력을 쥐고 있어도 단계는 끝나고, 다음 [`OpenLayer`](HostMsg::OpenLayer)가
    /// 그 프로세스 때문에 실패하지 않는다(Docker의 `RUN`과 같다). 무엇을 끝내는지는 `sbx-guest`의 `exec` 모듈에 있다.
    ///
    /// 빌드 층이 열려 있어야 한다. 아니면(보통의 샌드박스) [`GuestMsg::Error`]다. [`Exec::main`]은 거짓이어야 한다.
    /// 모르는 게스트는 `Protocol` 오류로 답한다.
    ExecStep(Exec),
    /// 이미지의 프로그램 없이 기본 프로세스 자리를 채운다(빌드 VM, `FROM scratch`처럼 실행할 것이 없는 이미지).
    ///
    /// 흐름: 게스트가 [`GuestMsg::Started`]를 `pid` 0으로 보내고(프로세스가 없다) 기다린다.
    /// [`SignalMain`](HostMsg::SignalMain)이나 이 연결로 온 [`Signal`](HostMsg::Signal)을 받으면
    /// [`GuestMsg::Exited`]에 `ExitStatus::Signal`을 담아 보내고 닫는다. 호스트가 연결을 닫아도 끝난다. 모르는 게스트는 `Protocol` 오류로 답한다.
    Hold,
    /// 호스트가 붙인 층 디스크 수를 알린다. 게스트는 부팅 때 쌓은 층 디스크 수와 견준다. 같으면 [`GuestMsg::Done`]이다.
    /// 다르면 실행기가 디스크를 빠뜨렸거나 더 붙인 것이다. 게스트는 잘못된 뿌리 위에서 아무것도 하지 않도록
    /// [`GuestMsg::Error`]로 답한 뒤 VM을 끈다.
    ///
    /// 모르는 게스트는 `Protocol` 오류로 답한다(그런 게스트는 층 디스크를 하나만 쓴다. 호스트는 하나일 때만 받아들인다).
    CheckLayers {
        /// 호스트가 실행기에 준 층 디스크 수(scratch 빼고).
        count: u32,
    },
    /// 게스트 시계를 호스트에 맞춘다. 연결을 연 소유 프로세스가 시작 직후, 주기적으로, 호스트 시계가 뛴 뒤 보낸다.
    /// 규칙(재기, 바로 맞추기와 천천히 맞추기의 경계)은 `sbx-guest`의 `clock` 모듈과 `sbx-runtime`의 `clock` 모듈에 있다.
    ///
    /// 흐름: 게스트가 [`GuestMsg::ClockAsk`]를 보내면 호스트가 곧바로 [`HostMsg::ClockNow`]로 답한다. 이것을 많아야
    /// [`CLOCK_ROUNDS`](crate::CLOCK_ROUNDS)번 한다. 게스트는 시계를 맞추고 [`GuestMsg::Done`], 맞추지 못하거나
    /// 재지 못하면 [`GuestMsg::Error`]. 시계를 맞출 수 없는 게스트는 `InvalidArgument` 오류다.
    /// 모르는 게스트는 답 없이 닫거나 `Protocol` 오류로 답한다. 호스트는 그 VM에서 맞추기를 그만둔다.
    ClockSync,
    /// 이름 붙은 볼륨 디스크 하나를 `target`에 마운트한다. 호스트는 시작할 때, 기본 프로세스보다 먼저, 볼륨마다
    /// 하나씩 보낸다. 규칙(포맷, 처음 채우기, 층과 가르기)은 `sbx-guest`의 `volume` 모듈에 있다.
    ///
    /// - `disk`는 실행기에 준 디스크 차례(0부터)다. 층 디스크 뒤여야 한다. 층 디스크나 scratch를 가리키면 오류다.
    /// - 쓰기로 붙인 디스크에 파일 시스템이 없으면(처음 쓰는 볼륨, 0으로 채운 새 파일) 게스트가 포맷한다.
    ///   읽기 전용인데 파일 시스템이 없으면 오류다.
    /// - `copy`이고 볼륨이 비어 있으면, 마운트하기 전의 `target` 내용(이미지에 있던 것)을 소유자와 권한째 볼륨에
    ///   먼저 채운다(Docker와 같다). 읽기 전용이면 채우지 않는다.
    /// - `target`이 없으면 만든다(쓰기 층에).
    ///
    /// 흐름: 게스트가 [`GuestMsg::Done`]. 실패하면 [`GuestMsg::Error`]. 모르는 게스트는 `Protocol` 오류로 답한다
    /// (볼륨은 시작할 때만 보내므로 언제나 같은 판의 게스트가 받는다).
    Volume {
        /// 디스크 차례(0부터).
        disk: u32,
        /// 게스트 절대 경로.
        target: String,
        /// 읽기 전용으로 마운트한다(실행기도 읽기 전용으로 붙였다).
        readonly: bool,
        /// 빈 볼륨을 이미지의 `target` 내용으로 채운다.
        copy: bool,
    },
    /// [`GuestMsg::ClockAsk`]의 답: 호스트 벽시계(Unix 시각, 나노초, 1970년 앞이면 음수). 보내기 바로 전에 읽는다.
    ClockNow {
        /// Unix 시각(나노초).
        unix_nanos: i64,
    },
    /// 호스트에서 바뀐 공유 폴더 항목을 게스트의 파일 감시(inotify)에 알린다. virtio-fs는 호스트 쪽 변경을 게스트
    /// inotify로 전하지 않는다. 그래서 VM을 소유한 프로세스가 호스트에서 공유 폴더를 감시하고 바뀐 경로를 보낸다.
    /// 규칙은 `sbx-guest`의 `changes` 모듈(게스트가 하는 일)과 `sbx-runtime`의 `watch` 모듈(언제 무엇을 보내나)에 있다.
    ///
    /// - `target`은 [`HostMsg::Share`]로 마운트한 게스트 절대 경로, `paths`는 그 아래의 상대 경로다. 빈 문자열은 `target`
    ///   자신이다. 구성 요소가 `..`, `.`, 빈 것이거나 NUL이 있거나 `/`로 시작하는 경로는 오류다.
    ///   많아야 [`MAX_SHARE_CHANGES`](crate::MAX_SHARE_CHANGES)개다.
    /// - 게스트는 경로마다, 있으면 그 항목의 수정 시각을 지금 값 그대로 다시 쓴다(`IN_MODIFY`가 생긴다. 내용과 시각은
    ///   그대로다). 없으면(지워졌거나 옮겨졌다) `target` 안에서 가장 가까운 있는 조상에 같은 일을 한다.
    ///
    /// 흐름: 게스트가 [`GuestMsg::Done`]. 경로가 규칙에 맞지 않으면 [`GuestMsg::Error`](`InvalidArgument`). 항목 하나를
    /// 알리지 못한 것(읽기 전용 공유 등)은 오류가 아니다. 모르는 게스트는 `Protocol` 오류로 답한다(공유는 시작할 때만
    /// 붙이므로 같은 판의 게스트가 받는다).
    ShareChanged {
        /// 공유를 마운트한 게스트 절대 경로.
        target: String,
        /// `target` 아래의 상대 경로들.
        paths: Vec<String>,
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
    /// [`HostMsg::Usage`]의 답.
    Usage(Usage),
    /// [`HostMsg::OpenLayer`]의 답: 연 층의 번호(1부터).
    LayerOpened {
        /// 층 번호.
        layer: u32,
    },
    /// [`HostMsg::ExportLayer`]의 항목.
    LayerEntry(LayerEntry),
    /// [`HostMsg::EgressSlot`]으로 맡긴 연결로 이름 하나가 허용 목록에 있는지 묻는다(투명 TCP의 DNS).
    /// 호스트는 [`HostMsg::Resolved`]로 답하고 연결을 맡긴 채로 둔다. 호스트는 `name`을 믿지 않는다.
    Lookup {
        /// 프로그램이 물은 이름.
        name: String,
    },
    /// [`HostMsg::ClockSync`] 안에서 호스트 시각을 묻는다. 호스트는 [`HostMsg::ClockNow`]로 답한다.
    ClockAsk,
}

/// 게스트 전체의 자원 사용량. 게스트가 보고하는 값이라 호스트는 표시에만 쓴다(믿고 결정하지 않는다).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// 켜진 CPU 수.
    pub cpus: u32,
    /// 부팅 뒤 모든 CPU가 일한 시간(USER_HZ 틱, `/proc/stat`의 idle·iowait 밖).
    pub cpu_busy: u64,
    /// 부팅 뒤 모든 CPU의 전체 시간(같은 단위).
    pub cpu_total: u64,
    /// 메모리 전체(바이트, `MemTotal`).
    pub memory_total: u64,
    /// 쓰는 메모리(바이트, `MemTotal - MemAvailable`).
    pub memory_used: u64,
    /// 쓰기 디스크 전체(바이트, `/`의 statvfs).
    pub disk_total: u64,
    /// 쓰기 디스크에서 쓴 것(바이트).
    pub disk_used: u64,
    /// 프로세스 수.
    pub pids: u32,
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
    pub const SIGNAL_MAIN: u8 = 0x13;
    pub const EXEC_DETACHED: u8 = 0x14;
    pub const USAGE: u8 = 0x15;
    pub const FORWARD: u8 = 0x16;
    pub const OPEN_LAYER: u8 = 0x17;
    pub const EXPORT_LAYER: u8 = 0x18;
    pub const TRANSPARENT: u8 = 0x19;
    pub const RESOLVED: u8 = 0x1A;
    pub const EXEC_STEP: u8 = 0x1B;
    pub const HOLD: u8 = 0x1C;
    pub const CHECK_LAYERS: u8 = 0x1D;
    // 0x20부터: 시계. 앞의 빈 번호는 다른 기능이 더할 자리로 둔다.
    pub const CLOCK_SYNC: u8 = 0x20;
    pub const CLOCK_NOW: u8 = 0x21;
    // 0x28부터: 볼륨.
    pub const VOLUME: u8 = 0x28;
    // 0x30부터: 공유 폴더.
    pub const SHARE_CHANGED: u8 = 0x30;

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
    pub const G_USAGE: u8 = 0x8E;
    pub const LAYER_OPENED: u8 = 0x8F;
    pub const LAYER_ENTRY: u8 = 0x90;
    pub const LOOKUP: u8 = 0x91;
    // 0xA0부터: 시계.
    pub const CLOCK_ASK: u8 = 0xA0;
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

fn enc_layer_entry(e: &mut Encoder, v: &LayerEntry) {
    e.str(&v.path);
    match &v.kind {
        LayerKind::File { size } => e.u8(0).u64(*size),
        LayerKind::Dir { opaque } => e.u8(1).bool(*opaque),
        LayerKind::Symlink { target } => e.u8(2).str(target),
        LayerKind::Hardlink { target } => e.u8(3).str(target),
        LayerKind::CharDevice { major, minor } => e.u8(4).u32(*major).u32(*minor),
        LayerKind::BlockDevice { major, minor } => e.u8(5).u32(*major).u32(*minor),
        LayerKind::Fifo => e.u8(6),
        LayerKind::Whiteout => e.u8(7),
    };
    e.u32(v.mode).u32(v.uid).u32(v.gid).i64(v.mtime);
    e.list(&v.xattrs, |e, (k, val)| {
        e.str(k).bytes(val);
    });
}

fn dec_layer_entry(d: &mut Decoder<'_>) -> DecodeResult<LayerEntry> {
    let path = d.string()?;
    let kind = match d.u8()? {
        0 => LayerKind::File { size: d.u64()? },
        1 => LayerKind::Dir { opaque: d.bool()? },
        2 => LayerKind::Symlink {
            target: d.string()?,
        },
        3 => LayerKind::Hardlink {
            target: d.string()?,
        },
        4 => LayerKind::CharDevice {
            major: d.u32()?,
            minor: d.u32()?,
        },
        5 => LayerKind::BlockDevice {
            major: d.u32()?,
            minor: d.u32()?,
        },
        6 => LayerKind::Fifo,
        7 => LayerKind::Whiteout,
        v => return bad(format!("layer entry kind {v}")),
    };
    let mode = d.u32()?;
    if mode & !0o7777 != 0 {
        return bad(format!("mode {mode:o} has bits outside 0o7777"));
    }
    Ok(LayerEntry {
        path,
        kind,
        mode,
        uid: d.u32()?,
        gid: d.u32()?,
        mtime: d.i64()?,
        xattrs: d.list(|d| Ok((d.string()?, d.bytes()?.to_vec())))?,
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
            HostMsg::Exec(x) | HostMsg::ExecDetached(x) | HostMsg::ExecStep(x) => {
                e = Encoder::new(match self {
                    HostMsg::Exec(_) => tag::EXEC,
                    HostMsg::ExecDetached(_) => tag::EXEC_DETACHED,
                    _ => tag::EXEC_STEP,
                });
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
                    .bool(x.main);
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
            HostMsg::SignalMain(n) => {
                e = Encoder::new(tag::SIGNAL_MAIN);
                e.i32(*n);
            }
            HostMsg::Usage => e = Encoder::new(tag::USAGE),
            HostMsg::Forward { port } => {
                e = Encoder::new(tag::FORWARD);
                e.u16(*port);
            }
            HostMsg::OpenLayer => e = Encoder::new(tag::OPEN_LAYER),
            HostMsg::ExportLayer => e = Encoder::new(tag::EXPORT_LAYER),
            HostMsg::Transparent { literals } => {
                e = Encoder::new(tag::TRANSPARENT);
                e.list(literals, |e, a| {
                    e.u32(u32::from(*a.ip())).u16(a.port());
                });
            }
            HostMsg::Resolved { ports, any_port } => {
                e = Encoder::new(tag::RESOLVED);
                e.list(ports, |e, p| {
                    e.u16(*p);
                })
                .bool(*any_port);
            }
            HostMsg::Hold => e = Encoder::new(tag::HOLD),
            HostMsg::CheckLayers { count } => {
                e = Encoder::new(tag::CHECK_LAYERS);
                e.u32(*count);
            }
            HostMsg::ClockSync => e = Encoder::new(tag::CLOCK_SYNC),
            HostMsg::ClockNow { unix_nanos } => {
                e = Encoder::new(tag::CLOCK_NOW);
                e.i64(*unix_nanos);
            }
            HostMsg::Volume {
                disk,
                target,
                readonly,
                copy,
            } => {
                e = Encoder::new(tag::VOLUME);
                e.u32(*disk).str(target).bool(*readonly).bool(*copy);
            }
            HostMsg::ShareChanged { target, paths } => {
                e = Encoder::new(tag::SHARE_CHANGED);
                e.str(target).list(paths, |e, p| {
                    e.str(p);
                });
            }
        }
        e.finish()
    }

    /// 프레임 본문에서.
    pub fn decode(body: &[u8]) -> DecodeResult<Self> {
        let mut d = Decoder::new(body);
        let msg = match d.u8()? {
            tag::HELLO => HostMsg::Hello { version: d.u32()? },
            t @ (tag::EXEC | tag::EXEC_DETACHED | tag::EXEC_STEP) => {
                let args = d.list(|d| d.string())?;
                let env = d.list(|d| Ok((d.string()?, d.string()?)))?;
                let cwd = d.string()?;
                let user = User {
                    user: dec_ident(&mut d)?,
                    group: d.opt(dec_ident)?,
                };
                let x = Exec {
                    args,
                    env,
                    cwd,
                    user,
                    tty: d.bool()?,
                    stdin: d.bool()?,
                    size: d.opt(dec_size)?,
                    main: d.bool()?,
                };
                match t {
                    tag::EXEC => HostMsg::Exec(x),
                    tag::EXEC_DETACHED => HostMsg::ExecDetached(x),
                    _ => HostMsg::ExecStep(x),
                }
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
            tag::SIGNAL_MAIN => HostMsg::SignalMain(d.i32()?),
            tag::USAGE => HostMsg::Usage,
            tag::FORWARD => HostMsg::Forward { port: d.u16()? },
            tag::OPEN_LAYER => HostMsg::OpenLayer,
            tag::EXPORT_LAYER => HostMsg::ExportLayer,
            tag::TRANSPARENT => HostMsg::Transparent {
                literals: d.list(|d| {
                    let ip = std::net::Ipv4Addr::from(d.u32()?);
                    Ok(std::net::SocketAddrV4::new(ip, d.u16()?))
                })?,
            },
            tag::RESOLVED => HostMsg::Resolved {
                ports: d.list(|d| d.u16())?,
                any_port: d.bool()?,
            },
            tag::HOLD => HostMsg::Hold,
            tag::CHECK_LAYERS => HostMsg::CheckLayers { count: d.u32()? },
            tag::CLOCK_SYNC => HostMsg::ClockSync,
            tag::CLOCK_NOW => HostMsg::ClockNow {
                unix_nanos: d.i64()?,
            },
            tag::VOLUME => HostMsg::Volume {
                disk: d.u32()?,
                target: d.string()?,
                readonly: d.bool()?,
                copy: d.bool()?,
            },
            tag::SHARE_CHANGED => HostMsg::ShareChanged {
                target: d.string()?,
                paths: d.list(|d| d.string())?,
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
            GuestMsg::Usage(u) => {
                e = Encoder::new(tag::G_USAGE);
                e.u32(u.cpus)
                    .u64(u.cpu_busy)
                    .u64(u.cpu_total)
                    .u64(u.memory_total)
                    .u64(u.memory_used)
                    .u64(u.disk_total)
                    .u64(u.disk_used)
                    .u32(u.pids);
            }
            GuestMsg::LayerOpened { layer } => {
                e = Encoder::new(tag::LAYER_OPENED);
                e.u32(*layer);
            }
            GuestMsg::LayerEntry(x) => {
                e = Encoder::new(tag::LAYER_ENTRY);
                enc_layer_entry(&mut e, x);
            }
            GuestMsg::Lookup { name } => {
                e = Encoder::new(tag::LOOKUP);
                e.str(name);
            }
            GuestMsg::ClockAsk => e = Encoder::new(tag::CLOCK_ASK),
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
            tag::G_USAGE => GuestMsg::Usage(Usage {
                cpus: d.u32()?,
                cpu_busy: d.u64()?,
                cpu_total: d.u64()?,
                memory_total: d.u64()?,
                memory_used: d.u64()?,
                disk_total: d.u64()?,
                disk_used: d.u64()?,
                pids: d.u32()?,
            }),
            tag::LAYER_OPENED => GuestMsg::LayerOpened { layer: d.u32()? },
            tag::LAYER_ENTRY => GuestMsg::LayerEntry(dec_layer_entry(&mut d)?),
            tag::LOOKUP => GuestMsg::Lookup { name: d.string()? },
            tag::CLOCK_ASK => GuestMsg::ClockAsk,
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
