//! 저장소 목록 파일. 한 줄에 `<이름> <URL> [key=<minisign 공개키>]...`, `#`로 시작하는 줄과 빈 줄은 무시한다.
//! 줄 순서가 우선순위다. 키가 있는 저장소는 서명된 색인만 받는다([`crate::trust`]).
//!
//! 파일이 없을 때의 목록(기본 저장소)은 쓰는 쪽이 [`Sources::with_defaults`]로 정한다. 이 크레이트는 어떤
//! 저장소도 기본으로 두지 않는다. 목록을 바꾸면(더하기, 지우기) 그때의 목록 전체를 파일에 쓰므로, 기본
//! 저장소를 지운 것도 그대로 남는다. 빈 파일은 빈 목록이다.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use url::Url;

use crate::error::RepoError;
use crate::location;

/// 저장소 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    /// 이름 (`[A-Za-z0-9._-]`, `.`으로 시작하지 않는다).
    pub name: String,
    /// 저장소 URL (디렉터리, 끝이 `/`).
    pub url: Url,
    /// 색인 서명을 확인할 minisign 공개키들(한 줄 base64). 하나라도 맞으면 된다(키를 바꿀 때 둘을 함께 둔다).
    /// 비어 있으면 서명하지 않은 저장소다.
    pub keys: Vec<String>,
}

/// minisign 공개키(한 줄 base64, `RW…`)가 맞는 꼴인지 본다.
pub fn check_key(key: &str) -> Result<(), String> {
    minisign_verify::PublicKey::from_base64(key)
        .map(|_| ())
        .map_err(|e| format!("invalid minisign public key {key:?}: {e}"))
}

impl Repository {
    /// 이름과 URL을 확인해 만든다.
    pub fn new(name: &str, url: &str) -> Result<Self, RepoError> {
        if name.is_empty()
            || name.starts_with('.')
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Err(RepoError::Config(format!(
                "invalid repository name {name:?} (use letters, digits, '.', '_', '-')"
            )));
        }
        Ok(Self {
            name: name.into(),
            url: location::repository(url).map_err(RepoError::Config)?,
            keys: Vec::new(),
        })
    }

    /// 색인 서명을 확인할 공개키를 둔다.
    pub fn with_keys(mut self, keys: Vec<String>) -> Result<Self, RepoError> {
        for k in &keys {
            check_key(k).map_err(RepoError::Config)?;
        }
        self.keys = keys;
        Ok(self)
    }

    /// 서명된 저장소인가.
    pub fn signed(&self) -> bool {
        !self.keys.is_empty()
    }

    /// schema 1 색인 URL.
    pub fn index_url(&self) -> Url {
        location::index(&self.url)
    }

    /// schema 2 색인 URL.
    pub fn index_v2_url(&self) -> Url {
        location::index_v2(&self.url)
    }
}

/// 저장소 목록 파일.
#[derive(Debug, Clone)]
pub struct Sources {
    path: PathBuf,
    defaults: Vec<Repository>,
}

