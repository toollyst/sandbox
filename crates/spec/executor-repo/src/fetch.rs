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

/// HTTP 클라이언트를 들고 있는 내려받는 쪽.
#[derive(Debug, Clone)]
pub struct Fetcher {
    http: reqwest::Client,
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
            .build()
            .map_err(|e| {
                RepoError::Config(format!("cannot create an HTTP client: {}", chain(&e)))
            })?;
        Ok(Self { http })
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

    /// 저장소의 색인을 받아 읽는다. `v2/index.json`을 먼저 찾고, 없으면 `index.json`을 읽는다.
    pub async fn index(&self, repo: &Repository) -> Result<Index, RepoError> {
        match self.index_at(&repo.index_v2_url()).await {
            Err(RepoError::Missing { .. }) => self.index_at(&repo.index_url()).await,
            other => other,
        }
    }

    async fn index_at(&self, url: &Url) -> Result<Index, RepoError> {
        let mut body = Vec::new();
        self.stream(url, MAX_INDEX_BYTES, &mut |c| {
            body.extend_from_slice(c);
            Ok(())
        })
        .await?;
        Index::parse(&body, url).map_err(|reason| RepoError::Index {
            url: url.to_string(),
            reason,
        })
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
