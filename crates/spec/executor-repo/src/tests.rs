use std::fs;
use std::io::Write;
use std::path::PathBuf;

use sbx_executor_install::{Spec, executable_name};
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;

use super::*;

const SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn base() -> Url {
    Url::parse("https://example.com/repo/index.json").unwrap()
}

fn mac() -> Platform {
    Platform::from_rust("macos", "aarch64")
}

fn artifact(os: &str, arch: &str, url: &str) -> serde_json::Value {
    json!({"os": os, "architecture": arch, "url": url, "sha256": SHA, "size": 1})
}

fn sample() -> serde_json::Value {
    json!({
        "schema": 1,
        "future_field": true,
        "executors": {
            "fc": {
                "description": "d",
                "versions": {
                    "1.9": {"protocol": 1, "artifacts": [artifact("darwin", "arm64", "fc/1.9.tar.gz")]},
                    "1.10": {"protocol": 1, "artifacts": [
                        artifact("darwin", "arm64", "https://cdn.example.org/fc-1.10.tar.gz"),
                        artifact("linux", "amd64", "../other/fc.tar.gz"),
                    ]},
                    "2.0": {"protocol": 2, "artifacts": [artifact("darwin", "arm64", "fc/2.0.tar.gz")]},
                    "3.0": {"protocol": 1, "artifacts": [artifact("linux", "arm64", "fc/3.0.tar.gz")]},
                }
            }
        }
    })
}

fn parse(v: serde_json::Value) -> Result<Index, String> {
    Index::parse(v.to_string().as_bytes(), &base())
}

fn spec(s: &str) -> Spec {
    s.parse().unwrap()
}

#[test]
fn index_is_parsed_and_urls_resolved() {
    let i = parse(sample()).unwrap();
    let fc = i.executor("fc").unwrap();
    let versions: Vec<_> = fc.versions.iter().map(|v| v.version.as_str()).collect();
    assert_eq!(versions, ["1.9", "1.10", "2.0", "3.0"]);
    let v = &fc.versions[1];
    assert_eq!(
        v.artifact(&mac()).unwrap().url.as_str(),
        "https://cdn.example.org/fc-1.10.tar.gz"
    );
    assert_eq!(
        v.artifact(&Platform::from_rust("linux", "x86_64"))
            .unwrap()
            .url
            .as_str(),
        "https://example.com/other/fc.tar.gz"
    );
    assert_eq!(
        fc.versions[0].artifacts[0].url.as_str(),
        "https://example.com/repo/fc/1.9.tar.gz"
    );
}

#[test]
fn bad_indexes_are_refused() {
    let with = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = sample();
        f(&mut v);
        parse(v).unwrap_err()
    };
    assert!(with(&|v| v["schema"] = json!(3)).contains("newer sbx"));
    assert!(parse(json!({"executors": {}})).is_err());
    assert!(
        with(&|v| v["executors"]["fc"]["versions"]["1.9"]["artifacts"][0]["sha256"] = json!("ABC"))
            .contains("sha256")
    );
    assert!(
        with(
            &|v| v["executors"]["fc"]["versions"]["1.9"]["artifacts"][0]["url"] =
                json!("http://example.com/x.tar.gz")
        )
        .contains("http")
    );
    assert!(
        with(&|v| v["executors"]["fc"]["versions"]["1.9"]["artifacts"]
            .as_array_mut()
            .unwrap()
            .push(artifact("darwin", "arm64", "dup.tar.gz")))
        .contains("two artifacts")
    );
    assert!(
        with(&|v| v["executors"]["../x"] = json!({"versions": {"1": {"protocol": 1}}}))
            .contains("invalid")
    );
    assert!(Index::parse(b"not json", &base()).is_err());
}

