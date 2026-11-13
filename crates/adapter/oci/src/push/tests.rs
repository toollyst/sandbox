use super::*;

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

use sbx_image::ImageDestination;

// ── 규약 조각(입출력 없음) ──────────────────────────────────────────────

#[test]
fn ranges_are_inclusive_and_may_say_bytes() {
    assert_eq!(parse_range("0-1023"), Some((0, 1023)));
    assert_eq!(parse_range(" bytes=0-0 "), Some((0, 0)));
    assert_eq!(parse_range("BYTES=5-9"), Some((5, 9)));
    for bad in [
        "", "-", "0-", "-5", "5-4", "a-b", "0 - 1x", "0-+1", "bytes=",
    ] {
        assert_eq!(parse_range(bad), None, "{bad:?}");
    }
    assert_eq!(parse_range("0-18446744073709551616"), None);
}

#[test]
fn locations_are_relative_to_the_request_and_keep_their_query() {
    let base = Url::parse("https://reg.example/v2/a/b/blobs/uploads/").unwrap();
    let u = resolve_location(&base, "/v2/a/b/blobs/uploads/123?_state=xyz").unwrap();
    assert_eq!(
        u.as_str(),
        "https://reg.example/v2/a/b/blobs/uploads/123?_state=xyz"
    );
    // 경로만 준 상대 주소.
    let u = resolve_location(&base, "123").unwrap();
    assert_eq!(u.as_str(), "https://reg.example/v2/a/b/blobs/uploads/123");
    // 절대 주소는 그대로(다른 곳이면 보낼 때 registry의 규칙을 따진다).
    let u = resolve_location(&base, "https://cdn.example/up/1?sig=s").unwrap();
    assert_eq!(u.host_str(), Some("cdn.example"));
    assert!(resolve_location(&base, "file:///etc/passwd").is_err());
    assert!(resolve_location(&base, "ftp://x/y").is_err());

    let d = Digest::parse(&format!("sha256:{}", "ab".repeat(32))).unwrap();
    let with = with_digest(&resolve_location(&base, "/u/1?_state=a%2Bb").unwrap(), &d);
    assert_eq!(
        with.as_str(),
        format!(
            "https://reg.example/u/1?_state=a%2Bb&digest=sha256%3A{}",
            "ab".repeat(32)
        )
    );
    let pairs: Vec<(String, String)> = with.query_pairs().into_owned().collect();
    assert_eq!(pairs[0], ("_state".into(), "a+b".into()));
    assert_eq!(pairs[1], ("digest".into(), d.to_string()));
    // 오류 문구에는 쿼리가 나오지 않는다.
    assert_eq!(shown(&with), "https://reg.example/u/1");
}

#[test]
fn chunks_follow_the_registrys_location_and_range() {
    let base = Url::parse("http://r/v2/x/blobs/uploads/1?_state=0").unwrap();
    let mut c = Chunked::new(base, 10, 4);
    assert_eq!(c.next(), Some((0, 4)));
    c.accepted(Some("/v2/x/blobs/uploads/1?_state=4"), Some("0-3"))
        .unwrap();
    assert_eq!(c.url().query(), Some("_state=4"));
    assert_eq!(c.next(), Some((4, 4)));
    // 레지스트리가 덜 받았다: 거기부터 다시.
    c.accepted(Some("?_state=6"), Some("bytes=0-5")).unwrap();
    assert_eq!(c.next(), Some((6, 4)));
    assert_eq!(c.url().path(), "/v2/x/blobs/uploads/1");
    // Range가 없으면 다 받은 것으로 본다. 마지막 조각은 남은 만큼.
    c.accepted(Some("/next"), None).unwrap();
    assert_eq!(c.offset(), 10);
    assert_eq!(c.next(), None);
    assert!(c.accepted(Some("/x"), None).is_err());
}

