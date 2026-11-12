//! sbx 릴리스 색인(`releases.json`)을 만들고 서명한다. 형식과 클라이언트 규칙은 `sbx-release-index` 크레이트 문서다.
//!
//! ```text
//! cargo xtask release-index --index <파일> --version <버전> --assets <디렉터리> --base-url <URL> [--security]
//! cargo xtask release-yank --index <파일> --version <버전> [--advisory <글>] [--undo]
//! cargo xtask release-sign --index <파일> (--key <파일> | --key-env <변수>) [--expires-days <D>]
//! cargo xtask sign-files (--key <파일> | --key-env <변수>) <파일>...
//! ```
//!
//! - `release-index`: `<디렉터리>` 아래(한 단계 안까지)의 `sbx-<플랫폼>.tar.gz`마다 SHA-256과 크기를 재어 그 버전의
//!   항목을 쓴다(있으면 바꾼다). URL은 `<URL>/<파일 이름>`. `layout_version`은 `crates/app/cli/src/layout.rs`의
//!   `CURRENT`, `protocol`은 `sbx-executor-proto`의 `MIN_VERSION..=VERSION`이다. 파일이 없으면 새로 만든다. 서명은
//!   하지 않는다(`release-sign`).
//! - `release-yank`: 그 버전을 거두거나(`--undo`면 되돌린다) 알림을 단다. 서명은 하지 않는다.
//! - `release-sign`: `serial`과 `expires`(기본 30일)를 새로 정하고 `<파일>.minisig`를 쓴다. 고칠 때마다, 그리고 기한
//!   전에 다시 한다.
//! - `sign-files`: 파일마다 `<파일>.minisig`(릴리스 묶음의 서명, `sbx upgrade`가 본다).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::sign;

/// 릴리스 색인의 기본 기한(일). 매달 다시 서명한다.
const DEFAULT_EXPIRES_DAYS: u64 = 30;

fn usage(cmd: &str) -> String {
    match cmd {
        "release-index" => "usage: cargo xtask release-index --index <file> --version <version> --assets <dir> --base-url <url> [--security]",
        "release-yank" => "usage: cargo xtask release-yank --index <file> --version <version> [--advisory <text>] [--undo]",
        "release-sign" => "usage: cargo xtask release-sign --index <file> (--key <file> | --key-env <var>) [--expires-days <days>]",
        _ => "usage: cargo xtask sign-files (--key <file> | --key-env <var>) <file>...",
    }
    .to_owned()
}

#[derive(Default)]
struct Options {
    index: Option<PathBuf>,
    version: Option<String>,
    assets: Option<PathBuf>,
    base_url: Option<String>,
    security: bool,
    advisory: Option<String>,
    undo: bool,
    key: Option<String>,
    expires_days: Option<u64>,
    files: Vec<PathBuf>,
}

fn parse(cmd: &str, args: &[String]) -> Result<Options, String> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--index" => o.index = Some(PathBuf::from(val()?)),
            "--version" => o.version = Some(val()?),
            "--assets" => o.assets = Some(PathBuf::from(val()?)),
            "--base-url" => o.base_url = Some(val()?),
            "--security" => o.security = true,
            "--advisory" => o.advisory = Some(val()?),
            "--undo" => o.undo = true,
            "--key" | "--key-env" => o.key = Some(sign::key_option(a, &val()?)?),
            "--expires-days" => o.expires_days = Some(sign::expires_days(&val()?)?),
            f if !f.starts_with('-') && cmd == "sign-files" => o.files.push(PathBuf::from(f)),
            other => return Err(format!("unknown option {other}\n{}", usage(cmd))),
        }
    }
    Ok(o)
}

fn need<T>(v: Option<T>, flag: &str, cmd: &str) -> Result<T, String> {
    v.ok_or_else(|| format!("{flag} is required\n{}", usage(cmd)))
}

/// 정식 릴리스 버전(`0.2.2`, 앞의 `v`는 뗀다). 클라이언트와 같은 규칙을 대충 본다(클라이언트가 다시 본다).
fn version(v: &str) -> Result<String, String> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let ok = v.split('.').count() == 3
        && v.split('.').all(|p| {
            !p.is_empty()
                && p.bytes().all(|b| b.is_ascii_digit())
                && (p == "0" || !p.starts_with('0'))
        });
    if ok {
        Ok(v.to_owned())
    } else {
        Err(format!("{v:?} is not a release version like 0.2.2"))
    }
}

fn load(path: &Path) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({ "schema": 1, "releases": [] }));
    }
    let v = sign::read_json(path)?;
    if v["schema"] != json!(1) {
        return Err(format!(
            "{} is not a schema 1 release index",
            path.display()
        ));
    }
    Ok(v)
}

fn save(path: &Path, v: &Value) -> Result<(), String> {
    let body = serde_json::to_string_pretty(v).map_err(|e| e.to_string())? + "\n";
    std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
}

