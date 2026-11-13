//! `COPY`, `ADD`의 원본: 빌드 컨텍스트(호스트 디렉터리)나 앞 스테이지·이미지의 트리에서 원본 경로를 찾는다.
//!
//! 규칙(Docker 29로 확인):
//!
//! - 원본 경로는 원본의 뿌리 밖으로 나가지 않는다. `../x`, `/x`는 뿌리 안의 `x`다(앞의 `..`는 뿌리에서 멈춘다).
//! - 원본 경로의 심볼릭 링크(맨 끝 포함)는 따라가되 원본의 뿌리를 `/`로 삼는다(chroot와 같다). 그래서 링크가 뿌리 밖을
//!   가리켜도 뿌리 안의 같은 경로를 본다. 원본 디렉터리 **안의** 링크는 따라가지 않고 링크로 복사한다.
//! - 와일드카드(`*`, `?`, `[...]`, Go `filepath.Match`)는 경로 조각마다 맞춘다(`*`는 `/`를 넘지 않는다). 맞는 것이
//!   없어도 오류가 아니다(빈 층). 와일드카드가 없는 경로가 없으면 `"/경로": not found`다.
//! - 컨텍스트에서는 `.dockerignore`가 뺀 것이 없는 것과 같다. 그것을 직접 이름으로 부르면 `not found`이고, 디렉터리나
//!   와일드카드 안에서는 조용히 빠진다.
//! - 컨텍스트의 일반 파일, 디렉터리, 심볼릭 링크만 복사한다. 소켓, FIFO, 장치는 건너뛴다(BuildKit과 같다).
//!   스테이지·이미지의 트리에서는 장치와 FIFO도 복사한다.
//! - 컨텍스트의 파일 내용은 연 파일이 둘러볼 때 본 파일(장치·inode)과 같은지 본다. 그사이 링크로 바뀌었으면 오류다.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use sbx_rootfs::{ContentRef, MergedTree, NodeKind};

use crate::ports::{Contents, Ignore};

/// 링크를 따라가는 최대 수(Linux `MAXSYMLINKS`).
const MAX_HOPS: usize = 40;

/// 파일 내용이 있는 곳.
#[derive(Clone)]
pub enum Content {
    /// 컨텍스트의 파일. 둘러볼 때 본 (장치, inode)도 함께다.
    Host(PathBuf, Option<(u64, u64)>),
    /// 층의 파일.
    Layer(ContentRef, Arc<dyn Contents>),
}

impl std::fmt::Debug for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Content::Host(p, _) => write!(f, "Host({})", p.display()),
            Content::Layer(c, _) => write!(f, "Layer({c:?})"),
        }
    }
}

impl Content {
    /// 내용을 연다.
    pub fn open(&self) -> io::Result<Box<dyn Read + Send>> {
        match self {
            Content::Host(path, id) => {
                let f = fs::File::open(path)?;
                let meta = f.metadata()?;
                if !meta.is_file() || id.is_some_and(|id| Some(id) != file_id(&meta)) {
                    return Err(io::Error::other(format!(
                        "{} changed while the build read the context",
                        path.display()
                    )));
                }
                Ok(Box::new(f))
            }
            Content::Layer(c, contents) => contents.open(c),
        }
    }
}

#[cfg(unix)]
fn file_id(m: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(unix)]
fn mode_of(m: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn mode_of(m: &fs::Metadata) -> u32 {
    if m.is_dir() { 0o755 } else { 0o644 }
}

fn mtime_of(m: &fs::Metadata) -> i64 {
    match m.modified() {
        Ok(t) => match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(e) => -(e.duration().as_secs() as i64),
        },
        Err(_) => 0,
    }
}

/// 원본 항목의 종류.
#[derive(Debug, Clone)]
pub enum SrcKind {
    /// 디렉터리.
    Dir,
    /// 일반 파일.
    File {
        /// 크기.
        size: u64,
        /// 내용.
        content: Content,
    },
    /// 심볼릭 링크(대상 그대로).
    Symlink(String),
    /// 문자 장치.
    CharDevice(u32, u32),
    /// 블록 장치.
    BlockDevice(u32, u32),
    /// FIFO.
    Fifo,
}

