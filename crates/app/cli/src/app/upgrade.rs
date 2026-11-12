//! `sbx upgrade`: 서명을 확인한 릴리스로 `sbx`와 `sbx-shim`을 바꾼다. 방향은 `docs/plan/upgrade.md`.
//!
//! 명시적 동작:
//!
//! 1. 색인을 받는다([`crate::update::fetch_index`]: 색인 키의 서명, 기한, 본 serial). 받은 것은 알림 기록에도 남긴다.
//! 2. `--check`면 여기서 끝난다(종료 코드 0, 더 높은 것이 있으면 [`behavior::EXIT_UPDATE_AVAILABLE`]).
//! 3. 바꿀 곳: 이 실행 파일이 있는 디렉터리. `sbx-shim`이 옆에 있어야 하고 쓸 수 있어야 하며, 패키지 관리자 경로
//!    ([`managed_path`])가 아니어야 한다. 패키지 빌드(`SBX_UPGRADE_COMMAND`)면 그 명령을 권하고 멈춘다.
//! 4. 목표: [`sbx_release_index::target`]. 다른 부 버전으로 가거나, 거둔 버전을 고르거나, 목표가 이 `sbx`의 실행기
//!    규약을 더 말하지 않으면 묻는다(`-y`면 묻지 않고, 터미널이 아니면 `-y`가 있어야 한다). 낮은 버전은 데이터
//!    디렉터리의 `layout-version`보다 낮은 배치를 쓰면 거부한다.
//! 5. 받기: 이 플랫폼의 tar.gz. 크기·SHA-256(색인)과 묶음 키의 minisign 서명이 맞아야 한다. `cosign`이 있으면
//!    Sigstore 번들도 본다(`SBX_VERIFY=require`면 cosign이 있어야 하고, `off`면 Sigstore는 보지 않는다. minisign은 늘 본다).
//! 6. 풀어서 `sbx`와 `sbx-shim`을 같은 디렉터리의 임시 이름으로 두고, 새 `sbx --version`이 목표 버전을 말하는지 본다.
//! 7. 잠금(`.sbx-upgrade.lock`) 아래에서 `sbx-shim`, `sbx` 차례로 바꾼다. 지금 것을 `.<이름>.old`로 옮기고 새 것을
//!    넣는다. 하나라도 실패하면 옮긴 것을 되돌린다. 끝나면 `.old`를 지운다(Windows에서 돌고 있는 파일은 지울 수 없어
//!    다음 `upgrade`가 지운다).
//!
//! 돌고 있는 샌드박스는 그대로 둔다(게스트 규약은 더하기만 한다).

use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sbx_release_index::{Release, Version};

use super::*;
use crate::cli::UpgradeArgs;

/// 색인과 묶음을 받을 때 읽기 하나를 기다리는 한도.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// 새 `sbx --version`을 기다리는 한도.
const TRY_TIMEOUT: Duration = Duration::from_secs(30);
/// Sigstore 번들의 크기 한도.
const MAX_BUNDLE_BYTES: u64 = 1 << 20;

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// 패키지 관리자가 둔 곳인가. 그렇다면 그 관리자로 올리라고 한다.
pub(crate) fn managed_path(dir: &Path) -> Option<&'static str> {
    let s = dir.to_string_lossy().replace('\\', "/");
    let rules: &[(&str, &str)] = &[
        ("/Cellar/", "brew upgrade"),
        ("/nix/store/", "your Nix configuration"),
        ("/.cargo/bin", "cargo install"),
        ("/snap/", "snap refresh"),
        ("/scoop/", "scoop update"),
        ("/WinGet/", "winget upgrade"),
    ];
    if let Some((_, how)) = rules.iter().find(|(p, _)| s.contains(p)) {
        return Some(how);
    }
    if s.starts_with("/usr/bin") || s.starts_with("/usr/sbin") || s.starts_with("/bin") {
        return Some("the package manager that installed it");
    }
    None
}

