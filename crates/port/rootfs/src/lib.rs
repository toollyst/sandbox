//! 이미지 레이어를 합쳐 rootfs 디스크 이미지를 만든다.
//!
//! 두 단계로 나뉜다.
//!
//! 1. **병합**: 레이어 항목을 아래 레이어부터 순서대로 적용해 [`MergedTree`]를 만든다.
//!    모든 형식이 함께 쓰는 단계다. 규칙은 OCI Image Spec의 layer 변경 규칙을 따른다.
//! 2. **쓰기**: [`RootfsBuilder`]가 병합된 트리를 형식별 디스크 이미지로 쓴다.
//!    형식별 구현은 cargo feature로 붙인다. 지금 있는 것은 `erofs`다.
//!
//! **호스트 파일시스템에 풀지 않는다.** 경로는 OS 경로(`Path`)가 아니라 `/`로 나눈 문자열로만 다룬다.
//! macOS와 Windows의 파일시스템은 대소문자를 구분하지 않고, 일반 사용자 권한으로는
//! 소유자와 장치 파일을 재현할 수 없기 때문이다.
//!
//! 병합 규칙:
//!
//! - whiteout(`.wh.<이름>`)은 아래 레이어의 그 경로와 하위 전체를 지운다.
//! - opaque(`.wh..wh..opq`)는 아래 레이어에 있던 그 디렉터리의 하위 전체를 지운다.
//! - whiteout과 opaque는 **같은 레이어의 항목을 지우지 않는다**. 그래서 한 레이어 안에서는
//!   whiteout을 먼저 적용하고 나머지 항목을 나중에 적용한다.
//! - 그 밖의 `.wh..wh.`로 시작하는 이름(AUFS 내부용)은 무시한다.
//! - 디렉터리 위에 디렉터리가 오면 메타데이터만 바꾸고 하위는 유지한다.
//! - 디렉터리가 아닌 것이 디렉터리를 대체하면 하위 전체가 지워진다.
//! - 부모가 없으면 기본 디렉터리(0755, root)를 만든다.
//! - 부모가 디렉터리가 아니면(심볼릭 링크 포함) 오류다. 심볼릭 링크를 따라가지 않는다.
//! - `..`가 들어간 경로는 오류다.
//! - 하드 링크는 대상과 같은 노드가 되고 같은 `link_group`을 갖는다. 나중에 대상 이름이
//!   지워져도 링크는 남는다.
//!
//! - 의존: `sbx-model`

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Seek, Write};

use sbx_model::RootfsFormat;

#[cfg(feature = "erofs")]
pub mod erofs;

const WHITEOUT_PREFIX: &str = ".wh.";
const OPAQUE: &str = ".wh..wh..opq";
const RESERVED_PREFIX: &str = ".wh..wh.";

/// 파일 메타데이터.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// 권한 비트(파일 종류 비트 제외).
    pub mode: u32,
    /// 소유자 uid.
    pub uid: u32,
    /// 소유자 gid.
    pub gid: u32,
    /// 수정 시각(Unix 초).
    pub mtime: i64,
    /// 확장 속성.
    pub xattrs: BTreeMap<String, Vec<u8>>,
}

impl Meta {
    /// 부모가 없어서 만드는 디렉터리의 메타데이터: 0755, root, 시각 0.
    pub fn implicit_dir() -> Self {
        Self {
            mode: 0o755,
            uid: 0,
            gid: 0,
            mtime: 0,
            xattrs: BTreeMap::new(),
        }
    }
}

/// 파일 내용의 위치. 레이어 번호와 그 레이어 안의 항목 순번이다.
///
/// 내용을 실제로 어떻게 다시 얻을지는 [`ContentSource`]가 정한다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRef {
    /// 레이어 번호(아래부터 0).
    pub layer: usize,
    /// 레이어 tar 안에서의 항목 순번.
    pub index: usize,
    /// 내용 크기(바이트).
    pub size: u64,
}

