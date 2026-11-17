//! `system df`, `system prune`: 디스크 사용량과 쓰지 않는 것 지우기(Docker `system df`, `system prune`).
//!
//! 크기는 파일이 디스크에서 실제로 차지하는 바이트다(Unix는 할당된 블록). 쓰기 디스크(`scratch.img`)는 sparse라서
//! 정한 크기가 아니라 게스트가 쓴 만큼이다. 이미지는 층 단위로 저장하므로(같은 층은 한 번) 여러 이미지가 함께 쓰는
//! 층은 한 번만 센다. `SHARED`는 그중 둘 이상이 쓰는 것이다.
//!
//! `prune`이 지우는 것(명시적 동작):
//!
//! - 멈춘 샌드박스(`created`, `stopped`). 실행 중인 것은 두지 않고 건너뛴다. `--filter until=`(그 시각 전에 만든 것)과
//!   `label=K[=V]`로 좁힌다.
//! - `-a`면 남은 샌드박스 어느 것도 쓰지 않는 이미지(참조와 함께). 없으면 이미지는 지우지 않는다(sbx에는 Docker의
//!   "dangling" 이미지가 없다: 받은 이미지에는 늘 참조가 있다).
//! - `--volumes`면 `volume prune`처럼 볼륨도: `-a`와 함께면 남은 샌드박스 어느 것도 가리키지 않는 볼륨(이름 붙은 볼륨도).
//!   `-a`가 없으면 지울 볼륨이 없다(sbx에는 이름 없는 볼륨이 없다). `--volumes`가 없으면 볼륨은 건드리지 않는다.
//!   `--filter label=`은 볼륨에도 적용한다(`until=`은 샌드박스에만).
//! - 어느 이미지도 쓰지 않는 레이어 blob·층 디스크·합친 디스크, 끝내지 못한 받기·만들기가 남긴 임시 파일, 지우다
//!   멈춘 샌드박스 디렉터리.
//!   임시 것은 [`LEFTOVER_AGE`]보다 오래된 것만 지운다(다른 프로세스가 지금 쓰는 중일 수 있다).
//!
//! 지우기 전에 묻는다. 터미널이 아니면 `-f`가 있어야 한다.
//!
//! `builder prune`(Docker `builder prune`): 빌드 캐시만.
//!
//! - 기본: 어느 이미지도 쓰지 않는 층을 가리키는 캐시 기록과 그 층(`system df`의 build cache 크기가 이것이다).
//!   `--keep-storage SIZE`면 마지막으로 쓴 것부터 그 크기까지 남긴다(빌드 뒤에 저절로 하는 줄이기와 같은 규칙,
//!   `Manager::trim_build_cache`).
//! - `-a`: 캐시 기록을 모두(이미지가 쓰는 층은 이미지와 함께 남는다)와 끝나지 않은 빌드의 뿌리 기록. `system prune`이
//!   빌드 캐시에 하는 것과 같다.

use std::fs;
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

/// 캐시에서 이미지들이 차지하는 것과 그 디스크 크기([`sbx_manage::Manager::stored_parts`]). 여럿이 쓰는 것도 한 번.
fn measured_parts(m: &sbx_manage::Manager) -> AppResult<Vec<(sbx_manage::StoredPart, u64)>> {
    Ok(m.stored_parts()?
        .into_iter()
        .map(|p| {
            let b = disk_usage(&p.path);
            (p, b)
        })
        .collect())
}

/// 이미지 하나의 디스크 크기: 그것이 쓰는 것 모두, 그중 다른 이미지도 쓰는 것, 그 이미지만 쓰는 것(`rmi`가 비우는 것).
fn image_sizes(
    parts: &[(sbx_manage::StoredPart, u64)],
    id: &sbx_model::ImageId,
) -> (u64, u64, u64) {
    let mut out = (0, 0, 0);
    for (p, b) in parts.iter().filter(|(p, _)| p.users.contains(id)) {
        out.0 += b;
        if p.users.len() > 1 {
            out.1 += b;
        } else {
            out.2 += b;
        }
    }
    out
}

impl Ctx {
    /// 이미지 ID마다 (SIZE, SHARED, UNIQUE)를 주는 함수([`image_sizes`]). `images`가 쓴다.
    pub(super) fn image_disk_sizes(
        &self,
        m: &sbx_manage::Manager,
    ) -> AppResult<impl Fn(&sbx_model::ImageId) -> (u64, u64, u64)> {
        let parts = measured_parts(m)?;
        Ok(move |id: &sbx_model::ImageId| image_sizes(&parts, id))
    }