fn sample_v2() -> serde_json::Value {
    json!({
        "schema": 2,
        "executors": {
            "wick": {
                "description": "Firecracker builds",
                "versions": {
                    "1.18.0": {"adapter": "firecracker", "artifacts": [artifact("darwin", "arm64", "wick/1.18.0.tar.gz")]},
                    "1.19.0": {"adapter": "firecracker", "artifacts": [artifact("darwin", "arm64", "wick/1.19.0.tar.gz")]},
                }
            },
            "solo": {"versions": {"1": {"protocol": 2, "artifacts": [artifact("darwin", "arm64", "solo.tar.gz")]}}}
        },
        "adapters": {
            "firecracker": {
                "versions": {
                    "0.1.0": {"protocol": 2, "artifacts": [artifact("darwin", "arm64", "a/0.1.0.tar.gz")]},
                    "0.2.0": {"protocol": 3, "artifacts": [artifact("darwin", "arm64", "a/0.2.0.tar.gz")]},
                    "0.0.9": {"protocol": 2, "artifacts": [artifact("linux", "amd64", "a/0.0.9.tar.gz")]},
                }
            }
        }
    })
}

#[test]
fn schema_2_has_adapters_and_vmm_packages() {
    let i = parse(sample_v2()).unwrap();
    let wick = i.executor("wick").unwrap();
    assert_eq!(wick.versions[1].adapter.as_deref(), Some("firecracker"));
    assert_eq!(wick.versions[1].protocol, None);
    assert_eq!(i.adapter("firecracker").unwrap().versions.len(), 3);
    let idx = vec![("r".to_owned(), i)];
    // VMM 묶음은 규약 버전과 상관없이, 자체 완결 실행기는 맞는 규약만.
    let names: Vec<String> = index::available(&idx, None, &mac(), 2)
        .into_iter()
        .map(|c| format!("{}@{}", c.name, c.version))
        .collect();
    assert_eq!(names, ["solo@1", "wick@1.19.0", "wick@1.18.0"]);
    assert!(index::available(&idx, Some("solo"), &mac(), 9).is_empty());
    assert_eq!(index::available(&idx, Some("wick"), &mac(), 9).len(), 2);
    // 어댑터는 이 플랫폼과 규약에서 가장 높은 것.
    assert_eq!(
        index::pick_adapter(&idx, &spec("firecracker"), &mac(), 2)
            .unwrap()
            .version,
        "0.1.0"
    );
    assert_eq!(
        index::pick_adapter(&idx, &spec("firecracker"), &mac(), 3)
            .unwrap()
            .version,
        "0.2.0"
    );
    assert!(
        index::pick_adapter(&idx, &spec("firecracker"), &mac(), 7)
            .unwrap_err()
            .contains("this sbx speaks 7")
    );
    assert!(
        index::pick_adapter(
            &idx,
            &spec("firecracker"),
            &Platform::from_rust("linux", "aarch64"),
            2
        )
        .unwrap_err()
        .contains("no build")
    );
    assert!(
        index::pick_adapter(&idx, &spec("nope"), &mac(), 2)
            .unwrap_err()
            .contains("no adapter")
    );
}

#[test]
fn schema_2_versions_are_checked() {
    let with = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = sample_v2();
        f(&mut v);
        parse(v).unwrap_err()
    };
    assert!(
        with(&|v| v["executors"]["wick"]["versions"]["1.18.0"]["protocol"] = json!(2))
            .contains("exactly one")
    );
    assert!(
        with(&|v| {
            v["executors"]["wick"]["versions"]["1.18.0"]
                .as_object_mut()
                .unwrap()
                .remove("adapter");
        })
        .contains("exactly one")
    );
    assert!(
        with(&|v| v["executors"]["wick"]["versions"]["1.18.0"]["adapter"] = json!("x@1"))
            .contains("invalid adapter")
    );
    assert!(
        with(&|v| v["adapters"]["firecracker"]["versions"]["0.1.0"]["adapter"] = json!("y"))
            .contains("adapter has no")
    );
    assert!(
        with(&|v| {
            v["adapters"]["firecracker"]["versions"]["0.1.0"]
                .as_object_mut()
                .unwrap()
                .remove("protocol");
        })
        .contains("missing")
    );
    assert!(
        with(&|v| v["adapters"]["../x"] = json!({"versions": {"1": {"protocol": 2}}}))
            .contains("invalid adapter name")
    );
}

