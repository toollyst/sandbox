//! 레이어 병합과 EROFS 쓰기 (`sbx-rootfs`): whiteout, opaque, 하드 링크, 디렉터리를 바꾸는 파일, 부모가 링크인
//! 경로, 그리고 이미지가 고른 커널 경로를 이미지 안에서 푸는 `MergedTree::resolve`. 항목은 구조화된 입력으로
//! 만든다(적은 이름을 자주 겹치게 해서 덮어쓰기, 지우기 규칙에 빨리 닿는다). tar 바이트부터는 `image_layer`가 본다.
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다(병합, EROFS 쓰기, 풀기 모두).
//! - 레이어를 적용할 때마다: 모든 경로가 정규화돼 있고(빈 조각, `.`, `..`, NUL 없음) 부모가 디렉터리다.
//!   whiteout 항목은 트리에 들어가지 않는다(`.wh.`로 시작하는 이름은 다른 항목의 부모 디렉터리일 때만 있다).
//! - 같은 `link_group`의 노드(하드 링크)는 종류와 메타데이터가 같다(EROFS에서는 inode 하나가 된다).
//! - `resolve`는 끝나고, 돌려준 경로에는 링크가 아닌 노드가 있다.

#![no_main]

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Cursor, Read};

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;
use sbx_fuzz::{Name, join};
use sbx_rootfs::erofs::ErofsBuilder;
use sbx_rootfs::{
    ContentRef, ContentSource, EntryKind, LayerEntry, MergedTree, Meta, Node, NodeKind,
    RootfsBuilder,
};

#[derive(Debug, Arbitrary)]
enum Kind {
    Dir,
    File(u8),
    Symlink { target: Vec<Name>, absolute: bool },
    Hardlink(Vec<Name>),
    Char(u32, u32),
    Block(u32, u32),
    Fifo,
}

#[derive(Debug, Arbitrary)]
enum XattrName {
    User,
    Trusted,
    Security,
    Acl,
    Raw(String),
}

#[derive(Debug, Arbitrary)]
struct Item {
    path: Vec<Name>,
    absolute: bool,
    kind: Kind,
    mode: u16,
    uid: u32,
    mtime: i64,
    xattrs: Vec<(XattrName, Vec<u8>)>,
}

#[derive(Debug, Arbitrary)]
struct Input {
    layers: Vec<Vec<Item>>,
    kernel: Vec<Name>,
}

/// 파일 내용: 크기만큼 순번 바이트.
struct Content;

impl ContentSource for Content {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(io::repeat(c.index as u8).take(c.size)))
    }
}

fn entry(layer: usize, index: usize, item: &Item) -> LayerEntry {
    let kind = match &item.kind {
        Kind::Dir => EntryKind::Dir,
        Kind::File(size) => EntryKind::File(ContentRef {
            layer,
            index,
            size: u64::from(*size),
        }),
        Kind::Symlink { target, absolute } => EntryKind::Symlink(join(target, *absolute)),
        Kind::Hardlink(target) => EntryKind::Hardlink(join(target, false)),
        Kind::Char(major, minor) => EntryKind::CharDevice {
            major: *major,
            minor: *minor,
        },
        Kind::Block(major, minor) => EntryKind::BlockDevice {
            major: *major,
            minor: *minor,
        },
        Kind::Fifo => EntryKind::Fifo,
    };
    let xattrs: BTreeMap<String, Vec<u8>> = item
        .xattrs
        .iter()
        .map(|(n, v)| {
            let name = match n {
                XattrName::User => "user.a".to_owned(),
                XattrName::Trusted => "trusted.overlay.opaque".to_owned(),
                XattrName::Security => "security.capability".to_owned(),
                XattrName::Acl => "system.posix_acl_access".to_owned(),
                XattrName::Raw(s) => s.clone(),
            };
            (name, v.clone())
        })
        .collect();
    LayerEntry {
        path: join(&item.path, item.absolute),
        kind,
        meta: Meta {
            mode: u32::from(item.mode) & 0o7777,
            uid: item.uid,
            gid: 0,
            mtime: item.mtime,
            xattrs,
        },
    }
}

fn check(tree: &MergedTree) {
    let mut groups: HashMap<u64, &Node> = HashMap::new();
    for (path, node) in tree.iter() {
        if let Some(g) = node.link_group {
            match groups.get(&g) {
                Some(first) => {
                    assert_eq!(first.kind, node.kind, "hard links of group {g} differ");
                    assert_eq!(first.meta, node.meta, "hard links of group {g} differ");
                }
                None => {
                    groups.insert(g, node);
                }
            }
        }
        if path.is_empty() {
            continue;
        }
        assert!(
            path.split('/')
                .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\0')),
            "unnormalized path {path:?}"
        );
        let name = path.rsplit('/').next().unwrap_or_default();
        // whiteout 항목 자체는 들어가지 않는다. 그 이름은 다른 항목의 부모 디렉터리로만 생길 수 있다(`.wh.x/y`).
        assert!(
            !name.starts_with(".wh.") || node.kind == NodeKind::Dir,
            "whiteout {path:?} is in the tree"
        );
        let parent = path.rsplit_once('/').map_or("", |(p, _)| p);
        assert!(
            matches!(tree.get(parent).map(|n| &n.kind), Some(NodeKind::Dir)),
            "{path:?} has no directory parent"
        );
    }
}

fuzz_target!(|input: Input| {
    let mut tree = MergedTree::new();
    for (layer, items) in input.layers.iter().enumerate() {
        let entries = items
            .iter()
            .enumerate()
            .map(|(i, item)| entry(layer, i, item))
            .collect();
        if tree.apply_layer(entries).is_err() {
            break;
        }
        check(&tree);
    }

    let kernel = join(&input.kernel, true);
    if let Some(real) = tree.resolve(&kernel) {
        let node = tree.get(&real).expect("resolve returned a missing path");
        assert!(
            !matches!(node.kind, NodeKind::Symlink(_)),
            "{kernel:?} resolved to a link"
        );
    }

    let mut out = Cursor::new(Vec::new());
    let _ = ErofsBuilder.build(&tree, &Content, &mut out);
});
