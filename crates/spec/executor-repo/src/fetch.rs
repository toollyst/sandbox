//! 내려받기: 색인과 묶음 압축 파일. `https`, `file`, loopback `http`([`crate::location::check`]).

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use url::Url;

use crate::error::RepoError;
use crate::index::{Artifact, Index, MAX_INDEX_BYTES};
use crate::location;
use crate::sources::Repository;
use crate::trust::{self, MAX_SIGNATURE_BYTES, Seen};

/// HTTP 클라이언트를 들고 있는 내려받는 쪽.
#[derive(Debug, Clone)]
pub struct Fetcher {
    http: reqwest::Client,
    seen: Option<Seen>,
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

fn fetch_err(url: &Url, reason: impl ToString) -> RepoError {
    RepoError::Fetch {
        url: url.to_string(),
        reason: reason.to_string(),
    }
}

/// 받은 내용을 차례로 넘기는 곳. 넘친 바이트 수를 넘기면 그만둔다.
struct Sink<'a> {
    url: &'a Url,
    limit: u64,
    got: u64,
    write: &'a mut (dyn FnMut(&[u8]) -> io::Result<()> + Send),
}

impl Sink<'_> {
    fn push(&mut self, chunk: &[u8]) -> Result<(), RepoError> {
        self.got += chunk.len() as u64;
        if self.got > self.limit {
            return Err(fetch_err(
                self.url,
                format!("larger than the {} bytes expected", self.limit),
            ));
        }
        (self.write)(chunk).map_err(RepoError::Io)
    }
}

