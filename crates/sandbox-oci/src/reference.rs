//! 이미지 참조 해석. Docker CLI와 같은 규칙이다.
//!
//! | 입력 | 결과 |
//! |---|---|
//! | `alpine` | `docker.io/library/alpine:latest` |
//! | `alpine:3.20` | `docker.io/library/alpine:3.20` |
//! | `user/app` | `docker.io/user/app:latest` |
//! | `ghcr.io/o/r:t` | 레지스트리 `ghcr.io` |
//! | `localhost:5000/r` | 레지스트리 `localhost:5000` |
//! | `r@sha256:...` | 태그 없이 digest로 고정 |
//!
//! 첫 경로 조각에 `.`이나 `:`가 있거나 `localhost`이면 레지스트리로 본다. 아니면 Docker Hub다.
//! `index.docker.io`는 `docker.io`로 바꾼다.

use std::fmt;

use sandbox_model::{Digest, ParseError};

/// Docker Hub.
pub const DOCKER_HUB: &str = "docker.io";

/// 해석한 이미지 참조.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// 레지스트리 호스트(포트 포함).
    pub registry: String,
    /// 저장소 경로.
    pub repository: String,
    /// 태그. digest만 준 경우 없다.
    pub tag: Option<String>,
    /// digest. 있으면 태그보다 우선한다.
    pub digest: Option<Digest>,
}

impl Reference {
    /// 해석한다. 규칙은 모듈 문서의 표를 따른다.
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        let err = || ParseError::new("image reference", input);

        let (name_tag, digest) = match input.split_once('@') {
            Some((n, d)) => (n, Some(Digest::parse(d).map_err(|_| err())?)),
            None => (input, None),
        };

        let last_slash = name_tag.rfind('/').map_or(0, |i| i + 1);
        let (name, tag) = match name_tag[last_slash..].rfind(':') {
            Some(i) => (
                &name_tag[..last_slash + i],
                Some(&name_tag[last_slash + i + 1..]),
            ),
            None => (name_tag, None),
        };

        let (registry, path) = match name.split_once('/') {
            Some((first, rest))
                if first.contains('.') || first.contains(':') || first == "localhost" =>
            {
                (first, rest.to_owned())
            }
            _ => (DOCKER_HUB, name.to_owned()),
        };
        let registry = if registry == "index.docker.io" {
            DOCKER_HUB
        } else {
            registry
        };
        let repository = if registry == DOCKER_HUB && !path.contains('/') {
            format!("library/{path}")
        } else {
            path
        };

        if !valid_repository(&repository) {
            return Err(err());
        }
        if let Some(t) = tag
            && !valid_tag(t)
        {
            return Err(err());
        }
        let tag = match (tag, &digest) {
            (Some(t), _) => Some(t.to_owned()),
            (None, None) => Some("latest".to_owned()),
            (None, Some(_)) => None,
        };

        Ok(Self {
            registry: registry.to_owned(),
            repository,
            tag,
            digest,
        })
    }

    /// 레지스트리 API 호스트. Docker Hub는 `registry-1.docker.io`다.
    pub fn api_host(&self) -> &str {
        if self.registry == DOCKER_HUB {
            "registry-1.docker.io"
        } else {
            &self.registry
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.registry, self.repository)?;
        if let Some(t) = &self.tag {
            write!(f, ":{t}")?;
        }
        if let Some(d) = &self.digest {
            write!(f, "@{d}")?;
        }
        Ok(())
    }
}

/// 경로 조각: 소문자, 숫자, `.`, `_`, `-`. 처음과 끝은 소문자나 숫자.
fn valid_repository(repo: &str) -> bool {
    !repo.is_empty()
        && repo.split('/').all(|part| {
            let bytes = part.as_bytes();
            let alnum = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit();
            !bytes.is_empty()
                && bytes.first().is_some_and(alnum)
                && bytes.last().is_some_and(alnum)
                && bytes
                    .iter()
                    .all(|b| alnum(b) || matches!(b, b'.' | b'_' | b'-'))
        })
}

/// 태그: 1~128자, 영숫자와 `_`, `.`, `-`. `.`이나 `-`로 시작하지 않는다.
fn valid_tag(tag: &str) -> bool {
    (1..=128).contains(&tag.len())
        && !tag.starts_with(['.', '-'])
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(s: &str) -> String {
        Reference::parse(s).unwrap().to_string()
    }

    #[test]
    fn docker_hub_defaults() {
        assert_eq!(full("alpine"), "docker.io/library/alpine:latest");
        assert_eq!(full("alpine:3.20"), "docker.io/library/alpine:3.20");
        assert_eq!(full("user/app"), "docker.io/user/app:latest");
        assert_eq!(
            full("index.docker.io/user/app"),
            "docker.io/user/app:latest"
        );
        assert_eq!(full("docker.io/alpine"), "docker.io/library/alpine:latest");
    }

    #[test]
    fn other_registries() {
        let r = Reference::parse("ghcr.io/org/repo:v1").unwrap();
        assert_eq!(
            (r.registry.as_str(), r.repository.as_str()),
            ("ghcr.io", "org/repo")
        );
        let r = Reference::parse("localhost:5000/repo").unwrap();
        assert_eq!(r.registry, "localhost:5000");
        assert_eq!(r.tag.as_deref(), Some("latest"));
        let r = Reference::parse("localhost/repo").unwrap();
        assert_eq!(r.registry, "localhost");
    }

    #[test]
    fn digest_pins_without_default_tag() {
        let d = format!("sha256:{}", "0".repeat(64));
        let r = Reference::parse(&format!("alpine@{d}")).unwrap();
        assert_eq!(r.tag, None);
        assert_eq!(r.digest.unwrap().as_str(), d);
        let r = Reference::parse(&format!("alpine:3@{d}")).unwrap();
        assert_eq!(r.tag.as_deref(), Some("3"));
    }

    #[test]
    fn api_host_for_docker_hub() {
        assert_eq!(
            Reference::parse("alpine").unwrap().api_host(),
            "registry-1.docker.io"
        );
        assert_eq!(
            Reference::parse("ghcr.io/a/b").unwrap().api_host(),
            "ghcr.io"
        );
    }

    #[test]
    fn invalid_references() {
        for bad in [
            "",
            "Alpine",
            "alpine:",
            "alpine:-x",
            "a//b",
            "alpine@sha256:zz",
            "-a",
        ] {
            assert!(Reference::parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