fn releases(v: &mut Value) -> Result<&mut Vec<Value>, String> {
    v["releases"]
        .as_array_mut()
        .ok_or_else(|| "releases is not a list".to_owned())
}

/// `sbx-<플랫폼>.tar.gz`를 `dir`과 그 바로 아래 디렉터리에서 찾는다.
fn archives(dir: &Path) -> Result<Vec<(String, PathBuf)>, String> {
    let mut found = Vec::new();
    let mut look = |d: &Path| -> Result<(), String> {
        for e in std::fs::read_dir(d).map_err(|e| format!("{}: {e}", d.display()))? {
            let p = e.map_err(|e| e.to_string())?.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if let Some(platform) = name
                .strip_prefix("sbx-")
                .and_then(|n| n.strip_suffix(".tar.gz"))
                && p.is_file()
            {
                found.push((platform.to_owned(), p.clone()));
            }
        }
        Ok(())
    };
    look(dir)?;
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let p = e.map_err(|e| e.to_string())?.path();
        if p.is_dir() {
            look(&p)?;
        }
    }
    found.sort();
    found.dedup_by(|a, b| a.0 == b.0);
    if found.is_empty() {
        return Err(format!("no sbx-<platform>.tar.gz under {}", dir.display()));
    }
    Ok(found)
}

/// `layout.rs`의 `pub const CURRENT: u32 = N;`.
fn layout_version(root: &Path) -> Result<u64, String> {
    let file = root.join("crates/app/cli/src/layout.rs");
    let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("pub const CURRENT: u32 = "))
        .and_then(|r| r.trim_end_matches(';').trim().parse().ok())
        .ok_or_else(|| format!("no `pub const CURRENT: u32 = N;` in {}", file.display()))
}

/// `release-index`. 쓴 항목의 플랫폼들을 돌려준다.
pub fn index(root: &Path, args: &[String]) -> Result<Vec<String>, String> {
    let cmd = "release-index";
    let o = parse(cmd, args)?;
    let path = need(o.index, "--index", cmd)?;
    let ver = version(&need(o.version, "--version", cmd)?)?;
    let dir = need(o.assets, "--assets", cmd)?;
    let base = need(o.base_url, "--base-url", cmd)?;
    let base = base.trim_end_matches('/');
    let mut assets = serde_json::Map::new();
    let mut platforms = Vec::new();
    for (platform, file) in archives(&dir)? {
        let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let sha: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        assets.insert(
            platform.clone(),
            json!({ "url": format!("{base}/{name}"), "sha256": sha, "size": bytes.len() }),
        );
        platforms.push(platform);
    }
    let entry = json!({
        "version": ver,
        "security": o.security,
        "yanked": false,
        "advisory": null,
        "layout_version": layout_version(root)?,
        "protocol": {
            "min": sbx_executor_proto::MIN_VERSION,
            "max": sbx_executor_proto::VERSION,
        },
        "assets": assets,
    });
    let mut idx = load(&path)?;
    let list = releases(&mut idx)?;
    list.retain(|r| r["version"] != json!(ver));
    list.push(entry);
    save(&path, &idx)?;
    Ok(platforms)
}

/// `release-yank`.
pub fn yank(args: &[String]) -> Result<(), String> {
    let cmd = "release-yank";
    let o = parse(cmd, args)?;
    let path = need(o.index, "--index", cmd)?;
    let ver = version(&need(o.version, "--version", cmd)?)?;
    let mut idx = load(&path)?;
    let entry = releases(&mut idx)?
        .iter_mut()
        .find(|r| r["version"] == json!(ver))
        .ok_or_else(|| format!("{ver} is not in {}", path.display()))?;
    entry["yanked"] = json!(!o.undo);
    if let Some(a) = o.advisory {
        entry["advisory"] = json!(a);
    } else if o.undo {
        entry["advisory"] = Value::Null;
    }
    save(&path, &idx)
}

/// `release-sign`. 새 `serial`을 돌려준다.
pub fn sign_index(args: &[String]) -> Result<u64, String> {
    let cmd = "release-sign";
    let o = parse(cmd, args)?;
    let path = need(o.index, "--index", cmd)?;
    let key = need(o.key, "--key or --key-env", cmd)?;
    let mut idx = load(&path)?;
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let mut sig = path.clone().into_os_string();
    sig.push(".minisig");
    sign::sign_json(
        &mut idx,
        &key,
        None,
        o.expires_days.unwrap_or(DEFAULT_EXPIRES_DAYS),
        "sbx release index",
        &path,
        Path::new(&sig),
    )
}

