//! Distribution Spec 클라이언트. pull에 필요한 GET만 한다.
//!
//! 인증: 401 응답의 `WWW-Authenticate`를 따른다. `Bearer`면 거기 적힌 곳(`realm`)에서 토큰을 받아 다시 요청하고,
//! `Basic`이면 자격 증명을 그대로 보낸다. 자격 증명은 그 레지스트리(사용자가 이미지 이름으로 고른 호스트)의 것만
//! 찾는다([`crate::auth`]). 있으면 토큰 요청에 Basic으로(identity token이면 OAuth2 refresh token으로 POST) 싣고,
//! 없으면 익명으로 받는다. 토큰은 저장소마다 메모리에만 둔다. 자격 증명을 실은 요청은 https로만 간다
//! (레지스트리 자체가 http인 `localhost`는 예외).
//!
//! `localhost`와 `127.0.0.1` 레지스트리는 http로, 나머지는 https로 접속한다. Docker와 같은 기본값이다.
//! `registries.conf`에서 `insecure`인 호스트([`Registry::with_insecure`])는 인증서를 검증하지 않는 https로, 연결이
//! 안 되면 http로 접속한다(containers/image와 같다). 그 호스트에는 http로도 자격 증명을 보낸다(사용자가 고른 설정).
//! blob 요청이 다른 호스트(CDN)로 redirect되면 `Authorization` 헤더는 따라가지 않는다(reqwest 기본 동작).
//!
//! **레지스트리가 고른 주소는 믿지 않는다.** 사용자가 이미지 이름으로 고른 호스트만 사설·로컬 주소여도 된다
//! (사내 레지스트리). 토큰 주소(`realm`)와 redirect는 레지스트리가 정하므로, 그 호스트는 인터넷 쪽 주소로만
//! 접속한다([`sbx_model::is_public_ip`]). 이름 해석 단계에서 걸러서 DNS로 사설 주소를 돌려줘도 막힌다.
//! 토큰 주소와 redirect는 https만 따른다(레지스트리 자체가 http인 `localhost`일 때만 http도 된다).
//! 응답이 멈추면 [`READ_TIMEOUT`] 뒤 끊는다.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Response, StatusCode, Url};
use serde::Deserialize;

use sbx_image::ImageError;

use crate::auth::{AuthFiles, Credential};

/// manifest와 config처럼 메모리로 읽는 문서의 최대 크기.
pub const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;

/// 401 응답의 Bearer 챌린지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    /// 토큰을 받을 주소.
    pub realm: String,
    /// `service` 값.
    pub service: Option<String>,
    /// `scope` 값.
    pub scope: Option<String>,
}

/// `WWW-Authenticate: Basic realm="..."`인가.
fn is_basic(header: &str) -> bool {
    header
        .trim()
        .split(|c: char| c.is_whitespace())
        .next()
        .is_some_and(|s| s.eq_ignore_ascii_case("basic"))
}

/// `WWW-Authenticate: Bearer realm="...",service="...",scope="..."`를 해석한다.
pub fn parse_challenge(header: &str) -> Option<Challenge> {
    let (scheme, rest) = header.trim().split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let mut params: HashMap<String, String> = HashMap::new();
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let key: String = chars.by_ref().take_while(|c| *c != '=').collect();
        if key.is_empty() {
            break;
        }
        let value = if chars.peek() == Some(&'"') {
            chars.next();
            let mut v = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => v.extend(chars.next()),
                    '"' => break,
                    c => v.push(c),
                }
            }
            v
        } else {
            chars.by_ref().take_while(|c| *c != ',').collect()
        };
        params.insert(key.trim().to_ascii_lowercase(), value);
    }
    Some(Challenge {
        realm: params.remove("realm")?,
        service: params.remove("service"),
        scope: params.remove("scope"),
    })
}

#[derive(Deserialize)]
struct TokenResponse {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    access_token: Option<String>,
}

/// 오류와 그 원인들을 `: `로 이은 한 줄. HTTP 클라이언트의 맨 위 오류("error sending request", "builder error")만으로는
/// 무엇이 잘못됐는지(연결 거부, 인증서 없음) 알 수 없다.
fn chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        let c_s = c.to_string();
        if !s.ends_with(&c_s) {
            s.push_str(": ");
            s.push_str(&c_s);
        }
        cur = c.source();
    }
    s
}

fn transport(e: impl std::error::Error) -> ImageError {
    ImageError::Transport(chain(&e))
}

