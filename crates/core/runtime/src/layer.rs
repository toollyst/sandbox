//! 빌드 층 받기 (게스트 → 호스트). 게스트가 보낸 층 항목(`GuestMsg::LayerEntry`)을 압축하지 않은 OCI 층 tar로 쓴다.
//!
//! **믿지 않는 입력**: 게스트는 사용자 코드가 도는 곳이다. 아래를 어기면 [`RuntimeError::Protocol`](한도를 넘으면
//! [`RuntimeError::Invalid`])이고, 그때까지 쓴 출력은 버려야 한다.
//!
//! - 경로는 `check_relative_path`를 통과하고 [`MAX_PATH`] 바이트 안, 이름 하나는 [`MAX_NAME`] 바이트 안이다.
//! - 어느 이름도 `.wh.`로 시작하지 않는다. OCI가 지우기 표시로 쓰는 이름이라 담을 수 없다(지우기는 종류로 온다).
//! - 같은 경로는 한 번만 온다. 부모는 앞서 디렉터리로 왔어야 한다(맨 위 바로 아래는 예외). 그래서 tar 안에서
//!   디렉터리가 아닌 것 아래에 무엇이 오는 일이 없다.
//! - 하드 링크 대상은 앞서 일반 파일로 온 경로다. 링크 대상(심볼릭 링크 포함)은 비어 있지 않고 NUL이 없고
//!   [`MAX_PATH`] 안이다.
//! - xattr 이름은 비어 있지 않고 [`MAX_XATTR_NAME`] 안이며 `=`, NUL, 줄바꿈이 없고(PAX 기록에 담을 수 없다)
//!   `trusted.overlay.`로 시작하지 않고, 한 항목에 한 번만 온다. 값은 [`MAX_XATTR_VALUE`] 안이다(Linux의 한도와 같다).
//!   값에 줄바꿈(0x0a)이 있으면 오류다: PAX 기록은 길이로 끝을 알리므로 담을 수는 있지만, PAX 기록을 줄바꿈에서
//!   나누어 읽는 쪽(`tar` 크레이트)이 그 층을 다르게 읽는다. 조용히 빼지 않고 알린다.
//! - 장치 번호는 ustar 필드(8진 7자리) 안이다.
//! - 일반 파일 내용의 합계는 부르는 쪽이 준 한도 안이다. 알린 크기가 남은 한도보다 크면 내용을 받기 전에 오류다.
//!   `Data`는 알린 크기를 넘지 않고, 다음 항목이나 `End` 전에 정확히 그만큼 온다.
//! - 경로, 링크 대상, xattr의 합계(항목마다 [`ENTRY_COST`]를 더한다)는 [`MAX_METADATA`] 안이다. 메모리에 두는 것은
//!   이것뿐이고, 항목 수도 이것으로 묶인다.
//!
//! **tar 형식** (같은 항목을 같은 차례로 받으면 같은 바이트다)
//!
//! - 항목은 받은 차례로 쓴다. ustar 머리이고 디렉터리 이름은 `/`로 끝난다. 앞에 `./`를 붙이지 않는다. 사용자·그룹
//!   이름은 비운다.
//! - xattr이 있으면 바로 앞에 PAX 확장 머리(`x`)를 둔다: `SCHILY.xattr.<이름>`. PAX에는 xattr만 담고, xattr의 이름과
//!   값에는 줄바꿈이 없다(위의 규칙). 그래서 PAX 기록을 줄바꿈으로 나누어 읽는 쪽(`tar` 크레이트)도 같게 읽는다.
//! - 이름이나 링크 대상이 100바이트를 넘으면 그 다음, 머리 바로 앞에 GNU 긴 이름 항목(`././@LongLink`, `L`·`K`)을
//!   둔다. 이름에는 줄바꿈이 올 수 있으므로 PAX `path`·`linkpath`로 쓰지 않는다(줄로 나누어 읽는 쪽은 그 기록을
//!   놓치거나, 값 안의 기록처럼 보이는 줄을 다른 이름으로 읽는다). 빌드의 다른 tar(`COPY`, 정해진 모양의 층)도 GNU
//!   긴 이름을 쓴다.
//! - 크기, uid, gid가 8진 필드를 넘으면 GNU base-256으로 쓴다. 수정 시각은 0과 8진 11자리 사이로 자른다(음수는 0).
//! - whiteout은 `<부모>/.wh.<이름>`, opaque 디렉터리는 그 디렉터리 머리 바로 뒤의 `<디렉터리>/.wh..wh..opq`다.
//!   둘 다 크기 0인 일반 파일이고, 권한은 0, 소유자와 시각은 게스트가 보낸 값(opaque 표시는 디렉터리의 것)이다.
//! - 끝에 0으로 찬 512바이트 블록 두 개. 바뀐 것이 없는 층은 이 1024바이트뿐이다.

