//! 연결 하나를 처리한다. 순서는 `sbx-guest-proto` 문서에 있다. 요청을 `exec`, `copy`로 나눠 보내는 맨 위 층이다.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use sbx_guest_proto::{Error, ErrorKind, GuestMsg, HostMsg, PROTOCOL_VERSION};

use crate::egress::{Egress, Slot};
use crate::wire::{Conn, Out, protocol};
use crate::{copy, exec, sys};

/// 게스트 쪽 설정.
pub struct Ctx {
    /// 사용자 해석에 쓰는 rootfs 위치. 게스트에서는 `/`.
    pub root: PathBuf,
    /// `Shutdown` 요청을 마친 뒤 부른다. 게스트에서는 VM을 끈다.
    pub on_shutdown: Box<dyn Fn() + Send + Sync>,
    /// `Share` 요청을 처리한다: (태그, 대상 경로, 읽기 전용). 게스트에서는 virtio-fs를 마운트한다.
    /// 없으면 `Share`는 `InvalidArgument` 오류다.
    pub share: Option<ShareFn>,
    /// 나가는 연결 프록시. 게스트 안에서 하나를 함께 쓴다.
    pub egress: Arc<Egress>,
    /// 실행 중인 기본 프로세스 (`SignalMain`이 시그널을 보낼 곳).
    pub main: Arc<exec::Main>,
}

/// 공유 폴더를 붙이는 방법.
pub type ShareFn = Box<dyn Fn(&str, &str, bool) -> io::Result<()> + Send + Sync>;

/// 연결 하나를 끝까지 처리한다.
pub fn serve<C: Conn>(conn: C, ctx: &Ctx) {
    let Ok(writer) = conn.try_clone() else {
        return;
    };
    let out = Out::new(writer);
    let mut input = conn;

    match HostMsg::read_from(&mut input) {
        Ok(HostMsg::Hello { version }) if version == PROTOCOL_VERSION => {}
        Ok(HostMsg::Hello { version }) => {
            return out.error(Error::new(
                ErrorKind::Version,
                format!("guest speaks protocol {PROTOCOL_VERSION}, host sent {version}"),
            ));
        }
        Ok(other) => return out.error(protocol(&other, "Hello")),
        Err(_) => return,
    }
    if out
        .send(&GuestMsg::HelloAck {
            version: PROTOCOL_VERSION,
        })
        .is_err()
    {
        return;
    }

    match HostMsg::read_from(&mut input) {
        Ok(HostMsg::Exec(x)) => exec::run(x, &ctx.root, input, out, &ctx.main),
        Ok(HostMsg::CopyIn { dest }) => copy::receive(&dest, &mut input, &out),
        Ok(HostMsg::CopyOut { path }) => copy::send(&path, &out),
        Ok(HostMsg::Stat { path }) => copy::stat(&path, &out),
        Ok(HostMsg::Share {
            tag,
            target,
            readonly,
        }) => match &ctx.share {
            _ if !target.starts_with('/') => out.error(Error::new(
                ErrorKind::InvalidArgument,
                format!("share target must be absolute: {target:?}"),
            )),
            Some(f) => match f(&tag, &target, readonly) {
                Ok(()) => {
                    let _ = out.send(&GuestMsg::Done);
                }
                Err(e) => out.error(crate::wire::io_error(
                    &format!("share {tag} at {target}"),
                    &e,
                )),
            },
            None => out.error(Error::new(
                ErrorKind::InvalidArgument,
                "shares are not supported by this guest",
            )),
        },
        Ok(HostMsg::EgressListen) => match ctx.egress.listen() {
            Ok(port) => {
                let _ = out.send(&GuestMsg::Listening { port });
            }
            Err(e) => out.error(crate::wire::io_error("egress proxy", &e)),
        },
        Ok(HostMsg::EgressSlot) => match Slot::new(input) {
            // 이 연결은 이제 프록시가 쓴다. 답은 쓸 때 보낸다.
            Ok(slot) => ctx.egress.offer(slot),
            Err(e) => out.error(crate::wire::io_error("egress slot", &e)),
        },
        Ok(HostMsg::SignalMain(n)) => {
            if ctx.main.signal(n) {
                let _ = out.send(&GuestMsg::Done);
            } else {
                out.error(Error::new(
                    ErrorKind::NotFound,
                    "no main process is running",
                ));
            }
        }
        Ok(HostMsg::Shutdown) => {
            sys::sync();
            let _ = out.send(&GuestMsg::Done);
            (ctx.on_shutdown)();
        }
        Ok(other) => out.error(protocol(&other, "a request")),
        Err(_) => {}
    }
}
