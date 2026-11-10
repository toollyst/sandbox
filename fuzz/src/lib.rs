//! 퍼징 대상들이 함께 쓰는 것: 대상마다 하나인 임시 디렉터리와, 그 안이 바뀌었는지 보는 스냅숏.
//!
//! 파일시스템에 쓰는 대상(`copy_from_guest`, `executor_unpack`, `image_layer`)은 실행마다 [`Scratch::fresh`]로
//! 빈 디렉터리를 받는다. 대상 디렉터리 밖에 아무것도 쓰지 않았는지는 [`snapshot`]을 앞뒤로 비교해 본다.
//! 링크는 따라가지 않는다(따라가면 검사가 공격자가 고른 곳을 읽는다).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 프로세스마다 하나인 임시 디렉터리. `<temp>/sbx-fuzz-<이름>-<pid>`.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// 대상 이름으로 만든다. 같은 프로세스에서는 같은 것을 돌려준다.
    pub fn get(name: &str) -> &'static Scratch {
        static CELL: OnceLock<Scratch> = OnceLock::new();
        CELL.get_or_init(|| {
            let root = std::env::temp_dir().join(format!("sbx-fuzz-{name}-{}", std::process::id()));
            remove_all(&root);
            fs::create_dir_all(&root).expect("create scratch directory");
            Scratch { root }
        })
    }

    /// 비운 `<root>/run`을 돌려준다.
    pub fn fresh(&self) -> PathBuf {
        let run = self.root.join("run");
        remove_all(&run);
        fs::create_dir_all(&run).expect("create run directory");
        run
    }
}

/// 디렉터리를 지운다. 권한을 없앤 디렉터리(게스트가 고른 모드)도 지울 수 있게 먼저 권한을 연다.
/// 링크는 따라가지 않는다.
pub fn remove_all(dir: &Path) {
    open_up(dir);
    let _ = fs::remove_dir_all(dir);
}

fn open_up(dir: &Path) {
    let Ok(m) = fs::symlink_metadata(dir) else {
        return;
    };
    if !m.is_dir() {
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    if let Ok(entries) = fs::read_dir(dir) {
        for e in entries.flatten() {
            open_up(&e.path());
        }
    }
}

/// 항목 하나의 모습: 종류와 내용(파일), 대상(링크). 권한은 넣지 않는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// 디렉터리.
    Dir,
    /// 일반 파일과 그 내용.
    File(Vec<u8>),
    /// 심볼릭 링크와 그 대상.
    Symlink(PathBuf),
    /// 그 밖의 것(장치, FIFO, 소켓).
    Other,
}

/// `dir` 아래 전체(`dir` 기준 상대 경로 → 모습). 링크를 따라가지 않는다. 파일 내용은 `max_file`바이트까지 읽는다.
pub fn snapshot(dir: &Path, max_file: u64) -> BTreeMap<PathBuf, Item> {
    let mut out = BTreeMap::new();
    walk(dir, Path::new(""), max_file, &mut out);
    out
}

fn walk(base: &Path, rel: &Path, max_file: u64, out: &mut BTreeMap<PathBuf, Item>) {
    let Ok(entries) = fs::read_dir(base.join(rel)) else {
        return;
    };
    for e in entries.flatten() {
        let r = rel.join(e.file_name());
        let p = base.join(&r);
        let Ok(m) = fs::symlink_metadata(&p) else {
            continue;
        };
        let item = if m.is_dir() {
            walk(base, &r, max_file, out);
            Item::Dir
        } else if m.file_type().is_symlink() {
            Item::Symlink(fs::read_link(&p).unwrap_or_default())
        } else if m.is_file() {
            use std::io::Read;
            let mut buf = Vec::new();
            if let Ok(f) = fs::File::open(&p) {
                let _ = f.take(max_file).read_to_end(&mut buf);
            }
            Item::File(buf)
        } else {
            Item::Other
        };
        out.insert(r, item);
    }
}

/// 이름 하나. 경로를 만드는 대상들이 쓴다. 적은 이름을 자주 겹치게 해서 덮어쓰기, 디렉터리 합치기,
/// 링크 너머 쓰기 같은 경우에 빨리 닿게 한다.
#[derive(Debug, Clone, arbitrary::Arbitrary)]
pub enum Name {
    /// `t`.
    T,
    /// `a`.
    A,
    /// `b`.
    B,
    /// `escape` (미리 놓아 둔 링크 이름과 같다).
    Escape,
    /// `.`.
    Dot,
    /// `..`.
    DotDot,
    /// 빈 이름 (`a//b`).
    Empty,
    /// `..\..\x` (Windows에서는 구분자).
    Backslash,
    /// `C:` (Windows 드라이브).
    Drive,
    /// `.wh.<이름>` (OCI whiteout).
    Whiteout(Box<Name>),
    /// `.wh..wh..opq` (OCI opaque).
    Opaque,
    /// 아무 문자열.
    Raw(String),
}

impl Name {
    /// 문자열로.
    pub fn render(&self) -> String {
        match self {
            Name::T => "t".into(),
            Name::A => "a".into(),
            Name::B => "b".into(),
            Name::Escape => "escape".into(),
            Name::Dot => ".".into(),
            Name::DotDot => "..".into(),
            Name::Empty => String::new(),
            Name::Backslash => "..\\..\\x".into(),
            Name::Drive => "C:".into(),
            Name::Whiteout(n) => format!(".wh.{}", n.render()),
            Name::Opaque => ".wh..wh..opq".into(),
            Name::Raw(s) => s.clone(),
        }
    }
}

/// 이름들을 `/`로 이은 경로. `absolute`면 `/`로 시작한다.
pub fn join(names: &[Name], absolute: bool) -> String {
    let s = names.iter().map(Name::render).collect::<Vec<_>>().join("/");
    if absolute { format!("/{s}") } else { s }
}
