//! 실시간 공유 폴더(`-v ./src:/app`)의 호스트 쪽 변경을 게스트의 파일 감시에 알린다(호스트 쪽). VM을 소유한 프로세스
//! (attached면 `sbx`, detached면 `sbx-shim`)가 VM마다 스레드 하나로 돌린다. 게스트가 알림을 만드는 방법은 `sbx-guest`의
//! `changes` 모듈에 있다. 실행기에는 아무것도 더 바라지 않는다.
//!
//! **왜**: virtio-fs는 호스트 쪽 변경을 게스트 inotify로 전하지 않는다. 그러면 호스트 편집기에서 고친 파일에 핫 리로드
//! (vite, webpack, nodemon, uvicorn `--reload`)가 돌지 않는다. Docker Desktop의 파일 공유는 이것을 전한다.
//!
//! **무엇을 보나**: 쓰기로 붙인 공유마다 호스트 디렉터리를 아래까지 본다(macOS FSEvents, Linux inotify, Windows
//! ReadDirectoryChangesW: `notify` 크레이트). 링크는 따라가지 않는다. 읽기 전용 공유(`:ro`)는 보지 않는다: 게스트가
//! 그 공유에서는 알림을 만들 수 없다(setattr이 `EROFS`).
//!
//! **언제 무엇을 보내나** ([`Pending`])
//! - 사건이 오면 [`QUIET`] 동안 더 오지 않을 때까지, 많아야 [`MAX_WAIT`]까지 모은다. 파일 접근(읽기, 닫기)만인 사건은 버린다.
//! - 모은 경로마다 지금 호스트의 모습([`Look`]: 있는지, 디렉터리인지, 크기, 수정 시각)을 읽어 마지막으로 보낸 모습과
//!   다를 때만 보낸다. 처음 보는 경로는 보낸다. 그래서 게스트의 알림 연산(수정 시각을 그대로 다시 쓴다)이 호스트에
//!   남기는 사건(ctime만 바뀐다)은 다시 보내지 않는다: 호스트와 게스트 사이에 되도는 고리가 없다.
//! - 사건을 놓쳤다는 표시(inotify 넘침, FSEvents의 다시 훑으라는 표시)가 오면 그 공유의 뿌리(빈 경로)를 보낸다.
//! - 경로는 공유 안의 상대 경로(`/`로 이음)다. UTF-8이 아닌 이름은 그 위의 UTF-8 조상으로 보낸다. 한 메시지에
//!   [`MAX_SHARE_CHANGES`]개까지, 넘으면 나눠 보낸다.
//! - 기억하는 모습은 [`MAX_REMEMBERED`]개까지다. 넘으면 비운다(그 뒤 경로마다 한 번 더 보낼 뿐이다).
//!
//! **비용**: 게스트가 공유에 쓴 것도 호스트에서는 사건이고 처음 보는 모습이라 한 번 되돌려 보낸다(게스트에서 항목마다
//! 공유 서버 왕복 두 번). `npm install`처럼 공유에 많이 쓰면 그만큼 더 들고, 게스트의 감시 도구가 그 항목들의 바뀜을 한
//! 번 더 본다.
//!
//! **기록**: 실패는 샌드박스의 `logs/executor.log`에 `sbx: share changes: …` 한 줄로, 종류마다 이번 실행에 한 번만 쓴다.
//! 메시지를 모르는 게스트(`Protocol`)면 그 줄을 남기고 이 VM에서는 그만둔다. 감시를 시작하지 못한 공유(디렉터리가
//! 없다, inotify 감시 수 한도)는 그 줄을 남기고 감시 없이 간다(공유 자체는 된다).
//!
//! **믿지 않음**: 게스트가 하는 일은 `Done`이나 오류로 답하는 것뿐이다. 게스트가 보낸 어떤 값도 무엇을 언제 보낼지를
//! 바꾸지 않는다. 기록에 넣는 게스트의 오류 문장은 [`MAX_NOTE`](crate::clock::MAX_NOTE) 글자로 자른다. 게스트가 답하지
//! 않으면 메시지 하나가 [`REQUEST_TIMEOUT`](crate::client::REQUEST_TIMEOUT)까지 이 스레드를 붙잡을 뿐이다(그동안 온 사건은
//! 채널에 쌓였다가 다음 묶음이 된다).
//!
//! **수명**: [`Running`]을 버리면(소유 기록이 사라지면) 감시를 멈추고 스레드가 끝난다.

