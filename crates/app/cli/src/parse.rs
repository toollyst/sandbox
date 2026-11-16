//! 옵션 값 해석. Docker와 같은 문법을 받는다. 다른 점은 [`crate::compat`]의 표에 있다.

use std::path::{Path, PathBuf};

use sbx_model::{Arch, Mount, MountKind, SandboxId, VolumeMount};

/// 해석 오류. 사용자에게 그대로 보여줄 문장이다.
pub type ParseResult<T> = Result<T, String>;

/// `-e K=V`. `=`가 없으면 호스트 환경 변수 값을 가져오고, 호스트에도 없으면 건너뛴다(Docker와 같다).
pub fn env(values: &[String], host: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    values
        .iter()
        .filter_map(|v| match v.split_once('=') {
            Some((k, val)) => Some((k.to_owned(), val.to_owned())),
            None => host(v).map(|val| (v.clone(), val)),
        })
        .collect()
}

/// `--env-file` 내용을 `-e` 값들로 바꾼다(Docker와 같다).
///
/// - 줄 앞 공백은 지우고, 빈 줄과 `#`로 시작하는 줄은 건너뛴다. 값은 따옴표를 벗기지 않고 그대로 둔다.
/// - `=`가 없는 줄은 이름만 남긴다. 값은 [`env()`]가 호스트에서 가져온다.
/// - 이름이 비었거나 공백이 있으면 오류다.
pub fn env_file(text: &str) -> ParseResult<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_start();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split_once('=').map_or(line, |(k, _)| k);
        if name.is_empty() {
            return Err(format!("no variable name on line {}: {line:?}", n + 1));
        }
        if name.contains(char::is_whitespace) {
            return Err(format!(
                "variable {name:?} contains whitespace (line {})",
                n + 1
            ));
        }
        out.push(line.to_owned());
    }
    Ok(out)
}

/// `--label k=v`. `=`가 없으면 값은 빈 문자열(Docker와 같다).
pub fn label(value: &str) -> (String, String) {
    match value.split_once('=') {
        Some((k, v)) => (k.to_owned(), v.to_owned()),
        None => (value.to_owned(), String::new()),
    }
}

/// `--memory`. 단위 `b`, `k`, `m`, `g`(대소문자 무관), 단위가 없으면 바이트. MiB 단위로 나누어떨어져야 한다.
pub fn memory_mib(value: &str) -> ParseResult<u32> {
    let v = value.trim().to_ascii_lowercase();
    let (num, mult): (&str, u64) = match v.char_indices().last() {
        Some((i, 'b')) => (&v[..i], 1),
        Some((i, 'k')) => (&v[..i], 1 << 10),
        Some((i, 'm')) => (&v[..i], 1 << 20),
        Some((i, 'g')) => (&v[..i], 1 << 30),
        _ => (&v[..], 1),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid memory value {value:?}"))?;
    let bytes = n
        .checked_mul(mult)
        .ok_or_else(|| format!("memory value too large: {value:?}"))?;
    if bytes == 0 || bytes % (1 << 20) != 0 {
        return Err(format!(
            "memory must be a positive multiple of 1 MiB: {value:?}"
        ));
    }
    u32::try_from(bytes >> 20).map_err(|_| format!("memory value too large: {value:?}"))
}

/// 쓰기 디스크(scratch)의 가장 작은 크기(MiB). 게스트가 그 위에 ext 파일 시스템을 만든다.
pub const MIN_SCRATCH_MIB: u32 = 64;

/// `--storage-opt KEY=VALUE`(Docker와 같은 모양). 지금 키는 `size`(쓰기 디스크 크기: `20g`, `512m`, `1t`)뿐이다.
/// 디스크는 sparse 파일이라 호스트에서는 게스트가 쓴 만큼만 차지한다. 모르는 키는 오류다.
pub fn storage_opt(value: &str) -> ParseResult<u32> {
    let (key, v) = value
        .split_once('=')
        .ok_or_else(|| format!("--storage-opt takes KEY=VALUE, not {value:?}"))?;
    if key.trim() != "size" {
        return Err(format!(
            "unknown storage option {key:?} (the only one is size, e.g. size=20g)"
        ));
    }
    let v = v.trim().to_ascii_lowercase();
    let v = v
        .strip_suffix('b')
        .filter(|x| x.ends_with(['k', 'm', 'g', 't']))
        .unwrap_or(&v);
    let mib = match v.strip_suffix('t') {
        Some(n) => n
            .parse::<u32>()
            .ok()
            .and_then(|n| n.checked_mul(1 << 20))
            .ok_or_else(|| format!("invalid size {v:?}"))?,
        None => memory_mib(v).map_err(|_| format!("invalid size {v:?} (e.g. 512m, 20g, 1t)"))?,
    };
    if mib < MIN_SCRATCH_MIB {
        return Err(format!("size must be at least {MIN_SCRATCH_MIB}m"));
    }
    Ok(mib)
}

/// 디스크 크기 하나(`--storage-opt`의 `size` 값과 같은 모양): `512m`, `20g`, `1t`.
pub fn disk_mib(value: &str) -> ParseResult<u32> {
    storage_opt(&format!("size={value}"))
}

/// 전송량(`--egress-limit`): 바이트 수, 또는 `k`·`m`·`g`·`t`(1024의 거듭제곱, 뒤에 `b`를 붙여도 된다). 1 이상.
pub fn transfer_bytes(value: &str) -> ParseResult<u64> {
    let v = value.trim().to_ascii_lowercase();
    let v = v.strip_suffix('b').unwrap_or(&v);
    let (num, shift) = match v.chars().last() {
        Some('k') => (&v[..v.len() - 1], 10),
        Some('m') => (&v[..v.len() - 1], 20),
        Some('g') => (&v[..v.len() - 1], 30),
        Some('t') => (&v[..v.len() - 1], 40),
        _ => (v, 0),
    };
    num.parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(1 << shift))
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("invalid size {value:?} (bytes, or e.g. 500m, 2g)"))
}