    pub(super) async fn system_df(&self) -> AppResult<u8> {
        let m = self.manager()?;
        let images = m.images()?;
        let sandboxes = self.runtime()?.list().await?;
        let used: std::collections::BTreeSet<_> =
            sandboxes.iter().map(|s| s.spec.image.clone()).collect();
        // 이미지 크기: 이미지 디렉터리, blob, 층 디스크, 합친 디스크. 여러 이미지가 함께 쓰는 것은 한 번만 센다.
        let parts = measured_parts(&m)?;
        let size: u64 = parts.iter().map(|(_, b)| b).sum();
        let shared: u64 = parts
            .iter()
            .filter(|(p, _)| p.users.len() > 1)
            .map(|(_, b)| b)
            .sum();
        let reclaimable: u64 = parts
            .iter()
            .filter(|(p, _)| p.users.iter().all(|u| !used.contains(u)))
            .map(|(_, b)| b)
            .sum();
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
        // 실행기와 어댑터. 실행 중인 샌드박스가 그 설치 디렉터리의 파일로 띄웠으면 쓰는 중이다.
        let installed: Vec<_> = [
            executors::installed(&self.data_dir),
            executors::adapters(&self.data_dir),
        ]
        .iter()
        .flat_map(|x| x.list().unwrap_or_default())
        .collect();
        let ex = installed.len();
        let ex_active = installed
            .iter()
            .filter(|i| {
                sandboxes.iter().any(|s| {
                    s.state == State::Running
                        && s.executor_files.iter().any(|f| f.starts_with(&i.dir))
                })
            })
            .count() as u64;
        // 빌드 캐시: 캐시 기록만 쥔 층(이미지도 쓰는 층은 images에서 센다).
        let cache = m.build_cache_usage()?;
        let cache_only: u64 = cache.only_cache.iter().map(|p| disk_usage(p)).sum();
        let cache_shared: u64 = cache.shared.iter().map(|p| disk_usage(p)).sum();
        let cache_records = disk_usage(&m.store().layout().build_cache_dir());
        let ex_size = disk_usage(&dirs::executors_dir(&self.data_dir))
            + disk_usage(&dirs::adapters_dir(&self.data_dir));
        // 볼륨: 디스크가 실제로 차지하는 것. 가리키는 샌드박스가 없는 것을 `system prune -a --volumes`가 지운다.
        let rt = self.runtime()?;
        let volumes = rt.volumes()?;
        let volume_users = rt.volume_users().await?;
        let vol_size: u64 = volumes.iter().map(|v| disk_usage(&v.dir)).sum();
        let vol_free: u64 = volumes
            .iter()
            .filter(|v| !volume_users.contains_key(&v.name))
            .map(|v| disk_usage(&v.dir))
            .sum();
        let vol_active = volumes
            .iter()
            .filter(|v| sbx_runtime::volume::in_use(&self.data_dir, &v.name).unwrap_or(false))
            .count() as u64;
        let rows = [
            output::DiskUsage {
                kind: "images".into(),
                total: images.len() as u64,
                active: images
                    .iter()
                    .filter(|i| used.contains(&i.record.id))
                    .count() as u64,
                size,
                shared,
                reclaimable,
            },
            output::DiskUsage {
                kind: "build cache".into(),
                total: cache.records as u64,
                active: 0,
                size: cache_only + cache_records,
                shared: cache_shared,
                reclaimable: cache_only + cache_records,
            },
            output::DiskUsage {
                kind: "sandboxes".into(),
                total: sandboxes.len() as u64,
                active: running,
                size: sb_size,
                shared: 0,
                reclaimable: sb_free,
            },
            output::DiskUsage {
                kind: "volumes".into(),
                total: volumes.len() as u64,
                active: vol_active,
                size: vol_size,
                shared: 0,
                reclaimable: vol_free,
            },
            output::DiskUsage {
                kind: "executors".into(),
                total: ex as u64,
                active: ex_active,
                size: ex_size,
                shared: 0,
                reclaimable: 0,
            },
        ];
        if self.json() {
            output::print_value(&rows)?;
            return Ok(0);
        }
        println!(
            "{:<12} {:<7} {:<7} {:<10} {:<10} RECLAIMABLE",
            "TYPE", "TOTAL", "ACTIVE", "SIZE", "SHARED"
        );
        for r in &rows {
            println!(
                "{:<12} {:<7} {:<7} {:<10} {:<10} {}",
                r.kind,
                r.total,
                r.active,
                behavior::human_size(r.size),
                behavior::human_size(r.shared),
                behavior::human_size(r.reclaimable)
            );
        }
        Ok(0)
    }

