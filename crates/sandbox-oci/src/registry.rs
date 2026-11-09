//! Distribution Spec 클라이언트. pull에 필요한 GET만 한다.
//!
//! 인증은 익명과 Bearer 토큰 흐름만 지원한다. 401 응답의 `WWW-Authenticate`에 적힌 곳에서
//! 토큰을 받아 다시 요청한다. 토큰은 저장소마다 메모리에만 둔다.
//!
//! `localhost`와 `127.0.0.1` 레지스트리는 http로, 나머지는 https로 접속한다. Docker와 같은 기본값이다.
//! blob 요청이 다른 호스트(CDN)로 redirect되면 `Authorization` 헤더는 따라가지 않는다(reqwest 기본 동작).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Response, StatusCode, Url};
use serde::Deserialize;

use sandbox_image::ImageError;

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

fn transport(e: impl std::fmt::Display) -> ImageError {
    ImageError::Transport(e.to_string())
}

/// 레지스트리 클라이언트.
#[derive(Debug)]
pub struct Registry {
    http: reqwest::Client,
    tokens: Mutex<HashMap<String, String>>,
}

impl Registry {
    /// 만든다.
    pub fn new() -> Result<Self, ImageError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("sbx/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(transport)?;
        Ok(Self {
            http,
            tokens: Mutex::new(HashMap::new()),
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
            let token = self.token(&challenge, repository).await?;
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

    async fn token(&self, ch: &Challenge, repository: &str) -> Result<String, ImageError> {
        let mut url = Url::parse(&ch.realm).map_err(transport)?;
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
}
