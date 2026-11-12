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
//!   `expires`는 지금부터 `--expires-days`(기본 90, 1~3650)일. 색인을 고칠 때마다(`repo-add` 뒤), 그리고 기한 전에 다시 한다.
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

/// `--expires-days`의 상한(10년). 그보다 길면 기한이 동결 공격을 막지 못하고, 아주 큰 값은 시각을 넘친다.
const MAX_EXPIRES_DAYS: u64 = 3650;

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
            "--key" | "--key-env" => key = Some(key_option(a, &val()?)?),
            "--serial" => serial = Some(val()?.parse().map_err(|_| "--serial needs a number")?),
            "--expires-days" => days = expires_days(&val()?)?,
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
    let mut index = read_json(&index_file)?;
    if index["schema"] != json!(2) {
        return Err(format!("{} is not a schema 2 index", index_file.display()));
    }
    let sig_file = o.repo.join("v2/index.json.minisig");
    sign_json(
        &mut index,
        &o.key,
        o.serial,
        o.expires_days,
        "sbx executor repository index",
        &index_file,
        &sig_file,
    )?;
    Ok(sig_file)
}

/// JSON 파일을 읽는다.
pub fn read_json(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// 비밀키(minisign 상자, 암호 없음) 내용을 읽는다.
pub fn secret_key(text: &str) -> Result<minisign::SecretKey, String> {
    minisign::SecretKeyBox::from_string(text.trim())
        .and_then(|b| b.into_unencrypted_secret_key())
        .map_err(|e| format!("cannot read the secret key: {e}"))
}

/// `index`의 맨 위 `serial`(주지 않으면 지금 Unix 초와 있던 값 + 1 중 큰 것)과 `expires`(지금부터 `days`일)를
/// 새로 정하고 서명해 `file`과 `sig_file`을 함께 바꾼다(서명을 먼저 임시 파일에 쓰고, 색인, 서명 순으로 rename).
pub fn sign_json(
    index: &mut Value,
    key: &str,
    serial: Option<u64>,
    days: u64,
    what: &str,
    file: &Path,
    sig_file: &Path,
) -> Result<u64, String> {
    let old = index["serial"].as_u64().unwrap_or(0);
    let serial = match serial {
        Some(s) if s <= old => {
            return Err(format!(
                "--serial {s} must be larger than the current {old}"
            ));
        }
        Some(s) => s,
        None => now().max(
            old.checked_add(1)
                .ok_or("the index serial is at its maximum; it cannot grow")?,
        ),
    };
    index["serial"] = json!(serial);
    index["expires"] = json!(now() + days * 86_400);
    let body = serde_json::to_string_pretty(&index).map_err(|e| e.to_string())? + "\n";
    let sk = secret_key(key)?;
    let trusted = format!("{what} serial {serial}");
    let sig = minisign::sign(None, &sk, body.as_bytes(), Some(&trusted), None)
        .map_err(|e| e.to_string())?;
    let tmp = |p: &Path| p.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(tmp(file), &body).map_err(|e| e.to_string())?;
    std::fs::write(tmp(sig_file), sig.into_string()).map_err(|e| e.to_string())?;
    std::fs::rename(tmp(file), file).map_err(|e| e.to_string())?;
    std::fs::rename(tmp(sig_file), sig_file).map_err(|e| e.to_string())?;
    Ok(serial)
}

/// `--key <파일>`이나 `--key-env <변수>`의 값.
pub fn key_option(flag: &str, value: &str) -> Result<String, String> {
    match flag {
        "--key" => std::fs::read_to_string(value).map_err(|e| format!("{value}: {e}")),
        "--key-env" => {
            std::env::var(value).map_err(|_| format!("environment variable {value} is not set"))
        }
        _ => unreachable!("not a key option: {flag}"),
    }
}

/// `--expires-days`의 값(1~[`MAX_EXPIRES_DAYS`]).
pub fn expires_days(value: &str) -> Result<u64, String> {
    let days: u64 = value.parse().map_err(|_| "--expires-days needs a number")?;
    if !(1..=MAX_EXPIRES_DAYS).contains(&days) {
        return Err(format!("--expires-days must be 1 to {MAX_EXPIRES_DAYS}"));
    }
    Ok(days)
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
        let before = now();
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
        assert!(first.serial.unwrap() >= before && first.expires.unwrap() >= before + 90 * 86_400);
        // 다시 서명하면 serial이 커진다. 더 작은 serial은 거부한다.
        sign(&sign_args(&[])).unwrap();
        assert!(f.index(&repo).await.unwrap().serial > first.serial);
        assert!(sign(&sign_args(&["--serial", "1"])).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn signing_keeps_serials_growing_and_refuses_bad_input() {
        let root = std::env::temp_dir().join(format!("sbx-xtask-sign2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("repo/v2")).unwrap();
        let s = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        keygen(&s(&["--out", root.join("k").to_str().unwrap()])).unwrap();
        let index = root.join("repo/v2/index.json");
        let args = |extra: &[&str]| {
            let mut a = s(&[
                "--repo",
                root.join("repo").to_str().unwrap(),
                "--key",
                root.join("k.key").to_str().unwrap(),
            ]);
            a.extend(s(extra));
            a
        };
        let read = || -> Value {
            serde_json::from_str(&std::fs::read_to_string(&index).unwrap()).unwrap()
        };
        // 있던 serial이 지금보다 크면(시계가 늦은 곳, 손으로 준 serial) 그보다 하나 크게 한다.
        let future = now() + 1_000_000;
        std::fs::write(
            &index,
            format!(r#"{{"schema": 2, "serial": {future}, "executors": {{}}}}"#),
        )
        .unwrap();
        sign(&args(&[])).unwrap();
        assert_eq!(read()["serial"], json!(future + 1));
        sign(&args(&[
            "--serial",
            &(future + 10).to_string(),
            "--expires-days",
            "1",
        ]))
        .unwrap();
        let v = read();
        assert_eq!(v["serial"], json!(future + 10));
        let exp = v["expires"].as_u64().unwrap();
        assert!(exp > now() && exp <= now() + 86_400, "{exp}");
        // 다른 필드와 내용은 그대로 둔다.
        assert_eq!(v["executors"], json!({}));

        // serial이 최대값이면 더 키울 수 없다(넘쳐서 0이 되면 모든 클라이언트가 롤백으로 본다).
        std::fs::write(
            &index,
            format!(
                r#"{{"schema": 2, "serial": {}, "executors": {{}}}}"#,
                u64::MAX
            ),
        )
        .unwrap();
        assert!(sign(&args(&[])).unwrap_err().contains("maximum"));
        std::fs::write(&index, r#"{"schema": 2, "executors": {}}"#).unwrap();
        // 기한은 1일 이상 10년 이하. 넘치는 값도 거부한다.
        for bad in ["0", "3651", "18446744073709551615", "x"] {
            assert!(sign(&args(&["--expires-days", bad])).is_err(), "{bad}");
        }
        // schema 2가 아닌 색인, 없는 키 변수, 읽을 수 없는 키, 모르는 옵션.
        std::fs::write(&index, r#"{"schema": 1, "executors": {}}"#).unwrap();
        assert!(sign(&args(&[])).unwrap_err().contains("schema 2"));
        std::fs::write(&index, r#"{"schema": 2, "executors": {}}"#).unwrap();
        let no_env = s(&[
            "--repo",
            root.join("repo").to_str().unwrap(),
            "--key-env",
            "SBX_TEST_NO_SUCH_VAR_9",
        ]);
        assert!(sign(&no_env).unwrap_err().contains("not set"));
        std::fs::write(root.join("bad.key"), "not a key").unwrap();
        let bad_key = s(&[
            "--repo",
            root.join("repo").to_str().unwrap(),
            "--key",
            root.join("bad.key").to_str().unwrap(),
        ]);
        assert!(sign(&bad_key).unwrap_err().contains("secret key"));
        assert!(sign(&args(&["--what"])).is_err());
        // 실패한 서명은 색인을 바꾸지 않고 임시 파일도 남기지 않는다.
        assert_eq!(read(), json!({"schema": 2, "executors": {}}));
        let names: Vec<_> = std::fs::read_dir(root.join("repo/v2"))
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.iter().all(|n| !n.contains("tmp")), "{names:?}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
