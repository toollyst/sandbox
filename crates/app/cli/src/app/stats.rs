//! `sbx stats`: 실행 중인 샌드박스의 CPU, 메모리, 쓰기 디스크, 프로세스 수. 값은 게스트가 보고한다
//! (`HostMsg::Usage`): VM 안에서 본 것이고, 표시에만 쓴다.
//!
//! CPU %는 두 표본 사이에 일한 시간의 비율에 vCPU 수를 곱한 것이다(Docker와 같다: 100%가 vCPU 하나).
//! 표준 출력이 터미널이면 [`INTERVAL`]마다 다시 그린다(`--no-stream`이면 한 번). 아니면, 그리고 JSON이면 한 번.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::time::Duration;

use sbx_guest_proto::Usage;
use sbx_model::State;

use super::{AppResult, Ctx};
use crate::{behavior, output};

/// 표본 사이의 간격.
pub const INTERVAL: Duration = Duration::from_secs(1);

/// 두 표본에서 CPU %.
pub fn cpu_percent(before: &Usage, now: &Usage) -> f64 {
    let busy = now.cpu_busy.saturating_sub(before.cpu_busy) as f64;
    let total = now.cpu_total.saturating_sub(before.cpu_total) as f64;
    if total <= 0.0 {
        return 0.0;
    }
    (busy / total * f64::from(now.cpus) * 100.0).max(0.0)
}

fn percent(used: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        used as f64 / total as f64 * 100.0
    }
}

impl Ctx {
    /// 고른 샌드박스(없으면 실행 중인 모두)의 (ID, 이름, 표본).
    async fn sample(&self, ids: &[String]) -> AppResult<Vec<(String, Option<String>, Usage)>> {
        let rt = self.runtime()?;
        let targets: Vec<_> = if ids.is_empty() {
            rt.list()
                .await?
                .into_iter()
                .filter(|s| s.state == State::Running)
                .map(|s| s.id)
                .collect()
        } else {
            let mut v = Vec::new();
            for q in ids {
                v.push(rt.find(q).await?.id().clone());
            }
            v
        };
        let mut out = Vec::new();
        for id in targets {
            let s = rt.get(&id).await?;
            match s.usage().await {
                Ok(u) => out.push((id.to_string(), s.info().spec.name.clone(), u)),
                // 고르지 않았으면 그 사이 멈춘 것은 건너뛴다.
                Err(_) if ids.is_empty() && s.state() != State::Running => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(out)
    }

    pub(super) async fn stats(&self, no_stream: bool, ids: &[String]) -> AppResult<u8> {
        let stream = !no_stream && !self.json() && std::io::stdout().is_terminal();
        let mut before: HashMap<String, Usage> = self
            .sample(ids)
            .await?
            .into_iter()
            .map(|(id, _, u)| (id, u))
            .collect();
        loop {
            tokio::time::sleep(INTERVAL).await;
            let now = self.sample(ids).await?;
            let rows: Vec<output::Stats> = now
                .iter()
                .map(|(id, name, u)| output::Stats {
                    id: id.clone(),
                    name: name.clone(),
                    cpu_percent: before.get(id).map_or(0.0, |b| cpu_percent(b, u)),
                    cpus: u.cpus,
                    memory_used: u.memory_used,
                    memory_total: u.memory_total,
                    disk_used: u.disk_used,
                    disk_total: u.disk_total,
                    pids: u.pids,
                })
                .collect();
            if self.json() {
                output::print_value(&rows)?;
                return Ok(0);
            }
            if stream {
                // 화면을 지우고 맨 위에서 다시 그린다.
                print!("\x1b[H\x1b[2J");
            }
            println!(
                "{:<12}  {:<16}  {:>7}  {:>21}  {:>6}  {:>21}  {:>5}",
                "SANDBOX",
                "NAME",
                "CPU %",
                "MEM USAGE / LIMIT",
                "MEM %",
                "DISK USAGE / SIZE",
                "PIDS"
            );
            for r in &rows {
                println!(
                    "{:<12}  {:<16}  {:>6.2}%  {:>21}  {:>5.2}%  {:>21}  {:>5}",
                    &r.id[..12.min(r.id.len())],
                    r.name.as_deref().unwrap_or("-"),
                    r.cpu_percent,
                    format!(
                        "{} / {}",
                        behavior::human_size(r.memory_used),
                        behavior::human_size(r.memory_total)
                    ),
                    percent(r.memory_used, r.memory_total),
                    format!(
                        "{} / {}",
                        behavior::human_size(r.disk_used),
                        behavior::human_size(r.disk_total)
                    ),
                    r.pids,
                );
            }
            if !stream {
                return Ok(0);
            }
            before = now.into_iter().map(|(id, _, u)| (id, u)).collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_percent_is_per_vcpu_like_docker() {
        let at = |busy, total| Usage {
            cpus: 2,
            cpu_busy: busy,
            cpu_total: total,
            ..Default::default()
        };
        // 두 vCPU 중 하나가 내내 일했다: 전체의 절반, vCPU 하나 = 100%.
        assert_eq!(cpu_percent(&at(0, 0), &at(100, 200)), 100.0);
        assert_eq!(cpu_percent(&at(100, 200), &at(100, 200)), 0.0);
        // 게스트가 거꾸로 가는 값을 보내도 음수가 되지 않는다.
        assert_eq!(cpu_percent(&at(500, 500), &at(100, 200)), 0.0);
        assert_eq!(percent(1, 4), 25.0);
        assert_eq!(percent(1, 0), 0.0);
    }
}
