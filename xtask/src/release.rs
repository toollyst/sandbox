//! sbx 릴리스 색인(`releases.json`)을 만들고 서명한다. 형식과 클라이언트 규칙은 `sbx-release-index` 크레이트 문서다.
//!
//! ```text
//! cargo xtask release-index --index <파일> --version <버전> --assets <디렉터리> --base-url <URL> [--security] <키> [--expires-days <D>]
//! cargo xtask release-yank --index <파일> --version <버전> [--advisory <글>] [--undo] <키> [--expires-days <D>]
//! cargo xtask release-sign --index <파일> <키> [--expires-days <D>]
//! cargo xtask sign-files (--key <파일> | --key-env <변수>) <파일>...
//! ```
//!
//! - `release-index`: `<디렉터리>` 아래(한 단계 안까지)의 `sbx-<플랫폼>.tar.gz`마다 SHA-256과 크기를 재어 그 버전의
//!   항목을 쓴다(있으면 바꾼다). URL은 `<URL>/<파일 이름>`. `layout_version`은 `crates/app/cli/src/layout.rs`의
//!   `CURRENT`, `protocol`은 `sbx-executor-proto`의 `MIN_VERSION..=VERSION`이다. 파일이 없으면 새로 만든다.
//! - `release-yank`: 그 버전을 거두거나(`--undo`면 되돌린다) 알림을 단다.
//! - `release-sign`: 내용은 그대로 두고 `serial`과 `expires`만 새로 정한다(기한 전에 다시 서명).
//!
//! `<키>`는 `--key <파일>`이나 `--key-env <변수>`(색인 키의 비밀키)다. 셋 모두 고친 뒤 `serial`과 `expires`(기본
//! 30일)를 새로 정하고 `<파일>.minisig`를 쓴다. 그 전에 있던 파일은 **그 키로 서명된 것이어야 한다**(공개키는
//! 비밀키에서 얻는다. 색인 키를 바꿀 때는 `--signed-by <옛 공개키>`): gh-pages에 누가 넣은 내용을 서명해 주지 않는다. 있던 `serial`이 지금보다 하루 넘게 앞서면
//! 거부한다(아주 큰 serial로 모든 클라이언트를 영영 막는 것).
//! - `sign-files`: 파일마다 `<파일>.minisig`(릴리스 묶음의 서명, `sbx upgrade`가 본다).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::sign;

/// 릴리스 색인의 기본 기한(일). 매달 다시 서명한다.
const DEFAULT_EXPIRES_DAYS: u64 = 30;

