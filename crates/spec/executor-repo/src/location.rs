//! URL 규칙: 받는 scheme, 저장소 URL에서 색인 URL 만들기. 가장 아래 모듈 중 하나다.

use url::Url;

/// 색인 파일 이름. 저장소 URL(디렉터리) 아래에 있다.
pub const INDEX_FILE: &str = "index.json";

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

/// 저장소 URL의 색인 URL.
pub fn index(repository: &Url) -> Url {
    repository
        .join(INDEX_FILE)
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
    }
}
