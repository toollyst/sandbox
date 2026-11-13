//! OCI Distribution push: [`sbx_image::ImageDestination`]의 레지스트리 쪽.
//!
//! **blob 하나** ([`OciImageSource`]의 `push_blob`):
//!
//! 1. `HEAD /v2/<저장소>/blobs/<digest>`가 2xx면 이미 있다. 아무것도 보내지 않는다.
//! 2. 같은 레지스트리의 다른 저장소에서 받은 이미지면([`PushTarget::mount_from`])
//!    `POST /v2/<저장소>/blobs/uploads/?mount=<digest>&from=<원래 저장소>`. 201이면 끌어왔다(내용을 보내지 않는다).
//!    202면 레지스트리가 끌어오지 않고 올리기 세션을 연 것이다: 규약대로 그 세션에 올린다. 그 밖의 응답이나 오류면
//!    mount 없이 3으로 간다. 거절(202도)한 뒤에는 그 대상의 남은 blob에 mount를 묻지 않는다.
//! 3. `POST /v2/<저장소>/blobs/uploads/`로 세션을 열고 한 번에 보낸다(`PUT <Location>&digest=<digest>`, 본문 전체,
//!    `Content-Length`). 레지스트리가 너무 크다고 하면(413) 그 세션을 지우고 새 세션에 나눠 보낸다([`Chunked`]:
//!    `PATCH <Location>`, `Content-Range`), 그리고 `PUT <Location>&digest=<digest>`(빈 본문)로 닫는다.
//! 4. 올리다 실패하면 연 세션을 지운다(`DELETE <Location>`, 해 보기만 한다).
//!
//! **manifest**: `PUT /v2/<저장소>/manifests/<태그>`, 본문은 받은 바이트 그대로, `Content-Type`은 그 미디어 형식이다.
//! 레지스트리가 알린 digest(`Docker-Content-Digest`)가 있으면 보낸 바이트의 sha256과 같아야 한다. blob도 같다.
//!
//! **규칙**
//!
//! - `Location`은 상대 주소든 절대 주소든 그 응답을 준 요청의 주소에 대어 푼다([`resolve_location`]). 레지스트리와
//!   다른 곳(스킴, 호스트, 포트)을 가리키면 [`crate::registry`]의 규칙(공개 주소, https)을 지켜야 하고, 인증은
//!   싣지 않는다. 쿼리(`_state` 등)는 그대로 두고 `digest`만 더한다([`with_digest`]).
//! - 나눠 보낼 때 다음 조각은 레지스트리가 알린 `Range`(`0-<끝>`, 끝 포함)의 다음 바이트부터다. 레지스트리가 덜
//!   받았으면 거기부터 다시 보낸다. 0에서 시작하지 않거나, 보낸 것보다 더 받았다거나, 전에 받았다던 것보다 줄면
//!   오류다. [`MAX_STALLS`]번 잇달아 하나도 더 받지 않아도 오류다. 416이면 세션 상태(`GET <Location>`)의 `Range`로
//!   다시 맞춘다. 조각 크기는 [`CHUNK_BYTES`]와 레지스트리의 `OCI-Chunk-Min-Length` 중 큰 것이다.
//! - 오류 문구에는 올리기 주소의 쿼리를 넣지 않는다(세션을 여는 열쇠일 수 있다, [`crate::registry::shown`]).
//!   자격 증명도 넣지 않는다. 레지스트리 오류 본문(`{"errors":[{"code","message"}]}`, [`parse_errors`])은 붙인다.
//!   `DIGEST_INVALID`, `SIZE_INVALID`는 digest 오류([`ImageError::Integrity`])다.
//! - 인증은 pull과 같은 파일에서 찾고 scope `repository:<저장소>:pull,push`로 요청한다. mount 요청만
//!   `repository:<원래 저장소>:pull`을 더한다.
//! - `registries.conf`: 이름이 가리키는 레지스트리로만 올린다. 미러와 `location` 바꾸기는 쓰지 않고, `blocked`면
//!   거부한다([`crate::registries::Registries::check_push`]). `insecure`는 pull과 같다.
//! - `policy.json`은 받을 이미지의 정책이라 push에는 쓰지 않는다. containers/image도 올릴 곳에는 정책을 보지 않는다
//!   (정책은 원본, sbx에서는 이미 받은 이미지에 적용됐다).