    pub(super) async fn system_prune(
        &self,
        all: bool,
        volumes: bool,
        force: bool,
        filters: &[String],
    ) -> AppResult<u8> {
        let f = Filters::parse(filters, filter::PRUNE).map_err(msg)?;
        let until = f
            .values("until")
            .map(|v| parse::time_ns(v, now_ns()).map_err(|e| msg(format!("until: {e}"))))
            .collect::<AppResult<Vec<u64>>>()?;
        if !force {
            let what = match (all, volumes) {
                (true, true) => {
                    "all stopped sandboxes, images and volumes no sandbox uses, the build cache, unused layers and leftovers"
                }
                (true, false) => {
                    "all stopped sandboxes, images no sandbox uses, the build cache, unused layers and leftovers"
                }
                (false, _) => "all stopped sandboxes, the build cache, unused layers and leftovers",
            };
            if !super::volume::confirm(what)? {
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
        if volumes && all {
            let only_labels = Filters::parse(
                &filters
                    .iter()
                    .filter(|v| v.starts_with("label="))
                    .cloned()
                    .collect::<Vec<_>>(),
                filter::VOLUME_PRUNE,
            )
            .map_err(msg)?;
            let (gone, freed) = self.prune_volumes(&rt, &only_labels).await?;
            report.volumes = gone;
            report.reclaimed += freed;
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
                // 지우면 비는 것: 이 이미지만 쓰던 것(앞에서 지운 이미지와 함께 쓰던 것도 그때는 이 이미지만 쓴다).
                let parts = measured_parts(&m)?;
                let size = image_sizes(&parts, &i.record.id).2;
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
        // 빌드 캐시 기록(캐시가 쥔 층은 그다음 정리가 지운다)과 끝나지 않은 빌드의 뿌리 기록.
        let records = disk_usage(&layout.build_cache_dir());
        match m.prune_build_cache() {
            Ok(n) => {
                report.build_cache = n as u64;
                report.reclaimed += records.saturating_sub(disk_usage(&layout.build_cache_dir()));
            }
            Err(e) => self.target_error("build cache", &msg(e.to_string())),
        }
        // 어느 이미지도 쓰지 않는 레이어 blob, 층 디스크, 합친 디스크(지우다 멈춘 rmi가 남긴 것 포함).
        let stored = |l: &sbx_store::CacheLayout| {
            disk_usage(&l.blobs_dir()) + disk_usage(&l.layers_dir()) + disk_usage(&l.merged_dir())
        };
        let before = stored(&layout);
        if let Ok(gone) = m.collect() {
            report
                .layers
                .extend(gone.blobs.iter().map(|d| d.to_string()));
        }
        report.reclaimed += before.saturating_sub(stored(&layout));
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
        if report.build_cache > 0 {
            println!("Deleted Build Cache: {} steps\n", report.build_cache);
        }
        for (title, list) in [
            ("Deleted Sandboxes", &report.sandboxes),
            ("Deleted Images", &report.images),
            ("Deleted Volumes", &report.volumes),
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

    /// `builder prune`: 빌드 캐시를 지운다(모듈 문서의 아래). 층은 같은 자리에서 정리한다.
    pub(super) fn builder_prune(
        &self,
        all: bool,
        force: bool,
        keep_storage: Option<&str>,
    ) -> AppResult<u8> {
        let keep = keep_storage
            .map(|k| parse::size_or_zero(k).map_err(|e| msg(format!("--keep-storage {k}: {e}"))))
            .transpose()?;
        if all && keep.is_some() {
            return Err(msg(
                "--keep-storage keeps cache that no image uses; it can't be used with -a",
            ));
        }
        if !force {
            let what = if all {
                "all build cache"
            } else {
                "build cache that no image uses"
            };
            if !super::volume::confirm(what)? {
                return Ok(0);
            }
        }
        let m = self.manager()?;
        let layout = m.store().layout().clone();
        let stored = |l: &CacheLayout| {
            disk_usage(&l.blobs_dir())
                + disk_usage(&l.layers_dir())
                + disk_usage(&l.merged_dir())
                + disk_usage(&l.build_cache_dir())
        };
        let before = stored(&layout);
        let removed = if all {
            let n = m.prune_build_cache()?;
            m.collect()?;
            n
        } else {
            m.trim_build_cache(keep.unwrap_or(0), &disk_usage)?.records
        };
        let report = output::BuildCachePruned {
            build_cache: removed as u64,
            reclaimed: before.saturating_sub(stored(&layout)),
        };
        if self.json() {
            output::print_value(&report)?;
            return Ok(0);
        }
        println!("Deleted build cache: {} steps", report.build_cache);
        println!(
            "Total reclaimed space: {}",
            behavior::human_size(report.reclaimed)
        );
        Ok(0)
    }
}
