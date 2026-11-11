//! 셸 자동완성. 정적인 부분(명령, 옵션, 정해진 값)은 clap 정의에서, 동적인 부분(샌드박스, 이미지, 실행기)은
//! data_dir과 cache_dir에서 그때그때 읽는다.
//!
//! 셸 쪽 스크립트는 [`registration`]이 쓴다(`sbx completion <셸>`). 그 스크립트는 탭을 누를 때마다
//! `COMPLETE=<셸> sbx -- <지금까지의 단어들>`로 sbx를 부르고, `main`이 맨 먼저 [`clap_complete::CompleteEnv`]로
//! 그 요청을 받아 후보를 찍고 끝낸다(명령은 실행하지 않는다). `COMPLETE=<셸> sbx`(단어 없이)도 같은 스크립트를 쓴다.
//!
//! **명시적 동작**
//!
//! | 인자 | 후보 |
//! |---|---|
//! | `start`, `stop`, `restart`, `kill`, `wait`, `rm`, `attach`, `inspect`, `logs`의 샌드박스, `exec`의 첫 인자 | [`sandboxes`] |
//! | `cp`의 양쪽 | [`cp_endpoints`]: `이름:`·`ID:`와 호스트 경로 |
//! | `pull`, `prepare`, `rmi`의 이미지, `run`·`create`의 첫 인자 | [`images`] |
//! | `executor use`, `executor rm` | [`executors`] |
//!
//! - 후보는 친 앞부분으로 시작하는 것만이다. 순서는 이름이 먼저, 그다음 ID다.
//! - 디렉터리: 이미 친 `--data-dir`/`--cache-dir`(`--data-dir X`와 `--data-dir=X`, 여러 번이면 마지막) >
//!   `SBX_DATA_DIR`/`SBX_CACHE_DIR`(비어 있으면 없는 것) > [`dirs`]의 기본값. 명령의 인자로 넘어갈
//!   자리(`run IMAGE` 뒤)에 같은 모양이 있어도 옵션으로 본다.
//! - 부작용 없음: 읽기만 한다. 실행기를 띄우지 않고, 디렉터리나 파일(소유 잠금 포함)을 만들거나 쓰지 않는다
//!   ([`sbx_runtime::sandbox_names`]). 읽지 못하면 그 부분의 후보 없이 끝난다(오류를 찍지 않는다).
//! - 샌드박스를 상태로 거르지 않는다(`stop`에도 멈춘 것이 나온다). 상태를 알려면 소유 잠금을 건드려야 하기 때문이다.
//! - `cp`에서 `:` 뒤(게스트 경로)는 완성하지 않는다. 게스트 안을 읽으려면 VM이 필요하기 때문이다.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use clap_complete::engine::{ArgValueCompleter, ValueCompleter};
use clap_complete::{CompletionCandidate, PathCompleter};

use crate::behavior::familiar;
use crate::dirs;
use crate::parse::{CpEndpoint, cp_endpoint};

/// 셸이 sbx를 다시 부를 때 쓰는 환경 변수 (`COMPLETE=<셸> sbx`). clap_complete의 기본값이다.
pub const ENV_VAR: &str = "COMPLETE";

/// 후보를 읽을 디렉터리. 정할 수 없으면 `None`(그 후보는 없다).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Dirs {
    /// 샌드박스와 설치한 실행기.
    pub data_dir: Option<PathBuf>,
    /// 이미지.
    pub cache_dir: Option<PathBuf>,
}

impl Dirs {
    /// 셸이 넘긴 단어들(`sbx`부터)과 환경 변수에서. 우선순위는 모듈 문서에 있다.
    pub fn from_words(words: &[OsString], env: impl Fn(&str) -> Option<OsString>) -> Self {
        let pick = |option: &str, var: &str, default: fn() -> Option<PathBuf>| {
            typed(words, option)
                .or_else(|| env(var).filter(|v| !v.is_empty()).map(PathBuf::from))
                .or_else(default)
        };
        Self {
            data_dir: pick("--data-dir", "SBX_DATA_DIR", dirs::data_dir),
            cache_dir: pick("--cache-dir", "SBX_CACHE_DIR", dirs::cache_dir),
        }
    }

    /// 지금 프로세스에서: 인자는 `sbx -- <단어들>`이다(clap_complete의 셸 스크립트가 그렇게 부른다).
    /// 완성할 자리 뒤에 친 옵션은 보지 않는다.
    fn current() -> Self {
        Self::from_words(&shell_words(), |k| std::env::var_os(k))
    }
}