/// 레이어 항목의 종류.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// 디렉터리.
    Dir,
    /// 일반 파일.
    File(ContentRef),
    /// 심볼릭 링크. 대상 문자열은 해석하지 않는다.
    Symlink(String),
    /// 하드 링크. 대상은 rootfs 안의 경로다.
    Hardlink(String),
    /// 문자 장치.
    CharDevice {
        /// major 번호.
        major: u32,
        /// minor 번호.
        minor: u32,
    },
    /// 블록 장치.
    BlockDevice {
        /// major 번호.
        major: u32,
        /// minor 번호.
        minor: u32,
    },
    /// FIFO.
    Fifo,
}

/// 레이어 tar에서 읽은 항목 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerEntry {
    /// tar에 적힌 경로 그대로.
    pub path: String,
    /// 종류.
    pub kind: EntryKind,
    /// 메타데이터.
    pub meta: Meta,
}

/// 병합된 트리의 노드 종류. 하드 링크는 풀려서 대상과 같은 종류가 된다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    /// 디렉터리.
    Dir,
    /// 일반 파일.
    File(ContentRef),
    /// 심볼릭 링크.
    Symlink(String),
    /// 문자 장치.
    CharDevice {
        /// major 번호.
        major: u32,
        /// minor 번호.
        minor: u32,
    },
    /// 블록 장치.
    BlockDevice {
        /// major 번호.
        major: u32,
        /// minor 번호.
        minor: u32,
    },
    /// FIFO.
    Fifo,
}

/// 병합된 트리의 노드.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// 종류.
    pub kind: NodeKind,
    /// 메타데이터.
    pub meta: Meta,
    /// 같은 값을 가진 노드는 같은 inode다(하드 링크).
    pub link_group: Option<u64>,
}

/// 병합 오류.
#[derive(Debug)]
pub enum RootfsError {
    /// `..`로 rootfs 밖을 가리키는 경로.
    PathTraversal(String),
    /// 쓸 수 없는 경로.
    InvalidPath(String),
    /// 부모가 디렉터리가 아님.
    NotADirectory(String),
    /// 하드 링크 대상이 없거나 디렉터리임.
    HardlinkTarget {
        /// 링크 경로.
        link: String,
        /// 대상 경로.
        target: String,
    },
    /// 이 형식으로 표현할 수 없는 내용.
    Unsupported(String),
    /// 입출력 오류.
    Io(io::Error),
}

impl fmt::Display for RootfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RootfsError::PathTraversal(p) => write!(f, "path escapes rootfs: {p:?}"),
            RootfsError::InvalidPath(p) => write!(f, "invalid path: {p:?}"),
            RootfsError::NotADirectory(p) => write!(f, "parent is not a directory: {p:?}"),
            RootfsError::HardlinkTarget { link, target } => {
                write!(
                    f,
                    "hardlink {link:?} points to missing or directory {target:?}"
                )
            }
            RootfsError::Unsupported(m) => write!(f, "unsupported: {m}"),
            RootfsError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for RootfsError {}

impl From<io::Error> for RootfsError {
    fn from(e: io::Error) -> Self {
        RootfsError::Io(e)
    }
}

/// `/`로 나눈 경로를 정규화한다. 루트는 빈 문자열이다.
///
/// 앞의 `/`와 `./`, 중간의 `.`과 빈 칸은 지운다. `..`는 오류다.
pub fn normalize(raw: &str) -> Result<String, RootfsError> {
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err(RootfsError::PathTraversal(raw.to_owned())),
            p if p.contains('\0') => return Err(RootfsError::InvalidPath(raw.to_owned())),
            p => parts.push(p),
        }
    }
    Ok(parts.join("/"))
}

/// 정규화된 경로를 (부모, 이름)으로 나눈다. 루트는 ("", "").
fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", path),
    }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

/// [`MergedTree::resolve`]가 따라가는 심볼릭 링크의 최대 수 (Linux `MAXSYMLINKS`와 같다).
pub const MAX_SYMLINK_HOPS: usize = 40;

/// 레이어를 합친 결과.
#[derive(Debug, Clone)]
pub struct MergedTree {
    nodes: BTreeMap<String, Node>,
    next_link_group: u64,
}

impl Default for MergedTree {
    fn default() -> Self {
        Self::new()
    }
}

