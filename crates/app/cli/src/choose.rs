//! 어느 실행기를 쓸지 고른다. nvm, gvm처럼 전역 기본, 폴더 파일, 명령마다 지정, 샌드박스마다 고정이 있다.
//!
//! | 순서 | 무엇 | 새 샌드박스 (`run`, `create`) | 있는 샌드박스 (`start`, `restart`) |
//! |---|---|---|---|
//! | 1 | `--executor`(`SBX_EXECUTOR`): 실행 파일 경로, 또는 설치한 `NAME[@VERSION]` | ✓ | ✓ |
//! | 2 | 샌드박스에 고정된 것 (라벨 [`PIN_LABEL`]) | | ✓ |
//! | 3 | 폴더 파일 [`VERSION_FILE`]: 지금 폴더부터 위로 처음 만나는 것 | ✓ | ✓ |
//! | 4 | 전역 기본 (`sbx executor use`) | ✓ | ✓ |
//! | 5 | PATH의 `sbx-executor` | ✓ | ✓ |
//!
//! - 설치한 실행기로 만든 샌드박스는 그 `NAME@VERSION`을 라벨 [`PIN_LABEL`]에 적는다. 그래서 기본을 바꿔도 있던
//!   샌드박스는 만든 실행기로 다시 시작한다. 경로나 PATH로 고른 실행기는 적지 않는다(옮겨질 수 있다).
//! - [`VERSION_FILE`]: `#`으로 시작하는 줄과 빈 줄을 뺀 첫 줄이 `NAME[@VERSION]`이다. 설치돼 있어야 한다.
//! - VMM 묶음(어댑터가 있는 설치)은 그 어댑터(설치한 것 중 가장 높은 버전)를 띄우고, 묶음 디렉터리를
//!   `sbx-executor-proto`의 `VMM_DIR_ENV`로 넘긴다.
//! - 고른 것이 설치돼 있지 않으면 무엇을 설치하면 되는지 알린다. 다른 것으로 넘어가지 않는다.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use sbx_executor_install::{Executors, Installed, Spec};

/// 샌드박스를 만든 설치 실행기(`NAME@VERSION`)를 적는 라벨.
pub const PIN_LABEL: &str = "io.github.toollyst.sandbox.executor";

/// 폴더별 실행기 파일.
pub const VERSION_FILE: &str = ".sbx-executor";

/// 어디서 골랐는지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// `--executor`, `SBX_EXECUTOR`.
    Flag,
    /// 샌드박스에 고정된 것.
    Pinned,
    /// 폴더 파일.
    File(PathBuf),
    /// 전역 기본.
    Default,
    /// PATH의 `sbx-executor`.
    Path,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flag => f.write_str("--executor"),
            Self::Pinned => f.write_str("pinned by the sandbox"),
            Self::File(p) => write!(f, "{}", p.display()),
            Self::Default => f.write_str("default (sbx executor use)"),
            Self::Path => f.write_str("sbx-executor on PATH"),
        }
    }
}

/// 고른 실행기.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// 띄울 실행 파일(자체 완결 실행기, 또는 VMM 묶음의 어댑터).
    pub program: PathBuf,
    /// 설치한 실행기(또는 VMM 묶음)이면 그것.
    pub installed: Option<Installed>,
    /// VMM 묶음이면 그것을 돌리는 어댑터.
    pub adapter: Option<Installed>,
    /// 어디서 골랐는지.
    pub source: Source,
}

impl Chosen {
    /// VMM 묶음 디렉터리.
    pub fn vmm_dir(&self) -> Option<&Path> {
        self.adapter
            .as_ref()
            .and(self.installed.as_ref())
            .map(|i| i.dir.as_path())
    }

    /// 실행기 프로세스에 더할 환경 변수.
    pub fn env(&self) -> Vec<(OsString, OsString)> {
        self.vmm_dir()
            .map(|d| {
                vec![(
                    sbx_executor_proto::VMM_DIR_ENV.into(),
                    d.as_os_str().to_owned(),
                )]
            })
            .unwrap_or_default()
    }

    /// 샌드박스에 적을 고정 값.
    pub fn pin(&self) -> Option<String> {
        self.installed.as_ref().map(ToString::to_string)
    }

    /// 사람이 읽는 이름: `NAME@VERSION`이나 실행 파일 경로.
    pub fn label(&self) -> String {
        match &self.installed {
            Some(i) => i.to_string(),
            None => self.program.display().to_string(),
        }
    }
}

/// 고르는 데 쓰는 것.
pub struct Inputs<'a> {
    /// 설치한 실행기.
    pub executors: &'a Executors,
    /// 설치한 어댑터.
    pub adapters: &'a Executors,
    /// `--executor`.
    pub flag: Option<&'a Path>,
    /// 샌드박스에 고정된 것.
    pub pinned: Option<&'a str>,
    /// 폴더 파일을 찾기 시작할 곳.
    pub cwd: Option<&'a Path>,
    /// PATH에 `program`이 있는지.
    pub on_path: &'a dyn Fn(&Path) -> bool,
}

