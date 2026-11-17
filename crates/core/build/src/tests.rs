//! 빌드 코어의 테스트. 저장소와 VM은 메모리 안의 가짜다. 가짜 VM의 `RUN`은 테스트가 준 함수가 정한 파일을 층으로 낸다.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sbx_model::build::{
    BuildPlan, Command, CopyKind, CopySource, CopyStep, RunStep, Stage, StageBase, Step, StepKind,
};
use sbx_model::{Arch, BoxFuture, Compression, Digest, ImageId, Layer};
use sbx_rootfs::{ContentRef, EntryKind, LayerEntry, Meta};
use sha2::{Digest as _, Sha256};

use super::*;
use crate::source::{clean, glob_match};

fn sha(b: &[u8]) -> Digest {
    Digest::parse(&format!("sha256:{}", source::hex(&Sha256::digest(b)))).unwrap()
}

/// 임시 디렉터리(테스트가 끝나면 지운다).
struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "sbx-build-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn write(&self, rel: &str, data: &str) {
        let p = self.0.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// tar를 층 항목으로 읽는다(테스트용, 일반 파일·디렉터리·링크).
fn read_tar(bytes: &[u8], layer: usize) -> (Vec<LayerEntry>, HashMap<usize, Vec<u8>>) {
    let mut a = tar::Archive::new(bytes);
    let mut entries = Vec::new();
    let mut data = HashMap::new();
    for (index, e) in a.entries().unwrap().enumerate() {
        let mut e = e.unwrap();
        let h = e.header().clone();
        let path = e.path().unwrap().to_string_lossy().into_owned();
        let meta = Meta {
            mode: h.mode().unwrap(),
            uid: h.uid().unwrap() as u32,
            gid: h.gid().unwrap() as u32,
            mtime: h.mtime().unwrap() as i64,
            xattrs: BTreeMap::new(),
        };
        let kind = match h.entry_type() {
            tar::EntryType::Directory => EntryKind::Dir,
            tar::EntryType::Symlink => {
                EntryKind::Symlink(e.link_name().unwrap().unwrap().to_string_lossy().into())
            }
            _ => {
                let mut b = Vec::new();
                e.read_to_end(&mut b).unwrap();
                let size = b.len() as u64;
                data.insert(index, b);
                EntryKind::File(ContentRef { layer, index, size })
            }
        };
        entries.push(LayerEntry { path, kind, meta });
    }
    (entries, data)
}

struct MemContents(HashMap<usize, Vec<u8>>);

impl Contents for MemContents {
    fn open(&self, c: &ContentRef) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(io::Cursor::new(
            self.0.get(&c.index).cloned().unwrap_or_default(),
        )))
    }
}

#[derive(Default)]
struct StoreState {
    layers: HashMap<Digest, Vec<u8>>,
    cache: HashMap<String, CacheRecord>,
    images: HashMap<String, Base>,
    committed: Vec<Built>,
    /// `add_layer`가 받은 `clamp`들.
    clamps: Vec<Option<i64>>,
}

struct MemStore {
    work: Temp,
    s: Mutex<StoreState>,
}

impl MemStore {
    fn new() -> Self {
        Self {
            work: Temp::new("work"),
            s: Mutex::default(),
        }
    }

    /// 파일들로 바탕 이미지 하나를 만든다.
    fn add_image(&self, reference: &str, files: &[(&str, &str)], config: &str) {
        let mut items = copy::Placement::new();
        for (path, data) in files {
            let mut cur = String::new();
            let parts: Vec<&str> = path.split('/').collect();
            for d in &parts[..parts.len() - 1] {
                cur = if cur.is_empty() {
                    (*d).into()
                } else {
                    format!("{cur}/{d}")
                };
                items.insert(cur.clone(), item(copy::ItemKind::Dir, 0o755));
            }
            let bytes = data.as_bytes().to_vec();
            let content = source::Content::Layer(
                ContentRef {
                    layer: 0,
                    index: 0,
                    size: bytes.len() as u64,
                },
                Arc::new(MemContents(HashMap::from([(0, bytes.clone())]))),
            );
            items.insert(
                (*path).into(),
                item(
                    copy::ItemKind::File {
                        size: bytes.len() as u64,
                        content,
                    },
                    0o644,
                ),
            );
        }
        let mut tar = Vec::new();
        tarw::write(&items, &mut tar).unwrap();
        let diff = sha(&tar);
        let layer = Layer {
            digest: diff.clone(),
            diff_id: diff.clone(),
            size: tar.len() as u64,
            compression: Compression::None,
        };
        let doc = format!(
            r#"{{"architecture":"arm64","os":"linux","created":"2024-01-01T00:00:00Z","config":{config},"rootfs":{{"type":"layers","diff_ids":["{diff}"]}}}}"#
        );
        let mut s = self.s.lock().unwrap();
        s.layers.insert(diff, tar);
        s.images.insert(
            reference.into(),
            Base {
                id: ImageId::new(sha(reference.as_bytes())),
                layers: vec![layer],
                config: Some(doc.into_bytes()),
                fallback: Default::default(),
            },
        );
    }

