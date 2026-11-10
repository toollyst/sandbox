//! 설치된 실행기 관리. `sbx executor ...`가 쓴다. 실행 코어와 관리 코어는 이것을 모른다
//! (어떤 실행기를 쓸지는 바이너리가 정한다).
//!
//! ```text
//! <root>/                       보통 <data_dir>/executors
//!   <이름>/<버전>/              실행기 묶음 하나. 둘 중 하나다.
//!                               - 자체 완결: 맨 위에 실행 파일 sbx-executor(Windows는 .exe)가 있다.
//!                                 나머지 파일(예: VMM, 커널)은 실행기가 자기 옆에서 찾는다.
//!                               - VMM 묶음: sbx-executor가 없고, 설치할 때 정한 어댑터 이름이
//!                                 .sbx-adapter에 한 줄로 있다. 어댑터가 이 디렉터리를 받아 VMM을 찾는다
//!                                 (sbx-executor-proto의 VMM_DIR_ENV).
//!   default                     기본 실행기 "<이름>@<버전>" 한 줄
//!   .staging-*                  설치 중인 임시 디렉터리 (목록에 나오지 않는다)
//! ```
//!
//! **규칙**
//!
//! - 이름과 버전은 `[A-Za-z0-9._+-]`이고 `.`으로 시작하지 않는다. 표기는 `<이름>@<버전>`.
//! - 설치([`Executors::install`])는 임시 디렉터리에 복사하고, 호출자의 확인(예: `capabilities`와 규약 버전)을
//!   통과한 뒤 rename한다. 그래서 반쯤 설치된 실행기는 없다. 같은 이름과 버전이 이미 있으면 거부한다.
//!   기본 실행기가 없으면 설치한 것이 기본이 된다.
//! - 버전을 빼고 이름만 주면 가장 높은 버전이다([`version_cmp`]: 숫자 조각은 숫자로 비교).
//!   `<이름>@latest`도 같다([`LATEST`]). 그래서 `latest`라는 버전은 설치할 수 없다.
//! - 버전은 **주 버전**과 **패치**로 읽는다: 첫 `-` 앞이 주 버전, 뒤가 패치다(`1.18.0-wick.2`는 주 버전 `1.18.0`의 패치
//!   `wick.2`). 버전을 주면 그 버전이 정확히 있을 때 그것, 없으면 그 버전으로 시작하는 것(바로 뒤가 `.`, `-`, `+`) 중
//!   가장 높은 것이다([`version_matches`]). 그래서 `wick@1.18.0`은 1.18.0의 가장 새 패치, `wick@1.18`은 1.18.x 전체에서
//!   가장 높은 것이다(Python의 `pyenv install 3.12`와 같다). 지우기([`Executors::remove`])는 정확한 버전만 받는다.
//! - 지우면([`Executors::remove`]) 그것이 기본이었을 때 기본도 지운다(다른 것을 고르지 않는다).
//! - 어댑터도 같은 배치로 다른 `<root>`에 둔다(보통 `<data_dir>/adapters`). 어댑터는 자체 완결 묶음이다.
//!
//! - 의존: 없음 (std만)

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

/// 버전 자리의 이 말은 "가장 높은 버전"이다(버전을 뺀 것과 같다).
pub const LATEST: &str = "latest";

/// 묶음 안의 실행 파일 이름.
pub fn executable_name() -> String {
    format!("sbx-executor{}", std::env::consts::EXE_SUFFIX)
}

/// `<이름>[@<버전>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// 이름.
    pub name: String,
    /// 버전. 없으면 가장 높은 버전.
    pub version: Option<String>,
}

fn valid_part(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
}

impl FromStr for Spec {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let (name, version) = match s.split_once('@') {
            Some((n, v)) => (n, Some(v)),
            None => (s, None),
        };
        if !valid_part(name) || version.is_some_and(|v| !valid_part(v)) {
            return Err(format!(
                "invalid executor {s:?} (expected NAME[@VERSION] with letters, digits, . _ + -)"
            ));
        }
        Ok(Self {
            name: name.to_owned(),
            version: version.filter(|v| *v != LATEST).map(str::to_owned),
        })
    }
}

impl fmt::Display for Spec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.version {
            Some(v) => write!(f, "{}@{v}", self.name),
            None => f.write_str(&self.name),
        }
    }
}

/// 설치된 실행기 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// 이름.
    pub name: String,
    /// 버전.
    pub version: String,
    /// 묶음 디렉터리.
    pub dir: PathBuf,
    /// 실행 파일. VMM 묶음([`Installed::adapter`]가 있음)에는 없다.
    pub executable: PathBuf,
    /// VMM 묶음이면 함께 쓸 어댑터 이름.
    pub adapter: Option<String>,
}

impl Installed {
    /// `NAME@VERSION`.
    pub fn spec(&self) -> Spec {
        Spec {
            name: self.name.clone(),
            version: Some(self.version.clone()),
        }
    }
}

impl fmt::Display for Installed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.version)
    }
}

