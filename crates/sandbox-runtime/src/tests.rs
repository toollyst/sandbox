//! 실행 코어 테스트. 같은 프로세스 안의 fake 실행기가 실제 게스트 처리를 호스트에서 돌린다.
//! 그래서 "게스트 경로"는 호스트의 임시 경로이고, 실행 사용자는 지금 사용자다.

use super::*;

use std::collections::BTreeMap;
use std::time::Instant;

use executor_fake::FakeExecutor;
use sandbox_model::{Digest, ImageId, Lifetime, Mount, NetworkPolicy, Resources, RootfsFormat};

use crate::chan::block_on;

struct Env {
    root: PathBuf,
    rt: Runtime,
    uid: u32,
    gid: u32,
}

impl Env {
    fn new(name: &str) -> Self {
        // Unix 소켓 길이 제한 때문에 짧은 경로를 쓴다.
        let root = PathBuf::from(format!("/tmp/sbx-rt-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("initramfs"), b"guest").unwrap();
        let rt = Runtime::new(
            RuntimeConfig {
                data_dir: root.join("d"),
                cache_dir: root.join("c"),
                guest_init: root.join("initramfs"),
                socket_dir: root.join("s"),
            },
            Arc::new(FakeExecutor::basic()),
        )
        .unwrap();
        // fake 게스트는 지금 사용자로 돈다. 디렉터리 소유 그룹은 부모를 따를 수 있으므로(macOS) id로 구한다.
        let id = |flag: &str| -> u32 {
            std::process::Command::new("id")
                .arg(flag)
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0)
        };
        let (uid, gid) = (id("-u"), id("-g"));
        Self { root, rt, uid, gid }
    }

    fn image() -> ImageId {
        ImageId::new(Digest::parse(&format!("sha256:{}", "ef".repeat(32))).unwrap())
    }

    /// 관리 쪽이 해 두었을 일: 이미지 기록, rootfs, 잠금 파일.
    fn prepare_image(&self) {
        let layout = self.rt.inner.store.layout();
        let id = Self::image();
        std::fs::create_dir_all(layout.image_dir(&id)).unwrap();
        std::fs::write(layout.image_record(&id), b"{}").unwrap();
        std::fs::write(layout.rootfs(&id, RootfsFormat::Erofs), b"fs").unwrap();
        std::fs::write(layout.image_lock(&id), b"").unwrap();
    }

    fn spec(&self, args: &[&str]) -> SandboxSpec {
        SandboxSpec {
            image: Self::image(),
            rootfs_format: RootfsFormat::Erofs,
            process: ProcessDefaults {
                args: args.iter().map(|s| s.to_string()).collect(),
                env: vec![("PATH".into(), "/usr/bin:/bin".into())],
                cwd: "/".into(),
                user: UserSpec {
                    user: Ident::Id(self.uid),
                    group: Some(Ident::Id(self.gid)),
                },
            },
            resources: Resources {
                vcpus: 1,
                memory_mib: 128,
                scratch_disk_mib: 4,
            },
            mounts: vec![],
            network: NetworkPolicy::Disabled,
            lifetime: Lifetime::Attached,
            hostname: None,
            labels: BTreeMap::new(),
        }
    }

    fn sandbox(&self, args: &[&str]) -> Sandbox {
        self.prepare_image();
        block_on(self.rt.create(self.spec(args))).unwrap()
    }

    fn state(&self, s: &Sandbox) -> SandboxInfo {
        block_on(self.rt.get(s.id())).unwrap().info().clone()
    }

    fn wait_state(&self, s: &Sandbox, state: State) -> SandboxInfo {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let info = self.state(s);
            if info.state == state {
                return info;
            }
            assert!(Instant::now() < deadline, "still {:?}", info.state);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn exec(args: &[&str]) -> ExecSpec {
    ExecSpec {
        args: args.iter().map(|s| s.to_string()).collect(),
        env: vec![],
        cwd: None,
        user: None,
        tty: false,
    }
}

/// 끝까지 읽는다. (stdout, stderr, 종료 코드)
fn collect(mut p: Process) -> (String, String, Option<i32>) {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    while let Some(o) = block_on(p.next()) {
        match o {
            Output::Stdout(b) => out.extend(b),
            Output::Stderr(b) => err.extend(b),
        }
    }
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        p.exit_code(),
    )
}

#[test]
fn create_does_not_pull_missing_image() {
    let e = Env::new("nopull");
    assert!(matches!(
        block_on(e.rt.create(e.spec(&["sh"]))),
        Err(RuntimeError::ImageNotPulled(_))
    ));
    // 캐시를 만들지도 않는다.
    assert!(!e.root.join("c").exists());
}

#[test]
fn create_does_not_prepare_missing_rootfs() {
    let e = Env::new("noprep");
    let layout = e.rt.inner.store.layout();
    let id = Env::image();
    std::fs::create_dir_all(layout.image_dir(&id)).unwrap();
    std::fs::write(layout.image_record(&id), b"{}").unwrap();
    assert!(matches!(
        block_on(e.rt.create(e.spec(&["sh"]))),
        Err(RuntimeError::RootfsNotPrepared { .. })
    ));
}

#[test]
fn create_writes_records_and_sparse_scratch() {
    let e = Env::new("create");
    let s = e.sandbox(&["sh"]);
    assert_eq!(s.state(), State::Created);
    assert_eq!(s.id().as_str().len(), 64);
    let paths = e.rt.paths(s.id());
    assert_eq!(std::fs::metadata(paths.scratch()).unwrap().len(), 4 << 20);
    let list = block_on(e.rt.list()).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].spec, e.spec(&["sh"]));
}

