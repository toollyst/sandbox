//! `/dev`의 링크. OCI 런타임(runc, crun)이 컨테이너의 `/dev`에 두는 것과 같은 링크를 만든다. devtmpfs는 커널이
//! 장치 노드만 채우므로 이 링크가 없다. 그러면 bash의 프로세스 치환(`<(…)`, `/dev/fd/63`)이나 `/dev/stdin`을 여는
//! 프로그램이 실패한다(예: 공식 postgres 이미지의 `initdb`).
//!
//! - [`LINKS`]의 이름마다 심볼릭 링크를 만든다. 그 이름이 이미 있으면(무엇이든, 깨진 링크도) 그대로 둔다.
//! - devtmpfs는 커널에 하나뿐이라 한 곳에 만든 링크는 모든 devtmpfs 마운트에 보인다. 그래도 `/dev`를 마운트하는
//!   곳마다(부팅의 새 뿌리, 빌드 층을 열 때) 부른다. 마운트하는 쪽이 이 사실에 기대지 않게 한다.

use std::io;
use std::path::Path;

/// (이름, 대상). runc의 기본 링크 가운데 `/proc`만 있으면 되는 것들이다.
pub const LINKS: [(&str, &str); 4] = [
    ("fd", "/proc/self/fd"),
    ("stdin", "/proc/self/fd/0"),
    ("stdout", "/proc/self/fd/1"),
    ("stderr", "/proc/self/fd/2"),
];

/// `dev`(마운트된 `/dev`) 안에 [`LINKS`]를 만든다. 규칙은 모듈 문서에 있다.
pub fn add_links(dev: &Path) -> io::Result<()> {
    for (name, target) in LINKS {
        let path = dev.join(name);
        match std::os::unix::fs::symlink(target, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => {
                return Err(io::Error::new(
                    e.kind(),
                    format!("{} -> {target}: {e}", path.display()),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_made_once_and_existing_names_are_kept() {
        let dev = std::env::temp_dir().join(format!("sbx-devfs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dev);
        std::fs::create_dir_all(&dev).unwrap();
        // 이미지나 다른 쪽이 둔 것은 바꾸지 않는다.
        std::fs::write(dev.join("stdout"), b"mine").unwrap();
        add_links(&dev).unwrap();
        for (name, target) in LINKS {
            let p = dev.join(name);
            if name == "stdout" {
                assert_eq!(std::fs::read(&p).unwrap(), b"mine");
            } else {
                assert_eq!(std::fs::read_link(&p).unwrap(), Path::new(target), "{name}");
            }
        }
        // 두 번 불러도 된다.
        add_links(&dev).unwrap();
        assert_eq!(
            std::fs::read_link(dev.join("fd")).unwrap(),
            Path::new("/proc/self/fd")
        );
        // 만들 수 없으면 무엇을 만들지 못했는지 알린다.
        let err = add_links(&dev.join("missing")).unwrap_err().to_string();
        assert!(err.contains("fd -> /proc/self/fd"), "{err}");
        let _ = std::fs::remove_dir_all(&dev);
    }
}