/// 읽는 중 이만큼 아무것도 오지 않으면 끊는다.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// redirect를 따르는 최대 횟수.
pub const MAX_REDIRECTS: usize = 10;

/// 사용자가 이미지 이름으로 고른 호스트. 이 호스트만 사설·로컬 주소로 접속할 수 있다.
type Named = Arc<Mutex<HashSet<String>>>;

/// URL의 호스트 이름 (소문자, 포트와 IPv6 괄호 없이).
fn host_key(url: &Url) -> Option<String> {
    url.host_str().map(|h| {
        h.trim_start_matches('[')
            .trim_end_matches(']')
            .to_ascii_lowercase()
    })
}

/// 레지스트리 주소에서 호스트 이름 (`localhost:5000` → `localhost`, `[::1]:5000` → `::1`).
fn named_key(host: &str) -> String {
    let h = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    h.to_ascii_lowercase()
}

/// 레지스트리가 알려 준 주소(`realm`, redirect)로 가도 되는지. 이름은 [`Guard`]가 해석할 때 거른다.
fn check_target(url: &Url, from_http: bool, named: &Named) -> Result<(), String> {
    let key = host_key(url).ok_or_else(|| format!("{url}: no host"))?;
    let is_named = named.lock().is_ok_and(|n| n.contains(&key));
    match url.scheme() {
        "https" => {}
        "http" if from_http => {}
        other => {
            return Err(format!(
                "{url}: refusing a {other} address given by the registry"
            ));
        }
    }
    if let Ok(ip) = key.parse::<IpAddr>()
        && !is_named
        && !sbx_model::is_public_ip(ip)
    {
        return Err(format!(
            "{url}: the registry pointed at a private or local address"
        ));
    }
    Ok(())
}

/// 이름 해석. 사용자가 고르지 않은 호스트는 인터넷 쪽 주소만 남긴다.
struct Guard {
    named: Named,
}

impl Resolve for Guard {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        let is_named = self.named.lock().is_ok_and(|n| n.contains(&host));
        Box::pin(async move {
            let all: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0)).await?.collect();
            let kept: Vec<SocketAddr> = if is_named {
                all
            } else {
                all.into_iter()
                    .filter(|a| sbx_model::is_public_ip(a.ip()))
                    .collect()
            };
            if kept.is_empty() {
                return Err(format!(
                    "{host}: the registry sent sbx to a name with no public address"
                )
                .into());
            }
            Ok(Box::new(kept.into_iter()) as Addrs)
        })
    }
}

/// HTTP 클라이언트. `lax`면 인증서를 검증하지 않는다(`insecure` 호스트에만 쓴다).
fn client(named: &Named, lax: bool) -> Result<reqwest::Client, ImageError> {
    let for_redirect = named.clone();
    let policy = reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() >= MAX_REDIRECTS {
            return attempt.error(format!("more than {MAX_REDIRECTS} redirects"));
        }
        let from_http = attempt.previous().iter().all(|u| u.scheme() == "http");
        match check_target(attempt.url(), from_http, &for_redirect) {
            Ok(()) => attempt.follow(),
            Err(e) => attempt.error(e),
        }
    });
    reqwest::Client::builder()
        .user_agent(concat!("sbx/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(READ_TIMEOUT)
        .redirect(policy)
        .dns_resolver(Arc::new(Guard {
            named: named.clone(),
        }))
        .danger_accept_invalid_certs(lax)
        .build()
        .map_err(transport)
}

/// 요청에 싣는 인증.
#[derive(Clone)]
enum Auth {
    None,
    Bearer(String),
    Basic(Credential),
}

/// 레지스트리 클라이언트.
#[derive(Debug)]
pub struct Registry {
    http: reqwest::Client,
    /// `호스트/저장소` → 토큰.
    tokens: Mutex<HashMap<String, String>>,
    /// Basic 인증을 쓰는 호스트 → 자격 증명.
    basic: Mutex<HashMap<String, Credential>>,
    /// 자격 증명을 찾는 파일. 없으면 익명으로만 받는다.
    auth: Option<AuthFiles>,
    named: Named,
    /// `insecure` 호스트(포트 포함)와 그 호스트에 쓰는, 인증서를 검증하지 않는 클라이언트.
    insecure: BTreeSet<String>,
    lax: Option<reqwest::Client>,
    /// `insecure` 호스트 중 https가 안 되어 http로 바꾼 것.
    plain_http: Mutex<HashSet<String>>,
}

/// 로그인 확인의 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginCheck {
    /// 레지스트리가 받아들였다.
    Accepted,
    /// 레지스트리가 인증을 묻지 않았다(확인할 수 없다).
    NotAsked,
}

