//! `system df`, `system prune`: 디스크 사용량과 쓰지 않는 것 지우기(Docker `system df`, `system prune`).
//!
//! 크기는 파일이 디스크에서 실제로 차지하는 바이트다(Unix는 할당된 블록). 쓰기 디스크(`scratch.img`)는 sparse라서
//! 정한 크기가 아니라 게스트가 쓴 만큼이다.
//!
//! `prune`이 지우는 것(명시적 동작):
//!
//! - 멈춘 샌드박스(`created`, `stopped`). 실행 중인 것은 두지 않고 건너뛴다. `--filter until=`(그 시각 전에 만든 것)과
//!   `label=K[=V]`로 좁힌다.
//! - `-a`면 남은 샌드박스 어느 것도 쓰지 않는 이미지(참조와 함께). 없으면 이미지는 지우지 않는다(sbx에는 Docker의
//!   "dangling" 이미지가 없다: 받은 이미지에는 늘 참조가 있다).
//! - 어느 이미지도 쓰지 않는 레이어 blob, 끝내지 못한 받기·만들기가 남긴 임시 파일, 지우다 멈춘 샌드박스 디렉터리.
//!   임시 것은 [`LEFTOVER_AGE`]보다 오래된 것만 지운다(다른 프로세스가 지금 쓰는 중일 수 있다).
//!
//! 지우기 전에 묻는다. 터미널이 아니면 `-f`가 있어야 한다.

use std::fs;
use std::io::{BufRead, IsTerminal};
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::*;

/// 임시 파일·디렉터리를 남은 것으로 보는 나이.
pub const LEFTOVER_AGE: Duration = Duration::from_secs(3600);

/// 파일이나 디렉터리가 디스크에서 차지하는 바이트(링크는 따라가지 않는다).
pub fn disk_usage(path: &Path) -> u64 {
    let Ok(m) = fs::symlink_metadata(path) else {
        return 0;
    };
    let own = allocated(&m);
    if !m.is_dir() {
        return own;
    }
    own + fs::read_dir(path)
        .map(|it| it.flatten().map(|e| disk_usage(&e.path())).sum::<u64>())
        .unwrap_or(0)
}

#[cfg(unix)]
fn allocated(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    m.blocks() * 512
}

#[cfg(not(unix))]
fn allocated(m: &fs::Metadata) -> u64 {
    m.len()
}

fn old_enough(path: &Path, now: SystemTime) -> bool {
    fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| now.duration_since(t).is_ok_and(|d| d >= LEFTOVER_AGE))
}

/// `dir` 안에서 `pick`이 고르고 충분히 오래된 항목을 지운다. 지운 경로와 바이트.
fn remove_leftovers(dir: &Path, pick: impl Fn(&str) -> bool, out: &mut Vec<String>) -> u64 {
    let now = SystemTime::now();
    let mut freed = 0;
    let Ok(it) = fs::read_dir(dir) else { return 0 };
    for e in it.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if !pick(&name) || !old_enough(&p, now) {
            continue;
        }
        let size = disk_usage(&p);
        let gone = if p.is_dir() {
            fs::remove_dir_all(&p)
        } else {
            fs::remove_file(&p)
        };
        if gone.is_ok() {
            freed += size;
            out.push(p.display().to_string());
        }
    }
    freed
}

impl Ctx {
    pub(super) async fn system_df(&self) -> AppResult<u8> {
        let m = self.manager()?;
        let layout = m.store().layout().clone();
        let images = m.images()?;
        let sandboxes = self.runtime()?.list().await?;
        let used: std::collections::BTreeSet<_> =
            sandboxes.iter().map(|s| s.spec.image.clone()).collect();
        // 이미지 크기: 이미지 디렉터리(rootfs 등)와 그 레이어. 같은 레이어를 쓰는 이미지가 여럿이면 한 번만 센다.
        let mut counted = std::collections::BTreeSet::new();
        let (mut size, mut reclaimable) = (0u64, 0u64);
        let layer_users = |d: &sbx_model::Digest| {
            images
                .iter()
                .filter(|i| i.record.layers.iter().any(|l| l.digest == *d))
                .map(|i| i.record.id.clone())
                .collect::<Vec<_>>()
        };
        for i in &images {
            let dir = disk_usage(&layout.image_dir(&i.record.id));
            let free = !used.contains(&i.record.id);
            size += dir;
            if free {
                reclaimable += dir;
            }
            for l in &i.record.layers {
                if !counted.insert(l.digest.clone()) {
                    continue;
                }
                let b = disk_usage(&layout.blob(&l.digest));
                size += b;
                if layer_users(&l.digest).iter().all(|u| !used.contains(u)) {
                    reclaimable += b;
                }
            }
        }
        let running = sandboxes
            .iter()
            .filter(|s| s.state == State::Running)
            .count() as u64;
        let sandbox_size =
            |s: &SandboxInfo| disk_usage(&self.data_dir.join("sandboxes").join(s.id.as_str()));
        let sb_size: u64 = sandboxes.iter().map(sandbox_size).sum();
        let sb_free: u64 = sandboxes
            .iter()
            .filter(|s| s.state != State::Running)
            .map(sandbox_size)
            .sum();
        let ex = executors::installed(&self.data_dir)
            .list()
            .map(|v| v.len())
            .unwrap_or(0)
            + executors::adapters(&self.data_dir)
                .list()
                .map(|v| v.len())
                .unwrap_or(0);
        let ex_size = disk_usage(&dirs::executors_dir(&self.data_dir))
            + disk_usage(&dirs::adapters_dir(&self.data_dir));
        let rows = [
            output::DiskUsage {
                kind: "images".into(),
                total: images.len() as u64,
                active: images
                    .iter()
                    .filter(|i| used.contains(&i.record.id))
                    .count() as u64,
                size,
                reclaimable,
            },
            output::DiskUsage {
                kind: "sandboxes".into(),
                total: sandboxes.len() as u64,
                active: running,
                size: sb_size,
                reclaimable: sb_free,
            },
            output::DiskUsage {
                kind: "executors".into(),
                total: ex as u64,
                active: 0,
                size: ex_size,
                reclaimable: 0,
            },
        ];
        if self.json() {
            output::print_value(&rows)?;
            return Ok(0);
        }
        println!(
            "{:<12} {:<7} {:<7} {:<10} RECLAIMABLE",
            "TYPE", "TOTAL", "ACTIVE", "SIZE"
        );
        for r in &rows {
            println!(
                "{:<12} {:<7} {:<7} {:<10} {}",
                r.kind,
                r.total,
                r.active,
                behavior::human_size(r.size),
                behavior::human_size(r.reclaimable)
            );
        }
        Ok(0)
    }

