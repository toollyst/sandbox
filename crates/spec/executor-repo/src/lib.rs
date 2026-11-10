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
//! | `schema` | 형식 버전. 이 클라이언트는 [`index::SCHEMAS`]를 읽는다. 다른 값이면 색인 전체를 거부한다. |
//! | `executors` | 이름 → 실행기. 이름과 버전은 `sbx-executor-install` 규칙(`[A-Za-z0-9._+-]`, `.`으로 시작하지 않는다). |
//! | `description` | 한 줄 설명 (선택). |
//! | `protocol` | 그 버전이 말하는 가장 높은 실행기 규약(`sbx-executor-proto`의 `VERSION`). |
//! | `min_protocol` | (schema 2, 선택) 그 버전이 말하는 가장 낮은 규약 버전. 없으면 `protocol` 하나만. 클라이언트는 자기 범위([`index::Protocols`])와 겹치는 것만 고른다. |
//! | `os`, `architecture` | 묶음이 도는 호스트 플랫폼. OCI image-spec의 값: `linux`/`darwin`/`windows`, `amd64`/`arm64`. 버전 하나에 플랫폼마다 하나. |
//! | `url` | 압축 파일. 절대 URL이거나 색인 URL 기준 상대 참조(RFC 3986). |
//! | `sha256`, `size` | 압축 파일의 SHA-256과 바이트 수. 받은 뒤 둘 다 맞아야 푼다. |
//!
//! 모르는 필드는 무시한다(같은 schema 안에서 필드를 더할 수 있게). 규약에 맞지 않는 곳이 하나라도 있으면
//! 색인 전체를 거부한다. 색인은 [`index::MAX_INDEX_BYTES`] 이하다.
//!
//! **URL**: `https`, `file`, 그리고 loopback 호스트의 `http`만 쓴다([`location::check`]). redirect된 곳도 같다.
//! 묶음의 무결성은 색인의 `sha256`이 지킨다. 색인은 서명된 저장소(아래 "서명")가 아니면 TLS에만 기댄다.
//!
//! # 저장소 (schema 2): 어댑터와 VMM 묶음
//!
//! 색인은 `<저장소 URL>/v2/index.json`이다([`location::INDEX_V2_FILE`]). 클라이언트는 이것을 먼저 찾고,
//! 없으면(HTTP 404, 없는 파일) `index.json`을 읽는다. 다른 오류(네트워크, 깨진 색인)는 되돌아가지 않고 알린다.
//! 그래서 저장소는 schema 1만 읽는 옛 sbx를 위한 `index.json`을 그대로 둔 채 v2를 더할 수 있다.
//!
//! schema 1과 같고, 둘이 더해진다.
//!
//! - `adapters`: 이름 → 어댑터. 모양은 `executors`와 같다. 어댑터는 실행기 규약을 말하는 자체 완결
//!   묶음(맨 위에 `sbx-executor`)이므로 버전마다 `protocol`이 있고 `adapter`는 없다.
//! - 실행기 버전의 `adapter`: 그 버전은 **VMM 묶음**이다. 실행기 규약을 말하지 않는 VMM(과 그것이 쓰는
//!   파일)만 담고, 이름의 어댑터가 그것을 돌린다(`sbx-executor-proto`의 "어댑터와 VMM 묶음"). 실행기 버전에는
//!   `protocol`(자체 완결)과 `adapter`(VMM 묶음) 중 **정확히 하나**가 있다.
//!
//! ```json
//! {
//!   "schema": 2,
//!   "executors": {
//!     "wick": {
//!       "description": "Firecracker builds",
//!       "versions": {
//!         "1.18.0": { "adapter": "firecracker", "artifacts": [ { "os": "darwin", "architecture": "arm64", "url": "...", "sha256": "...", "size": 1 } ] }
//!       }
//!     }
//!   },
//!   "adapters": {
//!     "firecracker": {
//!       "versions": {
//!         "0.2.0": { "protocol": 2, "artifacts": [ { "os": "darwin", "architecture": "arm64", "url": "...", "sha256": "...", "size": 1 } ] }
//!       }
//!     }
//!   }
//! }
//! ```
//!
//! 맨 위에 둘을 더 둘 수 있다(schema 2): `serial`(색인을 고칠 때마다 커지는 정수)과 `expires`(이 색인을 믿는
//! 기한, Unix 초). 서명된 색인에는 둘 다 있어야 한다. 클라이언트는 `expires`가 지난 색인을 서명과 상관없이 거부한다.
//!
//! VMM 묶음은 규약 버전으로 거르지 않는다. 맞는 것은 어댑터 쪽에서 고른다([`index::pick_adapter`]:
//! 이 플랫폼과 규약 버전의 가장 높은 버전). 상대 URL은 `v2/index.json` 기준이다.
//!
//! # 서명 ([`trust`])
//!
//! 저장소에 공개키를 두면(목록 파일의 `key=`) 그 저장소는 **서명된 저장소**다. 클라이언트는 그때
//!
//! 1. `v2/index.json`과 그 [minisign](https://jedisct1.github.io/minisign/) 서명 `v2/index.json.minisig`
//!    ([`location::SIGNATURE_SUFFIX`])를 받는다. 둘 중 하나라도 없으면 거부한다(`index.json`으로 되돌아가지 않는다).
//! 2. 서명이 키 중 하나와 맞아야 한다(미리 해시한 서명만. 키를 바꾸는 동안에는 옛 키와 새 키를 함께 둔다).
//! 3. 색인에 `serial`과 `expires`가 있어야 하고, `expires`가 지나지 않아야 한다(동결된 미러).
//! 4. `serial`이 이 기계가 그 저장소에서 본 가장 큰 값보다 작으면 거부한다(롤백). 본 값은 [`Seen`] 파일에 남는다.
//!
//! 서명은 색인 파일의 바이트 그대로에 대한 것이다. 묶음은 색인의 `sha256`으로 확인하므로 따로 서명하지 않는다.
//! 저장소를 만드는 쪽은 색인을 고칠 때마다 `serial`을 키우고 `expires`를 새로 정해 다시 서명한다
//! (`cargo xtask repo-sign`).
//!
//! # 묶음 압축 파일
//!
//! gzip으로 압축한 tar(`.tar.gz`). 맨 위에 실행 파일 `sbx-executor`(Windows는 `sbx-executor.exe`)가 있거나,
//! 맨 위가 디렉터리 하나뿐이고 그 안에 있다(그 디렉터리가 묶음이다). 일반 파일과 디렉터리만 담는다.
//! 심볼릭 링크, 하드 링크, 장치 파일, 절대 경로, `..`가 있으면 거부한다. 권한 비트(실행 가능)는 지킨다.
//! sparse 파일은 GNU 형식(`S`)과 pax 형식 1.0(macOS `tar`가 쓴다), 0.1을 읽는다. 다른 sparse 형식은 거부한다.
//! VMM 묶음에는 `sbx-executor`가 없어도 된다. 맨 위가 디렉터리 하나뿐이면 그 디렉터리가 묶음이고, 비어 있으면 안 된다.
//! [`unpack::unpack`]이 이 규칙대로 푼다.
//!
//! # 고르기 ([`index::pick`])
//!
//! 저장소 목록은 우선순위 순서다. 이 호스트 플랫폼([`Platform::host`])의 묶음이 있고 규약 버전이 맞는 것 중에서:
//!
//! - 이름은 그것을 내는 **첫 저장소**의 것이다. 뒤 저장소가 같은 이름에 더 높은 버전을 내도 그 저장소에서
//!   고르지 않는다(다른 저장소가 이름을 가로채지 못하게). 다른 저장소에서 받으려면 그 저장소만 넘긴다.
//! - `<이름>@<버전>`: 그 저장소의 그 버전(`1.18.0`이면 그 주 버전의 가장 새 패치).
//! - `<이름>`: 그 저장소의 가장 높은 버전(`sbx_executor_install::version_cmp`).
//! - 없으면 이유를 알린다: 이름이 없다, 버전이 없다, 이 플랫폼 빌드가 없다(있는 플랫폼 목록), 규약 버전이 다르다.
//!
//! 받지 못한 저장소는 건너뛰고 경고만 한다([`indexes`]). 남은 저장소로 고른다.
//!
//! # 저장소 목록 파일 ([`sources`])
//!
//! 한 줄에 `<이름> <URL> [key=<minisign 공개키>]...`. `#`으로 시작하는 줄과 빈 줄은 무시한다. 줄 순서가
//! 우선순위다. 손으로 고쳐도 된다.
//!
//! # 모듈
//!
//! [`error`], [`location`], [`platform`]이 가장 아래, 그 위에 [`index`], [`sources`], [`unpack`], 그 위에 [`trust`],
//! 그 위에 [`fetch`].
//!
//! - 의존: `minisign-verify`(서명 확인, 의존성 없음)
//!
//! - 의존: `sbx-executor-install`(이름 규칙, 버전 비교, 묶음 실행 파일 이름)

pub mod error;
pub mod fetch;
pub mod index;
pub mod location;
pub mod platform;
pub mod sources;
pub mod trust;
pub mod unpack;

use std::path::{Path, PathBuf};

pub use error::RepoError;
pub use fetch::Fetcher;
pub use index::{Artifact, Candidate, Index};
pub use platform::Platform;
pub use sources::{Repository, Sources};
pub use trust::Seen;

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
    let layout = if c.adapter.is_some() {
        unpack::Layout::Vmm
    } else {
        unpack::Layout::Executor
    };
    let dir = unpack::unpack(&archive, &work.join("bundle"), layout)?;
    std::fs::remove_file(&archive)?;
    Ok(dir)
}

#[cfg(test)]
mod tests;