/// 단어들에서 `option`의 값. 마지막 것.
fn typed(words: &[OsString], option: &str) -> Option<PathBuf> {
    let mut found = None;
    let mut it = words.iter();
    while let Some(w) = it.next() {
        let Some(w) = w.to_str() else { continue };
        if w == option {
            found = it.next().cloned();
        } else if let Some(v) = w.strip_prefix(option).and_then(|r| r.strip_prefix('=')) {
            found = Some(v.into());
        }
    }
    found.filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn keep(value: &str, current: &str) -> bool {
    value.starts_with(current)
}

/// 샌드박스 후보: 이름(설명은 짧은 ID), 짧은 ID(12자, 설명은 이름). 친 것이 12자 이상이면 전체 ID.
pub fn sandboxes(data_dir: &Path, current: &str) -> Vec<CompletionCandidate> {
    let list = sbx_runtime::sandbox_names(data_dir).unwrap_or_default();
    let short = |id: &str| id[..id.len().min(12)].to_owned();
    let mut out: Vec<CompletionCandidate> = list
        .iter()
        .filter_map(|(id, name)| {
            let name = name.as_deref()?;
            keep(name, current).then(|| {
                CompletionCandidate::new(name)
                    .help(Some(format!("sandbox {}", short(id.as_str())).into()))
            })
        })
        .collect();
    for (id, name) in &list {
        let value = if current.len() >= 12 {
            id.as_str().to_owned()
        } else {
            short(id.as_str())
        };
        if keep(&value, current) {
            out.push(CompletionCandidate::new(value).help(name.clone().map(Into::into)));
        }
    }
    out
}

/// 이미지 후보: 받아 둔 참조를 `images`처럼 줄인 것(`alpine:3.20`, 설명은 이미지 ID 앞 12자).
/// 줄인 모양이 친 것과 맞지 않고 원래 모양이 맞으면(`docker.io/library/al`) 원래 모양.
pub fn images(cache_dir: &Path, current: &str) -> Vec<CompletionCandidate> {
    let refs = sbx_store::Store::open(cache_dir).refs().unwrap_or_default();
    let mut out = Vec::new();
    for (reference, id) in &refs {
        let short = familiar(reference);
        let value = if keep(short, current) {
            short
        } else if keep(reference, current) {
            reference.as_str()
        } else {
            continue;
        };
        let hex = id.digest().hex();
        let hex = hex[..hex.len().min(12)].to_owned();
        out.push(CompletionCandidate::new(value).help(Some(hex.into())));
    }
    out
}

/// 설치한 실행기 후보: `이름`(한 번), `이름@주버전`(패치가 있는 주 버전마다 한 번), `이름@버전`.
pub fn executors(data_dir: &Path, current: &str) -> Vec<CompletionCandidate> {
    installed_in(&dirs::executors_dir(data_dir), current)
}

/// 설치한 어댑터 후보. 모양은 [`executors`]와 같다.
pub fn adapters(data_dir: &Path, current: &str) -> Vec<CompletionCandidate> {
    installed_in(&dirs::adapters_dir(data_dir), current)
}

/// 저장소 이름 후보(`--repo`). 목록 파일이 없으면 기본(`official`)이다.
pub fn repositories(data_dir: &Path, current: &str) -> Vec<CompletionCandidate> {
    let file = dirs::repositories_file(data_dir);
    let names: Vec<String> = if file.exists() {
        sbx_executor_repo::Sources::new(file)
            .list()
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.name)
            .collect()
    } else {
        vec!["official".into()]
    };
    names
        .into_iter()
        .filter(|n| keep(n, current))
        .map(CompletionCandidate::new)
        .collect()
}

fn installed_in(root: &Path, current: &str) -> Vec<CompletionCandidate> {
    let list = sbx_executor_install::Executors::new(root)
        .list()
        .unwrap_or_default();
    let mut out: Vec<CompletionCandidate> = Vec::new();
    for i in &list {
        if keep(&i.name, current) && !out.iter().any(|c| c.get_value() == i.name.as_str()) {
            out.push(CompletionCandidate::new(&i.name).help(Some("latest installed".into())));
        }
    }
    for i in &list {
        let line = sbx_executor_install::version_line(&i.version);
        let spec = format!("{}@{line}", i.name);
        if line != i.version
            && keep(&spec, current)
            && !out.iter().any(|c| c.get_value() == spec.as_str())
        {
            out.push(CompletionCandidate::new(spec).help(Some("newest installed patch".into())));
        }
    }
    for i in &list {
        let spec = format!("{}@{}", i.name, i.version);
        if keep(&spec, current) {
            out.push(CompletionCandidate::new(spec));
        }
    }
    out
}