use std::collections::{BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use sbx_guest_proto::{ErrorKind, MAX_SHARE_CHANGES};

use crate::client::Guest;
use crate::clock::{Kind, Notes, Topic, kind};
use crate::error::RuntimeError;

/// 사건이 이만큼 더 오지 않으면 모은 것을 보낸다.
pub const QUIET: Duration = Duration::from_millis(50);
/// 사건이 끊이지 않아도 첫 사건에서 이만큼 지나면 보낸다.
pub const MAX_WAIT: Duration = Duration::from_millis(500);
/// 기억하는 경로의 최대 수.
pub const MAX_REMEMBERED: usize = 65_536;

/// 보는 공유 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// 호스트 디렉터리.
    pub host: PathBuf,
    /// 게스트가 마운트한 절대 경로.
    pub target: String,
}

/// 호스트 경로의 지금 모습. 마지막으로 보낸 모습과 견준다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    /// 디렉터리인지.
    pub dir: bool,
    /// 크기.
    pub len: u64,
    /// 수정 시각.
    pub modified: Option<SystemTime>,
}

impl Look {
    /// 지금 읽는다. 없으면 `None`. 링크는 따라가지 않는다.
    pub fn of(path: &Path) -> Option<Self> {
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(Self {
            dir: m.is_dir(),
            len: m.len(),
            modified: m.modified().ok(),
        })
    }
}

/// 모은 사건을 공유별 상대 경로로 바꾸고, 바뀐 것만 남긴다. 규칙은 모듈 문서의 "언제 무엇을 보내나".
pub struct Pending {
    /// 공유마다 (뿌리 후보들, 게스트 경로). 뿌리 후보는 받은 경로와 그 실제 경로(macOS의 `/var` → `/private/var`).
    roots: Vec<(Vec<PathBuf>, String)>,
    seen: HashMap<PathBuf, Option<Look>>,
    paths: BTreeSet<PathBuf>,
    rescan: bool,
}

impl Pending {
    /// `shares`를 본다.
    pub fn new(shares: &[Share]) -> Self {
        let roots = shares
            .iter()
            .map(|s| {
                let mut r = vec![s.host.clone()];
                if let Ok(c) = s.host.canonicalize()
                    && c != s.host
                {
                    r.push(c);
                }
                (r, s.target.clone())
            })
            .collect();
        Self {
            roots,
            seen: HashMap::new(),
            paths: BTreeSet::new(),
            rescan: false,
        }
    }

    /// 사건 하나를 더한다.
    pub fn add(&mut self, e: &Event) {
        if e.need_rescan() {
            self.rescan = true;
        }
        if matches!(e.kind, EventKind::Access(_)) {
            return;
        }
        self.paths.extend(e.paths.iter().cloned());
    }

    /// 모은 것이 있는지.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && !self.rescan
    }

    /// 보낼 것: (게스트 경로, 상대 경로들). 한 묶음은 [`MAX_SHARE_CHANGES`]개 이하다. 모은 것은 비운다.
    pub fn take(&mut self) -> Vec<(String, Vec<String>)> {
        let mut per: Vec<BTreeSet<String>> = vec![BTreeSet::new(); self.roots.len()];
        if std::mem::take(&mut self.rescan) {
            for set in &mut per {
                set.insert(String::new());
            }
        }
        for p in std::mem::take(&mut self.paths) {
            let Some((i, rel)) = self.locate(&p) else {
                continue;
            };
            let now = Look::of(&p);
            if self.seen.get(&p) == Some(&now) {
                continue;
            }
            if self.seen.len() >= MAX_REMEMBERED {
                self.seen.clear();
            }
            self.seen.insert(p, now);
            per[i].insert(rel);
        }
        let mut out = Vec::new();
        for (set, (_, target)) in per.into_iter().zip(&self.roots) {
            let all: Vec<String> = set.into_iter().collect();
            for chunk in all.chunks(MAX_SHARE_CHANGES as usize) {
                out.push((target.clone(), chunk.to_vec()));
            }
        }
        out
    }

    /// `p`가 든 공유와 그 안의 상대 경로. 공유 밖이면 `None`.
    fn locate(&self, p: &Path) -> Option<(usize, String)> {
        self.roots.iter().enumerate().find_map(|(i, (roots, _))| {
            let rest = roots.iter().find_map(|r| p.strip_prefix(r).ok())?;
            Some((i, relative(rest)))
        })
    }
}