use std::collections::HashMap;
use std::io::Write;

use sbx_guest_proto::{GuestMsg, LayerEntry, LayerKind, check_relative_path};

use crate::error::RuntimeError;

/// 경로와 링크 대상의 길이 상한(바이트). Linux `PATH_MAX`.
pub const MAX_PATH: usize = 4096;
/// 이름 하나의 길이 상한(바이트). Linux `NAME_MAX`.
pub const MAX_NAME: usize = 255;
/// xattr 이름의 길이 상한(바이트). Linux `XATTR_NAME_MAX`.
pub const MAX_XATTR_NAME: usize = 255;
/// xattr 값의 길이 상한(바이트). Linux `XATTR_SIZE_MAX`.
pub const MAX_XATTR_VALUE: usize = 64 << 10;
/// 층 하나에서 메모리에 두는 경로·링크 대상·xattr의 합계 상한(바이트).
pub const MAX_METADATA: usize = 64 << 20;
/// 항목 하나가 [`MAX_METADATA`]에서 쓰는 고정 몫(바이트). 빈 이름이 많아도 메모리가 묶이게 한다.
pub const ENTRY_COST: usize = 64;

const WHITEOUT: &str = ".wh.";
const OPAQUE: &str = ".wh..wh..opq";
const BLOCK: usize = 512;
/// ustar 장치 번호 필드(8진 7자리)의 최대값.
const MAX_DEVICE: u32 = 0o7777777;
/// mtime 필드(8진 11자리)의 최대값.
const MAX_MTIME: i64 = 0o77777777777;

/// 받은 층의 크기.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayerStats {
    /// 게스트가 보낸 항목 수(whiteout 포함).
    pub entries: u64,
    /// 일반 파일 내용의 합계(바이트).
    pub content_bytes: u64,
    /// 쓴 tar의 크기(바이트).
    pub tar_bytes: u64,
}

fn proto_err(m: impl Into<String>) -> RuntimeError {
    RuntimeError::Protocol(m.into())
}

/// 앞서 받은 경로의 종류. 부모와 하드 링크 대상을 확인하는 데만 쓴다.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Seen {
    Dir,
    File,
    Other,
}

