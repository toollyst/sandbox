//! 연결 하나를 처리한다. 순서는 `sandbox-proto` 문서에 있다. 요청을 `exec`, `copy`로 나눠 보내는 맨 위 층이다.

use std::path::PathBuf;

use sandbox_proto::{Error, ErrorKind, GuestMsg, HostMsg, PROTOCOL_VERSION};

use crate::wire::{Conn, Out, protocol};
use crate::{copy, exec, sys};

/// 게스트 쪽 설정.
pub struct Ctx {
    /// 사용자 해석에 쓰는 rootfs 위치. 게스트에서는 `/`.
    pub root: PathBuf,
    /// `Shutdown` 요청을 마친 뒤 부른다. 게스트에서는 VM을 끈다.
    pub on_shutdown: Box<dyn Fn() + Send + Sync>,
}

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
        Ok(HostMsg::Exec(x)) => exec::run(x, &ctx.root, input, out),
        Ok(HostMsg::CopyIn { dest }) => copy::receive(&dest, &mut input, &out),
        Ok(HostMsg::CopyOut { path }) => copy::send(&path, &out),
        Ok(HostMsg::Stat { path }) => copy::stat(&path, &out),
        Ok(HostMsg::Shutdown) => {
            sys::sync();
            let _ = out.send(&GuestMsg::Done);
            (ctx.on_shutdown)();
        }
        Ok(other) => out.error(protocol(&other, "a request")),
        Err(_) => {}
    }
}