/// 상대 경로를 `/`로 이은 문자열로. UTF-8이 아니거나 평범하지 않은 구성 요소가 나오면 그 앞(조상)까지만.
fn relative(rest: &Path) -> String {
    let mut parts = Vec::new();
    for c in rest.components() {
        match c {
            Component::Normal(n) => match n.to_str() {
                Some(s) => parts.push(s),
                None => break,
            },
            _ => break,
        }
    }
    parts.join("/")
}

/// 감시가 돌고 있다. 버리면 멈춘다.
pub struct Running {
    _watcher: notify::RecommendedWatcher,
}

/// 보내는 쪽. 실제로는 [`Guest::share_changed`]이고 테스트는 기록한다.
pub type SendFn = Box<dyn FnMut(&str, &[String]) -> Result<(), RuntimeError> + Send>;

/// `shares`를 보기 시작하고 스레드를 띄운다. 하나도 보지 못하거나 스레드를 띄우지 못하면 `None`이다(기록은 `notes`).
pub fn start(shares: Vec<Share>, mut send: SendFn, mut notes: Notes) -> Option<Running> {
    if shares.is_empty() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let config = notify::Config::default().with_follow_symlinks(false);
    let mut watcher = match notify::RecommendedWatcher::new(tx, config) {
        Ok(w) => w,
        Err(e) => {
            notes.line(&format!("cannot watch shared folders: {e}"));
            return None;
        }
    };
    let mut watched = Vec::new();
    for s in shares {
        match watcher.watch(&s.host, RecursiveMode::Recursive) {
            Ok(()) => watched.push(s),
            Err(e) => notes.line(&format!(
                "cannot watch {} (shared at {}); the guest will not hear of host changes there: {e}",
                s.host.display(),
                s.target
            )),
        }
    }
    if watched.is_empty() {
        return None;
    }
    std::thread::Builder::new()
        .name("sbx-share-watch".into())
        .spawn(move || run(&rx, &mut Pending::new(&watched), &mut *send, &mut notes))
        .ok()?;
    Some(Running { _watcher: watcher })
}

/// 사건을 모아 보낸다. 감시가 멈추거나(보내는 쪽이 사라짐) 게스트가 메시지를 모르면 끝난다.
pub fn run(
    rx: &Receiver<notify::Result<Event>>,
    pending: &mut Pending,
    send: &mut (dyn FnMut(&str, &[String]) -> Result<(), RuntimeError> + Send),
    notes: &mut Notes,
) {
    let add = |pending: &mut Pending, r: notify::Result<Event>| match r {
        Ok(e) => pending.add(&e),
        // 감시의 오류(넘침 등)는 놓쳤을 수 있다는 뜻이다. 뿌리를 다시 알린다.
        Err(_) => pending.rescan = true,
    };
    loop {
        match rx.recv() {
            Ok(r) => add(pending, r),
            Err(_) => return,
        }
        let first = Instant::now();
        loop {
            let left = MAX_WAIT.saturating_sub(first.elapsed());
            if left.is_zero() {
                break;
            }
            match rx.recv_timeout(QUIET.min(left)) {
                Ok(r) => add(pending, r),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        for (target, paths) in pending.take() {
            if let Err(e) = send(&target, &paths) {
                notes.once(&e);
                if unsupported(&e) {
                    return;
                }
            }
        }
    }
}

/// 이 메시지를 모르는 게스트.
fn unsupported(e: &RuntimeError) -> bool {
    matches!(e, RuntimeError::Guest(g) if g.kind == ErrorKind::Protocol)
}

/// 이 모듈의 기록. 종류는 시계 맞추기와 같지만 `Unsupported`는 `Protocol`뿐이다: `InvalidArgument`는 호스트가 규칙에
/// 맞지 않는 경로를 보냈다는 뜻이고 다음 묶음은 될 수 있다.
pub const TOPIC: Topic = Topic {
    prefix: "sbx: share changes: ",
    kind: note_kind,
    unsupported: "; this guest cannot pass host changes to its file watchers \
                  (restart the sandbox after upgrading sbx)",
};

fn note_kind(e: &RuntimeError) -> Kind {
    match e {
        e if unsupported(e) => Kind::Unsupported,
        RuntimeError::Guest(g) => Kind::Guest(g.kind),
        e => kind(e),
    }
}

/// 게스트 하나의 공유 감시를 띄운다. 실패는 `log`에 쓴다.
pub(crate) fn start_for(guest: Guest, shares: Vec<Share>, log: PathBuf) -> Option<Running> {
    start(
        shares,
        Box::new(move |target: &str, paths: &[String]| guest.share_changed(target, paths)),
        Notes::about_in(TOPIC, log),
    )
}

#[cfg(test)]
mod tests;