/// 원본 항목 하나. `rel`은 원본(파일이나 디렉터리) 기준 상대 경로이고, 원본 자신은 빈 문자열이다.
#[derive(Debug, Clone)]
pub struct SrcEntry {
    /// 상대 경로.
    pub rel: String,
    /// 종류.
    pub kind: SrcKind,
    /// 권한 비트.
    pub mode: u32,
    /// 수정 시각.
    pub mtime: i64,
}

/// 맞은 원본 하나: 파일이거나 디렉터리(그 안의 모든 것).
#[derive(Debug, Clone)]
pub struct Src {
    /// 이름(대상이 디렉터리일 때 쓰는 마지막 이름). 원본의 뿌리면 빈 문자열이다.
    pub name: String,
    /// 첫 항목(`rel`이 빈 문자열)이 원본 자신이다. 디렉터리면 그 안의 것이 부모가 먼저인 차례로 뒤따른다.
    pub entries: Vec<SrcEntry>,
}

impl Src {
    /// 디렉터리인지.
    pub fn is_dir(&self) -> bool {
        matches!(self.entries.first().map(|e| &e.kind), Some(SrcKind::Dir))
    }
}

/// 원본 경로를 뿌리 안의 조각들로 정리한다(`..`는 뿌리에서 멈춘다).
pub fn clean(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p.to_owned()),
        }
    }
    out
}

/// 경로 조각에 와일드카드가 있는지.
pub fn has_wildcard(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Go `filepath.Match`로 이름 하나(`/` 없음)를 맞춘다. 잘못된 패턴은 오류다.
pub fn glob_match(pattern: &str, name: &str) -> Result<bool, String> {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_at(&p, &n).map_err(|()| format!("{pattern:?}: syntax error in pattern"))
}

fn glob_at(p: &[char], n: &[char]) -> Result<bool, ()> {
    let (mut pi, mut ni) = (0, 0);
    // 마지막 `*`의 자리(되돌아가기).
    let mut star: Option<(usize, usize)> = None;
    loop {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ni));
                    pi += 1;
                    continue;
                }
                '?' if ni < n.len() => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                '[' if ni < n.len() => {
                    let (ok, next) = class(p, pi, n[ni])?;
                    if ok {
                        pi = next;
                        ni += 1;
                        continue;
                    }
                }
                '\\' => {
                    let c = *p.get(pi + 1).ok_or(())?;
                    if ni < n.len() && n[ni] == c {
                        pi += 2;
                        ni += 1;
                        continue;
                    }
                }
                c if c != '?' && c != '[' && ni < n.len() && n[ni] == c => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                _ => {}
            }
        } else if ni == n.len() {
            return Ok(true);
        }
        match star {
            Some((sp, sn)) if sn < n.len() => {
                star = Some((sp, sn + 1));
                pi = sp + 1;
                ni = sn + 1;
            }
            _ => {
                // 남은 패턴이 바른지는 본다(Go는 잘못된 패턴을 오류로 낸다).
                check_rest(&p[pi.min(p.len())..])?;
                return Ok(false);
            }
        }
    }
}

