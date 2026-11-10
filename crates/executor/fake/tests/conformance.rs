//! 적합성 테스트: `sbx-executor-process` 어댑터가 규약을 지키는 외부 실행기(`sbx-executor-fake`)를 다룬다.
//!
//! 실행기 저장소(fc-fork 등)는 같은 시나리오를 자기 실행 파일로 돌려 규약을 지키는지 확인할 수 있다.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sbx_executor::{ChannelSpec, Disk, DiskRole, Executor, ExecutorError, GuestConn, VmConfig};
use sbx_executor_process::ProcessExecutor;
use sbx_guest_proto::{GUEST_PORT, GuestMsg, HostMsg, PROTOCOL_VERSION};
use sbx_model::RootfsFormat;

const FAKE: &str = env!("CARGO_BIN_EXE_sbx-executor-fake");

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    // 어댑터는 블로킹 작업을 스레드에서 돌리고 깨운다. 그 깨움을 기다리는 가장 작은 실행기.
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake};
    struct Thread(std::thread::Thread);
    impl Wake for Thread {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Thread(std::thread::current())).into();
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(Duration::from_millis(50));
    }
}

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        // Unix 소켓 경로 길이 제한 때문에 짧은 경로를 쓴다.
        let p = PathBuf::from(format!("/tmp/sbx-conf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn vm(&self) -> VmConfig {
        VmConfig {
            guest_init: self.0.join("initramfs"),
            kernel: None,
            disks: vec![
                Disk {
                    path: self.0.join("rootfs.erofs"),
                    role: DiskRole::Rootfs,
                    readonly: true,
                },
                Disk {
                    path: self.0.join("scratch.img"),
                    role: DiskRole::Scratch,
                    readonly: false,
                },
            ],
            vcpus: 1,
            memory_mib: 128,
            shares: vec![],
            control_channel: ChannelSpec {
                host_endpoint: self.0.join("c.sock"),
                guest_port: GUEST_PORT,
            },
            hostname: "conf".into(),
            log: self.0.join("logs/executor.log"),
        }
    }

    /// 환경 변수를 붙여 가짜 실행기를 부르는 래퍼 스크립트.
    fn wrapper(&self, env: &str) -> PathBuf {
        let p = self.0.join("executor.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{env} exec {FAKE} \"$@\"\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn script(&self, body: &str) -> PathBuf {
        let p = self.0.join("bad.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hello(c: &mut Box<dyn GuestConn>) {
    HostMsg::Hello {
        version: PROTOCOL_VERSION,
    }
    .write_to(c)
    .unwrap();
    assert!(matches!(
        GuestMsg::read_from(c).unwrap(),
        GuestMsg::HelloAck { .. }
    ));
}

fn connect_retry(ex: &ProcessExecutor, ch: &ChannelSpec) -> Box<dyn GuestConn> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        match ex.connect(ch) {
            Ok(c) => return c,
            Err(e) if std::time::Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("guest never became reachable: {e}"),
        }
    }
}

#[test]
fn capabilities_are_read_once() {
    let ex = ProcessExecutor::new(FAKE).unwrap();
    let caps = ex.capabilities();
    assert_eq!(caps.name, "fake");
    assert_eq!(caps.rootfs_formats, vec![RootfsFormat::Erofs]);
    assert_eq!(caps.max_disks, 2);
}

#[test]
fn launch_connect_kill() {
    let d = Dir::new("kill");
    let ex = ProcessExecutor::new(FAKE).unwrap();
    let vm = d.vm();
    let mut h = block_on(ex.launch(vm.clone())).unwrap();
    let mut c = connect_retry(&ex, &vm.control_channel);
    hello(&mut c);
    assert!(!block_on(h.wait(Duration::from_millis(50))).unwrap());
    h.kill().unwrap();
    assert!(block_on(h.wait(Duration::from_secs(5))).unwrap());
    assert!(vm.log.is_file(), "executor stderr goes to the log file");
}

#[test]
fn guest_shutdown_ends_the_executor() {
    let d = Dir::new("shutdown");
    let ex = ProcessExecutor::new(FAKE).unwrap();
    let vm = d.vm();
    let mut h = block_on(ex.launch(vm.clone())).unwrap();
    let mut c = connect_retry(&ex, &vm.control_channel);
    hello(&mut c);
    HostMsg::Shutdown.write_to(&mut c).unwrap();
    assert_eq!(GuestMsg::read_from(&mut c).unwrap(), GuestMsg::Done);
    assert!(block_on(h.wait(Duration::from_secs(10))).unwrap());
}

#[test]
fn dropping_the_handle_ends_the_vm() {
    let d = Dir::new("drop");
    let ex = ProcessExecutor::new(FAKE).unwrap();
    let vm = d.vm();
    let h = block_on(ex.launch(vm.clone())).unwrap();
    connect_retry(&ex, &vm.control_channel);
    drop(h);
    // 실행기가 끝나며 제어 소켓을 지웠다.
    assert!(ex.connect(&vm.control_channel).is_err());
}

#[test]
fn launch_receives_the_vm_spec() {
    let d = Dir::new("record");
    let record = d.0.join("record.json");
    let ex =
        ProcessExecutor::new(d.wrapper(&format!("SBX_FAKE_RECORD={}", record.display()))).unwrap();
    let vm = d.vm();
    let _h = block_on(ex.launch(vm.clone())).unwrap();
    let got: sbx_executor_proto::VmSpec =
        serde_json::from_slice(&std::fs::read(&record).unwrap()).unwrap();
    assert_eq!(got, sbx_executor_process::convert_vm(&vm));
    assert_eq!(got.disks[0].role, sbx_executor_proto::DiskRole::Rootfs);
}

#[test]
fn launch_failure_is_reported_with_the_log_path() {
    let d = Dir::new("fail");
    let ex = ProcessExecutor::new(d.wrapper("SBX_FAKE_FAIL_LAUNCH=no-kvm")).unwrap();
    match block_on(ex.launch(d.vm())) {
        Err(ExecutorError::Launch(m)) => {
            assert!(m.contains("no-kvm") && m.contains("executor.log"), "{m}")
        }
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("launch should fail"),
    }
}

#[test]
fn broken_executors_are_rejected() {
    let d = Dir::new("broken");
    type Check = fn(&ExecutorError) -> bool;
    let cases: [(&str, Check); 5] = [
        ("echo not-json", |e| matches!(e, ExecutorError::Invalid(_))),
        ("exit 3", |e| matches!(e, ExecutorError::Launch(_))),
        (
            r#"echo '{"version":99,"name":"x","arch":"amd64","rootfs_formats":[],"live_share":false,"snapshot":false,"max_disks":1}'"#,
            |e| matches!(e, ExecutorError::Unsupported(m) if m.contains("speaks protocol 99")),
        ),
        // 버전이 다르면 모양이 달라도(옛 버전의 필드 구성) 버전 차이로 알린다.
        (
            r#"echo '{"version":1,"name":"old","arch":"amd64","rootfs_formats":[],"live_share":false,"snapshot":false,"max_disks":1}'"#,
            |e| matches!(e, ExecutorError::Unsupported(m) if m.contains("old speaks protocol 1")),
        ),
        ("true", |e| matches!(e, ExecutorError::Invalid(_))),
    ];
    for (body, ok) in cases {
        let err = ProcessExecutor::new(d.script(body)).unwrap_err();
        assert!(ok(&err), "{body}: {err}");
    }
    assert!(matches!(
        ProcessExecutor::new(Path::new("/no/such/executor")),
        Err(ExecutorError::Launch(_))
    ));
}