/// `--cpus`. VM의 vCPU 개수라서 1 이상의 정수만 받는다.
pub fn cpus(value: &str) -> ParseResult<u8> {
    match value.parse::<u8>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "--cpus must be a whole number of vCPUs (1-255): {value:?}"
        )),
    }
}

/// `--timeout`(샌드박스 수명): Go 표기의 시간(`90s`, `30m`, `1h30m`) 또는 초(`600`). 1초 이상, 초 단위로 내린다.
pub fn lifetime_secs(value: &str) -> ParseResult<u64> {
    let v = value.trim();
    let secs = match v.parse::<u64>() {
        Ok(n) => Some(n),
        Err(_) => duration_ns(v).map(|ns| ns / 1_000_000_000),
    };
    match secs {
        Some(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "invalid timeout {value:?}: use seconds (600) or a duration like 30m or 1h30m, at least 1s"
        )),
    }
}

/// `--since`, `--until`의 시각. 유닉스 시각(나노초)을 돌려준다. 받는 것:
///
/// - RFC 3339: `2026-11-10T13:20:14Z`, 소수 초(`.5`), 시간대 차(`+09:00`).
/// - 유닉스 시각(초): `1794316814`, 소수 가능(`1794316814.5`).
/// - 지금에서 거슬러 간 시간(Go 표기, Docker와 같다): `90s`, `10m`, `1h30m`, `500ms`. 단위는 `ns`, `us`, `ms`, `s`, `m`, `h`.
///
/// Docker가 받는 그 밖의 형식(시간대 없는 날짜 등)은 받지 않는다. 어느 시간대로 읽을지 모호하기 때문이다.
pub fn time_ns(value: &str, now_ns: u64) -> ParseResult<u64> {
    let v = value.trim();
    if let Some(ago) = duration_ns(v) {
        return Ok(now_ns.saturating_sub(ago));
    }
    unix_ns(v).or_else(|| rfc3339_ns(v)).ok_or_else(|| {
        format!(
            "invalid time {value:?}: use RFC 3339 (2026-11-10T13:20:14Z), Unix seconds, \
             or a duration ago (10m, 1h30m)"
        )
    })
}