    pub(super) async fn system_prune(
        &self,
        all: bool,
        force: bool,
        filters: &[String],
    ) -> AppResult<u8> {
        let f = Filters::parse(filters, filter::PRUNE).map_err(msg)?;
        let until = f
            .values("until")
            .map(|v| parse::time_ns(v, now_ns()).map_err(|e| msg(format!("until: {e}"))))
            .collect::<AppResult<Vec<u64>>>()?;
        if !force {
            let what = if all {
                "all stopped sandboxes, images no sandbox uses, unused layers and leftovers"
            } else {
                "all stopped sandboxes, unused layers and leftovers"
            };
            if !std::io::stdin().is_terminal() {
                return Err(msg(format!(
                    "this removes {what}; use -f to do it without asking"
                )));
            }
            eprint!("WARNING! This will remove {what}.\nAre you sure you want to continue? [y/N] ");
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            if !matches!(line.trim(), "y" | "Y" | "yes") {
                return Ok(1);
            }
        }
        let mut report = output::PruneReport::default();
        let rt = self.runtime()?;
        let before = |s: &SandboxInfo| {
            until.iter().all(|u| {
                s.created
                    .and_then(|c| c.duration_since(std::time::UNIX_EPOCH).ok())
                    .is_some_and(|c| (c.as_nanos() as u64) < *u)
            })
        };
        for s in rt.list().await? {
            if s.state == State::Running
                || !before(&s)
                || !f.allows("label", |v| filter::label(&s.spec.labels, v))
            {
                continue;
            }
            let size = disk_usage(&self.data_dir.join("sandboxes").join(s.id.as_str()));
            match rt.get(&s.id).await {
                Ok(sb) if sb.state() != State::Running => match sb.delete().await {
                    Ok(()) => {
                        report.reclaimed += size;
                        report.sandboxes.push(s.id.to_string());
                    }
                    Err(e) => self.target_error(s.id.as_str(), &msg(e.to_string())),
                },
                _ => {}
            }
        }
        let m = self.manager()?;
        let layout = m.store().layout().clone();
        if all {
            let in_use: std::collections::BTreeSet<_> =
                rt.list().await?.into_iter().map(|s| s.spec.image).collect();
            for i in m.images()? {
                if in_use.contains(&i.record.id) {
                    continue;
                }
                let size = disk_usage(&layout.image_dir(&i.record.id));
                match m.remove(&i.record.id) {
                    Ok(r) => {
                        report.reclaimed += size;
                        report.images.push(i.record.id.to_string());
                        report
                            .layers
                            .extend(r.deleted_layers.iter().map(|d| d.to_string()));
                    }
                    Err(e) => self.target_error(&i.record.id.to_string(), &msg(e.to_string())),
                }
            }
        }
        // 어느 이미지도 쓰지 않는 레이어(지우다 멈춘 rmi가 남긴 것 포함).
        let blobs_before = disk_usage(&layout.blobs_dir());
        if let Ok(gone) = m.collect_blobs() {
            report.layers.extend(gone.iter().map(|d| d.to_string()));
        }
        report.reclaimed += blobs_before.saturating_sub(disk_usage(&layout.blobs_dir()));
        // 남은 임시 것: 끝내지 못한 받기·만들기, 반쯤 만들거나 지운 샌드박스 디렉터리(이름이 `.`로 시작한다).
        report.reclaimed += remove_leftovers(&layout.tmp_dir(), |_| true, &mut report.leftovers);
        report.reclaimed += remove_leftovers(&layout.pulls_dir(), |_| true, &mut report.leftovers);
        report.reclaimed += remove_leftovers(
            &self.data_dir.join("sandboxes"),
            |n| n.starts_with('.'),
            &mut report.leftovers,
        );
        if self.json() {
            output::print_value(&report)?;
            return Ok(0);
        }
        for (title, list) in [
            ("Deleted Sandboxes", &report.sandboxes),
            ("Deleted Images", &report.images),
            ("Deleted Layers", &report.layers),
            ("Deleted Leftovers", &report.leftovers),
        ] {
            if !list.is_empty() {
                println!("{title}:");
                list.iter().for_each(|x| println!("{x}"));
                println!();
            }
        }
        println!(
            "Total reclaimed space: {}",
            behavior::human_size(report.reclaimed)
        );
        Ok(0)
    }
}
