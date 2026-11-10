//! 커널 명령줄의 `sbx.*` 인자. 실행기 어댑터가 VMM에 맞게 넣는다.
//!
//! | 인자 | 값 | 기본 |
//! |---|---|---|
//! | `sbx.halt` | `poweroff` 또는 `reboot`. VM을 끝내는 방법. Firecracker는 게스트 reboot을 받으면 끝난다. | `poweroff` |
//! | `sbx.hostname` | 호스트 이름 | `sandbox` |
//!
//! 모르는 `sbx.*` 인자나 잘못된 값은 오류다. 다른 인자는 커널의 것이므로 무시한다.

/// VM을 끝내는 방법.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Halt {
    /// 전원 끄기.
    PowerOff,
    /// 재시작 요청. 재시작을 VM 종료로 다루는 VMM용.
    Reboot,
}

/// 부팅 설정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootConfig {
    /// VM을 끝내는 방법.
    pub halt: Halt,
    /// 호스트 이름.
    pub hostname: String,
}

/// `/proc/cmdline` 내용을 해석한다.
pub fn parse(cmdline: &str) -> Result<BootConfig, String> {
    let mut c = BootConfig {
        halt: Halt::PowerOff,
        hostname: "sandbox".into(),
    };
    for arg in cmdline.split_ascii_whitespace() {
        let Some(rest) = arg.strip_prefix("sbx.") else {
            continue;
        };
        match rest.split_once('=') {
            Some(("halt", "poweroff")) => c.halt = Halt::PowerOff,
            Some(("halt", "reboot")) => c.halt = Halt::Reboot,
            Some(("hostname", h)) if !h.is_empty() && h.len() <= 64 => c.hostname = h.into(),
            _ => return Err(format!("invalid boot argument {arg:?}")),
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_values() {
        let c = parse("console=ttyS0 reboot=k panic=1").unwrap();
        assert_eq!(c.halt, Halt::PowerOff);
        assert_eq!(c.hostname, "sandbox");
        let c = parse("console=ttyS0 sbx.halt=reboot sbx.hostname=box1\n").unwrap();
        assert_eq!(
            c,
            BootConfig {
                halt: Halt::Reboot,
                hostname: "box1".into()
            }
        );
    }

    #[test]
    fn unknown_sbx_arguments_are_errors() {
        assert!(parse("sbx.halt=sleep").is_err());
        assert!(parse("sbx.nope=1").is_err());
        assert!(parse("sbx.hostname=").is_err());
    }
}
