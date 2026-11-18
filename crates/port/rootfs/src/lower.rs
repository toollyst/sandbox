//! 층 하나만으로 overlay 아래층(lower)을 만든다. 층 단위 저장(`sbx-store`의 층 디스크)이 쓴다.
//!
//! [`MergedTree`]는 모든 층을 합친 결과다. 이 모듈은 층 **하나**를 overlayfs가 아래층으로 읽는 모양의 트리로 바꾼다.
//! 그 층들을 overlay로 쌓아 본 모습이 같은 층들을 [`MergedTree`]로 합친 모습과 같아야 한다(테스트가 그것을 본다).
//!
//! 바꾸는 규칙:
//!
//! - whiteout(`.wh.<이름>`)은 그 경로의 문자 장치 0/0이 된다(overlayfs의 whiteout). 같은 층에 그 이름의
//!   디렉터리가 있으면 장치 대신 그 디렉터리를 opaque로 한다(아래층의 것을 지우고 이 층의 것만 남긴다는 뜻이 같다).
//!   같은 층에 그 이름의 디렉터리가 아닌 것이 있으면 whiteout은 할 일이 없다(위의 것이 아래를 가린다).
//!   조상이 이 층에서 디렉터리가 아니면(파일이 디렉터리를 바꿨으면) 건너뛴다.
//! - opaque(`.wh..wh..opq`)는 그 디렉터리의 xattr [`OPAQUE_XATTR`]` = "y"`가 된다.
//! - 층 안에서 디렉터리가 아닌 것이 놓였다가 그 자리에 디렉터리가 오면 그 디렉터리도 opaque다. 합치기에서는 앞의 것이
//!   아래층의 디렉터리를 지웠기 때문이다.
//! - 그 밖의 `.wh..wh.`로 시작하는 이름은 [`MergedTree`]처럼 무시한다.
//! - 층 tar에 있던 `trusted.overlay.*` xattr은 지운다. overlay가 읽는 표시를 이미지가 정하지 못하게 한다
//!   (overlay는 이 이름들을 사용자에게 보이지 않으므로 보이는 모습은 같다).
//! - 하드 링크는 같은 층 안의 대상과만 같은 inode가 된다.
//!
//! **혼자 설 수 없는 층** ([`LowerLayer::needs_lower`]): 아래층을 알아야 [`MergedTree`]와 같은 모습이 되는 층이다.
//! 부르는 쪽은 그 층을 디스크 하나로 두지 않고 아래층들과 함께 합친다(`sbx-store`의 층 구성).
//!
//! - 하드 링크의 대상이 이 층에 없다(아래층의 파일을 가리킨다).
//! - 층에 항목이 없는 디렉터리가 생겨야 한다(경로의 부모를 층이 적지 않았다). 합칠 때는 아래층의 그 디렉터리가
//!   메타데이터를 지키지만, overlay에서는 위층 디렉터리의 메타데이터가 이긴다. 그 디렉터리나 조상이 이 층에서
//!   whiteout되었거나 조상이 opaque면 아래층의 것이 지워지므로 괜찮다.
//! - 루트의 opaque. overlay는 아래층 루트의 opaque를 보지 않는다.
//!
//! 루트 디렉터리의 메타데이터는 맞추지 않는다. overlay 루트의 메타데이터는 위층(scratch)의 것이다.
//!
//! 문자 장치 0/0은 tar에 있던 것이든 whiteout이든 overlay가 같게(가려진 것으로) 본다. 지금의 합친 rootfs도
//! overlay의 아래층이므로 그런 장치는 전에도 보이지 않았다.
//!
//! 알려진 차이(Docker의 overlay2와 같다): 아래층 어디에도 없는 경로의 whiteout은 그 디렉터리가 이 층에만 있으면
//! 디렉터리 목록에 보이고 열리지 않는다. overlayfs가 한 층에만 있는 디렉터리는 그대로 읽기 때문이다.

use std::collections::BTreeSet;

use crate::{
    EntryKind, LayerEntry, MergedTree, Meta, Node, NodeKind, OPAQUE, RESERVED_PREFIX, RootfsError,
    WHITEOUT_PREFIX, join, normalize, split,
};

/// overlayfs가 opaque 디렉터리를 알아보는 xattr. 값은 `y`다.
pub const OPAQUE_XATTR: &str = "trusted.overlay.opaque";

/// overlayfs 내부용 xattr의 접두어. 층 tar에서 온 것은 지운다.
pub const OVERLAY_XATTR_PREFIX: &str = "trusted.overlay.";

/// 층 하나를 overlay 아래층 모양으로 바꾼 것.
#[derive(Debug, Clone)]
pub struct LowerLayer {
    /// overlay 아래층 모양의 트리. whiteout은 문자 장치 0/0, opaque는 [`OPAQUE_XATTR`].
    pub tree: MergedTree,
    /// 혼자 설 수 없으면 그 이유(모듈 문서). 그때 `tree`를 디스크로 쓰면 안 된다.
    pub needs_lower: Option<String>,
}

fn whiteout_node() -> Node {
    Node {
        kind: NodeKind::CharDevice { major: 0, minor: 0 },
        meta: Meta {
            mode: 0,
            ..Meta::implicit_dir()
        },
        link_group: None,
    }
}

/// `path`의 조상들(가까운 것부터, 루트 제외).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let mut cur = path;
    std::iter::from_fn(move || {
        let (parent, _) = cur.rsplit_once('/')?;
        cur = parent;
        Some(parent)
    })
}

