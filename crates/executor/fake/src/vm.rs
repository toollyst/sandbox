//! 가짜 VM 하나의 상태와 핸들. 크레이트의 가장 아래 모듈이다.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sbx_executor::{ExecutorError, VmHandle};
use sbx_model::BoxFuture;

/// VM 하나의 상태. 끝났는지와 그 이유를 여러 스레드가 본다.
#[derive(Debug, Default)]
pub(crate) struct Life {
    reason: Mutex<Option<String>>,
    changed: Condvar,
}

impl Life {
    pub(crate) fn reason(&self) -> Option<String> {
        self.reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 처음 끝낼 때만 `true`. 게스트 쪽(unix)만 끝낸다.
    #[cfg(unix)]
    pub(crate) fn end(&self, reason: &str) -> bool {
        let mut r = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        let first = r.is_none();
        if first {
            *r = Some(reason.to_owned());
        }
        self.changed.notify_all();
        first
    }

    pub(crate) fn wait(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut r = self.reason.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(reason) = &*r {
                return Some(reason.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            r = self
                .changed
                .wait_timeout(r, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// VM 종료 이유: 게스트가 종료를 요청함.
pub const REASON_GUEST_SHUTDOWN: &str = "guest powered off";
/// VM 종료 이유: 강제로 끝냄.
pub const REASON_KILLED: &str = "killed";

/// 가짜 VM.
pub struct FakeVm {
    pub(crate) life: Arc<Life>,
    pub(crate) stop: Arc<dyn Fn(&str) + Send + Sync>,
}

impl std::fmt::Debug for FakeVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeVm")
            .field("exit", &self.life.reason())
            .finish()
    }
}

impl FakeVm {
    /// 아직 실행 중인지.
    pub fn is_running(&self) -> bool {
        self.life.reason().is_none()
    }

    /// 끝날 때까지 기다려 이유를 돌려준다(블로킹).
    pub fn wait_blocking(&self, timeout: Duration) -> Option<String> {
        self.life.wait(timeout)
    }

    /// 강제로 끝낸다.
    pub fn stop(&self, reason: &str) {
        (self.stop)(reason);
    }
}

impl VmHandle for FakeVm {
    fn pid(&self) -> Option<u32> {
        None
    }

    /// 블로킹으로 기다린다.
    fn wait(&mut self, timeout: Duration) -> BoxFuture<'_, Result<bool, ExecutorError>> {
        let life = self.life.clone();
        Box::pin(async move { Ok(life.wait(timeout).is_some()) })
    }

    fn kill(&mut self) -> Result<(), ExecutorError> {
        (self.stop)(REASON_KILLED);
        Ok(())
    }
}

impl Drop for FakeVm {
    fn drop(&mut self) {
        (self.stop)(REASON_KILLED);
    }
}
