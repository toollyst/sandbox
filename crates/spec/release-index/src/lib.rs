//! sbx 릴리스 색인: 어떤 `sbx` 버전이 나왔는지, 어느 것을 거뒀는지, 묶음이 어디 있는지 알리는 서명한 파일 하나.
//! 시작할 때 새 버전 알림과 `sbx upgrade`가 읽는다. 기획은 `docs/plan/upgrade.md`.
//!
//! # 색인 (schema 1)
//!
//! `releases.json`과 그 minisign 서명 `releases.json.minisig`(같은 디렉터리). 공식 색인은
//! [`OFFICIAL_URL`]이고, release.yml의 publish가 릴리스마다 항목을 더해 서명한다.
//!
//! ```json
//! {
//!   "schema": 1,
//!   "serial": 1763000000,
//!   "expires": 1765600000,
//!   "releases": [
//!     {
//!       "version": "0.2.2",
//!       "security": false,
//!       "yanked": false,
//!       "advisory": null,
//!       "layout_version": 1,
//!       "protocol": { "min": 2, "max": 2 },
//!       "assets": {
//!         "linux-x86_64": {
//!           "url": "https://github.com/toollyst/sandbox/releases/download/v0.2.2/sbx-linux-x86_64.tar.gz",
//!           "sha256": "<압축 파일의 SHA-256, 소문자 16진수 64자>",
//!           "size": 4718592
//!         }
//!       }
//!     }
//!   ]
//! }
//! ```
//!
//! | 필드 | 뜻 |
//! |---|---|
//! | `schema` | 형식 버전. 이 클라이언트는 [`index::SCHEMA`]만 읽는다. 다른 값이면 색인 전체를 거부한다(옛 `sbx`는 알림을 건너뛴다). |
//! | `serial` | 서명할 때마다 커지는 수. 본 것보다 작으면 거부한다(옛 색인을 다시 내미는 롤백). |
//! | `expires` | 이 색인을 믿는 마지막 시각(Unix 초). 지나면 거부한다(새 색인을 감추는 동결). |
//! | `version` | 정식 릴리스 버전 `MAJOR.MINOR.PATCH`([`Version`]). 겹치면 안 된다. |
//! | `security` | (선택, 기본 `false`) 그 버전이 보안 고침을 담는다. |
//! | `yanked` | (선택, 기본 `false`) 거둔 버전. 알림과 `upgrade`의 기본 목표에서 빠진다. |
//! | `advisory` | (선택) 그 버전에 대한 알림 한 줄. 제어 문자를 빼고 512자에서 자른다(실행기 저장소와 같다). |
//! | `layout_version` | (선택) 그 버전이 쓰는 데이터·캐시 디렉터리 배치 버전. 낮은 버전으로 내려갈 때 본다. |
//! | `protocol` | (선택) 그 버전이 말하는 실행기 규약 범위 `min..=max`. |
//! | `assets` | 플랫폼(`<os>-<arch>`, 예: `linux-x86_64`, `darwin-aarch64`, `windows-x86_64`) → gzip tar 묶음. |
//! | `url`, `sha256`, `size` | 묶음의 URL(절대이거나 색인 기준 상대), SHA-256, 바이트 수. 묶음 옆 `<url>.minisig`가 그 서명이다. |
//!
//! 모르는 필드는 무시한다. 규약에 맞지 않는 곳이 하나라도 있으면 색인 전체를 거부한다.
//!
//! **믿는 근거.** 색인과 묶음 모두 공식 minisign 키 중 하나의 서명이 맞아야 한다([`fetch()`], [`download()`]). 키는 `sbx` 안에만 있다.
//! URL 규칙(`https`, `file`, loopback `http`, redirect)과 서명 확인, 본 `serial` 기록은 실행기 저장소
//! (`sbx-executor-repo`)의 것을 그대로 쓴다.

pub mod fetch;
pub mod index;
pub mod select;

pub use fetch::{Source, download, fetch};
pub use index::{Asset, Index, Release, Version};
pub use select::{Status, status, target};

/// 공식 릴리스 색인.
pub const OFFICIAL_URL: &str = "https://toollyst.github.io/sandbox/releases.json";

/// 이 호스트의 플랫폼 이름(`assets`의 키). 릴리스 묶음 이름(`sbx-<플랫폼>.tar.gz`)과 같다.
pub fn host_platform() -> String {
    platform(std::env::consts::OS, std::env::consts::ARCH)
}

/// Rust의 OS·아키텍처 이름으로 만든 플랫폼 이름. macOS는 `darwin`이다.
pub fn platform(os: &str, arch: &str) -> String {
    let os = if os == "macos" { "darwin" } else { os };
    format!("{os}-{arch}")
}

#[cfg(test)]
mod tests;
