//! firecracker 실행 파일, 게스트 커널, jailer를 찾는다. 순서는 크레이트 문서의 표와 같다. 가장 아래 모듈이다.

use std::path::{Path, PathBuf};

/// firecracker 실행 파일을 가리키는 환경 변수.
pub const ENV_FIRECRACKER: &str = "SBX_FIRECRACKER";
/// 게스트 커널을 가리키는 환경 변수.
pub const ENV_KERNEL: &str = "SBX_FIRECRACKER_KERNEL";
/// 이 실행 파일 옆에 둘 때의 커널 파일 이름.
pub const BUNDLED_KERNEL: &str = "kernel";
/// jailer를 가리키거나(`off`면 쓰지 않는다) 하는 환경 변수.
pub const ENV_JAILER: &str = "SBX_FIRECRACKER_JAILER";
/// jailer를 끄는 값.
pub const JAILER_OFF: &str = "off";

/// 찾은 것.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// firecracker 실행 파일.
    pub firecracker: PathBuf,
    /// 게스트 커널.
    pub kernel: PathBuf,
    /// firecracker를 가둘 jailer. root일 때만 있다.
    pub jailer: Option<PathBuf>,
}

impl Settings {
    /// 이 프로세스의 환경과 실행 파일 위치로 찾는다.
    pub fn locate() -> Result<Self, String> {
        let beside = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let path = std::env::var_os("PATH").unwrap_or_default();
        Self::locate_in(
            |k| std::env::var_os(k).map(PathBuf::from),
            beside.as_deref(),
            &std::env::split_paths(&path).collect::<Vec<_>>(),
            is_root(),
        )
    }

    /// [`Settings::locate`]의 규칙. 환경, 이 실행 파일의 디렉터리, PATH 디렉터리를 받는다.
    pub fn locate_in(
        env: impl Fn(&str) -> Option<PathBuf>,
        beside: Option<&Path>,
        path: &[PathBuf],
        root: bool,
    ) -> Result<Self, String> {
        let exe = format!("firecracker{}", std::env::consts::EXE_SUFFIX);
        // sbx가 VMM 묶음과 함께 띄웠으면(sbx_executor_proto::VMM_DIR_ENV) 그 안의 firecracker.
        let in_vmm_dir = env(sbx_executor_proto::VMM_DIR_ENV).map(|d| d.join(&exe));
        let firecracker = match (env(ENV_FIRECRACKER), in_vmm_dir) {
            (Some(p), _) => existing(p, ENV_FIRECRACKER)?,
            (None, Some(p)) => existing(p, sbx_executor_proto::VMM_DIR_ENV)?,
            (None, None) => beside
                .map(|d| d.join(&exe))
                .filter(|p| p.is_file())
                .or_else(|| path.iter().map(|d| d.join(&exe)).find(|p| p.is_file()))
                .ok_or_else(|| {
                    format!("firecracker not found (set {ENV_FIRECRACKER}, or put it next to this executor or on PATH)")
                })?,
        };
        let kernel = match env(ENV_KERNEL) {
            Some(p) => existing(p, ENV_KERNEL)?,
            None => beside
                .map(|d| d.join(BUNDLED_KERNEL))
                .filter(|p| p.is_file())
                .ok_or_else(|| {
                    format!("guest kernel not found (set {ENV_KERNEL}, or put a file named {BUNDLED_KERNEL} next to this executor)")
                })?,
        };
        let jailer = jailer(&env, &firecracker, root)?;
        Ok(Self {
            firecracker,
            kernel,
            jailer,
        })
    }
}

/// root로 돌면 firecracker를 jailer로 가둔다. jailer는 `SBX_FIRECRACKER_JAILER`, 없으면 고른 firecracker와 같은
/// 디렉터리의 `jailer`다(jailer는 같은 버전의 firecracker와 써야 한다). 찾지 못하면 root인 채로 firecracker를
/// 돌리지 않고 실패한다. `SBX_FIRECRACKER_JAILER=off`면 쓰지 않는다. root가 아니면 쓰지 않는다(jailer는 root가
/// 필요하다).
fn jailer(
    env: &impl Fn(&str) -> Option<PathBuf>,
    firecracker: &Path,
    root: bool,
) -> Result<Option<PathBuf>, String> {
    let set = env(ENV_JAILER);
    if set.as_deref() == Some(Path::new(JAILER_OFF)) {
        return Ok(None);
    }
    if !cfg!(target_os = "linux") {
        return match set {
            Some(_) => Err(format!("{ENV_JAILER}: the jailer runs only on Linux")),
            None => Ok(None),
        };
    }
    match (set, root) {
        (Some(_), false) => Err(format!(
            "{ENV_JAILER} is set, but the jailer needs root (set {ENV_JAILER}={JAILER_OFF} or run as root)"
        )),
        (Some(p), true) => existing(p, ENV_JAILER).map(Some),
        (None, false) => Ok(None),
        (None, true) => {
            let p = firecracker.with_file_name(format!("jailer{}", std::env::consts::EXE_SUFFIX));
            if p.is_file() {
                Ok(Some(p))
            } else {
                Err(format!(
                    "running as root, but there is no jailer next to {} to confine firecracker; install a VMM \
                     package that ships the jailer, point {ENV_JAILER} at one, or set {ENV_JAILER}={JAILER_OFF} to run \
                     firecracker as root without it",
                    firecracker.display()
                ))
            }
        }
    }
}