#[cfg(unix)]
#[test]
fn main_process_exit_stops_the_sandbox_with_its_code() {
    let e = Env::new("mainexit");
    let s = e.sandbox(&["sh", "-c", "echo hi; exit 3"]);
    let main = block_on(s.start(false)).unwrap();
    assert_eq!(collect(main), ("hi\n".into(), "".into(), Some(3)));
    let info = e.wait_state(&s, State::Stopped);
    assert_eq!(info.exit_code, Some(3));
    assert_eq!(info.error, None);
    // 멈춘 뒤에는 다시 시작할 수 있다(Docker와 같다).
    let main = block_on(s.start(false)).unwrap();
    assert_eq!(block_on(main.wait()).unwrap(), 3);
}

#[cfg(unix)]
#[test]
fn exec_while_running_then_stop() {
    let e = Env::new("exec");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let main = block_on(s.start(false)).unwrap();
    assert_eq!(e.state(&s).state, State::Running);

    let mut x = exec(&["sh", "-c", "echo $FOO; echo err >&2; exit 4"]);
    x.env = vec![("FOO".into(), "bar".into())];
    let p = block_on(s.exec(x)).unwrap();
    assert_eq!(collect(p), ("bar\n".into(), "err\n".into(), Some(4)));

    // 표준 입력.
    let p = block_on(s.exec_with(exec(&["cat"]), true)).unwrap();
    p.write_stdin(b"piped");
    p.close_stdin();
    assert_eq!(collect(p).0, "piped");

    block_on(s.stop(Duration::from_secs(5))).unwrap();
    let info = e.state(&s);
    assert_eq!(
        (info.state, info.exit_code),
        (State::Stopped, Some(EXIT_VM_ENDED))
    );
    assert_eq!(block_on(main.wait()).unwrap(), EXIT_VM_ENDED);
    assert!(matches!(
        block_on(s.exec(exec(&["true"]))),
        Err(RuntimeError::InvalidState { .. })
    ));
    block_on(s.clone().delete()).unwrap();
    assert!(matches!(
        block_on(e.rt.get(s.id())),
        Err(RuntimeError::NotFound(_))
    ));
}

#[cfg(unix)]
#[test]
fn exec_failure_kinds_come_from_the_guest() {
    let e = Env::new("execfail");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let _main = block_on(s.start(false)).unwrap();
    match block_on(s.exec(exec(&["no-such-binary-xyz"]))) {
        Err(RuntimeError::CommandFailed(g)) => {
            assert_eq!(g.kind, sandbox_proto::ErrorKind::NotFound)
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        block_on(s.exec(exec(&[]))),
        Err(RuntimeError::NoCommand)
    ));
    block_on(s.stop(Duration::from_secs(5))).unwrap();
}

#[cfg(unix)]
#[test]
fn start_guards() {
    let e = Env::new("guards");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    std::fs::remove_file(e.root.join("initramfs")).unwrap();
    assert!(matches!(
        block_on(s.start(false)),
        Err(RuntimeError::GuestInitMissing(_))
    ));
    std::fs::write(e.root.join("initramfs"), b"guest").unwrap();
    let _main = block_on(s.start(false)).unwrap();
    assert!(matches!(
        block_on(s.start(false)),
        Err(RuntimeError::InvalidState { .. })
    ));
    assert!(matches!(
        block_on(s.clone().delete()),
        Err(RuntimeError::InvalidState { .. })
    ));
    block_on(s.stop(Duration::from_secs(5))).unwrap();
}

#[test]
fn running_record_without_owner_is_reported_stopped() {
    let e = Env::new("owner");
    let s = e.sandbox(&["sh"]);
    let mut st = StateRecord::new(State::Running);
    st.pid = Some(42);
    records::write_state(&e.rt.paths(s.id()), &st).unwrap();
    let info = e.state(&s);
    assert_eq!(info.state, State::Stopped);
    assert_eq!(info.error.as_deref(), Some(OWNER_GONE));
}

#[test]
fn corrupt_records_are_errors() {
    let e = Env::new("corrupt");
    let s = e.sandbox(&["sh"]);
    std::fs::write(e.rt.paths(s.id()).state(), b"{").unwrap();
    assert!(matches!(
        block_on(e.rt.list()),
        Err(RuntimeError::Corrupt { .. })
    ));
}