impl MergedTree {
    fn is_non_dir(&self, path: &str) -> bool {
        self.nodes
            .get(path)
            .is_some_and(|n| n.kind != NodeKind::Dir)
    }

    fn set_opaque(&mut self, path: &str) {
        if let Some(n) = self.nodes.get_mut(path) {
            n.meta.xattrs.insert(OPAQUE_XATTR.into(), b"y".to_vec());
        }
    }
}

/// 층 하나를 overlay 아래층 모양으로 바꾼다. 규칙은 모듈 문서에 있다.
///
/// [`MergedTree::apply_layer`]가 오류로 보는 것(경로 탈출, 이름 없는 whiteout, 디렉터리가 아닌 부모)은 여기서도 오류다.
/// 대상이 이 층에 없는 하드 링크는 오류가 아니라 혼자 설 수 없는 층이다(아래층에 있을 수 있다).
pub fn lower_layer(entries: Vec<LayerEntry>) -> Result<LowerLayer, RootfsError> {
    let mut tree = MergedTree::new();
    let mut needs_lower: Option<String> = None;
    let mut need = |why: String| {
        needs_lower.get_or_insert(why);
    };
    let mut whiteouts: BTreeSet<String> = BTreeSet::new();
    let mut opaque: BTreeSet<String> = BTreeSet::new();
    // 층이 직접 적은 경로.
    let mut listed: BTreeSet<String> = BTreeSet::new();
    // 디렉터리가 아닌 것을 놓은 경로. 끝에 디렉터리면 opaque다.
    let mut replaced: BTreeSet<String> = BTreeSet::new();

    for entry in entries {
        let path = normalize(&entry.path)?;
        let (dir, name) = split(&path);
        if name == OPAQUE {
            opaque.insert(dir.to_owned());
            continue;
        }
        if name.starts_with(RESERVED_PREFIX) {
            continue;
        }
        if let Some(hidden) = name.strip_prefix(WHITEOUT_PREFIX) {
            if hidden.is_empty() {
                return Err(RootfsError::InvalidPath(path));
            }
            whiteouts.insert(join(dir, hidden));
            continue;
        }
        let mut meta = entry.meta;
        meta.xattrs
            .retain(|k, _| !k.starts_with(OVERLAY_XATTR_PREFIX));
        let mut kind = entry.kind;
        if let EntryKind::Hardlink(target) = &kind {
            let t = normalize(target)?;
            if !tree.nodes.contains_key(&t) {
                need(format!(
                    "the hard link {path:?} points into a lower layer ({t:?})"
                ));
                // 이 트리는 쓰지 않는다. 뒤의 항목이 합칠 때와 같게 다뤄지도록 디렉터리가 아닌 것을 둔다
                // (하드 링크의 대상은 디렉터리일 수 없다).
                kind = EntryKind::Fifo;
            }
        }
        if !matches!(kind, EntryKind::Dir) {
            replaced.insert(path.clone());
        }
        tree.insert(path.clone(), kind, meta)?;
        listed.insert(path);
    }
    for p in replaced {
        if tree.nodes.get(&p).is_some_and(|n| n.kind == NodeKind::Dir) {
            tree.set_opaque(&p);
            opaque.insert(p);
        }
    }

    // 아래층의 그 경로가 이 층에서 지워졌는지(그 자신이나 조상이 whiteout이거나, 조상이 opaque다).
    let fresh = |p: &str| {
        whiteouts.contains(p) || ancestors(p).any(|a| whiteouts.contains(a) || opaque.contains(a))
    };
    // 표시가 놓일 디렉터리가 층에 없고 아래층의 그것도 지워졌으면 표시는 할 일이 없다. 디렉터리를 만들면 합칠 때는
    // 없는 빈 디렉터리가 보이게 된다.
    let pointless = |tree: &MergedTree, dir: &str| !tree.nodes.contains_key(dir) && fresh(dir);

    for target in &whiteouts {
        if ancestors(target).any(|a| tree.is_non_dir(a)) || pointless(&tree, split(target).0) {
            continue;
        }
        match tree.nodes.get(target.as_str()).map(|n| &n.kind) {
            Some(NodeKind::Dir) => tree.set_opaque(target),
            Some(_) => {}
            None => {
                tree.ensure_parents(target)?;
                tree.nodes.insert(target.clone(), whiteout_node());
            }
        }
    }
    for dir in &opaque {
        if dir.is_empty() {
            need("the layer makes the root directory opaque".into());
            continue;
        }
        if tree.is_non_dir(dir)
            || ancestors(dir).any(|a| tree.is_non_dir(a))
            || pointless(&tree, dir)
        {
            continue;
        }
        if !tree.nodes.contains_key(dir.as_str()) {
            tree.ensure_parents(dir)?;
            tree.nodes.insert(
                dir.clone(),
                Node {
                    kind: NodeKind::Dir,
                    meta: Meta::implicit_dir(),
                    link_group: None,
                },
            );
        }
        tree.set_opaque(dir);
    }

    // 층에 항목이 없는 디렉터리: 아래층의 것이 지워졌을 때만 괜찮다.
    if let Some(p) = tree
        .nodes
        .iter()
        .find(|(p, n)| {
            !p.is_empty() && n.kind == NodeKind::Dir && !listed.contains(*p) && !fresh(p)
        })
        .map(|(p, _)| p.clone())
    {
        need(format!("the layer has no entry for the directory {p:?}"));
    }
    Ok(LowerLayer { tree, needs_lower })
}

#[cfg(test)]
mod tests;