fn ask(question: &str) -> AppResult<bool> {
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

/// 데이터 디렉터리의 배치 버전(파일이 없으면 1).
fn data_layout(data_dir: &Path) -> u32 {
    std::fs::read_to_string(data_dir.join(crate::layout::FILE))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(1)
}

/// 물어야 할 것들(없으면 빈 목록).
fn questions(current: Version, target: &Release, data_layout: u32) -> AppResult<Vec<String>> {
    let v = target.version;
    if v < current
        && let Some(l) = target.layout_version
        && l < data_layout
    {
        return Err(msg(format!(
            "sbx {v} uses data layout version {l}, but the data directory is already at {data_layout}; \
an older sbx can't read it"
        )));
    }
    let mut q = Vec::new();
    if !v.same_line(&current) {
        q.push(format!(
            "sbx {current} → {v} is a different minor version and may change behavior (see \
https://github.com/toollyst/sandbox/blob/main/CHANGELOG.md)."
        ));
    }
    if target.yanked {
        q.push(match &target.advisory {
            Some(a) => format!("sbx {v} has been yanked: {a}"),
            None => format!("sbx {v} has been yanked."),
        });
    }
    if let Some((min, max)) = target.protocol
        && !(min..=max).contains(&sbx_executor_proto::VERSION)
    {
        q.push(format!(
            "sbx {v} speaks executor protocol {min}..{max}, not {} that this sbx and its executors use; \
installed executors may stop working until you install newer ones.",
            sbx_executor_proto::VERSION
        ));
    }
    Ok(q)
}

/// `.<이름>.old`.
fn old_name(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(".{name}.old"))
}

/// 지난 `upgrade`가 남긴 `.old`를 지운다(지울 수 없으면 둔다).
fn clean_old(dir: &Path) {
    for n in ["sbx", "sbx-shim"] {
        let _ = std::fs::remove_file(old_name(dir, &exe(n)));
    }
}

/// `new`(같은 디렉터리의 임시 파일)들을 제자리에 넣는다. 하나라도 실패하면 되돌린다.
fn replace_all(dir: &Path, staged: &[(String, PathBuf)]) -> std::io::Result<()> {
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut placed: Vec<(PathBuf, &Path)> = Vec::new();
    let result = (|| {
        for (name, new) in staged {
            let path = dir.join(name);
            let old = old_name(dir, name);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&path, &old)?;
            moved.push((old.clone(), path.clone()));
            std::fs::rename(new, &path)?;
            placed.push((path, new));
        }
        Ok(())
    })();
    if result.is_err() {
        // 넣은 새 파일은 놓아 둔 곳으로, 옮긴 지금 파일은 제자리로.
        for (path, new) in placed.iter().rev() {
            let _ = std::fs::rename(path, new);
        }
        for (old, path) in moved.iter().rev() {
            let _ = std::fs::rename(old, path);
        }
    }
    result
}

/// 새 `sbx --version`이 `want`를 말하는가.
async fn tries(sbx: &Path, want: Version) -> AppResult<()> {
    let mut cmd = tokio::process::Command::new(sbx);
    cmd.arg("--version")
        .env("SBX_NO_UPDATE_CHECK", "1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(TRY_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            msg(format!(
                "the new sbx did not answer --version within {TRY_TIMEOUT:?}"
            ))
        })?
        .map_err(|e| msg(format!("cannot run the new sbx: {e}")))?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if !out.status.success() || said != format!("sbx {want}") {
        return Err(msg(format!(
            "the new sbx says {said:?} for --version, not \"sbx {want}\"; nothing was changed"
        )));
    }
    Ok(())
}