    fn last(&self) -> Built {
        self.s.lock().unwrap().committed.last().cloned().unwrap()
    }

    /// 마지막 이미지를 합친 모습: 경로 → (종류, 권한, 소유자, 내용).
    fn files(&self) -> BTreeMap<String, String> {
        let built = self.last();
        let mut tree = sbx_rootfs::MergedTree::new();
        let mut data = HashMap::new();
        for (i, l) in built.layers.iter().enumerate() {
            let (e, d) = read_tar(&self.s.lock().unwrap().layers[&l.diff_id], i);
            tree.apply_layer(e).unwrap();
            data.insert(i, d);
        }
        tree.iter()
            .filter(|(p, _)| !p.is_empty())
            .map(|(p, n)| {
                let k = match &n.kind {
                    sbx_rootfs::NodeKind::File(c) => {
                        format!(
                            "file {:?}",
                            String::from_utf8_lossy(&data[&c.layer][&c.index])
                        )
                    }
                    sbx_rootfs::NodeKind::Dir => "dir".into(),
                    sbx_rootfs::NodeKind::Symlink(t) => format!("link {t}"),
                    k => format!("{k:?}"),
                };
                (
                    p.to_owned(),
                    format!("{k} {:o} {}:{}", n.meta.mode, n.meta.uid, n.meta.gid),
                )
            })
            .collect()
    }

    fn config(&self) -> serde_json::Value {
        serde_json::from_slice(&self.last().config_json).unwrap()
    }
}

fn item(kind: copy::ItemKind, mode: u32) -> copy::Item {
    copy::Item {
        kind,
        mode,
        uid: 0,
        gid: 0,
        mtime: 1_700_000_000,
        existing: false,
    }
}

impl Store for MemStore {
    fn image<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Result<Base, BuildError>> {
        Box::pin(async move {
            self.s
                .lock()
                .unwrap()
                .images
                .get(reference)
                .cloned()
                .ok_or_else(|| BuildError::Base {
                    reference: reference.into(),
                    message: "not found".into(),
                })
        })
    }
    fn cache_get(&self, key: &str) -> Result<Option<CacheRecord>, BuildError> {
        Ok(self.s.lock().unwrap().cache.get(key).cloned())
    }
    fn cache_put(&self, key: &str, record: &CacheRecord) -> Result<(), BuildError> {
        self.s
            .lock()
            .unwrap()
            .cache
            .insert(key.into(), record.clone());
        Ok(())
    }
    fn add_layer(&self, tar: &Path, clamp: Option<i64>) -> Result<Layer, BuildError> {
        self.s.lock().unwrap().clamps.push(clamp);
        let bytes = std::fs::read(tar)?;
        let diff = sha(&bytes);
        let layer = Layer {
            digest: diff.clone(),
            diff_id: diff.clone(),
            size: bytes.len() as u64,
            compression: Compression::None,
        };
        self.s.lock().unwrap().layers.insert(diff, bytes);
        Ok(layer)
    }
    fn read_layer(
        &self,
        layer: &Layer,
        index: usize,
    ) -> Result<(Vec<LayerEntry>, Arc<dyn Contents>), BuildError> {
        let bytes = self.s.lock().unwrap().layers[&layer.diff_id].clone();
        let (e, d) = read_tar(&bytes, index);
        Ok((e, Arc::new(MemContents(d))))
    }
    fn work_dir(&self) -> &Path {
        &self.work.0
    }
    fn commit(&self, image: &Built) -> Result<ImageId, BuildError> {
        self.s.lock().unwrap().committed.push(image.clone());
        Ok(ImageId::new(sha(&image.config_json)))
    }
}

/// `RUN`의 결과를 정하는 함수: (종료 코드, 출력, 층에 낼 파일).
type RunFn = Arc<dyn Fn(&RunSpec) -> (i32, String, Vec<(String, String)>) + Send + Sync>;

#[derive(Default)]
struct Log {
    started: Vec<Vec<Digest>>,
    runs: Vec<RunSpec>,
    puts: Vec<Vec<String>>,
    finished: usize,
}

struct FakeMachines {
    run: RunFn,
    log: Arc<Mutex<Log>>,
}

struct FakeVm {
    run: RunFn,
    log: Arc<Mutex<Log>>,
    pending: Vec<(String, String)>,
}

impl Machines for FakeMachines {
    fn start<'a>(&'a self, layers: &'a [Layer]) -> BoxFuture<'a, Result<Box<dyn Vm>, BuildError>> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap()
                .started
                .push(layers.iter().map(|l| l.diff_id.clone()).collect());
            Ok(Box::new(FakeVm {
                run: self.run.clone(),
                log: self.log.clone(),
                pending: Vec::new(),
            }) as Box<dyn Vm>)
        })
    }
}