use std::future::Future;
use std::io::{self, Read};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use http_body::{Frame, SizeHint};
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, HeaderName, LOCATION, RANGE};
use reqwest::{Method, Response, StatusCode, Url};

use sbx_image::{BlobContent, BlobPush, ImageError, PushTarget};
use sbx_model::Digest;

use crate::registry::{PushRequest, Registry, Target, shown};
use crate::{OciImageSource, Reference, sha256};

/// 나눠 보낼 때 조각 하나의 기본 크기.
pub const CHUNK_BYTES: u64 = 16 * 1024 * 1024;
/// 레지스트리가 요구하는 조각 최소 크기(`OCI-Chunk-Min-Length`)를 이만큼까지만 따른다(조각은 메모리가 아니라
/// 파일에서 바로 보내지만, 터무니없는 값은 레지스트리 오류로 본다).
pub const MAX_CHUNK_BYTES: u64 = 1024 * 1024 * 1024;
/// 하나도 더 받지 않은 조각이 이만큼 잇달면 그만둔다.
pub const MAX_STALLS: u32 = 3;
/// 레지스트리 오류 본문을 읽는 최대 크기.
pub const MAX_ERROR_BODY: usize = 16 * 1024;

const OCI_CHUNK_MIN: &str = "oci-chunk-min-length";
const DOCKER_CONTENT_DIGEST: &str = "docker-content-digest";
const OCTET_STREAM: &str = "application/octet-stream";

/// `Range` 헤더(`0-1023`, `bytes=0-1023`)의 처음과 끝(끝 포함).
pub fn parse_range(s: &str) -> Option<(u64, u64)> {
    let s = s.trim();
    let s = match s.get(..6) {
        Some(p) if p.eq_ignore_ascii_case("bytes=") => &s[6..],
        _ => s,
    };
    let (a, b) = s.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    if a.is_empty() || b.is_empty() || !a.bytes().chain(b.bytes()).all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (start, end) = (a.parse().ok()?, b.parse().ok()?);
    (start <= end).then_some((start, end))
}

/// 레지스트리가 준 `Location`을 그 응답을 준 요청의 주소(`base`)에 대어 푼다. http(s)만 받는다.
pub fn resolve_location(base: &Url, location: &str) -> Result<Url, String> {
    let u = base
        .join(location.trim())
        .map_err(|e| format!("the registry sent an unusable Location ({e})"))?;
    match u.scheme() {
        "http" | "https" if u.host_str().is_some() => Ok(u),
        other => Err(format!(
            "the registry sent a Location with scheme {other:?}"
        )),
    }
}

/// 올리기 주소에 `digest`를 더한다. 있던 쿼리는 그대로 둔다.
pub fn with_digest(url: &Url, digest: &Digest) -> Url {
    let mut u = url.clone();
    u.query_pairs_mut().append_pair("digest", digest.as_str());
    u
}

/// 레지스트리 오류 본문(`{"errors":[{"code":"…","message":"…"}]}`)을 한 줄로. 모양이 다르면 `None`.
/// 다섯 개까지, 하나에 200자까지, 제어 문자는 뺀다.
pub fn parse_errors(body: &[u8]) -> Option<(Vec<String>, String)> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let errors = v.get("errors")?.as_array()?;
    let clean = |s: &str| -> String {
        s.chars()
            .filter(|c| !c.is_control())
            .take(200)
            .collect::<String>()
    };
    let mut codes = Vec::new();
    let mut parts = Vec::new();
    for e in errors.iter().take(5) {
        let code = e.get("code").and_then(|c| c.as_str()).map(clean);
        let message = e.get("message").and_then(|m| m.as_str()).map(clean);
        parts.push(match (&code, &message) {
            (Some(c), Some(m)) if !m.is_empty() => format!("{c}: {m}"),
            (Some(c), _) => c.clone(),
            (None, Some(m)) => m.clone(),
            (None, None) => continue,
        });
        codes.extend(code);
    }
    (!parts.is_empty()).then(|| (codes, parts.join("; ")))
}