#[test]
fn schema_1_ignores_schema_2_fields() {
    let mut v = sample();
    v["adapters"] = json!({"firecracker": {"versions": {"1": {}}}});
    v["executors"]["fc"]["versions"]["1.9"]["adapter"] = json!("firecracker");
    let i = parse(v).unwrap();
    assert!(i.adapters.is_empty());
    assert_eq!(i.executor("fc").unwrap().versions[0].adapter, None);
}

#[test]
fn picking_follows_platform_protocol_version_and_repository_order() {
    let a = parse(sample()).unwrap();
    let mut b = sample();
    b["executors"]["fc"]["versions"]["1.10"]["artifacts"][0]["url"] = json!("https://b.example/x");
    b["executors"]["fc"]["versions"]["1.11"] = json!({"protocol": 1, "artifacts": []});
    let b = parse(b).unwrap();
    let idx = vec![("a".to_owned(), a), ("b".to_owned(), b)];

    // 가장 높은 버전 중 이 플랫폼과 규약 버전 1로 되는 것: 1.10 (2.0은 규약 2, 3.0은 linux/arm64뿐).
    let c = index::pick(&idx, &spec("fc"), &mac(), 1).unwrap();
    assert_eq!((c.repository.as_str(), c.version.as_str()), ("a", "1.10"));
    assert_eq!(c.spec(), spec("fc@1.10"));
    let c = index::pick(&idx, &spec("fc@1.9"), &mac(), 1).unwrap();
    assert_eq!(c.version, "1.9");
    // 규약 2를 말하면 2.0.
    assert_eq!(
        index::pick(&idx, &spec("fc"), &mac(), 2).unwrap().version,
        "2.0"
    );

    // 이유.
    let e = index::pick(&idx, &spec("nope"), &mac(), 1).unwrap_err();
    assert!(e.contains("no executor named"), "{e}");
    let e = index::pick(&idx, &spec("fc@9"), &mac(), 1).unwrap_err();
    assert!(e.contains("not in any repository"), "{e}");
    let e = index::pick(&idx, &spec("fc@3.0"), &mac(), 1).unwrap_err();
    assert!(
        e.contains("no build for darwin/arm64 (available: linux/arm64)"),
        "{e}"
    );
    let e = index::pick(&idx, &spec("fc@2.0"), &mac(), 1).unwrap_err();
    assert!(
        e.contains("speaks executor protocol 2; this sbx speaks 1"),
        "{e}"
    );

    let all: Vec<_> = index::available(&idx, None, &mac(), 1)
        .into_iter()
        .map(|c| format!("{}:{}", c.repository, c.version))
        .collect();
    assert_eq!(all, ["a:1.10", "b:1.10", "a:1.9", "b:1.9"]);
}

struct Repo {
    root: PathBuf,
}