impl Vm for FakeVm {
    fn open_layer(&mut self) -> BoxFuture<'_, Result<(), BuildError>> {
        self.pending.clear();
        Box::pin(async { Ok(()) })
    }
    fn run(&mut self, spec: RunSpec, output: OutputSink) -> BoxFuture<'_, Result<i32, BuildError>> {
        Box::pin(async move {
            let (code, out, files) = (self.run)(&spec);
            output(Stream::Stdout, out.as_bytes());
            self.log.lock().unwrap().runs.push(spec);
            self.pending = files;
            Ok(code)
        })
    }
    fn export<'a>(&'a mut self, tar: &'a Path) -> BoxFuture<'a, Result<(), BuildError>> {
        Box::pin(async move {
            let mut items = copy::Placement::new();
            for (path, data) in &self.pending {
                let bytes = data.as_bytes().to_vec();
                items.insert(
                    path.clone(),
                    item(
                        copy::ItemKind::File {
                            size: bytes.len() as u64,
                            content: source::Content::Layer(
                                ContentRef {
                                    layer: 0,
                                    index: 0,
                                    size: bytes.len() as u64,
                                },
                                Arc::new(MemContents(HashMap::from([(0, bytes)]))),
                            ),
                        },
                        0o644,
                    ),
                );
            }
            tarw::write(&items, std::fs::File::create(tar)?)?;
            Ok(())
        })
    }
    fn put(&mut self, entries: Vec<PutEntry>) -> BoxFuture<'_, Result<(), BuildError>> {
        Box::pin(async move {
            self.log
                .lock()
                .unwrap()
                .puts
                .push(entries.iter().map(|e| e.path.clone()).collect());
            Ok(())
        })
    }
    fn blocked(&mut self) -> BoxFuture<'_, Vec<String>> {
        Box::pin(async { vec!["example.com".into()] })
    }
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), BuildError>> {
        self.log.lock().unwrap().finished += 1;
        Box::pin(async { Ok(()) })
    }
}

/// 테스트 하나의 무대: 컨텍스트, 저장소, VM.
struct Rig {
    ctx: Temp,
    store: MemStore,
    machines: FakeMachines,
    events: Arc<Mutex<Vec<Event>>>,
    /// `SOURCE_DATE_EPOCH`.
    epoch: Option<i64>,
    /// 저장소에 둔다(`type=sbx`).
    keep: bool,
}

impl Rig {
    fn new(run: RunFn) -> Self {
        let store = MemStore::new();
        store.add_image(
            "alpine",
            &[
                (
                    "etc/passwd",
                    "root:x:0:0::/root:/bin/sh\nlp:x:4:7::/:/bin/sh\n",
                ),
                ("etc/group", "root:x:0:\nlp:x:7:\n"),
                ("bin/sh", "#!"),
            ],
            r#"{"Env":["PATH=/bin"],"Cmd":["/bin/sh"],"WorkingDir":"/"}"#,
        );
        Self {
            ctx: Temp::new("ctx"),
            store,
            machines: FakeMachines {
                run,
                log: Arc::default(),
            },
            events: Arc::default(),
            epoch: None,
            keep: true,
        }
    }

    fn build(&self, plan: BuildPlan, no_cache: bool) -> Result<Outcome, BuildError> {
        self.build_with(plan, no_cache, None)
    }

    fn build_with(
        &self,
        plan: BuildPlan,
        no_cache: bool,
        ignore: Option<Arc<dyn Ignore>>,
    ) -> Result<Outcome, BuildError> {
        let req = Request {
            plan,
            context: self.ctx.0.clone(),
            ignore,
            no_cache,
            arch: Arch::Aarch64,
            source_date_epoch: self.epoch,
            store: self.keep,
        };
        let ev = self.events.clone();
        block_on(build(
            &req,
            &self.store,
            &self.machines,
            Arc::new(move |e| ev.lock().unwrap().push(e)),
        ))
    }

    fn vms(&self) -> usize {
        self.machines.log.lock().unwrap().started.len()
    }
}

