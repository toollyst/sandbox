//! `COPY`, `ADD`, `WORKDIR`의 대상: 원본을 지금 뿌리(합친 트리)의 어디에 어떤 메타데이터로 놓을지 정한다.
//!
//! 규칙(Docker 29로 확인):
//!
//! - 대상이 `/`로 끝나거나 이미 있는 디렉터리면 파일은 그 안에 원래 이름으로 놓인다. 아니면 대상이 곧 그 파일이다.
//! - 디렉터리 원본은 그 **안의 것**을 대상 디렉터리에 놓는다(디렉터리 자신은 놓이지 않는다). 와일드카드로 맞은
//!   디렉터리도 그렇다(펼쳐진다).
//! - 원본이 둘 이상인데 대상이 디렉터리가 아니면 오류다(Docker의 문장과 같다).
//! - 대상 경로의 이미 있는 부분은 뿌리 안에서 링크를 따라간다(`/bin` → `usr/bin`).
//! - 새로 만드는 부모 디렉터리는 0755, root(`--chown`이 있으면 그 소유자, `--chmod`가 있으면 그 권한), 시각은 빌드
//!   시각이다. 디렉터리 원본으로 새로 만드는 대상 디렉터리는 권한 0755(`--chmod`)와 원본 디렉터리의 시각이다.
//!   이미 있는 디렉터리는 바꾸지 않는다.
//! - 놓이는 항목의 소유자는 `--chown`이 없으면 0:0이다. 권한은 원본 그대로이고 `--chmod`는 파일과 디렉터리에
//!   쓴다(링크는 0777 그대로). 수정 시각은 원본 그대로다.
//! - 이미 있는 디렉터리 자리에 디렉터리가 아닌 것을 놓으면 오류다.
//!
//! 결과 [`Placement`]는 경로 순서(부모가 먼저)의 항목들이다. 이미 있는 조상 디렉터리도 그 메타데이터 그대로 들어
//! 있다(`existing`). 층 tar에는 그것도 쓴다(층이 혼자 설 수 있게, `sbx-rootfs`의 `lower`). VM에 놓을 때는 뺀다.

use std::collections::BTreeMap;

use sbx_rootfs::{MergedTree, NodeKind};

use crate::source::{Content, Src, SrcKind};

/// 놓을 항목의 종류.
#[derive(Debug, Clone)]
pub enum ItemKind {
    /// 디렉터리.
    Dir,
    /// 일반 파일.
    File {
        /// 크기.
        size: u64,
        /// 내용.
        content: Content,
    },
    /// 심볼릭 링크.
    Symlink(String),
    /// 문자 장치.
    CharDevice(u32, u32),
    /// 블록 장치.
    BlockDevice(u32, u32),
    /// FIFO.
    Fifo,
}

/// 놓을 항목 하나.
#[derive(Debug, Clone)]
pub struct Item {
    /// 종류.
    pub kind: ItemKind,
    /// 권한 비트.
    pub mode: u32,
    /// 소유자.
    pub uid: u32,
    /// 그룹.
    pub gid: u32,
    /// 수정 시각.
    pub mtime: i64,
    /// 이미 뿌리에 있는 조상 디렉터리(메타데이터 그대로). 층 tar에만 쓴다.
    pub existing: bool,
}

/// 경로(뿌리 기준, 앞의 `/` 없음) → 항목. 경로 순서라 부모가 먼저다.
pub type Placement = BTreeMap<String, Item>;

/// 놓는 방법.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// 소유자(`--chown`, 풀린 숫자).
    pub owner: Option<(u32, u32)>,
    /// `--chmod`.
    pub chmod: Option<u32>,
    /// 빌드 시각(새로 만드는 부모 디렉터리).
    pub now: i64,
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_owned(),
        (_, true) => a.to_owned(),
        _ => format!("{a}/{b}"),
    }
}

/// 이미 있는 부분은 링크를 따라가고, 없는 부분은 그대로 붙인 경로.
pub fn resolve_dest(tree: &MergedTree, path: &str) -> Result<String, String> {
    let mut cur = String::new();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        let next = join(&cur, part);
        match tree.get(&next).map(|n| &n.kind) {
            None => {
                return Ok(parts[i..].iter().fold(cur, |acc, p| join(&acc, p)));
            }
            Some(NodeKind::Symlink(_)) => match tree.resolve(&next) {
                Some(r) => cur = r,
                // 끊어진 링크: 그 자리에 놓으면 링크를 바꾼다(남은 경로가 없을 때만).
                None if i + 1 == parts.len() => cur = next,
                None => {
                    return Err(format!(
                        "/{next} is a symbolic link that points nowhere inside the image"
                    ));
                }
            },
            Some(_) => cur = next,
        }
    }
    Ok(cur)
}