impl Repo {
    /// 실행 파일 하나를 담은 묶음을 `fc/1.0.tar.gz`로 두고, 그것을 가리키는 색인을 쓴다.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("sbx-exrepo-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("repo/fc")).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let body = b"#!/bin/sh\necho hi\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o755);
        b.append_data(&mut h, format!("fc-1.0/{}", executable_name()), &body[..])
            .unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&b.into_inner().unwrap()).unwrap();
        let archive = gz.finish().unwrap();
        fs::write(root.join("repo/fc/1.0.tar.gz"), &archive).unwrap();
        let sha: String = Sha256::digest(&archive)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let host = Platform::host();
        let index = json!({"schema": 1, "executors": {"fc": {"versions": {"1.0": {"protocol": 1, "artifacts": [
            {"os": host.os, "architecture": host.architecture, "url": "fc/1.0.tar.gz", "sha256": sha, "size": archive.len()}
        ]}}}}});
        fs::write(root.join("repo/index.json"), index.to_string()).unwrap();
        Self { root }
    }

    fn file_url(&self) -> String {
        Url::from_directory_path(self.root.join("repo"))
            .unwrap()
            .to_string()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

async fn install_from(repo: &Repo, url: &str) -> Result<PathBuf, RepoError> {
    let f = Fetcher::new()?;
    let r = Repository::new("t", url)?;
    let (idx, failed) = indexes(&f, std::slice::from_ref(&r)).await;
    if let Some(e) = failed.into_iter().next() {
        return Err(e);
    }
    let c = index::pick(&idx, &spec("fc"), &Platform::host(), 1).map_err(RepoError::NotFound)?;
    let work = repo
        .root
        .join(format!("work-{}", url.split(':').next().unwrap()));
    fetch_bundle(&f, &c, &work).await
}

#[tokio::test]
async fn download_and_unpack_from_a_file_repository() {
    let repo = Repo::new("file");
    let dir = install_from(&repo, &repo.file_url()).await.unwrap();
    assert!(dir.ends_with("bundle/fc-1.0"), "{}", dir.display());
    assert!(dir.join(executable_name()).is_file());
    assert!(
        !dir.parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("bundle.tar.gz")
            .exists()
    );

    // 내용이 바뀌면 (색인의 sha256과 다르면) 풀지 않고 받은 파일도 지운다.
    let ar = repo.root.join("repo/fc/1.0.tar.gz");
    let mut bytes = fs::read(&ar).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    fs::write(&ar, &bytes).unwrap();
    let work = repo.root.join("work-file");
    fs::remove_dir_all(&work).unwrap();
    let e = install_from(&repo, &repo.file_url()).await.unwrap_err();
    assert!(matches!(e, RepoError::Integrity { .. }), "{e}");
    assert!(!work.join("bundle.tar.gz").exists());
    assert!(!work.join("bundle").exists());
}

/// 파일을 그대로 돌려주는 아주 작은 HTTP/1.1 서버. 연결 하나에 요청 하나.
async fn serve(root: PathBuf) -> std::net::SocketAddr {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let root = root.clone();
            tokio::spawn(async move {
                let mut req = Vec::new();
                let mut buf = [0u8; 1024];
                while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = s.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                }
                let line = String::from_utf8_lossy(&req);
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let resp = match fs::read(root.join(path.trim_start_matches('/'))) {
                    Ok(body) => {
                        let mut r = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .into_bytes();
                        r.extend_from_slice(&body);
                        r
                    }
                    Err(_) => {
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_vec()
                    }
                };
                let _ = s.write_all(&resp).await;
            });
        }
    });
    addr
}

#[tokio::test]
async fn download_over_http_from_localhost() {
    let repo = Repo::new("http");
    let addr = serve(repo.root.join("repo")).await;
    let dir = install_from(&repo, &format!("http://{addr}/"))
        .await
        .unwrap();
    assert!(dir.join(executable_name()).is_file());

    // 없는 저장소는 받지 못한 것으로 알린다.
    let e = install_from(&repo, &format!("http://{addr}/missing/"))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, RepoError::Missing { url } if url.ends_with("/missing/index.json")),
        "{e}"
    );
}

#[tokio::test]
async fn v2_index_is_preferred_and_index_json_is_the_fallback() {
    let repo = Repo::new("v2");
    let f = Fetcher::new().unwrap();
    let r = Repository::new("t", &repo.file_url()).unwrap();
    // v2가 없으면 index.json (schema 1).
    let i = f.index(&r).await.unwrap();
    assert!(i.executor("fc").is_some());
    // v2가 있으면 그것만 읽는다. 상대 URL은 v2/index.json 기준이다.
    let v2 = repo.root.join("repo/v2");
    fs::create_dir_all(&v2).unwrap();
    fs::write(v2.join("index.json"), sample_v2().to_string()).unwrap();
    let i = f.index(&r).await.unwrap();
    assert!(i.executor("fc").is_none());
    let a = &i.adapter("firecracker").unwrap().versions[0].artifacts[0];
    assert!(
        a.url.path().ends_with("/repo/v2/a/0.0.9.tar.gz"),
        "{}",
        a.url
    );
    // v2가 깨졌으면 되돌아가지 않고 알린다.
    fs::write(v2.join("index.json"), b"{").unwrap();
    assert!(matches!(f.index(&r).await, Err(RepoError::Index { .. })));
}