/// 아무 런타임 없이 future를 끝까지 돌린다(가짜들은 기다리지 않는다).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut cx = Context::from_waker(Waker::noop());
    let mut f = Box::pin(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

fn stage(base: StageBase, name: Option<&str>, steps: Vec<Step>) -> Stage {
    Stage {
        name: name.map(Into::into),
        position: 0,
        line: 1,
        text: "FROM x".into(),
        base,
        steps,
    }
}

fn plan(stages: Vec<Stage>) -> BuildPlan {
    BuildPlan {
        stages,
        warnings: vec![],
    }
}

fn run_step(script: &str) -> Step {
    run_args(script, &[])
}

fn run_args(script: &str, args: &[(&str, &str)]) -> Step {
    Step {
        line: 2,
        text: format!("RUN {script}"),
        kind: StepKind::Run(RunStep {
            command: Command::Shell {
                shell: vec!["/bin/sh".into(), "-c".into()],
                script: script.into(),
            },
            args: args
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect(),
        }),
    }
}

fn copy_step(from: CopySource, sources: &[&str], dest: &str) -> Step {
    copy_with(CopyKind::Copy, from, sources, dest, None, None)
}

fn copy_with(
    kind: CopyKind,
    from: CopySource,
    sources: &[&str],
    dest: &str,
    chown: Option<&str>,
    chmod: Option<u32>,
) -> Step {
    Step {
        line: 3,
        text: format!("COPY {} {dest}", sources.join(" ")),
        kind: StepKind::Copy(CopyStep {
            kind,
            from,
            sources: sources.iter().map(|s| (*s).into()).collect(),
            dest: dest.into(),
            chown: chown.map(|c| c.parse().unwrap()),
            chmod,
        }),
    }
}

fn simple(step: StepKind, text: &str) -> Step {
    Step {
        line: 4,
        text: text.into(),
        kind: step,
    }
}

/// `RUN echo X > F`처럼: 스크립트가 `make F`이면 파일 F에 환경 `V`의 값을 쓴다. `fail`이면 3으로 끝난다.
fn script() -> RunFn {
    Arc::new(|spec: &RunSpec| {
        let s = spec.args.last().cloned().unwrap_or_default();
        if s == "fail" {
            return (3, "boom\n".into(), vec![]);
        }
        let v = spec
            .env
            .iter()
            .find(|(k, _)| k == "V")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        match s.strip_prefix("make ") {
            Some(f) => (0, format!("made {f}\n"), vec![(f.into(), v)]),
            None => (0, String::new(), vec![]),
        }
    })
}

#[test]
fn globs_follow_go_filepath_match() {
    let m = |p: &str, n: &str| glob_match(p, n).unwrap();
    assert!(m("*.txt", "a.txt"));
    assert!(!m("*.txt", "a.txt.bak"));
    assert!(m("a?c", "abc"));
    assert!(m("[a-c]x", "bx"));
    assert!(!m("[^a-c]x", "bx"));
    assert!(m("\\*", "*"));
    assert!(m("*", ""));
    assert!(m("a*b*c", "aXbYc"));
    assert!(glob_match("[", "a").is_err());
    assert!(glob_match("[z-a]", "b").is_err());
    assert_eq!(clean("../../x/./y/../z"), ["x", "z"]);
    assert_eq!(clean("/"), Vec::<String>::new());
}

struct Exclude(Vec<&'static str>);

impl Ignore for Exclude {
    fn is_excluded(&self, path: &str) -> bool {
        self.0
            .iter()
            .any(|p| path == *p || path.starts_with(&format!("{p}/")))
    }
    fn has_exceptions(&self) -> bool {
        false
    }
}

#[test]
fn context_sources_stay_inside_the_context_and_honor_dockerignore() {
    let t = Temp::new("find");
    t.write("a.txt", "a");
    t.write("dir/x", "x");
    t.write("dir/secret", "s");
    t.write("node_modules/m", "m");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", t.0.join("hosts")).unwrap();
        std::os::unix::fs::symlink("../a.txt", t.0.join("dir/up")).unwrap();
        std::os::unix::fs::symlink("/a.txt", t.0.join("abs")).unwrap();
    }
    let ctx = Context::new(
        t.0.clone(),
        Some(Arc::new(Exclude(vec!["dir/secret", "node_modules"]))),
    );
    // 밖으로 나가는 경로는 컨텍스트 안의 같은 경로다.
    let s = ctx.find("../../a.txt").unwrap();
    assert_eq!((s.len(), s[0].name.as_str()), (1, "a.txt"));
    // 빠진 것을 이름으로 부르면 없다. 디렉터리 안에서는 조용히 빠진다.
    assert!(ctx.find("node_modules").unwrap_err().contains("not found"));
    assert!(ctx.find("dir/secret").unwrap_err().contains("not found"));
    let d = &ctx.find("dir").unwrap()[0];
    let rels: Vec<&str> = d.entries.iter().map(|e| e.rel.as_str()).collect();
    // 링크(`up`)는 Unix에서만 만든다.
    let want: &[&str] = if cfg!(unix) {
        &["", "up", "x"]
    } else {
        &["", "x"]
    };
    assert_eq!(rels, want);
    // 와일드카드는 맞는 것이 없어도 오류가 아니다.
    assert!(ctx.find("*.none").unwrap().is_empty());
    assert_eq!(ctx.find("*.txt").unwrap().len(), 1);
    assert!(
        ctx.find("missing")
            .unwrap_err()
            .contains("\"/missing\": not found")
    );
    #[cfg(unix)]
    {
        // 컨텍스트 밖을 가리키는 링크는 컨텍스트 안에서 푼다: /etc/hosts는 컨텍스트의 etc/hosts이고 없다.
        assert!(ctx.find("hosts").unwrap_err().contains("not found"));
        // 절대 경로 링크도 컨텍스트가 뿌리다.
        let s = ctx.find("abs").unwrap();
        assert!(matches!(
            s[0].entries[0].kind,
            SrcKind::File { size: 1, .. }
        ));
        // 디렉터리 안의 링크는 따라가지 않고 링크로 복사한다.
        assert!(matches!(&d.entries[1].kind, SrcKind::Symlink(t) if t == "../a.txt"));
    }
}

fn tree(files: &[(&str, bool)]) -> sbx_rootfs::MergedTree {
    let mut t = sbx_rootfs::MergedTree::new();
    let entries = files
        .iter()
        .map(|(p, dir)| LayerEntry {
            path: (*p).into(),
            kind: if *dir {
                EntryKind::Dir
            } else {
                EntryKind::File(ContentRef {
                    layer: 0,
                    index: 0,
                    size: 0,
                })
            },
            meta: Meta {
                mode: 0o750,
                uid: 9,
                gid: 9,
                mtime: 5,
                xattrs: BTreeMap::new(),
            },
        })
        .collect();
    t.apply_layer(entries).unwrap();
    t
}

fn src_file(name: &str) -> Src {
    Src {
        name: name.into(),
        entries: vec![source::SrcEntry {
            rel: String::new(),
            kind: SrcKind::File {
                size: 0,
                content: source::Content::Host(PathBuf::from("/nonexistent"), None),
            },
            mode: 0o640,
            mtime: 7,
        }],
    }
}

fn src_dir(name: &str, children: &[&str]) -> Src {
    let mut entries = vec![source::SrcEntry {
        rel: String::new(),
        kind: SrcKind::Dir,
        mode: 0o700,
        mtime: 8,
    }];
    for c in children {
        entries.extend(src_file(c).entries.into_iter().map(|mut e| {
            e.rel = (*c).into();
            e
        }));
    }
    Src {
        name: name.into(),
        entries,
    }
}

fn shown(p: &copy::Placement) -> Vec<String> {
    p.iter()
        .map(|(k, i)| {
            let kind = if matches!(i.kind, copy::ItemKind::Dir) {
                "d"
            } else {
                "f"
            };
            let new = if i.existing { "old" } else { "new" };
            format!(
                "{k} {kind} {:o} {}:{} {} {new}",
                i.mode, i.uid, i.gid, i.mtime
            )
        })
        .collect()
}

#[test]
fn copy_destinations_follow_docker() {
    let t = tree(&[
        ("tmp", true),
        ("etc", true),
        ("usr", true),
        ("usr/bin", true),
    ]);
    let opts = copy::Options {
        owner: None,
        chmod: None,
        now: 100,
    };
    // 있는 디렉터리 안으로, 원래 이름으로. 있는 조상은 메타데이터 그대로 들어간다.
    let p = copy::place(&t, &[src_file("a.txt")], "/tmp", opts).unwrap();
    assert_eq!(
        shown(&p),
        ["tmp d 750 9:9 5 old", "tmp/a.txt f 640 0:0 7 new"]
    );
    // 없는 이름이면 그 이름의 파일. 새 부모는 0755, root, 빌드 시각.
    let p = copy::place(&t, &[src_file("a.txt")], "/new/name", opts).unwrap();
    assert_eq!(
        shown(&p),
        ["new d 755 0:0 100 new", "new/name f 640 0:0 7 new"]
    );
    // `/`로 끝나면 디렉터리.
    let p = copy::place(&t, &[src_file("a.txt")], "/new/", opts).unwrap();
    assert_eq!(
        shown(&p),
        ["new d 755 0:0 100 new", "new/a.txt f 640 0:0 7 new"]
    );
    // 디렉터리는 안의 것을 놓는다. 새 대상 디렉터리는 0755와 원본의 시각.
    let p = copy::place(&t, &[src_dir("d", &["x"])], "/x", opts).unwrap();
    assert_eq!(shown(&p), ["x d 755 0:0 8 new", "x/x f 640 0:0 7 new"]);
    // 원본이 둘인데 대상이 디렉터리가 아니면 오류다.
    let e = copy::place(&t, &[src_file("a"), src_file("b")], "/f", opts).unwrap_err();
    assert!(e.contains("must be a directory and end with a /"), "{e}");
    // --chown, --chmod는 새 부모와 놓는 것 모두에.
    let o = copy::Options {
        owner: Some((4, 7)),
        chmod: Some(0o600),
        now: 100,
    };
    let p = copy::place(&t, &[src_file("a")], "/n/m/", o).unwrap();
    assert_eq!(
        shown(&p),
        [
            "n d 600 4:7 100 new",
            "n/m d 600 4:7 100 new",
            "n/m/a f 600 4:7 7 new"
        ]
    );
    // 디렉터리 자리에 파일은 오류다.
    let t2 = tree(&[("x", true), ("x/sub", true)]);
    let e = copy::place(&t2, &[src_dir("d", &["sub"])], "/x", opts).unwrap_err();
    assert!(e.contains("directory"), "{e}");
    // 있는 파일 아래에는 놓지 못한다.
    let t3 = tree(&[("f", false)]);
    assert!(copy::place(&t3, &[src_file("a")], "/f/", opts).is_err());
}

#[test]
fn copy_destinations_follow_links_inside_the_image() {
    let mut t = tree(&[("usr", true), ("usr/bin", true)]);
    t.apply_layer(vec![LayerEntry {
        path: "bin".into(),
        kind: EntryKind::Symlink("usr/bin".into()),
        meta: Meta::implicit_dir(),
    }])
    .unwrap();
    let opts = copy::Options {
        owner: None,
        chmod: None,
        now: 1,
    };
    let p = copy::place(&t, &[src_file("tool")], "/bin/", opts).unwrap();
    assert_eq!(
        p.keys().collect::<Vec<_>>(),
        ["usr", "usr/bin", "usr/bin/tool"]
    );
}

fn two_stages() -> BuildPlan {
    plan(vec![
        stage(
            StageBase::Image("alpine".into()),
            Some("build"),
            vec![
                copy_step(CopySource::Context, &["src.txt"], "/src/"),
                run_args("make out", &[("V", "built")]),
            ],
        ),
        stage(
            StageBase::Image("alpine".into()),
            None,
            vec![
                copy_step(CopySource::Stage(0), &["/out"], "/app/out"),
                simple(
                    StepKind::Cmd(Command::Exec(vec!["/app/out".into()])),
                    "CMD [\"/app/out\"]",
                ),
            ],
        ),
    ])
}

/// `SOURCE_DATE_EPOCH`: 새 history와 `created`가 그 시각이고, 층의 시각을 누르라고 저장소에 알리고, 다른 값이면 다른
/// 캐시다. 저장하지 않는 빌드(`type=docker`만 고른 것)는 저장소에 두지 않고 같은 ID를 돌려준다.
#[test]
fn source_date_epoch_fixes_the_times_and_is_part_of_the_cache_key() {
    let mut r = Rig::new(script());
    r.ctx.write("src.txt", "source");
    r.epoch = Some(1_000_000_000);
    let first = r.build(two_stages(), false).unwrap();
    let cfg = r.store.config();
    assert_eq!(cfg["created"], "2001-09-09T01:46:40Z");
    let history = cfg["history"].as_array().unwrap();
    let ours: Vec<&serde_json::Value> = history.iter().skip(history.len() - 2).collect();
    assert!(
        ours.iter().all(|h| h["created"] == "2001-09-09T01:46:40Z"),
        "{ours:?}"
    );
    assert!(
        r.store
            .s
            .lock()
            .unwrap()
            .clamps
            .iter()
            .all(|c| *c == Some(1_000_000_000))
    );
    // 같은 값이면 캐시, 같은 ID.
    let again = r.build(two_stages(), false).unwrap();
    assert_eq!((again.cached, again.id.clone()), (3, first.id.clone()));
    // 다른 값이면 다른 층.
    r.epoch = Some(1_000_000_001);
    let other = r.build(two_stages(), false).unwrap();
    assert_eq!(other.cached, 0);
    assert_ne!(other.id, first.id);
    // 저장하지 않는 빌드.
    r.epoch = Some(1_000_000_000);
    r.keep = false;
    let before = r.store.s.lock().unwrap().committed.len();
    let unstored = r.build(two_stages(), false).unwrap();
    assert_eq!(r.store.s.lock().unwrap().committed.len(), before);
    assert_eq!((unstored.stored, unstored.id), (false, first.id));
}

#[test]
fn a_multi_stage_build_copies_between_stages_and_caches_every_step() {
    let r = Rig::new(script());
    r.ctx.write("src.txt", "source");
    let first = r.build(two_stages(), false).unwrap();
    assert_eq!((first.vms, first.cached), (1, 0));
    let files = r.store.files();
    assert_eq!(files["app/out"], "file \"built\" 644 0:0");
    assert!(!files.contains_key("src"), "{files:?}");
    let cfg = r.store.config();
    assert_eq!(cfg["config"]["Cmd"], serde_json::json!(["/app/out"]));
    assert_eq!(cfg["rootfs"]["diff_ids"].as_array().unwrap().len(), 2);
    // RUN은 ARG를 환경에 넣는다. 작업 디렉터리와 사용자는 config의 것이다.
    let runs = r.machines.log.lock().unwrap().runs.clone();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].env.contains(&("V".into(), "built".into())));
    assert_eq!((runs[0].cwd.as_str(), runs[0].user.as_str()), ("/", ""));
    // VM은 그때까지의 층(바탕 + COPY)으로 떴고 끝에 지워졌다.
    let log = r.machines.log.lock().unwrap();
    assert_eq!(log.started[0].len(), 2);
    assert_eq!(log.finished, 1);
    drop(log);

    // 두 번째는 모두 캐시에서 온다: VM을 띄우지 않고 같은 이미지 ID다.
    let second = r.build(two_stages(), false).unwrap();
    assert_eq!((second.vms, second.cached), (0, 3));
    assert_eq!(second.id, first.id);
    assert_eq!(r.vms(), 1);

    // 원본 하나를 바꾸면 그 단계부터 다시 만든다.
    r.ctx.write("src.txt", "changed");
    let third = r.build(two_stages(), false).unwrap();
    assert_eq!((third.vms, third.cached), (1, 0));
    // touch만 하면(내용이 같으면) 캐시에서 온다.
    let fourth = r.build(two_stages(), false).unwrap();
    assert_eq!(fourth.vms, 0);

    // --no-cache는 캐시를 읽지 않는다.
    let fifth = r.build(two_stages(), true).unwrap();
    assert_eq!((fifth.vms, fifth.cached), (1, 0));
}

