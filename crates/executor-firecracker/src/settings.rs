//! firecracker 실행 파일과 게스트 커널을 찾는다. 순서는 크레이트 문서의 표와 같다. 가장 아래 모듈이다.

use std::path::{Path, PathBuf};

/// firecracker 실행 파일을 가리키는 환경 변수.
pub const ENV_FIRECRACKER: &str = "SBX_FIRECRACKER";
/// 게스트 커널을 가리키는 환경 변수.
pub const ENV_KERNEL: &str = "SBX_FIRECRACKER_KERNEL";
/// 이 실행 파일 옆에 둘 때의 커널 파일 이름.
pub const BUNDLED_KERNEL: &str = "kernel";

/// 찾은 것.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// firecracker 실행 파일.
    pub firecracker: PathBuf,
    /// 게스트 커널.
    pub kernel: PathBuf,
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
        )
    }

    /// [`Settings::locate`]의 규칙. 환경, 이 실행 파일의 디렉터리, PATH 디렉터리를 받는다.
    pub fn locate_in(
        env: impl Fn(&str) -> Option<PathBuf>,
        beside: Option<&Path>,
        path: &[PathBuf],
    ) -> Result<Self, String> {
        let exe = format!("firecracker{}", std::env::consts::EXE_SUFFIX);
        let firecracker = match env(ENV_FIRECRACKER) {
            Some(p) => existing(p, ENV_FIRECRACKER)?,
            None => beside
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
        Ok(Self {
            firecracker,
            kernel,
        })
    }
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
        let e = Settings::locate_in(none, Some(&bundle), std::slice::from_ref(&bin)).unwrap_err();
        assert!(e.contains(ENV_KERNEL), "{e}");
        // PATH의 firecracker, 옆의 커널.
        std::fs::write(bundle.join(BUNDLED_KERNEL), b"").unwrap();
        let s = Settings::locate_in(none, Some(&bundle), std::slice::from_ref(&bin)).unwrap();
        assert_eq!(s.firecracker, bin.join(&exe));
        assert_eq!(s.kernel, bundle.join(BUNDLED_KERNEL));
        // 옆의 firecracker가 PATH보다 먼저.
        std::fs::write(bundle.join(&exe), b"").unwrap();
        let s = Settings::locate_in(none, Some(&bundle), std::slice::from_ref(&bin)).unwrap();
        assert_eq!(s.firecracker, bundle.join(&exe));
        // 환경 변수가 가장 먼저. 없는 파일이면 오류.
        let k = root.join("k");
        std::fs::write(&k, b"").unwrap();
        let env = |v: &str| (v == ENV_KERNEL).then(|| k.clone());
        assert_eq!(
            Settings::locate_in(env, Some(&bundle), &[]).unwrap().kernel,
            k
        );
        let env = |v: &str| (v == ENV_FIRECRACKER).then(|| root.join("missing"));
        assert!(Settings::locate_in(env, Some(&bundle), &[]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
