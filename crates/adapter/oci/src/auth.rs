//! 레지스트리 자격 증명. sbx만의 저장소는 없다. 공개 형식만 읽고 쓴다.
//!
//! - Docker `config.json`: `auths`(`auth` = base64 `user:password`, `identitytoken`), `credHelpers`(레지스트리마다
//!   쓸 helper), `credsStore`(나머지의 helper) ([docker login](https://docs.docker.com/reference/cli/docker/login/)).
//! - containers-auth.json(podman, skopeo, buildah): 같은 `auths` 형식이고, 키에 저장소 경로가 올 수 있다
//!   (`quay.io/org`). 가장 긴 키가 이긴다.
//! - credential helper 프로토콜: `docker-credential-<이름> get|store|erase`, 표준 입출력은 서버 주소나 JSON
//!   ([docker-credential-helpers](https://github.com/docker/docker-credential-helpers)).
//!
//! 어느 파일을 읽고 쓸지는 부르는 쪽이 [`AuthFiles`]로 정한다(환경 변수와 홈 디렉터리를 이 크레이트는 보지 않는다).
//! 찾는 순서(파일마다, 파일은 주어진 순서로): 그 레지스트리의 `credHelpers` → `auths`의 자격 증명 → `credsStore`.
//! helper가 없거나 실패하면 자격 증명이 없는 것으로 본다(익명으로 계속한다: Docker Hub는 공개 이미지에도 401로
//! 토큰을 받게 하므로, 망가진 helper가 공개 이미지까지 막으면 안 된다). 파일이 JSON이 아니면 오류다.
//!
//! 비밀 값은 오류 문구와 `Debug`에 나오지 않는다.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

/// helper 한 번을 기다리는 시간(키체인이 묻는 창을 띄울 수 있다).
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(60);

/// Docker Hub의 자격 증명 키(Docker가 쓰는 그대로).
pub const DOCKER_HUB_KEY: &str = "https://index.docker.io/v1/";

/// 자격 증명 하나.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential {
    /// 사용자 이름. `<token>`이면 `secret`이 identity token(OAuth2 refresh token)이다.
    pub username: String,
    /// 비밀번호, 접근 토큰, 또는 identity token.
    pub secret: String,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("username", &self.username)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl Credential {
    /// identity token이면 그것.
    pub fn identity_token(&self) -> Option<&str> {
        (self.username == "<token>").then_some(self.secret.as_str())
    }
}