/// 층 항목을 받아 `sink`에 tar로 쓴다. `next`는 다음 메시지를 준다. `End`에서 끝난다. 규칙은 모듈 문서에 있다.
/// `limit`은 일반 파일 내용 합계의 상한이다.
pub fn receive(
    sink: &mut dyn Write,
    limit: u64,
    next: &mut dyn FnMut() -> Result<GuestMsg, RuntimeError>,
) -> Result<LayerStats, RuntimeError> {
    let mut w = TarWriter { sink, written: 0 };
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut budget = MAX_METADATA;
    let mut stats = LayerStats::default();
    let mut left = limit;
    loop {
        let e = match next()? {
            GuestMsg::End => break,
            GuestMsg::LayerEntry(e) => e,
            GuestMsg::Error(e) => return Err(RuntimeError::Guest(e)),
            other => {
                return Err(proto_err(format!(
                    "expected LayerEntry or End, got {other:?}"
                )));
            }
        };
        stats.entries += 1;
        let cost = check(&e, &seen)?;
        budget = budget.checked_sub(cost).ok_or_else(|| {
            RuntimeError::Invalid(format!(
                "the layer has more than {MAX_METADATA} bytes of names and xattrs (at {:?})",
                e.path
            ))
        })?;
        let class = match e.kind {
            LayerKind::Dir { .. } => Seen::Dir,
            LayerKind::File { .. } => Seen::File,
            _ => Seen::Other,
        };
        match &e.kind {
            LayerKind::File { size } => {
                if *size > left {
                    return Err(RuntimeError::Invalid(format!(
                        "{:?} is {size} bytes; the layer may hold {limit} bytes of file contents",
                        e.path
                    )));
                }
                left -= size;
                stats.content_bytes += size;
                w.file(&e, *size, next)?;
            }
            LayerKind::Whiteout => {
                let (dir, name) = split(&e.path);
                w.marker(&join(dir, &format!("{WHITEOUT}{name}")), &e)?;
            }
            LayerKind::Dir { opaque } => {
                w.header(&e, b'5', 0, "")?;
                if *opaque {
                    w.marker(&format!("{}/{OPAQUE}", e.path), &e)?;
                }
            }
            LayerKind::Symlink { target } => w.header(&e, b'2', 0, target)?,
            LayerKind::Hardlink { target } => w.header(&e, b'1', 0, target)?,
            LayerKind::CharDevice { .. } => w.header(&e, b'3', 0, "")?,
            LayerKind::BlockDevice { .. } => w.header(&e, b'4', 0, "")?,
            LayerKind::Fifo => w.header(&e, b'6', 0, "")?,
        }
        seen.insert(e.path, class);
    }
    w.write(&[0; 2 * BLOCK])?;
    stats.tar_bytes = w.written;
    Ok(stats)
}

fn split(path: &str) -> (&str, &str) {
    path.rsplit_once('/').unwrap_or(("", path))
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

fn check_link(path: &str, what: &str, target: &str) -> Result<(), RuntimeError> {
    if target.is_empty() || target.len() > MAX_PATH || target.contains('\0') {
        return Err(proto_err(format!(
            "{path:?}: {what} target must be 1 to {MAX_PATH} bytes without NUL"
        )));
    }
    Ok(())
}

/// 항목 하나를 확인하고 [`MAX_METADATA`]에서 쓰는 몫을 돌려준다.
fn check(e: &LayerEntry, seen: &HashMap<String, Seen>) -> Result<usize, RuntimeError> {
    let p = &e.path;
    check_relative_path(p).map_err(proto_err)?;
    if p.len() > MAX_PATH {
        return Err(proto_err(format!(
            "a path of {} bytes; at most {MAX_PATH}",
            p.len()
        )));
    }
    for c in p.split('/') {
        if c.len() > MAX_NAME {
            return Err(proto_err(format!(
                "{p:?}: a name longer than {MAX_NAME} bytes"
            )));
        }
        if c.starts_with(WHITEOUT) {
            return Err(proto_err(format!(
                "{p:?}: names starting with {WHITEOUT:?} cannot be stored in an OCI layer"
            )));
        }
    }
    if seen.contains_key(p) {
        return Err(proto_err(format!("{p:?} was sent twice")));
    }
    let parent = split(p).0;
    if !parent.is_empty() && seen.get(parent) != Some(&Seen::Dir) {
        return Err(proto_err(format!(
            "{p:?}: its parent was not sent as a directory before it"
        )));
    }
    let mut cost = ENTRY_COST + p.len();
    match &e.kind {
        LayerKind::Symlink { target } => {
            check_link(p, "symlink", target)?;
            cost += target.len();
        }
        LayerKind::Hardlink { target } => {
            check_link(p, "hard link", target)?;
            if seen.get(target.as_str()) != Some(&Seen::File) {
                return Err(proto_err(format!(
                    "{p:?}: hard link target {target:?} was not sent as a file before it"
                )));
            }
            cost += target.len();
        }
        LayerKind::CharDevice { major, minor } | LayerKind::BlockDevice { major, minor }
            if *major > MAX_DEVICE || *minor > MAX_DEVICE =>
        {
            return Err(proto_err(format!(
                "{p:?}: device {major}:{minor} does not fit a tar header"
            )));
        }
        _ => {}
    }
    let mut names = std::collections::HashSet::new();
    for (name, value) in &e.xattrs {
        if !names.insert(name.as_str()) {
            return Err(proto_err(format!("{p:?}: xattr {name:?} was sent twice")));
        }
        if value.contains(&b'\n') {
            return Err(proto_err(format!(
                "{p:?}: xattr {name} has a newline byte in its value, which sbx's layer reader cannot read back"
            )));
        }
        if name.is_empty()
            || name.len() > MAX_XATTR_NAME
            || name.contains(['=', '\0', '\n'])
            || name.starts_with("trusted.overlay.")
        {
            return Err(proto_err(format!(
                "{p:?}: xattr name {name:?} is not allowed"
            )));
        }
        if value.len() > MAX_XATTR_VALUE {
            return Err(proto_err(format!(
                "{p:?}: xattr {name} is {} bytes; at most {MAX_XATTR_VALUE}",
                value.len()
            )));
        }
        cost += name.len() + value.len();
    }
    Ok(cost)
}

struct TarWriter<'a> {
    sink: &'a mut dyn Write,
    written: u64,
}