/// `cp`의 한쪽 끝 후보. 샌드박스 쪽(`이름:경로`)을 치는 중이면 없다. 아니면 `이름:`·`짧은 ID:`
/// ([`cp_endpoint`]가 호스트로 보는 모양, 즉 절대 경로나 `.`으로 시작하면 빼고)와 호스트 경로.
pub fn cp_endpoints(data_dir: &Path, current: &str) -> Vec<CompletionCandidate> {
    if matches!(cp_endpoint(current), CpEndpoint::Sandbox { .. }) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if !(Path::new(current).is_absolute() || current.starts_with('.')) {
        out.extend(sandboxes(data_dir, current).into_iter().map(|c| {
            let mut value = c.get_value().to_owned();
            value.push(":");
            CompletionCandidate::new(value).help(c.get_help().cloned())
        }));
    }
    out.extend(PathCompleter::any().complete(OsStr::new(current)));
    out
}

/// 여러 값을 받는 인자의 첫 값만 완성할 때: clap 정의 전체와 그 인자의 id.
type FirstOf = (fn() -> clap::Command, &'static str);

/// 후보 함수 하나를 clap 인자의 완성기로.
struct Completer {
    list: fn(&Dirs, &str) -> Vec<CompletionCandidate>,
    /// 여러 값을 받는 인자의 첫 값에서만 쓴다(`run IMAGE COMMAND...`의 IMAGE, `exec SANDBOX COMMAND...`의
    /// SANDBOX): clap 정의와 그 인자의 id.
    first: Option<FirstOf>,
}

impl ValueCompleter for Completer {
    fn complete(&self, current: &OsStr) -> Vec<CompletionCandidate> {
        match current.to_str() {
            Some(current) => (self.list)(&Dirs::current(), current),
            None => Vec::new(),
        }
    }

    fn complete_at(&self, arg_index: usize, current: &OsStr) -> Vec<CompletionCandidate> {
        if let Some((command, id)) = self.first
            && (arg_index > 0 || given_values(command(), id, &shell_words()) > 0)
        {
            return Vec::new();
        }
        self.complete(current)
    }
}

/// 셸이 넘긴 단어들 중 완성할 자리 앞까지(`sbx`부터). 자리는 bash, zsh, elvish가 주는 `_CLAP_COMPLETE_INDEX`,
/// 없으면 마지막 단어다(fish, powershell. clap_complete의 셸 어댑터와 같다).
fn shell_words() -> Vec<OsString> {
    let mut words: Vec<OsString> = std::env::args_os()
        .skip(1)
        .skip_while(|a| a != "--")
        .skip(1)
        .collect();
    let at = std::env::var("_CLAP_COMPLETE_INDEX")
        .ok()
        .and_then(|i| i.parse().ok())
        .unwrap_or(words.len().saturating_sub(1));
    words.truncate(at);
    words
}

/// `before`(완성할 자리 앞의 단어들)를 clap으로 해석했을 때 인자 `id`가 이미 받은 값의 수.
///
/// clap_complete 4.6.11은 여러 값을 받는 위치 인자의 둘째 값에도 `arg_index` 0을 준다. 그래서 첫 값인지는
/// 앞 단어들을 다시 해석해서 정한다. 해석이 아예 안 되면(`--help` 등) 0으로 본다.
pub(crate) fn given_values(command: clap::Command, id: &str, before: &[OsString]) -> usize {
    fn count(m: &clap::ArgMatches, id: &str) -> usize {
        match m.try_get_raw(id) {
            Ok(Some(v)) => v.count(),
            _ => m.subcommand().map_or(0, |(_, s)| count(s, id)),
        }
    }
    command
        .ignore_errors(true)
        .try_get_matches_from(before)
        .map_or(0, |m| count(&m, id))
}

fn completer(
    list: fn(&Dirs, &str) -> Vec<CompletionCandidate>,
    first: Option<FirstOf>,
) -> ArgValueCompleter {
    ArgValueCompleter::new(Completer { list, first })
}

fn in_data(
    f: fn(&Path, &str) -> Vec<CompletionCandidate>,
    d: &Dirs,
    c: &str,
) -> Vec<CompletionCandidate> {
    d.data_dir.as_deref().map(|p| f(p, c)).unwrap_or_default()
}

fn in_cache(d: &Dirs, c: &str) -> Vec<CompletionCandidate> {
    d.cache_dir
        .as_deref()
        .map(|p| images(p, c))
        .unwrap_or_default()
}

/// 샌드박스 인자의 완성기.
pub fn sandbox() -> ArgValueCompleter {
    completer(|d, c| in_data(sandboxes, d, c), None)
}

/// 첫 값이 샌드박스인 인자(`exec`)의 완성기. `command`는 clap 정의 전체, `id`는 그 인자의 id다.
pub fn first_sandbox(command: fn() -> clap::Command, id: &'static str) -> ArgValueCompleter {
    completer(|d, c| in_data(sandboxes, d, c), Some((command, id)))
}

/// 이미지 인자의 완성기.
pub fn image() -> ArgValueCompleter {
    completer(in_cache, None)
}

/// 첫 값이 이미지인 인자(`run`, `create`)의 완성기. 인자는 [`first_sandbox`]와 같다.
pub fn first_image(command: fn() -> clap::Command, id: &'static str) -> ArgValueCompleter {
    completer(in_cache, Some((command, id)))
}

/// `cp` 끝의 완성기.
pub fn cp_endpoint_arg() -> ArgValueCompleter {
    completer(|d, c| in_data(cp_endpoints, d, c), None)
}

/// 설치한 실행기 인자의 완성기.
pub fn executor() -> ArgValueCompleter {
    completer(|d, c| in_data(executors, d, c), None)
}

/// 설치한 어댑터 인자의 완성기.
pub fn adapter() -> ArgValueCompleter {
    completer(|d, c| in_data(adapters, d, c), None)
}

/// 저장소 이름(`--repo`)의 완성기.
pub fn repository() -> ArgValueCompleter {
    completer(|d, c| in_data(repositories, d, c), None)
}

/// `shell`에 등록하는 스크립트를 쓴다. `COMPLETE=<shell> sbx`가 쓰는 것과 같다: 셸이 다시 부를 sbx는
/// 지금 불린 이름(`argv[0]`)이고, 상대 경로(`./sbx`)면 지금 디렉터리를 붙인다.
pub fn registration(
    shell: clap_complete::Shell,
    out: &mut dyn std::io::Write,
) -> std::io::Result<()> {
    let name = shell.to_string();
    let shells = clap_complete::env::Shells::builtins();
    let completer = shells
        .completer(&name)
        .ok_or_else(|| std::io::Error::other(format!("unsupported shell: {name}")))?;
    let mut program = PathBuf::from(std::env::args_os().next().unwrap_or_else(|| "sbx".into()));
    if program.components().count() > 1
        && program.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        program = cwd.join(program);
    }
    completer.write_registration(ENV_VAR, "sbx", "sbx", &program.to_string_lossy(), out)
}

