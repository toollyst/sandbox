//! 테스트가 끝날 때 남은 호스트 프로세스를 찾고 끝내는 도구. 실행 파일이 `SBX_FAKE_PROCS` 파일에 남긴 기록
//! (`executor <pid>`, `owner <pid>`, `group <pgid>`)을 읽는다(`process` 모듈 문서).
//!
//! VM을 끝내면 실행기가 그 그룹들을 끝내고, 실행기와 소유 프로세스(sbx, shim)도 VM과 함께 끝난다. 그래서 샌드박스를
//! 모두 지운 뒤에 여기서 나오는 것은 새는 프로세스다.

use std::fmt;
use std::path::Path;

/// 기록된 것 가운데 아직 있는 것 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Left {
    /// 기록의 종류(`executor`, `owner`, `group`).
    pub what: String,
    /// PID나 프로세스 그룹 번호.
    pub id: u32,
}

impl Left {
    fn is_group(&self) -> bool {
        self.what == "group"
    }
}

impl fmt::Display for Left {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.what, self.id)
    }
}

/// 좀비(끝났지만 부모가 아직 거두지 않은 것)인지. `ps`로 본다. 테스트가 기다리지 않은 자식(예: `sbx mcp`)이 그렇다.
fn is_zombie(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim_start()
                .starts_with('Z')
        })
}

/// `record`에 적힌 것 가운데 아직 도는 것(좀비는 뺀다. 프로세스 그룹은 좀비만 남아도 있는 것으로 본다). 파일이 없으면
/// 빈 목록이다. `except`(부르는 테스트 자신 등)는 빼고 본다.
pub fn alive(record: &Path, except: &[u32]) -> Vec<Left> {
    let Ok(text) = std::fs::read_to_string(record) else {
        return Vec::new();
    };
    let mut out: Vec<Left> = Vec::new();
    for line in text.lines() {
        let Some((what, id)) = line.split_once(' ') else {
            continue;
        };
        let Ok(id) = id.trim().parse::<u32>() else {
            continue;
        };
        let left = Left {
            what: what.to_owned(),
            id,
        };
        if out.contains(&left) || id <= 1 || except.contains(&id) {
            continue;
        }
        let here = if left.is_group() {
            sbx_guest::exec::group_alive(id)
        } else {
            sbx_guest::exec::process_alive(id) && !is_zombie(id)
        };
        if here {
            out.push(left);
        }
    }
    out
}

/// 모두 SIGKILL로 끝낸다(그룹은 그룹 전체).
pub fn kill(left: &[Left]) {
    for l in left {
        let _ = sbx_guest::exec::kill_hard(l.id, l.is_group());
    }
}
