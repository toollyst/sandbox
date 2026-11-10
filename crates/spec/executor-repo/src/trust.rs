//! 색인을 믿는 근거: minisign 서명, 기한(`expires`), 본 적 있는 일련번호(`serial`).
//!
//! 서명은 색인을 고친 사람이 저장소 키를 가진 사람인지 본다. 서명만으로는 옛 색인을 다시 내미는 것(롤백)과
//! 새 색인을 감추는 것(동결)을 막지 못하므로, 서명 범위 안에 `serial`과 `expires`를 두고 클라이언트가 본 가장 큰
//! `serial`을 기억한다(TUF의 timestamp, APT의 `Valid-Until`과 같은 생각).

use std::fs;
use std::io;
use std::path::PathBuf;

use minisign_verify::{PublicKey, Signature};

use crate::error::RepoError;
use crate::sources::Repository;

/// 서명 파일의 크기 한도 (바이트).
pub const MAX_SIGNATURE_BYTES: u64 = 4 << 10;

/// `body`의 minisign 서명(`.minisig` 파일 내용)이 `keys` 중 하나로 맞는지 본다. 미리 해시한 서명(minisign 0.8 뒤의
/// 기본)만 받는다.
pub fn verify(body: &[u8], signature: &str, keys: &[String]) -> Result<(), String> {
    let sig = Signature::decode(signature).map_err(|e| format!("unreadable signature: {e}"))?;
    let mut last = String::from("no keys");
    for k in keys {
        let pk = PublicKey::from_base64(k).map_err(|e| format!("invalid key {k:?}: {e}"))?;
        match pk.verify(body, &sig, false) {
            Ok(()) => return Ok(()),
            Err(e) => last = e.to_string(),
        }
    }
    Err(format!(
        "the signature matches none of the repository's keys ({last})"
    ))
}

/// 지금(Unix 초).
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 저장소마다 본 가장 큰 `serial`을 적는 파일. 한 줄에 `<저장소 이름> <serial> <URL>`. 저장소를 지우고 같은
/// 이름으로 다른 URL을 더하면 새로 센다.
#[derive(Debug, Clone)]
pub struct Seen {
    path: PathBuf,
}

impl Seen {
    /// 이 파일을 쓴다. 없으면 본 것이 없다.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn read(&self) -> Result<Vec<(String, u64, String)>, RepoError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        Ok(text
            .lines()
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                let (n, s, u) = (p.next()?, p.next()?.parse().ok()?, p.next()?);
                Some((n.to_owned(), s, u.to_owned()))
            })
            .collect())
    }

    /// 본 가장 큰 일련번호.
    pub fn get(&self, repo: &Repository) -> Result<Option<u64>, RepoError> {
        Ok(self
            .read()?
            .into_iter()
            .find(|(n, _, u)| *n == repo.name && *u == repo.url.as_str())
            .map(|(_, s, _)| s))
    }

    /// `serial`이 본 것보다 작으면 거부하고, 크면 적는다.
    pub fn check_and_record(
        &self,
        repo: &Repository,
        serial: u64,
        url: &str,
    ) -> Result<(), RepoError> {
        let mut all = self.read()?;
        let i = all
            .iter()
            .position(|(n, _, u)| *n == repo.name && *u == repo.url.as_str());
        if let Some(i) = i {
            let seen = all[i].1;
            if serial < seen {
                return Err(RepoError::Untrusted {
                    url: url.to_owned(),
                    reason: format!(
                        "serial {serial} is older than {seen}, which this machine already saw (a rollback?)"
                    ),
                });
            }
            if serial == seen {
                return Ok(());
            }
            all[i].1 = serial;
        } else {
            all.push((repo.name.clone(), serial, repo.url.to_string()));
        }
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut text = String::from(
            "# sbx: the highest index serial seen per repository: <name> <serial> <url>\n",
        );
        for (n, s, u) in &all {
            text.push_str(&format!("{n} {s} {u}\n"));
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
