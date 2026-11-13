//! 층을 하나씩 바꿔 overlay로 쌓아 본 모습이 [`MergedTree`]로 합친 모습과 같은지 본다.
//!
//! overlay가 보이는 모습은 [`overlay_view`]가 overlayfs의 찾기 규칙대로 흉내 낸다(실제 커널로 보는 것은
//! `sbx-guest`의 privileged 테스트). 합친 모습([`visible`])은 지금의 rootfs를 overlay의 아래층으로 썼을 때 보이는 것이다.

use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::{ContentRef, EntryKind, LayerEntry, Meta, NodeKind};

#[cfg(all(feature = "erofs", target_os = "linux"))]
mod kernel;

fn meta(mode: u32) -> Meta {
    Meta {
        mode,
        uid: 1,
        gid: 2,
        mtime: 3,
        xattrs: BTreeMap::new(),
    }
}

fn entry(path: &str, kind: EntryKind, mode: u32) -> LayerEntry {
    LayerEntry {
        path: path.into(),
        kind,
        meta: meta(mode),
    }
}

fn dir(path: &str) -> LayerEntry {
    entry(path, EntryKind::Dir, 0o755)
}

fn file(path: &str, layer: usize, index: usize) -> LayerEntry {
    entry(
        path,
        EntryKind::File(ContentRef {
            layer,
            index,
            size: 1,
        }),
        0o644,
    )
}

/// whiteout과 opaque 표시(tar에서는 빈 일반 파일).
fn mark(path: &str) -> LayerEntry {
    file(path, 99, 0)
}

fn link(path: &str, target: &str) -> LayerEntry {
    entry(path, EntryKind::Hardlink(target.into()), 0o644)
}

fn is_whiteout(n: &Node) -> bool {
    n.kind == NodeKind::CharDevice { major: 0, minor: 0 }
}

fn is_opaque(n: &Node) -> bool {
    n.meta.xattrs.get(OPAQUE_XATTR).is_some_and(|v| v == b"y")
}

/// 비교할 모습: 경로 → (종류, overlay xattr을 뺀 메타데이터, 하드 링크 묶음의 대표 경로). 루트는 뺀다.
type View = BTreeMap<String, (NodeKind, Meta, Option<String>)>;

/// 보인 노드: 경로, 노드, 하드 링크 묶음(층 번호와 그 층의 묶음).
type Seen<'a> = Vec<(String, &'a Node, Option<(usize, u64)>)>;

fn shown_meta(m: &Meta) -> Meta {
    let mut m = m.clone();
    m.xattrs.retain(|k, _| !k.starts_with(OVERLAY_XATTR_PREFIX));
    m
}

/// 하드 링크 묶음(아무 키)마다 가장 작은 경로를 대표로 쓴다.
fn with_groups<K: Ord + Clone>(nodes: Vec<(String, &Node, Option<K>)>) -> View {
    let mut first: BTreeMap<K, String> = BTreeMap::new();
    for (p, _, g) in &nodes {
        if let Some(g) = g {
            let e = first.entry(g.clone()).or_insert_with(|| p.clone());
            if p < e {
                *e = p.clone();
            }
        }
    }
    nodes
        .into_iter()
        .map(|(p, n, g)| {
            let rep = g.map(|g| first[&g].clone());
            (p, (n.kind.clone(), shown_meta(&n.meta), rep))
        })
        .collect()
}

/// 모든 층을 합친 rootfs 하나를 overlay 아래층으로 썼을 때 보이는 모습(문자 장치 0/0은 가려진다).
fn visible(t: &MergedTree) -> View {
    with_groups(
        t.iter()
            .filter(|(p, n)| !p.is_empty() && !is_whiteout(n))
            .map(|(p, n)| (p.to_owned(), n, n.link_group))
            .collect(),
    )
}

fn children<'a>(t: &'a MergedTree, dir: &str) -> Vec<&'a str> {
    t.iter()
        .filter(|(p, _)| !p.is_empty() && split(p).0 == dir)
        .map(|(p, _)| split(p).1)
        .collect()
}