fn usage(cmd: &str) -> String {
    match cmd {
        "release-index" => "usage: cargo xtask release-index --index <file> --version <version> --assets <dir> --base-url <url> [--security] (--key <file> | --key-env <var>) [--expires-days <days>]",
        "release-yank" => "usage: cargo xtask release-yank --index <file> --version <version> [--advisory <text>] [--undo] (--key <file> | --key-env <var>) [--expires-days <days>]",
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
    signed_by: Option<String>,
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
            "--signed-by" => o.signed_by = Some(val()?),
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

fn sig_path(path: &Path) -> PathBuf {
    let mut sig = path.as_os_str().to_owned();
    sig.push(".minisig");
    PathBuf::from(sig)
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 있던 색인을 읽는다. 없으면 빈 색인. 있으면 `key`(비밀키)의 공개키로 한 서명이 맞아야 하고, `serial`이 지금보다 하루
/// 넘게 앞서면 안 된다.
fn load_verified(path: &Path, key: &str, signed_by: Option<&str>) -> Result<Value, String> {
    if !path.exists() {
        return Ok(json!({ "schema": 1, "releases": [] }));
    }
    let body = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sig_file = sig_path(path);
    let sig = std::fs::read_to_string(&sig_file).map_err(|e| {
        format!(
            "{}: {e} (refusing to sign a release index nobody signed)",
            sig_file.display()
        )
    })?;
    let pk = match signed_by {
        Some(b64) => minisign::PublicKey::from_base64(b64.trim())
            .map_err(|e| format!("--signed-by {b64:?}: {e}"))?,
        None => minisign::PublicKey::from_secret_key(&sign::secret_key(key)?)
            .map_err(|e| e.to_string())?,
    };
    let sig = minisign::SignatureBox::from_string(&sig)
        .map_err(|e| format!("{}: {e}", sig_file.display()))?;
    minisign::verify(&pk, &sig, std::io::Cursor::new(&body), true, false, false).map_err(|e| {
        format!(
            "{} is not signed by this key ({e}); refusing to sign content that changed since",
            path.display()
        )
    })?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| format!("{}: {e}", path.display()))?;
    if v["schema"] != json!(1) {
        return Err(format!(
            "{} is not a schema 1 release index",
            path.display()
        ));
    }
    let serial = v["serial"].as_u64().unwrap_or(0);
    if serial > now() + 86_400 {
        return Err(format!(
            "{} has serial {serial}, more than a day ahead of now; refusing to carry it forward",
            path.display()
        ));
    }
    Ok(v)
}

/// 고친 색인에 서명해 쓴다. 새 `serial`을 돌려준다.
fn sign_and_save(
    path: &Path,
    idx: &mut Value,
    key: &str,
    days: Option<u64>,
) -> Result<u64, String> {
    sign::sign_json(
        idx,
        key,
        None,
        days.unwrap_or(DEFAULT_EXPIRES_DAYS),
        "sbx release index",
        path,
        &sig_path(path),
    )
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
    let key = need(o.key, "--key or --key-env", cmd)?;
    let mut idx = load_verified(&path, &key, o.signed_by.as_deref())?;
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
    let list = releases(&mut idx)?;
    list.retain(|r| r["version"] != json!(ver));
    list.push(entry);
    sign_and_save(&path, &mut idx, &key, o.expires_days)?;
    Ok(platforms)
}

/// `release-yank`.
pub fn yank(args: &[String]) -> Result<(), String> {
    let cmd = "release-yank";
    let o = parse(cmd, args)?;
    let path = need(o.index, "--index", cmd)?;
    let ver = version(&need(o.version, "--version", cmd)?)?;
    let key = need(o.key, "--key or --key-env", cmd)?;
    let mut idx = load_verified(&path, &key, o.signed_by.as_deref())?;
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
    sign_and_save(&path, &mut idx, &key, o.expires_days).map(|_| ())
}

/// `release-sign`. 새 `serial`을 돌려준다.
pub fn sign_index(args: &[String]) -> Result<u64, String> {
    let cmd = "release-sign";
    let o = parse(cmd, args)?;
    let path = need(o.index, "--index", cmd)?;
    let key = need(o.key, "--key or --key-env", cmd)?;
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let mut idx = load_verified(&path, &key, o.signed_by.as_deref())?;
    sign_and_save(&path, &mut idx, &key, o.expires_days)
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
        for p in ["linux-x86_64", "darwin-aarch64"] {
            std::fs::create_dir_all(root.join(format!("artifacts/sbx-{p}"))).unwrap();
            std::fs::write(root.join(format!("artifacts/sbx-{p}/sbx-{p}.tar.gz")), p).unwrap();
        }
        std::fs::write(
            root.join("artifacts/sbx-linux-x86_64/sbx-linux-x86_64.tar.gz.sha256"),
            b"x",
        )
        .unwrap();
        let ws = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let file = root.join("releases.json");
        let f = file.to_str().unwrap();
        let assets = root.join("artifacts");
        let a = assets.to_str().unwrap();
        let base = url::Url::from_directory_path(&assets).unwrap();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let key = root.join("k.key");
        std::fs::write(&key, kp.sk.to_box(None).unwrap().into_string()).unwrap();
        let k = key.to_str().unwrap();
        let add = |v: &str, extra: &[&str]| {
            let mut args = s(&[
                "--index",
                f,
                "--version",
                v,
                "--assets",
                a,
                "--base-url",
                base.as_str(),
                "--key",
                k,
            ]);
            args.extend(s(extra));
            index(ws, &args)
        };
        for v in ["v0.2.1", "0.2.2"] {
            assert_eq!(add(v, &[]).unwrap(), ["darwin-aarch64", "linux-x86_64"]);
        }
        // 다시 쓰면 바꾼다(겹치지 않는다).
        add("0.2.2", &["--security"]).unwrap();
        yank(&s(&[
            "--index",
            f,
            "--version",
            "0.2.1",
            "--advisory",
            "use 0.2.2",
            "--key",
            k,
        ]))
        .unwrap();
        assert!(yank(&s(&["--index", f, "--version", "9.9.9", "--key", k])).is_err());
        assert!(add("0.2", &[]).is_err());
        let serial = sign_index(&s(&["--index", f, "--key", k])).unwrap();

        // 서명한 뒤 누가 고친 색인에는 서명하지 않는다(다른 키, 서명 없음, 아주 큰 serial도).
        let signed = std::fs::read_to_string(&file).unwrap();
        let signed_sig = std::fs::read_to_string(sig_path(&file)).unwrap();
        let restore = || {
            std::fs::write(&file, &signed).unwrap();
            std::fs::write(sig_path(&file), &signed_sig).unwrap();
        };
        std::fs::write(
            &file,
            signed.replace("\"yanked\": true", "\"yanked\": false"),
        )
        .unwrap();
        let e = sign_index(&s(&["--index", f, "--key", k])).unwrap_err();
        assert!(e.contains("not signed by this key"), "{e}");
        assert!(yank(&s(&["--index", f, "--version", "0.2.2", "--key", k])).is_err());
        assert!(add("0.2.3", &[]).is_err());
        restore();
        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let other_key = root.join("other.key");
        std::fs::write(&other_key, other.sk.to_box(None).unwrap().into_string()).unwrap();
        assert!(sign_index(&s(&["--index", f, "--key", other_key.to_str().unwrap()])).is_err());
        let mut far: Value = serde_json::from_str(&signed).unwrap();
        far["serial"] = json!(u64::MAX - 1);
        let body = serde_json::to_string_pretty(&far).unwrap();
        let sig = minisign::sign(None, &kp.sk, body.as_bytes(), None, None).unwrap();
        std::fs::write(&file, &body).unwrap();
        std::fs::write(sig_path(&file), sig.into_string()).unwrap();
        let e = sign_index(&s(&["--index", f, "--key", k])).unwrap_err();
        assert!(e.contains("ahead of now"), "{e}");
        restore();
        std::fs::remove_file(sig_path(&file)).unwrap();
        let e = sign_index(&s(&["--index", f, "--key", k])).unwrap_err();
        assert!(e.contains("nobody signed"), "{e}");
        restore();
        // 색인 키 바꾸기: 새 키로, 지금 파일을 서명한 옛 공개키를 대고.
        let ok = other_key.to_str().unwrap();
        let old_pk = kp.pk.to_base64();
        sign_index(&s(&["--index", f, "--key", ok, "--signed-by", &old_pk])).unwrap();
        assert!(
            sign_index(&s(&["--index", f, "--key", k])).is_err(),
            "now signed by the new key"
        );
        restore();

        // 클라이언트가 읽는다. 묶음 URL은 base/<이름>이라 그곳에 두고 서명한다.
        let linux = assets.join("sbx-linux-x86_64.tar.gz");
        std::fs::copy(
            assets.join("sbx-linux-x86_64/sbx-linux-x86_64.tar.gz"),
            &linux,
        )
        .unwrap();
        let sigs = sign_files(&s(&["--key", k, linux.to_str().unwrap()])).unwrap();
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
        assert_eq!(std::fs::read(&dest).unwrap(), b"linux-x86_64");
        let _ = std::fs::remove_dir_all(&root);
    }
}