/// `정수[.소수]`를 `unit` 배로. 소수는 9자리까지 본다.
fn decimal_times(num: &str, unit: u128) -> Option<u128> {
    let (int, frac) = num.split_once('.').unwrap_or((num, ""));
    if int.is_empty() && frac.is_empty() {
        return None;
    }
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if !all_digits(int) || !all_digits(frac) {
        return None;
    }
    let int: u128 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let mut total = int.checked_mul(unit)?;
    let frac = &frac[..frac.len().min(9)];
    if !frac.is_empty() {
        let scale = 10u128.pow(frac.len() as u32);
        total = total.checked_add(frac.parse::<u128>().ok()? * unit / scale)?;
    }
    Some(total)
}

fn duration_ns(v: &str) -> Option<u64> {
    const UNITS: &[(&str, u128)] = &[
        ("ns", 1),
        ("us", 1_000),
        ("ms", 1_000_000),
        ("s", 1_000_000_000),
        ("m", 60_000_000_000),
        ("h", 3_600_000_000_000),
    ];
    if v.is_empty() {
        return None;
    }
    let mut rest = v;
    let mut total: u128 = 0;
    while !rest.is_empty() {
        let end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
        let (num, after) = rest.split_at(end);
        // 두 글자 단위(ms 등)를 먼저 본다.
        let (unit, len) = UNITS
            .iter()
            .filter(|(u, _)| after.starts_with(u))
            .map(|(u, n)| (*n, u.len()))
            .max_by_key(|(_, len)| *len)?;
        total = total.checked_add(decimal_times(num, unit)?)?;
        rest = &after[len..];
    }
    u64::try_from(total).ok()
}

fn unix_ns(v: &str) -> Option<u64> {
    if v.starts_with('.') || v.ends_with('.') {
        return None;
    }
    u64::try_from(decimal_times(v, 1_000_000_000)?).ok()
}

fn rfc3339_ns(v: &str) -> Option<u64> {
    let b = v.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = v.get(r)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, min, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=days_in_month).contains(&day) || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    let mut rest = &v[19..];
    let mut frac_ns = 0u128;
    if let Some(r) = rest.strip_prefix('.') {
        let end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        if end == 0 {
            return None;
        }
        frac_ns = decimal_times(&format!("0.{}", &r[..end]), 1_000_000_000)?;
        rest = &r[end..];
    }
    let offset_secs: i64 = match rest.as_bytes() {
        [b'Z' | b'z'] => 0,
        [sign @ (b'+' | b'-'), h1, h2, b':', m1, m2] => {
            let digits = [*h1, *h2, *m1, *m2];
            if !digits.iter().all(u8::is_ascii_digit) {
                return None;
            }
            let d = |i: usize| i64::from(digits[i] - b'0');
            let (oh, om) = (d(0) * 10 + d(1), d(2) * 10 + d(3));
            if oh > 23 || om > 59 {
                return None;
            }
            let s = oh * 3600 + om * 60;
            if *sign == b'+' { s } else { -s }
        }
        _ => return None,
    };
    // 그레고리력 날짜를 일 수로 (Howard Hinnant의 days_from_civil).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3600 + min * 60 + sec - offset_secs;
    let secs = u64::try_from(secs).ok()?;
    u64::try_from(u128::from(secs) * 1_000_000_000 + frac_ns).ok()
}

/// `--platform linux/amd64` 또는 `linux/arm64`(`linux/arm64/v8`도 받는다).
pub fn platform(value: &str) -> ParseResult<Arch> {
    match value {
        "linux/amd64" => Ok(Arch::X86_64),
        "linux/arm64" | "linux/arm64/v8" => Ok(Arch::Aarch64),
        _ => Err(format!(
            "unsupported platform {value:?} (linux/amd64 or linux/arm64)"
        )),
    }
}

fn host_path(src: &str, whole: &str) -> ParseResult<PathBuf> {
    let p = Path::new(src);
    if p.is_absolute() {
        Ok(p.to_owned())
    } else {
        Err(format!(
            "mount source must be an absolute host path or a volume name: {whole:?}"
        ))
    }
}