/// `program`(경로 구분자 없는 이름)이 PATH에 있는지. Windows에서는 `.exe`도 본다.
pub fn on_path(program: &std::path::Path) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let p = dir.join(program);
        p.is_file() || (cfg!(windows) && p.with_extension("exe").is_file())
    })
}

type Result<T> = std::result::Result<T, String>;

/// 고른다. 순서는 모듈 문서에 있다. 아무것도 없으면 `Ok(None)`.
pub fn choose(i: &Inputs<'_>) -> Result<Option<Chosen>> {
    if let Some(flag) = i.flag {
        return flag_choice(i, flag).map(Some);
    }
    if let Some(pin) = i.pinned {
        let spec: Spec = pin.parse().map_err(|e| format!("label {PIN_LABEL}: {e}"))?;
        let found = i.executors.exact(&spec).map_err(|e| e.to_string())?;
        let Some(found) = found else {
            return Err(format!(
                "this sandbox was created with executor {spec}, which is no longer installed; \
                 install it again (sbx executor install {spec}) or choose one with --executor"
            ));
        };
        return with_adapter(i, found, Source::Pinned).map(Some);
    }
    if let Some((file, spec)) = version_file(i.cwd)? {
        let found = i.executors.find(&spec).map_err(|e| e.to_string())?;
        let Some(found) = found else {
            return Err(format!(
                "{} asks for executor {spec}, which is not installed; install it with: \
                 sbx executor install {spec}",
                file.display()
            ));
        };
        return with_adapter(i, found, Source::File(file)).map(Some);
    }
    if let Some(d) = i.executors.default().map_err(|e| e.to_string())? {
        return with_adapter(i, d, Source::Default).map(Some);
    }
    let fallback = PathBuf::from(sbx_executor_install::executable_name());
    if (i.on_path)(&fallback) {
        return Ok(Some(Chosen {
            program: fallback,
            installed: None,
            adapter: None,
            source: Source::Path,
        }));
    }
    Ok(None)
}

/// `--executor`: 경로처럼 보이거나 파일이 있으면 실행 파일, 아니면 설치한 `NAME[@VERSION]`.
fn flag_choice(i: &Inputs<'_>, flag: &Path) -> Result<Chosen> {
    let program = || Chosen {
        program: flag.to_owned(),
        installed: None,
        adapter: None,
        source: Source::Flag,
    };
    let text = flag.to_string_lossy();
    let looks_like_path = text.contains(std::path::MAIN_SEPARATOR) || text.contains('/');
    if looks_like_path || flag.is_file() {
        return Ok(program());
    }
    let Ok(spec) = text.parse::<Spec>() else {
        return Ok(program());
    };
    if let Some(found) = i.executors.find(&spec).map_err(|e| e.to_string())? {
        return with_adapter(i, found, Source::Flag);
    }
    // 설치한 이름이 아니면 PATH의 프로그램 이름일 수 있다.
    if spec.version.is_none() && (i.on_path)(flag) {
        return Ok(program());
    }
    Err(format!(
        "executor {spec} is not installed; install it with: sbx executor install {spec}"
    ))
}

fn with_adapter(i: &Inputs<'_>, found: Installed, source: Source) -> Result<Chosen> {
    let Some(name) = found.adapter.clone() else {
        return Ok(Chosen {
            program: found.executable.clone(),
            installed: Some(found),
            adapter: None,
            source,
        });
    };
    let spec = Spec {
        name: name.clone(),
        version: None,
    };
    let adapter = i.adapters.find(&spec).map_err(|e| e.to_string())?;
    let Some(adapter) = adapter else {
        return Err(format!(
            "executor {found} needs adapter {name}, which is not installed; install it with: \
             sbx executor adapter install {name}"
        ));
    };
    Ok(Chosen {
        program: adapter.executable.clone(),
        installed: Some(found),
        adapter: Some(adapter),
        source,
    })
}