/// 나눠 보내기의 상태. 레지스트리가 알린 `Location`과 `Range`만 믿는다(모듈 문서의 규칙).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunked {
    size: u64,
    chunk: u64,
    /// 레지스트리가 받았다고 한 바이트 수(다음 조각의 처음).
    offset: u64,
    /// 지금까지 보낸 가장 먼 곳(끝 다음).
    sent: u64,
    url: Url,
    stalls: u32,
}

impl Chunked {
    /// `size` 바이트를 `chunk`씩, 세션 `url`에.
    pub fn new(url: Url, size: u64, chunk: u64) -> Self {
        Self {
            size,
            chunk: chunk.max(1),
            offset: 0,
            sent: 0,
            url,
            stalls: 0,
        }
    }

    /// 다음 요청을 보낼 주소.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// 레지스트리가 받았다고 한 바이트 수.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// 다음에 보낼 조각(처음, 길이). 다 받았으면 `None`.
    pub fn next(&self) -> Option<(u64, u64)> {
        (self.offset < self.size).then(|| (self.offset, self.chunk.min(self.size - self.offset)))
    }

    /// `next`의 조각을 보냈고 레지스트리가 202로 답했다. 그 `Location`과 `Range`로 상태를 고친다.
    /// `Range`가 없으면 보낸 것을 다 받은 것으로 본다(규약은 반드시 보내라고 하지만 빼먹는 레지스트리가 있다).
    pub fn accepted(&mut self, location: Option<&str>, range: Option<&str>) -> Result<(), String> {
        let (start, len) = self.next().ok_or("nothing left to send")?;
        self.sent = self.sent.max(start + len);
        self.follow(location)?;
        let received = match range {
            Some(r) => self.received(r)?,
            None => start + len,
        };
        self.advance(received)
    }

    /// 416 뒤 세션 상태(`GET <Location>`의 204)로 다시 맞춘다.
    pub fn resync(&mut self, location: Option<&str>, range: Option<&str>) -> Result<(), String> {
        self.follow(location)?;
        let received = match range {
            Some(r) => self.received(r)?,
            // 아직 아무것도 받지 않은 세션은 Range를 보내지 않을 수 있다.
            None => 0,
        };
        self.advance(received)
    }

    fn follow(&mut self, location: Option<&str>) -> Result<(), String> {
        let loc = location.ok_or("the registry did not say where to continue (no Location)")?;
        self.url = resolve_location(&self.url, loc)?;
        Ok(())
    }

    /// `Range` 값에서 받은 바이트 수.
    fn received(&self, range: &str) -> Result<u64, String> {
        let (start, end) =
            parse_range(range).ok_or_else(|| format!("the registry sent a bad Range {range:?}"))?;
        if start != 0 {
            return Err(format!(
                "the registry's Range {range:?} does not start at 0"
            ));
        }
        let got = end + 1;
        if got > self.sent {
            return Err(format!(
                "the registry says it has {got} bytes but sbx sent {}",
                self.sent
            ));
        }
        Ok(got)
    }

    fn advance(&mut self, received: u64) -> Result<(), String> {
        if received < self.offset {
            return Err(format!(
                "the registry now has {received} bytes after saying it had {}",
                self.offset
            ));
        }
        if received == self.offset {
            self.stalls += 1;
            if self.stalls >= MAX_STALLS {
                return Err(format!(
                    "the registry took no more bytes {MAX_STALLS} times in a row (at {received})"
                ));
            }
        } else {
            self.stalls = 0;
        }
        self.offset = received;
        Ok(())
    }
}

/// 본문으로 나간 바이트 수. 본문(연결 작업이 읽는다)과 기다리는 쪽([`watch`])이 함께 본다.
#[derive(Default)]
struct Sent {
    bytes: AtomicU64,
    waker: Mutex<Option<Waker>>,
}

impl Sent {
    fn add(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
        if let Some(w) = self.waker.lock().ok().and_then(|mut w| w.take()) {
            w.wake();
        }
    }
}

/// 읽는 대로 보내는 본문. 크기가 정해져 있다(`Content-Length`).
struct ReadBody {
    reader: Box<dyn Read + Send + Sync>,
    left: u64,
    sent: Arc<Sent>,
}

