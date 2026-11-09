//! 규약의 [`VmSpec`]을 Firecracker 설정 파일(`--config-file`)로 바꾼다. 규칙은 크레이트 문서에 있다.

use std::path::Path;

use executor_proto::{DiskRole, VmSpec};
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
    let mut boot_args = String::from("console=ttyS0 reboot=k panic=1");
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
            };
            Ok(json!({
                "drive_id": id,
                "path_on_host": utf8(&d.path, "disk")?,
                "is_root_device": false,
                "is_read_only": d.readonly,
            }))
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
    use executor_proto::{DiskSpec, ShareSpec};

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
            "console=ttyS0 reboot=k panic=1 sbx.hostname=box-1 sbx.halt=reboot"
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
