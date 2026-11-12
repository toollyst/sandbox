use std::path::PathBuf;

use sbx_executor_repo::{Fetcher, Seen};
use serde_json::json;
use url::Url;

use crate::index::{Index, Version};
use crate::{Source, download, fetch, status, target};

fn v(s: &str) -> Version {
    Version::parse(s).unwrap()
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("sbx-release-index-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

const HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn release(version: &str) -> serde_json::Value {
    json!({
        "version": version,
        "assets": { "linux-x86_64": { "url": format!("v{version}/sbx-linux-x86_64.tar.gz"), "sha256": HASH, "size": 10 } }
    })
}

fn index_json(releases: Vec<serde_json::Value>) -> serde_json::Value {
    json!({ "schema": 1, "serial": 5, "expires": 4_000_000_000u64, "releases": releases })
}

fn parse(v: &serde_json::Value) -> Result<Index, String> {
    let url = Url::parse("https://example.com/sandbox/releases.json").unwrap();
    Index::parse(v.to_string().as_bytes(), &url)
}

#[test]
fn versions_are_strict_release_versions() {
    assert_eq!(v("0.2.1"), v("v0.2.1"));
    assert!(v("0.10.0") > v("0.9.9"));
    for bad in [
        "0.2",
        "0.2.1.0",
        "01.2.3",
        "0.2.1-rc.1",
        "0.2.1+build",
        "",
        "a.b.c",
        "0.2.-1",
        "1234567890.0.0",
    ] {
        assert!(Version::parse(bad).is_err(), "{bad:?}");
    }
    assert!(v("0.2.9").same_line(&v("0.2.1")));
    assert!(!v("0.3.0").same_line(&v("0.2.1")));
}

#[test]
fn index_is_parsed_sorted_and_urls_resolved() {
    let idx = parse(&index_json(vec![release("0.2.2"), release("0.2.1")])).unwrap();
    assert_eq!(idx.releases[0].version, v("0.2.1"));
    assert_eq!(
        idx.releases[1].assets["linux-x86_64"].url.as_str(),
        "https://example.com/sandbox/v0.2.2/sbx-linux-x86_64.tar.gz"
    );
}

#[test]
fn bad_indexes_are_refused_whole() {
    let mut cases = vec![
        (
            "schema",
            json!({ "schema": 2, "serial": 1, "expires": 1, "releases": [] }),
        ),
        (
            "no serial",
            json!({ "schema": 1, "expires": 1, "releases": [] }),
        ),
        (
            "twice",
            index_json(vec![release("0.2.1"), release("v0.2.1")]),
        ),
    ];
    let mut r = release("0.2.1");
    r["assets"]["linux-x86_64"]["sha256"] = json!("ABC");
    cases.push(("sha256", index_json(vec![r])));
    let mut r = release("0.2.1");
    r["assets"]["linux-x86_64"]["size"] = json!(0);
    cases.push(("size", index_json(vec![r])));
    let mut r = release("0.2.1");
    r["assets"]["linux-x86_64"]["url"] = json!("file:///etc/passwd");
    cases.push(("https index pointing at file:", index_json(vec![r])));
    let mut r = release("0.2.1");
    r["assets"] = json!({ "../x": { "url": "a", "sha256": HASH, "size": 1 } });
    cases.push(("platform", index_json(vec![r])));
    let mut r = release("0.2.1");
    r["protocol"] = json!({ "min": 3, "max": 2 });
    cases.push(("protocol", index_json(vec![r])));
    for (what, case) in cases {
        assert!(parse(&case).is_err(), "{what}");
    }
}

#[test]
fn advisories_are_cleaned_and_unknown_fields_ignored() {
    let mut r = release("0.2.1");
    r["advisory"] = json!("bad\u{1b}[31m one\u{202e}");
    r["future"] = json!({ "x": 1 });
    let idx = parse(&index_json(vec![r])).unwrap();
    let a = idx.releases[0].advisory.as_deref().unwrap();
    assert!(!a.contains('\u{1b}') && !a.contains('\u{202e}'), "{a:?}");
}

#[test]
fn status_finds_the_newest_unyanked_build_for_this_platform() {
    let mut yanked = release("0.2.4");
    yanked["yanked"] = json!(true);
    let mut other_platform = release("0.2.5");
    other_platform["assets"] =
        json!({ "darwin-aarch64": { "url": "x", "sha256": HASH, "size": 1 } });
    let mut sec = release("0.2.2");
    sec["security"] = json!(true);
    let idx = parse(&index_json(vec![
        release("0.2.1"),
        sec,
        release("0.2.3"),
        yanked,
        other_platform,
    ]))
    .unwrap();
    let s = status(v("0.2.1"), &idx, "linux-x86_64");
    assert_eq!(s.latest.unwrap().version, v("0.2.3"));
    assert!(s.security && s.urgent() && s.notable());
    let s = status(v("0.2.2"), &idx, "linux-x86_64");
    assert!(!s.security, "the security fix is already in 0.2.2");
    assert!(!s.urgent());
    let s = status(v("0.2.3"), &idx, "linux-x86_64");
    assert!(s.latest.is_none() && !s.notable());
    let s = status(v("0.2.4"), &idx, "linux-x86_64");
    assert!(
        s.yanked() && s.urgent(),
        "running a yanked version is urgent"
    );
}

#[test]
fn target_rules() {
    let mut yanked = release("0.2.3");
    yanked["yanked"] = json!(true);
    let idx = parse(&index_json(vec![
        release("0.2.1"),
        release("0.2.2"),
        yanked,
    ]))
    .unwrap();
    let p = "linux-x86_64";
    let t = |cur: &str, want: Option<&str>, down: bool| {
        target(v(cur), &idx, p, want.map(v), down).map(|r| r.map(|r| r.version.to_string()))
    };
    assert_eq!(
        t("0.2.1", None, false),
        Ok(Some("0.2.2".into())),
        "yanked is skipped"
    );
    assert_eq!(t("0.2.2", None, false), Ok(None));
    assert_eq!(
        t("0.2.1", Some("0.2.3"), false),
        Ok(Some("0.2.3".into())),
        "named yanked"
    );
    assert_eq!(t("0.2.2", Some("0.2.2"), false), Ok(None));
    assert!(
        t("0.2.2", Some("0.2.1"), false).is_err(),
        "downgrade needs the flag"
    );
    assert_eq!(t("0.2.2", Some("0.2.1"), true), Ok(Some("0.2.1".into())));
    assert!(
        t("0.2.1", Some("0.9.0"), false).is_err(),
        "not in the index"
    );
    assert!(
        target(v("0.2.1"), &idx, "plan9-mips", Some(v("0.2.2")), false).is_err(),
        "no build"
    );
}

struct Signed {
    dir: PathBuf,
    sk: minisign::SecretKey,
    pk: String,
}

impl Signed {
    fn new(name: &str) -> Self {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        Self {
            dir: tmp(name),
            pk: kp.pk.to_base64(),
            sk: kp.sk,
        }
    }

    fn sign(&self, file: &str, body: &[u8]) {
        let sig = minisign::sign(None, &self.sk, body, None, None).unwrap();
        std::fs::write(self.dir.join(file), body).unwrap();
        std::fs::write(self.dir.join(format!("{file}.minisig")), sig.into_string()).unwrap();
    }

    fn url(&self, file: &str) -> Url {
        Url::from_file_path(self.dir.join(file)).unwrap()
    }

    fn source(&self) -> Source {
        Source {
            url: self.url("releases.json"),
            keys: vec![self.pk.clone()],
        }
    }

    fn publish(&self, serial: u64, expires: u64) {
        let mut idx = index_json(vec![release("0.2.2")]);
        idx["serial"] = json!(serial);
        idx["expires"] = json!(expires);
        self.sign("releases.json", idx.to_string().as_bytes());
    }
}

const LATER: u64 = 4_000_000_000;

#[tokio::test]
async fn signed_index_is_checked_for_signature_expiry_and_rollback() {
    let s = Signed::new("fetch");
    let f = Fetcher::new().unwrap();
    let seen = Seen::new(s.dir.join("seen"));
    s.publish(10, LATER);
    let idx = fetch(&f, &s.source(), Some(&seen)).await.unwrap();
    assert_eq!(idx.serial, 10);

    s.publish(9, LATER);
    let e = fetch(&f, &s.source(), Some(&seen)).await.unwrap_err();
    assert!(e.to_string().contains("rollback"), "{e}");

    s.publish(11, 1);
    let e = fetch(&f, &s.source(), Some(&seen)).await.unwrap_err();
    assert!(e.to_string().contains("expired"), "{e}");

    s.publish(12, LATER);
    let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
    let wrong = Source {
        keys: vec![other.pk.to_base64()],
        ..s.source()
    };
    let e = fetch(&f, &wrong, None).await.unwrap_err();
    assert!(e.to_string().contains("matches none"), "{e}");

    let none = Source {
        keys: vec![],
        ..s.source()
    };
    assert!(
        fetch(&f, &none, None).await.is_err(),
        "no keys trusts nothing"
    );

    std::fs::remove_file(s.dir.join("releases.json.minisig")).unwrap();
    let e = fetch(&f, &s.source(), None).await.unwrap_err();
    assert!(e.to_string().contains("no signature"), "{e}");
}

#[tokio::test]
async fn archives_need_the_hash_and_a_signature() {
    let s = Signed::new("download");
    let f = Fetcher::new().unwrap();
    let body = b"an sbx archive".to_vec();
    s.sign("a.tar.gz", &body);
    let sha: String = {
        use sha2_digest::digest;
        digest(&body)
    };
    let asset = crate::Asset {
        url: s.url("a.tar.gz"),
        sha256: sha.clone(),
        size: body.len() as u64,
    };
    let dest = s.dir.join("got");
    download(&f, &asset, std::slice::from_ref(&s.pk), &dest)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), body);

    // 해시는 맞지만 서명이 다른 내용의 것이면 거부하고 지운다.
    let other = minisign::sign(None, &s.sk, &b"something else"[..], None, None).unwrap();
    std::fs::write(s.dir.join("a.tar.gz.minisig"), other.into_string()).unwrap();
    let e = download(&f, &asset, std::slice::from_ref(&s.pk), &dest)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("untrusted"), "{e}");
    assert!(!dest.exists());

    let bad_hash = crate::Asset {
        sha256: HASH.into(),
        ..asset
    };
    assert!(
        download(&f, &bad_hash, std::slice::from_ref(&s.pk), &dest)
            .await
            .is_err()
    );
    assert!(!dest.exists());
}

mod sha2_digest {
    pub fn digest(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    }
}