impl MergedTree {
    /// 루트 디렉터리만 있는 트리.
    pub fn new() -> Self {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            String::new(),
            Node {
                kind: NodeKind::Dir,
                meta: Meta::implicit_dir(),
                link_group: None,
            },
        );
        Self {
            nodes,
            next_link_group: 0,
        }
    }

    /// 경로의 노드. 루트는 빈 문자열이다.
    pub fn get(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path)
    }

    /// 경로를 이 트리 안에서 풀어 실제 노드의 경로를 돌려준다. 심볼릭 링크는 중간 경로와 마지막 모두
    /// 이 트리를 루트로 삼아 따라간다(절대 대상은 트리 루트 기준, `..`는 루트에서 멈춘다. chroot와 같다).
    /// 없는 경로이거나 링크를 [`MAX_SYMLINK_HOPS`]번 넘게 따라가면 `None`이다.
    pub fn resolve(&self, path: &str) -> Option<String> {
        let mut pending: Vec<String> = path.split('/').rev().map(str::to_owned).collect();
        let mut cur: Vec<String> = Vec::new();
        let mut hops = 0;
        while let Some(part) = pending.pop() {
            match part.as_str() {
                "" | "." => continue,
                ".." => {
                    cur.pop();
                    continue;
                }
                _ => cur.push(part),
            }
            let node = self.nodes.get(&cur.join("/"))?;
            if let NodeKind::Symlink(target) = &node.kind {
                hops += 1;
                if hops > MAX_SYMLINK_HOPS {
                    return None;
                }
                cur.pop();
                if target.starts_with('/') {
                    cur.clear();
                }
                pending.extend(target.split('/').rev().map(str::to_owned));
            }
        }
        Some(cur.join("/"))
    }

    /// 모든 노드를 경로 순서로.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Node)> {
        self.nodes.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// 노드 개수(루트 포함).
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// 루트만 있는지.
    pub fn is_empty(&self) -> bool {
        self.nodes.len() == 1
    }

    /// 레이어 하나를 적용한다. 모듈 문서의 병합 규칙을 따른다.
    pub fn apply_layer(&mut self, entries: Vec<LayerEntry>) -> Result<(), RootfsError> {
        let mut normalized = Vec::with_capacity(entries.len());
        for entry in entries {
            normalized.push((normalize(&entry.path)?, entry));
        }

        // 1. whiteout과 opaque: 아래 레이어에만 적용된다.
        for (path, _) in &normalized {
            let (dir, name) = split(path);
            if name == OPAQUE {
                self.remove_descendants(dir);
            } else if name.starts_with(RESERVED_PREFIX) {
                // AUFS 내부용 이름. 무시한다.
            } else if let Some(hidden) = name.strip_prefix(WHITEOUT_PREFIX) {
                if hidden.is_empty() {
                    return Err(RootfsError::InvalidPath(path.clone()));
                }
                self.remove_subtree(&join(dir, hidden));
            }
        }

        // 2. 나머지 항목.
        for (path, entry) in normalized {
            if split(&path).1.starts_with(WHITEOUT_PREFIX) {
                continue;
            }
            self.insert(path, entry.kind, entry.meta)?;
        }
        Ok(())
    }

    fn insert(&mut self, path: String, kind: EntryKind, meta: Meta) -> Result<(), RootfsError> {
        if path.is_empty() {
            // 루트 항목("./")은 메타데이터만 바꾼다.
            return match kind {
                EntryKind::Dir => {
                    if let Some(root) = self.nodes.get_mut("") {
                        root.meta = meta;
                    }
                    Ok(())
                }
                _ => Err(RootfsError::InvalidPath("/".into())),
            };
        }

        self.ensure_parents(&path)?;

        let node = match kind {
            EntryKind::Hardlink(target) => self.link_to(&path, &target)?,
            EntryKind::Dir => Node {
                kind: NodeKind::Dir,
                meta,
                link_group: None,
            },
            EntryKind::File(c) => Node {
                kind: NodeKind::File(c),
                meta,
                link_group: None,
            },
            EntryKind::Symlink(t) => Node {
                kind: NodeKind::Symlink(t),
                meta,
                link_group: None,
            },
            EntryKind::CharDevice { major, minor } => Node {
                kind: NodeKind::CharDevice { major, minor },
                meta,
                link_group: None,
            },
            EntryKind::BlockDevice { major, minor } => Node {
                kind: NodeKind::BlockDevice { major, minor },
                meta,
                link_group: None,
            },
            EntryKind::Fifo => Node {
                kind: NodeKind::Fifo,
                meta,
                link_group: None,
            },
        };

        let existing_is_dir = matches!(self.nodes.get(&path).map(|n| &n.kind), Some(NodeKind::Dir));
        if existing_is_dir {
            if node.kind == NodeKind::Dir {
                // 디렉터리 위 디렉터리: 메타데이터만 바꾸고 하위는 유지한다.
                if let Some(existing) = self.nodes.get_mut(&path) {
                    existing.meta = node.meta;
                }
                return Ok(());
            }
            self.remove_descendants(&path);
        }
        self.nodes.insert(path, node);
        Ok(())
    }

    fn link_to(&mut self, link: &str, raw_target: &str) -> Result<Node, RootfsError> {
        let target = normalize(raw_target)?;
        let next = self.next_link_group;
        let bad_target = || RootfsError::HardlinkTarget {
            link: link.to_owned(),
            target: raw_target.to_owned(),
        };
        let node = self.nodes.get_mut(&target).ok_or_else(bad_target)?;
        if node.kind == NodeKind::Dir {
            return Err(bad_target());
        }
        let group = match node.link_group {
            Some(g) => g,
            None => {
                node.link_group = Some(next);
                next
            }
        };
        let linked = Node {
            kind: node.kind.clone(),
            meta: node.meta.clone(),
            link_group: Some(group),
        };
        if group == next {
            self.next_link_group += 1;
        }
        Ok(linked)
    }

    fn ensure_parents(&mut self, path: &str) -> Result<(), RootfsError> {
        let mut end = 0;
        while let Some(pos) = path[end..].find('/') {
            let parent = &path[..end + pos];
            match self.nodes.get(parent).map(|n| &n.kind) {
                None => {
                    self.nodes.insert(
                        parent.to_owned(),
                        Node {
                            kind: NodeKind::Dir,
                            meta: Meta::implicit_dir(),
                            link_group: None,
                        },
                    );
                }
                Some(NodeKind::Dir) => {}
                Some(_) => return Err(RootfsError::NotADirectory(parent.to_owned())),
            }
            end += pos + 1;
        }
        Ok(())
    }

    /// 경로 자신과 하위 전체를 지운다.
    fn remove_subtree(&mut self, path: &str) {
        if path.is_empty() {
            self.remove_descendants("");
            return;
        }
        self.nodes.remove(path);
        self.remove_descendants(path);
    }

    /// 하위 전체를 지운다. 경로 자신은 남긴다.
    fn remove_descendants(&mut self, path: &str) {
        let keys: Vec<String> = if path.is_empty() {
            self.nodes
                .keys()
                .filter(|k| !k.is_empty())
                .cloned()
                .collect()
        } else {
            // '0'은 '/' 바로 다음 문자다. "a/".."a0" 범위가 정확히 "a"의 하위다.
            self.nodes
                .range(format!("{path}/")..format!("{path}0"))
                .map(|(k, _)| k.clone())
                .collect()
        };
        for k in keys {
            self.nodes.remove(&k);
        }
    }
}

