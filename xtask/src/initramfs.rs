//! `guest-initramfs`: 게스트 init initramfs를 만든다. 실행기에 [`VmConfig::guest_init`]으로 넘기는 파일이다.
//!
//! 내용 (cpio newc, 압축하지 않음, 모든 항목 소유자 0:0, 수정 시각 0이라 같은 입력이면 같은 바이트):
//!
//! | 경로 | 종류 |
//! |---|---|
//! | `dev` | 디렉터리 0755 |
//! | `dev/console` | 문자 장치 5:1, 0600. 커널이 init을 실행하기 전에 연다(없으면 init에 표준 입출력이 없다) |
//! | `init` | 정적 `sbx-guest`, 0755 |
//!
//! `/proc`, `/sys`, `/run`은 init이 만든다.
//!
//! 게스트 바이너리는 `--binary`로 주거나, 주지 않으면 `cargo build --release -p sbx-guest --target <arch>-unknown-linux-musl`로 만든다.
//! 링커는 `rust-lld`다(musl 대상은 C 런타임을 Rust가 가지고 있어 다른 아키텍처도 별도 도구 없이 링크된다).
//! `CARGO_TARGET_<TRIPLE>_LINKER`를 정해 두었으면 그것을 쓴다. 대상은 먼저 `rustup target add`로 설치해야 한다.
//! 어느 쪽이든 그 아키텍처의 정적 ELF(인터프리터 없음)인지 확인한다.
//!
//! [`VmConfig::guest_init`]: ../../sbx_executor/struct.VmConfig.html#structfield.guest_init

use std::path::{Path, PathBuf};
use std::process::Command;

/// 게스트 아키텍처.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// x86_64.
    X86_64,
    /// aarch64.
    Aarch64,
}

impl Arch {
    fn parse(s: &str) -> Result<Self, String> {
        match s {
            "x86_64" | "amd64" => Ok(Arch::X86_64),
            "aarch64" | "arm64" => Ok(Arch::Aarch64),
            _ => Err(format!("unknown arch {s:?} (x86_64 or aarch64)")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
        }
    }

    fn triple(self) -> String {
        format!("{}-unknown-linux-musl", self.name())
    }

    /// ELF `e_machine`.
    fn elf_machine(self) -> u16 {
        match self {
            Arch::X86_64 => 0x3E,
            Arch::Aarch64 => 0xB7,
        }
    }
}

/// `cargo xtask guest-initramfs --arch <arch> [--binary <파일>] [--out <파일>]`.
pub fn run(root: &Path, args: &[String]) -> Result<PathBuf, String> {
    let mut arch = None;
    let mut binary = None;
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--arch" => arch = Some(Arch::parse(&value()?)?),
            "--binary" => binary = Some(PathBuf::from(value()?)),
            "--out" => out = Some(PathBuf::from(value()?)),
            _ => return Err(format!("unknown argument {a:?}")),
        }
    }
    let arch = arch.ok_or("--arch is required (x86_64 or aarch64)")?;
    let binary = match binary {
        Some(b) => b,
        None => build_guest(root, arch)?,
    };
    let elf = std::fs::read(&binary).map_err(|e| format!("{}: {e}", binary.display()))?;
    check_static_elf(&elf, arch).map_err(|e| format!("{}: {e}", binary.display()))?;
    let out = out.unwrap_or_else(|| {
        crate::paths::target_dir(root)
            .join("guest")
            .join(arch.name())
            .join("initramfs")
    });
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&out, archive(&elf)).map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}

