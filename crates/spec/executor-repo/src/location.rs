//! URL 규칙: 받는 scheme, 저장소 URL에서 색인 URL 만들기. 가장 아래 모듈 중 하나다.

use url::Url;

/// schema 1 색인 파일 이름. 저장소 URL(디렉터리) 아래에 있다.
pub const INDEX_FILE: &str = "index.json";

/// schema 2 색인 파일 이름. 저장소 URL(디렉터리) 아래에 있다.
///
/// 따로 두는 까닭: schema 1만 읽는 sbx는 `index.json`의 schema가 2이면 읽지 않는다.
/// 저장소는 `index.json`(schema 1, 자체 완결 실행기만)을 그대로 두고 `v2/index.json`을 더한다.
pub const INDEX_V2_FILE: &str = "v2/index.json";

/// 쓸 수 있는 URL인지. `https`, `file`, 그리고 loopback 호스트(`localhost`, `127.0.0.1`, `[::1]`)의 `http`.
///
/// 색인의 무결성은 전송(TLS)에 기댄다. 그래서 다른 호스트의 `http`는 받지 않는다.
pub fn check(url: &Url) -> Result<(), String> {
    match url.scheme() {
        "https" | "file" => Ok(()),
        "http" if is_loopback(url) => Ok(()),
        "http" => Err(format!(
            "{url}: plain http is allowed only for localhost; use https"
        )),
        s => Err(format!("{url}: unsupported URL scheme {s:?}")),
    }
}

/// 색인 `index`가 가리키는 압축 파일 `artifact`의 URL. [`check`]에 더해, 색인과 같은 scheme이거나 `https`여야
/// 한다. 원격 색인이 이 기계의 파일(`file:`)이나 이 기계의 서비스(loopback `http:`)를 가리키지 못하게 한다.
pub fn check_artifact(index: &Url, artifact: &Url) -> Result<(), String> {
    check(artifact)?;
    if artifact.scheme() == index.scheme() || artifact.scheme() == "https" {
        Ok(())
    } else {
        Err(format!(
            "{artifact}: an index at a {}: URL may point only to {}: or https: archives",
            index.scheme(),
            index.scheme()
        ))
    }
}

fn is_loopback(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// 저장소 URL을 읽는다. 디렉터리 URL로 다룬다(끝에 `/`를 붙인다).
pub fn repository(s: &str) -> Result<Url, String> {
    let mut url = Url::parse(s).map_err(|e| format!("{s}: {e}"))?;
    check(&url)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{s}: a repository URL has no query or fragment"));
    }
    if !url.path().ends_with('/') {
        let p = format!("{}/", url.path());
        url.set_path(&p);
    }
    Ok(url)
}

/// 저장소 URL의 schema 1 색인 URL.
pub fn index(repository: &Url) -> Url {
    repository
        .join(INDEX_FILE)
        .expect("a directory URL joins a file name")
}

/// 색인 서명 파일의 접미사. 서명은 색인 URL에 이것을 붙인 곳에 있다(`v2/index.json.minisig`).
pub const SIGNATURE_SUFFIX: &str = ".minisig";

/// 색인 URL의 서명 URL.
pub fn signature(index: &Url) -> Url {
    let mut u = index.clone();
    let p = format!("{}{SIGNATURE_SUFFIX}", index.path());
    u.set_path(&p);
    u
}

/// 저장소 URL의 schema 2 색인 URL.
pub fn index_v2(repository: &Url) -> Url {
    repository
        .join(INDEX_V2_FILE)
        .expect("a directory URL joins a file name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemes_and_index_url() {
        for ok in [
            "https://example.com/x",
            "file:///srv/repo/",
            "http://localhost:8080/r",
            "http://127.0.0.1/r",
            "http://[::1]/r",
        ] {
            assert!(repository(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://example.com/r",
            "ftp://example.com/r",
            "https://e.com/r?x=1",
            "not a url",
        ] {
            assert!(repository(bad).is_err(), "{bad}");
        }
        let r = repository("https://example.com/sbx/executors").unwrap();
        assert_eq!(
            index(&r).as_str(),
            "https://example.com/sbx/executors/index.json"
        );
        let r = repository("https://example.com/sbx/").unwrap();
        assert_eq!(index(&r).as_str(), "https://example.com/sbx/index.json");
        assert_eq!(
            index_v2(&r).as_str(),
            "https://example.com/sbx/v2/index.json"
        );
        assert_eq!(
            signature(&index_v2(&r)).as_str(),
            "https://example.com/sbx/v2/index.json.minisig"
        );
    }
}