#[test]
fn chunk_rules_refuse_what_cannot_be() {
    let base = Url::parse("http://r/u").unwrap();
    // Location이 없으면 어디로 이어 갈지 모른다.
    let mut c = Chunked::new(base.clone(), 10, 4);
    assert!(
        c.accepted(None, Some("0-3"))
            .unwrap_err()
            .contains("Location")
    );
    // 0에서 시작하지 않는 Range.
    let mut c = Chunked::new(base.clone(), 10, 4);
    assert!(
        c.accepted(Some("/u"), Some("1-3"))
            .unwrap_err()
            .contains("0")
    );
    // 보낸 것보다 많이 받았다.
    let mut c = Chunked::new(base.clone(), 10, 4);
    assert!(
        c.accepted(Some("/u"), Some("0-7"))
            .unwrap_err()
            .contains("sent 4")
    );
    // 받았다던 것이 줄었다.
    let mut c = Chunked::new(base.clone(), 10, 4);
    c.accepted(Some("/u"), Some("0-3")).unwrap();
    assert!(
        c.accepted(Some("/u"), Some("0-1"))
            .unwrap_err()
            .contains("after saying")
    );
    // 하나도 더 받지 않는 일이 잇달면 그만둔다.
    let mut c = Chunked::new(base.clone(), 10, 4);
    c.accepted(Some("/u"), Some("0-3")).unwrap();
    for _ in 1..MAX_STALLS {
        c.accepted(Some("/u"), Some("0-3")).unwrap();
    }
    assert!(
        c.accepted(Some("/u"), Some("0-3"))
            .unwrap_err()
            .contains("no more")
    );
    // 416 뒤 상태: 아무것도 받지 않은 세션은 Range가 없을 수 있다.
    let mut c = Chunked::new(base.clone(), 10, 4);
    c.accepted(Some("/u"), Some("0-3")).unwrap();
    assert!(c.resync(Some("/u"), None).is_err());
    let mut c = Chunked::new(base, 10, 4);
    c.resync(Some("/v"), None).unwrap();
    assert_eq!((c.offset(), c.url().path()), (0, "/v"));
}

