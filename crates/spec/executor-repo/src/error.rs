//! 오류. 가장 아래 모듈이다.

use std::fmt;
use std::io;

/// 저장소 작업의 오류.
#[derive(Debug)]
pub enum RepoError {
    /// 저장소 목록 파일이나 이름, URL이 잘못됐다.
    Config(String),
    /// 내려받지 못했다.
    Fetch {
        /// URL.
        url: String,
        /// 이유.
        reason: String,
    },
    /// 그 URL에 아무것도 없다 (HTTP 404, 없는 파일).
    Missing {
        /// URL.
        url: String,
    },
    /// 색인이 규약에 맞지 않는다.
    Index {
        /// URL.
        url: String,
        /// 이유.
        reason: String,
    },
    /// 찾는 실행기, 버전, 플랫폼 빌드가 없다.
    NotFound(String),
    /// 내려받은 파일의 크기나 SHA-256이 색인과 다르다.
    Integrity {
        /// URL.
        url: String,
        /// 이유.
        reason: String,
    },
    /// 묶음 압축 파일이 규약에 맞지 않는다.
    Archive(String),
    /// 파일 시스템 오류.
    Io(io::Error),
}

impl fmt::Display for RepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config(m) | Self::NotFound(m) => f.write_str(m),
            Self::Fetch { url, reason } => write!(f, "cannot fetch {url}: {reason}"),
            Self::Missing { url } => write!(f, "cannot fetch {url}: not found"),
            Self::Index { url, reason } => write!(f, "invalid repository index {url}: {reason}"),
            Self::Integrity { url, reason } => {
                write!(f, "download {url} failed verification: {reason}")
            }
            Self::Archive(m) => write!(f, "invalid executor archive: {m}"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RepoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for RepoError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
