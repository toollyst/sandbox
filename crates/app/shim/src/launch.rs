//! shim을 띄운다 (`run -d`, `start`).
//!
//! - shim의 표준 입력은 닫고, 표준 오류는 샌드박스 로그 디렉터리의 [`SHIM_LOG`]에 이어 쓴다.
//! - shim은 띄운 프로세스와 다른 프로세스 그룹에서 돈다(Unix). 그래서 터미널의 Ctrl+C나 닫힘이 shim에 가지 않는다.
//!   Windows에서는 콘솔 없이(`DETACHED_PROCESS`), 새 프로세스 그룹으로 띄운다.
//! - shim이 시작 결과를 한 줄로 알릴 때까지 기다린다([`crate::READY`] 또는 `error <이유>`).
//!   그 전에 shim이 끝나면 실패다.
//! - 띄운 뒤에는 shim을 기다리지 않는다. 띄운 프로세스가 끝나도 shim은 남는다.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sbx_model::{SandboxId, blocking};

use crate::config::ShimConfig;
use crate::serve::READY;

/// shim 진단 로그 파일 이름 (샌드박스 로그 디렉터리 안).
pub const SHIM_LOG: &str = "shim.log";

/// shim으로 샌드박스를 시작한다. 시작이 끝나면(성공이든 실패든) 돌아온다.
///
/// `program`은 shim 실행 파일, `log_dir`은 그 샌드박스의 로그 디렉터리(`Sandbox::log_dir`)다.
pub async fn start_detached(
    program: &Path,
    config: &ShimConfig,
    id: &SandboxId,
    log_dir: &Path,
) -> Result<(), String> {
    let program = program.to_owned();
    let args = config.to_args(id);
    let log_dir = log_dir.to_owned();
    blocking(move || launch(&program, &args, &log_dir)).await
}

fn launch(program: &Path, args: &[std::ffi::OsString], log_dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(log_dir).map_err(|e| format!("{}: {e}", log_dir.display()))?;
    let log_path: PathBuf = log_dir.join(SHIM_LOG);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| format!("{}: {e}", log_path.display()))?;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot run shim {}: {e}", program.display()))?;
    let mut line = String::new();
    let read = match child.stdout.take() {
        Some(out) => BufReader::new(out).read_line(&mut line),
        None => Ok(0),
    };
    let line = line.trim_end();
    if read.is_ok() && line == READY {
        return Ok(());
    }
    let status = child.wait();
    match line.strip_prefix("error ") {
        Some(e) => Err(e.to_owned()),
        None => Err(format!(
            "shim exited ({}) before starting; see {}",
            status.map_or_else(|e| e.to_string(), |s| s.to_string()),
            log_path.display()
        )),
    }
}