impl Registry {
    /// 만든다.
    pub fn new() -> Result<Self, ImageError> {
        let named: Named = Arc::default();
        Ok(Self {
            http: client(&named, false)?,
            tokens: Mutex::new(HashMap::new()),
            basic: Mutex::new(HashMap::new()),
            auth: None,
            named,
            insecure: BTreeSet::new(),
            lax: None,
            plain_http: Mutex::new(HashSet::new()),
        })
    }

    /// 이 호스트들(`registries.conf`의 `insecure`, 포트 포함)은 인증서를 검증하지 않고, https가 안 되면 http로.
    pub fn with_insecure(mut self, hosts: BTreeSet<String>) -> Result<Self, ImageError> {
        if !hosts.is_empty() {
            self.lax = Some(client(&self.named, true)?);
        }
        self.insecure = hosts;
        Ok(self)
    }

    /// 401이면 `files`에서 그 레지스트리의 자격 증명을 찾아 쓴다.
    pub fn with_auth(mut self, files: AuthFiles) -> Self {
        self.auth = Some(files);
        self
    }

    /// 그 레지스트리의 자격 증명. 파일 읽기와 helper는 블로킹이라 따로 돈다.
    async fn credential(
        &self,
        host: &str,
        repository: &str,
    ) -> Result<Option<Credential>, ImageError> {
        let Some(files) = self.auth.clone() else {
            return Ok(None);
        };
        let (host, repository) = (host.to_owned(), repository.to_owned());
        tokio::task::spawn_blocking(move || crate::auth::lookup(&files, &host, &repository))
            .await
            .map_err(|e| ImageError::Transport(e.to_string()))?
            .map_err(|e| ImageError::Invalid(format!("registry credentials: {e}")))
    }

    fn base(host: &str) -> String {
        let local = host == "localhost"
            || host.starts_with("localhost:")
            || host == "127.0.0.1"
            || host.starts_with("127.0.0.1:");
        format!("{}://{host}", if local { "http" } else { "https" })
    }

    /// 그 호스트의 주소 앞부분. `insecure`이고 https가 안 됐던 호스트는 http.
    fn base_for(&self, host: &str) -> String {
        if self.plain_http.lock().is_ok_and(|p| p.contains(host)) {
            format!("http://{host}")
        } else {
            Self::base(host)
        }
    }

    /// `GET <base>/v2/<rest>`. `insecure` 호스트에 https 연결이 안 되면 http로 한 번 더 하고 기억한다.
    async fn send_to(
        &self,
        host: &str,
        rest: &str,
        accept: &[&str],
        auth: &Auth,
    ) -> Result<(String, Response), ImageError> {
        let url = format!("{}/v2/{rest}", self.base_for(host));
        match self.send(&url, accept, auth).await {
            Err(ImageError::Transport(_))
                if self.insecure.contains(host) && url.starts_with("https:") =>
            {
                if let Ok(mut p) = self.plain_http.lock() {
                    p.insert(host.to_owned());
                }
                let url = format!("{}/v2/{rest}", self.base_for(host));
                let resp = self.send(&url, accept, auth).await?;
                Ok((url, resp))
            }
            r => Ok((url, r?)),
        }
    }

    /// URL의 호스트(포트 포함)가 `insecure`인가.
    fn is_insecure(&self, url: &str) -> bool {
        !self.insecure.is_empty()
            && Url::parse(url).is_ok_and(|u| {
                let host = match (u.host_str(), u.port()) {
                    (Some(h), Some(p)) => format!("{h}:{p}"),
                    (Some(h), None) => h.to_owned(),
                    _ => return false,
                };
                self.insecure.contains(&host)
            })
    }

    fn http_for(&self, url: &str) -> &reqwest::Client {
        match &self.lax {
            Some(lax) if self.is_insecure(url) => lax,
            _ => &self.http,
        }
    }

    /// 자격 증명은 https로만 보낸다. `localhost`와 `insecure` 호스트는 예외다.
    fn require_https(&self, url: &str) -> Result<(), ImageError> {
        if self.is_insecure(url) {
            Ok(())
        } else {
            require_https(url)
        }
    }

