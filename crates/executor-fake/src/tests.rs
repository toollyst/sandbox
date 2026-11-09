use super::*;

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use sandbox_executor::{ChannelSpec, Disk, DiskRole};
use sandbox_proto::{GUEST_PORT, GuestMsg, HostMsg, PROTOCOL_VERSION};

fn ready<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future did not complete immediately"),
    }
}

fn vm(sock: &str) -> VmConfig {
    VmConfig {
        guest_init: "i".into(),
        kernel: None,
        disks: vec![Disk {
            path: "r".into(),
            role: DiskRole::Rootfs,
            readonly: true,
        }],
        vcpus: 1,
        memory_mib: 64,
        shares: vec![],
        control_channel: ChannelSpec {
            host_endpoint: format!("/tmp/sbx-fake-{sock}-{}.sock", std::process::id()).into(),
            guest_port: GUEST_PORT,
        },
        hostname: "h".into(),
        log: "/tmp/sbx-fake.log".into(),
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

#[test]
fn launch_serves_guest_protocol_until_killed() {
    let fake = FakeExecutor::basic();
    let cfg = vm("serve");
    let mut handle = ready(fake.launch(cfg.clone())).unwrap();
    assert_eq!(fake.launches().len(), 1);

    let mut c = fake.connect(&cfg.control_channel).unwrap();
    hello(&mut c);
    drop(c);

    assert!(!ready(handle.wait(Duration::from_millis(10))).unwrap());
    handle.kill().unwrap();
    assert!(ready(handle.wait(Duration::from_secs(1))).unwrap());
    assert!(fake.connect(&cfg.control_channel).is_err());
}

#[test]
fn other_ports_are_refused() {
    let fake = FakeExecutor::basic();
    let mut cfg = vm("port");
    let _handle = ready(fake.launch(cfg.clone())).unwrap();
    cfg.control_channel.guest_port = GUEST_PORT + 1;
    let err = fake.connect(&cfg.control_channel).err().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn guest_shutdown_request_ends_the_vm() {
    let fake = FakeExecutor::basic();
    let cfg = vm("shutdown");
    let mut handle = ready(fake.launch(cfg.clone())).unwrap();
    let mut c = fake.connect(&cfg.control_channel).unwrap();
    hello(&mut c);
    HostMsg::Shutdown.write_to(&mut c).unwrap();
    assert_eq!(GuestMsg::read_from(&mut c).unwrap(), GuestMsg::Done);
    assert!(ready(handle.wait(Duration::from_secs(5))).unwrap());
}

#[test]
fn launch_rejects_unsupported_config() {
    let fake = FakeExecutor::basic();
    let mut cfg = vm("reject");
    cfg.disks = vec![cfg.disks[0].clone(); 3];
    assert!(matches!(
        ready(fake.launch(cfg)),
        Err(ExecutorError::Unsupported(_))
    ));
}