#[cfg(unix)]
fn is_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    // /proc/self의 소유자는 이 프로세스의 실효 uid다(Linux). 다른 곳에서는 jailer를 쓰지 않으니 상관없다.
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

fn existing(p: PathBuf, var: &str) -> Result<PathBuf, String> {
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("{var}={}: no such file", p.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_then_bundle_then_path() {
        let root = std::env::temp_dir().join(format!("sbx-fc-settings-{}", std::process::id()));
        let (bundle, bin) = (root.join("bundle"), root.join("bin"));
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let exe = format!("firecracker{}", std::env::consts::EXE_SUFFIX);
        std::fs::write(bin.join(&exe), b"").unwrap();
        let none = |_: &str| None;

        // 커널이 없으면 오류.
        let e = Settings::locate_in(none, Some(&bundle), std::slice::from_ref(&bin), false)
            .unwrap_err();
        assert!(e.contains(ENV_KERNEL), "{e}");
        // PATH의 firecracker, 옆의 커널.
        std::fs::write(bundle.join(BUNDLED_KERNEL), b"").unwrap();
        let s =
            Settings::locate_in(none, Some(&bundle), std::slice::from_ref(&bin), false).unwrap();
        assert_eq!(s.firecracker, bin.join(&exe));
        assert_eq!(s.kernel, bundle.join(BUNDLED_KERNEL));
        // 옆의 firecracker가 PATH보다 먼저.
        std::fs::write(bundle.join(&exe), b"").unwrap();
        let s =
            Settings::locate_in(none, Some(&bundle), std::slice::from_ref(&bin), false).unwrap();
        assert_eq!(s.firecracker, bundle.join(&exe));
        // 환경 변수가 가장 먼저. 없는 파일이면 오류.
        let k = root.join("k");
        std::fs::write(&k, b"").unwrap();
        let env = |v: &str| (v == ENV_KERNEL).then(|| k.clone());
        assert_eq!(
            Settings::locate_in(env, Some(&bundle), &[], false)
                .unwrap()
                .kernel,
            k
        );
        let env = |v: &str| (v == ENV_FIRECRACKER).then(|| root.join("missing"));
        assert!(Settings::locate_in(env, Some(&bundle), &[], false).is_err());
        // sbx가 준 VMM 묶음 안의 firecracker가 옆의 것보다 먼저. 묶음에 없으면 오류(옆으로 넘어가지 않는다).
        let vmm = root.join("vmm");
        std::fs::create_dir_all(&vmm).unwrap();
        let env = |v: &str| (v == sbx_executor_proto::VMM_DIR_ENV).then(|| vmm.clone());
        assert!(Settings::locate_in(env, Some(&bundle), &[], false).is_err());
        std::fs::write(vmm.join(&exe), b"").unwrap();
        let s = Settings::locate_in(env, Some(&bundle), &[], false).unwrap();
        assert_eq!(
            (s.firecracker, s.kernel),
            (vmm.join(&exe), bundle.join(BUNDLED_KERNEL))
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_uses_the_jailer_beside_firecracker_or_refuses() {
        let root = std::env::temp_dir().join(format!("sbx-fc-jailer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fc = root.join("firecracker");
        std::fs::write(&fc, b"").unwrap();
        let none = |_: &str| None;
        let off = |v: &str| (v == ENV_JAILER).then(|| PathBuf::from(JAILER_OFF));
        // root가 아니면 쓰지 않는다. off면 root여도 쓰지 않는다.
        assert_eq!(jailer(&none, &fc, false), Ok(None));
        assert_eq!(jailer(&off, &fc, true), Ok(None));
        if cfg!(target_os = "linux") {
            // root인데 옆에 jailer가 없으면 실패하고, 있으면 그것을 쓴다.
            assert!(jailer(&none, &fc, true).unwrap_err().contains(ENV_JAILER));
            std::fs::write(root.join("jailer"), b"").unwrap();
            assert_eq!(jailer(&none, &fc, true), Ok(Some(root.join("jailer"))));
            // 가리킨 jailer는 root가 아니면 오류다.
            let set = |v: &str| (v == ENV_JAILER).then(|| root.join("jailer"));
            assert!(jailer(&set, &fc, false).is_err());
        } else {
            assert_eq!(jailer(&none, &fc, true), Ok(None));
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
