//! 실행기 묶음 풀기 (`sbx-executor-repo`의 `unpack::unpack`): gzip으로 압축한 tar를 빈 디렉터리에 푼다. 묶음은
//! 색인의 sha256과 맞아야 풀리지만, 그 검사는 "색인을 낸 쪽이 낸 그대로"만 보인다. 풀기 자체가 디렉터리 밖에 쓰면
//! 안 된다.
//!
//! 입력의 첫 바이트가 묶음 종류(`Layout`)를 고르고, 나머지가 tar다(퍼저가 tar 구조를 바꾸게 여기서 gzip으로 싼다).
//!
//! ```text
//! <run>/pkg.tar.gz        묶음
//! <run>/outside/secret    대상 밖. 바뀌면 안 된다.
//! <run>/dest/             여기에 푼다(없는 상태에서 시작)
//! ```
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - 결과가 무엇이든 `<run>`에는 처음 셋만 있고 `outside`는 그대로다.
//! - `dest` 아래에는 일반 파일과 디렉터리만 있다(링크, 장치, FIFO는 만들지 않는다). setuid, setgid, sticky 비트가 없다.
//! - 돌려준 묶음 디렉터리는 `dest`이거나 그 아래다.

#![no_main]

use std::fs;
use std::io::Write;

use libfuzzer_sys::fuzz_target;
use sbx_executor_repo::unpack::{Layout, unpack};
use sbx_fuzz::{Item, Scratch, snapshot};

fuzz_target!(|data: &[u8]| {
    let Some((&kind, tar)) = data.split_first() else {
        return;
    };
    let layout = if kind & 1 == 0 {
        Layout::Executor
    } else {
        Layout::Vmm
    };
    let run = Scratch::get("executor_unpack").fresh();
    let outside = run.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), b"do not touch").unwrap();
    let before = snapshot(&outside, 1 << 16);

    let archive = run.join("pkg.tar.gz");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(tar).unwrap();
    fs::write(&archive, gz.finish().unwrap()).unwrap();

    let dest = run.join("dest");
    let result = unpack(&archive, &dest, layout);

    assert_eq!(snapshot(&outside, 1 << 16), before, "outside changed");
    let mut top: Vec<_> = fs::read_dir(&run)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    top.sort();
    assert_eq!(
        top,
        ["dest", "outside", "pkg.tar.gz"],
        "something was written next to dest"
    );

    for (rel, item) in snapshot(&dest, 0) {
        assert!(
            matches!(item, Item::Dir | Item::File(_)),
            "{} is a {item:?}",
            rel.display()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::symlink_metadata(dest.join(&rel))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o7000,
                0,
                "{} kept setuid/setgid/sticky",
                rel.display()
            );
        }
    }
    if let Ok(root) = result {
        assert!(
            root.starts_with(&dest),
            "{} is outside {}",
            root.display(),
            dest.display()
        );
    }
});