impl Sources {
    /// 이 파일을 쓴다. 없으면 빈 목록이다.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            defaults: Vec::new(),
        }
    }

    /// 파일이 없을 때의 목록.
    pub fn with_defaults(mut self, defaults: Vec<Repository>) -> Self {
        self.defaults = defaults;
        self
    }

    /// 파일 경로.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 목록 (우선순위 순).
    pub fn list(&self) -> Result<Vec<Repository>, RepoError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(self.defaults.clone()),
            Err(e) => return Err(e.into()),
        };
        let mut out: Vec<Repository> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad =
                |m: String| RepoError::Config(format!("{}:{}: {m}", self.path.display(), n + 1));
            let mut parts = line.split_whitespace();
            let (Some(name), Some(url)) = (parts.next(), parts.next()) else {
                return Err(bad("expected \"<name> <url> [key=<public key>]...\"".into()));
            };
            let mut keys = Vec::new();
            for p in parts {
                let Some(k) = p.strip_prefix("key=") else {
                    return Err(bad(format!(
                        "unknown field {p:?} (expected key=<public key>)"
                    )));
                };
                keys.push(k.to_owned());
            }
            let r = Repository::new(name, url)
                .and_then(|r| r.with_keys(keys))
                .map_err(|e| bad(e.to_string()))?;
            if out.iter().any(|x| x.name == r.name) {
                return Err(bad(format!("repository {name} appears twice")));
            }
            out.push(r);
        }
        Ok(out)
    }

    /// 끝에 더한다. 같은 이름이 있으면 거부한다.
    pub fn add(&self, repo: Repository) -> Result<(), RepoError> {
        let mut list = self.list()?;
        if list.iter().any(|r| r.name == repo.name) {
            return Err(RepoError::Config(format!(
                "repository {} already exists",
                repo.name
            )));
        }
        list.push(repo);
        self.save(&list)
    }

    /// 지운다. 지운 것을 돌려준다.
    pub fn remove(&self, name: &str) -> Result<Repository, RepoError> {
        let mut list = self.list()?;
        let i = list
            .iter()
            .position(|r| r.name == name)
            .ok_or_else(|| RepoError::NotFound(format!("no repository named {name:?}")))?;
        let r = list.remove(i);
        self.save(&list)?;
        Ok(r)
    }

    /// 임시 파일에 쓰고 rename한다 (반쯤 쓴 파일은 없다).
    fn save(&self, list: &[Repository]) -> Result<(), RepoError> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = String::from(
            "# sbx executor repositories: <name> <url> [key=<minisign public key>]..., highest priority first\n",
        );
        for r in list {
            text.push_str(&format!("{} {}", r.name, r.url));
            for k in &r.keys {
                text.push_str(&format!(" key={k}"));
            }
            text.push('\n');
        }
        let tmp = self
            .path
            .with_extension(format!("tmp-{}", std::process::id()));
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_list_remove() {
        let dir = std::env::temp_dir().join(format!("sbx-exrepo-src-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let s = Sources::new(dir.join("repositories"));
        assert!(s.list().unwrap().is_empty());
        s.add(Repository::new("main", "https://example.com/r").unwrap())
            .unwrap();
        s.add(Repository::new("local", "file:///srv/r/").unwrap())
            .unwrap();
        assert!(
            s.add(Repository::new("main", "https://x.y/").unwrap())
                .is_err()
        );
        let names: Vec<_> = s.list().unwrap().into_iter().map(|r| r.name).collect();
        assert_eq!(names, ["main", "local"]);
        assert_eq!(s.list().unwrap()[0].url.as_str(), "https://example.com/r/");
        assert_eq!(s.remove("main").unwrap().name, "main");
        assert!(s.remove("main").is_err());
        assert_eq!(s.list().unwrap().len(), 1);

        // 손으로 고친 파일: 주석과 빈 줄은 무시, 잘못된 줄은 줄 번호와 함께 오류.
        fs::write(s.path(), "# c\n\nok https://a.b/\nbad\n").unwrap();
        let e = s.list().unwrap_err().to_string();
        assert!(
            e.ends_with(":4: expected \"<name> <url> [key=<public key>]...\""),
            "{e}"
        );
        for bad in ["", ".x", "a/b", "a b"] {
            assert!(Repository::new(bad, "https://a.b/").is_err(), "{bad}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn defaults_apply_until_the_list_is_changed() {
        let dir = std::env::temp_dir().join(format!("sbx-exrepo-def-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let s = Sources::new(dir.join("repositories")).with_defaults(vec![
            Repository::new("official", "https://o.example/").unwrap(),
        ]);
        let names = |s: &Sources| -> Vec<String> {
            s.list().unwrap().into_iter().map(|r| r.name).collect()
        };
        assert_eq!(names(&s), ["official"]);
        // 더하면 기본 저장소 뒤에 붙고, 파일에 둘 다 남는다.
        s.add(Repository::new("mine", "https://m.example/").unwrap())
            .unwrap();
        assert_eq!(names(&s), ["official", "mine"]);
        // 기본 저장소를 지운 것도 남는다. 모두 지우면 빈 목록이다(기본으로 돌아가지 않는다).
        s.remove("official").unwrap();
        s.remove("mine").unwrap();
        assert!(names(&s).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