fn check_rest(p: &[char]) -> Result<(), ()> {
    let mut i = 0;
    while i < p.len() {
        match p[i] {
            '[' => {
                let (_, next) = class(p, i, '\0')?;
                i = next;
            }
            '\\' => {
                if i + 1 >= p.len() {
                    return Err(());
                }
                i += 2;
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// `p[at]`이 `[`인 묶음을 `c`에 맞춘다. (맞았는지, 묶음 뒤 자리).
fn class(p: &[char], at: usize, c: char) -> Result<(bool, usize), ()> {
    let mut i = at + 1;
    let negated = p.get(i) == Some(&'^');
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    loop {
        let ch = *p.get(i).ok_or(())?;
        if ch == ']' && !first {
            i += 1;
            break;
        }
        first = false;
        let lo = if ch == '\\' {
            i += 1;
            *p.get(i).ok_or(())?
        } else if ch == '-' || ch == ']' {
            return Err(());
        } else {
            ch
        };
        i += 1;
        let mut hi = lo;
        if p.get(i) == Some(&'-') && p.get(i + 1).is_some_and(|x| *x != ']') {
            i += 1;
            let h = *p.get(i).ok_or(())?;
            hi = if h == '\\' {
                i += 1;
                *p.get(i).ok_or(())?
            } else {
                h
            };
            i += 1;
            if hi < lo {
                return Err(());
            }
        }
        if lo <= c && c <= hi {
            matched = true;
        }
    }
    Ok((matched != negated, i))
}

/// 빌드 컨텍스트.
pub struct Context {
    root: PathBuf,
    ignore: Option<Arc<dyn Ignore>>,
}

impl Context {
    /// `root`(디렉터리)를 컨텍스트로 쓴다.
    pub fn new(root: PathBuf, ignore: Option<Arc<dyn Ignore>>) -> Self {
        Self { root, ignore }
    }

    fn excluded(&self, rel: &str) -> bool {
        !rel.is_empty() && self.ignore.as_ref().is_some_and(|i| i.is_excluded(rel))
    }

    fn descend_excluded(&self) -> bool {
        self.ignore.as_ref().is_some_and(|i| i.has_exceptions())
    }

    fn host(&self, rel: &[String]) -> PathBuf {
        let mut p = self.root.clone();
        for c in rel {
            p.push(c);
        }
        p
    }

    /// 조각들을 뿌리 안에서 풀어(링크를 따라간다) 실제 상대 경로를 돌려준다. 없으면 `None`.
    fn resolve(&self, parts: &[String]) -> io::Result<Option<Vec<String>>> {
        let mut pending: Vec<String> = parts.iter().rev().cloned().collect();
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
            let meta = match fs::symlink_metadata(self.host(&cur)) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
                Err(e) if e.raw_os_error() == Some(20) => return Ok(None),
                Err(e) => return Err(e),
            };
            if meta.file_type().is_symlink() {
                hops += 1;
                if hops > MAX_HOPS {
                    return Ok(None);
                }
                let target = fs::read_link(self.host(&cur))?;
                let target = target.to_string_lossy().into_owned();
                cur.pop();
                if target.starts_with('/') {
                    cur.clear();
                }
                pending.extend(target.split('/').rev().map(str::to_owned));
            }
        }
        Ok(Some(cur))
    }

    /// 원본 경로 하나를 찾는다. 와일드카드면 맞는 것 모두(없으면 빈 목록).
    pub fn find(&self, raw: &str) -> Result<Vec<Src>, String> {
        let parts = clean(raw);
        let io = |e: io::Error| format!("{raw}: {e}");
        if !parts.iter().any(|p| has_wildcard(p)) {
            let shown = format!("\"/{}\"", parts.join("/"));
            let not_found = || format!("{shown}: not found");
            if self.excluded(&parts.join("/")) {
                return Err(not_found());
            }
            let real = self.resolve(&parts).map_err(io)?.ok_or_else(not_found)?;
            if self.excluded(&real.join("/")) {
                return Err(not_found());
            }
            let name = parts.last().cloned().unwrap_or_default();
            return Ok(vec![self.src(name, &real).map_err(io)?]);
        }
        // 와일드카드: 조각마다 맞춘다.
        let mut found: Vec<(Vec<String>, Vec<String>)> = vec![(Vec::new(), Vec::new())];
        for part in &parts {
            let mut next = Vec::new();
            for (shown, real) in found {
                if has_wildcard(part) {
                    let dir = self.host(&real);
                    let Ok(rd) = fs::read_dir(&dir) else { continue };
                    let mut names: Vec<String> = rd
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                    names.sort();
                    for n in names {
                        if glob_match(part, &n)? {
                            let mut s = shown.clone();
                            s.push(n.clone());
                            let mut r = real.clone();
                            r.push(n);
                            if let Some(r) = self.resolve(&r).map_err(io)? {
                                next.push((s, r));
                            }
                        }
                    }
                } else {
                    let mut s = shown.clone();
                    s.push(part.clone());
                    let mut r = real.clone();
                    r.push(part.clone());
                    if let Some(r) = self.resolve(&r).map_err(io)? {
                        next.push((s, r));
                    }
                }
            }
            found = next;
        }
        let mut out = Vec::new();
        for (shown, real) in found {
            if self.excluded(&shown.join("/")) || self.excluded(&real.join("/")) {
                continue;
            }
            let name = shown.last().cloned().unwrap_or_default();
            out.push(self.src(name, &real).map_err(io)?);
        }
        Ok(out)
    }

    /// 실제 상대 경로 `real`의 원본(디렉터리면 그 안까지).
    fn src(&self, name: String, real: &[String]) -> io::Result<Src> {
        let path = self.host(real);
        let meta = fs::metadata(&path)?;
        let mut entries = Vec::new();
        let rel = real.join("/");
        self.walk(&path, &rel, String::new(), &meta, &mut entries)?;
        Ok(Src { name, entries })
    }

    /// `path`(컨텍스트 기준 `ctx_rel`)를 원본 기준 `rel`로 더한다. 빠진 것은 건너뛴다.
    fn walk(
        &self,
        path: &Path,
        ctx_rel: &str,
        rel: String,
        meta: &fs::Metadata,
        out: &mut Vec<SrcEntry>,
    ) -> io::Result<bool> {
        let ft = meta.file_type();
        let (mode, mtime) = (mode_of(meta), mtime_of(meta));
        if ft.is_dir() {
            let at = out.len();
            out.push(SrcEntry {
                rel: rel.clone(),
                kind: SrcKind::Dir,
                mode,
                mtime,
            });
            let mut names: Vec<_> = fs::read_dir(path)?
                .map(|e| e.map(|e| e.file_name()))
                .collect::<Result<_, _>>()?;
            names.sort();
            for n in names {
                let n = n.to_string_lossy().into_owned();
                let child_ctx = if ctx_rel.is_empty() {
                    n.clone()
                } else {
                    format!("{ctx_rel}/{n}")
                };
                let child_rel = if rel.is_empty() {
                    n.clone()
                } else {
                    format!("{rel}/{n}")
                };
                let excluded = self.excluded(&child_ctx);
                let child = path.join(&n);
                let m = fs::symlink_metadata(&child)?;
                if excluded {
                    // 예외 패턴이 있으면 빠진 디렉터리 안에서 다시 들어온 것을 찾는다. 그것이 있으면 디렉터리도 남는다.
                    if m.is_dir() && self.descend_excluded() {
                        let before = out.len();
                        let kept = self.walk(&child, &child_ctx, child_rel, &m, out)?;
                        if !kept {
                            out.truncate(before);
                        }
                    }
                    continue;
                }
                self.walk(&child, &child_ctx, child_rel, &m, out)?;
            }
            // 빠진 디렉터리 안을 볼 때: 자신이 빠졌고 안에 남은 것이 없으면 버린다.
            let kept = out.len() > at + 1 || !self.excluded(ctx_rel);
            return Ok(kept);
        }
        let kind = if ft.is_file() {
            SrcKind::File {
                size: meta.len(),
                content: Content::Host(path.to_owned(), file_id(meta)),
            }
        } else if ft.is_symlink() {
            SrcKind::Symlink(fs::read_link(path)?.to_string_lossy().into_owned())
        } else {
            return Ok(false);
        };
        if self.excluded(ctx_rel) {
            return Ok(false);
        }
        out.push(SrcEntry {
            rel,
            kind,
            mode,
            mtime,
        });
        Ok(true)
    }
}

/// 층을 합친 트리(앞 스테이지, 이미지). 파일 내용은 층 번호로 고른다.
#[derive(Clone, Default)]
pub struct TreeView {
    /// 합친 트리.
    pub tree: MergedTree,
    /// 층 번호 → 내용.
    pub contents: BTreeMap<usize, Arc<dyn Contents>>,
}

impl TreeView {
    /// 내용을 여는 쪽.
    pub fn content(&self, c: &ContentRef) -> Content {
        let contents = self
            .contents
            .get(&c.layer)
            .cloned()
            .unwrap_or_else(|| Arc::new(Missing));
        Content::Layer(*c, contents)
    }

    /// 파일 하나를 읽는다(링크를 따라간다). 없거나 일반 파일이 아니면 `None`. `limit`보다 크면 앞부분만.
    pub fn read_file(&self, path: &str, limit: u64) -> io::Result<Option<Vec<u8>>> {
        let Some(real) = self.tree.resolve(path) else {
            return Ok(None);
        };
        match self.tree.get(&real).map(|n| &n.kind) {
            Some(NodeKind::File(c)) => {
                let mut b = Vec::new();
                self.content(c).open()?.take(limit).read_to_end(&mut b)?;
                Ok(Some(b))
            }
            _ => Ok(None),
        }
    }

    /// 원본 경로 하나를 찾는다(규칙은 모듈 문서).
    pub fn find(&self, raw: &str) -> Result<Vec<Src>, String> {
        let parts = clean(raw);
        let mut found: Vec<(String, String)> = vec![(String::new(), String::new())];
        let wild = parts.iter().any(|p| has_wildcard(p));
        for part in &parts {
            let mut next = Vec::new();
            for (shown, real) in found {
                let join = |a: &str, b: &str| {
                    if a.is_empty() {
                        b.to_owned()
                    } else {
                        format!("{a}/{b}")
                    }
                };
                if has_wildcard(part) {
                    for name in self.children(&real) {
                        if glob_match(part, &name)?
                            && let Some(r) = self.tree.resolve(&join(&real, &name))
                        {
                            next.push((join(&shown, &name), r));
                        }
                    }
                } else if let Some(r) = self.tree.resolve(&join(&real, part)) {
                    next.push((join(&shown, part), r));
                }
            }
            found = next;
        }
        if found.is_empty() && !wild {
            return Err(format!("\"/{}\": not found", parts.join("/")));
        }
        Ok(found
            .into_iter()
            .map(|(shown, real)| Src {
                name: shown.rsplit('/').next().unwrap_or_default().to_owned(),
                entries: self.entries(&real),
            })
            .collect())
    }

    fn children(&self, dir: &str) -> Vec<String> {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        self.tree
            .iter()
            .filter_map(|(p, _)| p.strip_prefix(&prefix))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            .map(str::to_owned)
            .collect()
    }

    fn entries(&self, real: &str) -> Vec<SrcEntry> {
        let prefix = if real.is_empty() {
            String::new()
        } else {
            format!("{real}/")
        };
        let mut out = Vec::new();
        for (p, node) in self.tree.iter() {
            let rel = if p == real {
                String::new()
            } else if let Some(rest) = p.strip_prefix(&prefix) {
                if rest.is_empty() {
                    continue;
                }
                rest.to_owned()
            } else {
                continue;
            };
            let kind = match &node.kind {
                NodeKind::Dir => SrcKind::Dir,
                NodeKind::File(c) => SrcKind::File {
                    size: c.size,
                    content: self.content(c),
                },
                NodeKind::Symlink(t) => SrcKind::Symlink(t.clone()),
                NodeKind::CharDevice { major, minor } => SrcKind::CharDevice(*major, *minor),
                NodeKind::BlockDevice { major, minor } => SrcKind::BlockDevice(*major, *minor),
                NodeKind::Fifo => SrcKind::Fifo,
            };
            out.push(SrcEntry {
                rel,
                kind,
                mode: node.meta.mode,
                mtime: node.meta.mtime,
            });
        }
        // 원본 자신이 맨 앞이다(트리의 순서에서 `real`이 그 아래 것보다 앞이다).
        out
    }
}

struct Missing;

impl Contents for Missing {
    fn open(&self, _: &ContentRef) -> io::Result<Box<dyn Read + Send>> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the layer content is not available",
        ))
    }
}