#[test]
fn registry_errors_become_one_line() {
    let (codes, s) = parse_errors(
        br#"{"errors":[{"code":"DIGEST_INVALID","message":"provided digest did not match","detail":{}},{"code":"X"}]}"#,
    )
    .unwrap();
    assert_eq!(codes, ["DIGEST_INVALID", "X"]);
    assert_eq!(s, "DIGEST_INVALID: provided digest did not match; X");
    assert_eq!(parse_errors(b"not json"), None);
    assert_eq!(parse_errors(br#"{"errors":[]}"#), None);
    assert_eq!(parse_errors(br#"{"errors":"x"}"#), None);
    let (_, s) = parse_errors(br#"{"errors":[{"message":"a\u001b[31mb"}]}"#).unwrap();
    assert_eq!(s, "a[31mb");
}

// ── 흉내 레지스트리 ─────────────────────────────────────────────────────

/// 흉내 레지스트리의 동작 손잡이.
#[derive(Clone, Default)]
struct Knobs {
    /// Bearer 토큰을 요구한다. 챌린지의 scope는 `pull`만 말한다. 쓰기는 push scope로 받은 토큰만 된다.
    bearer: bool,
    /// cross-repository mount를 받아들인다(원래 저장소에 blob이 있으면). 아니면 202로 세션을 연다.
    mount: bool,
    /// 한 번에 보내는 PUT의 본문이 이보다 크면 413.
    max_body: Option<usize>,
    /// 이 길이의 blob을 올리는 PUT에서 응답 없이 연결을 끊는다.
    drop_put_of: Option<usize>,
    /// 첫 PATCH는 절반만 받는다.
    short_first_patch: bool,
    /// Location을 자기 주소의 절대 주소로 준다.
    absolute_self: bool,
    /// Location을 이 호스트(다른 곳)의 절대 주소로 준다.
    absolute_other: Option<String>,
    /// Basic 인증만 받는다(`me:pw`).
    basic: bool,
}

#[derive(Default)]
struct Upload {
    repo: String,
    data: Vec<u8>,
    /// 마지막으로 준 Location의 `_state`. 다음 요청은 그것을 가져와야 한다.
    state: String,
}

#[derive(Default)]
struct Store {
    blobs: HashMap<(String, String), Vec<u8>>,
    manifests: HashMap<(String, String), (String, Vec<u8>)>,
    uploads: HashMap<u32, Upload>,
    next: u32,
    /// `메서드 경로?쿼리` 그리고 받은 Content-Range(있으면).
    log: Vec<String>,
    /// 토큰 요청의 scope들과 Authorization.
    tokens: Vec<(Vec<String>, Option<String>)>,
    patches: usize,
}

struct Fake {
    port: u16,
    store: Arc<Mutex<Store>>,
}

impl Fake {
    fn host(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
    fn log(&self) -> Vec<String> {
        self.store.lock().unwrap().log.clone()
    }
    fn has_blob(&self, repo: &str, d: &Digest) -> bool {
        self.store
            .lock()
            .unwrap()
            .blobs
            .contains_key(&(repo.into(), d.to_string()))
    }
    fn manifest(&self, repo: &str, tag: &str) -> Option<(String, Vec<u8>)> {
        self.store
            .lock()
            .unwrap()
            .manifests
            .get(&(repo.into(), tag.into()))
            .cloned()
    }
    fn put_blob(&self, repo: &str, data: &[u8]) {
        self.store
            .lock()
            .unwrap()
            .blobs
            .insert((repo.into(), sha256(data).to_string()), data.to_vec());
    }
}

struct Req {
    method: String,
    path: String,
    query: HashMap<String, String>,
    raw_query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn read_req(c: &TcpStream) -> Option<Req> {
    let mut r = BufReader::new(c.try_clone().ok()?);
    let mut first = String::new();
    r.read_line(&mut first).ok()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_owned();
    let target = parts.next()?.to_owned();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    std::io::Read::read_exact(&mut r, &mut body).ok()?;
    let (path, raw_query) = target
        .split_once('?')
        .map_or((target.clone(), String::new()), |(p, q)| {
            (p.into(), q.into())
        });
    let query = Url::parse(&format!("http://x/?{raw_query}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    Some(Req {
        method,
        path,
        query,
        raw_query,
        headers,
        body,
    })
}

fn reply(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let mut s = format!("HTTP/1.1 {status}\r\n");
    for (k, v) in headers {
        s.push_str(&format!("{k}: {v}\r\n"));
    }
    s.push_str(&format!(
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    ));
    let mut out = s.into_bytes();
    out.extend_from_slice(body);
    out
}

fn oci_error(status: &str, code: &str) -> Vec<u8> {
    reply(
        status,
        &[("Content-Type", "application/json".into())],
        format!(r#"{{"errors":[{{"code":"{code}","message":"{code} from the fake"}}]}}"#)
            .as_bytes(),
    )
}

/// `/v2/<저장소>/<kind>/<rest>`를 나눈다.
fn split_v2(path: &str) -> Option<(String, &str, &str)> {
    let rest = path.strip_prefix("/v2/")?;
    for kind in ["/blobs/uploads/", "/blobs/", "/manifests/"] {
        if let Some(i) = rest.find(kind) {
            return Some((rest[..i].to_owned(), kind, &rest[i + kind.len()..]));
        }
    }
    None
}

fn serve(knobs: Knobs) -> Fake {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let store: Arc<Mutex<Store>> = Arc::default();
    let shared = store.clone();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { return };
            let (store, knobs) = (shared.clone(), knobs.clone());
            std::thread::spawn(move || {
                let Some(req) = read_req(&c) else { return };
                if let Some(out) = handle(&knobs, port, &store, &req) {
                    let _ = c.write_all(&out);
                }
            });
        }
    });
    Fake { port, store }
}

/// 요청 하나. `None`이면 응답 없이 끊는다.
fn handle(k: &Knobs, port: u16, store: &Mutex<Store>, req: &Req) -> Option<Vec<u8>> {
    let mut st = store.lock().unwrap();
    let line = match req.headers.get("content-range") {
        Some(r) => format!("{} {}?{} [{r}]", req.method, req.path, req.raw_query),
        None => format!("{} {}?{}", req.method, req.path, req.raw_query),
    };
    st.log.push(line);
    if req.path == "/token" {
        let scopes: Vec<String> = Url::parse(&format!("http://x/?{}", req.raw_query))
            .unwrap()
            .query_pairs()
            .filter(|(k, _)| k == "scope")
            .map(|(_, v)| v.into_owned())
            .collect();
        // push는 자격 증명(`me:pw`)이 있을 때만 준다. 익명에는 pull만.
        let signed_in = req.headers.get("authorization").map(String::as_str)
            == Some(&format!("Basic {}", crate::auth::base64_encode(b"me:pw")));
        let token = if signed_in && scopes.iter().any(|s| s.ends_with(":pull,push")) {
            "push"
        } else {
            "pull"
        };
        st.tokens
            .push((scopes, req.headers.get("authorization").cloned()));
        return Some(reply(
            "200 OK",
            &[("Content-Type", "application/json".into())],
            format!(r#"{{"token":"{token}"}}"#).as_bytes(),
        ));
    }
    let (repo, kind, rest) = split_v2(&req.path)?;
    if k.basic
        && req.headers.get("authorization").map(String::as_str)
            != Some(&format!("Basic {}", crate::auth::base64_encode(b"me:pw")))
    {
        return Some(reply(
            "401 Unauthorized",
            &[("WWW-Authenticate", r#"Basic realm="fake""#.into())],
            b"",
        ));
    }
    if k.bearer {
        let auth = req.headers.get("authorization").map(String::as_str);
        let reading = matches!(req.method.as_str(), "GET" | "HEAD");
        let ok = auth == Some("Bearer push") || (reading && auth == Some("Bearer pull"));
        if !ok {
            return Some(reply(
                "401 Unauthorized",
                &[(
                    "WWW-Authenticate",
                    format!(
                        r#"Bearer realm="http://127.0.0.1:{port}/token",service="fake",scope="repository:{repo}:pull""#
                    ),
                )],
                b"",
            ));
        }
    }
    let location = |id: u32, state: &str| -> String {
        let path = format!("/v2/{repo}/blobs/uploads/{id}?_state={state}");
        match &k.absolute_other {
            Some(h) => format!("http://{h}{path}"),
            None if k.absolute_self => format!("http://127.0.0.1:{port}{path}"),
            None => path,
        }
    };
    let digest_header = |d: &str| ("Docker-Content-Digest", d.to_owned());
    match (req.method.as_str(), kind) {
        ("HEAD" | "GET", "/blobs/") => Some(match st.blobs.get(&(repo.clone(), rest.into())) {
            Some(b) if req.method == "GET" => reply("200 OK", &[], b),
            Some(b) => {
                let mut r = reply("200 OK", &[], b);
                // HEAD: 본문 없이 길이만.
                let cut = r.len() - b.len();
                r.truncate(cut);
                r
            }
            None => reply("404 Not Found", &[], b""),
        }),
        ("POST", "/blobs/uploads/") => {
            if let (Some(d), Some(from)) = (req.query.get("mount"), req.query.get("from"))
                && k.mount
                && let Some(b) = st.blobs.get(&(from.clone(), d.clone())).cloned()
            {
                st.blobs.insert((repo.clone(), d.clone()), b);
                return Some(reply(
                    "201 Created",
                    &[
                        ("Location", format!("/v2/{repo}/blobs/{d}")),
                        digest_header(d),
                    ],
                    b"",
                ));
            }
            st.next += 1;
            let id = st.next;
            let state = format!("s{id}-0");
            st.uploads.insert(
                id,
                Upload {
                    repo: repo.clone(),
                    data: Vec::new(),
                    state: state.clone(),
                },
            );
            Some(reply(
                "202 Accepted",
                &[("Location", location(id, &state)), ("Range", "0-0".into())],
                b"",
            ))
        }
        (m, "/blobs/uploads/") => {
            let id: u32 = rest.parse().ok()?;
            let patches = st.patches;
            let Some(up) = st.uploads.get_mut(&id) else {
                return Some(oci_error("404 Not Found", "BLOB_UPLOAD_UNKNOWN"));
            };
            assert_eq!(up.repo, repo);
            if m != "DELETE" && req.query.get("_state") != Some(&up.state) {
                return Some(oci_error("400 Bad Request", "BLOB_UPLOAD_INVALID"));
            }
            match m {
                "PATCH" => {
                    let (start, _) = parse_range(req.headers.get("content-range")?)?;
                    if start != up.data.len() as u64 {
                        return Some(reply("416 Range Not Satisfiable", &[], b""));
                    }
                    let take = if k.short_first_patch && patches == 0 {
                        req.body.len() / 2
                    } else {
                        req.body.len()
                    };
                    up.data.extend_from_slice(&req.body[..take]);
                    let len = up.data.len();
                    up.state = format!("s{id}-{len}");
                    let state = up.state.clone();
                    st.patches += 1;
                    Some(reply(
                        "202 Accepted",
                        &[
                            ("Location", location(id, &state)),
                            ("Range", format!("0-{}", len.saturating_sub(1))),
                        ],
                        b"",
                    ))
                }
                "PUT" => {
                    if k.drop_put_of.is_some_and(|n| n == req.body.len()) {
                        return None;
                    }
                    if k.max_body.is_some_and(|m| req.body.len() > m) {
                        return Some(oci_error("413 Payload Too Large", "SIZE_INVALID_NOT"));
                    }
                    up.data.extend_from_slice(&req.body);
                    let want = req.query.get("digest")?.clone();
                    if sha256(&up.data).to_string() != want {
                        return Some(oci_error("400 Bad Request", "DIGEST_INVALID"));
                    }
                    let data = std::mem::take(&mut up.data);
                    st.uploads.remove(&id);
                    st.blobs.insert((repo.clone(), want.clone()), data);
                    Some(reply(
                        "201 Created",
                        &[
                            ("Location", format!("/v2/{repo}/blobs/{want}")),
                            digest_header(&want),
                        ],
                        b"",
                    ))
                }
                "GET" => {
                    let len = up.data.len();
                    let state = up.state.clone();
                    let mut h = vec![("Location", location(id, &state))];
                    if len > 0 {
                        h.push(("Range", format!("0-{}", len - 1)));
                    }
                    Some(reply("204 No Content", &h, b""))
                }
                "DELETE" => {
                    st.uploads.remove(&id);
                    Some(reply("204 No Content", &[], b""))
                }
                _ => Some(reply("405 Method Not Allowed", &[], b"")),
            }
        }
        ("PUT", "/manifests/") => {
            let v: serde_json::Value = serde_json::from_slice(&req.body).ok()?;
            let mut refs = vec![v["config"]["digest"].as_str()?.to_owned()];
            for l in v["layers"].as_array()? {
                refs.push(l["digest"].as_str()?.to_owned());
            }
            if refs
                .iter()
                .any(|d| !st.blobs.contains_key(&(repo.clone(), d.clone())))
            {
                return Some(oci_error("400 Bad Request", "MANIFEST_BLOB_UNKNOWN"));
            }
            let ct = req.headers.get("content-type").cloned().unwrap_or_default();
            let d = sha256(&req.body).to_string();
            st.manifests
                .insert((repo.clone(), rest.into()), (ct, req.body.clone()));
            Some(reply("201 Created", &[digest_header(&d)], b""))
        }
        ("GET", "/manifests/") => Some(match st.manifests.get(&(repo.clone(), rest.into())) {
            Some((ct, b)) => reply("200 OK", &[("Content-Type", ct.clone())], b),
            None => reply("404 Not Found", &[], b""),
        }),
        _ => Some(reply("405 Method Not Allowed", &[], b"")),
    }
}

// ── 이미지와 올리기 ─────────────────────────────────────────────────────

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

/// 레이어 둘과 config, 그리고 그 manifest. 받은 그대로라는 것을 보려고 일부러 공백을 둔다.
fn image() -> (Vec<Vec<u8>>, Vec<u8>, Vec<u8>) {
    let layers = vec![b"first layer bytes".to_vec(), vec![7u8; 1000]];
    let config = br#"{"architecture":"amd64", "os":"linux"}"#.to_vec();
    let manifest = format!(
        r#"{{ "schemaVersion": 2, "mediaType": "{OCI_MANIFEST}",
  "config": {{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"{}","size":{}}},
  "layers": [{{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"{}","size":{}}},
             {{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":"{}","size":{}}}] }}"#,
        sha256(&config),
        config.len(),
        sha256(&layers[0]),
        layers[0].len(),
        sha256(&layers[1]),
        layers[1].len()
    )
    .into_bytes();
    (layers, config, manifest)
}

/// 이미지 전체를 순서대로(레이어, config, manifest) 올린다. blob마다 결과.
async fn push_all(src: &OciImageSource, target: &PushTarget) -> Result<Vec<BlobPush>, ImageError> {
    let (layers, config, manifest) = image();
    let mut out = Vec::new();
    for b in layers.iter().chain([&config]) {
        out.push(src.push_blob(target, &sha256(b), b, &|_| {}).await?);
    }
    src.push_manifest(target, OCI_MANIFEST, &manifest).await?;
    Ok(out)
}

fn auth_file(host: &str) -> crate::auth::AuthFiles {
    let d = std::env::temp_dir().join(format!(
        "sbx-push-auth-{}-{}",
        std::process::id(),
        host.replace(':', "_")
    ));
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("config.json");
    let auth = crate::auth::base64_encode(b"me:pw");
    std::fs::write(
        &p,
        format!(r#"{{"auths":{{"{host}":{{"auth":"{auth}"}}}}}}"#),
    )
    .unwrap();
    crate::auth::AuthFiles {
        read: vec![p],
        write: None,
    }
}

fn count(log: &[String], prefix: &str) -> usize {
    log.iter().filter(|l| l.starts_with(prefix)).count()
}

#[tokio::test]
async fn fresh_push_sends_each_blob_then_the_manifest_as_it_is() {
    // Location이 상대 주소든 레지스트리 자신의 절대 주소든 같다.
    for absolute_self in [false, true] {
        let fake = serve(Knobs {
            absolute_self,
            ..Knobs::default()
        });
        let src = OciImageSource::new().unwrap();
        let t = src
            .target(&format!("{}/team/app:1", fake.host()), None)
            .unwrap();
        assert_eq!(t.reference, format!("{}/team/app:1", fake.host()));
        assert_eq!(t.mount_from, None);

        let sizes = Mutex::new(Vec::new());
        let (layers, _, manifest) = image();
        let r = src
            .push_blob(&t, &sha256(&layers[1]), &layers[1], &|n| {
                sizes.lock().unwrap().push(n)
            })
            .await
            .unwrap();
        assert_eq!(r, BlobPush::Uploaded);
        assert_eq!(sizes.lock().unwrap().last(), Some(&1000));
        let got = push_all(&src, &t).await.unwrap();
        // 방금 올린 둘째 레이어는 이미 있다.
        assert_eq!(
            got,
            [BlobPush::Uploaded, BlobPush::Exists, BlobPush::Uploaded]
        );
        let (ct, bytes) = fake.manifest("team/app", "1").unwrap();
        assert_eq!((ct.as_str(), bytes), (OCI_MANIFEST, manifest));
        let log = fake.log();
        assert!(
            log.last()
                .unwrap()
                .starts_with("PUT /v2/team/app/manifests/1")
        );
        // 한 번에 보낸다: 세션을 열고 그 Location(`_state`)에 PUT, PATCH는 없다.
        assert_eq!(count(&log, "POST /v2/team/app/blobs/uploads/"), 3);
        assert_eq!(count(&log, "PATCH"), 0);
        assert!(
            log.iter().any(
                |l| l.starts_with("PUT /v2/team/app/blobs/uploads/1?_state=s1-0&digest=sha256")
            ),
            "{log:?}"
        );
    }
}

#[tokio::test]
async fn blobs_the_registry_has_are_not_sent() {
    let fake = serve(Knobs::default());
    let (layers, config, _) = image();
    for b in layers.iter().chain([&config]) {
        fake.put_blob("team/app", b);
    }
    let src = OciImageSource::new().unwrap();
    let t = src
        .target(&format!("{}/team/app:2", fake.host()), None)
        .unwrap();
    assert_eq!(push_all(&src, &t).await.unwrap(), vec![BlobPush::Exists; 3]);
    let log = fake.log();
    assert_eq!(count(&log, "HEAD"), 3);
    assert_eq!(count(&log, "POST"), 0);
    assert!(fake.manifest("team/app", "2").is_some());
}

#[tokio::test]
async fn blobs_are_mounted_from_the_repository_they_came_from() {
    let fake = serve(Knobs {
        mount: true,
        bearer: true,
        ..Knobs::default()
    });
    let (layers, config, _) = image();
    for b in layers.iter().chain([&config]) {
        fake.put_blob("lib/base", b);
    }
    let src = OciImageSource::with_auth(auth_file(&fake.host())).unwrap();
    let from = format!("{}/lib/base", fake.host());
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), Some(&from))
        .unwrap();
    assert_eq!(t.mount_from.as_deref(), Some(from.as_str()));
    let got = push_all(&src, &t).await.unwrap();
    assert_eq!(got, vec![BlobPush::Mounted(from.clone()); 3]);
    let log = fake.log();
    assert!(
        log.iter()
            .any(|l| l.contains("mount=sha256%3A") && l.contains("from=lib%2Fbase")),
        "{log:?}"
    );
    assert_eq!(count(&log, "PUT /v2/team/app/blobs"), 0);
    // mount 요청의 토큰은 원래 저장소의 pull을 더해 묻는다.
    let tokens = fake.store.lock().unwrap().tokens.clone();
    assert!(
        tokens
            .iter()
            .any(|(s, _)| s == &["repository:team/app:pull,push", "repository:lib/base:pull"]),
        "{tokens:?}"
    );
    // 다른 레지스트리나 같은 저장소에서 받은 것은 mount하지 않는다.
    let t = src
        .target(&format!("{}/lib/base:1", fake.host()), Some(&from))
        .unwrap();
    assert_eq!(t.mount_from, None);
    let t = src
        .target(
            &format!("{}/team/app:1", fake.host()),
            Some("docker.io/lib/base"),
        )
        .unwrap();
    assert_eq!(t.mount_from, None);
}

#[tokio::test]
async fn a_refused_mount_falls_back_to_uploading_in_the_session_it_opened() {
    let fake = serve(Knobs::default());
    let (layers, config, _) = image();
    for b in layers.iter().chain([&config]) {
        fake.put_blob("lib/base", b);
    }
    let src = OciImageSource::new().unwrap();
    let from = format!("{}/lib/base", fake.host());
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), Some(&from))
        .unwrap();
    assert_eq!(
        push_all(&src, &t).await.unwrap(),
        vec![BlobPush::Uploaded; 3]
    );
    let log = fake.log();
    // 첫 blob만 mount를 묻고, 레지스트리가 연 세션(1)에 그대로 올린다. 그 뒤에는 묻지 않는다.
    assert_eq!(count(&log, "POST /v2/team/app/blobs/uploads/?mount="), 1);
    assert_eq!(count(&log, "POST /v2/team/app/blobs/uploads/"), 3);
    assert!(
        log.iter()
            .any(|l| l.starts_with("PUT /v2/team/app/blobs/uploads/1?_state=s1-0&digest=")),
        "{log:?}"
    );
    assert!(fake.manifest("team/app", "1").is_some());
}

#[tokio::test]
async fn auth_challenges_get_a_push_token_with_the_stored_credentials() {
    let fake = serve(Knobs {
        bearer: true,
        ..Knobs::default()
    });
    let src = OciImageSource::with_auth(auth_file(&fake.host())).unwrap();
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), None)
        .unwrap();
    push_all(&src, &t).await.unwrap();
    let tokens = fake.store.lock().unwrap().tokens.clone();
    // 챌린지는 pull만 말해도 push scope로 묻는다. 한 번 받은 토큰은 다시 쓴다.
    assert_eq!(tokens.len(), 1, "{tokens:?}");
    assert_eq!(tokens[0].0, ["repository:team/app:pull,push"]);
    assert_eq!(
        tokens[0].1.as_deref(),
        Some(format!("Basic {}", crate::auth::base64_encode(b"me:pw")).as_str())
    );
    assert!(fake.manifest("team/app", "1").is_some());

    // 자격 증명이 없으면 토큰에 push가 없다: login을 권한다.
    let src = OciImageSource::new().unwrap();
    let t = src
        .target(&format!("{}/team/other:1", fake.host()), None)
        .unwrap();
    let e = push_all(&src, &t).await.unwrap_err().to_string();
    assert!(
        e.contains("push access denied") && e.contains("sbx login"),
        "{e}"
    );

    // Basic만 받는 레지스트리: 있으면 싣고, 없으면 login을 권한다. 비밀은 문구에 나오지 않는다.
    let basic = serve(Knobs {
        basic: true,
        ..Knobs::default()
    });
    let src = OciImageSource::with_auth(auth_file(&basic.host())).unwrap();
    let t = src
        .target(&format!("{}/team/app:1", basic.host()), None)
        .unwrap();
    push_all(&src, &t).await.unwrap();
    assert!(basic.manifest("team/app", "1").is_some());
    let src = OciImageSource::new().unwrap();
    let t = src
        .target(&format!("{}/team/app:2", basic.host()), None)
        .unwrap();
    let e = push_all(&src, &t).await.unwrap_err().to_string();
    assert!(
        e.contains("push access denied") && e.contains("sbx login"),
        "{e}"
    );
    assert!(!e.contains("bWU6cHc") && !e.contains(":pw"), "{e}");
}

#[tokio::test]
async fn a_wrong_digest_is_rejected_and_the_session_removed() {
    let fake = serve(Knobs::default());
    let src = OciImageSource::new().unwrap();
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), None)
        .unwrap();
    let claimed = sha256(b"what the manifest says");
    let e = src
        .push_blob(&t, &claimed, &b"something else".to_vec(), &|_| {})
        .await
        .unwrap_err();
    assert!(matches!(e, ImageError::Integrity(_)), "{e}");
    let shown = e.to_string();
    assert!(shown.contains("DIGEST_INVALID"), "{shown}");
    assert!(!shown.contains("_state"), "{shown}");
    assert!(!fake.has_blob("team/app", &claimed));
    let log = fake.log();
    assert!(
        log.last()
            .unwrap()
            .starts_with("DELETE /v2/team/app/blobs/uploads/1"),
        "{log:?}"
    );
    assert!(fake.store.lock().unwrap().uploads.is_empty());
}

#[tokio::test]
async fn blocked_registries_and_digest_references_are_refused_before_any_request() {
    let fake = serve(Knobs::default());
    let dir = std::env::temp_dir().join(format!("sbx-push-blocked-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let conf = dir.join("registries.conf");
    std::fs::write(
        &conf,
        format!(
            "[[registry]]\nlocation = \"{h}\"\nblocked = true\n\
             [[registry]]\nprefix = \"example.invalid\"\nlocation = \"{h}/elsewhere\"\n\
             [[registry.mirror]]\nlocation = \"{h}/mirror\"\n",
            h = fake.host()
        ),
    )
    .unwrap();
    let src = OciImageSource::new()
        .unwrap()
        .with_registries(crate::registries::ConfFiles {
            main: Some(conf),
            dropin_dirs: vec![],
        });
    let e = src
        .target(&format!("{}/team/app:1", fake.host()), None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("blocked") && e.contains("won't push"), "{e}");
    let e = src
        .target(&format!("example.invalid/a@{}", sha256(b"x")), None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("needs a tag"), "{e}");
    // 바꾸기와 미러는 push에 쓰지 않는다: 이름 그대로가 올릴 곳이다.
    let t = src.target("example.invalid/a:1", None).unwrap();
    assert_eq!(t.reference, "example.invalid/a:1");
    assert!(fake.log().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn an_interrupted_upload_leaves_no_blob_and_no_tag() {
    let (layers, config, _) = image();
    let fake = serve(Knobs {
        drop_put_of: Some(layers[1].len()),
        ..Knobs::default()
    });
    let src = OciImageSource::new().unwrap();
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), None)
        .unwrap();
    let e = push_all(&src, &t).await.unwrap_err();
    assert!(matches!(e, ImageError::Transport(_)), "{e}");
    assert!(fake.has_blob("team/app", &sha256(&layers[0])));
    assert!(!fake.has_blob("team/app", &sha256(&layers[1])));
    assert!(!fake.has_blob("team/app", &sha256(&config)));
    assert!(fake.manifest("team/app", "1").is_none());
    let log = fake.log();
    assert_eq!(count(&log, "PUT /v2/team/app/manifests"), 0);
    assert!(log.last().unwrap().starts_with("DELETE"), "{log:?}");
}

#[tokio::test]
async fn too_large_for_one_request_goes_in_chunks_that_follow_the_range() {
    let fake = serve(Knobs {
        max_body: Some(100),
        short_first_patch: true,
        ..Knobs::default()
    });
    let mut src = OciImageSource::new().unwrap();
    src.chunk = Some(300);
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), None)
        .unwrap();
    let blob: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    let d = sha256(&blob);
    let done = Mutex::new(Vec::new());
    let r = src
        .push_blob(&t, &d, &blob, &|n| done.lock().unwrap().push(n))
        .await
        .unwrap();
    assert_eq!(r, BlobPush::Uploaded);
    assert!(fake.has_blob("team/app", &d));
    let log = fake.log();
    let patches: Vec<&String> = log.iter().filter(|l| l.starts_with("PATCH")).collect();
    // 첫 조각은 절반(150)만 받았다: 150부터 다시 보낸다. 그 뒤 300씩, 마지막은 남은 만큼.
    let ranges: Vec<&str> = patches
        .iter()
        .map(|l| l.rsplit_once('[').unwrap().1.trim_end_matches(']'))
        .collect();
    assert_eq!(ranges, ["0-299", "150-449", "450-749", "750-999"]);
    // 조각마다 새 Location(`_state`)으로 간다. 413을 받은 첫 세션은 지웠다.
    assert!(patches[1].contains("_state=s2-150"), "{patches:?}");
    assert!(
        log.iter()
            .any(|l| l.starts_with("DELETE /v2/team/app/blobs/uploads/1"))
    );
    assert!(log.last().unwrap().contains("_state=s2-1000&digest="));
    let done = done.lock().unwrap();
    assert_eq!(done.last(), Some(&1000));
    assert!(done.iter().all(|n| *n <= 1000), "{done:?}");
}

#[tokio::test]
async fn an_upload_location_on_another_host_gets_no_credentials_and_must_be_public() {
    // 다른 곳(로컬 다른 포트)을 가리키는 Location은 사설·로컬 주소라 따르지 않는다.
    let other = serve(Knobs::default());
    let fake = serve(Knobs {
        absolute_other: Some(format!("localhost:{}", other.port)),
        ..Knobs::default()
    });
    let src = OciImageSource::new().unwrap();
    let t = src
        .target(&format!("{}/team/app:1", fake.host()), None)
        .unwrap();
    let e = push_all(&src, &t).await.unwrap_err().to_string();
    assert!(
        e.contains("no public address") || e.contains("private or local"),
        "{e}"
    );
    assert!(other.log().is_empty(), "{:?}", other.log());
}