#[test]
fn build_args_are_part_of_the_cache_key() {
    let r = Rig::new(script());
    let p = |v: &str| {
        plan(vec![stage(
            StageBase::Image("alpine".into()),
            None,
            vec![run_args("make f", &[("V", v)])],
        )])
    };
    r.build(p("1"), false).unwrap();
    assert_eq!(r.build(p("1"), false).unwrap().vms, 0);
    assert_eq!(r.build(p("2"), false).unwrap().vms, 1);
    assert_eq!(r.store.files()["f"], "file \"2\" 644 0:0");
}

#[test]
fn a_failing_run_shows_its_output_code_and_blocked_names() {
    let r = Rig::new(script());
    let p = plan(vec![stage(
        StageBase::Image("alpine".into()),
        None,
        vec![run_step("fail")],
    )]);
    match r.build(p, false).unwrap_err() {
        BuildError::RunFailed {
            code,
            output,
            blocked,
            line,
            ..
        } => {
            assert_eq!((code, output.as_str(), line), (3, "boom\n", 2));
            assert_eq!(blocked, ["example.com"]);
        }
        e => panic!("{e}"),
    }
    // 실패해도 VM은 지운다.
    assert_eq!(r.machines.log.lock().unwrap().finished, 1);
}

#[test]
fn scratch_with_copy_needs_no_vm() {
    let r = Rig::new(script());
    r.ctx.write("bin/app", "app");
    let p = plan(vec![stage(
        StageBase::Scratch,
        None,
        vec![
            copy_step(CopySource::Context, &["bin"], "/"),
            simple(StepKind::Workdir("/data".into()), "WORKDIR /data"),
            simple(StepKind::Workdir("/data".into()), "WORKDIR /data"),
        ],
    )]);
    let out = r.build(p, false).unwrap();
    assert_eq!(out.vms, 0);
    let files = r.store.files();
    assert_eq!(files["app"], "file \"app\" 644 0:0");
    assert_eq!(files["data"], "dir 755 0:0");
    let cfg = r.store.config();
    assert_eq!(cfg["config"]["WorkingDir"], "/data");
    assert_eq!(
        cfg["config"]["Env"],
        serde_json::json!([format!("PATH={}", sbx_model::build::DEFAULT_PATH)])
    );
    // WORKDIR은 이미 있어도 층 하나다(빈 층, BuildKit의 diff_id).
    let ids = cfg["rootfs"]["diff_ids"].as_array().unwrap();
    assert_eq!(ids.len(), 3);
    assert_eq!(
        ids[2],
        "sha256:5f70bf18a086007016e948b04aed3b82103a36bea41755b6cddfaf10ace3c6ef"
    );
    let h = cfg["history"].as_array().unwrap();
    assert_eq!(h[0]["created_by"], "COPY bin / # buildkit");
    assert_eq!(h[1]["created_by"], "WORKDIR /data");
    assert!(h.iter().all(|e| e["comment"] == config::HISTORY_COMMENT));
}