/// cosign이 있으면 Sigstore 번들을 본다. `SBX_VERIFY`: `auto`(기본), `require`, `off`.
async fn sigstore(
    fetcher: &sbx_executor_repo::Fetcher,
    archive: &Path,
    url: &url::Url,
    v: Version,
) -> AppResult<&'static str> {
    let mode = std::env::var("SBX_VERIFY").unwrap_or_else(|_| "auto".into());
    match mode.as_str() {
        "off" => return Ok("Sigstore not checked (SBX_VERIFY=off)"),
        "auto" | "require" => {}
        other => {
            return Err(msg(format!(
                "SBX_VERIFY must be auto, require or off (got {other})"
            )));
        }
    }
    let cosign = which("cosign");
    let Some(cosign) = cosign else {
        if mode == "require" {
            return Err(msg(
                "SBX_VERIFY=require needs cosign (https://docs.sigstore.dev/cosign/system_config/installation/)",
            ));
        }
        return Ok("Sigstore not checked (no cosign)");
    };
    let mut bundle_url = url.clone();
    bundle_url.set_path(&format!("{}.sigstore.json", url.path()));
    let bundle = fetcher
        .bytes(&bundle_url, MAX_BUNDLE_BYTES)
        .await
        .map_err(|e| msg(format!("cannot fetch the Sigstore bundle: {e}")))?;
    let bundle_file = archive.with_extension("sigstore.json");
    std::fs::write(&bundle_file, bundle)?;
    let tag = format!("v{v}").replace('.', "[.]");
    let identity = format!(
        "^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/{tag}$"
    );
    let out = tokio::process::Command::new(cosign)
        .args(["verify-blob", "--bundle"])
        .arg(&bundle_file)
        .args(["--certificate-identity-regexp", &identity])
        .args([
            "--certificate-oidc-issuer",
            "https://token.actions.githubusercontent.com",
        ])
        .arg(archive)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| msg(format!("cannot run cosign: {e}")))?;
    if !out.status.success() {
        return Err(msg(format!(
            "the Sigstore signature does not verify: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok("Sigstore verified")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(exe(name)))
        .find(|p| p.is_file())
}

impl Ctx {
    /// `upgrade`.
    pub(super) async fn upgrade(&self, a: &UpgradeArgs) -> AppResult<u8> {
        let current = crate::update::current();
        let platform = sbx_release_index::host_platform();
        let want = a
            .version
            .as_deref()
            .map(Version::parse)
            .transpose()
            .map_err(msg)?;
        if !a.check
            && let Some(cmd) = crate::update::packaged_upgrade()
        {
            return Err(msg(format!(
                "this sbx was installed by a package manager; upgrade it with '{cmd}'"
            )));
        }
        let index = crate::update::fetch_index(&self.data_dir, READ_TIMEOUT)
            .await
            .map_err(|e| msg(format!("cannot read the sbx release index: {e}")))?;
        crate::update::record(&self.cache_dir, &index);
        let status = sbx_release_index::status(current, &index, &platform);
        let report = |installed: Option<Version>, path: Option<&Path>| output::Upgrade {
            current: current.to_string(),
            latest: status.latest.map(|r| r.version.to_string()),
            security: status.latest.is_some() && status.security,
            yanked: status.yanked(),
            advisory: status.advisory().map(str::to_owned),
            installed: installed.map(|v| v.to_string()),
            path: path.map(|p| p.display().to_string()),
        };
        if a.check {
            if self.json() {
                output::print_value(&report(None, None))?;
            } else {
                match crate::update::summary(&status) {
                    Some(s) => println!("{s}"),
                    None => println!("sbx {current} is the newest release"),
                }
            }
            return Ok(if status.latest.is_some() {
                behavior::EXIT_UPDATE_AVAILABLE
            } else {
                0
            });
        }
        let Some(target) =
            sbx_release_index::target(current, &index, &platform, want, a.allow_downgrade)
                .map_err(msg)?
        else {
            if self.json() {
                output::print_value(&report(None, None))?;
            } else {
                println!(
                    "sbx {current} is already {}",
                    if want.is_some() {
                        "that version"
                    } else {
                        "the newest release"
                    }
                );
            }
            return Ok(0);
        };
        let v = target.version;

        // 바꿀 곳.
        let me = std::env::current_exe()?;
        let me = std::fs::canonicalize(&me).unwrap_or(me);
        let dir = me
            .parent()
            .ok_or_else(|| msg("cannot tell where sbx is installed"))?
            .to_path_buf();
        if let Some(how) = managed_path(&dir) {
            return Err(msg(format!(
                "sbx in {} is managed by a package manager; upgrade it with {how}",
                dir.display()
            )));
        }
        for n in ["sbx", "sbx-shim"] {
            if !dir.join(exe(n)).is_file() {
                return Err(msg(format!(
                    "{} is not next to sbx in {}; reinstall with the installer instead",
                    exe(n),
                    dir.display()
                )));
            }
        }

        let q = questions(current, target, data_layout(&self.data_dir))?;
        if !q.is_empty() && !a.yes {
            if self.json() || !std::io::stdin().is_terminal() {
                return Err(msg(format!("{} Pass -y to go ahead.", q.join(" "))));
            }
            for line in &q {
                eprintln!("{line}");
            }
            if !ask(&format!("Upgrade to sbx {v}?"))? {
                return Ok(1);
            }
        }

        let asset = target
            .assets
            .get(&platform)
            .ok_or_else(|| msg(format!("sbx {v} has no build for {platform}")))?;
        let scratch = dir.join(format!(".sbx-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch).map_err(|e| {
            msg(format!(
                "cannot write to {} ({e}); is it yours?",
                dir.display()
            ))
        })?;
        let result = self.install(v, asset, &dir, &scratch).await;
        let _ = std::fs::remove_dir_all(&scratch);
        let checked = result?;
        if self.json() {
            output::print_value(&report(Some(v), Some(&dir)))?;
        } else {
            println!("sbx {current} → {v} in {} ({checked})", dir.display());
            println!("What changed: https://github.com/toollyst/sandbox/releases/tag/v{v}");
        }
        Ok(0)
    }

    /// 받고 확인하고 바꾼다. 확인한 것을 말하는 글을 돌려준다.
    async fn install(
        &self,
        v: Version,
        asset: &sbx_release_index::Asset,
        dir: &Path,
        scratch: &Path,
    ) -> AppResult<&'static str> {
        let fetcher = crate::update::fetcher(READ_TIMEOUT).map_err(msg)?;
        let archive = scratch.join("sbx.tar.gz");
        if !self.json() {
            eprintln!("Downloading sbx {v} ({} bytes)", asset.size);
        }
        sbx_release_index::download(&fetcher, asset, &crate::update::archive_keys(), &archive)
            .await
            .map_err(|e| msg(format!("cannot download sbx {v}: {e}")))?;
        let sigstore = sigstore(&fetcher, &archive, &asset.url, v).await?;
        let root = sbx_executor_repo::unpack::unpack(
            &archive,
            &scratch.join("unpacked"),
            sbx_executor_repo::unpack::Layout::Vmm,
        )
        .map_err(|e| msg(format!("cannot unpack sbx {v}: {e}")))?;
        let mut staged = Vec::new();
        for n in ["sbx-shim", "sbx"] {
            let name = exe(n);
            let from = root.join(&name);
            let meta = std::fs::symlink_metadata(&from)
                .map_err(|_| msg(format!("the sbx {v} archive has no {name}")))?;
            if !meta.is_file() {
                return Err(msg(format!(
                    "{name} in the sbx {v} archive is not a regular file"
                )));
            }
            let to = scratch.join(format!("{name}.new"));
            std::fs::copy(&from, &to)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755))?;
            }
            staged.push((name, to));
        }
        tries(&staged[1].1, v).await?;
        sbx_executor_repo::file::locked(&dir.join("sbx-upgrade"), || {
            clean_old(dir);
            replace_all(dir, &staged)
        })
        .map_err(|e| {
            msg(format!(
                "cannot replace sbx in {}: {e}; nothing was changed",
                dir.display()
            ))
        })?;
        clean_old(dir);
        Ok(if sigstore == "Sigstore verified" {
            "minisign and Sigstore verified"
        } else {
            "minisign verified"
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(v: &str) -> Release {
        Release {
            version: Version::parse(v).unwrap(),
            security: false,
            yanked: false,
            advisory: None,
            layout_version: Some(1),
            protocol: Some((sbx_executor_proto::MIN_VERSION, sbx_executor_proto::VERSION)),
            assets: Default::default(),
        }
    }

    #[test]
    fn package_manager_paths_are_left_alone() {
        assert_eq!(
            managed_path(Path::new("/opt/homebrew/Cellar/sbx/0.2.2/bin")),
            Some("brew upgrade")
        );
        assert_eq!(
            managed_path(Path::new("/home/u/.cargo/bin")),
            Some("cargo install")
        );
        assert!(managed_path(Path::new("/usr/bin")).is_some());
        assert_eq!(managed_path(Path::new("/home/u/.local/bin")), None);
        assert_eq!(
            managed_path(Path::new("C:\\Users\\u\\AppData\\Local\\Programs\\sbx")),
            None
        );
    }

    #[test]
    fn questions_for_minor_jumps_yanked_and_protocol() {
        let cur = Version::parse("0.2.1").unwrap();
        assert!(questions(cur, &release("0.2.2"), 1).unwrap().is_empty());
        assert_eq!(questions(cur, &release("0.3.0"), 1).unwrap().len(), 1);
        let mut y = release("0.2.2");
        y.yanked = true;
        assert_eq!(questions(cur, &y, 1).unwrap().len(), 1);
        let mut p = release("0.2.2");
        p.protocol = Some((
            sbx_executor_proto::VERSION + 1,
            sbx_executor_proto::VERSION + 1,
        ));
        assert_eq!(questions(cur, &p, 1).unwrap().len(), 1);
        let mut old = release("0.2.0");
        old.layout_version = Some(1);
        assert!(
            questions(cur, &old, 2).is_err(),
            "older layout than the data"
        );
    }

    #[test]
    fn replace_all_is_all_or_nothing() {
        let d = std::env::temp_dir().join(format!("sbx-upgrade-replace-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a"), "old a").unwrap();
        std::fs::write(d.join("b"), "old b").unwrap();
        std::fs::write(d.join("new-a"), "new a").unwrap();
        // b의 새 파일이 없다: a를 바꾼 뒤 실패하므로 a도 되돌린다.
        let r = replace_all(
            &d,
            &[
                ("a".into(), d.join("new-a")),
                ("b".into(), d.join("missing")),
            ],
        );
        assert!(r.is_err());
        assert_eq!(std::fs::read_to_string(d.join("a")).unwrap(), "old a");
        assert_eq!(std::fs::read_to_string(d.join("b")).unwrap(), "old b");
        std::fs::write(d.join("new-b"), "new b").unwrap();
        replace_all(
            &d,
            &[("a".into(), d.join("new-a")), ("b".into(), d.join("new-b"))],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(d.join("a")).unwrap(), "new a");
        assert_eq!(std::fs::read_to_string(d.join("b")).unwrap(), "new b");
        let _ = std::fs::remove_dir_all(&d);
    }
}