#[cfg(unix)]
#[test]
fn copy_follows_docker_cp_rules() {
    let e = Env::new("cp");
    let s = e.sandbox(&["sh", "-c", "sleep 30"]);
    let _main = block_on(s.start(false)).unwrap();
    // 호스트 원본.
    let src = e.root.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), "A").unwrap();
    std::fs::write(src.join("sub/b.txt"), "B").unwrap();
    // "게스트" 쪽(fake라 호스트 경로).
    let g = e.root.join("g");
    std::fs::create_dir_all(&g).unwrap();
    let gs = |p: &str| format!("{}/{p}", g.display());
    let cp_in = |host: &Path, guest: &str| block_on(s.copy_in(host, guest));

    // 파일 → 없는 경로: 그 이름으로.
    cp_in(&src.join("a.txt"), &gs("renamed.txt")).unwrap();
    assert_eq!(std::fs::read_to_string(g.join("renamed.txt")).unwrap(), "A");
    // 파일 → 디렉터리: 그 안에 원본 이름으로.
    cp_in(&src.join("a.txt"), &gs("")).unwrap();
    assert_eq!(std::fs::read_to_string(g.join("a.txt")).unwrap(), "A");
    // 파일 → 없는 디렉터리/: 오류.
    assert!(matches!(
        cp_in(&src.join("a.txt"), &gs("missing/")),
        Err(RuntimeError::Invalid(_))
    ));
    // 디렉터리 → 없는 경로: 그 경로로.
    cp_in(&src, &gs("tree")).unwrap();
    assert_eq!(
        std::fs::read_to_string(g.join("tree/sub/b.txt")).unwrap(),
        "B"
    );
    // 디렉터리 → 있는 디렉터리: 그 안에 원본 이름으로.
    cp_in(&src, &gs("tree")).unwrap();
    assert!(g.join("tree/src/a.txt").is_file());
    // 디렉터리/. → 있는 디렉터리: 내용만.
    std::fs::create_dir(g.join("flat")).unwrap();
    cp_in(&src.join("."), &gs("flat")).unwrap();
    assert!(g.join("flat/a.txt").is_file() && g.join("flat/sub/b.txt").is_file());
    // 디렉터리 → 파일: 오류.
    assert!(matches!(
        cp_in(&src, &gs("a.txt")),
        Err(RuntimeError::Invalid(_))
    ));

    // 게스트 → 호스트.
    let out = e.root.join("out");
    std::fs::create_dir(&out).unwrap();
    block_on(s.copy_out(&gs("tree"), &out.join("t2"))).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("t2/a.txt")).unwrap(), "A");
    block_on(s.copy_out(&gs("tree"), &out)).unwrap();
    assert!(out.join("tree/sub/b.txt").is_file());
    block_on(s.copy_out(&format!("{}/.", gs("flat")), &out.join("t2"))).unwrap();
    assert!(out.join("t2/sub/b.txt").is_file());
    block_on(s.copy_out(&gs("a.txt"), &out.join("copy.txt"))).unwrap();
    assert_eq!(std::fs::read_to_string(out.join("copy.txt")).unwrap(), "A");
    match block_on(s.copy_out(&gs("nope"), &out)) {
        Err(RuntimeError::Guest(g)) => assert_eq!(g.kind, sandbox_proto::ErrorKind::NotFound),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        block_on(s.copy_in(&src, "relative")),
        Err(RuntimeError::Invalid(_))
    ));
    block_on(s.stop(Duration::from_secs(5))).unwrap();
}

#[cfg(unix)]
#[test]
fn copy_mounts_are_copied_before_the_main_process() {
    let e = Env::new("mount");
    let src = e.root.join("work-src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("input"), "data").unwrap();
    let dest = e.root.join("work");
    e.prepare_image();
    let mut spec = e.spec(&["sh", "-c", &format!("cat {}/input", dest.display())]);
    spec.mounts = vec![Mount {
        source: src,
        destination: dest.to_str().unwrap().into(),
        kind: MountKind::Copy,
        readonly: false,
    }];
    let s = block_on(e.rt.create(spec)).unwrap();
    let main = block_on(s.start(false)).unwrap();
    assert_eq!(collect(main), ("data".into(), "".into(), Some(0)));
}

#[cfg(unix)]
#[test]
fn main_command_that_cannot_start_leaves_the_sandbox_created() {
    let e = Env::new("mainfail");
    let s = e.sandbox(&["no-such-binary-xyz"]);
    match block_on(s.start(false)) {
        Err(RuntimeError::CommandFailed(g)) => {
            assert_eq!(g.kind, sandbox_proto::ErrorKind::NotFound)
        }
        other => panic!("{other:?}"),
    }
    let info = e.state(&s);
    assert_eq!(info.state, State::Created);
    assert!(info.error.is_some());
}

#[cfg(unix)]
#[test]
fn main_exit_is_reported_after_the_sandbox_is_stopped() {
    let e = Env::new("mainorder");
    let s = e.sandbox(&["sh", "-c", "exit 3"]);
    let main = block_on(s.start(false)).unwrap();
    assert_eq!(block_on(main.wait()).unwrap(), 3);
    // 기다리지 않고 곧바로: 이미 stopped이고 지울 수 있다.
    let info = e.state(&s);
    assert_eq!(info.state, State::Stopped);
    assert_eq!(info.exit_code, Some(3));
    block_on(block_on(e.rt.get(s.id())).unwrap().delete()).unwrap();
}