/// 8진 필드. 맞지 않으면 `None`.
fn octal(field: &mut [u8], v: u64) -> Option<()> {
    let digits = field.len() - 1;
    let s = format!("{v:0digits$o}");
    if s.len() > digits {
        return None;
    }
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
    Some(())
}

/// 8진으로, 맞지 않으면 GNU base-256으로 쓴다(첫 바이트의 맨 위 비트가 표시, 나머지는 big-endian).
fn number(field: &mut [u8], v: u64) {
    if octal(field, v).is_none() {
        field.fill(0);
        let b = v.to_be_bytes();
        let n = field.len();
        field[n - 8..].copy_from_slice(&b);
        field[0] |= 0x80;
    }
}

/// PAX 기록 한 줄: `<길이> <키>=<값>\n`. 길이는 그 줄 전체(자기 자릿수 포함)다.
fn pax_record(out: &mut Vec<u8>, key: &str, value: &[u8]) {
    let rest = 1 + key.len() + 1 + value.len() + 1;
    let mut len = rest + 1;
    while len.to_string().len() + rest != len {
        len = len.to_string().len() + rest;
    }
    out.extend_from_slice(len.to_string().as_bytes());
    out.push(b' ');
    out.extend_from_slice(key.as_bytes());
    out.push(b'=');
    out.extend_from_slice(value);
    out.push(b'\n');
}

