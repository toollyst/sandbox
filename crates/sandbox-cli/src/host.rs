//! 호스트 진단(`sbx info`): OS 이름과 하드웨어 가상화를 쓸 수 있는지. 맨 아래 모듈이다.
//!
//! 진단이 실패해도 오류로 만들지 않고 읽을 수 있는 글로 돌려준다. 보고하는 것이 목적이다.

use std::process::Command;

/// 사람이 읽는 OS 이름과 버전. 예: `macOS 15.4`, `Ubuntu 24.04.1 LTS (kernel 6.8.0-45-generic)`.
pub fn os() -> String {
    let os = std::env::consts::OS;
    match os {
        "macos" => match output("sw_vers", &["-productVersion"]) {
            Some(v) => format!("macOS {v}"),
            None => "macOS".into(),
        },
        "linux" => {
            let name = std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|t| pretty_name(&t))
                .unwrap_or_else(|| "Linux".into());
            match output("uname", &["-r"]) {
                Some(k) => format!("{name} (kernel {k})"),
                None => name,
            }
        }
        other => other.into(),
    }
}

/// 하드웨어 가상화를 쓸 수 있는지. `(쓸 수 있는지, 설명)`.
pub fn hypervisor() -> (bool, String) {
    match std::env::consts::OS {
        "linux" => {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/kvm")
            {
                Ok(_) => (true, "KVM (/dev/kvm is usable)".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                    false,
                    "no /dev/kvm (enable virtualization, or use a machine with nested virtualization)"
                        .into(),
                ),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => (
                    false,
                    "/dev/kvm: permission denied (add your user to the kvm group, then log in again)"
                        .into(),
                ),
                Err(e) => (false, format!("/dev/kvm: {e}")),
            }
        }
        "macos" => match output("sysctl", &["-n", "kern.hv_support"]).as_deref() {
            Some("1") => (true, "Hypervisor.framework".into()),
            _ => (
                false,
                "Hypervisor.framework is not supported on this Mac".into(),
            ),
        },
        other => (false, format!("no supported hypervisor on {other}")),
    }
}

/// `/etc/os-release`의 `PRETTY_NAME`.
fn pretty_name(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
}

/// 명령의 표준 출력 첫 줄. 실패하면 `None`.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let line = s.lines().next()?.trim().to_owned();
    (!line.is_empty()).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_release_pretty_name() {
        let t = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(pretty_name(t).as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(pretty_name("ID=x\n"), None);
        assert_eq!(pretty_name("PRETTY_NAME=\n"), None);
    }

    #[test]
    fn host_is_described() {
        assert!(!os().is_empty());
        assert!(!hypervisor().1.is_empty());
    }
}
