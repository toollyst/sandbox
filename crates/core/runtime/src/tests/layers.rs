//! 빌드 층 API를 fake 실행기 위에서 끝까지 돌린다. fake는 층을 scratch 디스크 옆 디렉터리(`build_root`)로 흉내 낸다.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use sbx_rootfs::{ContentRef, ContentSource, EntryKind, MergedTree, Meta, NodeKind};

use super::*;

/// 층 tar를 다른 구현(`tar` 크레이트)으로 읽어 `sbx-rootfs`의 항목으로 바꾼다. 관리 쪽(`sbx-manage`의 레이어
/// 읽기)과 같은 필드를 같은 방법으로 읽는다: PAX `path`·`linkpath`, `SCHILY.xattr.*`, 장치 번호.
/// (관리 쪽을 직접 쓰지 않는 것은 실행 쪽 크레이트가 캐시 쓰기 feature를 켜지 않기 위해서다. `cargo xtask check-deps`.)
#[derive(Default)]
struct Contents(HashMap<(usize, usize), Vec<u8>>);

impl ContentSource for Contents {
    fn open(&self, c: &ContentRef) -> std::io::Result<Box<dyn Read + '_>> {
        Ok(Box::new(self.0[&(c.layer, c.index)].as_slice()))
    }
}

fn read_layer(layer: usize, t: &[u8], contents: &mut Contents) -> Vec<sbx_rootfs::LayerEntry> {
    let mut out = Vec::new();
    for (index, e) in tar::Archive::new(t).entries().unwrap().enumerate() {
        let mut e = e.unwrap();
        let h = e.header().clone();
        let path = String::from_utf8(e.path_bytes().into_owned()).unwrap();
        let link = e
            .link_name_bytes()
            .map(|l| String::from_utf8(l.into_owned()).unwrap());
        let mut xattrs = std::collections::BTreeMap::new();
        if let Some(exts) = e.pax_extensions().unwrap() {
            for x in exts {
                let x = x.unwrap();
                if let Some(n) = x.key().unwrap().strip_prefix("SCHILY.xattr.") {
                    xattrs.insert(n.to_owned(), x.value_bytes().to_vec());
                }
            }
        }
        let dev = || {
            (
                h.device_major().unwrap().unwrap(),
                h.device_minor().unwrap().unwrap(),
            )
        };
        let kind = match h.entry_type() {
            tar::EntryType::Regular => {
                let mut data = Vec::new();
                e.read_to_end(&mut data).unwrap();
                let size = data.len() as u64;
                contents.0.insert((layer, index), data);
                EntryKind::File(ContentRef { layer, index, size })
            }
            tar::EntryType::Directory => EntryKind::Dir,
            tar::EntryType::Symlink => EntryKind::Symlink(link.unwrap()),
            tar::EntryType::Link => EntryKind::Hardlink(link.unwrap()),
            tar::EntryType::Char => {
                let (major, minor) = dev();
                EntryKind::CharDevice { major, minor }
            }
            tar::EntryType::Block => {
                let (major, minor) = dev();
                EntryKind::BlockDevice { major, minor }
            }
            tar::EntryType::Fifo => EntryKind::Fifo,
            other => panic!("unexpected {other:?} at {path}"),
        };
        out.push(sbx_rootfs::LayerEntry {
            path,
            kind,
            meta: Meta {
                mode: h.mode().unwrap() & 0o7777,
                uid: h.uid().unwrap() as u32,
                gid: h.gid().unwrap() as u32,
                mtime: h.mtime().unwrap() as i64,
                xattrs,
            },
        });
    }
    out
}

/// 층 하나를 내보낸다: (tar, 받은 크기).
fn export(s: &Sandbox) -> (Vec<u8>, LayerStats) {
    block_on(s.export_layer(Vec::new(), 1 << 20)).unwrap()
}