/// 원본들의 내용 해시(캐시 키에 넣는다): 상대 경로, 종류, 권한, 크기, 링크 대상, 파일 내용. 수정 시각과 호스트의
/// 소유자는 넣지 않는다(결과 층에서 소유자는 `--chown`이나 root다).
pub fn content_hash(srcs: &[Src]) -> io::Result<String> {
    let mut h = Sha256::new();
    for s in srcs {
        h.update(format!("src {:?}\n", s.name));
        for e in &s.entries {
            let kind = match &e.kind {
                SrcKind::Dir => "dir".to_owned(),
                SrcKind::File { size, content } => {
                    let mut fh = Sha256::new();
                    let mut r = content.open()?;
                    let mut buf = vec![0u8; 64 << 10];
                    let mut total = 0u64;
                    loop {
                        let n = r.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        total += n as u64;
                        fh.update(&buf[..n]);
                    }
                    if total != *size {
                        return Err(io::Error::other(format!(
                            "{:?} changed while the build read it",
                            e.rel
                        )));
                    }
                    format!("file {size} {}", hex(&fh.finalize()))
                }
                SrcKind::Symlink(t) => format!("symlink {t:?}"),
                SrcKind::CharDevice(a, b) => format!("char {a} {b}"),
                SrcKind::BlockDevice(a, b) => format!("block {a} {b}"),
                SrcKind::Fifo => "fifo".to_owned(),
            };
            h.update(format!("{:?} {:o} {kind}\n", e.rel, e.mode));
        }
    }
    Ok(hex(&h.finalize()))
}

/// 바이트를 소문자 16진수로.
pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
