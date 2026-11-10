//! 저장소 색인 서명(minisign). 형식과 클라이언트 규칙은 `sbx-executor-repo` 크레이트 문서의 "서명"이다.
//!
//! ```text
//! cargo xtask repo-keygen --out <접두사>
//! cargo xtask repo-sign --repo <디렉터리> (--key <파일> | --key-env <변수>) [--serial <N>] [--expires-days <D>]
//! ```
//!
//! - `repo-keygen`: 키 한 쌍을 만든다. `<접두사>.pub`(한 줄 base64 공개키, `sbx executor repo add --key`에 준다)와
//!   `<접두사>.key`(비밀키, 암호 없음, 권한 0600). 비밀키는 저장소를 고치는 곳(CI 비밀 값)에만 두고, 오프라인 사본을
//!   따로 둔다. 이미 있으면 덮어쓰지 않는다.
//! - `repo-sign`: `<디렉터리>/v2/index.json`의 맨 위 `serial`과 `expires`를 새로 정하고 서명해
//!   `v2/index.json.minisig`를 쓴다. `serial`의 기본은 지금 Unix 초와 (있던 값 + 1) 중 큰 것(늘 커진다).
//!   `expires`는 지금부터 `--expires-days`(기본 90)일. 색인을 고칠 때마다(`repo-add` 뒤), 그리고 기한 전에 다시 한다.
//!   `--key-env`는 비밀키 내용을 담은 환경 변수(CI가 비밀 값을 파일로 쓰지 않게).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `repo-keygen`. 공개키를 돌려준다.
pub fn keygen(args: &[String]) -> Result<String, String> {
    let out = match args {
        [flag, prefix] if flag == "--out" => PathBuf::from(prefix),
        _ => return Err("usage: cargo xtask repo-keygen --out <prefix>".into()),
    };
    let with = |ext: &str| {
        let mut p = out.clone().into_os_string();
        p.push(ext);
        PathBuf::from(p)
    };
    let (pub_file, key_file) = (with(".pub"), with(".key"));
    for f in [&pub_file, &key_file] {
        if f.exists() {
            return Err(format!("{} already exists", f.display()));
        }
    }
    let kp = minisign::KeyPair::generate_unencrypted_keypair().map_err(|e| e.to_string())?;
    let pk = kp.pk.to_base64();
    let sk = kp
        .sk
        .to_box(Some("sbx executor repository signing key"))
        .map_err(|e| e.to_string())?
        .into_string();
    write_private(&key_file, sk.as_bytes())?;
    std::fs::write(&pub_file, format!("{pk}\n"))
        .map_err(|e| format!("{}: {e}", pub_file.display()))?;
    Ok(pk)
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
        .and_then(|mut f| f.write_all(bytes))
        .map_err(|e| format!("{}: {e}", path.display()))
}

struct Options {
    repo: PathBuf,
    key: String,
    serial: Option<u64>,
    expires_days: u64,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let (mut repo, mut key, mut serial, mut days) = (None, None, None, 90);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--repo" => repo = Some(PathBuf::from(val()?)),
            "--key" => {
                let f = val()?;
                key = Some(std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?);
            }
            "--key-env" => {
                let v = val()?;
                key = Some(
                    std::env::var(&v)
                        .map_err(|_| format!("environment variable {v} is not set"))?,
                );
            }
            "--serial" => serial = Some(val()?.parse().map_err(|_| "--serial needs a number")?),
            "--expires-days" => {
                days = val()?
                    .parse()
                    .map_err(|_| "--expires-days needs a number")?;
                if days == 0 {
                    return Err("--expires-days must be at least 1".into());
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Options {
        repo: repo.ok_or("--repo is required")?,
        key: key.ok_or("--key or --key-env is required")?,
        serial,
        expires_days: days,
    })
}

/// `repo-sign`. 서명 파일을 돌려준다.
pub fn sign(args: &[String]) -> Result<PathBuf, String> {
    let o = parse(args)?;
    let index_file = o.repo.join("v2/index.json");
    let text = std::fs::read_to_string(&index_file)
        .map_err(|e| format!("{}: {e}", index_file.display()))?;
    let mut index: Value =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", index_file.display()))?;
    if index["schema"] != json!(2) {
        return Err(format!("{} is not a schema 2 index", index_file.display()));
    }
    let old = index["serial"].as_u64().unwrap_or(0);
    let serial = match o.serial {
        Some(s) if s <= old => {
            return Err(format!(
                "--serial {s} must be larger than the current {old}"
            ));
        }
        Some(s) => s,
        None => now().max(old + 1),
    };
    index["serial"] = json!(serial);
    index["expires"] = json!(now() + o.expires_days * 86_400);
    let body = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())? + "\n";
    let sk = minisign::SecretKeyBox::from_string(o.key.trim())
        .and_then(|b| b.into_unencrypted_secret_key())
        .map_err(|e| format!("cannot read the secret key: {e}"))?;
    let trusted = format!("sbx executor repository index serial {serial}");
    let sig = minisign::sign(None, &sk, body.as_bytes(), Some(&trusted), None)
        .map_err(|e| e.to_string())?;
    let sig_file = o.repo.join("v2/index.json.minisig");
    // 색인과 서명을 함께 바꾼다: 서명을 먼저 임시 파일에 쓰고, 색인, 서명 순으로 rename한다.
    let tmp = |p: &Path| p.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(tmp(&index_file), &body).map_err(|e| e.to_string())?;
    std::fs::write(tmp(&sig_file), sig.into_string()).map_err(|e| e.to_string())?;
    std::fs::rename(tmp(&index_file), &index_file).map_err(|e| e.to_string())?;
    std::fs::rename(tmp(&sig_file), &sig_file).map_err(|e| e.to_string())?;
    Ok(sig_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signed_repository_is_read_by_the_client() {
        let root = std::env::temp_dir().join(format!("sbx-xtask-sign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/v2")).unwrap();
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let prefix = root.join("k");
        let pk = keygen(&s(&["--out", prefix.to_str().unwrap()])).unwrap();
        assert!(
            keygen(&s(&["--out", prefix.to_str().unwrap()])).is_err(),
            "no overwrite"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.join("k.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(
            root.join("repo/v2/index.json"),
            r#"{"schema": 2, "executors": {}}"#,
        )
        .unwrap();
        let key = root.join("k.key");
        let sign_args = |extra: &[&str]| {
            let mut a = s(&[
                "--repo",
                root.join("repo").to_str().unwrap(),
                "--key",
                key.to_str().unwrap(),
            ]);
            a.extend(s(extra));
            a
        };
        sign(&sign_args(&[])).unwrap();
        let repo = sbx_executor_repo::Repository::new(
            "s",
            url::Url::from_directory_path(root.join("repo"))
                .unwrap()
                .as_str(),
        )
        .unwrap()
        .with_keys(vec![pk])
        .unwrap();
        let f = sbx_executor_repo::Fetcher::new().unwrap();
        let first = f.index(&repo).await.unwrap();
        assert!(first.serial.unwrap() >= now() && first.expires.unwrap() > now() + 89 * 86_400);
        // 다시 서명하면 serial이 커진다. 더 작은 serial은 거부한다.
        sign(&sign_args(&[])).unwrap();
        assert!(f.index(&repo).await.unwrap().serial > first.serial);
        assert!(sign(&sign_args(&["--serial", "1"])).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
