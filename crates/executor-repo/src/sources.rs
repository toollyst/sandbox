//! 저장소 목록 파일. 한 줄에 `<이름> <URL>`, `#`로 시작하는 줄과 빈 줄은 무시한다. 줄 순서가 우선순위다.

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
        })
    }

    /// 색인 URL.
    pub fn index_url(&self) -> Url {
        location::index(&self.url)
    }
}

/// 저장소 목록 파일.
#[derive(Debug, Clone)]
pub struct Sources {
    path: PathBuf,
}

impl Sources {
    /// 이 파일을 쓴다. 없으면 빈 목록이다.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// 파일 경로.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 목록 (우선순위 순).
    pub fn list(&self) -> Result<Vec<Repository>, RepoError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
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
            let (Some(name), Some(url), None) = (parts.next(), parts.next(), parts.next()) else {
                return Err(bad("expected \"<name> <url>\"".into()));
            };
            let r = Repository::new(name, url).map_err(|e| bad(e.to_string()))?;
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
        let mut text =
            String::from("# sbx executor repositories: <name> <url>, highest priority first\n");
        for r in list {
            text.push_str(&format!("{} {}\n", r.name, r.url));
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
        assert!(e.ends_with(":4: expected \"<name> <url>\""), "{e}");
        for bad in ["", ".x", "a/b", "a b"] {
            assert!(Repository::new(bad, "https://a.b/").is_err(), "{bad}");
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
