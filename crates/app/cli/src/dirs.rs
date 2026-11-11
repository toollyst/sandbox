//! 기본 위치. 옵션(`--data-dir`, `--cache-dir`, `--socket-dir`, `--guest-init`)이나 같은 이름의 `SBX_*` 환경 변수가 우선한다.
//!
//! | OS | data_dir | cache_dir | socket_dir |
//! |---|---|---|---|
//! | macOS | `~/Library/Application Support/sbx` | `~/Library/Caches/sbx` | `/tmp/sbx-$USER` |
//! | Linux | `$XDG_DATA_HOME/sbx` 또는 `~/.local/share/sbx` | `$XDG_CACHE_HOME/sbx` 또는 `~/.cache/sbx` | `$XDG_RUNTIME_DIR/sbx` 또는 `/tmp/sbx-$USER` |
//! | Windows | `%LOCALAPPDATA%\sbx\data` | `%LOCALAPPDATA%\sbx\cache` | `%LOCALAPPDATA%\sbx\run` |
//!
//! socket_dir은 Unix 소켓 경로 길이 제한 때문에 data_dir 밖의 짧은 경로다.
//! guest_init 기본값: sbx에 내장된 것(배포 빌드, 캐시의 `guest/sha256-<hex>/<arch>/initramfs`로 꺼낸다).
//! 내장하지 않은 빌드면 `<cache_dir>/guest/<게스트 프로토콜 버전>/<amd64|arm64>/initramfs`이고,
//! 그 파일은 `cargo xtask guest-initramfs --arch <x86_64|aarch64>`로 만든다.

use std::env;
use std::path::{Path, PathBuf};

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

/// 레지스트리 자격 증명 파일(공개 형식, [`sbx_oci::auth`]). 읽는 순서대로:
///
/// 1. `REGISTRY_AUTH_FILE`(containers-auth.json). 있으면 `sbx login`도 여기에 쓴다(podman과 같다).
/// 2. Docker: `$DOCKER_CONFIG/config.json`, 없으면 `~/.docker/config.json`(Windows는 `%USERPROFILE%`). login의 기본.
/// 3. containers: `$XDG_RUNTIME_DIR/containers/auth.json`(Linux), `$XDG_CONFIG_HOME/containers/auth.json` 또는
///    `~/.config/containers/auth.json`.
pub fn registry_auth() -> sbx_oci::auth::AuthFiles {
    let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let user_home = home().or_else(|| var("USERPROFILE"));
    let docker = var("DOCKER_CONFIG")
        .or_else(|| user_home.as_ref().map(|h| h.join(".docker")))
        .map(|d| d.join("config.json"));
    let explicit = var("REGISTRY_AUTH_FILE");
    let mut read: Vec<PathBuf> = explicit.iter().chain(docker.iter()).cloned().collect();
    if cfg!(target_os = "linux")
        && let Some(run) = var("XDG_RUNTIME_DIR")
    {
        read.push(run.join("containers/auth.json"));
    }
    if let Some(c) = var("XDG_CONFIG_HOME").or_else(|| user_home.map(|h| h.join(".config"))) {
        read.push(c.join("containers/auth.json"));
    }
    sbx_oci::auth::AuthFiles {
        read,
        write: explicit.or(docker),
    }
}

/// `registries.conf`(미러, 이름 바꾸기, 차단). containers/image와 같은 자리와 순서다:
///
/// - 주 파일: `CONTAINERS_REGISTRIES_CONF`, 없으면 `~/.config/containers/registries.conf`(있으면), 없으면
///   `/etc/containers/registries.conf`.
/// - drop-in(`registries.conf.d`): 사용자 주 파일을 골랐으면 사용자 것만, 아니면 `/etc` 것 다음 사용자 것.
/// - Windows에는 `/etc` 자리가 없다.
pub fn registries_conf() -> sbx_oci::registries::ConfFiles {
    let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let user = home().map(|h| h.join(".config/containers"));
    let system = (!cfg!(windows)).then(|| PathBuf::from("/etc/containers"));
    let user_main = user.as_ref().map(|u| u.join("registries.conf"));
    let explicit = var("CONTAINERS_REGISTRIES_CONF");
    let use_user = explicit.is_none() && user_main.as_ref().is_some_and(|p| p.is_file());
    let main = explicit.or_else(|| {
        if use_user {
            user_main.clone()
        } else {
            system.as_ref().map(|s| s.join("registries.conf"))
        }
    });
    let mut dropin_dirs = Vec::new();
    if !use_user && let Some(s) = &system {
        dropin_dirs.push(s.join("registries.conf.d"));
    }
    if let Some(u) = &user {
        dropin_dirs.push(u.join("registries.conf.d"));
    }
    sbx_oci::registries::ConfFiles { main, dropin_dirs }
}

/// 서명 정책 `policy.json`: `~/.config/containers/policy.json`(있으면), 없으면 `/etc/containers/policy.json`
/// (containers/image와 같은 자리). 둘 다 없으면 `None`이고 Docker처럼 모두 받는다.
pub fn signature_policy() -> Option<PathBuf> {
    let user = home().map(|h| h.join(".config/containers/policy.json"));
    let system = (!cfg!(windows)).then(|| PathBuf::from("/etc/containers/policy.json"));
    user.into_iter().chain(system).find(|p| p.is_file())
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

/// 실행기 저장소 목록 파일: `<data_dir>/executor-repositories`.
pub fn repositories_file(data_dir: &Path) -> PathBuf {
    data_dir.join("executor-repositories")
}

/// 설치한 어댑터를 두는 곳: `<data_dir>/adapters`.
pub fn adapters_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("adapters")
}

/// 설치한 실행기를 두는 곳: `<data_dir>/executors`.
pub fn executors_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("executors")
}