/// 파일 내용을 다시 얻는 방법.
///
/// 원래 레이어 blob을 다시 읽을 수도 있고, 병합할 때 내용 해시를 이름으로 임시 저장해 둘 수도 있다.
/// 어느 쪽이든 호스트 파일 이름이 원래 경로와 무관해야 한다.
pub trait ContentSource {
    /// 내용을 연다.
    fn open(&self, content: &ContentRef) -> io::Result<Box<dyn Read + '_>>;
}

/// [`RootfsBuilder`]의 출력.
pub trait WriteSeek: Write + Seek {}
impl<T: Write + Seek> WriteSeek for T {}

/// rootfs 형식 하나의 writer.
///
/// 출력은 결정적이어야 한다. 같은 트리와 같은 내용이면 같은 바이트가 나와야 한다.
pub trait RootfsBuilder: Send + Sync {
    /// 이 writer가 만드는 형식.
    fn format(&self) -> RootfsFormat;

    /// 병합된 트리를 디스크 이미지로 쓴다.
    fn build(
        &self,
        tree: &MergedTree,
        content: &dyn ContentSource,
        out: &mut dyn WriteSeek,
    ) -> Result<(), RootfsError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(mode: u32) -> Meta {
        Meta {
            mode,
            ..Meta::implicit_dir()
        }
    }

