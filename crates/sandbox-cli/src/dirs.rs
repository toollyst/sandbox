//! data_dir과 cache_dir 기본 위치. `--data-dir`/`--cache-dir`(또는 `SBX_DATA_DIR`/`SBX_CACHE_DIR`)이 우선한다.
//!
//! | OS | data_dir | cache_dir |
//! |---|---|---|
//! | macOS | `~/Library/Application Support/sbx` | `~/Library/Caches/sbx` |
//! | Linux | `$XDG_DATA_HOME/sbx` 또는 `~/.local/share/sbx` | `$XDG_CACHE_HOME/sbx` 또는 `~/.cache/sbx` |
//! | Windows | `%LOCALAPPDATA%\sbx\data` | `%LOCALAPPDATA%\sbx\cache` |

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