/// `-v`, `--mount`가 붙이는 것.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attach {
    /// 호스트 폴더(`bind`, `copy`).
    Folder(Mount),
    /// 이름 붙은 볼륨.
    Volume(VolumeMount),
}

/// `-v`의 첫 부분이 볼륨 이름인지(Docker 규칙): `/`, `.`, `~`로 시작하거나 경로 구분자가 있거나 Windows 드라이브
/// (`C:`)이면 호스트 경로다. 그 밖은 이름이다(이름 규칙은 [`sbx_model::check_volume_name`]이 본다).
pub fn is_volume_name(src: &str) -> bool {
    let b = src.as_bytes();
    let drive = b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':';
    !(src.is_empty() || src.starts_with(['/', '.', '~']) || src.contains(['/', '\\']) || drive)
}

const ANONYMOUS: &str = "anonymous volumes are not supported: name the volume (-v NAME:/path, \
                         or --mount type=volume,source=NAME,target=/path); it is created on first use";

fn named(name: &str, dst: &str, whole: &str, readonly: bool, no_copy: bool) -> ParseResult<Attach> {
    sbx_model::check_volume_name(name).map_err(|e| format!("{e}: {whole:?}"))?;
    Ok(Attach::Volume(VolumeMount {
        name: name.to_owned(),
        destination: guest_path(dst, whole)?,
        readonly,
        no_copy,
    }))
}

fn guest_path(dst: &str, whole: &str) -> ParseResult<String> {
    if dst.starts_with('/') {
        Ok(dst.to_owned())
    } else {
        Err(format!("mount target must be an absolute path: {whole:?}"))
    }
}

/// `-v SRC:DST[:OPTIONS]`. SRC가 호스트 경로면 `--mount type=bind`, 이름이면 `--mount type=volume`의 줄임말이다
/// ([`is_volume_name`]). OPTIONS는 쉼표로 이은 `ro`, `rw`, `nocopy`(볼륨만: 빈 볼륨을 이미지 내용으로 채우지 않는다).
/// 경로 하나(`-v /path`, Docker의 이름 없는 볼륨)는 거부한다.
///
/// 뒤에서부터 나눈다. 그래서 Windows 경로(`C:\work:/work`)도 받는다.
pub fn volume(value: &str) -> ParseResult<Attach> {
    let options =
        |o: &str| !o.is_empty() && o.split(',').all(|x| matches!(x, "ro" | "rw" | "nocopy"));
    let (rest, opts) = match value.rsplit_once(':') {
        Some((rest, o)) if options(o) => (rest, o.split(',').collect::<Vec<_>>()),
        _ => (value, Vec::new()),
    };
    let readonly = opts.contains(&"ro");
    if readonly && opts.contains(&"rw") {
        return Err(format!("both ro and rw: {value:?}"));
    }
    let no_copy = opts.contains(&"nocopy");
    let Some((src, dst)) = rest.rsplit_once(':') else {
        if rest.starts_with('/') {
            return Err(format!("{ANONYMOUS}: {value:?}"));
        }
        return Err(format!("expected SRC:DST[:ro]: {value:?}"));
    };
    if is_volume_name(src) {
        return named(src, dst, value, readonly, no_copy);
    }
    if no_copy {
        return Err(format!("nocopy is only for volumes: {value:?}"));
    }
    Ok(Attach::Folder(Mount {
        source: host_path(src, value)?,
        destination: guest_path(dst, value)?,
        kind: MountKind::Bind,
        readonly,
    }))
}