impl TarWriter<'_> {
    fn write(&mut self, b: &[u8]) -> Result<(), RuntimeError> {
        self.sink.write_all(b)?;
        self.written += b.len() as u64;
        Ok(())
    }

    fn pad(&mut self, len: u64) -> Result<(), RuntimeError> {
        let rem = (len % BLOCK as u64) as usize;
        if rem != 0 {
            self.write(&vec![0; BLOCK - rem])?;
        }
        Ok(())
    }

    /// 머리 하나(필요하면 PAX 머리와 GNU 긴 이름 항목 먼저).
    #[allow(clippy::too_many_arguments)]
    fn raw_header(
        &mut self,
        name: &str,
        typeflag: u8,
        mode: u32,
        uid: u32,
        gid: u32,
        size: u64,
        mtime: i64,
        link: &str,
        device: (u32, u32),
        xattrs: &[(String, Vec<u8>)],
    ) -> Result<(), RuntimeError> {
        let mut pax = Vec::new();
        for (k, v) in xattrs {
            pax_record(&mut pax, &format!("SCHILY.xattr.{k}"), v);
        }
        if !pax.is_empty() {
            let h = header_block("PaxHeader", b'x', 0, 0, 0, pax.len() as u64, 0, "", (0, 0));
            self.write(&h)?;
            self.write(&pax)?;
            self.pad(pax.len() as u64)?;
        }
        if name.len() > 100 {
            self.long_name(b'L', name)?;
        }
        if link.len() > 100 {
            self.long_name(b'K', link)?;
        }
        let short = |s: &str| -> String {
            if s.len() <= 100 {
                s.to_owned()
            } else {
                // GNU 긴 이름 항목이 정본이다. 필드에는 UTF-8 경계에서 자른 앞부분을 둔다.
                let mut end = 100;
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                s[..end].to_owned()
            }
        };
        let h = header_block(
            &short(name),
            typeflag,
            mode,
            uid,
            gid,
            size,
            mtime.clamp(0, MAX_MTIME),
            &short(link),
            device,
        );
        self.write(&h)
    }

    /// GNU 긴 이름 항목(`L`은 이름, `K`는 링크 대상): GNU 머리의 `././@LongLink`, 내용은 값과 NUL 하나.
    /// `tar` 크레이트가 이 모양으로 쓴다.
    fn long_name(&mut self, typeflag: u8, value: &str) -> Result<(), RuntimeError> {
        let mut data = value.as_bytes().to_vec();
        data.push(0);
        let mut h = header_block(
            "././@LongLink",
            typeflag,
            0o644,
            0,
            0,
            data.len() as u64,
            0,
            "",
            (0, 0),
        );
        h[257..265].copy_from_slice(b"ustar  \0");
        seal(&mut h);
        self.write(&h)?;
        self.write(&data)?;
        self.pad(data.len() as u64)
    }

    fn header(
        &mut self,
        e: &LayerEntry,
        typeflag: u8,
        size: u64,
        link: &str,
    ) -> Result<(), RuntimeError> {
        let name = if typeflag == b'5' {
            format!("{}/", e.path)
        } else {
            e.path.clone()
        };
        let device = match e.kind {
            LayerKind::CharDevice { major, minor } | LayerKind::BlockDevice { major, minor } => {
                (major, minor)
            }
            _ => (0, 0),
        };
        self.raw_header(
            &name, typeflag, e.mode, e.uid, e.gid, size, e.mtime, link, device, &e.xattrs,
        )
    }

    /// whiteout이나 opaque 표시: 크기 0인 일반 파일.
    fn marker(&mut self, name: &str, e: &LayerEntry) -> Result<(), RuntimeError> {
        self.raw_header(name, b'0', 0, e.uid, e.gid, 0, e.mtime, "", (0, 0), &[])
    }

    fn file(
        &mut self,
        e: &LayerEntry,
        size: u64,
        next: &mut dyn FnMut() -> Result<GuestMsg, RuntimeError>,
    ) -> Result<(), RuntimeError> {
        self.header(e, b'0', size, "")?;
        let mut left = size;
        while left > 0 {
            let data = match next()? {
                GuestMsg::Data(d) => d,
                GuestMsg::Error(err) => return Err(RuntimeError::Guest(err)),
                other => {
                    return Err(proto_err(format!(
                        "{:?}: expected Data, got {other:?}",
                        e.path
                    )));
                }
            };
            if data.len() as u64 > left {
                return Err(proto_err(format!(
                    "{:?}: more data than the declared {size} bytes",
                    e.path
                )));
            }
            self.write(&data)?;
            left -= data.len() as u64;
        }
        self.pad(size)
    }
}

#[allow(clippy::too_many_arguments)]
fn header_block(
    name: &str,
    typeflag: u8,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    mtime: i64,
    link: &str,
    (major, minor): (u32, u32),
) -> [u8; BLOCK] {
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    number(&mut h[100..108], u64::from(mode));
    number(&mut h[108..116], u64::from(uid));
    number(&mut h[116..124], u64::from(gid));
    number(&mut h[124..136], size);
    number(&mut h[136..148], mtime as u64);
    h[156] = typeflag;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    if matches!(typeflag, b'3' | b'4') {
        number(&mut h[329..337], u64::from(major));
        number(&mut h[337..345], u64::from(minor));
    }
    seal(&mut h);
    h
}

/// 검사합: 검사합 칸을 공백으로 보고 모든 바이트를 더한다.
fn seal(h: &mut [u8; BLOCK]) {
    h[148..156].fill(b' ');
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    let s = format!("{sum:06o}\0 ");
    h[148..156].copy_from_slice(s.as_bytes());
}

#[cfg(test)]
mod tests;
