//! 가짜 게스트: 제어 소켓에서 vsock 방식으로 연결을 받아 `sbx-guest`의 연결 처리를 돌린다.

#[cfg(not(unix))]
pub(crate) use other::connect;
#[cfg(unix)]
pub(crate) use unix::{connect, start};

#[cfg(unix)]
mod unix {
    use std::io;
    use std::sync::Arc;

    use sbx_executor::{ChannelSpec, ExecutorError, GuestConn};

    use crate::vm::{FakeVm, Life, REASON_GUEST_SHUTDOWN};

    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::thread;

    use sbx_executor_proto::vsock;
    use sbx_guest::server::{self, Ctx};

    /// 공유 폴더 흉내: 대상 경로에 호스트 경로를 가리키는 심볼릭 링크를 둔다. 대상이 빈 디렉터리면 바꾼다.
    fn link_share(host: &Path, target: &Path) -> io::Result<()> {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        match std::fs::symlink_metadata(target) {
            Ok(m) if m.is_dir() => std::fs::remove_dir(target)?,
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} exists", target.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        std::os::unix::fs::symlink(host, target)
    }

    pub(crate) fn connect(channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Ok(Box::new(vsock::connect(
            &channel.host_endpoint,
            channel.guest_port,
        )?))
    }

    /// 투명 TCP의 게스트 커널 흉내. 규칙은 [`crate::netdir::net_dir`]에 있다.
    struct FakeNet {
        dir: PathBuf,
    }

