//! 호스트에서 바뀐 공유 폴더 항목을 게스트의 파일 감시에 알린다 (`HostMsg::ShareChanged`).
//!
//! virtio-fs는 호스트 쪽 변경을 게스트에 알리지 않는다(Linux의 virtio-fs 드라이버에는 알림 큐가 없다). 그래서 inotify를
//! 쓰는 감시 도구(vite·webpack의 chokidar, nodemon, watchdog, Rust의 notify, libuv의 `fs.watch`)가 호스트에서 고친
//! 파일을 보지 못하고 핫 리로드가 돌지 않는다. 호스트(`sbx-runtime`의 `watch` 모듈)가 바뀐 경로를 보내면, 게스트가 그
//! 항목에 파일 시스템 연산 하나를 해서 커널이 알림을 만들게 한다.
//!
//! **하는 일**: 경로마다
//! 1. 항목의 수정 시각을 새로 읽는다(Linux는 `statx`의 `AT_STATX_FORCE_SYNC`: 캐시가 아니라 공유 서버에 묻는다).
//! 2. 그 값 그대로 수정 시각만 다시 쓴다(`utimensat`, 접근 시각은 `UTIME_OMIT`, 마지막 링크는 따라가지 않는다).
//!    커널은 수정 시각만 정한 setattr을 `IN_MODIFY`로 알린다(`fsnotify_change`). 그 항목을 보는 감시와, 부모 디렉터리를
//!    보는 감시(그 이름과 함께)가 받는다. 내용, 크기, 수정 시각은 그대로다. 호스트 파일의 ctime만 바뀐다.
//! 3. 항목이 없으면(지워졌거나 옮겨졌다) 공유 안에서 가장 가까운 있는 조상 디렉터리에 같은 일을 한다. 디렉터리의
//!    `IN_MODIFY`를 받은 감시(chokidar, `fs.watch`)는 그 디렉터리를 다시 읽어 없어진 것을 안다.
//!
//! 새로 생긴 항목도 2를 한다: 부모 디렉터리 감시가 그 이름의 `IN_MODIFY`를 받는다. `IN_CREATE`·`IN_DELETE`는 만들 수
//! 없다(만들려면 파일을 실제로 만들고 지워야 한다). 그래서 이름의 생김과 사라짐을 그 사건으로만 보는 감시(watchdog의
//! `FileCreatedEvent` 등)는 "바뀜"으로 받는다.
//!
//! **알리지 못하는 것**
//! - 읽기 전용 공유: setattr이 `EROFS`로 실패한다. 실패한 항목은 건너뛰고 오류로 만들지 않는다.
//! - 1과 2 사이(공유 서버 왕복 두 번)에 호스트가 그 파일을 또 바꾸면 2가 앞의 수정 시각을 다시 쓴다.
//!
//! **믿음**: 호스트는 게스트가 믿는 쪽이다. 그래도 경로는 규칙([`check`])에 맞아야 하고, 조상을 찾아 올라가는 것은
//! `target`에서 멈춘다. 중간 구성 요소의 링크는 따라간다(게스트 안의 일이고 호스트에는 닿지 않는다).

use std::path::{Path, PathBuf};

use sbx_guest_proto::MAX_SHARE_CHANGES;

use crate::sys;

/// `ShareChanged`의 값이 규칙에 맞는지. 맞지 않으면 그 까닭.
pub fn check(target: &str, paths: &[String]) -> Result<(), String> {
    if !target.starts_with('/') || target.contains('\0') {
        return Err(format!("share target must be absolute: {target:?}"));
    }
    if paths.len() > MAX_SHARE_CHANGES as usize {
        return Err(format!(
            "{} changed paths in one message; at most {MAX_SHARE_CHANGES}",
            paths.len()
        ));
    }
    for p in paths {
        let bad = p.starts_with('/')
            || p.contains('\0')
            || (!p.is_empty() && p.split('/').any(|c| c.is_empty() || c == "." || c == ".."));
        if bad {
            return Err(format!("changed path must be relative and plain: {p:?}"));
        }
    }
    Ok(())
}

