//! 플랫폼 이름. OCI image-spec의 `os`/`architecture` 값(Go의 GOOS/GOARCH)을 쓴다. 가장 아래 모듈 중 하나다.

use std::fmt;

/// 실행기가 도는 호스트 플랫폼.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Platform {
    /// `linux`, `darwin`, `windows`, ...
    pub os: String,
    /// `amd64`, `arm64`, ...
    pub architecture: String,
}

impl Platform {
    /// 이 프로세스가 도는 플랫폼.
    pub fn host() -> Self {
        Self::from_rust(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// Rust의 `std::env::consts::{OS, ARCH}` 값에서. 모르는 값은 그대로 쓴다.
    pub fn from_rust(os: &str, arch: &str) -> Self {
        let os = match os {
            "macos" => "darwin",
            o => o,
        };
        let architecture = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            "x86" => "386",
            "powerpc64" => "ppc64",
            "loongarch64" => "loong64",
            a => a,
        };
        Self {
            os: os.into(),
            architecture: architecture.into(),
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.os, self.architecture)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oci_names() {
        assert_eq!(
            Platform::from_rust("macos", "aarch64").to_string(),
            "darwin/arm64"
        );
        assert_eq!(
            Platform::from_rust("linux", "x86_64").to_string(),
            "linux/amd64"
        );
        assert_eq!(
            Platform::from_rust("linux", "riscv64").to_string(),
            "linux/riscv64"
        );
    }
}
