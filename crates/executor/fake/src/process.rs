//! `sbx-executor-fake` 실행 파일. `sbx-executor-proto` 규약을 그대로 따른다.
//!
//! 테스트를 위한 환경 변수:
//! - `SBX_FAKE_FAIL_LAUNCH=<메시지>`: `Launch`에 그 메시지로 `Error`를 보내고 끝난다.
//! - `SBX_FAKE_RECORD=<파일>`: 받은 `Launch`의 VM 설정을 JSON으로 그 파일에 쓴다.
//! - `SBX_FAKE_DROP_LAYER_DISK=1`: 받은 층 디스크 가운데 마지막 것을 게스트에 주지 않는다(디스크를 빠뜨린 실행기).
//! - `SBX_FAKE_MAX_DISKS=<수>`: `capabilities`가 알리는 `max_disks`(기본 [`crate::MAX_DISKS`]). 더 많이 받으면 거부한다.
//! - `SBX_FAKE_PROTOCOL=<버전>`: 그 규약 버전 하나만 말한다(옛 실행기 흉내). 없으면 2부터 [`proto::VERSION`]까지.
//! - `SBX_FAKE_LINGER_MS=<밀리초>`: 표준 입력이 닫힌 뒤(소유 프로세스가 죽은 뒤) VM을 끄기까지 그만큼 둔다. 그동안
//!   제어 소켓도 그대로 듣는다(VM을 천천히 끄는 실행기 흉내). `Kill`에는 곧바로 끈다.
//! - `SBX_FAKE_PROCS=<파일>`: 그 파일에 이 실행기의 PID(`executor <pid>`), 부모(소유 프로세스, `owner <pid>`), 게스트
//!   처리가 띄운 프로세스 그룹(`group <pgid>`)을 한 줄씩 덧붙인다. 테스트가 끝날 때 남은 프로세스를 찾는다.
//! - `SBX_FAKE_WATCH_PID=<pid>`: 그 프로세스가 사라지면 VM을 끈다(테스트 프로세스가 `kill -9`로 죽어도 샌드박스의
//!   프로세스가 남지 않게). 0.2초마다 본다.

use std::io::{self, BufReader, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use sbx_executor_proto::{self as proto, Event, Request};

use crate::guest;
use crate::vm::REASON_KILLED;

/// 이 실행기의 capability.
pub fn capabilities() -> proto::Capabilities {
    let (min, max) = versions();
    proto::Capabilities {
        version: max,
        min_version: Some(min),
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
        max_disks: std::env::var("SBX_FAKE_MAX_DISKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::MAX_DISKS as u32),
    }
}

/// 말하는 규약 버전 범위. `SBX_FAKE_PROTOCOL`이 있으면 그 하나다.
fn versions() -> (u32, u32) {
    match std::env::var("SBX_FAKE_PROTOCOL")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(v) => (v, v),
        None => (2, proto::VERSION),
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
    let (min, max) = versions();
    let vm = match proto::read_line::<Request>(&mut input) {
        Ok(Some(Request::Launch { version, vm }))
            if (min..=max).contains(&version) && vm.min_version() <= version =>
        {
            vm
        }
        Ok(Some(Request::Launch { version, .. })) => {
            return fail(
                &mut out,
                format!(
                    "fake executor speaks protocol {min} to {max}, got {version} (or a disk that version lacks)"
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
    let scratch = vm.disks.iter().find(|d| d.role == proto::DiskRole::Scratch);
    let build = scratch.map(|d| (crate::build_root(&d.path), crate::net_dir(&d.path)));
    let clock = scratch.map(|d| crate::clock_file(&d.path));
    let mut layer_disks: Vec<std::path::PathBuf> = vm
        .disks
        .iter()
        .filter(|d| d.role == proto::DiskRole::Rootfs)
        .map(|d| d.path.clone())
        .collect();
    if std::env::var_os("SBX_FAKE_DROP_LAYER_DISK").is_some() {
        // 디스크 하나를 빠뜨린 실행기를 흉내 낸다. 게스트는 받은 것만 센다.
        layer_disks.pop();
    }
    let disks = vm
        .disks
        .iter()
        .map(|d| crate::vm::FakeDisk {
            path: d.path.clone(),
            volume: d.role == proto::DiskRole::Volume,
            readonly: d.readonly,
        })
        .collect();
    let record = std::env::var_os("SBX_FAKE_PROCS").map(std::path::PathBuf::from);
    if let Some(path) = &record {
        let line = format!(
            "executor {}\nowner {}\n",
            std::process::id(),
            std::os::unix::process::parent_id()
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
    let procs = std::sync::Arc::new(crate::vm::Procs::new(record));
    let handle = match guest::start(
        &vm.control_socket,
        shares,
        build,
        layer_disks,
        clock,
        disks,
        procs,
    ) {
        Ok(h) => h,
        Err(e) => return fail(&mut out, format!("cannot open control socket: {e}")),
    };
    emit(&mut out, &Event::Started { pid: None });

    let handle = std::sync::Arc::new(handle);
    {
        let handle = handle.clone();
        thread::spawn(move || {
            // Kill이 오거나 표준 입력이 닫히면 끝낸다(규약 3, 5). 해석할 수 없는 줄도 끝낸다.
            if let Ok(None) = proto::read_line::<Request>(&mut input) {
                let linger = std::env::var("SBX_FAKE_LINGER_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok());
                if let Some(ms) = linger {
                    thread::sleep(Duration::from_millis(ms));
                }
            }
            handle.stop(REASON_KILLED);
        });
    }
    if let Some(pid) = std::env::var("SBX_FAKE_WATCH_PID")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        let handle = handle.clone();
        thread::spawn(move || {
            while sbx_guest::exec::process_alive(pid) {
                thread::sleep(Duration::from_millis(200));
            }
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
