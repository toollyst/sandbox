//! 연결 하나를 처리한다. 순서는 `sbx-guest-proto` 문서에 있다. 요청을 `exec`, `copy`로 나눠 보내는 맨 위 층이다.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use sbx_guest_proto::{Error, ErrorKind, GuestMsg, HostMsg, PROTOCOL_VERSION};

use crate::egress::{Egress, Slot};
use crate::layer::{self, Layers};
use crate::wire::{Conn, Out, protocol};
use crate::{changes, clock, copy, exec, sys};

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
    /// 빌드 층(`OpenLayer`, `ExportLayer`). 없으면 그 요청은 `InvalidArgument` 오류다. 규칙은 [`layer`] 모듈에 있다.
    pub layers: Option<Box<dyn Layers>>,
    /// 빌드 단계(`ExecStep`)가 남긴 프로세스를 끝내는 방법.
    pub sweep: exec::Sweep,
    /// 부팅 때 쌓은 층 디스크 수(`CheckLayers`가 견준다). 모르면 `None`이고 그때는 어느 수든 받는다.
    pub layer_disks: Option<u32>,
    /// 맞출 시계(`ClockSync`). 없으면 그 요청은 `InvalidArgument` 오류다. 규칙은 [`clock`] 모듈에 있다.
    pub clock: Option<Box<dyn clock::Clock>>,
    /// `Volume` 요청을 처리한다: (디스크 차례, 대상 경로, 읽기 전용, 채우기). 게스트에서는 [`crate::volume`]의
    /// 붙이기다. 없으면 `Volume`은 `InvalidArgument` 오류다.
    pub volume: Option<VolumeFn>,
    /// 호스트에서 돌 때(fake 실행기, 테스트) 띄운 프로세스 그룹들. 전원 끄기 흉내가 모두 끝낸다(`exec` 모듈 문서).
    /// 게스트에서는 `None`이다(전원이 꺼지면 모두 사라진다).
    pub procs: Option<Arc<exec::Procs>>,
}

/// 볼륨을 붙이는 방법.
pub type VolumeFn = Box<dyn Fn(u32, &str, bool, bool) -> io::Result<()> + Send + Sync>;

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

    let request = HostMsg::read_from(&mut input);
    // 층 열기가 실패한 뒤에는 파일 시스템을 쓰는 요청을 받지 않는다(엉뚱한 뿌리에서 하지 않게).
    if let (
        Ok(
            HostMsg::Exec(_)
            | HostMsg::ExecDetached(_)
            | HostMsg::ExecStep(_)
            | HostMsg::CopyIn { .. }
            | HostMsg::CopyOut { .. }
            | HostMsg::Stat { .. },
        ),
        Some(l),
    ) = (&request, &ctx.layers)
        && let Err(e) = l.check_ready()
    {
        return out.error(e);
    }
    match request {
        Ok(HostMsg::Exec(x)) => {
            exec::run(x, &ctx.root, input, out, &ctx.main, ctx.procs.as_deref())
        }
        Ok(HostMsg::ExecStep(x)) => match &ctx.layers {
            Some(l) if l.is_open() => exec::run_step(
                x,
                &ctx.root,
                input,
                out,
                &ctx.main,
                ctx.sweep,
                ctx.procs.as_deref(),
            ),
            _ => out.error(Error::new(
                ErrorKind::InvalidArgument,
                "a build step needs an open build layer",
            )),
        },
        Ok(HostMsg::Hold) => exec::hold(input, out, &ctx.main),
        Ok(HostMsg::CheckLayers { count }) => match ctx.layer_disks {
            Some(have) if have != count => {
                out.error(Error::new(
                    ErrorKind::InvalidArgument,
                    format!(
                        "the host attached {count} layer disks but the guest found {have}; \
                         the executor did not attach the disks it was given, so the VM is powering off"
                    ),
                ));
                (ctx.on_shutdown)();
            }
            _ => {
                let _ = out.send(&GuestMsg::Done);
            }
        },
        Ok(HostMsg::ExecDetached(x)) => exec::run_detached(x, &ctx.root, out, ctx.procs.as_deref()),
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
        Ok(HostMsg::Volume {
            disk,
            target,
            readonly,
            copy,
        }) => match &ctx.volume {
            _ if !target.starts_with('/') => out.error(Error::new(
                ErrorKind::InvalidArgument,
                format!("volume target must be absolute: {target:?}"),
            )),
            Some(f) => match f(disk, &target, readonly, copy) {
                Ok(()) => {
                    let _ = out.send(&GuestMsg::Done);
                }
                Err(e) => out.error(crate::wire::io_error(
                    &format!("volume on disk {disk} at {target}"),
                    &e,
                )),
            },
            None => out.error(Error::new(
                ErrorKind::InvalidArgument,
                "volumes are not supported by this guest",
            )),
        },
        Ok(HostMsg::ShareChanged { target, paths }) => match changes::apply(&target, &paths) {
            Ok(_) => {
                let _ = out.send(&GuestMsg::Done);
            }
            Err(e) => out.error(Error::new(ErrorKind::InvalidArgument, e)),
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
        Ok(HostMsg::Transparent { literals }) => match ctx.egress.enable_transparent(&literals) {
            Ok(()) => {
                let _ = out.send(&GuestMsg::Done);
            }
            Err(e) => out.error(Error::new(ErrorKind::Io, e)),
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
        Ok(HostMsg::Forward { port }) => crate::egress::forward(port, input, &out),
        Ok(HostMsg::Usage) => match crate::usage::read() {
            Ok(u) => {
                let _ = out.send(&GuestMsg::Usage(u));
            }
            Err(e) => out.error(crate::wire::io_error("usage", &e)),
        },
        Ok(HostMsg::ClockSync) => match &ctx.clock {
            Some(c) => clock::sync(&**c, &mut input, &out),
            None => out.error(Error::new(
                ErrorKind::InvalidArgument,
                "this guest cannot set its clock",
            )),
        },
        Ok(HostMsg::OpenLayer) => layer::open(ctx.layers.as_deref(), &out),
        Ok(HostMsg::ExportLayer) => layer::export(ctx.layers.as_deref(), &out),
        Ok(HostMsg::Shutdown) => {
            sys::sync();
            let _ = out.send(&GuestMsg::Done);
            (ctx.on_shutdown)();
        }
        Ok(other) => out.error(protocol(&other, "a request")),
        Err(_) => {}
    }
}
