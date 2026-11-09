//! 경로. 다른 모듈이 쓰는 가장 아래 모듈이다.

use std::path::{Path, PathBuf};

/// cargo의 빌드 디렉터리: `CARGO_TARGET_DIR`(상대 경로면 `root` 기준), 없으면 `<root>/target`.
pub fn target_dir(root: &Path) -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) => root.join(d),
        None => root.join("target"),
    }
}
