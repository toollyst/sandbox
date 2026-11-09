//! 샌드박스 설정을 실행기 기능과 맞춰 본다.
//!
//! 실행기가 지원하지 않는 것을 요청하면 다른 방식으로 바꾸지 않고 오류를 낸다.
//!
//! | 요청 | 필요한 capability | 없을 때 |
//! |---|---|---|
//! | rootfs 형식 | `rootfs_formats`에 그 형식 | [`RuntimeError::FormatUnsupported`] |
//! | `MountKind::Bind` | `live_share` | [`RuntimeError::BindUnsupported`] (`Copy`로 바꾸지 않는다) |
//!
//! 아직 구현하지 않은 것도 조용히 무시하지 않고 오류다.
//!
//! | 요청 | 결과 |
//! |---|---|
//! | `MountKind::Bind` (실행기가 지원해도) | [`RuntimeError::Unimplemented`] (로드맵 M3) |
//! | `MountKind::Copy` + `readonly` | [`RuntimeError::Invalid`] (복사본이라 읽기 전용을 강제할 수 없다) |
//! | `Lifetime::Detached` | [`RuntimeError::Unimplemented`] (로드맵 M3) |

use sandbox_executor::Capabilities;
use sandbox_model::{Lifetime, MountKind, SandboxSpec};

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
    if spec.mounts.iter().any(|m| m.kind == MountKind::Bind) {
        return Err(RuntimeError::Unimplemented("bind mounts"));
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
    if spec.lifetime == Lifetime::Detached {
        return Err(RuntimeError::Unimplemented("detached sandboxes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_model::{
        Arch, Digest, ImageId, Lifetime, Mount, NetworkPolicy, ProcessDefaults, Resources,
        RootfsFormat, UserSpec,
    };
    use std::collections::BTreeMap;

    fn caps(formats: Vec<RootfsFormat>, live_share: bool) -> Capabilities {
        Capabilities {
            name: "test",
            arch: Arch::X86_64,
            rootfs_formats: formats,
            live_share,
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
            lifetime: Lifetime::Attached,
            hostname: None,
            labels: BTreeMap::new(),
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
        // 지원하는 실행기여도 아직 구현하지 않았으므로 명시적으로 거부한다.
        assert!(matches!(
            check(&s, &caps(vec![RootfsFormat::Erofs], true)),
            Err(RuntimeError::Unimplemented(_))
        ));
    }

    #[test]
    fn readonly_copy_and_detached_are_rejected() {
        let mut m = mount(MountKind::Copy);
        m.readonly = true;
        let c = caps(vec![RootfsFormat::Erofs], false);
        assert!(matches!(
            check(&spec(vec![m]), &c),
            Err(RuntimeError::Invalid(_))
        ));
        let mut s = spec(vec![]);
        s.lifetime = Lifetime::Detached;
        assert!(matches!(check(&s, &c), Err(RuntimeError::Unimplemented(_))));
    }

    #[test]
    fn copy_mount_needs_no_capability() {
        let s = spec(vec![mount(MountKind::Copy)]);
        assert!(check(&s, &caps(vec![RootfsFormat::Erofs], false)).is_ok());
    }
}
