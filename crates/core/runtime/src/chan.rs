//! 비동기 런타임에 의존하지 않는 작은 도구. 보내는 쪽은 스레드, 받는 쪽은 async다.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

struct State<T> {
    queue: VecDeque<T>,
    senders: usize,
    waker: Option<Waker>,
}

/// 보내는 쪽. 복제할 수 있다. 모두 버려지면 받는 쪽은 `None`을 받는다.
pub struct Sender<T>(Arc<Mutex<State<T>>>);

/// 받는 쪽.
pub struct Receiver<T>(Arc<Mutex<State<T>>>);

/// 한계 없는 채널.
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let s = Arc::new(Mutex::new(State {
        queue: VecDeque::new(),
        senders: 1,
        waker: None,
    }));
    (Sender(s.clone()), Receiver(s))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl<T> Sender<T> {
    /// 보낸다. 받는 쪽이 없어도 실패하지 않는다.
    pub fn send(&self, v: T) {
        let mut s = lock(&self.0);
        s.queue.push_back(v);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        lock(&self.0).senders += 1;
        Self(self.0.clone())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut s = lock(&self.0);
        s.senders -= 1;
        if s.senders == 0
            && let Some(w) = s.waker.take()
        {
            w.wake();
        }
    }
}

impl<T> Receiver<T> {
    /// 다음 값. 보내는 쪽이 모두 사라지고 비었으면 `None`.
    pub fn recv(&mut self) -> impl Future<Output = Option<T>> + '_ {
        std::future::poll_fn(move |cx| {
            let mut s = lock(&self.0);
            if let Some(v) = s.queue.pop_front() {
                return Poll::Ready(Some(v));
            }
            if s.senders == 0 {
                return Poll::Ready(None);
            }
            s.waker = Some(cx.waker().clone());
            Poll::Pending
        })
    }
}

struct Unpark(std::thread::Thread);

impl Wake for Unpark {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}

/// 스레드 안에서 future 하나를 끝까지 돌린다. 비동기 런타임 밖(작업 스레드)에서만 쓴다.
pub fn block_on<F: Future>(f: F) -> F::Output {
    let waker = Arc::new(Unpark(std::thread::current())).into();
    let mut cx = Context::from_waker(&waker);
    let mut f = std::pin::pin!(f);
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::park_timeout(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_then_none_after_senders_drop() {
        let (tx, mut rx) = channel();
        let tx2 = tx.clone();
        let t = std::thread::spawn(move || {
            tx.send(1);
            tx2.send(2);
        });
        t.join().unwrap();
        assert_eq!(block_on(rx.recv()), Some(1));
        assert_eq!(block_on(rx.recv()), Some(2));
        assert_eq!(block_on(rx.recv()), None);
    }

    #[test]
    fn receiver_wakes_when_value_arrives_later() {
        let (tx, mut rx) = channel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send("late");
        });
        assert_eq!(block_on(rx.recv()), Some("late"));
    }
}