impl Fetcher {
    /// 시스템 설정(프록시 환경 변수 포함)을 쓰는 클라이언트를 만든다.
    pub fn new() -> Result<Self, RepoError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("sbx-executor-repo/", env!("CARGO_PKG_VERSION")))
            .redirect(reqwest::redirect::Policy::custom(
                |attempt| match location::check_redirect(attempt.previous(), attempt.url()) {
                    Ok(()) => attempt.follow(),
                    Err(e) => attempt.error(e),
                },
            ))
            .build()
            .map_err(|e| {
                RepoError::Config(format!("cannot create an HTTP client: {}", chain(&e)))
            })?;
        Ok(Self { http, seen: None })
    }

    /// `url`의 내용을 `limit` 바이트까지 `write`에 넘긴다. 받은 바이트 수를 돌려준다.
    async fn stream(
        &self,
        url: &Url,
        limit: u64,
        write: &mut (dyn FnMut(&[u8]) -> io::Result<()> + Send),
    ) -> Result<u64, RepoError> {
        location::check(url).map_err(|e| fetch_err(url, e))?;
        let mut sink = Sink {
            url,
            limit,
            got: 0,
            write,
        };
        if url.scheme() == "file" {
            let path = url
                .to_file_path()
                .map_err(|()| fetch_err(url, "not a local file path"))?;
            let mut f = fs::File::open(&path).map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    RepoError::Missing {
                        url: url.to_string(),
                    }
                } else {
                    fetch_err(url, e)
                }
            })?;
            let mut buf = vec![0; 64 << 10];
            loop {
                let n = f.read(&mut buf).map_err(|e| fetch_err(url, e))?;
                if n == 0 {
                    break;
                }
                sink.push(&buf[..n])?;
            }
            return Ok(sink.got);
        }
        let mut resp = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|e| fetch_err(url, chain(&e)))?;
        // redirect 뒤의 URL도 같은 규칙을 따라야 한다 (https가 http로 내려가지 않게).
        location::check(resp.url()).map_err(|e| fetch_err(url, format!("redirected: {e}")))?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(RepoError::Missing {
                url: url.to_string(),
            });
        }
        if !resp.status().is_success() {
            return Err(fetch_err(url, resp.status()));
        }
        while let Some(chunk) = resp.chunk().await.map_err(|e| fetch_err(url, chain(&e)))? {
            sink.push(&chunk)?;
        }
        Ok(sink.got)
    }

    /// 본 일련번호를 여기에 적고, 그보다 오래된 서명된 색인을 거부한다([`Seen`]). 없으면 기억하지 않는다.
    pub fn with_seen(mut self, seen: Seen) -> Self {
        self.seen = Some(seen);
        self
    }

    /// 저장소의 색인을 받아 읽는다.
    ///
    /// - 키가 없는 저장소: `v2/index.json`을 먼저 찾고, 없으면 `index.json`을 읽는다.
    /// - 키가 있는 저장소([`Repository::signed`]): `v2/index.json`과 그 서명 `v2/index.json.minisig`가 있어야 하고,
    ///   서명이 키 하나와 맞아야 하며, 색인에 `serial`과 `expires`가 있어야 한다. `serial`이 본 것보다 작으면 거부한다.
    ///
    /// 어느 쪽이든 `expires`가 지났으면 거부한다.
    pub async fn index(&self, repo: &Repository) -> Result<Index, RepoError> {
        let v2 = repo.index_v2_url();
        let index = if repo.signed() {
            let untrusted = |reason: String| RepoError::Untrusted {
                url: v2.to_string(),
                reason,
            };
            let body = match self.bytes(&v2, MAX_INDEX_BYTES).await {
                Err(RepoError::Missing { .. }) => {
                    return Err(untrusted("a signed repository needs v2/index.json".into()));
                }
                other => other?,
            };
            let sig_url = location::signature(&v2);
            let sig = match self.bytes(&sig_url, MAX_SIGNATURE_BYTES).await {
                Err(RepoError::Missing { .. }) => {
                    return Err(untrusted(format!("no signature at {sig_url}")));
                }
                other => other?,
            };
            let sig = String::from_utf8(sig)
                .map_err(|_| untrusted("the signature is not text".into()))?;
            trust::verify(&body, &sig, &repo.keys).map_err(untrusted)?;
            let index = parse(&body, &v2)?;
            let (Some(serial), Some(_)) = (index.serial, index.expires) else {
                return Err(untrusted("a signed index needs serial and expires".into()));
            };
            check_expiry(&index, &v2)?;
            if let Some(seen) = &self.seen {
                seen.check_and_record(repo, serial, v2.as_str())?;
            }
            index
        } else {
            let (url, body) = match self.bytes(&v2, MAX_INDEX_BYTES).await {
                Err(RepoError::Missing { .. }) => {
                    let v1 = repo.index_url();
                    let b = self.bytes(&v1, MAX_INDEX_BYTES).await?;
                    (v1, b)
                }
                other => (v2, other?),
            };
            let index = parse(&body, &url)?;
            check_expiry(&index, &url)?;
            index
        };
        Ok(index)
    }

    async fn bytes(&self, url: &Url, limit: u64) -> Result<Vec<u8>, RepoError> {
        let mut body = Vec::new();
        self.stream(url, limit, &mut |c| {
            body.extend_from_slice(c);
            Ok(())
        })
        .await?;
        Ok(body)
    }

    /// 묶음 압축 파일을 `dest`에 받는다. 크기와 SHA-256이 색인과 같아야 한다. 다르면 `dest`를 지운다.
    pub async fn download(&self, a: &Artifact, dest: &Path) -> Result<(), RepoError> {
        let r = self.download_to(a, dest).await;
        if r.is_err() {
            let _ = fs::remove_file(dest);
        }
        r
    }

    async fn download_to(&self, a: &Artifact, dest: &Path) -> Result<(), RepoError> {
        let mut file = fs::File::create(dest)?;
        let mut hash = Sha256::new();
        let got = {
            let mut write = |c: &[u8]| {
                hash.update(c);
                file.write_all(c)
            };
            self.stream(&a.url, a.size, &mut write).await?
        };
        let integrity = |reason: String| RepoError::Integrity {
            url: a.url.to_string(),
            reason,
        };
        if got != a.size {
            return Err(integrity(format!("{got} bytes, index says {}", a.size)));
        }
        let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if digest != a.sha256 {
            return Err(integrity(format!(
                "sha256 {digest}, index says {}",
                a.sha256
            )));
        }
        file.sync_all()?;
        Ok(())
    }
}

fn parse(body: &[u8], url: &Url) -> Result<Index, RepoError> {
    Index::parse(body, url).map_err(|reason| RepoError::Index {
        url: url.to_string(),
        reason,
    })
}

fn check_expiry(index: &Index, url: &Url) -> Result<(), RepoError> {
    match index.expires {
        Some(e) if e <= trust::now() => Err(RepoError::Untrusted {
            url: url.to_string(),
            reason: format!(
                "it expired at {e} (Unix time); the repository has not been re-signed (a frozen mirror?)"
            ),
        }),
        _ => Ok(()),
    }
}
