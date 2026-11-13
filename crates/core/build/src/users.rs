//! 사용자·그룹 이름을 숫자로: 지금 뿌리의 `/etc/passwd`, `/etc/group`(Docker 29로 확인).
//!
//! - `--chown=이름`은 그 사용자의 기본 그룹이다(`lp` → `4:7`). `--chown=4`는 `4:4`다(그룹 번호도 사용자 번호).
//! - `USER`(`WORKDIR`이 만드는 디렉터리의 소유자)는 그룹이 없으면 그 사용자의 기본 그룹, 사용자가 `passwd`에 없으면 0이다.
//! - 없는 이름은 오류다.

use std::io;

use sbx_model::{Ident, UserSpec};

/// 뿌리의 파일을 읽는 함수(없으면 `None`).
pub type ReadFile<'a> = &'a mut dyn FnMut(&str) -> io::Result<Option<Vec<u8>>>;

fn fields(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| l.split(':').collect::<Vec<_>>())
        .filter(|f| f.len() >= 3)
}

/// `passwd`에서 (uid, 기본 gid).
fn passwd_by_name(text: &str, name: &str) -> Option<(u32, u32)> {
    fields(text)
        .filter(|f| f.len() >= 4 && f[0] == name)
        .find_map(|f| Some((f[2].parse().ok()?, f[3].parse().ok()?)))
}

fn passwd_by_uid(text: &str, uid: u32) -> Option<u32> {
    fields(text)
        .filter(|f| f.len() >= 4 && f[2].parse() == Ok(uid))
        .find_map(|f| f[3].parse().ok())
}

fn group_by_name(text: &str, name: &str) -> Option<u32> {
    fields(text)
        .filter(|f| f[0] == name)
        .find_map(|f| f[2].parse().ok())
}

fn read_text(read: ReadFile<'_>, path: &str) -> io::Result<String> {
    Ok(read(path)?
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default())
}

fn group(read: ReadFile<'_>, g: &Ident) -> Result<u32, String> {
    match g {
        Ident::Id(n) => Ok(*n),
        Ident::Name(name) => {
            let text = read_text(read, "/etc/group").map_err(|e| e.to_string())?;
            group_by_name(&text, name).ok_or_else(|| format!("unable to find group {name}"))
        }
    }
}

/// `--chown`의 숫자.
pub fn chown(read: ReadFile<'_>, spec: &UserSpec) -> Result<(u32, u32), String> {
    let (uid, primary) = match &spec.user {
        Ident::Id(n) => (*n, *n),
        Ident::Name(name) => {
            let text = read_text(read, "/etc/passwd").map_err(|e| e.to_string())?;
            passwd_by_name(&text, name).ok_or_else(|| format!("unable to find user {name}"))?
        }
    };
    let gid = match &spec.group {
        Some(g) => group(read, g)?,
        None => primary,
    };
    Ok((uid, gid))
}

/// config `User`(원문)의 숫자. 비었으면 root.
pub fn user(read: ReadFile<'_>, user: &str) -> Result<(u32, u32), String> {
    if user.is_empty() {
        return Ok((0, 0));
    }
    let spec: UserSpec = user.parse().map_err(|e| format!("{e}"))?;
    let (uid, primary) = match &spec.user {
        Ident::Id(n) => {
            let text = read_text(read, "/etc/passwd").map_err(|e| e.to_string())?;
            (*n, passwd_by_uid(&text, *n).unwrap_or(0))
        }
        Ident::Name(name) => {
            let text = read_text(read, "/etc/passwd").map_err(|e| e.to_string())?;
            passwd_by_name(&text, name).ok_or_else(|| format!("unable to find user {name}"))?
        }
    };
    let gid = match &spec.group {
        Some(g) => group(read, g)?,
        None => primary,
    };
    Ok((uid, gid))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str =
        "root:x:0:0:root:/root:/bin/sh\nlp:x:4:7:lp:/var/spool/lpd:/sbin/nologin\n";
    const GROUP: &str = "root:x:0:\nlp:x:7:lp\nstaff:x:50:\n";

    fn files(path: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(match path {
            "/etc/passwd" => Some(PASSWD.into()),
            "/etc/group" => Some(GROUP.into()),
            _ => None,
        })
    }

    #[test]
    fn chown_names_take_the_primary_group_and_numbers_repeat() {
        let mut r = files;
        let c = |s: &str, r: ReadFile<'_>| chown(r, &s.parse().unwrap());
        assert_eq!(c("lp", &mut r), Ok((4, 7)));
        assert_eq!(c("4", &mut r), Ok((4, 4)));
        assert_eq!(c("lp:staff", &mut r), Ok((4, 50)));
        assert_eq!(c("4:staff", &mut r), Ok((4, 50)));
        assert_eq!(c("9:8", &mut r), Ok((9, 8)));
        assert!(c("nobody", &mut r).unwrap_err().contains("nobody"));
        assert!(c("lp:nogroup", &mut r).unwrap_err().contains("nogroup"));
    }

    #[test]
    fn users_take_the_passwd_group_or_root() {
        let mut r = files;
        assert_eq!(user(&mut r, ""), Ok((0, 0)));
        assert_eq!(user(&mut r, "4"), Ok((4, 7)));
        assert_eq!(user(&mut r, "77"), Ok((77, 0)));
        assert_eq!(user(&mut r, "4:7"), Ok((4, 7)));
        assert_eq!(user(&mut r, "lp"), Ok((4, 7)));
    }
}
