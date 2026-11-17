//! 규약의 [`VmSpec`]을 Firecracker 설정 파일(`--config-file`)로 바꾼다. 규칙은 크레이트 문서에 있다.

use std::path::Path;

use sbx_executor_proto::{DiskRole, VmSpec};
use serde_json::{Value, json};

/// Unix 소켓 경로의 최대 바이트 수 (macOS가 가장 짧다: `sun_path` 104바이트, 끝의 NUL 포함).
pub const MAX_SOCKET_PATH: usize = 103;

/// 게스트 vsock CID. 게스트 쪽에서는 의미가 없고, 3 이상이면 된다.
pub const GUEST_CID: u32 = 3;

/// 설정 파일 내용. 커널은 `vm.kernel`(이미지가 담은 커널), 없으면 `default_kernel`이다.
/// `x86_64`면 x86_64 게스트용 명령줄을 쓴다.
pub fn firecracker_config(
    vm: &VmSpec,
    default_kernel: &Path,
    x86_64: bool,
) -> Result<Value, String> {
    let kernel = vm.kernel.as_deref().unwrap_or(default_kernel);
    if !vm.shares.is_empty() {
        return Err("this executor cannot attach shared folders (live_share is false)".into());
    }
    if vm.hostname.is_empty()
        || !vm
            .hostname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.')
    {
        return Err(format!("invalid hostname {:?}", vm.hostname));
    }
    let socket = vm
        .control_socket
        .to_str()
        .ok_or("control socket path is not UTF-8")?;
    if socket.len() > MAX_SOCKET_PATH {
        return Err(format!(
            "control socket path is {} bytes; at most {MAX_SOCKET_PATH} are allowed: {socket}",
            socket.len()
        ));
    }
    let utf8 = |p: &Path, what: &str| {
        p.to_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{what} path is not UTF-8: {}", p.display()))
    };
    let mut boot_args = String::from("console=ttyS0 quiet reboot=k panic=1");
    if x86_64 {
        boot_args.push_str(" pci=off");
    }
    boot_args.push_str(&format!(" sbx.hostname={} sbx.halt=reboot", vm.hostname));
    let drives = vm
        .disks
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let id = match d.role {
                DiskRole::Rootfs => format!("rootfs{i}"),
                DiskRole::Scratch => format!("scratch{i}"),
                DiskRole::Volume => format!("volume{i}"),
            };
            let mut drive = json!({
                "drive_id": id,
                "path_on_host": utf8(&d.path, "disk")?,
                "is_root_device": false,
                "is_read_only": d.readonly,
            });
            // 볼륨은 게스트의 flush가 호스트 파일의 fsync까지 가야 한다(규약 3). Firecracker는 `Writeback`일 때만
            // 게스트에 flush 기능을 보이고 받은 flush를 fsync로 넘긴다. 기본값 `Unsafe`는 flush를 보이지 않는다.
            // 층과 scratch는 VM과 함께 버려도 되므로 기본값 그대로다.
            if d.role == DiskRole::Volume {
                drive["cache_type"] = json!("Writeback");
            }
            Ok(drive)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({
        "boot-source": {
            "kernel_image_path": utf8(kernel, "kernel")?,
            "initrd_path": utf8(&vm.guest_init, "guest init")?,
            "boot_args": boot_args,
        },
        "drives": drives,
        "machine-config": {
            "vcpu_count": vm.vcpus,
            "mem_size_mib": vm.memory_mib,
            "smt": false,
        },
        "vsock": {
            "guest_cid": GUEST_CID,
            "uds_path": socket,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sbx_executor_proto::{DiskSpec, ShareSpec};

    fn vm() -> VmSpec {
        VmSpec {
            guest_init: "/c/initramfs".into(),
            kernel: None,
            disks: vec![
                DiskSpec {
                    path: "/c/rootfs.erofs".into(),
                    role: DiskRole::Rootfs,
                    readonly: true,
                },
                DiskSpec {
                    path: "/d/scratch.img".into(),
                    role: DiskRole::Scratch,
                    readonly: false,
                },
            ],
            vcpus: 2,
            memory_mib: 512,
            shares: vec![],
            control_socket: "/s/abc.sock".into(),
            hostname: "box-1".into(),
        }
    }

    #[test]
    fn spec_becomes_firecracker_config() {
        let c = firecracker_config(&vm(), Path::new("/k/kernel"), false).unwrap();
        assert_eq!(c["boot-source"]["kernel_image_path"], "/k/kernel");
        assert_eq!(c["boot-source"]["initrd_path"], "/c/initramfs");
        assert_eq!(
            c["boot-source"]["boot_args"],
            "console=ttyS0 quiet reboot=k panic=1 sbx.hostname=box-1 sbx.halt=reboot"
        );
        let d = c["drives"].as_array().unwrap();
        assert_eq!(
            (
                d[0]["path_on_host"].as_str(),
                d[0]["is_read_only"].as_bool()
            ),
            (Some("/c/rootfs.erofs"), Some(true))
        );
        assert_eq!(
            (
                d[1]["path_on_host"].as_str(),
                d[1]["is_read_only"].as_bool()
            ),
            (Some("/d/scratch.img"), Some(false))
        );
        assert!(d.iter().all(|x| x["is_root_device"] == false));
        assert_eq!(c["machine-config"]["vcpu_count"], 2);
        assert_eq!(c["machine-config"]["mem_size_mib"], 512);
        assert_eq!(c["vsock"]["uds_path"], "/s/abc.sock");
        let x86 = firecracker_config(&vm(), Path::new("/k"), true).unwrap();
        assert!(
            x86["boot-source"]["boot_args"]
                .as_str()
                .unwrap()
                .contains(" pci=off ")
        );
        // 이미지가 담은 커널이 있으면 그것으로 부팅한다.
        let mut v = vm();
        v.kernel = Some("/c/images/x/kernel".into());
        let c = firecracker_config(&v, Path::new("/k/kernel"), false).unwrap();
        assert_eq!(c["boot-source"]["kernel_image_path"], "/c/images/x/kernel");
    }

    /// 층 디스크가 여럿이면(맨 아래 층, scratch, 위 층들) 받은 차례 그대로 drive가 된다. 게스트 장치 이름이 그 차례다.
    #[test]
    fn layer_disks_become_drives_in_order() {
        let mut v = vm();
        for i in 1..=2 {
            v.disks.push(DiskSpec {
                path: format!("/c/layers/{i}/layer.erofs").into(),
                role: DiskRole::Rootfs,
                readonly: true,
            });
        }
        let c = firecracker_config(&v, Path::new("/k/kernel"), false).unwrap();
        let d = c["drives"].as_array().unwrap();
        let got: Vec<(&str, &str, bool)> = d
            .iter()
            .map(|x| {
                (
                    x["drive_id"].as_str().unwrap(),
                    x["path_on_host"].as_str().unwrap(),
                    x["is_read_only"].as_bool().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("rootfs0", "/c/rootfs.erofs", true),
                ("scratch1", "/d/scratch.img", false),
                ("rootfs2", "/c/layers/1/layer.erofs", true),
                ("rootfs3", "/c/layers/2/layer.erofs", true),
            ]
        );
        assert!(d.iter().all(|x| x["is_root_device"] == false));
    }

    /// 볼륨 디스크는 `Writeback`으로 붙여 게스트의 flush가 호스트에 닿게 한다. 다른 디스크는 `cache_type`이 없다.
    #[test]
    fn volume_disks_pass_flushes_to_the_host() {
        let mut v = vm();
        v.disks.push(DiskSpec {
            path: "/d/volumes/pg/disk.img".into(),
            role: DiskRole::Volume,
            readonly: false,
        });
        v.disks.push(DiskSpec {
            path: "/d/volumes/ro/disk.img".into(),
            role: DiskRole::Volume,
            readonly: true,
        });
        let c = firecracker_config(&v, Path::new("/k/kernel"), false).unwrap();
        let d = c["drives"].as_array().unwrap();
        assert!(d[..2].iter().all(|x| x.get("cache_type").is_none()));
        assert_eq!(d[2]["drive_id"], "volume2");
        assert_eq!(d[2]["cache_type"], "Writeback");
        assert_eq!(d[2]["is_read_only"], false);
        assert_eq!(
            (d[3]["cache_type"].as_str(), d[3]["is_read_only"].as_bool()),
            (Some("Writeback"), Some(true))
        );
    }

    /// 설정 파일은 upstream Firecracker가 읽는 키만 담는다: 아래가 전부다(빠진 키도, 더한 키도 없다). 확장 키
    /// (`extensions` 같은 것)는 VMM의 인터페이스가 정해지기 전에는 넣지 않는다.
    #[test]
    fn config_has_exactly_the_upstream_keys() {
        let c = firecracker_config(&vm(), Path::new("/k/kernel"), false).unwrap();
        assert_eq!(
            c,
            json!({
                "boot-source": {
                    "kernel_image_path": "/k/kernel",
                    "initrd_path": "/c/initramfs",
                    "boot_args": "console=ttyS0 quiet reboot=k panic=1 sbx.hostname=box-1 sbx.halt=reboot",
                },
                "drives": [
                    {"drive_id": "rootfs0", "path_on_host": "/c/rootfs.erofs", "is_root_device": false, "is_read_only": true},
                    {"drive_id": "scratch1", "path_on_host": "/d/scratch.img", "is_root_device": false, "is_read_only": false},
                ],
                "machine-config": {"vcpu_count": 2, "mem_size_mib": 512, "smt": false},
                "vsock": {"guest_cid": 3, "uds_path": "/s/abc.sock"},
            })
        );
    }

    #[test]
    fn unsupported_or_unsafe_specs_are_refused() {
        let mut v = vm();
        v.hostname = "a b".into();
        assert!(firecracker_config(&v, Path::new("/k"), false).is_err());
        let mut v = vm();
        v.control_socket = format!("/{}", "s".repeat(MAX_SOCKET_PATH)).into();
        assert!(
            firecracker_config(&v, Path::new("/k"), false)
                .unwrap_err()
                .contains("bytes")
        );
        let mut v = vm();
        v.shares.push(ShareSpec {
            host_path: "/h".into(),
            tag: "sbx0".into(),
            readonly: true,
        });
        assert!(firecracker_config(&v, Path::new("/k"), false).is_err());
    }
}