    impl FakeNet {
        fn append(&self, line: &str) -> io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join(crate::netdir::NET_ROUTES))?;
            f.write_all(format!("{line}\n").as_bytes())
        }
    }

    impl sbx_guest::transparent::Net for FakeNet {
        fn make_range_local(&self) -> io::Result<()> {
            Ok(())
        }

        fn make_local(&self, _ip: std::net::Ipv4Addr) -> io::Result<()> {
            Ok(())
        }

        fn listen(&self, addr: std::net::SocketAddrV4) -> io::Result<std::net::TcpListener> {
            let l = std::net::TcpListener::bind("127.0.0.1:0")?;
            self.append(&format!("{addr} {}", l.local_addr()?))?;
            Ok(l)
        }

        fn point_resolver(&self) -> io::Result<()> {
            sbx_guest::transparent::write_resolv_conf(&self.dir)
        }
    }

    /// 볼륨 흉내: 디스크 옆 내용 디렉터리([`crate::vm::volume_content`])를 대상 경로에 링크로 둔다. 실제 게스트처럼
    /// 처음 쓰는 볼륨은 쓰기로만 붙고(내용 디렉터리를 만든다), 빈 볼륨이면 대상 경로의 지금 내용으로 채운다. 그 뒤
    /// 대상(디렉터리면 지우고, 앞 샌드박스가 둔 링크면 바꾼다)을 링크로 바꾼다. 읽기 전용은 강제하지 않는다.
    fn link_volume(
        disk: &crate::vm::FakeDisk,
        target: &Path,
        readonly: bool,
        copy: bool,
    ) -> io::Result<()> {
        if !disk.volume {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a volume disk", disk.path.display()),
            ));
        }
        if readonly && !disk.readonly {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a read-only volume must be attached read-only",
            ));
        }
        let content = crate::vm::volume_content(&disk.path);
        if !content.is_dir() {
            if readonly {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "the volume has never been used, so there is nothing to read; attach it writable once first",
                ));
            }
            std::fs::create_dir(&content)?;
        }
        let image = match std::fs::symlink_metadata(target) {
            Ok(m) if m.is_dir() => Some(target),
            Ok(m) if m.file_type().is_symlink() => {
                std::fs::remove_file(target)?;
                None
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("volume target {} is not a directory", target.display()),
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e),
        };
        if let Some(dir) = image {
            if copy && !readonly && sbx_guest::volume::is_empty(&content)? {
                sbx_guest::volume::copy_up(dir, &content)?;
            }
            std::fs::remove_dir_all(dir)?;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(&content, target)
    }

    /// 제어 소켓을 열고 게스트 처리를 돌린다. `shares`는 (태그, 호스트 경로)다.
    /// `dirs`(scratch 디스크 옆의 두 디렉터리)가 있으면 빌드 층을 첫째로 흉내 내고([`crate::layers`]),
    /// 루프백 DNS와 투명 TCP를 둘째로 흉내 낸다([`crate::netdir::net_dir`]).
    /// `layer_disks`는 받은 층 디스크(역할 `rootfs`, 받은 차례)다. 게스트가 부팅 때 센 층 수 노릇을 한다
    /// (`CheckLayers`).
    /// `clock`(scratch 디스크 옆의 파일)이 있으면 게스트 시계를 그 파일로 흉내 낸다([`crate::clock`]).
    /// `disks`는 받은 디스크 전부(받은 차례)다. 볼륨(`Volume`)은 [`link_volume`]으로 흉내 낸다.
    /// `procs`에 이 VM이 띄운 프로세스 그룹을 적는다. VM을 끝내면(전원 끄기 흉내) 끝났다고 알리기 **전에** 그 그룹들을
    /// 모두 SIGKILL로 끝낸다. 그래서 실행기 프로세스가 곧바로 끝나도 호스트에 고아가 남지 않는다.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn start(
        socket: &Path,
        shares: Vec<(String, PathBuf)>,
        dirs: Option<(PathBuf, PathBuf)>,
        layer_disks: Vec<PathBuf>,
        clock: Option<PathBuf>,
        disks: Vec<crate::vm::FakeDisk>,
        procs: Arc<crate::vm::Procs>,
    ) -> Result<FakeVm, ExecutorError> {
        let (build_root, net) = dirs.unzip();
        let path: PathBuf = socket.to_owned();
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        let life = Arc::new(Life::default());
        // 열린 연결. VM이 끝나면 모두 닫는다(실제 VM에서 전원이 꺼지면 연결이 사라지는 것과 같다).
        let open: Arc<std::sync::Mutex<Vec<UnixStream>>> = Arc::default();

        let stop: Arc<dyn Fn(&str) + Send + Sync> = {
            let (life, path, open) = (life.clone(), path.clone(), open.clone());
            let procs = procs.clone();
            Arc::new(move |reason: &str| {
                // 전원 끄기: 이 VM의 프로세스를 먼저 모두 끝낸다. 끝났다는 알림(`life`)을 본 실행기 프로세스가 곧바로
                // 끝나도 고아가 남지 않게, 알리기 전에 한다. 여러 번 불려도 된다.
                procs.kill_all();
                if life.end(reason) {
                    // 기다리는 accept를 깨운다.
                    let _ = UnixStream::connect(&path);
                    let _ = std::fs::remove_file(&path);
                    for c in open.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
                        let _ = c.shutdown(std::net::Shutdown::Both);
                    }
                }
            })
        };
        let egress = Arc::new(match &net {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                let _ = std::fs::remove_file(dir.join(crate::netdir::NET_ROUTES));
                sbx_guest::egress::Egress::with_net(Box::new(FakeNet { dir: dir.clone() }))
            }
            None => sbx_guest::egress::Egress::default(),
        });
        if let Some(dir) = &net {
            // 게스트의 루프백 DNS(53번 대신 빈 포트). 포트는 파일에 적는다.
            let sock = std::net::UdpSocket::bind("127.0.0.1:0")?;
            std::fs::write(
                dir.join(crate::netdir::NET_DNS),
                sock.local_addr()?.port().to_string(),
            )?;
            let resolver = egress.resolver();
            thread::spawn(move || sbx_guest::dns::serve(&sock, resolver));
        }
        let opened: Arc<std::sync::atomic::AtomicBool> = Arc::default();
        let rebase_root = build_root.clone();
        let ctx = Arc::new(Ctx {
            root: "/".into(),
            on_shutdown: {
                let stop = stop.clone();
                Box::new(move || stop(REASON_GUEST_SHUTDOWN))
            },
            share: Some(Box::new(move |tag: &str, target: &str, _readonly: bool| {
                let host = shares
                    .iter()
                    .find(|(t, _)| t == tag)
                    .map(|(_, h)| h.clone())
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, format!("no share tagged {tag}"))
                    })?;
                link_share(&host, Path::new(target))
            })),
            egress,
            main: Default::default(),
            layers: build_root.map(|r| {
                Box::new(crate::layers::DirLayers::new(
                    r,
                    layer_disks.clone(),
                    opened.clone(),
                )) as Box<dyn sbx_guest::layer::Layers>
            }),
            // 호스트에서 돈다: 단계의 프로세스 그룹만 끝낸다.
            sweep: sbx_guest::exec::Sweep::Group,
            procs: Some(procs),
            layer_disks: u32::try_from(layer_disks.len()).ok(),
            clock: clock.map(|p| {
                Box::new(crate::clock::FileClock::new(p)) as Box<dyn sbx_guest::clock::Clock>
            }),
            volume: Some(Box::new(
                move |disk: u32, target: &str, readonly: bool, copy: bool| {
                    let d = disks.get(disk as usize).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotFound, format!("no disk {disk}"))
                    })?;
                    link_volume(d, Path::new(target), readonly, copy)
                },
            )),
        });
        {
            let life = life.clone();
            thread::spawn(move || {
                for conn in listener.incoming() {
                    if life.reason().is_some() {
                        break;
                    }
                    let Ok(mut conn) = conn else { continue };
                    let ctx = ctx.clone();
                    let open = open.clone();
                    let opened = opened.clone();
                    let rebase_root = rebase_root.clone();
                    thread::spawn(move || {
                        match vsock::accept(&mut conn) {
                            Ok(port) if port == sbx_guest_proto::GUEST_PORT => {}
                            // 그 포트를 듣는 게스트가 없다: 답 없이 닫는다.
                            _ => return,
                        }
                        // 받아들인 연결만 VM 종료 때 닫을 목록에 넣는다.
                        if let Ok(c) = conn.try_clone() {
                            open.lock().unwrap_or_else(|e| e.into_inner()).push(c);
                        }
                        if vsock::ack(&mut conn, port_ack()).is_err() {
                            return;
                        }
                        match rebase_root {
                            Some(root) => serve_rebased(conn, &ctx, root, opened),
                            None => server::serve(conn, &ctx),
                        }
                    });
                }
            });
        }
        Ok(FakeVm { life, stop })
    }

    /// 연결 하나를 처리한다. 빌드 층이 열린 뒤에는 호스트가 보낸 메시지의 경로를 뿌리 아래로 옮긴다
    /// ([`crate::rootdir::Rebased`]). 답은 그대로 같은 연결에 쓴다.
    fn serve_rebased(
        conn: UnixStream,
        ctx: &Ctx,
        root: PathBuf,
        opened: Arc<std::sync::atomic::AtomicBool>,
    ) {
        server::serve(crate::rootdir::Rebased::new(conn, root, opened), ctx);
    }

    /// `OK` 뒤의 숫자. 규약은 아무 숫자나 허용한다. 호스트 쪽 임시 포트를 흉내 낸다.
    fn port_ack() -> u32 {
        1_073_741_824
    }
}

#[cfg(not(unix))]
mod other {
    use std::io;

    use sbx_executor::{ChannelSpec, GuestConn};

    /// 띄우기는 `FakeExecutor::launch`가 Unix가 아니면 곧바로 거부한다. 붙기도 할 수 없다.
    pub(crate) fn connect(_channel: &ChannelSpec) -> io::Result<Box<dyn GuestConn>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the fake guest needs Unix sockets",
        ))
    }
}
