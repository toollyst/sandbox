//! 색인과 묶음 받기. 둘 다 서명을 확인한 뒤에만 넘긴다.

use std::fmt;
use std::fs;
use std::path::Path;

use sbx_executor_repo::index::Artifact;
use sbx_executor_repo::trust::{self, MAX_SIGNATURE_BYTES};
use sbx_executor_repo::{Fetcher, Platform, RepoError, Seen, location};
use url::Url;

use crate::index::{Asset, Index, MAX_INDEX_BYTES};

/// 본 `serial`을 적을 때 쓰는 이름. 실행기 저장소 이름과 섞이지 않게 저장소 이름으로 쓸 수 없는 `@`를 넣는다.
pub const SEEN_NAME: &str = "@sbx-releases";

/// 색인이 있는 곳과 믿는 키.
#[derive(Debug, Clone)]
pub struct Source {
    /// `releases.json`의 URL.
    pub url: Url,
    /// minisign 공개키(한 줄 base64). 하나라도 맞으면 된다. 비어 있으면 아무것도 믿지 않는다.
    pub keys: Vec<String>,
}

/// 받기 오류.
#[derive(Debug)]
pub enum Error {
    /// 받지 못했다(연결, 없음, 크기, 해시).
    Fetch(RepoError),
    /// 색인이 형식에 맞지 않는다.
    Index {
        /// 색인 URL.
        url: String,
        /// 이유.
        reason: String,
    },
    /// 서명, 기한, 일련번호가 맞지 않는다.
    Untrusted {
        /// 무엇의 URL.
        url: String,
        /// 이유.
        reason: String,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fetch(e) => write!(f, "{e}"),
            Self::Index { url, reason } => write!(f, "invalid sbx release index {url}: {reason}"),
            Self::Untrusted { url, reason } => write!(f, "untrusted {url}: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<RepoError> for Error {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::Untrusted { url, reason } => Self::Untrusted { url, reason },
            e => Self::Fetch(e),
        }
    }
}

fn untrusted(url: &Url, reason: impl Into<String>) -> Error {
    Error::Untrusted {
        url: url.to_string(),
        reason: reason.into(),
    }
}

async fn signature(fetcher: &Fetcher, of: &Url) -> Result<String, Error> {
    let sig_url = location::signature(of);
    let sig = match fetcher.bytes(&sig_url, MAX_SIGNATURE_BYTES).await {
        Err(RepoError::Missing { .. }) => {
            return Err(untrusted(of, format!("no signature at {sig_url}")));
        }
        other => other?,
    };
    String::from_utf8(sig).map_err(|_| untrusted(of, "the signature is not text"))
}

/// 색인을 받아 서명, 기한, 일련번호를 확인하고 읽는다. `seen`이 있으면 본 것보다 작은 `serial`을 거부하고 큰 것을 적는다.
pub async fn fetch(fetcher: &Fetcher, src: &Source, seen: Option<&Seen>) -> Result<Index, Error> {
    if src.keys.is_empty() {
        return Err(untrusted(
            &src.url,
            "this sbx has no release signing keys, so it trusts no release index",
        ));
    }
    let body = fetcher.bytes(&src.url, MAX_INDEX_BYTES).await?;
    let sig = signature(fetcher, &src.url).await?;
    trust::verify(&body, &sig, &src.keys).map_err(|e| untrusted(&src.url, e))?;
    let index = Index::parse(&body, &src.url).map_err(|reason| Error::Index {
        url: src.url.to_string(),
        reason,
    })?;
    if index.expires <= trust::now() {
        return Err(untrusted(
            &src.url,
            format!(
                "it expired at {} (Unix time); it has not been re-signed (a frozen mirror?)",
                index.expires
            ),
        ));
    }
    if let Some(seen) = seen {
        match seen.check_and_record_named(
            SEEN_NAME,
            src.url.as_str(),
            index.serial,
            src.url.as_str(),
        ) {
            Ok(()) => {}
            // 적지 못했다(읽기 전용 데이터 디렉터리): 이미 적힌 serial과는 견줬으니(적을 때만 쓴다) 그대로 쓴다.
            Err(RepoError::Io(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(index)
}

/// 묶음을 `dest`에 받는다. 크기와 SHA-256이 색인과 같고, 옆의 `.minisig`가 `keys` 중 하나와 맞아야 한다.
/// 하나라도 틀리면 `dest`를 지운다.
pub async fn download(
    fetcher: &Fetcher,
    asset: &Asset,
    keys: &[String],
    dest: &Path,
) -> Result<(), Error> {
    if keys.is_empty() {
        return Err(untrusted(
            &asset.url,
            "this sbx has no release signing keys",
        ));
    }
    let artifact = Artifact {
        platform: Platform::host(),
        url: asset.url.clone(),
        sha256: asset.sha256.clone(),
        size: asset.size,
    };
    fetcher.download(&artifact, dest).await?;
    let checked = async {
        let sig = signature(fetcher, &asset.url).await?;
        let body = fs::read(dest).map_err(|e| Error::Fetch(RepoError::Io(e)))?;
        trust::verify(&body, &sig, keys).map_err(|e| untrusted(&asset.url, e))
    }
    .await;
    if checked.is_err() {
        let _ = fs::remove_file(dest);
    }
    checked
}