/// 설치 실패.
#[derive(Debug)]
pub enum InstallError {
    /// 같은 이름과 버전이 이미 있다.
    AlreadyInstalled(String),
    /// 버전 없이 설치하려 했다.
    NoVersion,
    /// 묶음에 실행 파일이 없다(VMM 묶음으로 설치하려면 어댑터를 정해야 한다).
    NoExecutable(PathBuf),
    /// 어댑터 이름이 규칙에 맞지 않는다.
    BadAdapter(String),
    /// 호출자의 확인을 통과하지 못했다.
    Rejected(String),
    /// 입출력.
    Io(io::Error),
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallError::AlreadyInstalled(s) => write!(f, "{s} is already installed"),
            InstallError::NoVersion => f.write_str("a version is required (NAME@VERSION)"),
            InstallError::NoExecutable(d) => write!(
                f,
                "{} has no {} at its top level (for a VMM without one, name the adapter that drives it)",
                d.display(),
                executable_name()
            ),
            InstallError::BadAdapter(a) => write!(f, "invalid adapter name {a:?}"),
            InstallError::Rejected(m) => f.write_str(m),
            InstallError::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for InstallError {}

impl From<io::Error> for InstallError {
    fn from(e: io::Error) -> Self {
        InstallError::Io(e)
    }
}

/// `want`가 `have`를 가리키는지: 같거나, `have`가 `want`로 시작하고 바로 뒤가 `.`, `-`, `+`이다.
/// `1.18.0`은 `1.18.0-wick.2`를, `1.18`은 `1.18.3`을 가리킨다. `1.1`은 `1.18`을 가리키지 않는다.
pub fn version_matches(want: &str, have: &str) -> bool {
    have == want
        || have
            .strip_prefix(want)
            .is_some_and(|rest| rest.starts_with(['.', '-', '+']))
}

/// 주 버전: 첫 `-` 앞(`1.18.0-wick.2` → `1.18.0`). `-`가 없으면 버전 그대로.
pub fn version_line(v: &str) -> &str {
    v.split_once('-').map_or(v, |(line, _)| line)
}

/// 버전 비교. 숫자 조각은 숫자로, 나머지는 문자열로 비교한다(`1.10` > `1.9`).
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for ((da, xa), (db, xb)) in ca.iter().zip(&cb) {
        let o = if *da && *db {
            let (ta, tb) = (xa.trim_start_matches('0'), xb.trim_start_matches('0'));
            ta.len().cmp(&tb.len()).then(ta.cmp(tb))
        } else {
            xa.cmp(xb)
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    ca.len().cmp(&cb.len())
}

/// 설치된 실행기들.
#[derive(Debug, Clone)]
pub struct Executors {
    root: PathBuf,
}

static STAGING: AtomicU64 = AtomicU64::new(0);

impl Executors {
    /// `root` 아래를 쓴다. 없어도 된다(설치할 때 만든다).
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn installed(&self, name: &str, version: &str) -> Installed {
        let dir = self.root.join(name).join(version);
        Installed {
            name: name.to_owned(),
            version: version.to_owned(),
            executable: dir.join(executable_name()),
            adapter: read_adapter(&dir),
            dir,
        }
    }

    /// 모두. 이름 순, 같은 이름은 버전 순.
    pub fn list(&self) -> io::Result<Vec<Installed>> {
        let mut out = Vec::new();
        let names = match fs::read_dir(&self.root) {
            Ok(r) => r,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for n in names {
            let n = n?;
            let name = n.file_name().to_string_lossy().into_owned();
            if !valid_part(&name) || !n.file_type()?.is_dir() {
                continue;
            }
            for v in fs::read_dir(n.path())? {
                let version = v?.file_name().to_string_lossy().into_owned();
                if valid_part(&version) {
                    out.push(self.installed(&name, &version));
                }
            }
        }
        out.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(version_cmp(&a.version, &b.version))
        });
        Ok(out)
    }

    /// 찾는다. 버전이 없으면 가장 높은 버전. 버전이 정확히 없으면 그 버전으로 시작하는 것 중 가장 높은 것
    /// ([`version_matches`]).
    pub fn find(&self, spec: &Spec) -> io::Result<Option<Installed>> {
        let list: Vec<Installed> = self
            .list()?
            .into_iter()
            .filter(|i| i.name == spec.name)
            .collect();
        Ok(match &spec.version {
            None => list.into_iter().next_back(),
            Some(v) => match list.iter().position(|i| i.version == *v) {
                Some(n) => list.into_iter().nth(n),
                None => list.into_iter().rfind(|i| version_matches(v, &i.version)),
            },
        })
    }

    /// 정확히 그 버전.
    fn exact(&self, spec: &Spec) -> io::Result<Option<Installed>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|i| i.name == spec.name && spec.version.as_deref() == Some(i.version.as_str())))
    }

    /// 기본 실행기. 정하지 않았거나 그것이 지워졌으면 `None`.
    pub fn default(&self) -> io::Result<Option<Installed>> {
        let text = match fs::read_to_string(self.root.join("default")) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let Ok(spec) = text.trim().parse::<Spec>() else {
            return Ok(None);
        };
        match spec.version {
            Some(_) => self.find(&spec),
            None => Ok(None),
        }
    }

    /// 기본 실행기를 정한다. 설치되어 있어야 한다.
    pub fn set_default(&self, spec: &Spec) -> io::Result<Installed> {
        let found = self.find(spec)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("{spec} is not installed"))
        })?;
        fs::create_dir_all(&self.root)?;
        let tmp = self.root.join(format!(".default-{}", std::process::id()));
        fs::write(&tmp, format!("{found}\n"))?;
        fs::rename(&tmp, self.root.join("default"))?;
        Ok(found)
    }

    /// `src`(실행 파일 하나 또는 묶음 디렉터리)를 `spec`으로 설치한다. 규칙은 모듈 문서에 있다.
    ///
    /// `adapter`가 있으면 VMM 묶음으로 설치한다(`sbx-executor`가 없어도 된다. 실행 파일 하나면 그 이름 그대로 둔다).
    /// `verify`는 임시 위치에 놓인 것([`Installed`], 경로는 임시 위치)을 받는다.
    pub fn install(
        &self,
        spec: &Spec,
        src: &Path,
        adapter: Option<&str>,
        verify: impl FnOnce(&Installed) -> Result<(), String>,
    ) -> Result<Installed, InstallError> {
        let version = spec.version.as_deref().ok_or(InstallError::NoVersion)?;
        if let Some(a) = adapter
            && !valid_part(a)
        {
            return Err(InstallError::BadAdapter(a.to_owned()));
        }
        let target = self.installed(&spec.name, version);
        if target.dir.exists() {
            return Err(InstallError::AlreadyInstalled(target.to_string()));
        }
        fs::create_dir_all(&self.root)?;
        let staging = self.root.join(format!(
            ".staging-{}-{}",
            std::process::id(),
            STAGING.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        let result = (|| {
            if src.is_dir() {
                copy_dir(src, &staging)?;
            } else {
                fs::create_dir(&staging)?;
                let name: std::ffi::OsString = match adapter {
                    // VMM 실행 파일 하나: 어댑터가 찾을 이름 그대로 둔다.
                    Some(_) => src
                        .file_name()
                        .map(|n| n.to_os_string())
                        .unwrap_or_else(|| "vmm".into()),
                    None => executable_name().into(),
                };
                let to = staging.join(&name);
                fs::copy(src, &to)?;
                make_executable(&to)?;
            }
            let exe = staging.join(executable_name());
            match adapter {
                Some(a) => fs::write(staging.join(ADAPTER_FILE), format!("{a}\n"))?,
                None if !exe.is_file() => return Err(InstallError::NoExecutable(src.to_owned())),
                None => make_executable(&exe)?,
            }
            let staged = Installed {
                name: spec.name.clone(),
                version: version.to_owned(),
                executable: exe.clone(),
                adapter: adapter.map(str::to_owned),
                dir: staging.clone(),
            };
            verify(&staged).map_err(InstallError::Rejected)?;
            fs::create_dir_all(self.root.join(&spec.name))?;
            fs::rename(&staging, &target.dir)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        result?;
        let target = self.installed(&spec.name, version);
        if self.default()?.is_none() {
            self.set_default(&Spec {
                name: target.name.clone(),
                version: Some(target.version.clone()),
            })?;
        }
        Ok(target)
    }

    /// 지운다. 기본이었으면 기본도 지운다. 지운 것을 돌려준다.
    pub fn remove(&self, spec: &Spec) -> io::Result<Installed> {
        let found = match &spec.version {
            Some(_) => self.exact(spec)?,
            None => None,
        }
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{spec} is not installed (give NAME@VERSION)"),
            )
        })?;
        if self.default()?.is_some_and(|d| d == found) {
            fs::remove_file(self.root.join("default"))?;
        }
        fs::remove_dir_all(&found.dir)?;
        let _ = fs::remove_dir(self.root.join(&found.name));
        Ok(found)
    }
}

/// VMM 묶음에 어댑터 이름을 적는 파일.
pub const ADAPTER_FILE: &str = ".sbx-adapter";

fn read_adapter(dir: &Path) -> Option<String> {
    let text = fs::read_to_string(dir.join(ADAPTER_FILE)).ok()?;
    let name = text.trim();
    valid_part(name).then(|| name.to_owned())
}

fn copy_dir(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let (from, to) = (e.path(), dst.join(e.file_name()));
        let t = e.file_type()?;
        if t.is_dir() {
            copy_dir(&from, &to)?;
        } else if t.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&from)?, &to)?;
            #[cfg(not(unix))]
            fs::copy(&from, &to).map(|_| ())?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

fn make_executable(p: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = fs::metadata(p)?.permissions();
        perm.set_mode(perm.mode() | 0o755);
        fs::set_permissions(p, perm)?;
    }
    #[cfg(not(unix))]
    let _ = p;
    Ok(())
}

#[cfg(test)]
mod tests;