/// 읽고 쓰는 파일.
#[derive(Debug, Clone, Default)]
pub struct AuthFiles {
    /// 찾는 순서대로. 없는 파일은 건너뛴다.
    pub read: Vec<PathBuf>,
    /// `sbx login`이 쓰고 `logout`이 지우는 파일.
    pub write: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    #[serde(default)]
    auths: BTreeMap<String, AuthEntry>,
    #[serde(default)]
    cred_helpers: BTreeMap<String, String>,
    #[serde(default)]
    creds_store: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct AuthEntry {
    #[serde(default)]
    auth: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    identitytoken: Option<String>,
}

/// 레지스트리 호스트를 비교할 꼴로: Docker Hub의 다른 이름은 `docker.io`, 나머지는 그대로(소문자).
fn canonical_host(host: &str) -> String {
    let h = host.to_ascii_lowercase();
    match h.as_str() {
        "index.docker.io" | "registry-1.docker.io" | "registry.hub.docker.com" => {
            "docker.io".into()
        }
        _ => h,
    }
}

/// 설정 파일의 키를 `호스트[/경로]` 꼴로: 앞의 `https://`·`http://`, 끝의 `/v1/`·`/v2/`·`/`를 뗀다.
fn canonical_key(key: &str) -> String {
    let k = key
        .strip_prefix("https://")
        .or_else(|| key.strip_prefix("http://"))
        .unwrap_or(key);
    let k = k.trim_end_matches('/');
    let k = k
        .strip_suffix("/v1")
        .or_else(|| k.strip_suffix("/v2"))
        .unwrap_or(k);
    match k.split_once('/') {
        Some((h, rest)) => format!("{}/{rest}", canonical_host(h)),
        None => canonical_host(k),
    }
}

/// 사용자가 준 레지스트리(`ghcr.io`, `https://ghcr.io/`, `docker.io`)를 호스트로.
pub fn registry_host(given: &str) -> String {
    let k = canonical_key(given.trim());
    k.split('/').next().unwrap_or_default().to_owned()
}

/// helper와 `auths`에 쓰는 서버 키. Docker Hub는 Docker가 쓰는 키다.
pub fn server_key(host: &str) -> String {
    let h = canonical_host(host);
    if h == "docker.io" {
        DOCKER_HUB_KEY.into()
    } else {
        h
    }
}

fn read_config(path: &Path) -> Result<Option<Config>, String> {
    match std::fs::read(path) {
        Ok(b) if b.iter().all(u8::is_ascii_whitespace) => Ok(Some(Config::default())),
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// 가장 긴 키가 맞는 값. `want`는 `호스트/저장소`, 키는 그것이거나 그 앞부분(경로 경계에서)이어야 한다.
fn best<'a, V>(map: &'a BTreeMap<String, V>, host: &str, repository: &str) -> Option<&'a V> {
    let want = format!("{}/{repository}", canonical_host(host));
    map.iter()
        .map(|(k, v)| (canonical_key(k), v))
        .filter(|(k, _)| {
            want == *k
                || (want.starts_with(k.as_str()) && want.as_bytes().get(k.len()) == Some(&b'/'))
        })
        .max_by_key(|(k, _)| k.len())
        .map(|(_, v)| v)
}

fn from_entry(e: &AuthEntry) -> Option<Credential> {
    if let Some(t) = e.identitytoken.as_deref().filter(|t| !t.is_empty()) {
        return Some(Credential {
            username: "<token>".into(),
            secret: t.into(),
        });
    }
    if let Some(a) = e.auth.as_deref().filter(|a| !a.is_empty()) {
        let raw = base64_decode(a)?;
        let s = String::from_utf8(raw).ok()?;
        let (u, p) = s.split_once(':')?;
        return Some(Credential {
            username: u.into(),
            secret: p.into(),
        });
    }
    match (&e.username, &e.password) {
        (Some(u), Some(p)) if !u.is_empty() => Some(Credential {
            username: u.clone(),
            secret: p.clone(),
        }),
        _ => None,
    }
}

/// `host`의 `repository`에 쓸 자격 증명을 찾는다. 블로킹이다(helper를 부를 수 있다).
pub fn lookup(
    files: &AuthFiles,
    host: &str,
    repository: &str,
) -> Result<Option<Credential>, String> {
    for path in &files.read {
        let Some(cfg) = read_config(path)? else {
            continue;
        };
        if let Some(helper) = best(&cfg.cred_helpers, host, repository) {
            return Ok(helper_get(helper, &server_key(host)));
        }
        if let Some(c) = best(&cfg.auths, host, repository).and_then(from_entry) {
            return Ok(Some(c));
        }
        if let Some(store) = cfg.creds_store.as_deref().filter(|s| !s.is_empty())
            && let Some(c) = helper_get(store, &server_key(host))
        {
            return Ok(Some(c));
        }
    }
    Ok(None)
}

/// helper 이름: 프로그램 이름의 일부이므로 경로가 될 글자를 받지 않는다.
fn helper_program(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        && !name.starts_with('.');
    ok.then(|| format!("docker-credential-{name}"))
}

/// helper를 부른다. (성공했는가, 표준 출력, 표준 오류의 첫 줄).
fn helper(name: &str, action: &str, input: &[u8]) -> Result<(bool, Vec<u8>, String), String> {
    let program =
        helper_program(name).ok_or_else(|| format!("invalid credential helper name {name:?}"))?;
    let mut child = Command::new(&program)
        .arg(action)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(input);
    }
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut o = Vec::new();
        let mut e = Vec::new();
        if let Some(s) = out.as_mut() {
            let _ = s.take(1 << 20).read_to_end(&mut o);
        }
        if let Some(s) = err.as_mut() {
            let _ = s.take(64 << 10).read_to_end(&mut e);
        }
        (o, e)
    });
    let deadline = Instant::now() + HELPER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{program} {action}: no answer within {}s",
                    HELPER_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("{program}: {e}")),
        }
    };
    let (o, e) = reader.join().unwrap_or_default();
    let first = String::from_utf8_lossy(&o)
        .lines()
        .chain(String::from_utf8_lossy(&e).lines())
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    Ok((status.success(), o, first))
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct HelperCredential {
    #[serde(default)]
    username: String,
    #[serde(default)]
    secret: String,
}

