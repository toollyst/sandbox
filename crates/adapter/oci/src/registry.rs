//! Distribution Spec 클라이언트. pull에 필요한 GET만 한다.
//!
//! 인증은 익명과 Bearer 토큰 흐름만 지원한다. 401 응답의 `WWW-Authenticate`에 적힌 곳에서
//! 토큰을 받아 다시 요청한다. 토큰은 저장소마다 메모리에만 둔다.
//!
//! `localhost`와 `127.0.0.1` 레지스트리는 http로, 나머지는 https로 접속한다. Docker와 같은 기본값이다.
//! blob 요청이 다른 호스트(CDN)로 redirect되면 `Authorization` 헤더는 따라가지 않는다(reqwest 기본 동작).
//!
//! **레지스트리가 고른 주소는 믿지 않는다.** 사용자가 이미지 이름으로 고른 호스트만 사설·로컬 주소여도 된다
//! (사내 레지스트리). 토큰 주소(`realm`)와 redirect는 레지스트리가 정하므로, 그 호스트는 인터넷 쪽 주소로만
//! 접속한다([`sbx_model::is_public_ip`]). 이름 해석 단계에서 걸러서 DNS로 사설 주소를 돌려줘도 막힌다.
//! 토큰 주소와 redirect는 https만 따른다(레지스트리 자체가 http인 `localhost`일 때만 http도 된다).
//! 응답이 멈추면 [`READ_TIMEOUT`] 뒤 끊는다.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Response, StatusCode, Url};
use serde::Deserialize;

use sbx_image::ImageError;

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

/// 레지스트리 클라이언트.
#[derive(Debug)]
pub struct Registry {
    http: reqwest::Client,
    tokens: Mutex<HashMap<String, String>>,
    named: Named,
}

impl Registry {
    /// 만든다.
    pub fn new() -> Result<Self, ImageError> {
        let named: Named = Arc::default();
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
        let http = reqwest::Client::builder()
            .user_agent(concat!("sbx/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(READ_TIMEOUT)
            .redirect(policy)
            .dns_resolver(Arc::new(Guard {
                named: named.clone(),
            }))
            .build()
            .map_err(transport)?;
        Ok(Self {
            http,
            tokens: Mutex::new(HashMap::new()),
            named,
        })
    }

    fn base(host: &str) -> String {
        let insecure = host == "localhost"
            || host.starts_with("localhost:")
            || host == "127.0.0.1"
            || host.starts_with("127.0.0.1:");
        format!("{}://{host}", if insecure { "http" } else { "https" })
    }

    /// `GET /v2/<repository>/<path>`. 401이면 토큰을 받아 한 번 더 시도한다.
    pub async fn get(
        &self,
        host: &str,
        repository: &str,
        path: &str,
        accept: &[&str],
    ) -> Result<Response, ImageError> {
        let url = format!("{}/v2/{repository}/{path}", Self::base(host));
        if let Ok(mut n) = self.named.lock() {
            n.insert(named_key(host));
        }
        let key = format!("{host}/{repository}");
        let cached = self.tokens.lock().ok().and_then(|t| t.get(&key).cloned());
        let mut resp = self.send(&url, accept, cached.as_deref()).await?;
        if resp.status() == StatusCode::UNAUTHORIZED {
            let challenge = resp
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .and_then(parse_challenge)
                .ok_or_else(|| {
                    ImageError::Transport(format!("{url}: 401 without a Bearer challenge"))
                })?;
            let token = self
                .token(&challenge, repository, url.starts_with("http:"))
                .await?;
            if let Ok(mut t) = self.tokens.lock() {
                t.insert(key, token.clone());
            }
            resp = self.send(&url, accept, Some(&token)).await?;
        }
        match resp.status() {
            s if s.is_success() => Ok(resp),
            StatusCode::NOT_FOUND => Err(ImageError::NotFound(url)),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ImageError::Transport(
                format!("{url}: access denied (private image or wrong name)"),
            )),
            s => Err(ImageError::Transport(format!("{url}: HTTP {s}"))),
        }
    }

    async fn send(
        &self,
        url: &str,
        accept: &[&str],
        token: Option<&str>,
    ) -> Result<Response, ImageError> {
        let mut req = self.http.get(url);
        if !accept.is_empty() {
            req = req.header(ACCEPT, accept.join(", "));
        }
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send().await.map_err(transport)
    }

    async fn token(
        &self,
        ch: &Challenge,
        repository: &str,
        from_http: bool,
    ) -> Result<String, ImageError> {
        let mut url = Url::parse(&ch.realm).map_err(transport)?;
        check_target(&url, from_http, &self.named).map_err(ImageError::Transport)?;
        {
            let mut q = url.query_pairs_mut();
            if let Some(s) = &ch.service {
                q.append_pair("service", s);
            }
            let scope = ch
                .scope
                .clone()
                .unwrap_or_else(|| format!("repository:{repository}:pull"));
            q.append_pair("scope", &scope);
        }
        let resp = self.http.get(url).send().await.map_err(transport)?;
        if !resp.status().is_success() {
            return Err(ImageError::Transport(format!(
                "token endpoint {}: HTTP {}",
                ch.realm,
                resp.status()
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
}
