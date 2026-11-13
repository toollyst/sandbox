//! 부팅 때 붙은 디스크에서 이미지의 층을 찾는 규칙. 커널 명령줄도 실행기 기능도 쓰지 않고 디스크 차례와 내용만 본다.
//!
//! 실행기는 받은 차례대로 virtio-blk 장치를 붙인다(`sbx-executor-proto`의 "게스트에 대한 약속"). n번째 디스크가
//! n번째 장치 이름이다([`device`]: `vda`, `vdb`, …, `vdz`, `vdaa`, …).
//!
//! - 첫째(`vda`)는 이미지의 맨 아래 층이다. EROFS여야 한다(아니면 마운트가 실패하고 부팅이 멈춘다).
//! - 둘째(`vdb`)는 scratch다.
//! - 셋째부터 EROFS인 디스크([`is_erofs`]: 1024바이트 자리의 EROFS 매직)는 위 층들이다. 아래부터 차례다.
//! - 처음으로 장치가 없거나 EROFS가 아닌 디스크에서 층이 끝난다. 그 뒤의 디스크는 EROFS여도 층이 아니다.
//!
//! 그래서 층 뒤에 붙는 다른 디스크(V1의 이름 붙은 볼륨, ext4)는 층으로 읽히지 않는다. 볼륨은 호스트가 부팅 뒤 게스트
//! 규약으로 알린다. 지킬 것: 호스트는 1024바이트 자리의 4바이트가 EROFS 매직인 볼륨을 붙이지 않는다(ext4는 그 자리가
//! inode 수이므로 정상인 ext4는 그렇지 않다. 게스트가 볼륨 장치에 직접 쓸 수 있으므로 붙일 때마다 본다). 그러면 층의
//! 수는 호스트가 붙인 층 디스크의 수와 같다.
//!
//! 층 수는 [`MAX_LAYER_DISKS`]까지 센다(overlayfs의 아래층 상한 `OVL_MAX_STACK`과 같다).

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// EROFS superblock의 자리와 매직.
const EROFS_MAGIC_OFFSET: u64 = 1024;
const EROFS_MAGIC: u32 = 0xE0F5_E1E2;

/// 부팅 때 쌓는 층의 상한.
pub const MAX_LAYER_DISKS: usize = 500;

/// `index`(0부터)번째 virtio-blk 장치 이름(Linux `virtblk_name_format`과 같다): `vda`…`vdz`, `vdaa`….
pub fn device(index: usize) -> String {
    let mut letters = Vec::new();
    let mut n = index;
    loop {
        letters.push(b'a' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    letters.reverse();
    format!("vd{}", String::from_utf8_lossy(&letters))
}

/// EROFS 이미지인지: 1024바이트 자리에 EROFS 매직이 있다. 짧으면 아니다.
pub fn is_erofs(r: &mut (impl Read + Seek)) -> io::Result<bool> {
    let mut m = [0u8; 4];
    r.seek(SeekFrom::Start(EROFS_MAGIC_OFFSET))?;
    match r.read_exact(&mut m) {
        Ok(()) => Ok(u32::from_le_bytes(m) == EROFS_MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// `dev`(보통 `/dev`)에서 층 디스크들을 아래부터 찾는다. 규칙은 모듈 문서에 있다. 첫째는 내용을 보지 않고 넣는다.
pub fn layer_devices(dev: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = vec![dev.join(device(0))];
    for index in 2.. {
        if out.len() >= MAX_LAYER_DISKS {
            break;
        }
        let path = dev.join(device(index));
        let mut f = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(io::Error::new(e.kind(), format!("{}: {e}", path.display()))),
        };
        if !is_erofs(&mut f)? {
            break;
        }
        out.push(path);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_names_follow_the_kernel() {
        assert_eq!(device(0), "vda");
        assert_eq!(device(1), "vdb");
        assert_eq!(device(25), "vdz");
        assert_eq!(device(26), "vdaa");
        assert_eq!(device(27), "vdab");
        assert_eq!(device(26 + 26 * 26), "vdaaa");
    }

    fn erofs() -> Vec<u8> {
        let mut b = vec![0u8; 4096];
        b[1024..1028].copy_from_slice(&EROFS_MAGIC.to_le_bytes());
        b
    }

    #[test]
    fn layers_are_the_first_disk_and_the_erofs_disks_after_scratch() {
        let dir = std::env::temp_dir().join(format!("sbx-guest-disks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let put = |name: &str, bytes: &[u8]| std::fs::write(dir.join(name), bytes).unwrap();
        put("vda", &erofs());
        // scratch는 처음에 0으로 채워져 있다.
        put("vdb", &[0u8; 8192]);
        assert_eq!(layer_devices(&dir).unwrap(), [dir.join("vda")]);
        put("vdc", &erofs());
        put("vdd", &erofs());
        // ext4(1024 자리는 inode 수)인 볼륨 뒤의 EROFS는 층이 아니다.
        let mut ext4 = vec![0u8; 4096];
        ext4[1024..1028].copy_from_slice(&4096u32.to_le_bytes());
        ext4[1024 + 56..1024 + 58].copy_from_slice(&0xEF53u16.to_le_bytes());
        put("vde", &ext4);
        put("vdf", &erofs());
        let want: Vec<PathBuf> = ["vda", "vdc", "vdd"].iter().map(|n| dir.join(n)).collect();
        assert_eq!(layer_devices(&dir).unwrap(), want);
        // 짧은 디스크도 EROFS가 아니다.
        assert!(!is_erofs(&mut io::Cursor::new(vec![0u8; 100])).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
