//! OCI 적용 범위. 무엇을 받고 무엇을 받지 않는지를 코드로 적는다.
//!
//! 범위를 넓힐 때는 이 목록과 변환 코드만 고친다. 실행 코어는 바뀌지 않는다.
//! 목록에 없는 필드는 적용하지 않고, `ResolvedImage::ignored_fields`에 남겨 `image inspect`에서 보여준다.

use sbx_image::Compression;

/// 받는 manifest 형식.
///
/// Docker v2 형식도 받는다. Docker Hub 공개 이미지 중 상당수가 아직 그 형식이다.
pub const MANIFEST_MEDIA_TYPES: &[&str] = &[
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.oci.image.index.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
    "application/vnd.docker.distribution.manifest.list.v2+json",
];

/// 받지 않는 manifest 형식.
pub const REJECTED_MANIFEST_MEDIA_TYPES: &[&str] = &[
    "application/vnd.docker.distribution.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v1+prettyjws",
];

/// 받는 레이어 형식과 압축.
pub const LAYER_MEDIA_TYPES: &[(&str, Compression)] = &[
    ("application/vnd.oci.image.layer.v1.tar", Compression::None),
    (
        "application/vnd.oci.image.layer.v1.tar+gzip",
        Compression::Gzip,
    ),
    (
        "application/vnd.docker.image.rootfs.diff.tar.gzip",
        Compression::Gzip,
    ),
    (
        "application/vnd.oci.image.layer.v1.tar+zstd",
        Compression::Zstd,
    ),
];

/// 레이어 형식의 압축. 받지 않는 형식이면 `None`.
pub fn layer_compression(media_type: &str) -> Option<Compression> {
    LAYER_MEDIA_TYPES
        .iter()
        .find(|(m, _)| *m == media_type)
        .map(|(_, c)| *c)
}

/// 적용하는 image config 필드. 나머지는 `ignored_fields`로 간다.
pub const APPLIED_CONFIG_FIELDS: &[&str] = &[
    "Entrypoint",
    "Cmd",
    "Env",
    "WorkingDir",
    "User",
    "StopSignal",
    "Labels",
];

/// Runtime Spec 중 VM 경계에서는 의미가 없어 적용하지 않는 필드.
///
/// 격리 경계는 VM이고, 이 필드들은 게스트 내부의 설정이기 때문이다.
/// 나중에 Runtime Spec 번들을 받게 되더라도 적용하지 않고, 값이 있으면 경고한다.
pub const RUNTIME_SPEC_NOT_APPLIED: &[&str] = &[
    "linux.namespaces",
    "linux.cgroupsPath",
    "linux.seccomp",
    "process.capabilities",
    "process.apparmorProfile",
    "process.selinuxLabel",
    "linux.devices",
    "linux.sysctl",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_types() {
        assert_eq!(
            layer_compression("application/vnd.oci.image.layer.v1.tar+gzip"),
            Some(Compression::Gzip)
        );
        assert_eq!(
            layer_compression("application/vnd.oci.image.layer.v1.tar+zstd"),
            Some(Compression::Zstd)
        );
        assert_eq!(
            layer_compression("application/vnd.oci.image.layer.nondistributable.v1.tar"),
            None
        );
    }

    #[test]
    fn accepted_and_rejected_do_not_overlap() {
        for m in REJECTED_MANIFEST_MEDIA_TYPES {
            assert!(!MANIFEST_MEDIA_TYPES.contains(m));
        }
    }
}
