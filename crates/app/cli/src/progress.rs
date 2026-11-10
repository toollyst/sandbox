//! 터미널 진행 표시. stderr가 터미널일 때만 한 줄을 고쳐 그린다. 파이프와 로그 파일에는 아무것도 쓰지 않는다.

use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sbx_manage::PullProgress;

use crate::behavior::human_size;

/// 다시 그리는 최소 간격. 바이트가 들어올 때마다 그리지 않는다.
const REDRAW: Duration = Duration::from_millis(80);
/// 막대 칸 수.
const BAR: usize = 20;

/// stderr의 진행 줄 하나.
pub struct Status {
    enabled: bool,
    /// 마지막으로 그린 때. 그린 적이 없으면 `None`.
    drawn: Mutex<Option<Instant>>,
}

impl Status {
    /// stderr가 터미널이면 그린다.
    pub fn new() -> Self {
        Self {
            enabled: io::stderr().is_terminal(),
            drawn: Mutex::new(None),
        }
    }

    /// 받기 진행을 그린다. 끝났을 때는 간격과 상관없이 그린다.
    pub fn pull(&self, label: &str, p: PullProgress) {
        self.draw(&render_pull(label, p), p.layers_done == p.layers);
    }

    /// 진행 줄에 글을 쓴다(오래 걸리는 단계 앞에).
    pub fn text(&self, text: &str) {
        self.draw(text, true);
    }

    /// 진행 줄을 지운다. 그 뒤 다른 출력이 같은 줄에 섞이지 않는다.
    pub fn clear(&self) {
        let mut drawn = self.drawn.lock().unwrap_or_else(|e| e.into_inner());
        if drawn.take().is_some() {
            let mut err = io::stderr().lock();
            let _ = write!(err, "\r\x1b[2K");
            let _ = err.flush();
        }
    }

    fn draw(&self, line: &str, force: bool) {
        if !self.enabled {
            return;
        }
        let mut drawn = self.drawn.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        if !force && drawn.is_some_and(|t| now.duration_since(t) < REDRAW) {
            return;
        }
        *drawn = Some(now);
        // 크기를 모르면(0을 주는 터미널도 있다) 80칸으로 본다.
        let width = crossterm::terminal::size()
            .ok()
            .filter(|(w, _)| *w > 0)
            .map_or(80, |(w, _)| usize::from(w));
        let line: String = line.chars().take(width.saturating_sub(1)).collect();
        let mut err = io::stderr().lock();
        let _ = write!(err, "\r\x1b[2K{line}");
        let _ = err.flush();
    }
}

impl Default for Status {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Status {
    fn drop(&mut self) {
        self.clear();
    }
}

/// `Pulling alpine:3.20 [=======>            ] 1.2MB/3.6MB, 1/2 layers`
pub fn render_pull(label: &str, p: PullProgress) -> String {
    let filled = if p.bytes == 0 {
        BAR
    } else {
        ((p.bytes_done.min(p.bytes) as u128 * BAR as u128) / p.bytes as u128) as usize
    };
    let mut bar = "=".repeat(filled);
    if filled < BAR {
        bar.push('>');
        bar.push_str(&" ".repeat(BAR - filled - 1));
    }
    format!(
        "Pulling {label} [{bar}] {}/{}, {}/{} layers",
        human_size(p.bytes_done),
        human_size(p.bytes),
        p.layers_done,
        p.layers
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_line() {
        let p = |done, layers_done| PullProgress {
            layers: 2,
            layers_done,
            bytes: 4_000_000,
            bytes_done: done,
        };
        assert_eq!(
            render_pull("alpine", p(0, 0)),
            "Pulling alpine [>                   ] 0B/4.0MB, 0/2 layers"
        );
        assert_eq!(
            render_pull("alpine", p(1_000_000, 1)),
            "Pulling alpine [=====>              ] 1.0MB/4.0MB, 1/2 layers"
        );
        assert_eq!(
            render_pull("alpine", p(4_000_000, 2)),
            "Pulling alpine [====================] 4.0MB/4.0MB, 2/2 layers"
        );
    }
}