/// `layers`(아래부터)를 overlayfs처럼 쌓아 본 모습. 이름 찾기는 위층부터 하고, whiteout이면 없고, 디렉터리가
/// 아니면 그것이다. 디렉터리면 아래층의 같은 디렉터리와 합치되, opaque거나 아래층에 디렉터리가 아닌 것이 있으면
/// 거기서 멈춘다. 메타데이터는 찾은 맨 위의 것이다.
fn overlay_view(layers: &[&MergedTree]) -> View {
    fn walk<'a>(layers: &[&'a MergedTree], path: &str, stack: &[usize], out: &mut Seen<'a>) {
        let names: BTreeSet<&str> = stack
            .iter()
            .flat_map(|&l| children(layers[l], path))
            .collect();
        for name in names {
            let child = join(path, name);
            let Some(pos) = stack.iter().position(|&l| layers[l].get(&child).is_some()) else {
                continue;
            };
            let top = stack[pos];
            let node = layers[top].get(&child).unwrap();
            if is_whiteout(node) {
                continue;
            }
            out.push((child.clone(), node, node.link_group.map(|g| (top, g))));
            if node.kind != NodeKind::Dir {
                continue;
            }
            let mut below = vec![top];
            if !is_opaque(node) {
                for &l in &stack[pos + 1..] {
                    match layers[l].get(&child) {
                        None => continue,
                        Some(n) if n.kind == NodeKind::Dir => {
                            below.push(l);
                            if is_opaque(n) {
                                break;
                            }
                        }
                        Some(_) => break,
                    }
                }
            }
            walk(layers, &child, &below, out);
        }
    }
    let stack: Vec<usize> = (0..layers.len()).rev().collect();
    let mut out = Vec::new();
    walk(layers, "", &stack, &mut out);
    with_groups(out)
}

fn merged(layers: &[Vec<LayerEntry>]) -> Result<MergedTree, RootfsError> {
    let mut t = MergedTree::new();
    for l in layers {
        t.apply_layer(l.clone())?;
    }
    Ok(t)
}

/// 층 구성 규칙(`sbx-store`)대로 쌓은 모습: 아래 `merge_through`번 층까지 합친 것 하나, 그 위로 층마다 하나.
/// 혼자 설 수 없는 층은 합친 쪽에 들어가야 한다.
fn stacked(layers: &[Vec<LayerEntry>], merge_through: Option<usize>) -> View {
    let mut parts: Vec<MergedTree> = Vec::new();
    let upper_from = match merge_through {
        Some(k) => {
            parts.push(merged(&layers[..=k]).unwrap());
            k + 1
        }
        None => 0,
    };
    for l in &layers[upper_from..] {
        let lower = lower_layer(l.clone()).unwrap();
        assert!(lower.needs_lower.is_none(), "{:?}", lower.needs_lower);
        parts.push(lower.tree);
    }
    let refs: Vec<&MergedTree> = parts.iter().collect();
    overlay_view(&refs)
}

fn assert_same(layers: &[Vec<LayerEntry>]) {
    let want = visible(&merged(layers).unwrap());
    assert_eq!(stacked(layers, None), want);
}

#[test]
fn whiteout_becomes_a_character_device_and_hides_the_lower_file() {
    let layers = vec![
        vec![dir("etc"), file("etc/a", 0, 0), file("etc/b", 0, 1)],
        vec![dir("etc"), mark("etc/.wh.a")],
    ];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert!(upper.needs_lower.is_none());
    assert!(is_whiteout(upper.tree.get("etc/a").unwrap()));
    assert_same(&layers);
}

#[test]
fn whiteout_with_a_new_directory_of_that_name_makes_it_opaque() {
    let layers = vec![
        vec![dir("d"), file("d/old", 0, 0)],
        vec![mark(".wh.d"), dir("d"), file("d/new", 1, 0)],
    ];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert!(is_opaque(upper.tree.get("d").unwrap()));
    assert_same(&layers);
    // 디렉터리 항목이 없어도(whiteout이 아래층의 것을 지우므로) 혼자 선다.
    let layers = vec![
        vec![dir("d"), file("d/old", 0, 0)],
        vec![mark(".wh.d"), file("d/new", 1, 0)],
    ];
    assert!(
        lower_layer(layers[1].clone())
            .unwrap()
            .needs_lower
            .is_none()
    );
    assert_same(&layers);
}

#[test]
fn whiteout_with_a_file_of_that_name_leaves_the_file() {
    let layers = vec![vec![file("a", 0, 0)], vec![file("a", 1, 0), mark(".wh.a")]];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert_eq!(
        upper.tree.get("a").unwrap().kind,
        NodeKind::File(ContentRef {
            layer: 1,
            index: 0,
            size: 1
        })
    );
    assert_same(&layers);
}