fn tar_names(t: &[u8]) -> Vec<String> {
    tar::Archive::new(t)
        .entries()
        .unwrap()
        .map(|e| String::from_utf8(e.unwrap().path_bytes().into_owned()).unwrap())
        .collect()
}

/// 내보낸 tar를 읽어 rootfs 병합(`sbx-rootfs`, `prepare`가 쓰는 것)에 차례로 넣은 결과가 마지막 층으로 본 모습과
/// 같아야 한다.
#[test]
fn build_layers_export_tars_that_merge_into_what_the_guest_saw() {
    let e = Env::new("layers");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let root = sbx_executor_fake::build_root(&e.rt.paths(s.id()).scratch());
    let _main = block_on(s.start(IoMode::default())).unwrap();
    let r = root.display().to_string();
    let sh = |script: &str| {
        let p = block_on(s.exec(exec(&["sh", "-ec", script]))).unwrap();
        let (out, err, code) = collect(p);
        assert_eq!(code, Some(0), "{script}\n{out}{err}");
    };

    // 층 1: 더하기(파일, 디렉터리, 심볼릭 링크, 하드 링크). 호스트에서 복사해 넣기도 한다.
    assert_eq!(block_on(s.open_layer()).unwrap(), 1);
    sh(&format!(
        "mkdir -p {r}/etc {r}/data/d1/sub {r}/lib && echo base > {r}/etc/hosts && echo k > {r}/etc/keep \
         && echo old > {r}/data/d1/old && echo x > {r}/data/d1/sub/x && echo t > {r}/lib/target \
         && ln -s target {r}/lib/link && echo same > {r}/lib/h1 && ln {r}/lib/h1 {r}/lib/h2 \
         && chmod 0750 {r}/lib && chmod 0600 {r}/etc/keep"
    ));
    let src = e.root.join("src");
    std::fs::create_dir_all(src.join("app")).unwrap();
    std::fs::write(src.join("app/main.py"), "print(1)\n").unwrap();
    block_on(s.copy_in(&src.join("app"), &format!("{r}/srv"))).unwrap();
    let (l1, stats) = export(&s);
    assert_eq!(stats.tar_bytes, l1.len() as u64);
    let names = tar_names(&l1);
    assert!(names.contains(&"srv/main.py".to_owned()), "{names:?}");

    // 층 2: 바꾸기, 지우기(whiteout), 지우고 다시 만든 디렉터리(opaque).
    assert_eq!(block_on(s.open_layer()).unwrap(), 2);
    sh(&format!(
        "echo changed > {r}/etc/hosts && rm {r}/etc/keep && rm -r {r}/data/d1 && mkdir {r}/data/d1 \
         && echo fresh > {r}/data/d1/new"
    ));
    let (l2, _) = export(&s);
    let names = tar_names(&l2);
    for want in [
        "etc/.wh.keep",
        "data/d1/",
        "data/d1/.wh..wh..opq",
        "etc/hosts",
    ] {
        assert!(names.iter().any(|n| n == want), "{want} not in {names:?}");
    }

    // 층 3: 바꾸지 않는다.
    assert_eq!(block_on(s.open_layer()).unwrap(), 3);
    let (l3, stats) = export(&s);
    assert_eq!((l3.len(), stats.entries), (1024, 0));

    // 층 4: 층 1에서 만든 것을 지운다.
    assert_eq!(block_on(s.open_layer()).unwrap(), 4);
    sh(&format!("rm {r}/lib/h1 {r}/lib/link && rm -r {r}/srv"));
    let (l4, _) = export(&s);
    let names = tar_names(&l4);
    for want in ["lib/.wh.h1", "lib/.wh.link", ".wh.srv"] {
        assert!(names.iter().any(|n| n == want), "{want} not in {names:?}");
    }

    // 이미지 레이어를 합치는 것과 같은 병합.
    let mut content = Contents::default();
    let mut tree = MergedTree::new();
    for (i, t) in [&l1, &l2, &l3, &l4].into_iter().enumerate() {
        tree.apply_layer(read_layer(i, t, &mut content)).unwrap();
    }

    // 마지막 층으로 본 모습(게스트가 본 것)과 견준다.
    let mut seen = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for c in std::fs::read_dir(&d).unwrap() {
            let p = c.unwrap().path();
            let rel = p
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let m = std::fs::symlink_metadata(&p).unwrap();
            let node = tree
                .get(&rel)
                .unwrap_or_else(|| panic!("{rel} is missing from the merged layers"));
            assert_eq!((node.meta.uid, node.meta.gid), (m.uid(), m.gid()), "{rel}");
            match &node.kind {
                NodeKind::Dir => {
                    assert!(m.is_dir(), "{rel}");
                    assert_eq!(node.meta.mode, m.permissions().mode() & 0o7777, "{rel}");
                    stack.push(p.clone());
                }
                NodeKind::File(c) => {
                    assert!(m.is_file(), "{rel}");
                    assert_eq!(node.meta.mode, m.permissions().mode() & 0o7777, "{rel}");
                    let mut got = Vec::new();
                    content.open(c).unwrap().read_to_end(&mut got).unwrap();
                    assert_eq!(got, std::fs::read(&p).unwrap(), "{rel}");
                }
                NodeKind::Symlink(t) => {
                    assert_eq!(
                        std::fs::read_link(&p).unwrap().to_string_lossy(),
                        *t,
                        "{rel}"
                    );
                }
                other => panic!("{rel}: unexpected {other:?}"),
            }
            seen.push(rel);
        }
    }
    let merged: Vec<&str> = tree
        .iter()
        .map(|(p, _)| p)
        .filter(|p| !p.is_empty())
        .collect();
    seen.sort();
    assert_eq!(merged, seen);
    assert_eq!(
        merged,
        vec![
            "data",
            "data/d1",
            "data/d1/new",
            "etc",
            "etc/hosts",
            "lib",
            "lib/h2",
            "lib/target",
        ]
    );
    // 층 1의 하드 링크는 tar의 링크 항목이다. 층 4에서 h1을 지워도 h2는 내용과 함께 남는다(위에서 견주었다).
    let link = tar::Archive::new(l1.as_slice())
        .entries()
        .unwrap()
        .map(|e| e.unwrap())
        .find(|e| e.header().entry_type() == tar::EntryType::Link)
        .map(|e| {
            (
                String::from_utf8(e.path_bytes().into_owned()).unwrap(),
                String::from_utf8(e.link_name_bytes().unwrap().into_owned()).unwrap(),
            )
        });
    assert_eq!(link, Some(("lib/h2".into(), "lib/h1".into())));
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
}

#[test]
fn build_layers_need_a_running_sandbox_and_report_guest_errors() {
    let e = Env::new("layers-err");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    assert!(matches!(
        block_on(s.open_layer()),
        Err(RuntimeError::InvalidState { .. })
    ));
    let _main = block_on(s.start(IoMode::default())).unwrap();
    // 층을 열기 전에는 내보낼 것이 없다.
    match block_on(s.export_layer(Vec::new(), 1 << 20)) {
        Err(RuntimeError::Guest(g)) => assert!(g.message.contains("no layer"), "{g:?}"),
        other => panic!("{other:?}"),
    }
    // 한도를 넘는 층은 받지 않는다.
    assert_eq!(block_on(s.open_layer()).unwrap(), 1);
    let root = sbx_executor_fake::build_root(&e.rt.paths(s.id()).scratch());
    std::fs::write(root.join("big"), vec![7u8; 100]).unwrap();
    assert!(matches!(
        block_on(s.export_layer(Vec::new(), 99)),
        Err(RuntimeError::Invalid(_))
    ));
    block_on(s.stop(None, Duration::from_secs(5))).unwrap();
}