fn helper_get(name: &str, server: &str) -> Option<Credential> {
    let (ok, out, _) = helper(name, "get", server.as_bytes()).ok()?;
    if !ok {
        return None;
    }
    let c: HelperCredential = serde_json::from_slice(&out).ok()?;
    (!c.secret.is_empty()).then_some(Credential {
        username: c.username,
        secret: c.secret,
    })
}

/// 쓸 helper: 그 레지스트리의 `credHelpers`, 없으면 `credsStore`.
fn write_helper(cfg: &Config, host: &str) -> Option<String> {
    best(&cfg.cred_helpers, host, "")
        .cloned()
        .or_else(|| cfg.creds_store.clone().filter(|s| !s.is_empty()))
}

fn load_value(path: &Path) -> Result<serde_json::Value, String> {
    match std::fs::read(path) {
        Ok(b) if b.iter().all(u8::is_ascii_whitespace) => Ok(serde_json::json!({})),
        Ok(b) => {
            let v: serde_json::Value =
                serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", path.display()))?;
            if v.is_object() {
                Ok(v)
            } else {
                Err(format!("{}: not a JSON object", path.display()))
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::json!({})),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// 같은 디렉터리의 임시 파일에 쓰고 바꾼다. 비밀이 들어 있으므로 사용자만 읽는다(0600).
fn save_value(path: &Path, v: &serde_json::Value) -> Result<(), String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let dir = path
        .parent()
        .ok_or_else(|| format!("{}: no parent directory", path.display()))?;
    if !dir.as_os_str().is_empty() {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir).map_err(err)?;
    }
    let mut body = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    body.push(b'\n');
    let tmp = path.with_extension(format!("sbx-{}.tmp", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let written = (|| {
        let mut f = o.open(&tmp)?;
        f.write_all(&body)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(err)
}

/// 어디에 남겼는가.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stored {
    /// 그 파일의 `auths`에.
    File(PathBuf),
    /// 그 helper에(파일의 `auths`에는 빈 항목만).
    Helper(String),
}

/// `host`의 자격 증명을 남긴다: 그 레지스트리에 helper가 정해져 있으면 helper에, 아니면 [`AuthFiles::write`]의
/// `auths`에 (Docker와 같다). 파일의 다른 내용은 그대로 둔다.
pub fn store(files: &AuthFiles, host: &str, cred: &Credential) -> Result<Stored, String> {
    let path = files
        .write
        .as_deref()
        .ok_or("no credentials file to write (set DOCKER_CONFIG or HOME)")?;
    let key = server_key(host);
    let mut v = load_value(path)?;
    let cfg: Config = serde_json::from_value(v.clone()).unwrap_or_default();
    let entry = match write_helper(&cfg, host) {
        Some(h) => {
            let input = serde_json::json!({ "ServerURL": key, "Username": cred.username, "Secret": cred.secret });
            let (ok, _, msg) = helper(&h, "store", input.to_string().as_bytes())?;
            if !ok {
                return Err(format!("docker-credential-{h} store: {msg}"));
            }
            serde_json::json!({})
        }
        None if cred.identity_token().is_some() => {
            serde_json::json!({ "identitytoken": cred.secret })
        }
        None => serde_json::json!({
            "auth": base64_encode(format!("{}:{}", cred.username, cred.secret).as_bytes())
        }),
    };
    let auths = v
        .as_object_mut()
        .map(|o| o.entry("auths").or_insert_with(|| serde_json::json!({})))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| format!("{}: `auths` is not an object", path.display()))?;
    // 같은 레지스트리를 가리키는 다른 꼴의 키(`https://ghcr.io`)는 지운다. 남기면 어느 것이 쓰일지 모른다.
    let want = canonical_key(&key);
    auths.retain(|k, _| canonical_key(k) != want);
    auths.insert(key, entry);
    save_value(path, &v)?;
    Ok(match write_helper(&cfg, host) {
        Some(h) => Stored::Helper(h),
        None => Stored::File(path.to_owned()),
    })
}

/// `host`의 자격 증명을 지운다. 지운 것이 있었으면 `true`.
pub fn erase(files: &AuthFiles, host: &str) -> Result<bool, String> {
    let Some(path) = files.write.as_deref() else {
        return Ok(false);
    };
    let key = server_key(host);
    let mut v = load_value(path)?;
    let cfg: Config = serde_json::from_value(v.clone()).unwrap_or_default();
    let mut removed = false;
    if let Some(h) = write_helper(&cfg, host) {
        // 없던 것을 지우면 helper가 실패한다. 그것은 지운 것이 없다는 뜻이다.
        if let Ok((ok, _, _)) = helper(&h, "erase", key.as_bytes()) {
            removed |= ok;
        }
    }
    let want = canonical_key(&key);
    if let Some(auths) = v
        .get_mut("auths")
        .and_then(serde_json::Value::as_object_mut)
    {
        let before = auths.len();
        auths.retain(|k, _| canonical_key(k) != want);
        if auths.len() != before {
            removed = true;
            save_value(path, &v)?;
        }
    }
    Ok(removed)
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 표준 base64(채움 있음).
pub fn base64_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// 표준 base64를 푼다(채움은 있어도 없어도 된다). 잘못되면 `None`.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim().trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = B64.iter().position(|&x| x == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    (bits < 6).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sbx-auth-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn base64_round_trips() {
        for s in ["", "a", "ab", "abc", "user:pa:ss", "한글:비밀"] {
            let e = base64_encode(s.as_bytes());
            assert_eq!(base64_decode(&e).unwrap(), s.as_bytes(), "{s} {e}");
        }
        assert_eq!(base64_encode(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_decode("dXNlcjpwYXNz").unwrap(), b"user:pass");
        assert!(base64_decode("!!").is_none());
    }

    #[test]
    fn keys_match_by_host_and_longest_path() {
        assert_eq!(canonical_key("https://index.docker.io/v1/"), "docker.io");
        assert_eq!(canonical_key("http://localhost:5000/"), "localhost:5000");
        assert_eq!(registry_host("https://GHCR.io/"), "ghcr.io");
        assert_eq!(server_key("registry-1.docker.io"), DOCKER_HUB_KEY);
        let mut m = BTreeMap::new();
        m.insert("quay.io".to_owned(), 1);
        m.insert("quay.io/org".to_owned(), 2);
        m.insert("quay.io/organization".to_owned(), 3);
        assert_eq!(best(&m, "quay.io", "org/app"), Some(&2));
        assert_eq!(best(&m, "quay.io", "orgx/app"), Some(&1));
        assert_eq!(best(&m, "quay.io", "organization/app"), Some(&3));
        assert_eq!(best(&m, "ghcr.io", "org/app"), None);
    }

    #[test]
    fn auths_are_read_in_order_and_secrets_stay_out_of_debug() {
        let d = dir("read");
        let a = d.join("a.json");
        let b = d.join("b.json");
        std::fs::write(&a, r#"{"auths":{"ghcr.io":{}}}"#).unwrap();
        std::fs::write(
            &b,
            format!(
                r#"{{"auths":{{"https://ghcr.io/":{{"auth":"{}"}},"https://index.docker.io/v1/":{{"identitytoken":"rt"}}}}}}"#,
                base64_encode(b"me:s3cret")
            ),
        )
        .unwrap();
        let files = AuthFiles {
            read: vec![d.join("missing.json"), a, b],
            write: None,
        };
        let c = lookup(&files, "ghcr.io", "me/app").unwrap().unwrap();
        assert_eq!((c.username.as_str(), c.secret.as_str()), ("me", "s3cret"));
        assert!(!format!("{c:?}").contains("s3cret"));
        let hub = lookup(&files, "registry-1.docker.io", "library/alpine")
            .unwrap()
            .unwrap();
        assert_eq!(hub.identity_token(), Some("rt"));
        assert_eq!(lookup(&files, "quay.io", "x").unwrap(), None);

        std::fs::write(d.join("bad.json"), "{").unwrap();
        let bad = AuthFiles {
            read: vec![d.join("bad.json")],
            write: None,
        };
        assert!(lookup(&bad, "ghcr.io", "x").is_err());
    }

    #[test]
    fn a_missing_helper_means_no_credential() {
        let d = dir("helper");
        let p = d.join("config.json");
        std::fs::write(
            &p,
            r#"{"credsStore":"sbx-test-no-such-helper","credHelpers":{"x.io":"../evil"}}"#,
        )
        .unwrap();
        let files = AuthFiles {
            read: vec![p],
            write: None,
        };
        assert_eq!(lookup(&files, "ghcr.io", "a").unwrap(), None);
        assert_eq!(lookup(&files, "x.io", "a").unwrap(), None);
        assert!(helper_program("../evil").is_none());
        assert!(helper_program(".hidden").is_none());
        assert_eq!(
            helper_program("osxkeychain").as_deref(),
            Some("docker-credential-osxkeychain")
        );
    }

    #[test]
    fn store_keeps_other_settings_and_erase_removes_every_spelling() {
        let d = dir("store");
        let p = d.join("sub").join("config.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(
            &p,
            r#"{"auths":{"https://ghcr.io":{"auth":"b2xkOm9sZA=="}},"proxies":{"default":{"httpProxy":"x"}}}"#,
        )
        .unwrap();
        let files = AuthFiles {
            read: vec![p.clone()],
            write: Some(p.clone()),
        };
        let cred = Credential {
            username: "me".into(),
            secret: "new".into(),
        };
        assert_eq!(
            store(&files, "ghcr.io", &cred).unwrap(),
            Stored::File(p.clone())
        );
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["proxies"]["default"]["httpProxy"], "x");
        assert_eq!(v["auths"].as_object().unwrap().len(), 1);
        assert_eq!(lookup(&files, "ghcr.io", "a").unwrap(), Some(cred));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(erase(&files, "https://ghcr.io/").unwrap());
        assert!(!erase(&files, "ghcr.io").unwrap());
        assert_eq!(lookup(&files, "ghcr.io", "a").unwrap(), None);

        // 새 파일과 Docker Hub의 키.
        let fresh = AuthFiles {
            read: vec![d.join("new/config.json")],
            write: Some(d.join("new/config.json")),
        };
        store(
            &fresh,
            "docker.io",
            &Credential {
                username: "u".into(),
                secret: "p".into(),
            },
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(d.join("new/config.json")).unwrap()).unwrap();
        assert_eq!(v["auths"][DOCKER_HUB_KEY]["auth"], base64_encode(b"u:p"));
    }
}