/// 한 번에 읽는 크기.
const READ_BYTES: u64 = 64 * 1024;

impl http_body::Body for ReadBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if this.left == 0 {
            return Poll::Ready(None);
        }
        // 로컬 파일이라 막히는 시간이 짧다(pull도 받은 것을 같은 자리에서 파일에 쓴다).
        let mut buf = vec![0u8; this.left.min(READ_BYTES) as usize];
        let got = match this.reader.read(&mut buf) {
            Ok(0) => {
                return Poll::Ready(Some(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the blob is shorter than its size",
                ))));
            }
            Ok(n) => n,
            Err(e) => return Poll::Ready(Some(Err(e))),
        };
        buf.truncate(got);
        this.left -= got as u64;
        this.sent.add(got as u64);
        Poll::Ready(Some(Ok(Frame::data(Bytes::from(buf)))))
    }

    fn is_end_stream(&self) -> bool {
        self.left == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.left)
    }
}

/// `fut`를 기다리는 동안 본문이 보낸 바이트를 `report(base + 보낸 것)`로 알린다.
async fn watch<F: Future>(
    fut: F,
    sent: &Sent,
    base: u64,
    report: &(dyn Fn(u64) + Sync),
) -> F::Output {
    let mut fut = std::pin::pin!(fut);
    let mut last = None;
    std::future::poll_fn(move |cx| {
        if let Ok(mut w) = sent.waker.lock() {
            *w = Some(cx.waker().clone());
        }
        let n = sent.bytes.load(Ordering::Relaxed);
        if last != Some(n) {
            last = Some(n);
            report(base + n);
        }
        fut.as_mut().poll(cx)
    })
    .await
}

/// 응답 본문의 앞부분(오류 문구용).
async fn error_body(mut resp: Response) -> Vec<u8> {
    let mut out = Vec::new();
    while let Ok(Some(chunk)) = resp.chunk().await {
        let room = MAX_ERROR_BODY.saturating_sub(out.len());
        out.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if out.len() >= MAX_ERROR_BODY {
            break;
        }
    }
    out
}

fn header<'r>(resp: &'r Response, name: &str) -> Option<&'r str> {
    resp.headers().get(name).and_then(|v| v.to_str().ok())
}

/// 레지스트리가 알린 digest가 있으면 기대한 것과 같아야 한다.
fn check_digest(resp: &Response, url: &Url, expected: &Digest) -> Result<(), ImageError> {
    match header(resp, DOCKER_CONTENT_DIGEST) {
        Some(d) if d.trim() != expected.as_str() => Err(ImageError::Integrity(format!(
            "{}: the registry stored {} instead of {expected}",
            shown(url),
            d.trim()
        ))),
        _ => Ok(()),
    }
}

/// 실패한 응답을 오류로. 레지스트리 오류 본문이 있으면 붙인다.
async fn failed(url: &Url, host: &str, resp: Response) -> ImageError {
    let status = resp.status();
    let body = error_body(resp).await;
    let (codes, detail) = parse_errors(&body).unwrap_or_default();
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!(": {detail}")
    };
    if codes
        .iter()
        .any(|c| c == "DIGEST_INVALID" || c == "SIZE_INVALID")
    {
        return ImageError::Integrity(format!(
            "{}: the registry rejected the content (HTTP {status}){detail}",
            shown(url)
        ));
    }
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let login = crate::auth::registry_host(host);
        return ImageError::Transport(format!(
            "{}: push access denied (HTTP {status}){detail}; `sbx login {login}` with an account that may push here",
            shown(url)
        ));
    }
    ImageError::Transport(format!("{}: HTTP {status}{detail}", shown(url)))
}

/// 세션 하나: 올리기 주소와 레지스트리가 요구한 조각 최소 크기.
struct Upload {
    url: Url,
    min_chunk: Option<u64>,
}

/// mount 요청의 결과.
enum Started {
    Mounted,
    /// 끌어오지 않고 세션을 열었다.
    Session(Upload),
}

/// 한 번에 보내기의 결과.
enum Whole {
    Done,
    /// 413: 나눠 보내야 한다.
    TooLarge,
}

/// 대상 저장소 하나에 대한 요청들.
struct Session<'a> {
    registry: &'a Registry,
    host: String,
    repository: String,
    scopes: Vec<String>,
}

