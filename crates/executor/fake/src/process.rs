//! `sbx-executor-fake` 실행 파일. `sbx-executor-proto` 규약을 그대로 따른다.
//!
//! 테스트를 위한 환경 변수:
//! - `SBX_FAKE_FAIL_LAUNCH=<메시지>`: `Launch`에 그 메시지로 `Error`를 보내고 끝난다.
//! - `SBX_FAKE_RECORD=<파일>`: 받은 `Launch`의 VM 설정을 JSON으로 그 파일에 쓴다.

use std::io::{self, BufReader, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use sbx_executor_proto::{self as proto, Event, Request};

use crate::guest;
use crate::vm::REASON_KILLED;

/// 이 실행기의 capability.
pub fn capabilities() -> proto::Capabilities {
    proto::Capabilities {
        version: proto::VERSION,
        min_version: None,
        name: "fake".into(),
        arch: if cfg!(target_arch = "aarch64") {
            proto::Arch::Arm64
        } else {
            proto::Arch::Amd64
        },
        rootfs_formats: vec!["erofs".into()],
        live_share: true,
        custom_kernel: true,
        snapshot: false,
        max_disks: 2,
    }
}

fn emit<T: serde::Serialize>(out: &mut impl Write, e: &T) {
    // 표준 출력이 닫혔으면 sbx가 없는 것이다. 보낼 곳이 없으므로 무시한다.
    let _ = proto::write_line(out, e);
}

fn fail(out: &mut impl Write, message: String) -> ExitCode {
    emit(out, &Event::Error { message });
    ExitCode::from(1)
}

fn run() -> ExitCode {
    let mut input = BufReader::new(io::stdin());
    let mut out = io::stdout();
    let vm = match proto::read_line::<Request>(&mut input) {
        Ok(Some(Request::Launch { version, vm })) if version == proto::VERSION => vm,
        Ok(Some(Request::Launch { version, .. })) => {
            return fail(
                &mut out,
                format!(
                    "fake executor speaks protocol {}, got {version}",
                    proto::VERSION
                ),
            );
        }
        Ok(Some(other)) => return fail(&mut out, format!("expected launch, got {other:?}")),
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => return fail(&mut out, format!("bad message: {e}")),
    };
    if let Ok(path) = std::env::var("SBX_FAKE_RECORD") {
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&vm).unwrap_or_default());
    }
    if let Ok(message) = std::env::var("SBX_FAKE_FAIL_LAUNCH") {
        return fail(&mut out, message);
    }
    let caps = capabilities();
    if vm.vcpus == 0 || vm.memory_mib == 0 || vm.disks.len() > caps.max_disks as usize {
        return fail(&mut out, "unsupported vm spec".into());
    }
    let shares = vm
        .shares
        .iter()
        .map(|s| (s.tag.clone(), s.host_path.clone()))
        .collect();
    let handle = match guest::start(&vm.control_socket, shares) {
        Ok(h) => h,
        Err(e) => return fail(&mut out, format!("cannot open control socket: {e}")),
    };
    emit(&mut out, &Event::Started { pid: None });

    let handle = std::sync::Arc::new(handle);
    {
        let handle = handle.clone();
        thread::spawn(move || {
            // Kill이 오거나 표준 입력이 닫히면 끝낸다(규약 3, 5). 해석할 수 없는 줄도 끝낸다.
            let _ = proto::read_line::<Request>(&mut input);
            handle.stop(REASON_KILLED);
        });
    }
    let reason = loop {
        if let Some(r) = handle.wait_blocking(Duration::from_secs(3600)) {
            break r;
        }
    };
    emit(&mut out, &Event::Exited { reason });
    ExitCode::SUCCESS
}

/// 실행 파일 진입점.
pub fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("capabilities") => {
            emit(&mut io::stdout(), &capabilities());
            ExitCode::SUCCESS
        }
        Some("run") => run(),
        _ => {
            eprintln!("usage: sbx-executor-fake capabilities|run");
            ExitCode::from(2)
        }
    }
}
