//! 실행기 저장소: 실행기 묶음을 내려받아 설치하게 하는 공개 규약과 그 클라이언트. `sbx executor install`이 쓴다.
//!
//! apt 저장소처럼 누구나 자기 서버에 둘 수 있다. 서버 프로그램은 필요 없다. 정적 파일(색인 하나와 묶음 압축
//! 파일들)만 있으면 되므로 GitHub Pages, S3 같은 정적 호스팅이나 로컬 디렉터리(`file://`)로도 된다.
//! 실행기 묶음의 내용(실행기 규약을 말하는 `sbx-executor`와 그것이 쓰는 파일)은 `sbx-executor-install`과
//! `sbx-executor-proto`의 규칙을 따른다. 이 규약은 그것을 어디서 어떻게 받는지만 정한다.
//!
//! # 저장소 (schema 1)
//!
//! 저장소는 URL 하나(디렉터리)다. 색인은 `<저장소 URL>/index.json`이다(URL이 `/`로 끝나지 않으면 붙여서 본다).
//!
//! ```json
//! {
//!   "schema": 1,
//!   "executors": {
//!     "firecracker": {
//!       "description": "Firecracker microVMs",
//!       "versions": {
//!         "0.1.0": {
//!           "protocol": 1,
//!           "artifacts": [
//!             {
//!               "os": "darwin",
//!               "architecture": "arm64",
//!               "url": "firecracker/0.1.0/firecracker-0.1.0-darwin-arm64.tar.gz",
//!               "sha256": "<압축 파일의 SHA-256, 소문자 16진수 64자>",
//!               "size": 31457280
//!             }
//!           ]
//!         }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! | 필드 | 뜻 |
//! |---|---|
//! | `schema` | 형식 버전. 이 클라이언트는 [`index::SCHEMA`]만 읽는다. 다른 값이면 색인 전체를 거부한다. |
//! | `executors` | 이름 → 실행기. 이름과 버전은 `sbx-executor-install` 규칙(`[A-Za-z0-9._+-]`, `.`으로 시작하지 않는다). |
//! | `description` | 한 줄 설명 (선택). |
//! | `protocol` | 그 버전이 말하는 실행기 규약(`sbx-executor-proto`의 `VERSION`). 클라이언트는 자기가 말하는 규약의 버전만 고른다. |
//! | `os`, `architecture` | 묶음이 도는 호스트 플랫폼. OCI image-spec의 값: `linux`/`darwin`/`windows`, `amd64`/`arm64`. 버전 하나에 플랫폼마다 하나. |
//! | `url` | 압축 파일. 절대 URL이거나 색인 URL 기준 상대 참조(RFC 3986). |
//! | `sha256`, `size` | 압축 파일의 SHA-256과 바이트 수. 받은 뒤 둘 다 맞아야 푼다. |
//!
//! 모르는 필드는 무시한다(같은 schema 안에서 필드를 더할 수 있게). 규약에 맞지 않는 곳이 하나라도 있으면
//! 색인 전체를 거부한다. 색인은 [`index::MAX_INDEX_BYTES`] 이하다.
//!
//! **URL**: `https`, `file`, 그리고 loopback 호스트의 `http`만 쓴다([`location::check`]). redirect된 곳도 같다.
//! 색인의 무결성은 TLS에 기대고, 묶음의 무결성은 색인의 `sha256`이 지킨다. 서명은 아직 없다(다음 schema).
//!
//! # 묶음 압축 파일
//!
//! gzip으로 압축한 tar(`.tar.gz`). 맨 위에 실행 파일 `sbx-executor`(Windows는 `sbx-executor.exe`)가 있거나,
//! 맨 위가 디렉터리 하나뿐이고 그 안에 있다(그 디렉터리가 묶음이다). 일반 파일과 디렉터리만 담는다.
//! 심볼릭 링크, 하드 링크, 장치 파일, 절대 경로, `..`가 있으면 거부한다. 권한 비트(실행 가능)는 지킨다.
//! sparse 파일은 GNU 형식(`S`)과 pax 형식 1.0(macOS `tar`가 쓴다), 0.1을 읽는다. 다른 sparse 형식은 거부한다.
//! [`unpack::unpack`]이 이 규칙대로 푼다.
//!
//! # 고르기 ([`index::pick`])
//!
//! 저장소 목록은 우선순위 순서다. 이 호스트 플랫폼([`Platform::host`])의 묶음이 있고 규약 버전이 맞는 것 중에서:
//!
//! - `<이름>@<버전>`: 그 버전. 여러 저장소에 있으면 앞 저장소.
//! - `<이름>`: 가장 높은 버전(`sbx_executor_install::version_cmp`). 같은 버전이면 앞 저장소.
//! - 없으면 이유를 알린다: 이름이 없다, 버전이 없다, 이 플랫폼 빌드가 없다(있는 플랫폼 목록), 규약 버전이 다르다.
//!
//! 받지 못한 저장소는 건너뛰고 경고만 한다([`indexes`]). 남은 저장소로 고른다.
//!
//! # 저장소 목록 파일 ([`sources`])
//!
//! 한 줄에 `<이름> <URL>`. `#`으로 시작하는 줄과 빈 줄은 무시한다. 줄 순서가 우선순위다. 손으로 고쳐도 된다.
//!
//! # 모듈
//!
//! [`error`], [`location`], [`platform`]이 가장 아래, 그 위에 [`index`], [`sources`], [`unpack`], 그 위에 [`fetch`].
//!
//! - 의존: `sbx-executor-install`(이름 규칙, 버전 비교, 묶음 실행 파일 이름)

pub mod error;
pub mod fetch;
pub mod index;
pub mod location;
pub mod platform;
pub mod sources;
pub mod unpack;

use std::path::{Path, PathBuf};

pub use error::RepoError;
pub use fetch::Fetcher;
pub use index::{Artifact, Candidate, Index};
pub use platform::Platform;
pub use sources::{Repository, Sources};

/// 저장소들의 색인을 받는다. 받은 것(저장소 순서)과, 받지 못한 저장소의 오류를 돌려준다.
pub async fn indexes(
    fetcher: &Fetcher,
    repos: &[Repository],
) -> (Vec<(String, Index)>, Vec<RepoError>) {
    let mut ok = Vec::new();
    let mut failed = Vec::new();
    for r in repos {
        match fetcher.index(r).await {
            Ok(i) => ok.push((r.name.clone(), i)),
            Err(e) => failed.push(e),
        }
    }
    (ok, failed)
}

/// 고른 것을 `work`(빈 디렉터리이거나 없는 곳)에 받아 풀고, 묶음 디렉터리를 돌려준다.
/// 설치(`sbx_executor_install::Executors::install`)는 호출자가 한다. `work`를 지우는 것도 호출자다.
pub async fn fetch_bundle(
    fetcher: &Fetcher,
    c: &Candidate,
    work: &Path,
) -> Result<PathBuf, RepoError> {
    std::fs::create_dir_all(work)?;
    let archive = work.join("bundle.tar.gz");
    fetcher.download(&c.artifact, &archive).await?;
    let dir = unpack::unpack(&archive, &work.join("bundle"))?;
    std::fs::remove_file(&archive)?;
    Ok(dir)
}

#[cfg(test)]
mod tests;