#[cfg(test)]
mod tests {
    use super::*;

    use sbx_model::{
        Digest, Ident, ImageId, NetworkPolicy, ProcessDefaults, Resources, RootfsFormat,
        SandboxSpec, UserSpec,
    };

    fn image_id(byte: &str) -> ImageId {
        ImageId::new(Digest::parse(&format!("sha256:{}", byte.repeat(32))).unwrap())
    }

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sbx-complete-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn values(c: &[CompletionCandidate]) -> Vec<String> {
        c.iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }

    /// 실행 코어가 남겼을 기록: `spec.json`(이름만 의미 있다)과 `state.json`.
    fn plant_sandbox(data: &Path, id: &str, name: Option<&str>) {
        let dir = data.join("sandboxes").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        let spec = SandboxSpec {
            image: image_id("ab"),
            rootfs_format: RootfsFormat::Erofs,
            process: ProcessDefaults {
                args: vec!["sh".into()],
                env: vec![],
                cwd: "/".into(),
                user: UserSpec {
                    user: Ident::Id(0),
                    group: None,
                },
            },
            resources: Resources {
                vcpus: 1,
                memory_mib: 128,
                scratch_disk_mib: 4,
            },
            mounts: vec![],
            network: NetworkPolicy::Disabled,
            hostname: None,
            labels: Default::default(),
            stop: Default::default(),
            open_stdin: false,
            tty: false,
            name: name.map(Into::into),
            lifetime_secs: None,
            ports: Vec::new(),
            egress_limit_bytes: None,
        };
        std::fs::write(dir.join("spec.json"), serde_json::to_vec(&spec).unwrap()).unwrap();
        std::fs::write(dir.join("state.json"), br#"{"state":"stopped"}"#).unwrap();
    }

    #[test]
    fn sandboxes_are_names_then_short_ids() {
        let root = temp("sb");
        let data = root.join("data");
        let (a, b) = ("a1".repeat(32), "b2".repeat(32));
        plant_sandbox(&data, &a, Some("web"));
        plant_sandbox(&data, &b, None);
        assert_eq!(values(&sandboxes(&data, "")), ["web", &a[..12], &b[..12]]);
        assert_eq!(values(&sandboxes(&data, "w")), ["web"]);
        assert_eq!(values(&sandboxes(&data, "b2")), [&b[..12]]);
        // 12자 이상 치면 전체 ID.
        assert_eq!(values(&sandboxes(&data, &a[..14])), [a.as_str()]);
        // 없는 data_dir은 후보가 없고, 만들지도 않는다.
        assert!(sandboxes(&root.join("none"), "").is_empty());
        assert!(!root.join("none").exists());
        // 소유 잠금을 만들지 않는다.
        assert!(!data.join("sandboxes").join(&a).join("owner.lock").exists());
    }

    #[test]
    fn images_are_familiar_references() {
        let root = temp("img");
        let cache = root.join("cache");
        let layout = sbx_store::CacheLayout::new(&cache);
        let id = image_id("cd");
        for r in [
            "docker.io/library/alpine:3.20",
            "docker.io/user/app:1",
            "ghcr.io/o/tool:latest",
        ] {
            sbx_store::write::set_ref(&layout, r, &id).unwrap();
        }
        assert_eq!(sbx_store::Store::open(&cache).refs().unwrap().len(), 3);
        assert_eq!(
            values(&images(&cache, "")),
            ["alpine:3.20", "user/app:1", "ghcr.io/o/tool:latest"]
        );
        assert_eq!(values(&images(&cache, "al")), ["alpine:3.20"]);
        assert_eq!(
            values(&images(&cache, "docker.io/library/")),
            ["docker.io/library/alpine:3.20"]
        );
        assert_eq!(
            images(&cache, "a")[0].get_help().map(|h| h.to_string()),
            Some("cdcdcdcdcdcd".into())
        );
        assert!(images(&root.join("none"), "").is_empty());
    }

    #[test]
    fn cp_endpoints_offer_sandbox_prefixes_and_host_paths() {
        let root = temp("cp");
        let data = root.join("data");
        plant_sandbox(&data, &"c3".repeat(32), Some("box"));
        assert_eq!(values(&cp_endpoints(&data, "bo")), ["box:"]);
        // 샌드박스 쪽 경로는 완성하지 않는다.
        assert!(cp_endpoints(&data, "box:/et").is_empty());
        // 절대 경로는 호스트 경로만.
        let host = format!("{}/da", root.display());
        assert_eq!(
            values(&cp_endpoints(&data, &host)),
            [format!(
                "{}{sep}data{sep}",
                root.display(),
                sep = std::path::MAIN_SEPARATOR
            )]
        );
    }

    #[test]
    fn executors_are_names_and_versions() {
        let root = temp("ex");
        let data = root.join("data");
        for v in ["1.0.0", "1.2.0"] {
            std::fs::create_dir_all(dirs::executors_dir(&data).join("fc").join(v)).unwrap();
        }
        assert_eq!(
            values(&executors(&data, "")),
            ["fc", "fc@1.0.0", "fc@1.2.0"]
        );
        assert_eq!(values(&executors(&data, "fc@1.2")), ["fc@1.2.0"]);
    }

    #[test]
    fn dirs_come_from_typed_options_then_env_then_defaults() {
        let w = |s: &[&str]| s.iter().map(OsString::from).collect::<Vec<_>>();
        let env = |k: &str| match k {
            "SBX_DATA_DIR" => Some(OsString::from("/env/data")),
            "SBX_CACHE_DIR" => Some(OsString::new()),
            _ => None,
        };
        let d = Dirs::from_words(&w(&["sbx", "rm", ""]), env);
        assert_eq!(d.data_dir, Some("/env/data".into()));
        // 비어 있는 환경 변수는 없는 것이다.
        assert_eq!(d.cache_dir, dirs::cache_dir());
        let d = Dirs::from_words(
            &w(&[
                "sbx",
                "--data-dir",
                "/a",
                "run",
                "--cache-dir=/c",
                "--data-dir=/b",
                "",
            ]),
            env,
        );
        assert_eq!(d.data_dir, Some("/b".into()));
        assert_eq!(d.cache_dir, Some("/c".into()));
    }

    #[test]
    fn every_shell_has_a_registration_script() {
        use clap::ValueEnum;
        for shell in clap_complete::Shell::value_variants() {
            let mut buf = Vec::new();
            registration(*shell, &mut buf).unwrap();
            let script = String::from_utf8(buf).unwrap();
            assert!(script.contains(ENV_VAR), "{shell}: {script}");
        }
    }
}