#[test]
fn copy_into_a_running_vm_is_put_there_too_and_owners_resolve_from_the_image() {
    let r = Rig::new(script());
    r.ctx.write("a", "a");
    let p = plan(vec![stage(
        StageBase::Image("alpine".into()),
        None,
        vec![
            run_step("make one"),
            copy_with(
                CopyKind::Copy,
                CopySource::Context,
                &["a"],
                "/srv/",
                Some("lp"),
                None,
            ),
            simple(StepKind::User("lp".into()), "USER lp"),
            simple(StepKind::Workdir("w".into()), "WORKDIR w"),
            run_step("make two"),
        ],
    )]);
    let out = r.build(p, false).unwrap();
    assert_eq!(out.vms, 1);
    let log = r.machines.log.lock().unwrap();
    assert_eq!(log.puts, [vec!["srv", "srv/a"], vec!["w"]]);
    assert_eq!(log.runs[1].cwd, "/w");
    assert_eq!(log.runs[1].user, "lp");
    drop(log);
    let files = r.store.files();
    assert_eq!(files["srv/a"], "file \"a\" 644 4:7");
    assert_eq!(files["srv"], "dir 755 4:7");
    assert_eq!(files["w"], "dir 755 4:7");
}

#[test]
fn entrypoint_clears_the_inherited_cmd_but_not_one_set_before_it() {
    let r = Rig::new(script());
    let ep = |c: &[&str]| {
        simple(
            StepKind::Entrypoint(Command::Exec(c.iter().map(|s| (*s).into()).collect())),
            "ENTRYPOINT",
        )
    };
    let cmd = simple(StepKind::Cmd(Command::Exec(vec!["x".into()])), "CMD");
    r.build(
        plan(vec![stage(
            StageBase::Image("alpine".into()),
            None,
            vec![ep(&["/e"])],
        )]),
        false,
    )
    .unwrap();
    assert_eq!(r.store.config()["config"]["Cmd"], serde_json::Value::Null);
    r.build(
        plan(vec![stage(
            StageBase::Image("alpine".into()),
            None,
            vec![cmd, ep(&["/e"])],
        )]),
        false,
    )
    .unwrap();
    assert_eq!(r.store.config()["config"]["Cmd"], serde_json::json!(["x"]));
    let h = r.store.config()["history"].as_array().unwrap().clone();
    assert_eq!(h.last().unwrap()["created_by"], r#"ENTRYPOINT ["/e"]"#);
    assert_eq!(h.last().unwrap()["empty_layer"], true);
}

#[test]
fn add_refuses_archives_it_would_extract() {
    let r = Rig::new(script());
    let mut tar = Vec::new();
    tarw::write(&copy::Placement::new(), &mut tar).unwrap();
    let mut t = vec![0u8; 512];
    t[257..262].copy_from_slice(b"ustar");
    std::fs::write(r.ctx.0.join("a.tar"), &t).unwrap();
    std::fs::write(r.ctx.0.join("a.gz"), [0x1f, 0x8b, 8, 0]).unwrap();
    r.ctx.write("plain", "x");
    for name in ["a.tar", "a.gz"] {
        let p = plan(vec![stage(
            StageBase::Scratch,
            None,
            vec![copy_with(
                CopyKind::Add,
                CopySource::Context,
                &[name],
                "/",
                None,
                None,
            )],
        )]);
        let e = r.build(p, false).unwrap_err().to_string();
        assert!(e.contains("does not extract archives"), "{e}");
    }
    let p = plan(vec![stage(
        StageBase::Scratch,
        None,
        vec![copy_with(
            CopyKind::Add,
            CopySource::Context,
            &["plain"],
            "/",
            None,
            None,
        )],
    )]);
    r.build(p, false).unwrap();
}

#[test]
fn onbuild_bases_are_refused_and_progress_counts_from() {
    let r = Rig::new(script());
    r.store
        .add_image("trigger", &[("a", "a")], r#"{"OnBuild":["RUN x"]}"#);
    let p = plan(vec![stage(
        StageBase::Image("trigger".into()),
        None,
        vec![],
    )]);
    let e = r.build(p, false).unwrap_err().to_string();
    assert!(e.contains("ONBUILD"), "{e}");

    let r = Rig::new(script());
    r.build(
        plan(vec![stage(
            StageBase::Image("alpine".into()),
            Some("b"),
            vec![run_step("make x")],
        )]),
        false,
    )
    .unwrap();
    let steps: Vec<(usize, usize, StepStatus)> = r
        .events
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::Step(s) => Some((s.index, s.steps, s.status.clone())),
            _ => None,
        })
        .map(|(i, n, s)| {
            let s = match s {
                StepStatus::Done { .. } => StepStatus::Done { millis: 0 },
                s => s,
            };
            (i, n, s)
        })
        .collect();
    assert_eq!(
        steps,
        [
            (1, 2, StepStatus::Started),
            (1, 2, StepStatus::Done { millis: 0 }),
            (2, 2, StepStatus::Started),
            (2, 2, StepStatus::Done { millis: 0 }),
        ]
    );
    assert!(r.events.lock().unwrap().iter().any(|e| matches!(
        e,
        Event::Output { data, .. } if data == b"made x\n"
    )));
}