/// `cwd`부터 위로 처음 만나는 [`VERSION_FILE`]과 그 안의 실행기.
pub fn version_file(cwd: Option<&Path>) -> Result<Option<(PathBuf, Spec)>> {
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    for dir in cwd.ancestors() {
        let file = dir.join(VERSION_FILE);
        let text = match std::fs::read_to_string(&file) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            // 디렉터리 같은 것이면 파일이 아니다.
            Err(_) if !file.is_file() => continue,
            Err(e) => return Err(format!("{}: {e}", file.display())),
        };
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or_else(|| format!("{}: names no executor", file.display()))?;
        let spec = line
            .parse::<Spec>()
            .map_err(|e| format!("{}: {e}", file.display()))?;
        return Ok(Some((file, spec)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct T {
        root: PathBuf,
        ex: Executors,
        ad: Executors,
    }

    impl T {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("sbx-choose-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src")).unwrap();
            Self {
                ex: Executors::new(root.join("executors")),
                ad: Executors::new(root.join("adapters")),
                root,
            }
        }

        fn bundle(&self, file: &str) -> PathBuf {
            let d = self.root.join("src").join(file);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(file), "x").unwrap();
            std::fs::write(d.join(sbx_executor_install::executable_name()), "x").unwrap();
            d
        }

        fn add(&self, ex: &Executors, spec: &str, adapter: Option<&str>) {
            let src = match adapter {
                Some(_) => {
                    let d = self.root.join("src").join(format!("vmm-{spec}"));
                    std::fs::create_dir_all(&d).unwrap();
                    std::fs::write(d.join("firecracker"), "vmm").unwrap();
                    d
                }
                None => self.bundle(&spec.replace('@', "-")),
            };
            ex.install(&spec.parse().unwrap(), &src, adapter, |_| Ok(()))
                .unwrap();
        }

        fn inputs<'a>(
            &'a self,
            flag: Option<&'a Path>,
            pinned: Option<&'a str>,
            cwd: Option<&'a Path>,
        ) -> Inputs<'a> {
            Inputs {
                executors: &self.ex,
                adapters: &self.ad,
                flag,
                pinned,
                cwd,
                on_path: &|p| p == Path::new("on-path"),
            }
        }
    }

    impl Drop for T {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn label(c: Result<Option<Chosen>>) -> String {
        c.unwrap().map(|c| c.label()).unwrap_or_default()
    }

    #[test]
    fn order_is_flag_pin_file_default_path() {
        let t = T::new("order");
        assert_eq!(choose(&t.inputs(None, None, None)), Ok(None));
        t.add(&t.ex, "solo@1.0", None);
        t.add(&t.ex, "solo@2.0", None);
        // 처음 설치한 것이 기본이 된다.
        assert_eq!(label(choose(&t.inputs(None, None, None))), "solo@1.0");
        let work = t.root.join("work/deep");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(
            t.root.join("work").join(VERSION_FILE),
            "# project\n\nsolo@latest\n",
        )
        .unwrap();
        let c = choose(&t.inputs(None, None, Some(&work))).unwrap().unwrap();
        assert_eq!(c.label(), "solo@2.0");
        assert_eq!(
            c.source,
            Source::File(t.root.join("work").join(VERSION_FILE))
        );
        let c = choose(&t.inputs(None, Some("solo@1.0"), Some(&work)))
            .unwrap()
            .unwrap();
        assert_eq!((c.label(), &c.source), ("solo@1.0".into(), &Source::Pinned));
        assert_eq!(c.pin().as_deref(), Some("solo@1.0"));
        let f = Path::new("solo@2.0");
        let c = choose(&t.inputs(Some(f), Some("solo@1.0"), Some(&work)))
            .unwrap()
            .unwrap();
        assert_eq!((c.label(), c.source), ("solo@2.0".into(), Source::Flag));
    }

    #[test]
    fn flag_paths_and_programs() {
        let t = T::new("flag");
        let p = Path::new("/opt/x/sbx-executor");
        let c = choose(&t.inputs(Some(p), None, None)).unwrap().unwrap();
        assert_eq!((c.program.as_path(), c.pin()), (p, None));
        let c = choose(&t.inputs(Some(Path::new("on-path")), None, None))
            .unwrap()
            .unwrap();
        assert_eq!(c.program, Path::new("on-path"));
        let e = choose(&t.inputs(Some(Path::new("wick@1.18.0")), None, None)).unwrap_err();
        assert!(e.contains("sbx executor install wick@1.18.0"), "{e}");
    }

    #[test]
    fn missing_pins_and_files_are_errors_not_fallbacks() {
        let t = T::new("missing");
        t.add(&t.ex, "solo@1.0", None);
        let e = choose(&t.inputs(None, Some("gone@3"), None)).unwrap_err();
        assert!(e.contains("no longer installed"), "{e}");
        std::fs::write(t.root.join(VERSION_FILE), "wick@9\n").unwrap();
        let e = choose(&t.inputs(None, None, Some(&t.root))).unwrap_err();
        assert!(e.contains("sbx executor install wick@9"), "{e}");
        std::fs::write(t.root.join(VERSION_FILE), "# nothing\n").unwrap();
        assert!(choose(&t.inputs(None, None, Some(&t.root))).is_err());
    }

    #[test]
    fn vmm_packages_run_through_their_adapter() {
        let t = T::new("vmm");
        t.add(&t.ex, "wick@1.18.0", Some("firecracker"));
        let e = choose(&t.inputs(None, None, None)).unwrap_err();
        assert!(
            e.contains("sbx executor adapter install firecracker"),
            "{e}"
        );
        t.add(&t.ad, "firecracker@0.1.0", None);
        t.add(&t.ad, "firecracker@0.2.0", None);
        let c = choose(&t.inputs(None, None, None)).unwrap().unwrap();
        assert_eq!(c.label(), "wick@1.18.0");
        assert_eq!(c.adapter.as_ref().unwrap().to_string(), "firecracker@0.2.0");
        assert_eq!(c.program, c.adapter.as_ref().unwrap().executable);
        let dir = t.root.join("executors").join("wick").join("1.18.0");
        assert_eq!(c.vmm_dir(), Some(dir.as_path()));
        assert_eq!(
            c.env(),
            vec![(
                OsString::from(sbx_executor_proto::VMM_DIR_ENV),
                dir.into_os_string()
            )]
        );
    }
}