    /// `GET /v2/<repository>/<path>`. 401이면 인증해서 한 번 더 시도한다.
    pub async fn get(
        &self,
        host: &str,
        repository: &str,
        path: &str,
        accept: &[&str],
    ) -> Result<Response, ImageError> {
        if let Ok(mut n) = self.named.lock() {
            n.insert(named_key(host));
        }
        let rest = format!("{repository}/{path}");
        let key = format!("{host}/{repository}");
        let cached = match self.basic.lock().ok().and_then(|b| b.get(host).cloned()) {
            Some(c) => Auth::Basic(c),
            None => self
                .tokens
                .lock()
                .ok()
                .and_then(|t| t.get(&key).cloned())
                .map_or(Auth::None, Auth::Bearer),
        };
        let (url, mut resp) = self.send_to(host, &rest, accept, &cached).await?;
        let mut cred = None;
        if resp.status() == StatusCode::UNAUTHORIZED {
            let header = resp
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_owned();
            cred = self.credential(host, repository).await?;
            let auth = if is_basic(&header) {
                let c = cred.clone().ok_or_else(|| denied(&url, host, false))?;
                if let Ok(mut b) = self.basic.lock() {
                    b.insert(host.to_owned(), c.clone());
                }
                Auth::Basic(c)
            } else {
                let challenge = parse_challenge(&header).ok_or_else(|| {
                    ImageError::Transport(format!("{url}: 401 without a Bearer or Basic challenge"))
                })?;
                let token = self
                    .token(
                        &challenge,
                        Some(repository),
                        url.starts_with("http:"),
                        cred.as_ref(),
                    )
                    .await?;
                if let Ok(mut t) = self.tokens.lock() {
                    t.insert(key, token.clone());
                }
                Auth::Bearer(token)
            };
            resp = self.send(&url, accept, &auth).await?;
        }
        match resp.status() {
            s if s.is_success() => Ok(resp),
            StatusCode::NOT_FOUND => Err(ImageError::NotFound(url)),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(denied(&url, host, cred.is_some()))
            }
            s => Err(ImageError::Transport(format!("{url}: HTTP {s}"))),
        }
    }

    /// `sbx login`: 이 자격 증명을 레지스트리가 받아들이는지 본다(`GET /v2/`, Docker와 같다).
    pub async fn check_login(
        &self,
        host: &str,
        cred: &Credential,
    ) -> Result<LoginCheck, ImageError> {
        if let Ok(mut n) = self.named.lock() {
            n.insert(named_key(host));
        }
        let (url, resp) = self.send_to(host, "", &[], &Auth::None).await?;
        if resp.status() != StatusCode::UNAUTHORIZED {
            return if resp.status().is_success() {
                Ok(LoginCheck::NotAsked)
            } else {
                Err(ImageError::Transport(format!(
                    "{url}: HTTP {}",
                    resp.status()
                )))
            };
        }
        let header = resp
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        if is_basic(&header) {
            let resp = self.send(&url, &[], &Auth::Basic(cred.clone())).await?;
            return match resp.status() {
                s if s.is_success() => Ok(LoginCheck::Accepted),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(refused(host)),
                s => Err(ImageError::Transport(format!("{url}: HTTP {s}"))),
            };
        }
        let challenge = parse_challenge(&header).ok_or_else(|| {
            ImageError::Transport(format!("{url}: 401 without a Bearer or Basic challenge"))
        })?;
        self.token(&challenge, None, url.starts_with("http:"), Some(cred))
            .await?;
        Ok(LoginCheck::Accepted)
    }

    async fn send(&self, url: &str, accept: &[&str], auth: &Auth) -> Result<Response, ImageError> {
        let mut req = self.http_for(url).get(url);
        if !accept.is_empty() {
            req = req.header(ACCEPT, accept.join(", "));
        }
        req = match auth {
            Auth::None => req,
            Auth::Bearer(t) => req.bearer_auth(t),
            Auth::Basic(c) => {
                self.require_https(url)?;
                req.basic_auth(&c.username, Some(&c.secret))
            }
        };
        req.send().await.map_err(transport)
    }

    /// 토큰을 받는다. `repository`가 없으면(로그인 확인) 챌린지의 scope만 쓴다.
    async fn token(
        &self,
        ch: &Challenge,
        repository: Option<&str>,
        from_http: bool,
        cred: Option<&Credential>,
    ) -> Result<String, ImageError> {
        let mut url = Url::parse(&ch.realm).map_err(transport)?;
        check_target(&url, from_http, &self.named).map_err(ImageError::Transport)?;
        let scope = ch
            .scope
            .clone()
            .or_else(|| repository.map(|r| format!("repository:{r}:pull")));
        let resp = match cred {
            // OAuth2 refresh token (Docker의 identity token).
            Some(c) if c.identity_token().is_some() => {
                self.require_https(url.as_str())?;
                let mut form = Url::parse("x:/").map_err(transport)?;
                {
                    let mut q = form.query_pairs_mut();
                    q.append_pair("grant_type", "refresh_token")
                        .append_pair("refresh_token", &c.secret)
                        .append_pair("client_id", "sbx");
                    if let Some(s) = &ch.service {
                        q.append_pair("service", s);
                    }
                    if let Some(s) = &scope {
                        q.append_pair("scope", s);
                    }
                }
                self.http_for(url.as_str())
                    .post(url)
                    .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(form.query().unwrap_or_default().to_owned())
                    .send()
                    .await
                    .map_err(transport)?
            }
            _ => {
                {
                    let mut q = url.query_pairs_mut();
                    if let Some(s) = &ch.service {
                        q.append_pair("service", s);
                    }
                    if let Some(s) = &scope {
                        q.append_pair("scope", s);
                    }
                }
                let mut req = self.http_for(url.as_str()).get(url.clone());
                if let Some(c) = cred {
                    self.require_https(url.as_str())?;
                    req = req.basic_auth(&c.username, Some(&c.secret));
                }
                req.send().await.map_err(transport)?
            }
        };
        let status = resp.status();
        if !status.is_success() {
            if cred.is_some() && matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN)
            {
                return Err(ImageError::Transport(format!(
                    "token endpoint {}: the registry refused the credentials (HTTP {status}); check `sbx login`",
                    ch.realm
                )));
            }
            return Err(ImageError::Transport(format!(
                "token endpoint {}: HTTP {status}",
                ch.realm
            )));
        }
        let bytes = read_limited(resp, MAX_DOCUMENT_BYTES).await?.0;
        let t: TokenResponse = serde_json::from_slice(&bytes)
            .map_err(|e| ImageError::Invalid(format!("token response: {e}")))?;
        t.token
            .or(t.access_token)
            .ok_or_else(|| ImageError::Invalid("token response without token".into()))
    }
}