/// `--mount type=bind|copy|volume,source=...,target=...[,readonly][,volume-nocopy]`.
///
/// 키 이름은 Docker와 같다: `source`/`src`, `target`/`destination`/`dst`, `readonly`/`ro`(값 없이 또는 `true`/`false`),
/// `volume-nocopy`(볼륨만). `type=volume`의 `source`는 볼륨 이름이고, 없으면(Docker의 이름 없는 볼륨) 거부한다.
pub fn mount(value: &str) -> ParseResult<Attach> {
    let (mut kind, mut src, mut dst, mut readonly) = (None, None, None, false);
    let (mut volume, mut no_copy) = (false, false);
    for field in value.split(',') {
        let (k, v) = match field.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (field, None),
        };
        match (k, v) {
            ("type", Some("bind")) => kind = Some(MountKind::Bind),
            ("type", Some("copy")) => kind = Some(MountKind::Copy),
            ("type", Some("volume")) => volume = true,
            ("type", Some(t)) => {
                return Err(format!(
                    "unsupported mount type {t:?} (bind, copy or volume)"
                ));
            }
            ("volume-nocopy", None | Some("true" | "1")) => no_copy = true,
            ("volume-nocopy", Some("false" | "0")) => no_copy = false,
            ("source" | "src", Some(s)) => src = Some(s),
            ("target" | "destination" | "dst", Some(d)) => dst = Some(d),
            ("readonly" | "ro", None | Some("true" | "1")) => readonly = true,
            ("readonly" | "ro", Some("false" | "0")) => readonly = false,
            _ => return Err(format!("unknown mount option {field:?}")),
        }
    }
    if volume {
        let dst = dst.ok_or_else(|| format!("mount needs target=: {value:?}"))?;
        let Some(name) = src else {
            return Err(format!("{ANONYMOUS}: {value:?}"));
        };
        return named(name, dst, value, readonly, no_copy);
    }
    if no_copy {
        return Err(format!("volume-nocopy is only for type=volume: {value:?}"));
    }
    Ok(Attach::Folder(Mount {
        source: host_path(
            src.ok_or_else(|| format!("mount needs source=: {value:?}"))?,
            value,
        )?,
        destination: guest_path(
            dst.ok_or_else(|| format!("mount needs target=: {value:?}"))?,
            value,
        )?,
        kind: kind
            .ok_or_else(|| format!("mount needs type=bind, type=copy or type=volume: {value:?}"))?,
        readonly,
    }))
}

/// `cp`의 한쪽 끝.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpEndpoint {
    /// 호스트 경로.
    Host(PathBuf),
    /// 샌드박스 안의 경로.
    Sandbox {
        /// 샌드박스 ID(나 그 앞부분) 또는 이름.
        id: String,
        /// 게스트 경로.
        path: String,
    },
}

/// `cp`의 인자 하나. 절대 경로이거나 `.`으로 시작하면 호스트, `ID:경로`나 `이름:경로`이면 샌드박스다(Docker와 같다).
pub fn cp_endpoint(value: &str) -> CpEndpoint {
    if Path::new(value).is_absolute() || value.starts_with('.') {
        return CpEndpoint::Host(value.into());
    }
    match value.split_once(':') {
        Some((id, path))
            if SandboxId::new(id).is_ok() || sbx_model::check_sandbox_name(id).is_ok() =>
        {
            CpEndpoint::Sandbox {
                id: id.to_owned(),
                path: path.to_owned(),
            }
        }
        _ => CpEndpoint::Host(value.into()),
    }
}

/// `-p`의 값([`sbx_model::PortMapping`]).
pub fn port_mapping(v: &str) -> Result<sbx_model::PortMapping, String> {
    v.parse()
}

/// `--connect`의 항목 하나. 대상은 아직 찾지 않았다(ID 앞부분일 수 있다). 찾아서 기록할 꼴로 바꾸는 규칙은
/// [`crate::behavior::connect_rule`]에 있다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectArg {
    /// `NAME=`으로 준 안의 이름(소문자, [`sbx_model::check_connect_name`]). 없으면 대상의 이름을 쓴다.
    pub name: Option<String>,
    /// 대상: 이름, ID, ID 앞부분(다른 명령과 같다).
    pub target: String,
    /// 대상 안의 포트.
    pub port: u16,
}