/// 원본들을 놓을 자리를 정한다. `dest`는 절대 경로이고, `/`로 끝나면 디렉터리다.
pub fn place(
    tree: &MergedTree,
    srcs: &[Src],
    dest: &str,
    opts: Options,
) -> Result<Placement, String> {
    let dir_syntax = dest.ends_with('/') || dest.ends_with("/.") || dest == ".";
    let target = resolve_dest(tree, dest)?;
    let target_is_dir = matches!(tree.get(&target).map(|n| &n.kind), Some(NodeKind::Dir));
    let into_dir = dir_syntax || target_is_dir;
    if srcs.len() > 1 && !into_dir {
        return Err(
            "When using COPY with more than one source file, the destination must be a directory and end with a /"
                .into(),
        );
    }
    let mut out = Placement::new();
    for src in srcs {
        let Some(first) = src.entries.first() else {
            continue;
        };
        if src.is_dir() {
            let mode = opts.chmod.unwrap_or(0o755);
            ensure_dir(tree, &mut out, &target, opts, Some((mode, first.mtime)))?;
            for e in &src.entries[1..] {
                put(tree, &mut out, join(&target, &e.rel), e, opts)?;
            }
        } else {
            let path = if into_dir {
                ensure_dir(tree, &mut out, &target, opts, None)?;
                join(&target, &src.name)
            } else {
                let parent = target.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
                ensure_dir(tree, &mut out, parent, opts, None)?;
                target.clone()
            };
            if path.is_empty() {
                return Err("cannot copy a file to /".into());
            }
            put(tree, &mut out, path, first, opts)?;
        }
    }
    Ok(out)
}

/// `WORKDIR`: 디렉터리가 없으면 만든다(소유자는 `USER`). 놓을 것이 없으면 빈 목록이다(빈 층).
pub fn workdir(tree: &MergedTree, dir: &str, opts: Options) -> Result<Placement, String> {
    let target = resolve_dest(tree, dir)?;
    let mut out = Placement::new();
    ensure_dir(tree, &mut out, &target, opts, None)?;
    if out.values().all(|i| i.existing) {
        out.clear();
    }
    Ok(out)
}

/// `dir`과 그 조상이 디렉터리로 있게 한다. 없는 것은 만든다. `own`은 `dir` 자신을 새로 만들 때의 (권한, 시각)이다.
fn ensure_dir(
    tree: &MergedTree,
    out: &mut Placement,
    dir: &str,
    opts: Options,
    own: Option<(u32, i64)>,
) -> Result<(), String> {
    let (uid, gid) = opts.owner.unwrap_or((0, 0));
    let mut cur = String::new();
    let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        cur = join(&cur, part);
        if let Some(item) = out.get(&cur) {
            if !matches!(item.kind, ItemKind::Dir) {
                return Err(format!("/{cur} is not a directory"));
            }
            continue;
        }
        match tree.get(&cur) {
            Some(n) if matches!(n.kind, NodeKind::Dir) => {
                out.insert(
                    cur.clone(),
                    Item {
                        kind: ItemKind::Dir,
                        mode: n.meta.mode,
                        uid: n.meta.uid,
                        gid: n.meta.gid,
                        mtime: n.meta.mtime,
                        existing: true,
                    },
                );
            }
            Some(_) => return Err(format!("/{cur} is not a directory")),
            None => {
                let last = i + 1 == parts.len();
                let (mode, mtime) = match (last, own) {
                    (true, Some(own)) => own,
                    _ => (opts.chmod.unwrap_or(0o755), opts.now),
                };
                out.insert(
                    cur.clone(),
                    Item {
                        kind: ItemKind::Dir,
                        mode,
                        uid,
                        gid,
                        mtime,
                        existing: false,
                    },
                );
            }
        }
    }
    Ok(())
}

fn put(
    tree: &MergedTree,
    out: &mut Placement,
    path: String,
    e: &crate::source::SrcEntry,
    opts: Options,
) -> Result<(), String> {
    let (uid, gid) = opts.owner.unwrap_or((0, 0));
    let kind = match &e.kind {
        SrcKind::Dir => ItemKind::Dir,
        SrcKind::File { size, content } => ItemKind::File {
            size: *size,
            content: content.clone(),
        },
        SrcKind::Symlink(t) => ItemKind::Symlink(t.clone()),
        SrcKind::CharDevice(a, b) => ItemKind::CharDevice(*a, *b),
        SrcKind::BlockDevice(a, b) => ItemKind::BlockDevice(*a, *b),
        SrcKind::Fifo => ItemKind::Fifo,
    };
    let is_dir = matches!(kind, ItemKind::Dir);
    let existing_dir = matches!(out.get(&path).map(|i| &i.kind), Some(ItemKind::Dir))
        || matches!(tree.get(&path).map(|n| &n.kind), Some(NodeKind::Dir));
    if existing_dir && !is_dir {
        return Err(format!(
            "cannot replace the directory /{path} with a non-directory"
        ));
    }
    let mode = match kind {
        ItemKind::Symlink(_) => e.mode,
        _ => opts.chmod.unwrap_or(e.mode),
    };
    out.insert(
        path,
        Item {
            kind,
            mode,
            uid,
            gid,
            mtime: e.mtime,
            existing: false,
        },
    );
    Ok(())
}