/// 규칙을 본 뒤 경로마다 알린다. 알린 항목(조상 포함, 겹치지 않게)을 돌려준다.
pub fn apply(target: &str, paths: &[String]) -> Result<Vec<PathBuf>, String> {
    check(target, paths)?;
    let root = Path::new(target);
    let mut touched: Vec<PathBuf> = Vec::new();
    for p in paths {
        let mut at = if p.is_empty() {
            root.to_path_buf()
        } else {
            root.join(p)
        };
        loop {
            if touched.contains(&at) {
                break;
            }
            match sys::touch_mtime(&at) {
                Ok(()) => {
                    touched.push(at);
                    break;
                }
                // 없으면 조상으로. `target`에서 멈춘다.
                Err(e)
                    if matches!(e.raw_os_error(), Some(libc::ENOENT | libc::ENOTDIR))
                        && at != root =>
                {
                    at = match at.parent() {
                        Some(parent) if parent.starts_with(root) => parent.to_path_buf(),
                        _ => break,
                    };
                }
                // 읽기 전용 공유 등: 건너뛴다.
                Err(_) => break,
            }
        }
    }
    Ok(touched)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, SystemTime};

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("sbx-changes-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn paths_must_be_plain_and_relative() {
        assert!(check("/app", &s(&["", "a", "a/b.txt", "...", ".hidden"])).is_ok());
        for bad in ["/etc", "a/../b", "..", ".", "a//b", "a/", "a/./b", "a\0b"] {
            assert!(check("/app", &s(&[bad])).is_err(), "{bad:?}");
        }
        assert!(check("app", &[]).is_err());
        let many = vec![String::from("a"); MAX_SHARE_CHANGES as usize + 1];
        assert!(check("/app", &many).is_err());
    }

    /// 있는 항목은 수정 시각이 그대로인 채 알리고, 없는 항목은 공유 안의 가장 가까운 조상을 알린다.
    #[test]
    fn existing_entries_keep_their_times_and_gone_ones_reach_an_ancestor() {
        let d = dir("apply");
        std::fs::create_dir_all(d.join("src/deep")).unwrap();
        let f = d.join("src/main.ts");
        std::fs::write(&f, b"x").unwrap();
        let old = SystemTime::UNIX_EPOCH + Duration::new(1_600_000_000, 123_456_789);
        std::fs::File::options()
            .write(true)
            .open(&f)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(old)
                    .set_accessed(old),
            )
            .unwrap();
        let before = std::fs::metadata(&f).unwrap();
        let target = d.to_str().unwrap();

        let touched = apply(
            target,
            &s(&["src/main.ts", "src/deep/gone/x", "src/main.ts"]),
        )
        .unwrap();
        assert_eq!(touched, [f.clone(), d.join("src/deep")]);
        let after = std::fs::metadata(&f).unwrap();
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(after.accessed().unwrap(), before.accessed().unwrap());
        assert_eq!(after.len(), 1);
        // ctime은 setattr로 바뀐다(이것이 알림을 만든 연산이다).
        assert!(
            (after.ctime(), after.ctime_nsec()) >= (before.ctime(), before.ctime_nsec()),
            "ctime went back"
        );

        // 조상이 모두 없으면 `target` 자신. 빈 경로도 `target`이다.
        let touched = apply(target, &s(&["nope/a/b", ""])).unwrap();
        assert_eq!(touched, std::slice::from_ref(&d));
        // `target`이 없으면 아무것도 하지 않는다.
        let gone = d.join("missing");
        assert!(
            apply(gone.to_str().unwrap(), &s(&["a"]))
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(d).unwrap();
    }

    /// Linux: 수정 시각만 다시 쓰면 그 파일과 부모 디렉터리의 감시가 `IN_MODIFY`를 받는다.
    #[cfg(target_os = "linux")]
    #[test]
    fn touching_raises_in_modify_for_inotify_watchers() {
        let d = dir("inotify");
        std::fs::write(d.join("a.txt"), b"1").unwrap();
        let w = crate::sys::linux::Inotify::watch(&d).unwrap();
        apply(d.to_str().unwrap(), &s(&["a.txt"])).unwrap();
        let events = w.read_events().unwrap();
        assert!(
            events
                .iter()
                .any(|(mask, name)| mask & libc::IN_MODIFY != 0 && name == "a.txt"),
            "{events:?}"
        );
        std::fs::remove_dir_all(d).unwrap();
    }
}
