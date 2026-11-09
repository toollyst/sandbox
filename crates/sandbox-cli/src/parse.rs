//! 옵션 값 해석. Docker와 같은 문법을 받는다. 다른 점은 [`DOCKER_DIFFERENCES`](crate::behavior::DOCKER_DIFFERENCES)에 있다.

use std::path::{Path, PathBuf};

use sandbox_model::{Arch, Mount, MountKind, SandboxId};

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

/// `--cpus`. VM의 vCPU 개수라서 1 이상의 정수만 받는다.
pub fn cpus(value: &str) -> ParseResult<u8> {
    match value.parse::<u8>() {
        Ok(n) if n >= 1 => Ok(n),
        _ => Err(format!(
            "--cpus must be a whole number of vCPUs (1-255): {value:?}"
        )),
    }
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
            "mount source must be an absolute host path (named volumes are not supported): {whole:?}"
        ))
    }
}

fn guest_path(dst: &str, whole: &str) -> ParseResult<String> {
    if dst.starts_with('/') {
        Ok(dst.to_owned())
    } else {
        Err(format!("mount target must be an absolute path: {whole:?}"))
    }
}

/// `-v SRC:DST[:ro|rw]`. `--mount type=bind`의 줄임말이다.
///
/// 뒤에서부터 나눈다. 그래서 Windows 경로(`C:\work:/work`)도 받는다.
pub fn volume(value: &str) -> ParseResult<Mount> {
    let (rest, readonly) = match value.rsplit_once(':') {
        Some((rest, "ro")) => (rest, true),
        Some((rest, "rw")) => (rest, false),
        _ => (value, false),
    };
    let (src, dst) = rest
        .rsplit_once(':')
        .ok_or_else(|| format!("expected SRC:DST[:ro]: {value:?}"))?;
    Ok(Mount {
        source: host_path(src, value)?,
        destination: guest_path(dst, value)?,
        kind: MountKind::Bind,
        readonly,
    })
}

/// `--mount type=bind|copy,source=...,target=...[,readonly]`.
///
/// 키 이름은 Docker와 같다: `source`/`src`, `target`/`destination`/`dst`, `readonly`/`ro`(값 없이 또는 `true`/`false`).
pub fn mount(value: &str) -> ParseResult<Mount> {
    let (mut kind, mut src, mut dst, mut readonly) = (None, None, None, false);
    for field in value.split(',') {
        let (k, v) = match field.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (field, None),
        };
        match (k, v) {
            ("type", Some("bind")) => kind = Some(MountKind::Bind),
            ("type", Some("copy")) => kind = Some(MountKind::Copy),
            ("type", Some(t)) => {
                return Err(format!("unsupported mount type {t:?} (bind or copy)"));
            }
            ("source" | "src", Some(s)) => src = Some(s),
            ("target" | "destination" | "dst", Some(d)) => dst = Some(d),
            ("readonly" | "ro", None | Some("true" | "1")) => readonly = true,
            ("readonly" | "ro", Some("false" | "0")) => readonly = false,
            _ => return Err(format!("unknown mount option {field:?}")),
        }
    }
    Ok(Mount {
        source: host_path(
            src.ok_or_else(|| format!("mount needs source=: {value:?}"))?,
            value,
        )?,
        destination: guest_path(
            dst.ok_or_else(|| format!("mount needs target=: {value:?}"))?,
            value,
        )?,
        kind: kind.ok_or_else(|| format!("mount needs type=bind or type=copy: {value:?}"))?,
        readonly,
    })
}

/// `cp`의 한쪽 끝.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CpEndpoint {
    /// 호스트 경로.
    Host(PathBuf),
    /// 샌드박스 안의 경로.
    Sandbox {
        /// 샌드박스.
        id: SandboxId,
        /// 게스트 경로.
        path: String,
    },
}

/// `cp`의 인자 하나. 절대 경로이거나 `.`으로 시작하면 호스트, `ID:경로`이면 샌드박스다(Docker와 같다).
pub fn cp_endpoint(value: &str) -> CpEndpoint {
    if Path::new(value).is_absolute() || value.starts_with('.') {
        return CpEndpoint::Host(value.into());
    }
    match value.split_once(':') {
        Some((id, path)) => match SandboxId::new(id) {
            Ok(id) => CpEndpoint::Sandbox {
                id,
                path: path.to_owned(),
            },
            Err(_) => CpEndpoint::Host(value.into()),
        },
        None => CpEndpoint::Host(value.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(unix)]
    #[test]
    fn volumes() {
        assert_eq!(
            volume("/data:/work:ro"),
            Ok(Mount {
                source: "/data".into(),
                destination: "/work".into(),
                kind: MountKind::Bind,
                readonly: true
            })
        );
        assert!(!volume("/data:/work").unwrap().readonly);
        assert!(volume("named:/work").is_err());
        assert!(volume("/data:work").is_err());
        assert!(volume("/data").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn mounts() {
        let m = mount("type=copy,src=/in,dst=/work,readonly").unwrap();
        assert_eq!(
            (m.kind, m.source, m.destination.as_str(), m.readonly),
            (MountKind::Copy, PathBuf::from("/in"), "/work", true)
        );
        assert_eq!(
            mount("type=bind,source=/a,target=/b").unwrap().kind,
            MountKind::Bind
        );
        assert!(mount("type=volume,src=/a,dst=/b").is_err());
        assert!(mount("src=/a,dst=/b").is_err());
        assert!(mount("type=bind,src=/a,dst=/b,foo=1").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cp_endpoints() {
        assert_eq!(
            cp_endpoint("abc:/etc/hosts"),
            CpEndpoint::Sandbox {
                id: SandboxId::new("abc").unwrap(),
                path: "/etc/hosts".into()
            }
        );
        assert_eq!(cp_endpoint("/tmp/x"), CpEndpoint::Host("/tmp/x".into()));
        assert_eq!(cp_endpoint("./a:b"), CpEndpoint::Host("./a:b".into()));
        assert_eq!(cp_endpoint("Not_Id:x"), CpEndpoint::Host("Not_Id:x".into()));
    }
}