    fn dir(path: &str) -> LayerEntry {
        LayerEntry {
            path: path.into(),
            kind: EntryKind::Dir,
            meta: meta(0o755),
        }
    }

    fn file(path: &str, layer: usize, index: usize) -> LayerEntry {
        LayerEntry {
            path: path.into(),
            kind: EntryKind::File(ContentRef {
                layer,
                index,
                size: 1,
            }),
            meta: meta(0o644),
        }
    }

    fn whiteout(path: &str) -> LayerEntry {
        file(path, 0, 0)
    }

    fn symlink(path: &str, target: &str) -> LayerEntry {
        LayerEntry {
            path: path.into(),
            kind: EntryKind::Symlink(target.into()),
            meta: meta(0o777),
        }
    }

    #[test]
    fn resolve_follows_symlinks_inside_the_tree() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![
            dir("usr"),
            dir("usr/lib"),
            dir("boot"),
            file("boot/vmlinuz-6.8", 0, 0),
            symlink("boot/vmlinuz", "vmlinuz-6.8"),
            symlink("vmlinuz", "boot/vmlinuz"),
            symlink("lib", "usr/lib"),
            file("usr/lib/k", 0, 1),
            symlink("up", "../../../boot/vmlinuz"),
            symlink("abs", "/lib/k"),
            symlink("loop", "loop"),
            symlink("dangling", "nope"),
        ])
        .unwrap();
        assert_eq!(t.resolve("/vmlinuz").as_deref(), Some("boot/vmlinuz-6.8"));
        assert_eq!(t.resolve("/lib/k").as_deref(), Some("usr/lib/k"));
        assert_eq!(t.resolve("up").as_deref(), Some("boot/vmlinuz-6.8"));
        assert_eq!(t.resolve("/abs").as_deref(), Some("usr/lib/k"));
        assert_eq!(
            t.resolve("/boot/./vmlinuz-6.8").as_deref(),
            Some("boot/vmlinuz-6.8")
        );
        assert_eq!(t.resolve("/").as_deref(), Some(""));
        assert_eq!(t.resolve("/loop"), None);
        assert_eq!(t.resolve("/dangling"), None);
        assert_eq!(t.resolve("/boot/vmlinuz-6.8/x"), None);
    }

    fn paths(t: &MergedTree) -> Vec<&str> {
        t.iter().map(|(p, _)| p).filter(|p| !p.is_empty()).collect()
    }

    fn content_layer(t: &MergedTree, path: &str) -> usize {
        match t.get(path).unwrap().kind {
            NodeKind::File(c) => c.layer,
            _ => panic!("{path} is not a file"),
        }
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("./etc/passwd").unwrap(), "etc/passwd");
        assert_eq!(normalize("/etc//passwd").unwrap(), "etc/passwd");
        assert_eq!(normalize("etc/./passwd").unwrap(), "etc/passwd");
        assert_eq!(normalize("./").unwrap(), "");
        assert!(matches!(
            normalize("../etc"),
            Err(RootfsError::PathTraversal(_))
        ));
        assert!(matches!(
            normalize("a/../../etc"),
            Err(RootfsError::PathTraversal(_))
        ));
    }

    #[test]
    fn upper_layer_replaces_file() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![dir("etc"), file("etc/hosts", 0, 1)])
            .unwrap();
        t.apply_layer(vec![file("etc/hosts", 1, 0)]).unwrap();
        assert_eq!(content_layer(&t, "etc/hosts"), 1);
    }

    #[test]
    fn whiteout_removes_lower_subtree() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![dir("a"), file("a/x", 0, 1), file("ab", 0, 2)])
            .unwrap();
        t.apply_layer(vec![whiteout(".wh.a")]).unwrap();
        assert_eq!(paths(&t), vec!["ab"]);
    }

    #[test]
    fn whiteout_does_not_hide_same_layer_entry() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![file("a", 0, 0)]).unwrap();
        // 같은 레이어에 whiteout과 새 파일이 함께 있으면 새 파일이 남는다.
        t.apply_layer(vec![file("a", 1, 0), whiteout(".wh.a")])
            .unwrap();
        assert_eq!(content_layer(&t, "a"), 1);
    }

    #[test]
    fn opaque_clears_lower_children_only() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![dir("d"), file("d/old", 0, 1), dir("d/sub")])
            .unwrap();
        t.apply_layer(vec![
            dir("d"),
            file("d/new", 1, 1),
            whiteout("d/.wh..wh..opq"),
        ])
        .unwrap();
        assert_eq!(paths(&t), vec!["d", "d/new"]);
    }

    #[test]
    fn reserved_whiteout_names_are_ignored() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![file("x", 0, 0)]).unwrap();
        t.apply_layer(vec![whiteout(".wh..wh.plnk")]).unwrap();
        assert_eq!(paths(&t), vec!["x"]);
    }

    #[test]
    fn dir_over_dir_keeps_children_and_updates_meta() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![dir("d"), file("d/x", 0, 1)]).unwrap();
        let mut d = dir("d");
        d.meta.mode = 0o700;
        t.apply_layer(vec![d]).unwrap();
        assert_eq!(paths(&t), vec!["d", "d/x"]);
        assert_eq!(t.get("d").unwrap().meta.mode, 0o700);
    }

    #[test]
    fn file_over_dir_removes_children() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![dir("d"), file("d/x", 0, 1)]).unwrap();
        t.apply_layer(vec![file("d", 1, 0)]).unwrap();
        assert_eq!(paths(&t), vec!["d"]);
    }

    #[test]
    fn missing_parents_are_created() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![file("a/b/c", 0, 0)]).unwrap();
        assert_eq!(paths(&t), vec!["a", "a/b", "a/b/c"]);
        assert_eq!(t.get("a").unwrap().meta, Meta::implicit_dir());
    }

    #[test]
    fn non_directory_parent_is_an_error() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![LayerEntry {
            path: "lib".into(),
            kind: EntryKind::Symlink("usr/lib".into()),
            meta: meta(0o777),
        }])
        .unwrap();
        assert!(matches!(
            t.apply_layer(vec![file("lib/x", 1, 0)]),
            Err(RootfsError::NotADirectory(p)) if p == "lib"
        ));
    }

    #[test]
    fn case_only_differences_are_kept() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![file("Makefile", 0, 0), file("makefile", 0, 1)])
            .unwrap();
        assert_eq!(paths(&t), vec!["Makefile", "makefile"]);
    }

    #[test]
    fn hardlink_shares_group_and_survives_target_removal() {
        let mut t = MergedTree::new();
        t.apply_layer(vec![
            file("a", 0, 0),
            LayerEntry {
                path: "b".into(),
                kind: EntryKind::Hardlink("a".into()),
                meta: meta(0),
            },
        ])
        .unwrap();
        let group = t.get("a").unwrap().link_group;
        assert!(group.is_some());
        assert_eq!(t.get("b").unwrap().link_group, group);

        t.apply_layer(vec![whiteout(".wh.a")]).unwrap();
        assert_eq!(paths(&t), vec!["b"]);
        assert_eq!(content_layer(&t, "b"), 0);
    }

    #[test]
    fn hardlink_to_missing_or_dir_is_an_error() {
        let link = |target: &str| LayerEntry {
            path: "l".into(),
            kind: EntryKind::Hardlink(target.into()),
            meta: meta(0),
        };
        let mut t = MergedTree::new();
        assert!(matches!(
            t.apply_layer(vec![link("nope")]),
            Err(RootfsError::HardlinkTarget { .. })
        ));
        t.apply_layer(vec![dir("d")]).unwrap();
        assert!(matches!(
            t.apply_layer(vec![link("d")]),
            Err(RootfsError::HardlinkTarget { .. })
        ));
    }

    #[test]
    fn root_entry_updates_root_meta() {
        let mut t = MergedTree::new();
        let mut root = dir("./");
        root.meta.mode = 0o700;
        t.apply_layer(vec![root]).unwrap();
        assert_eq!(t.get("").unwrap().meta.mode, 0o700);
        assert!(t.is_empty());
    }
}