fn build_guest(root: &Path, arch: Arch) -> Result<PathBuf, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let triple = arch.triple();
    let linker_var = format!(
        "CARGO_TARGET_{}_LINKER",
        triple.to_uppercase().replace('-', "_")
    );
    let mut cmd = Command::new(cargo);
    if std::env::var_os(&linker_var).is_none() {
        cmd.env(&linker_var, "rust-lld");
    }
    let status = cmd
        .current_dir(root)
        .args(["build", "--release", "-p", "sbx-guest", "--target", &triple])
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!(
            "building sbx-guest for {triple} failed (is the target installed? `rustup target add {triple}`)"
        ));
    }
    Ok(crate::paths::target_dir(root)
        .join(triple)
        .join("release")
        .join("sbx-guest"))
}

/// 그 아키텍처의 64비트 리틀 엔디언 실행 ELF이고, 동적 로더(`PT_INTERP`)가 없는지.
pub fn check_static_elf(b: &[u8], arch: Arch) -> Result<(), String> {
    let u16_at = |o: usize| b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]));
    let u32_at = |o: usize| {
        b.get(o..o + 4)
            .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    };
    let u64_at = |o: usize| {
        b.get(o..o + 8)
            .map(|s| u64::from_le_bytes(s.try_into().expect("8 bytes")))
    };
    if b.get(0..4) != Some(b"\x7fELF") {
        return Err("not an ELF file".into());
    }
    if b.get(4) != Some(&2) || b.get(5) != Some(&1) {
        return Err("not a 64-bit little-endian ELF".into());
    }
    match u16_at(16) {
        Some(2 | 3) => {} // ET_EXEC, ET_DYN(static-pie)
        _ => return Err("not an executable".into()),
    }
    let machine = u16_at(18).ok_or("truncated header")?;
    if machine != arch.elf_machine() {
        return Err(format!(
            "built for machine 0x{machine:x}, not {}",
            arch.name()
        ));
    }
    let phoff =
        usize::try_from(u64_at(32).ok_or("truncated header")?).map_err(|e| e.to_string())?;
    let phentsize = usize::from(u16_at(54).ok_or("truncated header")?);
    let phnum = usize::from(u16_at(56).ok_or("truncated header")?);
    for i in 0..phnum {
        let p_type = u32_at(phoff + i * phentsize).ok_or("truncated program headers")?;
        if p_type == 3 {
            return Err(
                "dynamically linked (has an interpreter); build a static musl binary".into(),
            );
        }
    }
    Ok(())
}

const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFCHR: u32 = 0o020000;

/// cpio 항목 하나.
struct Entry<'a> {
    name: &'a str,
    mode: u32,
    rdev: (u32, u32),
    data: &'a [u8],
}

/// initramfs 바이트.
pub fn archive(init: &[u8]) -> Vec<u8> {
    let entries = [
        Entry {
            name: "dev",
            mode: S_IFDIR | 0o755,
            rdev: (0, 0),
            data: &[],
        },
        Entry {
            name: "dev/console",
            mode: S_IFCHR | 0o600,
            rdev: (5, 1),
            data: &[],
        },
        Entry {
            name: "init",
            mode: S_IFREG | 0o755,
            rdev: (0, 0),
            data: init,
        },
    ];
    let mut out = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let nlink = if e.mode & S_IFDIR != 0 { 2 } else { 1 };
        write_entry(&mut out, i as u32 + 1, e, nlink);
    }
    write_entry(
        &mut out,
        0,
        &Entry {
            name: "TRAILER!!!",
            mode: 0,
            rdev: (0, 0),
            data: &[],
        },
        1,
    );
    // 커널은 512바이트 단위가 아니어도 읽지만, 도구들과 맞춘다.
    while !out.len().is_multiple_of(512) {
        out.push(0);
    }
    out
}

