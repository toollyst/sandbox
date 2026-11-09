//! 기본 위치. 옵션(`--data-dir`, `--cache-dir`, `--socket-dir`, `--guest-init`)이나 같은 이름의 `SBX_*` 환경 변수가 우선한다.
//!
//! | OS | data_dir | cache_dir | socket_dir |
//! |---|---|---|---|
//! | macOS | `~/Library/Application Support/sbx` | `~/Library/Caches/sbx` | `/tmp/sbx-$USER` |
//! | Linux | `$XDG_DATA_HOME/sbx` 또는 `~/.local/share/sbx` | `$XDG_CACHE_HOME/sbx` 또는 `~/.cache/sbx` | `$XDG_RUNTIME_DIR/sbx` 또는 `/tmp/sbx-$USER` |
//! | Windows | `%LOCALAPPDATA%\sbx\data` | `%LOCALAPPDATA%\sbx\cache` | `%LOCALAPPDATA%\sbx\run` |
//!
//! socket_dir은 Unix 소켓 경로 길이 제한 때문에 data_dir 밖의 짧은 경로다.
//! guest_init 기본값은 `<cache_dir>/guest/<게스트 프로토콜 버전>/<amd64|arm64>/initramfs`다.
//! 파일은 `cargo xtask guest-initramfs --arch <x86_64|aarch64>`로 만든다.

use std::env;
use std::path::PathBuf;

fn home() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

fn xdg(var: &str, fallback: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home().map(|h| h.join(fallback)))
}

/// 기본 data_dir.
pub fn data_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support/sbx"))
    } else if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("sbx").join("data"))
    } else {
        xdg("XDG_DATA_HOME", ".local/share").map(|p| p.join("sbx"))
    }
}

/// 기본 cache_dir.
pub fn cache_dir() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Caches/sbx"))
    } else if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("sbx").join("cache"))
    } else {
        xdg("XDG_CACHE_HOME", ".cache").map(|p| p.join("sbx"))
    }
}

/// 기본 socket_dir.
pub fn socket_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("sbx").join("run"));
    }
    if cfg!(target_os = "linux")
        && let Some(run) = env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())
    {
        return Some(PathBuf::from(run).join("sbx"));
    }
    let user = env::var("USER").unwrap_or_else(|_| "user".into());
    Some(PathBuf::from(format!("/tmp/sbx-{user}")))
}