#[test]
fn opaque_marker_becomes_the_xattr() {
    let layers = vec![
        vec![dir("d"), file("d/old", 0, 0), dir("d/sub")],
        vec![dir("d"), file("d/new", 1, 0), mark("d/.wh..wh..opq")],
    ];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert!(is_opaque(upper.tree.get("d").unwrap()));
    assert!(upper.tree.get("d/.wh..wh..opq").is_none());
    assert_same(&layers);
}

#[test]
fn a_directory_the_layer_does_not_list_needs_the_lower_layers() {
    let layers = vec![
        vec![entry("tmp", EntryKind::Dir, 0o1777)],
        vec![file("tmp/x", 1, 0)],
    ];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert!(
        upper.needs_lower.as_deref().unwrap().contains("\"tmp\""),
        "{:?}",
        upper.needs_lower
    );
    // 합치면 아래층의 /tmp 메타데이터가 남는다. 따로 쌓으면 그것을 잃는다(그래서 합친다).
    assert_eq!(
        merged(&layers).unwrap().get("tmp").unwrap().meta.mode,
        0o1777
    );
    assert_eq!(
        stacked(&layers, Some(1)),
        visible(&merged(&layers).unwrap())
    );
    // whiteout의 부모도 같다.
    assert!(
        lower_layer(vec![mark("etc/.wh.x")])
            .unwrap()
            .needs_lower
            .is_some()
    );
    // opaque인 부모 아래라면 괜찮다.
    let alone = lower_layer(vec![dir("d"), mark("d/.wh..wh..opq"), file("d/e/f", 1, 0)]).unwrap();
    assert!(alone.needs_lower.is_none(), "{:?}", alone.needs_lower);
}

#[test]
fn hard_links_into_a_lower_layer_need_the_lower_layers() {
    let layers = [vec![file("a", 0, 0)], vec![link("b", "a")]];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert!(upper.needs_lower.unwrap().contains("hard link"));
    // 같은 층 안의 하드 링크는 한 inode다.
    let same = lower_layer(vec![file("a", 0, 0), link("b", "a")]).unwrap();
    assert!(same.needs_lower.is_none());
    let (a, b) = (same.tree.get("a").unwrap(), same.tree.get("b").unwrap());
    assert!(a.link_group.is_some() && a.link_group == b.link_group);
}

#[test]
fn overlay_xattrs_from_the_image_are_dropped() {
    let mut d = dir("d");
    d.meta
        .xattrs
        .insert("trusted.overlay.redirect".into(), b"/x".to_vec());
    d.meta.xattrs.insert("user.keep".into(), b"1".to_vec());
    let l = lower_layer(vec![d]).unwrap();
    let x = &l.tree.get("d").unwrap().meta.xattrs;
    assert_eq!(x.keys().collect::<Vec<_>>(), ["user.keep"]);
}

#[test]
fn opaque_root_needs_the_lower_layers() {
    let l = lower_layer(vec![mark(".wh..wh..opq"), file("x", 0, 0)]).unwrap();
    assert!(l.needs_lower.unwrap().contains("root"));
}

#[test]
fn whiteouts_under_a_replaced_directory_are_skipped() {
    // 같은 층에서 d가 파일이 되었으면 d 아래의 whiteout은 할 일이 없다.
    let layers = vec![
        vec![dir("d"), file("d/x", 0, 0)],
        vec![file("d", 1, 0), mark("d/.wh.x")],
    ];
    let upper = lower_layer(layers[1].clone()).unwrap();
    assert!(upper.needs_lower.is_none());
    assert_same(&layers);
}

#[test]
fn same_errors_as_merging() {
    assert!(matches!(
        lower_layer(vec![mark("../x")]),
        Err(RootfsError::PathTraversal(_))
    ));
    assert!(matches!(
        lower_layer(vec![mark("a/.wh.")]),
        Err(RootfsError::InvalidPath(_))
    ));
    assert!(matches!(
        lower_layer(vec![file("a", 0, 0), file("a/b", 0, 1)]),
        Err(RootfsError::NotADirectory(_))
    ));
}

