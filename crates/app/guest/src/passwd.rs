//! 사용자 해석. rootfs의 `/etc/passwd`와 `/etc/group`을 읽는다. 규칙은 Docker와 같다.
//!
//! - 숫자 사용자: 그대로 uid. `/etc/passwd`에 있으면 그 기본 그룹, 없으면 gid 0, 홈은 `/`.
//! - 이름 사용자: `/etc/passwd`에서 찾는다. 없으면 오류.
//! - 그룹을 주면 그것이 gid다. 이름이면 `/etc/group`에서 찾고, 없으면 오류.
//! - 보조 그룹은 설정하지 않는다(사용자를 바꾸면 비운다). Docker와 다른 점이다.

use std::path::Path;

use sbx_guest_proto::{Error, ErrorKind, Ident, User};

/// 해석한 사용자.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Creds {
    /// uid.
    pub uid: u32,
    /// gid.
    pub gid: u32,
    /// 홈 디렉터리. 환경에 `HOME`이 없을 때 쓴다.
    pub home: String,
}

struct PasswdEntry {
    name: String,
    uid: u32,
    gid: u32,
    home: String,
}

fn read_lines(path: &Path) -> Vec<String> {
    // 파일이 없거나 읽을 수 없으면 항목이 없는 것으로 본다(Docker와 같다).
    std::fs::read(path)
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn passwd(root: &Path) -> Vec<PasswdEntry> {
    read_lines(&root.join("etc/passwd"))
        .iter()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(':').collect();
            if f.len() < 7 || l.starts_with('#') {
                return None;
            }
            Some(PasswdEntry {
                name: f[0].to_owned(),
                uid: f[2].parse().ok()?,
                gid: f[3].parse().ok()?,
                home: f[5].to_owned(),
            })
        })
        .collect()
}

fn group_id(root: &Path, name: &str) -> Option<u32> {
    read_lines(&root.join("etc/group")).iter().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.len() >= 3 && f[0] == name).then(|| f[2].parse().ok())?
    })
}

/// `root`를 rootfs로 보고 사용자를 해석한다.
pub fn resolve(root: &Path, user: &User) -> Result<Creds, Error> {
    let entries = passwd(root);
    let (uid, default_gid, home) = match &user.user {
        Ident::Id(uid) => match entries.iter().find(|e| e.uid == *uid) {
            Some(e) => (*uid, e.gid, e.home.clone()),
            None => (*uid, 0, "/".to_owned()),
        },
        Ident::Name(name) => match entries.iter().find(|e| &e.name == name) {
            Some(e) => (e.uid, e.gid, e.home.clone()),
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidArgument,
                    format!("unable to find user {name}: no matching entries in passwd file"),
                ));
            }
        },
    };
    let gid = match &user.group {
        None => default_gid,
        Some(Ident::Id(g)) => *g,
        Some(Ident::Name(name)) => group_id(root, name).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidArgument,
                format!("unable to find group {name}: no matching entries in group file"),
            )
        })?,
    };
    let home = if home.is_empty() {
        "/".to_owned()
    } else {
        home
    };
    Ok(Creds { uid, gid, home })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("sandbox-guest-passwd-{}", std::process::id()));
        std::fs::create_dir_all(p.join("etc")).unwrap();
        std::fs::write(
            p.join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\n# comment\nbroken\nnobody:x:65534:65533:nobody:/nonexistent:/bin/false\nweb:x:33:33::/var/www:/bin/sh\n",
        )
        .unwrap();
        std::fs::write(p.join("etc/group"), "root:x:0:\nwww:x:82:web\n").unwrap();
        p
    }

    fn user(user: Ident, group: Option<Ident>) -> User {
        User { user, group }
    }

    #[test]
    fn docker_rules() {
        let r = root();
        let c = |u, g| resolve(&r, &user(u, g));
        assert_eq!(
            c(Ident::Name("nobody".into()), None).unwrap(),
            Creds {
                uid: 65534,
                gid: 65533,
                home: "/nonexistent".into()
            }
        );
        assert_eq!(
            c(Ident::Id(0), None).unwrap(),
            Creds {
                uid: 0,
                gid: 0,
                home: "/root".into()
            }
        );
        // passwd에 없는 숫자 사용자는 gid 0, 홈 /.
        assert_eq!(
            c(Ident::Id(1234), None).unwrap(),
            Creds {
                uid: 1234,
                gid: 0,
                home: "/".into()
            }
        );
        assert_eq!(
            c(Ident::Name("web".into()), Some(Ident::Name("www".into())))
                .unwrap()
                .gid,
            82
        );
        assert_eq!(c(Ident::Id(33), Some(Ident::Id(7))).unwrap().gid, 7);
        assert_eq!(
            c(Ident::Name("ghost".into()), None).unwrap_err().kind,
            ErrorKind::InvalidArgument
        );
        assert_eq!(
            c(Ident::Id(0), Some(Ident::Name("ghost".into())))
                .unwrap_err()
                .kind,
            ErrorKind::InvalidArgument
        );
        std::fs::remove_dir_all(r).unwrap();
    }

    #[test]
    fn missing_files_allow_only_numeric_users() {
        let r = std::env::temp_dir().join(format!("sandbox-guest-nopasswd-{}", std::process::id()));
        assert_eq!(resolve(&r, &user(Ident::Id(5), None)).unwrap().gid, 0);
        assert!(resolve(&r, &user(Ident::Name("root".into()), None)).is_err());
    }
}