/// 자격 증명은 https로만 보낸다. 레지스트리 자체가 http인 `localhost`·`127.0.0.1`만 예외다.
fn require_https(url: &str) -> Result<(), ImageError> {
    let u = Url::parse(url).map_err(transport)?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1"));
    if u.scheme() == "https" || local {
        Ok(())
    } else {
        Err(ImageError::Transport(format!(
            "{url}: refusing to send registry credentials over http"
        )))
    }
}

fn denied(url: &str, host: &str, had_credentials: bool) -> ImageError {
    let login = crate::auth::registry_host(host);
    ImageError::Transport(if had_credentials {
        format!(
            "{url}: access denied with the stored credentials for {login} (`sbx login {login}` to replace them)"
        )
    } else {
        format!(
            "{url}: access denied (private image or wrong name; `sbx login {login}` if it is private)"
        )
    })
}

fn refused(host: &str) -> ImageError {
    ImageError::Transport(format!(
        "{host}: the registry refused the username or password"
    ))
}

/// 응답 본문을 최대 `limit` 바이트까지 읽는다. 넘으면 오류. `Content-Type`도 돌려준다.
pub async fn read_limited(
    mut resp: Response,
    limit: usize,
) -> Result<(Vec<u8>, Option<String>), ImageError> {
    let content_type = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(transport)? {
        if out.len() + chunk.len() > limit {
            return Err(ImageError::Invalid(format!(
                "document larger than {limit} bytes"
            )));
        }
        out.extend_from_slice(&chunk);
    }
    Ok((out, content_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_hub_challenge() {
        let c = parse_challenge(
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#,
        )
        .unwrap();
        assert_eq!(c.realm, "https://auth.docker.io/token");
        assert_eq!(c.service.as_deref(), Some("registry.docker.io"));
        assert_eq!(c.scope.as_deref(), Some("repository:library/alpine:pull"));
    }

    #[test]
    fn challenge_with_commas_in_scope_and_no_service() {
        let c = parse_challenge(
            r#"bearer realm="https://ghcr.io/token", scope="repository:a/b:pull,push""#,
        )
        .unwrap();
        assert_eq!(c.scope.as_deref(), Some("repository:a/b:pull,push"));
        assert_eq!(c.service, None);
    }

    #[test]
    fn non_bearer_or_missing_realm() {
        assert!(parse_challenge(r#"Basic realm="x""#).is_none());
        assert!(parse_challenge(r#"Bearer service="x""#).is_none());
    }

    #[test]
    fn local_registries_use_http() {
        assert_eq!(Registry::base("localhost:5000"), "http://localhost:5000");
        assert_eq!(
            Registry::base("registry-1.docker.io"),
            "https://registry-1.docker.io"
        );
    }

    #[test]
    fn named_keys_drop_ports_and_brackets() {
        assert_eq!(named_key("localhost:5000"), "localhost");
        assert_eq!(named_key("Registry.Corp"), "registry.corp");
        assert_eq!(named_key("[::1]:5000"), "::1");
        assert_eq!(named_key("10.0.0.5:5000"), "10.0.0.5");
    }

    #[test]
    fn registry_given_addresses_must_be_public_and_https() {
        let named: Named = Arc::default();
        named.lock().unwrap().insert("10.0.0.5".into());
        let ok = |u: &str, http: bool| check_target(&Url::parse(u).unwrap(), http, &named);
        assert!(ok("https://auth.docker.io/token", false).is_ok());
        assert!(ok("http://auth.docker.io/token", false).is_err());
        assert!(ok("file:///etc/passwd", true).is_err());
        assert!(ok("https://169.254.169.254/latest", false).is_err());
        assert!(ok("https://[::ffff:169.254.169.254]/", false).is_err());
        assert!(ok("https://[64:ff9b::a00:1]/", false).is_err());
        // 사용자가 고른 사내 레지스트리는 된다.
        assert!(ok("https://10.0.0.5/token", false).is_ok());
        assert!(ok("http://127.0.0.1:1/token", true).is_err());
    }

    /// 요청마다 `respond(경로)`를 돌려주는 HTTP 서버. 받은 요청 경로를 모은다.
    fn serve(respond: impl Fn(&str) -> String + Send + 'static) -> (u16, Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { return };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    continue;
                }
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                }
                let path = first.split(' ').nth(1).unwrap_or("").to_owned();
                log.lock().unwrap().push(path.clone());
                let _ = c.write_all(respond(&path).as_bytes());
            }
        });
        (port, seen)
    }

    fn reply(status: &str, headers: &str) -> String {
        format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n")
    }

    #[tokio::test]
    async fn insecure_hosts_fall_back_to_http() {
        // `[::1]`은 localhost가 아니라 기본은 https다. 받는 쪽은 평문 http.
        let Ok(l) = std::net::TcpListener::bind("[::1]:0") else {
            return; // IPv6 루프백이 없는 환경.
        };
        let port = l.local_addr().unwrap().port();
        drop(l);
        let (p, seen) = {
            use std::io::{BufRead, BufReader, Write};
            let l = std::net::TcpListener::bind(format!("[::1]:{port}")).unwrap();
            let seen: Arc<Mutex<Vec<String>>> = Arc::default();
            let log = seen.clone();
            std::thread::spawn(move || {
                for c in l.incoming() {
                    let Ok(mut c) = c else { return };
                    let mut r = BufReader::new(c.try_clone().unwrap());
                    let mut first = String::new();
                    if r.read_line(&mut first).is_err() || !first.starts_with("GET ") {
                        continue; // TLS ClientHello
                    }
                    loop {
                        let mut h = String::new();
                        if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                            break;
                        }
                    }
                    log.lock().unwrap().push(first.clone());
                    let _ = c.write_all(reply("200 OK", "").as_bytes());
                }
            });
            (port, seen)
        };
        let host = format!("[::1]:{p}");
        let plain = Registry::new().unwrap();
        assert!(plain.get(&host, "a/b", "manifests/1", &[]).await.is_err());
        assert!(seen.lock().unwrap().is_empty());
        let r = Registry::new()
            .unwrap()
            .with_insecure([host.clone()].into())
            .unwrap();
        r.get(&host, "a/b", "manifests/1", &[]).await.unwrap();
        r.get(&host, "a/b", "manifests/2", &[]).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);
        assert!(r.require_https(&format!("http://{host}/v2/")).is_ok());
        assert!(plain.require_https(&format!("http://{host}/v2/")).is_err());
    }

    #[tokio::test]
    async fn hostile_registry_cannot_point_sbx_at_internal_addresses() {
        // 다른 내부 서비스(메타데이터 서버 흉내). 레지스트리가 이리로 보내도 요청이 오면 안 된다.
        let (inner, inner_seen) = serve(|_| reply("200 OK", ""));
        let r = Registry::new().unwrap();

        // 1. 토큰 주소가 사설 IP.
        let (p, _) = serve(|_| {
            reply(
                "401 Unauthorized",
                "WWW-Authenticate: Bearer realm=\"http://10.255.255.1/token\"\r\n",
            )
        });
        let e = r
            .get(&format!("127.0.0.1:{p}"), "x/y", "manifests/latest", &[])
            .await;
        let e = e.unwrap_err().to_string();
        assert!(e.contains("private or local"), "{e}");

        // 2. 토큰 주소가 이름인데 루프백으로 풀린다(DNS로 내부 주소를 돌려주는 경우와 같다).
        let (p, _) = serve(move |_| {
            reply(
                "401 Unauthorized",
                &format!("WWW-Authenticate: Bearer realm=\"http://localhost:{inner}/token\"\r\n"),
            )
        });
        let e = r
            .get(&format!("127.0.0.1:{p}"), "x/y", "manifests/latest", &[])
            .await;
        let e = e.unwrap_err().to_string();
        assert!(e.contains("no public address"), "{e}");

        // 3. redirect가 내부 주소로.
        let (p, _) = serve(move |_| {
            reply(
                "302 Found",
                &format!("Location: http://localhost:{inner}/x\r\n"),
            )
        });
        let e = r
            .get(&format!("127.0.0.1:{p}"), "x/y", "blobs/sha256:00", &[])
            .await;
        assert!(e.is_err());
        let (p, _) = serve(|_| reply("302 Found", "Location: http://169.254.169.254/latest\r\n"));
        let e = r
            .get(&format!("127.0.0.1:{p}"), "x/y", "blobs/sha256:00", &[])
            .await;
        let e = e.unwrap_err().to_string();
        assert!(e.contains("private or local"), "{e}");

        assert!(
            inner_seen.lock().unwrap().is_empty(),
            "{:?}",
            inner_seen.lock().unwrap()
        );

        // 사용자가 고른 레지스트리(루프백) 안의 redirect는 된다.
        let (p, seen) = serve(|path| {
            if path.ends_with("/a") {
                reply("302 Found", "Location: /v2/x/y/b\r\n")
            } else {
                reply("200 OK", "")
            }
        });
        r.get(&format!("127.0.0.1:{p}"), "x/y", "a", &[])
            .await
            .unwrap();
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    /// 받은 요청(경로와 본문, `Authorization`).
    type Seen = Arc<Mutex<Vec<(String, Option<String>)>>>;

    /// `serve`와 같고, `Authorization` 헤더도 넘긴다. 본문을 줄 수 있다.
    fn serve_auth(respond: impl Fn(&str, Option<&str>) -> String + Send + 'static) -> (u16, Seen) {
        use std::io::{BufRead, BufReader, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen: Seen = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { return };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    continue;
                }
                let mut auth = None;
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        if k.eq_ignore_ascii_case("authorization") {
                            auth = Some(v.trim().to_owned());
                        } else if k.eq_ignore_ascii_case("content-length") {
                            len = v.trim().parse().unwrap_or(0);
                        }
                    }
                }
                let mut body = vec![0; len];
                let _ = std::io::Read::read_exact(&mut r, &mut body);
                let path = first.split(' ').nth(1).unwrap_or("").to_owned();
                let shown = if body.is_empty() {
                    path.clone()
                } else {
                    format!("{path} {}", String::from_utf8_lossy(&body))
                };
                log.lock().unwrap().push((shown, auth.clone()));
                let _ = c.write_all(respond(&path, auth.as_deref()).as_bytes());
            }
        });
        (port, seen)
    }

    fn json_reply(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn files_with(host: &str, user: &str, pass: &str) -> AuthFiles {
        let d = std::env::temp_dir().join(format!(
            "sbx-registry-auth-{}-{}",
            std::process::id(),
            host.replace(':', "_")
        ));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.json");
        let auth = crate::auth::base64_encode(format!("{user}:{pass}").as_bytes());
        std::fs::write(
            &p,
            format!(r#"{{"auths":{{"{host}":{{"auth":"{auth}"}}}}}}"#),
        )
        .unwrap();
        AuthFiles {
            read: vec![p],
            write: None,
        }
    }

    #[tokio::test]
    async fn basic_registries_get_the_stored_credentials() {
        let good = format!("Basic {}", crate::auth::base64_encode(b"me:pw"));
        let (p, seen) = serve_auth(move |_, auth| {
            if auth == Some(good.as_str()) {
                reply("200 OK", "")
            } else {
                reply(
                    "401 Unauthorized",
                    "WWW-Authenticate: Basic realm=\"r\"\r\n",
                )
            }
        });
        let host = format!("127.0.0.1:{p}");
        let e = Registry::new()
            .unwrap()
            .get(&host, "x/y", "manifests/a", &[])
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains(&format!("sbx login {host}")), "{e}");

        let r = Registry::new()
            .unwrap()
            .with_auth(files_with(&host, "me", "pw"));
        r.get(&host, "x/y", "manifests/a", &[]).await.unwrap();
        // 다음 요청은 처음부터 Basic으로 간다.
        let before = seen.lock().unwrap().len();
        r.get(&host, "x/y", "manifests/b", &[]).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), before + 1);

        assert_eq!(
            r.check_login(
                &host,
                &Credential {
                    username: "me".into(),
                    secret: "pw".into()
                }
            )
            .await
            .unwrap(),
            LoginCheck::Accepted
        );
        let e = r
            .check_login(
                &host,
                &Credential {
                    username: "me".into(),
                    secret: "no".into(),
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("refused"), "{e}");
    }

    #[tokio::test]
    async fn token_requests_carry_the_credentials_or_the_identity_token() {
        let port = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let me = port.clone();
        let basic = format!("Basic {}", crate::auth::base64_encode(b"me:pw"));
        let (p, seen) = serve_auth(move |path, auth| {
            let port = me.load(std::sync::atomic::Ordering::SeqCst);
            if path.starts_with("/token") {
                // GET은 Basic, POST(identity token)는 본문으로 온다.
                if auth == Some(basic.as_str()) || auth.is_none() && path == "/token" {
                    json_reply(r#"{"token":"t1"}"#)
                } else {
                    reply("401 Unauthorized", "")
                }
            } else if auth == Some("Bearer t1") {
                reply("200 OK", "")
            } else {
                reply(
                    "401 Unauthorized",
                    &format!(
                        "WWW-Authenticate: Bearer realm=\"http://127.0.0.1:{port}/token\",service=\"reg\"\r\n"
                    ),
                )
            }
        });
        port.store(p, std::sync::atomic::Ordering::SeqCst);
        let host = format!("127.0.0.1:{p}");
        let r = Registry::new()
            .unwrap()
            .with_auth(files_with(&host, "me", "pw"));
        r.get(&host, "x/y", "manifests/a", &[]).await.unwrap();
        let log = seen.lock().unwrap().clone();
        let token = log.iter().find(|(p, _)| p.starts_with("/token")).unwrap();
        assert!(
            token.0.contains("scope=repository%3Ax%2Fy%3Apull"),
            "{token:?}"
        );
        assert_eq!(
            token.1.as_deref(),
            Some(format!("Basic {}", crate::auth::base64_encode(b"me:pw")).as_str())
        );

        // identity token: POST로 refresh_token을 보낸다.
        seen.lock().unwrap().clear();
        let r = Registry::new().unwrap();
        let rt = Credential {
            username: "<token>".into(),
            secret: "refresh-1".into(),
        };
        assert_eq!(
            r.check_login(&host, &rt).await.unwrap(),
            LoginCheck::Accepted
        );
        let log = seen.lock().unwrap().clone();
        let post = log.iter().find(|(p, _)| p.starts_with("/token")).unwrap();
        assert!(post.0.contains("grant_type=refresh_token"), "{post:?}");
        assert!(post.0.contains("refresh_token=refresh-1"), "{post:?}");
        assert!(post.0.contains("service=reg"), "{post:?}");

        // 틀린 비밀번호는 토큰 단계에서 거부된다.
        let e = r
            .check_login(
                &host,
                &Credential {
                    username: "me".into(),
                    secret: "no".into(),
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(e.contains("refused the credentials"), "{e}");
    }

    #[test]
    fn credentials_never_go_over_plain_http_to_other_hosts() {
        assert!(require_https("https://ghcr.io/v2/").is_ok());
        assert!(require_https("http://localhost:5000/v2/").is_ok());
        assert!(require_https("http://127.0.0.1:5000/token").is_ok());
        assert!(require_https("http://registry.lan/v2/").is_err());
        assert!(is_basic("Basic realm=\"x\""));
        assert!(is_basic("basic"));
        assert!(!is_basic("Bearer realm=\"x\""));
    }
}
