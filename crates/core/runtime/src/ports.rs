//! 포트 공개 (`-p`, 호스트 쪽). VM을 소유한 프로세스(붙어 실행하는 sbx나 `sbx-shim`)가 듣는다.
//!
//! - VM을 띄우기 **전에** 모든 주소에 bind한다([`bind`]). 하나라도 실패하면(이미 쓰는 포트) 시작이 실패한다.
//!   포트 0이면 OS가 고른 포트이고, 고른 주소는 상태 기록에 남는다(`inspect`가 보인다).
//! - 게스트가 준비되면 [`serve`]가 받기 시작한다. 연결마다 게스트에 새 연결을 열어 `HostMsg::Forward`를 보내고,
//!   `Done`이면 나가는 연결과 같은 방법([`crate::egress`]의 splice)으로 잇는다. 아니면 그 연결만 닫는다.
//! - 동시에 열린 연결은 샌드박스마다 [`crate::egress::MAX_CONNECTIONS`]개까지다(나가는 연결과 따로 센다).
//! - [`Forwarding`]을 버리면(샌드박스가 멈춤) 듣기를 멈춘다. 이미 이어진 연결은 VM이 끝나면 끊긴다.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;

use sbx_model::PortMapping;

use crate::error::RuntimeError;
use crate::client::Guest;

/// bind한 주소들. 아직 받지 않는다.
#[derive(Debug)]
pub struct Bound {
    listeners: Vec<(TcpListener, u16)>,
}

impl Bound {
    /// 공개할 포트가 없는가.
    pub fn listeners_empty(&self) -> bool {
        self.listeners.is_empty()
    }

    /// (호스트 주소, 게스트 포트).
    pub fn published(&self) -> Vec<(SocketAddr, u16)> {
        self.listeners
            .iter()
            .filter_map(|(l, g)| l.local_addr().ok().map(|a| (a, *g)))
            .collect()
    }
}

/// 모두 bind한다.
pub fn bind(maps: &[PortMapping]) -> Result<Bound, RuntimeError> {
    let mut listeners = Vec::with_capacity(maps.len());
    for m in maps {
        let addr = SocketAddr::new(m.host_ip, m.host_port);
        let l = TcpListener::bind(addr)
            .map_err(|e| RuntimeError::Invalid(format!("-p {m}: can't listen on {addr}: {e}")))?;
        listeners.push((l, m.guest_port));
    }
    Ok(Bound { listeners })
}

/// 받는 중. 버리면 멈춘다.
#[derive(Debug)]
pub struct Forwarding {
    stop: Arc<AtomicBool>,
    addrs: Vec<SocketAddr>,
}

impl Drop for Forwarding {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // accept에서 기다리는 스레드를 깨운다.
        for a in &self.addrs {
            let wake = if a.ip().is_unspecified() {
                SocketAddr::new(
                    if a.is_ipv4() {
                        std::net::Ipv4Addr::LOCALHOST.into()
                    } else {
                        std::net::Ipv6Addr::LOCALHOST.into()
                    },
                    a.port(),
                )
            } else {
                *a
            };
            let _ = TcpStream::connect_timeout(&wake, std::time::Duration::from_secs(1));
        }
    }
}

/// 받기 시작한다.
pub fn serve(bound: Bound, guest: Guest) -> Forwarding {
    let stop = Arc::new(AtomicBool::new(false));
    let open = Arc::new(AtomicUsize::new(0));
    let mut addrs = Vec::new();
    for (listener, guest_port) in bound.listeners {
        if let Ok(a) = listener.local_addr() {
            addrs.push(a);
        }
        let (stop, open, guest) = (stop.clone(), open.clone(), guest.clone());
        thread::spawn(move || {
            for tcp in listener.incoming() {
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(tcp) = tcp else { continue };
                if open.fetch_add(1, Ordering::SeqCst) >= crate::egress::MAX_CONNECTIONS {
                    open.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
                let (open, guest) = (open.clone(), guest.clone());
                thread::spawn(move || {
                    if let Ok(conn) = guest.forward(guest_port) {
                        crate::egress::splice(conn, tcp);
                    }
                    open.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
    }
    Forwarding { stop, addrs }
}
