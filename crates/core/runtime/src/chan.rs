//! 비동기 런타임에 의존하지 않는 작은 도구. 보내는 쪽은 스레드, 받는 쪽은 async다.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

struct State<T> {
    queue: VecDeque<T>,
    /// 큐에 든 값의 무게 합.
    weight: usize,
    senders: usize,
    receiver: bool,
    waker: Option<Waker>,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    /// 자리가 나거나 받는 쪽이 사라지면 깨운다.
    room: Condvar,
    max: usize,
    weigh: fn(&T) -> usize,
}

/// 보내는 쪽. 복제할 수 있다. 모두 버려지면 받는 쪽은 `None`을 받는다.
pub struct Sender<T>(Arc<Shared<T>>);

/// 받는 쪽. 버리면 보내는 쪽의 [`Sender::send`]가 `false`를 돌려준다.
pub struct Receiver<T>(Arc<Shared<T>>);

/// 무게 한도가 있는 채널. 큐의 무게 합이 `max`를 넘을 값은 자리가 날 때까지 보내는 스레드가 기다린다
/// (게스트 출력이면 그 연결을 읽지 않으므로 게스트가 기다린다). 큐가 비었으면 무게와 상관없이 하나는 들어간다.
pub fn channel<T>(max: usize, weigh: fn(&T) -> usize) -> (Sender<T>, Receiver<T>) {
    let s = Arc::new(Shared {
        state: Mutex::new(State {
            queue: VecDeque::new(),
            weight: 0,
            senders: 1,
            receiver: true,
            waker: None,
        }),
        room: Condvar::new(),
        max,
        weigh,
    });
    (Sender(s.clone()), Receiver(s))
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl<T> Sender<T> {
    /// 보낸다. 자리가 없으면 기다린다. 받는 쪽이 없으면 버리고 `false`.
    pub fn send(&self, v: T) -> bool {
        let w = (self.0.weigh)(&v);
        let mut s = lock(&self.0.state);
        loop {
            if !s.receiver {
                return false;
            }
            if s.queue.is_empty() || s.weight + w <= self.0.max {
                break;
            }
            s = self.0.room.wait(s).unwrap_or_else(|e| e.into_inner());
        }
        s.weight += w;
        s.queue.push_back(v);
        if let Some(w) = s.waker.take() {
            w.wake();
        }
        true
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        lock(&self.0.state).senders += 1;
        Self(self.0.clone())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut s = lock(&self.0.state);
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
            let mut s = lock(&self.0.state);
            if let Some(v) = s.queue.pop_front() {
                s.weight -= (self.0.weigh)(&v);
                self.0.room.notify_all();
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

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut s = lock(&self.0.state);
        s.receiver = false;
        s.queue.clear();
        s.weight = 0;
        self.0.room.notify_all();
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
        let (tx, mut rx) = channel(16, |_: &i32| 1);
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
        let (tx, mut rx) = channel(16, |_: &&str| 1);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            tx.send("late");
        });
        assert_eq!(block_on(rx.recv()), Some("late"));
    }

    #[test]
    fn a_full_channel_makes_the_sender_wait() {
        let (tx, mut rx) = channel(4, |v: &Vec<u8>| v.len());
        let sent = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n = sent.clone();
        let t = std::thread::spawn(move || {
            for _ in 0..10 {
                assert!(tx.send(vec![0; 2]));
                n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        std::thread::sleep(Duration::from_millis(50));
        // 무게 4까지만 들어간다(2 + 2).
        assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 2);
        for _ in 0..10 {
            assert_eq!(block_on(rx.recv()).map(|v| v.len()), Some(2));
        }
        t.join().unwrap();
        // 무게가 한도보다 큰 값도 큐가 비었으면 들어간다.
        let (tx, mut rx) = channel(4, |v: &Vec<u8>| v.len());
        assert!(tx.send(vec![0; 100]));
        assert_eq!(block_on(rx.recv()).map(|v| v.len()), Some(100));
    }

    #[test]
    fn dropping_the_receiver_releases_a_waiting_sender() {
        let (tx, rx) = channel(1, |_: &u8| 1);
        assert!(tx.send(1));
        let t = std::thread::spawn(move || tx.send(2));
        std::thread::sleep(Duration::from_millis(30));
        drop(rx);
        assert!(!t.join().unwrap());
    }
}
