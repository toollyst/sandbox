//! 이미지 레이어 하나를 rootfs 디스크까지 (`sbx prepare`의 경로): 압축 풀기(`sbx-manage`의 zstd, gzip),
//! 레이어 tar 읽기(`sbx-manage`의 레이어 읽기), 병합(`sbx-rootfs`의 `MergedTree`), EROFS 쓰기(`ErofsBuilder`).
//! 레이어 내용은 레지스트리가 고른 것이다(digest는 내용이 manifest와 같다는 것만 보인다).
//!
//! 첫 바이트가 압축(0 없음, 1 gzip, 2 zstd)을 고르고, 나머지가 레이어 blob이다. 같은 blob을 두 번째 레이어로도
//! 한 번 더 얹는다(위 레이어가 아래 레이어를 지우고 바꾸는 규칙까지 가게).
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다. 풀린 크기는 [`MAX_UNPACKED`]에서 자른다(작은 입력이 큰 출력이 되는 압축 폭탄으로 퍼저가
//!   느려지지 않게. 잘리면 tar가 끝나지 않아 오류다).
//! - 레이어 파일 내용은 작업 공간의 생성된 이름(`<레이어>-<순번>`)에만 저장된다: 작업 공간에는 그 이름의 파일만 있다.
//! - 병합한 트리의 모든 경로는 정규화돼 있고(빈 조각, `.`, `..` 없음) 부모는 디렉터리다.

#![no_main]

use std::fs;
use std::io::{Cursor, Read};

use libfuzzer_sys::fuzz_target;
use sbx_fuzz::Scratch;
use sbx_manage::fuzzing::{Spool, ZstdReader, read_layer};
use sbx_rootfs::erofs::ErofsBuilder;
use sbx_rootfs::{MergedTree, NodeKind, RootfsBuilder};

/// 풀린 레이어를 이만큼에서 자른다.
const MAX_UNPACKED: u64 = 4 << 20;

fn decoded(kind: u8, blob: &[u8]) -> Box<dyn Read + '_> {
    match kind % 3 {
        0 => Box::new(blob),
        1 => Box::new(flate2::read::GzDecoder::new(blob)),
        _ => Box::new(ZstdReader::new(blob)),
    }
}

fn check_tree(tree: &MergedTree) {
    for (path, _) in tree.iter() {
        if path.is_empty() {
            continue;
        }
        assert!(
            path.split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".."),
            "unnormalized path {path:?}"
        );
        let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
        assert!(
            matches!(tree.get(parent).map(|n| &n.kind), Some(NodeKind::Dir)),
            "{path:?} has no directory parent"
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&kind, blob)) = data.split_first() else {
        return;
    };
    let spool = Scratch::get("image_layer").fresh();

    let mut tree = MergedTree::new();
    for layer in 0..2 {
        let r = decoded(kind, blob).take(MAX_UNPACKED);
        let Ok(entries) = read_layer(layer, r, &spool) else {
            break;
        };
        if tree.apply_layer(entries).is_err() {
            break;
        }
        check_tree(&tree);
    }

    for e in fs::read_dir(&spool).unwrap() {
        let name = e.unwrap().file_name().into_string().unwrap();
        let ok = name
            .split_once('-')
            .is_some_and(|(l, i)| l.parse::<usize>().is_ok() && i.parse::<usize>().is_ok());
        assert!(ok, "spool holds {name:?}, not a generated name");
    }

    let mut out = Cursor::new(Vec::new());
    let _ = ErofsBuilder.build(&tree, &Spool::new(&spool), &mut out);
});