/// `--connect [NAME=]SANDBOX:PORT`. 쉼표로 여럿을 준다. 숫자만 있는 조각은 앞 항목의 대상에 포트를 더한다
/// (`db:5432,8080`은 `db:5432,db:8080`). 대상은 샌드박스 이름이나 ID(앞부분)의 꼴이어야 한다.
pub fn connect(value: &str) -> ParseResult<Vec<ConnectArg>> {
    let bad = |why: &str| format!("invalid --connect {value:?}: {why} (want [NAME=]SANDBOX:PORT)");
    let port = |p: &str| match p.parse::<u16>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(bad(&format!("{p:?} is not a port (1 to 65535)"))),
    };
    let mut out: Vec<ConnectArg> = Vec::new();
    for piece in value.split(',') {
        if !piece.is_empty() && piece.bytes().all(|b| b.is_ascii_digit()) {
            let Some(last) = out.last() else {
                return Err(bad("a port comes after SANDBOX:"));
            };
            let more = ConnectArg {
                port: port(piece)?,
                ..last.clone()
            };
            out.push(more);
            continue;
        }
        let (name, rest) = match piece.split_once('=') {
            Some((n, rest)) => (
                Some(sbx_model::check_connect_name(n).map_err(|e| bad(&e.to_string()))?),
                rest,
            ),
            None => (None, piece),
        };
        let Some((target, p)) = rest.rsplit_once(':') else {
            return Err(bad("missing :PORT"));
        };
        if sbx_model::check_sandbox_name(target).is_err() && SandboxId::new(target).is_err() {
            return Err(bad(&format!("{target:?} is not a sandbox name or ID")));
        }
        out.push(ConnectArg {
            name,
            target: target.to_owned(),
            port: port(p)?,
        });
    }
    Ok(out)
}