fn pad4(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

/// newc 머리 (110바이트): 매직 `070701`과 8자리 16진수 13개.
fn write_entry(out: &mut Vec<u8>, ino: u32, e: &Entry<'_>, nlink: u32) {
    let fields = [
        ino,
        e.mode,
        0, // uid
        0, // gid
        nlink,
        0, // mtime
        u32::try_from(e.data.len()).expect("init is smaller than 4 GiB"),
        0, // devmajor
        0, // devminor
        e.rdev.0,
        e.rdev.1,
        u32::try_from(e.name.len() + 1).expect("short name"),
        0, // check
    ];
    out.extend_from_slice(b"070701");
    for f in fields {
        out.extend_from_slice(format!("{f:08x}").as_bytes());
    }
    out.extend_from_slice(e.name.as_bytes());
    out.push(0);
    pad4(out);
    out.extend_from_slice(e.data);
    pad4(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (이름, mode, rdev, 내용).
    type Parsed = (String, u32, (u32, u32), Vec<u8>);

    /// newc를 다시 읽는다.
    fn parse(mut b: &[u8]) -> Vec<Parsed> {
        let start = b.len();
        let pos = |b: &[u8]| start - b.len();
        let mut out = Vec::new();
        loop {
            assert_eq!(&b[..6], b"070701");
            let field = |i: usize| {
                u32::from_str_radix(std::str::from_utf8(&b[6 + i * 8..14 + i * 8]).unwrap(), 16)
                    .unwrap()
            };
            let (mode, size, rmaj, rmin, namesize) =
                (field(1), field(6), field(9), field(10), field(11));
            let name = String::from_utf8(b[110..110 + namesize as usize - 1].to_vec()).unwrap();
            assert_eq!(b[110 + namesize as usize - 1], 0);
            b = &b[110 + namesize as usize..];
            while pos(b) % 4 != 0 {
                b = &b[1..];
            }
            let data = b[..size as usize].to_vec();
            b = &b[size as usize..];
            while pos(b) % 4 != 0 {
                b = &b[1..];
            }
            if name == "TRAILER!!!" {
                assert!(b.iter().all(|&x| x == 0));
                return out;
            }
            out.push((name, mode, (rmaj, rmin), data));
        }
    }

    #[test]
    fn archive_has_dev_console_and_init() {
        let init = b"\x7fELF fake init, odd length".to_vec();
        let a = archive(&init);
        assert_eq!(a.len() % 512, 0);
        let entries = parse(&a);
        assert_eq!(
            entries,
            vec![
                ("dev".into(), 0o040755, (0, 0), vec![]),
                ("dev/console".into(), 0o020600, (5, 1), vec![]),
                ("init".into(), 0o100755, (0, 0), init.clone()),
            ]
        );
        // 같은 입력이면 같은 바이트.
        assert_eq!(archive(&init), a);
    }

    /// 프로그램 머리 `types`를 가진 최소 ELF64.
    fn elf(machine: u16, types: &[u32]) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[0..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[16..18].copy_from_slice(&2u16.to_le_bytes());
        b[18..20].copy_from_slice(&machine.to_le_bytes());
        b[32..40].copy_from_slice(&64u64.to_le_bytes());
        b[54..56].copy_from_slice(&56u16.to_le_bytes());
        b[56..58].copy_from_slice(&(types.len() as u16).to_le_bytes());
        for t in types {
            let mut ph = vec![0u8; 56];
            ph[0..4].copy_from_slice(&t.to_le_bytes());
            b.extend(ph);
        }
        b
    }

    #[test]
    fn static_elf_check() {
        assert!(check_static_elf(&elf(0x3E, &[1, 1]), Arch::X86_64).is_ok());
        assert!(check_static_elf(&elf(0xB7, &[1]), Arch::Aarch64).is_ok());
        // 아키텍처가 다름.
        assert!(check_static_elf(&elf(0x3E, &[1]), Arch::Aarch64).is_err());
        // PT_INTERP가 있음 = 동적 링크.
        assert!(check_static_elf(&elf(0x3E, &[3, 1]), Arch::X86_64).is_err());
        assert!(check_static_elf(b"#!/bin/sh", Arch::X86_64).is_err());
        assert!(check_static_elf(&elf(0x3E, &[1])[..60], Arch::X86_64).is_err());
    }
}
