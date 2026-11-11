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
    // 이름이 잘못된 항목은 건너뛴다(entries_this_client_cannot_name_are_skipped_not_fatal). 그 안의 다른 잘못은 여전히
    // 색인을 거부한다.
    assert!(
        with(&|v| v["executors"]["fc"]["versions"]["1.9"]["artifacts"][0]["sha256"] = json!("x"))
            .contains("sha256")
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
fn a_main_version_picks_its_newest_patch() {
    let mut v = sample_v2();
    for p in ["1.18.0+wick.1", "1.18.0+wick.10", "1.18.0+wick.2"] {
        v["executors"]["wick"]["versions"][p] = json!({"adapter": "firecracker", "artifacts": [artifact("darwin", "arm64", "w.tar.gz")]});
    }
    let idx = vec![("r".to_owned(), parse(v).unwrap())];
    let pick = |s: &str| index::pick(&idx, &spec(s), &mac(), 2).map(|c| c.version);
    // 1.18.0 자체도 있지만 그 주 버전의 가장 새 패치를 고른다.
    assert_eq!(pick("wick@1.18.0").unwrap(), "1.18.0+wick.10");
    assert_eq!(pick("wick@1.18.0+wick").unwrap(), "1.18.0+wick.10");
    assert_eq!(pick("wick@1.18.0+wick.2").unwrap(), "1.18.0+wick.2");
    assert_eq!(pick("wick@1").unwrap(), "1.19.0");
    assert!(
        pick("wick@1.17")
            .unwrap_err()
            .contains("not in any repository")
    );
}

#[test]
fn protocol_ranges_overlap_with_the_client_range() {
    let mut v = sample_v2();
    // 0.2.0은 2부터 3까지 말한다.
    v["adapters"]["firecracker"]["versions"]["0.2.0"]["min_protocol"] = json!(2);
    let idx = vec![("r".to_owned(), parse(v).unwrap())];
    let a = spec("firecracker");
    // 2만 말하는 클라이언트도 0.2.0을 고른다(가장 높은 버전).
    assert_eq!(
        index::pick_adapter(&idx, &a, &mac(), 2).unwrap().version,
        "0.2.0"
    );
    assert_eq!(
        index::pick_adapter(&idx, &a, &mac(), 1..=3)
            .unwrap()
            .version,
        "0.2.0"
    );
    // 4부터 말하는 클라이언트는 고를 것이 없고, 범위를 알린다.
    let e = index::pick_adapter(&idx, &a, &mac(), 4..=5).unwrap_err();
    assert!(
        e.contains("speaks executor protocol 2, 2 to 3; this sbx speaks 4 to 5"),
        "{e}"
    );
    // 규칙에 어긋난 범위는 색인 전체를 거부한다.
    let mut bad = sample_v2();
    bad["adapters"]["firecracker"]["versions"]["0.2.0"]["min_protocol"] = json!(4);
    assert!(parse(bad).unwrap_err().contains("above"));
    let mut bad = sample_v2();
    bad["executors"]["wick"]["versions"]["1.18.0"]["min_protocol"] = json!(1);
    assert!(parse(bad).unwrap_err().contains("needs \"protocol\""));
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
    // 이름이 잘못된 어댑터는 건너뛴다.
    let mut v = sample_v2();
    v["adapters"]["../x"] = json!({"versions": {"1": {"protocol": 2}}});
    assert!(parse(v).unwrap().adapter("../x").is_none());
}

/// 퍼징 대상 executor_index를 만들며 찾았다(대상의 불변식이 손으로 넣은 입력에서 걸렸다): 색인이 버전
/// `latest`를 내면 받아들였다. 그 버전은 설치되지만 `x@latest`가 "가장 높은 것"으로 읽혀 이름으로 지울 수 없었다.
#[test]
fn a_version_named_latest_is_refused() {
    for kind in ["executors", "adapters"] {
        let mut v = sample_v2();
        let first = if kind == "executors" {
            "solo"
        } else {
            "firecracker"
        };
        v[kind][first]["versions"]["latest"] =
            json!({"protocol": 2, "artifacts": [artifact("darwin", "arm64", "l.tar.gz")]});
        // 그 버전만 빠지고 색인은 읽힌다(나머지 버전과 다른 이름은 그대로).
        let i = parse(v).unwrap();
        let e = if kind == "executors" {
            i.executor(first)
        } else {
            i.adapter(first)
        }
        .unwrap();
        assert!(e.versions.iter().all(|x| x.version != "latest"), "{kind}");
        assert!(!e.versions.is_empty(), "{kind}");
    }
}

#[test]
fn entries_this_client_cannot_name_are_skipped_not_fatal() {
    // 대문자, 경로, 끝의 점(Windows에서 `fake.`는 `fake`다): 그 항목만 빠지고 색인과 다른 항목은 쓸 수 있다.
    let mut v = sample();
    let good = v["executors"]["fc"].clone();
    for bad in ["Fc", "../x", "fc.", "a b"] {
        v["executors"][bad] = good.clone();
    }
    v["executors"]["fc"]["versions"]["2.0-RC1"] = v["executors"]["fc"]["versions"]["1.9"].clone();
    v["executors"]["fc"]["versions"]["3.0."] = v["executors"]["fc"]["versions"]["1.9"].clone();
    let i = parse(v).unwrap();
    let names: Vec<&str> = i.executors.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["fc"]);
    let versions: Vec<&str> = i.executors[0]
        .versions
        .iter()
        .map(|x| x.version.as_str())
        .collect();
    assert!(
        !versions.contains(&"2.0-RC1") && !versions.contains(&"3.0."),
        "{versions:?}"
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

#[test]
fn a_name_belongs_to_the_first_repository_that_has_it() {
    let a = parse(sample()).unwrap();
    // 뒤 저장소가 같은 이름에 더 높은 버전을 내도 가로채지 못한다.
    let mut b = sample();
    b["executors"]["fc"]["versions"]["9.0"] = json!({"protocol": 1, "artifacts": [artifact("darwin", "arm64", "https://evil.example/fc")]});
    b["executors"]["only-b"] = json!({"versions": {"1.0": {"protocol": 1, "artifacts": [artifact("darwin", "arm64", "x")]}}});
    let b = parse(b).unwrap();
    let idx = vec![("a".to_owned(), a), ("b".to_owned(), b)];
    let c = index::pick(&idx, &spec("fc"), &mac(), 1).unwrap();
    assert_eq!((c.repository.as_str(), c.version.as_str()), ("a", "1.10"));
    let e = index::pick(&idx, &spec("fc@9.0"), &mac(), 1).unwrap_err();
    assert!(e.contains("not in any repository"), "{e}");
    // 그 저장소만 넘기면 그 저장소에서 고른다. 다른 이름은 그 이름을 내는 첫 저장소에서.
    let c = index::pick(&idx[1..], &spec("fc"), &mac(), 1).unwrap();
    assert_eq!((c.repository.as_str(), c.version.as_str()), ("b", "9.0"));
    let c = index::pick(&idx, &spec("only-b"), &mac(), 1).unwrap();
    assert_eq!(c.repository, "b");
}

/// 서명된 v2 저장소: 색인, 서명, 공개키.
struct Signed {
    repo: Repo,
    sk: minisign::SecretKey,
    key: String,
}

impl Signed {
    fn new(name: &str) -> Self {
        let repo = Repo::new(name);
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let s = Self {
            repo,
            key: kp.pk.to_base64(),
            sk: kp.sk,
        };
        fs::create_dir_all(s.dir()).unwrap();
        s
    }

    fn dir(&self) -> PathBuf {
        self.repo.root.join("repo/v2")
    }

    fn index(serial: Option<u64>, expires: Option<u64>) -> Vec<u8> {
        let mut v = sample_v2();
        if let Some(s) = serial {
            v["serial"] = json!(s);
        }
        if let Some(e) = expires {
            v["expires"] = json!(e);
        }
        v.to_string().into_bytes()
    }

    /// 색인을 쓰고 이 키로 서명한다.
    fn publish(&self, body: &[u8]) {
        fs::write(self.dir().join("index.json"), body).unwrap();
        let sig = minisign::sign(None, &self.sk, body, Some("test"), None).unwrap();
        fs::write(self.dir().join("index.json.minisig"), sig.into_string()).unwrap();
    }

    fn repository(&self, keys: Vec<String>) -> Repository {
        Repository::new("s", &self.repo.file_url())
            .unwrap()
            .with_keys(keys)
            .unwrap()
    }
}

fn untrusted(r: Result<Index, RepoError>, why: &str) {
    match r {
        Err(RepoError::Untrusted { reason, .. }) => assert!(reason.contains(why), "{reason}"),
        other => panic!("expected untrusted ({why}), got {other:?}"),
    }
}

#[tokio::test]
async fn signed_repositories_need_a_matching_fresh_signature() {
    let s = Signed::new("signed");
    let later = trust::now() + 3600;
    let seen = Seen::new(s.repo.root.join("seen"));
    let f = Fetcher::new().unwrap().with_seen(seen.clone());
    let r = s.repository(vec![s.key.clone()]);

    // 서명이 맞으면 읽고 일련번호를 적는다.
    s.publish(&Signed::index(Some(5), Some(later)));
    let i = f.index(&r).await.unwrap();
    assert_eq!((i.serial, i.expires), (Some(5), Some(later)));
    assert_eq!(seen.get(&r).unwrap(), Some(5));

    // 서명 파일이 없으면, 색인을 바꾸면, 키가 다르면 거부한다.
    fs::remove_file(s.dir().join("index.json.minisig")).unwrap();
    untrusted(f.index(&r).await, "no signature");
    s.publish(&Signed::index(Some(5), Some(later)));
    let mut body = fs::read(s.dir().join("index.json")).unwrap();
    body.push(b' ');
    fs::write(s.dir().join("index.json"), &body).unwrap();
    untrusted(f.index(&r).await, "matches none");
    s.publish(&Signed::index(Some(5), Some(later)));
    let other = minisign::KeyPair::generate_unencrypted_keypair()
        .unwrap()
        .pk
        .to_base64();
    untrusted(
        f.index(&s.repository(vec![other.clone()])).await,
        "matches none",
    );
    // 키 둘 중 하나만 맞아도 된다(키를 바꾸는 동안).
    f.index(&s.repository(vec![other, s.key.clone()]))
        .await
        .unwrap();

    // serial·expires가 없으면, 기한이 지났으면, 본 것보다 오래됐으면 거부한다.
    s.publish(&Signed::index(None, Some(later)));
    untrusted(f.index(&r).await, "needs serial and expires");
    s.publish(&Signed::index(Some(6), Some(trust::now() - 1)));
    untrusted(f.index(&r).await, "expired");
    s.publish(&Signed::index(Some(4), Some(later)));
    untrusted(f.index(&r).await, "older than 5");
    assert_eq!(seen.get(&r).unwrap(), Some(5));
    s.publish(&Signed::index(Some(7), Some(later)));
    f.index(&r).await.unwrap();
    assert_eq!(seen.get(&r).unwrap(), Some(7));

    // 서명된 저장소는 v1로 되돌아가지 않는다.
    fs::remove_dir_all(s.dir()).unwrap();
    untrusted(
        f.index(&r).await,
        "v2/index.json not found (a signed repository is read only from there",
    );
}

/// 서명 파일만 바꿔 쓴다.
fn write_signature(s: &Signed, text: impl AsRef<[u8]>) {
    fs::write(s.dir().join("index.json.minisig"), text).unwrap();
}

#[tokio::test]
async fn hostile_signature_files_are_refused_and_never_raise_the_seen_serial() {
    let s = Signed::new("hostile-sig");
    let later = trust::now() + 3600;
    let seen = Seen::new(s.repo.root.join("seen"));
    let f = Fetcher::new().unwrap().with_seen(seen.clone());
    let r = s.repository(vec![s.key.clone()]);
    s.publish(&Signed::index(Some(10), Some(later)));
    f.index(&r).await.unwrap();

    // 같은 키로 서명된 다른 색인의 서명을 붙여도 맞지 않는다(서명은 본문 전체에 묶인다).
    let old = Signed::index(Some(3), Some(later));
    let old_sig = minisign::sign(None, &s.sk, &old[..], Some("test"), None)
        .unwrap()
        .into_string();
    fs::write(
        s.dir().join("index.json"),
        Signed::index(Some(99), Some(later)),
    )
    .unwrap();
    write_signature(&s, &old_sig);
    untrusted(f.index(&r).await, "matches none");
    // 서명이 맞지 않은 색인의 serial은 적지 않는다(누구나 큰 serial을 내밀어 진짜 색인을 막을 수 없게).
    assert_eq!(seen.get(&r).unwrap(), Some(10));

    // 서명은 맞지만 오래된 색인을 통째로 다시 내밀면 거부한다(롤백).
    fs::write(s.dir().join("index.json"), &old).unwrap();
    untrusted(f.index(&r).await, "older than 10");

    // 기한이 지난 색인은 serial이 더 커도 적지 않는다.
    s.publish(&Signed::index(Some(50), Some(trust::now() - 1)));
    untrusted(f.index(&r).await, "expired");
    assert_eq!(seen.get(&r).unwrap(), Some(10));

    // 같은 serial을 다시 받는 것은 괜찮다(같은 색인을 다시 읽는다).
    s.publish(&Signed::index(Some(10), Some(later)));
    f.index(&r).await.unwrap();

    // 읽을 수 없는 서명, 텍스트가 아닌 서명, 너무 큰 서명.
    write_signature(&s, "not a minisign signature");
    untrusted(f.index(&r).await, "unreadable signature");
    write_signature(&s, [0xff, 0xfe, 0x00, 0x01]);
    untrusted(f.index(&r).await, "not text");
    write_signature(&s, vec![b'a'; trust::MAX_SIGNATURE_BYTES as usize + 1]);
    match f.index(&r).await {
        Err(RepoError::Fetch { reason, .. }) => assert!(reason.contains("larger"), "{reason}"),
        other => panic!("expected a size error, got {other:?}"),
    }
    assert_eq!(seen.get(&r).unwrap(), Some(10));
}

#[tokio::test]
async fn a_signed_schema_1_body_cannot_shed_its_serial() {
    // schema 1에는 serial·expires가 없다. 서명된 저장소의 v2 자리에 schema 1을 두면(서명이 맞아도) 거부한다.
    let s = Signed::new("schema1-signed");
    let f = Fetcher::new().unwrap();
    let r = s.repository(vec![s.key.clone()]);
    let mut v = sample();
    v["serial"] = json!(5);
    v["expires"] = json!(trust::now() + 3600);
    s.publish(v.to_string().as_bytes());
    untrusted(f.index(&r).await, "needs serial and expires");
}

#[test]
fn serial_and_expires_must_be_plain_numbers_once() {
    for bad in [
        json!({"schema": 2, "serial": "5", "executors": {}}),
        json!({"schema": 2, "serial": -1, "executors": {}}),
        json!({"schema": 2, "serial": 1.5, "executors": {}}),
        json!({"schema": 2, "expires": "never", "executors": {}}),
    ] {
        assert!(
            Index::parse(bad.to_string().as_bytes(), &base()).is_err(),
            "{bad}"
        );
    }
    // u64를 넘는 수.
    let big = br#"{"schema": 2, "serial": 18446744073709551616, "executors": {}}"#;
    assert!(Index::parse(big, &base()).is_err());
    // 같은 키가 두 번이면 거부한다(읽는 쪽마다 다른 값을 고를 수 없게).
    let dup = br#"{"schema": 2, "serial": 1, "serial": 999, "executors": {}}"#;
    assert!(Index::parse(dup, &base()).is_err());
    let i = Index::parse(
        br#"{"schema": 2, "serial": 7, "expires": 9, "executors": {}}"#,
        &base(),
    )
    .unwrap();
    assert_eq!((i.serial, i.expires), (Some(7), Some(9)));
}

#[test]
fn a_remote_index_cannot_point_at_this_machine() {
    let at = |index: &str, artifact: &str| {
        let mut v = sample();
        v["executors"]["fc"]["versions"]["1.9"]["artifacts"][0]["url"] = json!(artifact);
        Index::parse(v.to_string().as_bytes(), &Url::parse(index).unwrap())
    };
    let https = "https://example.com/repo/index.json";
    // https 색인: https 압축 파일만. 이 기계의 파일이나 loopback 서비스는 안 된다.
    assert!(at(https, "https://cdn.example.net/x.tar.gz").is_ok());
    assert!(at(https, "x.tar.gz").is_ok());
    for bad in [
        "file:///etc/passwd",
        "file:///dev/zero",
        "http://127.0.0.1:8080/admin",
        "http://localhost/x.tar.gz",
        "http://[::1]/x",
        "ftp://example.com/x",
        "http://example.com/x",
    ] {
        let e = at(https, bad).unwrap_err();
        assert!(e.contains(bad.split(':').next().unwrap()), "{bad}: {e}");
        // ftp와 밖의 평문 http는 URL 검사부터 받지 않는다. 나머지(이 기계를 가리키는 것)는 이 규칙에 걸린다.
        if !bad.starts_with("ftp") && !bad.contains("example.com") {
            assert!(
                e.contains("an index at a https: URL may point only to https: archives"),
                "{bad}: {e}"
            );
        }
    }
    // file 색인: file과 https. loopback http 색인: 그 http와 https.
    let file = "file:///srv/repo/index.json";
    assert!(at(file, "x.tar.gz").is_ok() && at(file, "https://example.com/x").is_ok());
    assert!(at(file, "http://127.0.0.1/x").is_err());
    let local = "http://127.0.0.1:9000/repo/index.json";
    assert!(at(local, "x.tar.gz").is_ok() && at(local, "https://example.com/x").is_ok());
    let e = at(local, "file:///etc/passwd").unwrap_err();
    assert!(
        e.contains("an index at a http: URL may point only to http: or https: archives"),
        "{e}"
    );
}

#[test]
fn the_repository_list_round_trips_and_keeps_removals() {
    let root = std::env::temp_dir().join(format!("sbx-exrepo-list-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let official = Repository::new("official", "https://example.com/o").unwrap();
    let src = Sources::new(root.join("repos")).with_defaults(vec![official.clone()]);
    // 파일이 없으면 기본값. 지우면 그 상태가 남는다(기본값이 되살아나지 않는다).
    assert_eq!(src.list().unwrap(), std::slice::from_ref(&official));
    src.remove("official").unwrap();
    assert!(src.list().unwrap().is_empty());
    assert!(matches!(
        src.remove("official"),
        Err(RepoError::NotFound(_))
    ));
    // 줄을 깨뜨릴 수 있는 URL(줄바꿈, 공백, #)도 한 줄로 저장되고 그대로 읽힌다.
    let tricky = Repository::new("t", "https://example.com/a b\nevil https://x/").unwrap();
    src.add(tricky.clone()).unwrap();
    src.add(Repository::new("l", "http://127.0.0.1:1/r").unwrap())
        .unwrap();
    assert_eq!(src.list().unwrap().len(), 2);
    assert_eq!(src.list().unwrap()[0], tricky);
    assert!(
        src.add(Repository::new("t", "https://example.com/other").unwrap())
            .is_err()
    );
    // 이름과 URL 규칙.
    for bad in ["", ".x", "a b", "a/b", "é", "a\nb"] {
        assert!(
            Repository::new(bad, "https://example.com/").is_err(),
            "{bad:?}"
        );
    }
    for bad in [
        "http://example.com/r",
        "ftp://example.com/r",
        "https://example.com/r?x=1",
        "https://example.com/r#f",
        "not a url",
    ] {
        assert!(Repository::new("ok", bad).is_err(), "{bad}");
    }
    // 주석과 빈 줄은 건너뛰고, 같은 이름이 두 번이면 거부한다.
    fs::write(root.join("repos"), "# c\n\n  a https://example.com/a\n").unwrap();
    assert_eq!(src.list().unwrap().len(), 1);
    fs::write(
        root.join("repos"),
        "a https://example.com/a\na https://example.com/b\n",
    )
    .unwrap();
    assert!(src.list().unwrap_err().to_string().contains("twice"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn verify_needs_a_key_and_reports_bad_keys() {
    let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let sig = minisign::sign(None, &kp.sk, &b"body"[..], None, None)
        .unwrap()
        .into_string();
    let pk = kp.pk.to_base64();
    assert!(trust::verify(b"body", &sig, std::slice::from_ref(&pk)).is_ok());
    assert!(
        trust::verify(b"body", &sig, &[])
            .unwrap_err()
            .contains("no keys")
    );
    assert!(trust::verify(b"bodyx", &sig, std::slice::from_ref(&pk)).is_err());
    assert!(
        trust::verify(b"body", &sig, &["garbage".into()])
            .unwrap_err()
            .contains("invalid key")
    );
}

#[test]
fn the_seen_file_is_kept_per_name_and_url_and_survives_junk() {
    let root = std::env::temp_dir().join(format!("sbx-exrepo-seen-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let seen = Seen::new(root.join("dir/seen"));
    let a = Repository::new("a", "https://example.com/a").unwrap();
    let a_moved = Repository::new("a", "https://example.com/elsewhere").unwrap();
    let b = Repository::new("b", "https://example.com/b").unwrap();
    // 없는 파일(과 디렉터리)은 본 것이 없다.
    assert_eq!(seen.get(&a).unwrap(), None);
    seen.check_and_record(&a, 7, "u").unwrap();
    seen.check_and_record(&b, 3, "u").unwrap();
    // 같은 이름이라도 URL이 다르면 새로 센다. 옛 줄은 남는다(되돌아가면 다시 쓴다).
    assert_eq!(seen.get(&a_moved).unwrap(), None);
    seen.check_and_record(&a_moved, 1, "u").unwrap();
    assert_eq!(seen.get(&a).unwrap(), Some(7));
    assert!(seen.check_and_record(&a, 6, "u").is_err());
    // 손으로 고친 줄이나 깨진 줄은 건너뛴다. 다른 저장소의 기록은 지우지 않는다.
    let path = root.join("dir/seen");
    let mut text = fs::read_to_string(&path).unwrap();
    text.push_str("garbage\nb notanumber https://example.com/b\n\n");
    fs::write(&path, text).unwrap();
    seen.check_and_record(&a, 8, "u").unwrap();
    assert_eq!(
        (
            seen.get(&a).unwrap(),
            seen.get(&b).unwrap(),
            seen.get(&a_moved).unwrap()
        ),
        (Some(8), Some(3), Some(1))
    );
    // 임시 파일을 남기지 않는다(잠금 파일만 남는다).
    let mut names: Vec<_> = fs::read_dir(root.join("dir"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [".seen.lock", "seen"]);
    let _ = fs::remove_dir_all(&root);
}

/// 이미 본 serial이면 적을 것이 없으니 잠그지도 쓰지도 않는다. 읽기만 되는 데이터 디렉터리(다른 사용자가 만든
/// 것, 읽기 전용 마운트)에서도 이미 본 색인은 읽히고, 롤백은 그래도 거부된다.
#[cfg(unix)]
#[test]
fn a_read_only_data_directory_still_checks_serials_it_already_saw() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("sbx-exrepo-seen-ro-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let dir = root.join("dir");
    let seen = Seen::new(dir.join("seen"));
    let a = Repository::new("a", "https://example.com/a").unwrap();
    seen.check_and_record(&a, 5, "u").unwrap();
    fs::remove_file(dir.join(".seen.lock")).unwrap();
    let ro = |mode| fs::set_permissions(&dir, fs::Permissions::from_mode(mode)).unwrap();
    ro(0o555);
    // root는 권한을 무시한다. 그때는 이 검사가 뜻이 없다.
    if fs::write(dir.join("probe"), "").is_ok() {
        ro(0o755);
        let _ = fs::remove_dir_all(&root);
        return;
    }
    let same = seen.check_and_record(&a, 5, "u");
    let older = seen.check_and_record(&a, 4, "u");
    let newer = seen.check_and_record(&a, 6, "u");
    ro(0o755);
    same.unwrap();
    assert!(
        matches!(older, Err(RepoError::Untrusted { ref reason, .. }) if reason.contains("older than 5")),
        "{older:?}"
    );
    // 새 serial은 적어야 하므로 쓰지 못하면 실패한다(적지 못한 채 받아들이면 롤백 창이 열린다).
    assert!(newer.is_err(), "{newer:?}");
    assert_eq!(seen.get(&a).unwrap(), Some(5));
    assert!(!dir.join(".seen.lock").exists());
    let _ = fs::remove_dir_all(&root);
}

#[tokio::test]
async fn a_signed_repository_ignores_a_v1_index_and_unsigned_serials_are_not_tracked() {
    let s = Signed::new("v1-only");
    let later = trust::now() + 3600;
    let seen = Seen::new(s.repo.root.join("seen"));
    let f = Fetcher::new().unwrap().with_seen(seen.clone());
    // Repo::new가 둔 v1 index.json이 있어도 서명된 저장소는 그것을 읽지 않는다.
    assert!(s.repo.root.join("repo/index.json").is_file());
    untrusted(
        f.index(&s.repository(vec![s.key.clone()])).await,
        "v2/index.json not found (a signed repository is read only from there",
    );
    // 서명 없는 저장소의 serial은 믿을 근거가 없으므로 적지도 막지도 않는다.
    let u = Repository::new("u", &s.repo.file_url()).unwrap();
    fs::write(
        s.dir().join("index.json"),
        Signed::index(Some(9), Some(later)),
    )
    .unwrap();
    f.index(&u).await.unwrap();
    fs::write(
        s.dir().join("index.json"),
        Signed::index(Some(1), Some(later)),
    )
    .unwrap();
    f.index(&u).await.unwrap();
    assert_eq!(seen.get(&u).unwrap(), None);
}

#[tokio::test]
async fn unsigned_indexes_still_honor_their_own_expiry() {
    let s = Signed::new("unsigned-expiry");
    let f = Fetcher::new().unwrap();
    let r = Repository::new("u", &s.repo.file_url()).unwrap();
    fs::write(
        s.dir().join("index.json"),
        Signed::index(None, Some(trust::now() - 1)),
    )
    .unwrap();
    untrusted(f.index(&r).await, "expired");
    fs::write(s.dir().join("index.json"), Signed::index(Some(1), None)).unwrap();
    f.index(&r).await.unwrap();
}

#[test]
fn repository_keys_are_checked_and_kept_in_the_list_file() {
    let root = std::env::temp_dir().join(format!("sbx-exrepo-keys-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let key = minisign::KeyPair::generate_unencrypted_keypair()
        .unwrap()
        .pk
        .to_base64();
    let src = Sources::new(root.join("repos"));
    let r = Repository::new("k", "https://example.com/r")
        .unwrap()
        .with_keys(vec![key.clone()])
        .unwrap();
    assert!(r.signed());
    src.add(r.clone()).unwrap();
    src.add(Repository::new("plain", "https://example.com/p").unwrap())
        .unwrap();
    assert_eq!(
        src.list().unwrap(),
        [
            r,
            Repository::new("plain", "https://example.com/p").unwrap()
        ]
    );
    assert!(
        fs::read_to_string(root.join("repos"))
            .unwrap()
            .contains(&format!(" key={key}"))
    );
    assert!(
        Repository::new("k", "https://example.com/r")
            .unwrap()
            .with_keys(vec!["nope".into()])
            .is_err()
    );
    fs::write(root.join("repos"), "x https://example.com/x what\n").unwrap();
    assert!(
        src.list()
            .unwrap_err()
            .to_string()
            .contains("unknown field")
    );
    fs::write(root.join("repos"), "x https://example.com/x key=bad\n").unwrap();
    assert!(
        src.list()
            .unwrap_err()
            .to_string()
            .contains("invalid minisign public key")
    );
    let _ = fs::remove_dir_all(&root);
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
                let file = root.join(path.trim_start_matches('/'));
                // `<파일>.redirect`가 있으면 그 내용(URL)으로 보낸다.
                let mut redirect = file.clone().into_os_string();
                redirect.push(".redirect");
                if let Ok(to) = fs::read_to_string(&redirect) {
                    let r = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        to.trim()
                    );
                    let _ = s.write_all(r.as_bytes()).await;
                    return;
                }
                let resp = match fs::read(&file) {
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
async fn redirects_are_checked_at_every_hop() {
    let repo = Repo::new("redirect");
    let addr = serve(repo.root.join("repo")).await;
    let f = Fetcher::new().unwrap();
    let at = |p: &str| Repository::new("t", &format!("http://{addr}/{p}")).unwrap();
    // loopback 안에서 옮겨 가는 것은 따라간다.
    fs::create_dir_all(repo.root.join("repo/moved")).unwrap();
    fs::write(
        repo.root.join("repo/moved/index.json.redirect"),
        format!("http://{addr}/index.json"),
    )
    .unwrap();
    assert!(
        f.index(&at("moved/"))
            .await
            .unwrap()
            .executor("fc")
            .is_some()
    );
    // 평문 http로 밖에 나가는 redirect는 거부한다(마지막 URL만이 아니라 그 단계에서).
    fs::create_dir_all(repo.root.join("repo/out")).unwrap();
    fs::write(
        repo.root.join("repo/out/index.json.redirect"),
        "http://example.invalid/index.json",
    )
    .unwrap();
    let e = f.index(&at("out/")).await.unwrap_err().to_string();
    assert!(e.contains("redirected: http://example.invalid/index.json: plain http is allowed only for localhost"), "{e}");
    // 끝없는 redirect도 멈춘다.
    fs::create_dir_all(repo.root.join("repo/loop")).unwrap();
    fs::write(
        repo.root.join("repo/loop/index.json.redirect"),
        format!("http://{addr}/loop/index.json"),
    )
    .unwrap();
    let e = f.index(&at("loop/")).await.unwrap_err().to_string();
    assert!(e.contains("more than 10 redirects"), "{e}");
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

#[test]
fn only_unread_repositories_before_the_chosen_one_block_it() {
    let read = vec![
        ("b".to_owned(), parse(sample()).unwrap()),
        ("d".to_owned(), parse(sample()).unwrap()),
    ];
    let order: Vec<String> = ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
    assert_eq!(unread_before(&order, &read, "b"), Some("a"));
    assert_eq!(unread_before(&order, &read, "d"), Some("a"));
    assert_eq!(unread_before(&order[1..], &read, "b"), None);
    assert_eq!(unread_before(&order[1..], &read, "d"), Some("c"));
    // 고른 저장소가 목록에 없으면(있을 수 없지만) 앞의 읽지 못한 것 모두가 막는다.
    assert_eq!(unread_before(&order[1..], &read, "zzz"), Some("c"));
}

#[test]
fn a_version_has_a_bounded_number_of_artifacts() {
    let with = |n: usize| {
        let arts: Vec<_> = (0..n)
            .map(|i| artifact(&format!("os{i}"), "arm64", &format!("{i}.tar.gz")))
            .collect();
        let v = json!({"schema": 2, "executors": {"fc": {"versions": {"1.0": {"protocol": 1, "artifacts": arts}}}}});
        Index::parse(v.to_string().as_bytes(), &base())
    };
    assert!(with(index::MAX_ARTIFACTS_PER_VERSION).is_ok());
    assert!(
        with(index::MAX_ARTIFACTS_PER_VERSION + 1)
            .unwrap_err()
            .contains("at most")
    );
}

#[test]
fn a_large_index_parses_in_linear_time() {
    // 크기 한도 가까운 색인(이름 2만 개, 버전마다 압축 파일 하나)도 금방 읽힌다. 2차 시간이면 수십 초가 걸린다.
    let mut ex = serde_json::Map::new();
    for i in 0..20_000 {
        ex.insert(
            format!("e{i}"),
            json!({"versions": {"1.0": {"protocol": 1, "artifacts": [artifact("darwin", "arm64", "x.tar.gz")]}}}),
        );
    }
    let body = json!({"schema": 2, "executors": ex}).to_string();
    assert!(body.len() < index::MAX_INDEX_BYTES as usize);
    let t = std::time::Instant::now();
    let i = Index::parse(body.as_bytes(), &base()).unwrap();
    assert_eq!(i.executors.len(), 20_000);
    let idx = vec![("r".to_owned(), i)];
    let _ = index::available(&idx, None, &mac(), 1);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(20),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn concurrent_writers_never_lower_the_seen_serial() {
    // 여러 프로세스(여기서는 스레드마다 따로 연 파일)가 동시에 적어도 가장 큰 값이 남는다.
    let root = std::env::temp_dir().join(format!("sbx-exrepo-seen-race-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("seen");
    let repos: Vec<Repository> = (0..4)
        .map(|i| Repository::new(&format!("r{i}"), &format!("https://example.com/{i}")).unwrap())
        .collect();
    std::thread::scope(|s| {
        for t in 0..8u64 {
            let (path, repos) = (&path, &repos);
            s.spawn(move || {
                let seen = Seen::new(path);
                for k in 0..25u64 {
                    let serial = k * 8 + t;
                    for r in repos {
                        // 다른 스레드가 더 큰 값을 먼저 적었으면 거부되는 것이 맞다.
                        let _ = seen.check_and_record(r, serial, "u");
                    }
                }
            });
        }
    });
    let seen = Seen::new(&path);
    for r in &repos {
        assert_eq!(seen.get(r).unwrap(), Some(24 * 8 + 7), "{}", r.name);
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn concurrent_repository_adds_are_all_kept() {
    let root = std::env::temp_dir().join(format!("sbx-exrepo-list-race-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let path = root.join("repos");
    std::thread::scope(|s| {
        for t in 0..16 {
            let path = &path;
            s.spawn(move || {
                Sources::new(path)
                    .add(Repository::new(&format!("r{t}"), "https://example.com/").unwrap())
                    .unwrap();
            });
        }
    });
    assert_eq!(Sources::new(&path).list().unwrap().len(), 16);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn every_redirect_hop_stays_on_https() {
    let u = |s: &str| Url::parse(s).unwrap();
    let from_https = [u("https://example.com/index.json")];
    assert!(location::check_redirect(&from_https, &u("https://cdn.example.net/x")).is_ok());
    for bad in [
        "http://example.com/x",
        "http://127.0.0.1/x",
        "file:///etc/passwd",
        "ftp://x/",
    ] {
        assert!(
            location::check_redirect(&from_https, &u(bad)).is_err(),
            "{bad}"
        );
    }
    // 중간에 한 번이라도 https였다가 내려가는 것도 안 된다(처음이 https면 끝까지).
    let chain = [u("https://a.example/"), u("https://b.example/")];
    assert!(location::check_redirect(&chain, &u("http://localhost/x")).is_err());
    // loopback http 저장소는 loopback http 안에서만, 또는 https로.
    let local = [u("http://127.0.0.1:9000/r")];
    assert!(location::check_redirect(&local, &u("http://localhost:9001/r")).is_ok());
    assert!(location::check_redirect(&local, &u("https://example.com/r")).is_ok());
    assert!(location::check_redirect(&local, &u("http://example.com/r")).is_err());
    // 10번까지만.
    // 처음 요청 + redirect 10번까지.
    let chain: Vec<Url> = (0..=location::MAX_REDIRECTS)
        .map(|i| u(&format!("https://e.example/{i}")))
        .collect();
    assert!(
        location::check_redirect(&chain[..location::MAX_REDIRECTS], &u("https://e.example/z"))
            .is_ok()
    );
    assert!(
        location::check_redirect(&chain, &u("https://e.example/z"))
            .unwrap_err()
            .contains("redirects")
    );
}