/// clap의 `--connect` 값 검사: [`connect`]로 읽어 보고 받은 글을 그대로 둔다(대상은 만들 때 찾는다).
pub fn connect_value(value: &str) -> ParseResult<String> {
    connect(value).map(|_| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_takes_names_targets_and_ports() {
        let c = |name: Option<&str>, target: &str, port: u16| ConnectArg {
            name: name.map(Into::into),
            target: target.into(),
            port,
        };
        assert_eq!(connect("db:5432"), Ok(vec![c(None, "db", 5432)]));
        assert_eq!(
            connect("PG=db:5432,8080,cache:6379"),
            Ok(vec![
                c(Some("pg"), "db", 5432),
                c(Some("pg"), "db", 8080),
                c(None, "cache", 6379)
            ])
        );
        assert_eq!(connect("3f2a:80"), Ok(vec![c(None, "3f2a", 80)]));
        for bad in [
            "",
            "db",
            "db:",
            "db:0",
            "db:65536",
            ":80",
            "5432",
            "db:5432,",
            "localhost=db:1",
            "a b:1",
            "x=:1",
            "10.0.0.1=db:1",
        ] {
            assert!(connect(bad).is_err(), "{bad:?}");
        }
        assert!(connect("db").unwrap_err().contains("[NAME=]SANDBOX:PORT"));
    }

    #[test]
    fn times_take_rfc3339_unix_seconds_and_durations_ago() {
        let now = 1_794_316_814_000_000_000;
        assert_eq!(time_ns("2026-11-10T13:20:14Z", 0), Ok(now));
        assert_eq!(time_ns("2026-11-10t13:20:14z", 0), Ok(now));
        assert_eq!(
            time_ns("2026-11-10T13:20:14.5+09:00", 0),
            Ok(1_794_284_414_500_000_000)
        );
        assert_eq!(
            time_ns("2000-02-29T00:00:00Z", 0),
            Ok(951_782_400_000_000_000)
        );
        assert_eq!(time_ns("1794316814", 0), Ok(now));
        assert_eq!(time_ns("1794316814.25", 0), Ok(now + 250_000_000));
        // 단위 없는 수는 기간이 아니라 유닉스 시각이다.
        assert_eq!(time_ns("10", now), Ok(10_000_000_000));
        assert_eq!(time_ns("10m", now), Ok(now - 600_000_000_000));
        assert_eq!(time_ns("1h30m", now), Ok(now - 5_400_000_000_000));
        assert_eq!(time_ns("1.5s", now), Ok(now - 1_500_000_000));
        assert_eq!(time_ns("500ms", now), Ok(now - 500_000_000));
        // 지금보다 먼 과거는 0에서 멈춘다.
        assert_eq!(time_ns("1h", 5), Ok(0));
        for bad in [
            "",
            "yesterday",
            "10x",
            "m",
            "1.",
            ".5",
            "2026-11-10",
            "2026-11-10T13:20:14",
            "2026-02-30T00:00:00Z",
            "2026-11-10T24:00:00Z",
            "2026-11-10T13:20:14.Z",
            "2026-11-10T13:20:14+0900",
            "1969-12-31T23:59:59Z",
        ] {
            assert!(time_ns(bad, now).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn env_files_follow_docker() {
        let v = env_file("\u{feff}# c\n\n  A=1 \nB=\"q\"\nHOME\nC=x=y\r\n").unwrap();
        assert_eq!(v, ["A=1 ", "B=\"q\"", "HOME", "C=x=y"]);
        assert!(env_file("=1").unwrap_err().contains("line 1"));
        assert!(env_file("ok=1\nA B=1").unwrap_err().contains("line 2"));
    }

    #[test]
    fn env_values() {
        let host = |k: &str| (k == "HOME").then(|| "/home/me".to_string());
        let v: Vec<String> = ["A=1", "B=", "HOME", "MISSING", "C=x=y"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            env(&v, host),
            vec![
                ("A".into(), "1".into()),
                ("B".into(), "".into()),
                ("HOME".into(), "/home/me".into()),
                ("C".into(), "x=y".into()),
            ]
        );
    }

    #[test]
    fn memory_units() {
        assert_eq!(memory_mib("512m"), Ok(512));
        assert_eq!(memory_mib("1g"), Ok(1024));
        assert_eq!(memory_mib("1G"), Ok(1024));
        assert_eq!(memory_mib("2048k"), Ok(2));
        assert_eq!(memory_mib("1048576"), Ok(1));
        assert!(memory_mib("1500k").is_err());
        assert!(memory_mib("0m").is_err());
        assert!(memory_mib("lots").is_err());
    }

    #[test]
    fn cpus_must_be_whole() {
        assert_eq!(cpus("2"), Ok(2));
        assert!(cpus("0").is_err());
        assert!(cpus("1.5").is_err());
    }

    #[test]
    fn platforms() {
        assert_eq!(platform("linux/arm64/v8"), Ok(Arch::Aarch64));
        assert!(platform("windows/amd64").is_err());
    }

    fn folder(a: ParseResult<Attach>) -> Mount {
        match a {
            Ok(Attach::Folder(m)) => m,
            other => panic!("{other:?}"),
        }
    }

    fn named_volume(a: ParseResult<Attach>) -> VolumeMount {
        match a {
            Ok(Attach::Volume(v)) => v,
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn volumes() {
        assert_eq!(
            folder(volume("/data:/work:ro")),
            Mount {
                source: "/data".into(),
                destination: "/work".into(),
                kind: MountKind::Bind,
                readonly: true
            }
        );
        assert!(!folder(volume("/data:/work")).readonly);
        assert!(volume("/data:work").is_err());
        // 이름이면 볼륨이다(Docker 규칙).
        assert_eq!(
            named_volume(volume("pgdata:/var/lib/postgresql/data")),
            VolumeMount {
                name: "pgdata".into(),
                destination: "/var/lib/postgresql/data".into(),
                readonly: false,
                no_copy: false,
            }
        );
        let v = named_volume(volume("cache:/c:ro,nocopy"));
        assert!(v.readonly && v.no_copy);
        assert!(volume("a/b:/c").unwrap_err().contains("absolute host path"));
        assert!(volume("./src:/c").is_err());
        assert!(volume("x:/c").unwrap_err().contains("volume name"));
        assert!(volume("pg:/c:ro,rw").unwrap_err().contains("both"));
        assert!(
            volume("/data:/c:nocopy")
                .unwrap_err()
                .contains("only for volumes")
        );
        // 이름 없는 볼륨은 거부한다.
        assert!(volume("/data").unwrap_err().contains("anonymous"));
        assert!(volume("/data:ro").unwrap_err().contains("anonymous"));
        assert!(volume("pgdata").is_err());
    }

    #[test]
    fn volume_names_are_told_from_paths_like_docker() {
        for name in ["pgdata", "db.v2", "C"] {
            assert!(is_volume_name(name), "{name}");
        }
        for path in ["/data", "./src", "../x", "~/x", "a/b", "C:\\work", "c:", ""] {
            assert!(!is_volume_name(path), "{path}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn mounts() {
        let m = folder(mount("type=copy,src=/in,dst=/work,readonly"));
        assert_eq!(
            (m.kind, m.source, m.destination.as_str(), m.readonly),
            (MountKind::Copy, PathBuf::from("/in"), "/work", true)
        );
        assert_eq!(
            folder(mount("type=bind,source=/a,target=/b")).kind,
            MountKind::Bind
        );
        let v = named_volume(mount(
            "type=volume,source=pg,target=/d,readonly,volume-nocopy",
        ));
        assert_eq!((v.name.as_str(), v.destination.as_str()), ("pg", "/d"));
        assert!(v.readonly && v.no_copy);
        assert!(
            mount("type=volume,src=/a,dst=/b")
                .unwrap_err()
                .contains("volume name")
        );
        assert!(
            mount("type=volume,dst=/b")
                .unwrap_err()
                .contains("anonymous")
        );
        assert!(mount("type=volume,src=pg,dst=/b,volume-driver=x").is_err());
        assert!(mount("type=bind,src=/a,dst=/b,volume-nocopy").is_err());
        assert!(mount("src=/a,dst=/b").is_err());
        assert!(mount("type=bind,src=/a,dst=/b,foo=1").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cp_endpoints() {
        assert_eq!(
            cp_endpoint("abc:/etc/hosts"),
            CpEndpoint::Sandbox {
                id: "abc".into(),
                path: "/etc/hosts".into()
            }
        );
        // 이름도 된다.
        assert_eq!(
            cp_endpoint("App.v2_x:out"),
            CpEndpoint::Sandbox {
                id: "App.v2_x".into(),
                path: "out".into()
            }
        );
        assert_eq!(cp_endpoint("/tmp/x"), CpEndpoint::Host("/tmp/x".into()));
        assert_eq!(cp_endpoint("./a:b"), CpEndpoint::Host("./a:b".into()));
        assert_eq!(cp_endpoint("a b:x"), CpEndpoint::Host("a b:x".into()));
        assert_eq!(cp_endpoint("a/b:c"), CpEndpoint::Host("a/b:c".into()));
    }

    #[test]
    fn storage_opt_sets_the_writable_disk_size() {
        assert_eq!(storage_opt("size=20g"), Ok(20 << 10));
        assert_eq!(storage_opt("size=20G"), Ok(20 << 10));
        assert_eq!(storage_opt("size=20GB"), Ok(20 << 10));
        assert_eq!(storage_opt("size=512m"), Ok(512));
        assert_eq!(storage_opt("size=1t"), Ok(1 << 20));
        assert!(storage_opt("size=1m").unwrap_err().contains("at least"));
        assert!(storage_opt("size=x").is_err());
        assert!(
            storage_opt("dm.basesize=1g")
                .unwrap_err()
                .contains("unknown")
        );
        assert!(storage_opt("20g").is_err());
    }

    #[test]
    fn lifetimes_read_like_durations() {
        assert_eq!(transfer_bytes("1000"), Ok(1000));
        assert_eq!(transfer_bytes("500m"), Ok(500 << 20));
        assert_eq!(transfer_bytes("2GB"), Ok(2 << 30));
        for bad in ["0", "", "m", "1.5g", "-1", "99999999999t"] {
            assert!(transfer_bytes(bad).is_err(), "{bad}");
        }
        assert_eq!(lifetime_secs("600"), Ok(600));
        assert_eq!(lifetime_secs("30m"), Ok(1800));
        assert_eq!(lifetime_secs("1h30m"), Ok(5400));
        assert_eq!(lifetime_secs("1.5s"), Ok(1));
        assert!(lifetime_secs("0").is_err());
        assert!(lifetime_secs("500ms").is_err());
        assert!(lifetime_secs("soon").is_err());
    }
}