/// 결정적인 의사 난수(xorshift). 외부 크레이트 없이 많은 경우를 만든다.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn random_path(r: &mut Rng) -> String {
    let depth = 1 + r.below(3);
    (0..depth)
        .map(|_| ["a", "b", "c"][r.below(3) as usize])
        .collect::<Vec<_>>()
        .join("/")
}

fn random_layer(r: &mut Rng, layer: usize, files: &mut Vec<String>) -> Vec<LayerEntry> {
    let mut out = Vec::new();
    for _ in 0..1 + r.below(6) {
        let p = random_path(r);
        // 대개는 Docker처럼 부모 디렉터리 항목도 적는다.
        if r.chance(75) {
            let mut anc: Vec<&str> = ancestors(&p).collect();
            anc.reverse();
            for a in anc {
                let mut d = dir(a);
                d.meta.mode = 0o700 + r.below(0o70) as u32;
                out.push(d);
            }
        }
        let (parent, name) = split(&p);
        let index = out.len();
        match r.below(9) {
            0 | 1 => {
                let mut d = dir(&p);
                d.meta.mtime = r.below(5) as i64;
                out.push(d);
            }
            2 | 3 => {
                out.push(file(&p, layer, index));
                files.push(p);
            }
            4 => out.push(mark(&join(parent, &format!(".wh.{name}")))),
            5 => out.push(mark(&join(parent, ".wh..wh..opq"))),
            6 if !files.is_empty() => {
                let t = files[r.below(files.len() as u64) as usize].clone();
                out.push(link(&p, &t));
            }
            7 => out.push(entry(&p, EntryKind::Symlink("a".into()), 0o777)),
            8 => out.push(entry(&p, EntryKind::CharDevice { major: 0, minor: 0 }, 0)),
            _ => out.push(entry(&p, EntryKind::Fifo, 0o600)),
        }
    }
    out
}

/// 실패했을 때 보일 짧은 모양.
fn brief(layers: &[Vec<LayerEntry>]) -> String {
    let mut out = String::new();
    for (i, l) in layers.iter().enumerate() {
        out.push_str(&format!("layer {i}:\n"));
        for e in l {
            let k = match &e.kind {
                EntryKind::Dir => "dir".to_owned(),
                EntryKind::File(_) => "file".to_owned(),
                EntryKind::Hardlink(t) => format!("link -> {t}"),
                other => format!("{other:?}"),
            };
            out.push_str(&format!("  {} {k} {:o}\n", e.path, e.meta.mode));
        }
    }
    out
}

/// 많은 무작위 층 묶음에서: 혼자 설 수 없는 층의 위로는 층마다 하나씩 쌓고, 그 아래는 합친다. 어디서부터 합쳐도
/// (디스크 수 상한 때문에 더 아래까지 합쳐도) 보이는 모습이 모든 층을 합친 것과 같다.
#[test]
fn stacking_lower_layers_matches_merging_on_random_layers() {
    let mut r = Rng(0x5eed_1234_abcd_0001);
    let (mut checked, mut alone_layers, mut merged_layers) = (0, 0, 0);
    for _ in 0..4000 {
        let n = 1 + r.below(4) as usize;
        let mut files = Vec::new();
        let layers: Vec<Vec<LayerEntry>> = (0..n)
            .map(|l| random_layer(&mut r, l, &mut files))
            .collect();
        let Ok(all) = merged(&layers) else { continue };
        let lowers: Vec<_> = layers.iter().map(|l| lower_layer(l.clone())).collect();
        if lowers.iter().any(Result::is_err) {
            // 합치기는 되는데 층 하나로는 안 되는 경우는 없어야 한다.
            panic!(
                "lower_layer failed where merging works:\n{}",
                brief(&layers)
            );
        }
        let highest = lowers
            .iter()
            .rposition(|l| l.as_ref().unwrap().needs_lower.is_some());
        alone_layers += n - highest.map_or(0, |h| h + 1);
        merged_layers += highest.map_or(0, |h| h + 1);
        let want = visible(&all);
        assert_eq!(stacked(&layers, highest), want, "{}", brief(&layers));
        let deeper = highest.map_or(0, |h| h + 1) + r.below(n as u64) as usize;
        if deeper < n {
            assert_eq!(stacked(&layers, Some(deeper)), want, "{}", brief(&layers));
        }
        checked += 1;
    }
    assert!(checked > 2000, "{checked}");
    assert!(
        alone_layers > 1000 && merged_layers > 500,
        "{alone_layers} {merged_layers}"
    );
}