impl Session<'_> {
    async fn send(
        &self,
        scopes: &[String],
        method: Method,
        target: Target,
        headers: Vec<(HeaderName, String)>,
        body: Option<crate::registry::BodyFn<'_>>,
    ) -> Result<(Url, Response), ImageError> {
        self.registry
            .push_request(
                &self.host,
                &self.repository,
                scopes,
                &PushRequest {
                    method,
                    target,
                    headers,
                    body,
                },
            )
            .await
    }

    async fn exists(&self, digest: &Digest) -> Result<bool, ImageError> {
        let (url, resp) = self
            .send(
                &self.scopes,
                Method::HEAD,
                Target::Path(format!("{}/blobs/{digest}", self.repository)),
                Vec::new(),
                None,
            )
            .await?;
        match resp.status() {
            s if s.is_success() => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(failed(&url, &self.host, resp).await),
        }
    }

    /// 세션을 연다(202와 `Location`).
    async fn start(&self, query: &str, scopes: &[String]) -> Result<Started, ImageError> {
        let (url, resp) = self
            .send(
                scopes,
                Method::POST,
                Target::Path(format!("{}/blobs/uploads/{query}", self.repository)),
                vec![(CONTENT_LENGTH, "0".into())],
                None,
            )
            .await?;
        match resp.status() {
            StatusCode::CREATED if !query.is_empty() => Ok(Started::Mounted),
            StatusCode::ACCEPTED => {
                let loc = header(&resp, LOCATION.as_str()).ok_or_else(|| {
                    ImageError::Transport(format!(
                        "{}: the registry opened an upload without a Location",
                        shown(&url)
                    ))
                })?;
                let at = resolve_location(&url, loc)
                    .map_err(|e| ImageError::Transport(format!("{}: {e}", shown(&url))))?;
                let min_chunk = header(&resp, OCI_CHUNK_MIN).and_then(|v| v.trim().parse().ok());
                Ok(Started::Session(Upload { url: at, min_chunk }))
            }
            _ => Err(failed(&url, &self.host, resp).await),
        }
    }

    async fn open(&self) -> Result<Upload, ImageError> {
        match self.start("", &self.scopes).await? {
            Started::Session(u) => Ok(u),
            Started::Mounted => unreachable!("a plain upload is never a mount"),
        }
    }

    /// `from`(`레지스트리/저장소`)에서 끌어온다.
    async fn mount(&self, digest: &Digest, from: &str) -> Result<Started, ImageError> {
        let from_repo = Reference::parse(from)
            .map_err(ImageError::Reference)?
            .repository;
        let mut scopes = self.scopes.clone();
        scopes.push(format!("repository:{from_repo}:pull"));
        let mut q = Url::parse("x:/").map_err(|e| ImageError::Transport(e.to_string()))?;
        q.query_pairs_mut()
            .append_pair("mount", digest.as_str())
            .append_pair("from", &from_repo);
        let query = format!("?{}", q.query().unwrap_or_default());
        self.start(&query, &scopes).await
    }

    /// 세션을 지운다. 해 보기만 한다.
    async fn cancel(&self, url: &Url) {
        let _ = self
            .send(
                &self.scopes,
                Method::DELETE,
                Target::Url(url.clone()),
                Vec::new(),
                None,
            )
            .await;
    }

    /// `content`의 `offset`부터 `len` 바이트를 보내는 본문. 다시 만들면 보낸 바이트 수도 처음으로 돌린다.
    fn body<'b>(
        content: &'b dyn BlobContent,
        offset: u64,
        len: u64,
        sent: &'b Arc<Sent>,
    ) -> impl Fn() -> Result<reqwest::Body, ImageError> + Sync + 'b {
        move || {
            sent.bytes.store(0, Ordering::Relaxed);
            let r = content.open(offset)?;
            Ok(reqwest::Body::wrap(ReadBody {
                reader: Box::new(r.take(len)),
                left: len,
                sent: sent.clone(),
            }))
        }
    }

    async fn put_whole(
        &self,
        up: &Upload,
        digest: &Digest,
        content: &dyn BlobContent,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<Whole, ImageError> {
        let size = content.size();
        let sent = Arc::new(Sent::default());
        let body = Self::body(content, 0, size, &sent);
        let (url, resp) = watch(
            self.send(
                &self.scopes,
                Method::PUT,
                Target::Url(with_digest(&up.url, digest)),
                vec![
                    (CONTENT_TYPE, OCTET_STREAM.into()),
                    (CONTENT_LENGTH, size.to_string()),
                ],
                Some(&body),
            ),
            &sent,
            0,
            progress,
        )
        .await?;
        match resp.status() {
            s if s.is_success() => {
                check_digest(&resp, &url, digest)?;
                Ok(Whole::Done)
            }
            StatusCode::PAYLOAD_TOO_LARGE => Ok(Whole::TooLarge),
            _ => Err(failed(&url, &self.host, resp).await),
        }
    }

    async fn put_chunks(
        &self,
        up: Upload,
        digest: &Digest,
        content: &dyn BlobContent,
        chunk: u64,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<(), ImageError> {
        let chunk = up
            .min_chunk
            .map_or(chunk, |m| chunk.max(m.min(MAX_CHUNK_BYTES)));
        let mut state = Chunked::new(up.url, content.size(), chunk);
        let bad = |url: &Url, e: String| ImageError::Transport(format!("{}: {e}", shown(url)));
        while let Some((start, len)) = state.next() {
            let sent = Arc::new(Sent::default());
            let body = Self::body(content, start, len, &sent);
            let (url, resp) = watch(
                self.send(
                    &self.scopes,
                    Method::PATCH,
                    Target::Url(state.url().clone()),
                    vec![
                        (CONTENT_TYPE, OCTET_STREAM.into()),
                        (CONTENT_LENGTH, len.to_string()),
                        (CONTENT_RANGE, format!("{start}-{}", start + len - 1)),
                    ],
                    Some(&body),
                ),
                &sent,
                start,
                progress,
            )
            .await?;
            match resp.status() {
                StatusCode::ACCEPTED | StatusCode::NO_CONTENT => {
                    let (loc, range) = (
                        header(&resp, LOCATION.as_str()).map(str::to_owned),
                        header(&resp, RANGE.as_str()).map(str::to_owned),
                    );
                    state
                        .accepted(loc.as_deref(), range.as_deref())
                        .map_err(|e| bad(&url, e))?;
                }
                StatusCode::RANGE_NOT_SATISFIABLE => {
                    let (url, resp) = self
                        .send(
                            &self.scopes,
                            Method::GET,
                            Target::Url(state.url().clone()),
                            Vec::new(),
                            None,
                        )
                        .await?;
                    if !resp.status().is_success() {
                        return Err(failed(&url, &self.host, resp).await);
                    }
                    // 상태 응답의 Location이 없으면 같은 주소에 계속한다.
                    let loc = header(&resp, LOCATION.as_str())
                        .map_or_else(|| url.to_string(), str::to_owned);
                    let range = header(&resp, RANGE.as_str()).map(str::to_owned);
                    state
                        .resync(Some(&loc), range.as_deref())
                        .map_err(|e| bad(&url, e))?;
                }
                _ => return Err(failed(&url, &self.host, resp).await),
            }
            progress(state.offset());
        }
        let (url, resp) = self
            .send(
                &self.scopes,
                Method::PUT,
                Target::Url(with_digest(state.url(), digest)),
                vec![(CONTENT_LENGTH, "0".into())],
                None,
            )
            .await?;
        if !resp.status().is_success() {
            return Err(failed(&url, &self.host, resp).await);
        }
        check_digest(&resp, &url, digest)
    }

    /// 세션 `up`에 올린다. 실패하면 세션을 지운다.
    async fn upload(
        &self,
        up: Upload,
        digest: &Digest,
        content: &dyn BlobContent,
        chunk: u64,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<(), ImageError> {
        let first = up.url.clone();
        match self.put_whole(&up, digest, content, progress).await {
            Ok(Whole::Done) => Ok(()),
            Ok(Whole::TooLarge) => {
                self.cancel(&first).await;
                let up = self.open().await?;
                let at = up.url.clone();
                let r = self.put_chunks(up, digest, content, chunk, progress).await;
                if r.is_err() {
                    self.cancel(&at).await;
                }
                r
            }
            Err(e) => {
                self.cancel(&first).await;
                Err(e)
            }
        }
    }
}

impl OciImageSource {
    /// 참조를 올릴 곳으로. 모듈 문서의 규칙을 따른다. 레지스트리에 가지 않는다.
    pub(crate) fn push_target(
        &self,
        reference: &str,
        blobs_from: Option<&str>,
    ) -> Result<PushTarget, ImageError> {
        let r = Reference::parse(reference).map_err(ImageError::Reference)?;
        if r.digest.is_some() {
            return Err(ImageError::Unsupported(format!(
                "{reference}: push needs a tag, not a digest"
            )));
        }
        self.state()?.registries.check_push(&r)?;
        let mount_from = blobs_from
            .and_then(|s| Reference::parse(s).ok())
            .filter(|f| f.registry == r.registry && f.repository != r.repository)
            .map(|f| format!("{}/{}", f.registry, f.repository));
        Ok(PushTarget {
            reference: r.to_string(),
            mount_from,
        })
    }

    fn session(&self, target: &PushTarget) -> Result<(Reference, Session<'_>), ImageError> {
        let r = Reference::parse(&target.reference).map_err(ImageError::Reference)?;
        let s = Session {
            registry: &self.state()?.registry,
            host: r.api_host().to_owned(),
            repository: r.repository.clone(),
            scopes: vec![format!("repository:{}:pull,push", r.repository)],
        };
        Ok((r, s))
    }

    fn chunk_bytes(&self) -> u64 {
        self.chunk.unwrap_or(CHUNK_BYTES)
    }

    fn mount_refused(&self, key: &str) -> bool {
        self.no_mount.lock().is_ok_and(|n| n.contains(key))
    }

    fn refuse_mount(&self, key: String) {
        if let Ok(mut n) = self.no_mount.lock() {
            n.insert(key);
        }
    }

    pub(crate) async fn push_blob_inner(
        &self,
        target: &PushTarget,
        digest: &Digest,
        content: &dyn BlobContent,
        progress: &(dyn Fn(u64) + Sync),
    ) -> Result<BlobPush, ImageError> {
        let (_, s) = self.session(target)?;
        if s.exists(digest).await? {
            progress(content.size());
            return Ok(BlobPush::Exists);
        }
        let mut opened = None;
        if let Some(from) = &target.mount_from {
            let key = format!("{} {from}", target.reference);
            if !self.mount_refused(&key) {
                match s.mount(digest, from).await {
                    Ok(Started::Mounted) => {
                        progress(content.size());
                        return Ok(BlobPush::Mounted(from.clone()));
                    }
                    // 레지스트리가 끌어오지 않고 세션을 열었다: 그 세션에 올린다.
                    Ok(Started::Session(up)) => {
                        self.refuse_mount(key);
                        opened = Some(up);
                    }
                    Err(_) => self.refuse_mount(key),
                }
            }
        }
        let up = match opened {
            Some(up) => up,
            None => s.open().await?,
        };
        s.upload(up, digest, content, self.chunk_bytes(), progress)
            .await?;
        Ok(BlobPush::Uploaded)
    }

    pub(crate) async fn push_manifest_inner(
        &self,
        target: &PushTarget,
        media_type: &str,
        manifest: &[u8],
    ) -> Result<(), ImageError> {
        let (r, s) = self.session(target)?;
        let tag = r
            .tag
            .clone()
            .ok_or_else(|| ImageError::Unsupported("push needs a tag".into()))?;
        let body = || Ok(reqwest::Body::from(manifest.to_vec()));
        let (url, resp) = s
            .send(
                &s.scopes,
                Method::PUT,
                Target::Path(format!("{}/manifests/{tag}", r.repository)),
                vec![
                    (CONTENT_TYPE, media_type.to_owned()),
                    (CONTENT_LENGTH, manifest.len().to_string()),
                ],
                Some(&body),
            )
            .await?;
        if !resp.status().is_success() {
            return Err(failed(&url, &s.host, resp).await);
        }
        check_digest(&resp, &url, &sha256(manifest))
    }
}

#[cfg(test)]
mod tests;
