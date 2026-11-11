//! 디스크 배치 버전: data_dir과 cache_dir의 `layout-version` 파일(COMPATIBILITY.md "Data and cache directories").
//!
//! 파일 내용은 십진수 하나와 줄바꿈이다. 지금 배치는 [`CURRENT`]다.
//!
//! - 파일이 없으면(0.2.0이 쓴 디렉터리, 또는 처음) 지금 배치로 본다. 디렉터리가 있으면 [`CURRENT`]를 적는다.
//!   적지 못해도(읽기 전용 디렉터리) 오류가 아니다. 디렉터리가 없으면 만들지 않는다(다음에 적는다).
//! - [`CURRENT`]보다 크면 더 새 `sbx`가 배치를 바꾼 것이다. 짐작하지 않고 거부한다.
//! - [`CURRENT`]보다 작으면 [`migrate`]가 제자리에서 옮긴 뒤 [`CURRENT`]를 적는다(지금은 옮길 것이 없다).
//! - 숫자가 아니면 거부한다(누가 무엇을 썼는지 모른다).
//!
//! 배치를 바꾸는 변경(파일 이름, 디렉터리 구조, 형식에 버전이 없는 파일의 모양)은 [`CURRENT`]를 올리고 [`migrate`]에
//! 옮기는 단계를 더한다. 형식 안에 버전이 있는 파일(`main.log`의 머리, 저장소 색인의 `schema`)은 그 버전으로 다룬다.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// 이 `sbx`가 쓰는 배치.
pub const CURRENT: u32 = 1;

/// 파일 이름.
pub const FILE: &str = "layout-version";

/// `dir`(data_dir이나 cache_dir)을 확인한다. `what`은 오류 문구의 이름("data directory").
pub fn check(dir: &Path, what: &str) -> Result<(), String> {
    let path = dir.join(FILE);
    let found = match fs::read_to_string(&path) {
        Ok(s) => Some(parse(&s).ok_or_else(|| {
            format!(
                "the {what} {} has an unreadable {FILE} ({:?}); it was not written by sbx",
                dir.display(),
                s.trim()
            )
        })?),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };
    match found {
        Some(v) if v > CURRENT => Err(format!(
            "the {what} {} was changed by a newer sbx (layout {v}; this sbx understands up to {CURRENT}). \
             Upgrade sbx, or use another directory",
            dir.display()
        )),
        Some(v) if v == CURRENT => Ok(()),
        older => {
            migrate(dir, older.unwrap_or(CURRENT))?;
            if dir.is_dir() {
                write(&path);
            }
            Ok(())
        }
    }
}

fn parse(s: &str) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

/// `from` 배치를 [`CURRENT`]로 옮긴다. 배치 1이 처음이라 아직 할 일이 없다.
fn migrate(_dir: &Path, from: u32) -> Result<(), String> {
    debug_assert!(from <= CURRENT);
    Ok(())
}

/// 옆에 임시 파일로 쓰고 바꿔 넣는다. 실패는 무시한다(읽기 전용 디렉터리여도 sbx는 쓸 수 있어야 한다).
fn write(path: &Path) {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let ok = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .and_then(|mut f| f.write_all(format!("{CURRENT}\n").as_bytes()))
        .is_ok();
    if ok {
        if fs::rename(&tmp, path).is_err() {
            let _ = fs::remove_file(&tmp);
        }
    } else {
        let _ = fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn tmp() -> Tmp {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "sbx-layout-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p)
    }

    #[test]
    fn missing_file_is_written_with_the_current_layout() {
        let d = tmp();
        check(d.path(), "data directory").unwrap();
        assert_eq!(
            fs::read_to_string(d.path().join(FILE)).unwrap(),
            format!("{CURRENT}\n")
        );
        // 다시 확인해도 그대로다.
        check(d.path(), "data directory").unwrap();
    }

    #[test]
    fn a_missing_directory_is_not_created() {
        let d = tmp();
        let sub = d.path().join("nope");
        check(&sub, "data directory").unwrap();
        assert!(!sub.exists());
    }

    #[test]
    fn a_newer_layout_is_refused() {
        let d = tmp();
        fs::write(d.path().join(FILE), format!("{}\n", CURRENT + 1)).unwrap();
        let e = check(d.path(), "cache directory").unwrap_err();
        assert!(e.contains("changed by a newer sbx"), "{e}");
        assert!(e.contains(&format!("layout {}", CURRENT + 1)), "{e}");
    }

    #[test]
    fn garbage_is_refused() {
        for bad in ["", "x", "-1", "1.0", "+1", "99999999999999999999"] {
            let d = tmp();
            fs::write(d.path().join(FILE), bad).unwrap();
            let e = check(d.path(), "data directory").unwrap_err();
            assert!(e.contains("unreadable"), "{bad:?}: {e}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_fine() {
        let d = tmp();
        fs::write(d.path().join(FILE), format!(" {CURRENT}\r\n")).unwrap();
        check(d.path(), "data directory").unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_still_works() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp();
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let r = check(d.path(), "data directory");
        fs::set_permissions(d.path(), fs::Permissions::from_mode(0o755)).unwrap();
        r.unwrap();
        // root는 모드와 상관없이 쓸 수 있다. 그 밖에는 쓰지 못했어야 한다.
        if !d.path().join(FILE).exists() {
            assert_eq!(
                fs::read_dir(d.path()).unwrap().count(),
                0,
                "no temp file left"
            );
        }
    }
}
