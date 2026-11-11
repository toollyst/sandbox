//! 샌드박스 설정을 실행기 기능과 맞춰 본다.
//!
//! 실행기가 지원하지 않는 것을 요청하면 다른 방식으로 바꾸지 않고 오류를 낸다.
//!
//! | 요청 | 필요한 capability | 없을 때 |
//! |---|---|---|
//! | rootfs 형식 | `rootfs_formats`에 그 형식 | [`RuntimeError::FormatUnsupported`] |
//! | `MountKind::Bind` | `live_share` | [`RuntimeError::BindUnsupported`] (`Copy`로 바꾸지 않는다) |
//!
//! 받을 수 없는 조합도 조용히 무시하지 않고 오류다.
//!
//! | 요청 | 결과 |
//! |---|---|
//! | `MountKind::Copy` + `readonly` | [`RuntimeError::Invalid`] (복사본이라 읽기 전용을 강제할 수 없다) |
//! | `NetworkPolicy::Allow`에 규칙이 없음 | [`RuntimeError::Invalid`] (차단이면 `Disabled`다) |
//! | `-p` 게스트 포트 0, 같은 호스트 주소·포트(0이 아닌)를 두 번 | [`RuntimeError::Invalid`] |

use sbx_executor::Capabilities;
use sbx_model::{MountKind, NetworkPolicy, SandboxSpec};

use crate::error::RuntimeError;

/// 검사한다. 규칙은 모듈 문서의 표를 따른다.
pub fn check(spec: &SandboxSpec, caps: &Capabilities) -> Result<(), RuntimeError> {
    if !caps.supports_format(spec.rootfs_format) {
        return Err(RuntimeError::FormatUnsupported {
            executor: caps.name,
            format: spec.rootfs_format,
        });
    }
    if !caps.live_share
        && let Some(m) = spec.mounts.iter().find(|m| m.kind == MountKind::Bind)
    {
        return Err(RuntimeError::BindUnsupported {
            executor: caps.name,
            destination: m.destination.clone(),
        });
    }
    if let Some(m) = spec
        .mounts
        .iter()
        .find(|m| m.kind == MountKind::Copy && m.readonly)
    {
        return Err(RuntimeError::Invalid(format!(
            "copy mount to {} cannot be read-only",
            m.destination
        )));
    }
    for (i, p) in spec.ports.iter().enumerate() {
        if p.guest_port == 0 {
            return Err(RuntimeError::Invalid(format!(
                "-p {p}: the sandbox port can't be 0"
            )));
        }
        if p.host_port != 0
            && spec.ports[..i].iter().any(|q| {
                q.host_port == p.host_port
                    && (q.host_ip == p.host_ip
                        || q.host_ip.is_unspecified()
                        || p.host_ip.is_unspecified())
            })
        {
            return Err(RuntimeError::Invalid(format!(
                "-p {p}: host port {} is published twice",
                p.host_port
            )));
        }
    }
    if spec.network == NetworkPolicy::Allow(Vec::new()) {
        return Err(RuntimeError::Invalid(
            "network allow list is empty (use the disabled policy to block everything)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sbx_model::{
        Arch, Digest, ImageId, Mount, NetworkPolicy, ProcessDefaults, Resources, RootfsFormat,
        UserSpec,
    };
    use std::collections::BTreeMap;

    fn caps(formats: Vec<RootfsFormat>, live_share: bool) -> Capabilities {
        Capabilities {
            name: "test",
            arch: Arch::X86_64,
            rootfs_formats: formats,
            live_share,
            custom_kernel: false,
            snapshot: false,
            max_disks: 2,
        }
    }

    fn spec(mounts: Vec<Mount>) -> SandboxSpec {
        SandboxSpec {
            image: ImageId::new(Digest::parse(&format!("sha256:{}", "01".repeat(32))).unwrap()),
            rootfs_format: RootfsFormat::Erofs,
            process: ProcessDefaults {
                args: vec!["sh".into()],
                env: vec![],
                cwd: "/".into(),
                user: UserSpec::root(),
            },
            resources: Resources {
                vcpus: 1,
                memory_mib: 256,
                scratch_disk_mib: 64,
            },
            mounts,
            network: NetworkPolicy::Disabled,
            hostname: None,
            labels: BTreeMap::new(),
            stop: Default::default(),
            open_stdin: false,
            tty: false,
            name: None,
            lifetime_secs: None,
            ports: Vec::new(),
        }
    }

    fn mount(kind: MountKind) -> Mount {
        Mount {
            source: "/host".into(),
            destination: "/work".into(),
            kind,
            readonly: false,
        }
    }

    #[test]
    fn unsupported_format_is_rejected() {
        assert!(matches!(
            check(&spec(vec![]), &caps(vec![], false)),
            Err(RuntimeError::FormatUnsupported { .. })
        ));
    }

    #[test]
    fn bind_requires_live_share_and_is_not_downgraded() {
        let s = spec(vec![mount(MountKind::Bind)]);
        assert!(matches!(
            check(&s, &caps(vec![RootfsFormat::Erofs], false)),
            Err(RuntimeError::BindUnsupported { destination, .. }) if destination == "/work"
        ));
        assert!(check(&s, &caps(vec![RootfsFormat::Erofs], true)).is_ok());
    }

    #[test]
    fn readonly_copy_is_rejected() {
        let mut m = mount(MountKind::Copy);
        m.readonly = true;
        let c = caps(vec![RootfsFormat::Erofs], false);
        assert!(matches!(
            check(&spec(vec![m]), &c),
            Err(RuntimeError::Invalid(_))
        ));
    }

    #[test]
    fn copy_mount_needs_no_capability() {
        let s = spec(vec![mount(MountKind::Copy)]);
        assert!(check(&s, &caps(vec![RootfsFormat::Erofs], false)).is_ok());
    }
}