/// `sign-files`. 쓴 서명 파일들을 돌려준다.
pub fn sign_files(args: &[String]) -> Result<Vec<PathBuf>, String> {
    let cmd = "sign-files";
    let o = parse(cmd, args)?;
    let sk = sign::secret_key(&need(o.key, "--key or --key-env", cmd)?)?;
    if o.files.is_empty() {
        return Err(usage(cmd));
    }
    let mut out = Vec::new();
    for f in o.files {
        let body = std::fs::read(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        let name = f.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let trusted = format!("sbx release archive {name}");
        let sig = minisign::sign(None, &sk, body.as_slice(), Some(&trusted), None)
            .map_err(|e| e.to_string())?;
        let mut sig_file = f.clone().into_os_string();
        sig_file.push(".minisig");
        let sig_file = PathBuf::from(sig_file);
        std::fs::write(&sig_file, sig.into_string())
            .map_err(|e| format!("{}: {e}", sig_file.display()))?;
        out.push(sig_file);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn the_index_it_makes_is_read_by_the_client() {
        let root = std::env::temp_dir().join(format!("sbx-xtask-release-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("artifacts/sbx-linux-x86_64")).unwrap();
        std::fs::create_dir_all(root.join("artifacts/sbx-darwin-aarch64")).unwrap();
        std::fs::write(
            root.join("artifacts/sbx-linux-x86_64/sbx-linux-x86_64.tar.gz"),
            b"linux",
        )
        .unwrap();
        std::fs::write(
            root.join("artifacts/sbx-darwin-aarch64/sbx-darwin-aarch64.tar.gz"),
            b"mac",
        )
        .unwrap();
        std::fs::write(
            root.join("artifacts/sbx-linux-x86_64/sbx-linux-x86_64.tar.gz.sha256"),
            b"x",
        )
        .unwrap();
        let ws = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let file = root.join("releases.json");
        let f = file.to_str().unwrap();
        let assets = root.join("artifacts");
        let base = url::Url::from_directory_path(&assets).unwrap();
        for v in ["v0.2.1", "0.2.2"] {
            let p = index(
                ws,
                &s(&[
                    "--index",
                    f,
                    "--version",
                    v,
                    "--assets",
                    assets.to_str().unwrap(),
                    "--base-url",
                    base.as_str(),
                ]),
            )
            .unwrap();
            assert_eq!(p, ["darwin-aarch64", "linux-x86_64"]);
        }
        // 다시 쓰면 바꾼다(겹치지 않는다).
        index(
            ws,
            &s(&[
                "--index",
                f,
                "--version",
                "0.2.2",
                "--assets",
                assets.to_str().unwrap(),
                "--base-url",
                base.as_str(),
                "--security",
            ]),
        )
        .unwrap();
        yank(&s(&[
            "--index",
            f,
            "--version",
            "0.2.1",
            "--advisory",
            "use 0.2.2",
        ]))
        .unwrap();
        assert!(yank(&s(&["--index", f, "--version", "9.9.9"])).is_err());
        assert!(
            index(
                ws,
                &s(&[
                    "--index",
                    f,
                    "--version",
                    "0.2",
                    "--assets",
                    "x",
                    "--base-url",
                    "y"
                ])
            )
            .is_err()
        );

        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let key = root.join("k.key");
        std::fs::write(&key, kp.sk.to_box(None).unwrap().into_string()).unwrap();
        let serial = sign_index(&s(&["--index", f, "--key", key.to_str().unwrap()])).unwrap();
        // 묶음은 클라이언트가 서명을 본다. 리눅스 것만 서명해 확인한다.
        let linux = root.join("artifacts/sbx-linux-x86_64/sbx-linux-x86_64.tar.gz");
        // 위에서 하위 디렉터리의 것을 찾았지만 URL은 base/<이름>이다: 클라이언트가 찾게 base에 둔다.
        std::fs::copy(&linux, assets.join("sbx-linux-x86_64.tar.gz")).unwrap();
        let sigs = sign_files(&s(&[
            "--key",
            key.to_str().unwrap(),
            assets.join("sbx-linux-x86_64.tar.gz").to_str().unwrap(),
        ]))
        .unwrap();
        assert_eq!(sigs.len(), 1);

        let src = sbx_release_index::Source {
            url: url::Url::from_file_path(&file).unwrap(),
            keys: vec![kp.pk.to_base64()],
        };
        let fetcher = sbx_executor_repo::Fetcher::new().unwrap();
        let idx = sbx_release_index::fetch(&fetcher, &src, None)
            .await
            .unwrap();
        assert_eq!(idx.serial, serial);
        assert_eq!(idx.releases.len(), 2);
        let (old, new) = (&idx.releases[0], &idx.releases[1]);
        assert!(old.yanked && old.advisory.as_deref() == Some("use 0.2.2"));
        assert!(
            new.security
                && new.protocol
                    == Some((sbx_executor_proto::MIN_VERSION, sbx_executor_proto::VERSION))
        );
        assert!(new.layout_version.is_some());
        let dest = root.join("got");
        sbx_release_index::download(
            &fetcher,
            &new.assets["linux-x86_64"],
            &[kp.pk.to_base64()],
            &dest,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"linux");
        let _ = std::fs::remove_dir_all(&root);
    }
}
