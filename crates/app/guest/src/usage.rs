//! 자원 사용량 (`HostMsg::Usage`, `sbx stats`). `/proc/stat`, `/proc/meminfo`, `/proc`의 프로세스 디렉터리,
//! `/`(overlay: 위층이 쓰기 디스크다)의 statvfs를 읽는다. 해석은 순수 함수라 어느 OS에서나 테스트한다.

use std::io;

use sbx_guest_proto::Usage;

/// `/proc/stat`의 첫 줄(`cpu ...`)과 `cpuN` 줄 수에서 (CPU 수, 일한 틱, 전체 틱).
/// 일하지 않은 시간은 idle(4번째)과 iowait(5번째)이다. guest·guest_nice는 user에 이미 들어 있어 더하지 않는다.
pub fn parse_stat(s: &str) -> Option<(u32, u64, u64)> {
    let mut lines = s.lines();
    let first = lines.next()?;
    let mut f = first.split_whitespace();
    if f.next()? != "cpu" {
        return None;
    }
    let v: Vec<u64> = f.take(8).map(|x| x.parse().ok()).collect::<Option<_>>()?;
    if v.len() < 4 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    let cpus = lines
        .filter(|l| {
            l.strip_prefix("cpu")
                .and_then(|r| r.split_whitespace().next())
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count();
    Some((
        u32::try_from(cpus.max(1)).ok()?,
        total.saturating_sub(idle),
        total,
    ))
}

/// `/proc/meminfo`에서 (전체, 쓰는 것) 바이트.
pub fn parse_meminfo(s: &str) -> Option<(u64, u64)> {
    let kib = |key: &str| {
        s.lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .map(|k| k * 1024)
    };
    let total = kib("MemTotal")?;
    let available = kib("MemAvailable").or_else(|| kib("MemFree"))?;
    Some((total, total.saturating_sub(available)))
}

/// 지금의 사용량.
pub fn read() -> io::Result<Usage> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, format!("can't read {what}"));
    let (cpus, cpu_busy, cpu_total) =
        parse_stat(&std::fs::read_to_string("/proc/stat")?).ok_or_else(|| bad("/proc/stat"))?;
    let (memory_total, memory_used) = parse_meminfo(&std::fs::read_to_string("/proc/meminfo")?)
        .ok_or_else(|| bad("/proc/meminfo"))?;
    let pids = std::fs::read_dir("/proc")?
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
        })
        .count();
    let (disk_total, disk_used) = crate::sys::disk_usage("/")?;
    Ok(Usage {
        cpus,
        cpu_busy,
        cpu_total,
        memory_total,
        memory_used,
        disk_total,
        disk_used,
        pids: u32::try_from(pids).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_and_meminfo_are_parsed() {
        let stat = "cpu  100 5 50 1000 20 1 2 3 0 0\ncpu0 50 2 25 500 10 0 1 1 0 0\ncpu1 50 3 25 500 10 1 1 2 0 0\nintr 1 2\nctxt 9\n";
        assert_eq!(parse_stat(stat), Some((2, 161, 1181)));
        assert_eq!(parse_stat("intr 1\n"), None);
        assert_eq!(parse_stat("cpu  a b c d\n"), None);
        let mem = "MemTotal:        2048 kB\nMemFree:          100 kB\nMemAvailable:    1024 kB\n";
        assert_eq!(parse_meminfo(mem), Some((2048 * 1024, 1024 * 1024)));
        assert_eq!(
            parse_meminfo("MemTotal: 10 kB\nMemFree: 4 kB\n"),
            Some((10240, 6144))
        );
        assert_eq!(parse_meminfo("MemFree: 4 kB\n"), None);
    }
}
