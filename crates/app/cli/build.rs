//! 게스트 init 내장. 배포 빌드(`cargo xtask dist`)는 `SBX_EMBED_GUEST`에 initramfs의 절대 경로를 준다.
//!
//! sbx를 빌드하는 대상 아키텍처의 것 하나만 넣는다. VM 게스트는 호스트와 아키텍처가 같아야 하기 때문이다.
//! 주지 않으면 내장하지 않는다(개발 빌드). 경로를 줬는데 파일이 없으면 빌드 오류다.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=SBX_EMBED_GUEST");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => Some("X86_64"),
        Ok("aarch64") => Some("Aarch64"),
        _ => None,
    };
    let embedded = match std::env::var_os("SBX_EMBED_GUEST").filter(|v| !v.is_empty()) {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_absolute() && path.is_file(),
                "SBX_EMBED_GUEST={} must be an absolute path to an initramfs (build one with `cargo xtask guest-initramfs`)",
                path.display()
            );
            let arch =
                arch.expect("SBX_EMBED_GUEST is set but the target architecture has no guest");
            println!("cargo:rerun-if-changed={}", path.display());
            let copy = out.join("guest.initramfs");
            std::fs::copy(&path, &copy).expect("copy initramfs");
            Some((arch, copy))
        }
        None => None,
    };
    let body = match embedded {
        Some((arch, copy)) => format!(
            "    match arch {{\n        sbx_model::Arch::{arch} => Some(include_bytes!({:?})),\n        _ => None,\n    }}\n",
            copy.display().to_string()
        ),
        None => "    let _ = arch;\n    None\n".to_owned(),
    };
    let code = format!(
        "/// 내장한 게스트 init. 그 아키텍처의 것을 내장하지 않았으면 `None`.\n\
         pub fn embedded(arch: sbx_model::Arch) -> Option<&'static [u8]> {{\n{body}}}\n"
    );
    std::fs::write(out.join("embedded_guest.rs"), code).expect("write embedded_guest.rs");
}
