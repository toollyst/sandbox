//! `sbx` 실행 파일 끝-끝 테스트. 가짜 실행기로 VM 없이 CLI → 실행 코어 → 실행기 규약 → 게스트 처리를 모두 지난다.
//!
//! 이 테스트 실행 파일이 곧 가짜 실행기이자 shim이다: `sbx --executor <이 파일>`이 `<이 파일> capabilities|run`으로
//! 부르면 `sbx_executor_fake::process::main`을, `sbx --shim <이 파일>`이 `<이 파일> start ...`로 부르면
//! `sbx_shim::main`을 돈다. 그래서 다른 패키지의 실행 파일을 빌드할 필요가 없다(`harness = false`).
//!
//! 가짜 실행기의 게스트 처리는 호스트에서 지금 사용자로 돈다. 그래서 이미지의 사용자는 지금 사용자이고,
//! 게스트 경로는 호스트 경로다. 실제 VM 검증은 실행기(fc-fork) 쪽 E2E가 맡는다.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if matches!(
        args.get(1).map(String::as_str),
        Some("capabilities" | "run")
    ) {
        #[cfg(unix)]
        return sbx_executor_fake::process::main();
    }
    if args.get(1).map(String::as_str) == Some(sbx_shim::config::START) {
        return sbx_shim::main();
    }
    #[cfg(unix)]
    {
        unix::run_all(&args[1..])
    }
    #[cfg(not(unix))]
    {
        println!("e2e: skipped (the fake executor needs Unix sockets)");
        ExitCode::SUCCESS
    }
}

#[cfg(unix)]
mod unix {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, ExitCode, Output, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use sbx_model::{Arch, Digest, ImageConfig, ImageId, RootfsFormat};
    use sbx_store::{CacheLayout, ImageRecord, write};

    type Test = (&'static str, fn());

    const TESTS: &[Test] = &[
        (
            "run_streams_output_and_returns_the_exit_code",
            run_streams_output_and_returns_the_exit_code,
        ),
        (
            "run_command_not_found_is_127_and_not_executable_is_126",
            run_command_not_found_is_127_and_not_executable_is_126,
        ),
        ("run_i_forwards_stdin", run_i_forwards_stdin),
        (
            "run_t_gives_the_main_process_a_terminal",
            run_t_gives_the_main_process_a_terminal,
        ),
        ("create_start_inspect_rm", create_start_inspect_rm),
        ("names_work_in_place_of_ids", names_work_in_place_of_ids),
        (
            "completion_offers_sandboxes_and_images",
            completion_offers_sandboxes_and_images,
        ),
        ("json_output_contract", json_output_contract),
        ("events_replay_and_follow", events_replay_and_follow),
        ("filters", filters),
        ("jq_shapes_json_output", jq_shapes_json_output),
        (
            "exec_cp_stop_on_a_running_sandbox",
            exec_cp_stop_on_a_running_sandbox,
        ),
        (
            "signals_are_forwarded_to_the_main_process",
            signals_are_forwarded_to_the_main_process,
        ),
        (
            "commands_that_do_not_launch_need_no_executor",
            commands_that_do_not_launch_need_no_executor,
        ),
        (
            "run_failures_before_the_command_are_125",
            run_failures_before_the_command_are_125,
        ),
        (
            "detached_run_outlives_sbx_and_keeps_logs",
            detached_run_outlives_sbx_and_keeps_logs,
        ),
        ("detached_rm_and_logs_follow", detached_rm_and_logs_follow),
        (
            "logs_timestamps_since_until_and_json",
            logs_timestamps_since_until_and_json,
        ),
        (
            "stop_sends_the_stop_signal_then_powers_off",
            stop_sends_the_stop_signal_then_powers_off,
        ),
        ("kill_wait_and_restart", kill_wait_and_restart),
        ("attach_to_a_detached_sandbox", attach_to_a_detached_sandbox),
        (
            "installed_executor_is_used_by_default",
            installed_executor_is_used_by_default,
        ),
        (
            "executor_is_installed_from_a_repository",
            executor_is_installed_from_a_repository,
        ),
        (
            "image_kernel_is_given_to_the_executor",
            image_kernel_is_given_to_the_executor,
        ),
        (
            "mcp_serves_sandboxes_to_agents",
            mcp_serves_sandboxes_to_agents,
        ),
        ("attach_tty_and_signal_proxy", attach_tty_and_signal_proxy),
        ("volume_is_shared_live", volume_is_shared_live),
        ("network_allow_list", network_allow_list),
        (
            "embedded_guest_init_is_unpacked_into_the_cache",
            embedded_guest_init_is_unpacked_into_the_cache,
        ),
    ];

    /// 인자: 이름 일부(걸러 내기). `--`로 시작하는 인자(cargo test가 넘기는 것)는 무시한다.
    pub fn run_all(args: &[String]) -> ExitCode {
        let filters: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
        if args.iter().any(|a| a == "--list") {
            for (name, _) in TESTS {
                println!("{name}: test");
            }
            return ExitCode::SUCCESS;
        }
        let selected: Vec<&Test> = TESTS
            .iter()
            .filter(|(n, _)| filters.is_empty() || filters.iter().any(|f| n.contains(f.as_str())))
            .collect();
        println!("\nrunning {} tests", selected.len());
        let mut failed = Vec::new();
        for (name, f) in &selected {
            let ok = std::panic::catch_unwind(f).is_ok();
            println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
            if !ok {
                failed.push(*name);
            }
        }
        let passed = selected.len() - failed.len();
        if failed.is_empty() {
            println!("\ntest result: ok. {passed} passed; 0 failed\n");
            ExitCode::SUCCESS
        } else {
            println!(
                "\ntest result: FAILED. {passed} passed; {} failed: {}\n",
                failed.len(),
                failed.join(", ")
            );
            ExitCode::FAILURE
        }
    }

    const IMAGE: &str = "e2e-fake";

    fn id_output(flag: &str) -> String {
        let o = Command::new("id").arg(flag).output().unwrap();
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    }

    /// 테스트 하나의 환경: 짧은 임시 디렉터리(Unix 소켓 길이 제한) 안에 data, cache, socket, 게스트 init.
    struct Env {
        root: PathBuf,
    }

    impl Env {
        fn new() -> Self {
            static N: AtomicUsize = AtomicUsize::new(0);
            let root = PathBuf::from(format!(
                "/tmp/sbx-e2e-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("initramfs"), b"guest").unwrap();
            let env = Self { root };
            env.plant_image();
            env
        }

        /// 관리 쪽이 해 두었을 일: 이미지 기록, 참조, rootfs. 사용자는 지금 사용자다.
        fn plant_image(&self) {
            let layout = CacheLayout::new(self.root.join("cache"));
            let id = ImageId::new(Digest::parse(&format!("sha256:{}", "e2".repeat(32))).unwrap());
            let record = ImageRecord {
                id: id.clone(),
                reference: format!("docker.io/library/{IMAGE}:latest"),
                source: "test".into(),
                arch: Arch::host().unwrap(),
                config: ImageConfig {
                    cmd: vec!["sh".into()],
                    user: Some(format!("{}:{}", id_output("-u"), id_output("-g"))),
                    ..ImageConfig::default()
                },
                layers: vec![],
                ignored_fields: vec![],
            };
            write::write_image_record(&layout, &record).unwrap();
            write::set_ref(&layout, &record.reference, &id).unwrap();
            std::fs::write(layout.rootfs(&id, RootfsFormat::Erofs), b"fs").unwrap();
        }

        fn path(&self, name: &str) -> PathBuf {
            self.root.join(name)
        }

        fn cmd(&self, args: &[&str]) -> Command {
            let mut c = Command::new(env!("CARGO_BIN_EXE_sbx"));
            c.args(args)
                .env("SBX_DATA_DIR", self.root.join("data"))
                .env("SBX_CACHE_DIR", self.root.join("cache"))
                .env("SBX_SOCKET_DIR", self.root.join("s"))
                .env("SBX_GUEST_INIT", self.root.join("initramfs"))
                .env("SBX_EXECUTOR", std::env::current_exe().unwrap())
                .env("SBX_SHIM", std::env::current_exe().unwrap())
                .stdin(Stdio::null());
            c
        }

        fn sbx(&self, args: &[&str]) -> Output {
            self.cmd(args).output().unwrap()
        }

        fn sbx_with_stdin(&self, args: &[&str], input: &[u8]) -> Output {
            let mut child = self
                .cmd(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(input).unwrap();
            child.wait_with_output().unwrap()
        }

        fn spawn(&self, args: &[&str]) -> Child {
            self.cmd(args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        }

        /// `ps -a --format json`. 모양이 공개 규약([`sbx_cli::output::Sandbox`])과 정확히 같은지도 본다.
        fn ps(&self) -> Vec<serde_json::Value> {
            let o = self.sbx(&["ps", "-a", "--format", "json"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            let _: Vec<sbx_cli::output::Sandbox> = serde_json::from_slice(&o.stdout)
                .unwrap_or_else(|e| panic!("ps --format json: {e}: {}", text(&o.stdout)));
            serde_json::from_slice(&o.stdout).unwrap()
        }

        /// 실행 중인 샌드박스가 하나 생길 때까지 기다려 그 ID를 돌려준다.
        fn wait_running(&self) -> String {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(s) = self.ps().into_iter().find(|s| s["state"] == "running") {
                    return s["id"].as_str().unwrap().to_owned();
                }
                assert!(Instant::now() < deadline, "no running sandbox");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn text(b: &[u8]) -> String {
        String::from_utf8_lossy(b).into_owned()
    }

    fn wait_with_timeout(mut child: Child, timeout: Duration) -> Output {
        let deadline = Instant::now() + timeout;
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("sbx did not finish in {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }

    fn run_streams_output_and_returns_the_exit_code() {
        let e = Env::new();
        let o = e.sbx(&[
            "run",
            "--rm",
            IMAGE,
            "sh",
            "-c",
            "echo out; echo err >&2; exit 7",
        ]);
        assert_eq!(o.status.code(), Some(7), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "out\n");
        assert!(text(&o.stderr).contains("err\n"), "{}", text(&o.stderr));
        // --rm: 남지 않는다.
        assert!(e.ps().is_empty());
    }

    fn run_command_not_found_is_127_and_not_executable_is_126() {
        let e = Env::new();
        let o = e.sbx(&["run", "--rm", IMAGE, "no-such-command-xyz"]);
        assert_eq!(o.status.code(), Some(127), "{}", text(&o.stderr));
        let plain = e.path("plain");
        std::fs::write(&plain, b"not a program").unwrap();
        let o = e.sbx(&["run", "--rm", IMAGE, plain.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(126), "{}", text(&o.stderr));
        // 시작에 실패해도 --rm이면 지운다.
        assert!(e.ps().is_empty());
    }

    fn run_t_gives_the_main_process_a_terminal() {
        let e = Env::new();
        let o = e.sbx(&[
            "run",
            "--rm",
            "-t",
            IMAGE,
            "sh",
            "-c",
            "test -t 0 && test -t 1 && echo tty; exit 6",
        ]);
        assert_eq!(o.status.code(), Some(6), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "tty\r\n");
    }

    /// 게스트 init을 내장해 빌드했을 때만 돈다(`SBX_EMBED_GUEST`, CI의 Linux 작업).
    fn embedded_guest_init_is_unpacked_into_the_cache() {
        let Some(bytes) = sbx_cli::guest::embedded(Arch::host().unwrap()) else {
            println!("  (skipped: this build has no embedded guest init)");
            return;
        };
        let e = Env::new();
        let o = e
            .cmd(&["run", "--rm", IMAGE, "true"])
            .env_remove("SBX_GUEST_INIT")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let guest = e.path("cache").join("guest");
        let found: Vec<PathBuf> = std::fs::read_dir(&guest)
            .unwrap()
            .flatten()
            .map(|d| {
                d.path()
                    .join(Arch::host().unwrap().oci_name())
                    .join("initramfs")
            })
            .filter(|p| p.is_file())
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(std::fs::read(&found[0]).unwrap(), bytes);
        assert!(found[0].to_string_lossy().contains("sha256-"));
    }

    fn network_allow_list() {
        use std::io::{BufRead, BufReader};
        // 목적지: 아무 요청에나 "hi"로 답하는 HTTP 서버.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in server.incoming().flatten() {
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let mut c = c;
                let _ = c.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi",
                );
            }
        });
        let e = Env::new();
        let allow = format!("127.0.0.1:{port}");
        // fake 게스트는 호스트에서 돌므로 직접 연결도 된다. 프록시를 거치게 -x와 --noproxy ''로 강제한다.
        let script = format!(
            "curl -s -x \"$HTTP_PROXY\" --noproxy '' http://127.0.0.1:{port}/; echo; \
             curl -s -p -x \"$HTTP_PROXY\" --noproxy '' http://127.0.0.1:{port}/; echo; \
             curl -s -o /dev/null -w '%{{http_code}}' -x \"$HTTP_PROXY\" --noproxy '' http://127.0.0.1:{}/",
            port.wrapping_add(1)
        );
        let o = e.sbx(&[
            "run",
            "--rm",
            "--network",
            "allow",
            "--allow",
            &allow,
            IMAGE,
            "sh",
            "-c",
            &script,
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "hi\nhi\n403");

        assert_eq!(
            e.sbx(&["run", "--rm", "--allow", &allow, IMAGE, "true"])
                .status
                .code(),
            Some(125)
        );
        assert_eq!(
            e.sbx(&["run", "--rm", "--network", "allow", IMAGE, "true"])
                .status
                .code(),
            Some(125)
        );
        // 차단(기본)이면 프록시 환경 변수가 없다.
        let o = e.sbx(&[
            "run",
            "--rm",
            IMAGE,
            "sh",
            "-c",
            "printf %s \"$HTTP_PROXY\"",
        ]);
        assert_eq!(text(&o.stdout), "");
    }

    fn volume_is_shared_live() {
        let e = Env::new();
        let host = e.path("host");
        std::fs::create_dir(&host).unwrap();
        std::fs::write(host.join("in.txt"), b"hello").unwrap();
        // 가짜 실행기의 게스트 경로는 호스트 경로다.
        let guest = e.path("guest-data");
        let o = e.sbx(&[
            "run",
            "--rm",
            "-v",
            &format!("{}:{}", host.display(), guest.display()),
            IMAGE,
            "sh",
            "-c",
            &format!(
                "cat {g}/in.txt; echo back > {g}/out.txt",
                g = guest.display()
            ),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "hello");
        assert_eq!(std::fs::read(host.join("out.txt")).unwrap(), b"back\n");
    }

    fn run_i_forwards_stdin() {
        let e = Env::new();
        let o = e.sbx_with_stdin(&["run", "--rm", "-i", IMAGE, "cat"], b"hello\nworld\n");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "hello\nworld\n");
        // -i가 없으면 표준 입력은 처음부터 닫혀 있다.
        let o = e.sbx_with_stdin(&["run", "--rm", IMAGE, "cat"], b"ignored");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "");
    }

    fn create_start_inspect_rm() {
        let e = Env::new();
        let o = e.sbx(&["create", IMAGE, "sh", "-c", "echo started; exit 3"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        assert_eq!(e.ps()[0]["state"], "created");

        // start는 붙어서 기본 프로세스가 끝날 때까지 기다린다.
        let o = e.sbx(&["start", "-a", &id]);
        assert_eq!(o.status.code(), Some(3), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "started\n");

        let o = e.sbx(&["inspect", &id]);
        let info: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(info["state"], "stopped");
        assert_eq!(info["exit_code"], 3);
        let o = e.sbx(&["ps", "-a"]);
        assert!(
            text(&o.stdout).contains("stopped (3)"),
            "{}",
            text(&o.stdout)
        );

        // 다시 시작할 수 있다(Docker와 같다).
        assert_eq!(e.sbx(&["start", "-a", &id]).status.code(), Some(3));

        let o = e.sbx(&["rm", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), id);
        assert!(e.ps().is_empty());

        // create --rm은 받지 않는다.
        assert_eq!(e.sbx(&["create", "--rm", IMAGE]).status.code(), Some(1));
    }

    /// `--format json`: 명령마다 `sbx_cli::output::JSON_OUTPUT`의 모양, 오류는 stderr에 JSON 한 줄.
    fn json_output_contract() {
        use sbx_cli::output::{Done, ErrorLine, Exited, Image, Removed, Sandbox, SandboxRef};
        let e = Env::new();
        let json = |args: &[&str]| {
            let mut all = vec!["--format", "json"];
            all.extend_from_slice(args);
            e.sbx(&all)
        };
        let lines = |b: &[u8]| -> Vec<serde_json::Value> {
            text(b)
                .lines()
                .map(|l| serde_json::from_str(l).unwrap_or_else(|err| panic!("{err}: {l:?}")))
                .collect()
        };

        // create: SandboxRef 하나.
        let o = json(&["create", "--name", "j1", IMAGE, "sh", "-c", "exit 4"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let r: SandboxRef = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(r.name.as_deref(), Some("j1"));
        assert!(r.id.len() > 12, "{r:?}");

        // inspect: Sandbox 하나. 저장 형식(spec)이 아니라 공개 모양이다.
        let o = json(&["inspect", "j1"]);
        let s: Sandbox = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(s.id, r.id);
        assert_eq!(s.process.args, ["sh", "-c", "exit 4"]);
        assert_eq!(
            s.image.reference.as_deref(),
            Some(&*format!("docker.io/library/{IMAGE}:latest"))
        );

        // 붙어서 실행하는 명령은 받지 않는다. 아무것도 하지 않고 JSON 오류로 끝난다.
        let o = json(&["start", "-a", "j1"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(o.stdout.is_empty());
        let err: ErrorLine = serde_json::from_str(text(&o.stderr).trim()).unwrap();
        assert!(err.error.message.contains("`start`"), "{err:?}");
        assert_eq!(err.error.exit_code, Some(1));
        assert_eq!(e.ps()[0]["state"], "created", "start -a must not have run");
        for args in [&["exec", "j1", "true"][..], &["run", IMAGE, "true"]] {
            let o = json(args);
            assert_eq!(o.status.code(), Some(1), "{args:?}");
            assert!(o.stdout.is_empty(), "{args:?}");
        }

        // start(붙지 않음) → SandboxRef, wait → 대상마다 Exited 한 줄. 없는 대상은 stderr에 그 대상의 오류 줄.
        let o = json(&["start", "j1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let _: SandboxRef = serde_json::from_slice(&o.stdout).unwrap();
        let o = json(&["wait", "j1", "no-such-sandbox"]);
        assert_eq!(o.status.code(), Some(1));
        let out = lines(&o.stdout);
        assert_eq!(out.len(), 1, "{out:?}");
        let x: Exited = serde_json::from_value(out[0].clone()).unwrap();
        assert_eq!(
            (x.target.as_str(), x.id.as_str(), x.exit_code),
            ("j1", &*r.id, 4)
        );
        let errs: Vec<ErrorLine> = lines(&o.stderr)
            .into_iter()
            .map(|v| serde_json::from_value(v).unwrap())
            .collect();
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert_eq!(errs[0].error.target.as_deref(), Some("no-such-sandbox"));
        assert_eq!(errs[0].error.exit_code, None);
        assert_eq!(errs[1].error.target, None);
        assert_eq!(errs[1].error.exit_code, Some(1));

        // rm → Done 한 줄.
        let o = json(&["rm", "j1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let d: Done = serde_json::from_value(lines(&o.stdout).remove(0)).unwrap();
        assert_eq!((d.target.as_str(), d.id.as_str()), ("j1", &*r.id));

        // images → [Image], rmi → Removed 한 줄. -q가 --format을 이긴다.
        let o = json(&["images"]);
        let images: Vec<Image> = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(images.len(), 1);
        let o = json(&["images", "-q"]);
        assert_eq!(
            text(&o.stdout),
            format!("{}\n", &images[0].id["sha256:".len()..][..12])
        );
        let o = json(&["rmi", IMAGE]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let rm: Removed = serde_json::from_value(lines(&o.stdout).remove(0)).unwrap();
        assert_eq!(rm.target, IMAGE);
        assert_eq!(rm.deleted.first(), Some(&images[0].id));
    }

    /// `events`: 다른 프로세스들(CLI, shim)이 남긴 샌드박스·이미지 이벤트를 시각 순으로 다시 읽고, 새것을 따라간다.
    fn events_replay_and_follow() {
        use sbx_cli::output::Event;
        use std::io::BufRead;
        let e = Env::new();
        let ok = |args: &[&str]| {
            let o = e.sbx(args);
            assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o.stderr));
            text(&o.stdout).trim().to_owned()
        };
        // 따라가는 쪽을 먼저 띄운다(--since 없음: 새것만).
        let mut follower = e.spawn(&["events", "--format", "json"]);
        let (tx, rx) = std::sync::mpsc::channel();
        let stdout = follower.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        std::thread::sleep(Duration::from_millis(300));

        let id = ok(&["create", "--name", "ev", IMAGE, "sh", "-c", "exit 2"]);
        let o = e.sbx(&["start", "-a", "ev"]);
        assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
        let detached = ok(&["run", "-d", IMAGE, "sleep", "30"]);
        ok(&["rm", "-f", &detached]);
        ok(&["rm", "ev"]);
        ok(&["rmi", IMAGE]);

        let want_sandbox = ["create", "start", "stop", "remove"];
        // 따라가는 쪽: 처음 것이 곧 온다.
        let first: Event =
            serde_json::from_str(&rx.recv_timeout(Duration::from_secs(10)).unwrap()).unwrap();
        assert_eq!(
            (
                first.kind.as_str(),
                first.action.as_str(),
                first.id.as_str()
            ),
            ("sandbox", "create", &*id)
        );
        let _ = follower.kill();
        let _ = follower.wait();

        // 다시 읽기: 지난 1시간에서 지금까지. 끝나면 스스로 끝난다.
        let o = e.sbx(&[
            "events", "--since", "1h", "--until", "0s", "--format", "json",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let all: Vec<Event> = text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|err| panic!("{err}: {l}")))
            .collect();
        assert!(all.windows(2).all(|w| w[0].time <= w[1].time), "{all:?}");
        let of = |sid: &str| -> Vec<&str> {
            all.iter()
                .filter(|e| e.id == sid)
                .map(|e| e.action.as_str())
                .collect()
        };
        assert_eq!(of(&id), want_sandbox);
        // shim이 소유한 샌드박스도 같은 기록에 남는다.
        assert_eq!(of(&detached), want_sandbox);
        let stop = all
            .iter()
            .find(|e| e.id == id && e.action == "stop")
            .unwrap();
        assert_eq!(stop.attributes["exit_code"], "2");
        assert_eq!(stop.attributes["name"], "ev");
        let image: Vec<&str> = all
            .iter()
            .filter(|e| e.kind == "image")
            .map(|e| e.action.as_str())
            .collect();
        assert_eq!(image, ["untag", "delete"]);

        // 사람이 읽는 줄(Docker와 같은 모양).
        let o = e.sbx(&["events", "--since", "1h", "--until", "0s"]);
        let first = text(&o.stdout).lines().next().unwrap().to_owned();
        assert!(
            first.contains(&format!(" sandbox create {id} (image=")) && first.contains("name=ev)"),
            "{first}"
        );
        // 지나간 --until만 주면 아무것도 기다리지 않고 끝난다. 잘못된 시각은 오류다.
        let o = e.sbx(&["events", "--until", "0s"]);
        assert_eq!((o.status.code(), text(&o.stdout)), (Some(0), String::new()));
        let o = e.sbx(&["events", "--since", "yesterday"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(text(&o.stderr).contains("--since"), "{}", text(&o.stderr));
    }

    /// `--filter` (`ps`, `images`, `events`): 다른 키는 AND, 같은 키는 OR, 모르는 키는 오류.
    fn filters() {
        let e = Env::new();
        let ok = |args: &[&str]| {
            let o = e.sbx(args);
            assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o.stderr));
            text(&o.stdout)
        };
        let ids = |args: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = ok(args).lines().map(str::to_owned).collect();
            v.sort();
            v
        };
        let short = |id: &str| id[..12].to_owned();
        let stopped = ok(&["create", "-l", "owner=mcp", IMAGE, "sh", "-c", "exit 3"])
            .trim()
            .to_owned();
        assert_eq!(e.sbx(&["start", "-a", &stopped]).status.code(), Some(3));
        let running = ok(&["run", "-d", "--name", "web1", IMAGE, "sleep", "30"])
            .trim()
            .to_owned();
        let created = ok(&["create", IMAGE, "true"]).trim().to_owned();
        let sorted = |mut v: Vec<String>| {
            v.sort();
            v
        };

        // status, exited는 -a 없이도 멈춘 것을 본다.
        assert_eq!(
            ids(&["ps", "-q", "--filter", "status=stopped"]),
            [short(&stopped)]
        );
        assert_eq!(ids(&["ps", "-q", "-f", "exited=3"]), [short(&stopped)]);
        assert_eq!(ids(&["ps", "-q", "-f", "exited=0"]), Vec::<String>::new());
        assert_eq!(
            ids(&["ps", "-aq", "-f", "label=owner=mcp"]),
            [short(&stopped)]
        );
        assert_eq!(ids(&["ps", "-aq", "-f", "label=owner"]), [short(&stopped)]);
        assert_eq!(ids(&["ps", "-aq", "-f", "name=web"]), [short(&running)]);
        // 같은 키는 OR.
        assert_eq!(
            ids(&["ps", "-q", "-f", "status=running", "-f", "status=created"]),
            sorted(vec![short(&running), short(&created)])
        );
        // 다른 키는 AND.
        assert_eq!(
            ids(&["ps", "-aq", "-f", "status=running", "-f", "label=owner"]),
            Vec::<String>::new()
        );
        assert_eq!(
            ids(&["ps", "-aq", "-f", &format!("id={}", &created[..6])]),
            [short(&created)]
        );
        assert_eq!(ids(&["ps", "-aq", "-f", "ancestor=e2e-fake"]).len(), 3);
        assert_eq!(
            ids(&["ps", "-aq", "-f", "ancestor=alpine"]),
            Vec::<String>::new()
        );
        let o = e.sbx(&["ps", "-f", "color=red"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("unknown key \"color\""),
            "{}",
            text(&o.stderr)
        );

        // images
        assert_eq!(
            ok(&["images", "-q", "-f", "reference=e2e*"])
                .lines()
                .count(),
            1
        );
        assert_eq!(
            ok(&[
                "images",
                "-q",
                "-f",
                &format!("reference=docker.io/library/{IMAGE}:*")
            ])
            .lines()
            .count(),
            1
        );
        assert_eq!(ok(&["images", "-q", "-f", "reference=alpine"]), "");
        assert_eq!(
            ok(&["images", "-q", "-f", "dangling=false"])
                .lines()
                .count(),
            1
        );
        assert_eq!(ok(&["images", "-q", "-f", "dangling=true"]), "");

        // events
        let o = ok(&[
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "-f",
            "sandbox=web1",
            "-f",
            "event=start",
        ]);
        assert_eq!(o.lines().count(), 1, "{o}");
        assert!(o.contains(&format!("sandbox start {running}")), "{o}");
        let o = ok(&[
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "-f",
            "image=e2e-fake",
            "-f",
            "event=create",
        ]);
        assert_eq!(o.lines().count(), 3, "{o}");
        assert_eq!(
            ok(&[
                "events",
                "--since",
                "1h",
                "--until",
                "0s",
                "-f",
                "type=image"
            ]),
            ""
        );

        ok(&["rm", "-f", &stopped, &running, &created]);
    }

    /// `--jq`(gh와 같다): JSON 출력 값(또는 줄)마다 식을 돌린다. 문자열은 따옴표 없이. 틀린 식이면 아무것도 하지 않는다.
    fn jq_shapes_json_output() {
        let e = Env::new();
        let jq = |args: &[&str]| {
            let mut all = vec!["--format", "json"];
            all.extend_from_slice(args);
            e.sbx(&all)
        };
        // 틀린 식: 만들지 않는다.
        let o = jq(&["--jq", ".[", "create", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("at character"),
            "{}",
            text(&o.stderr)
        );
        assert!(e.ps().is_empty());
        // --format json 없이는 받지 않는다.
        let o = e.sbx(&["--jq", ".", "ps"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(text(&o.stderr).contains("--jq needs --format json"));

        // 값 하나(create의 SandboxRef) → 문자열은 그대로.
        let o = jq(&["--jq", ".name", "create", "--name", "jq1", IMAGE, "true"]);
        assert_eq!(text(&o.stdout), "jq1\n", "{}", text(&o.stderr));
        let o = jq(&["create", "--name", "jq2", IMAGE, "sleep", "1"]);
        assert_eq!(o.status.code(), Some(0));
        let o = jq(&[
            "--jq",
            r#"map(select(.name | startswith("jq"))) | sort_by(.name) | .[] | "\(.name) \(.state)""#,
            "ps",
            "-a",
        ]);
        assert_eq!(text(&o.stdout), "jq1 created\njq2 created\n");
        let o = jq(&["--jq", "{n: length}", "ps", "-a"]);
        assert_eq!(text(&o.stdout), "{\"n\":2}\n");
        // 대상마다 한 줄인 명령은 줄마다 돌린다.
        let o = jq(&["--jq", ".target", "rm", "jq1", "jq2"]);
        assert_eq!(text(&o.stdout), "jq1\njq2\n", "{}", text(&o.stderr));
        let o = jq(&[
            "--jq",
            "select(.type == \"sandbox\") | .action",
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
        ]);
        assert_eq!(text(&o.stdout), "create\ncreate\nremove\nremove\n");
        // logs(줄 조각마다 LogLine)에도 적용된다.
        let id = text(
            &jq(&[
                "--jq",
                ".id",
                "run",
                "-d",
                IMAGE,
                "sh",
                "-c",
                "echo from-log",
            ])
            .stdout,
        )
        .trim()
        .to_owned();
        assert_eq!(text(&jq(&["--jq", ".id", "wait", &id]).stdout).trim(), id);
        let o = jq(&[
            "--jq",
            "select(.stream == \"stdout\") | .data | rtrimstr(\"\\n\")",
            "logs",
            &id,
        ]);
        assert_eq!(text(&o.stdout), "from-log\n", "{}", text(&o.stderr));
        // 식이 실행 중에 실패하면 명령의 오류다.
        let o = jq(&["--jq", ".foo", "ps", "-a"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("cannot index"),
            "{}",
            text(&o.stderr)
        );
    }

    /// 셸 스크립트가 하는 것처럼 `COMPLETE=bash sbx -- <단어들>`로 부른다. 마지막 단어가 완성할 자리다.
    fn complete(e: &Env, words: &[&str]) -> Vec<String> {
        let mut args = vec!["--", "sbx"];
        args.extend(words);
        let o = e
            .cmd(&args)
            .env("COMPLETE", "bash")
            .env("_CLAP_COMPLETE_INDEX", words.len().to_string())
            .env("_CLAP_IFS", "\n")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{words:?}: {}", text(&o.stderr));
        text(&o.stdout).lines().map(str::to_owned).collect()
    }

    fn completion_offers_sandboxes_and_images() {
        let e = Env::new();
        let o = e.sbx(&["create", "--name", "web", IMAGE]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        let short = &id[..12];
        let image = format!("{IMAGE}:latest");
        let has = |words: &[&str], want: &str| {
            let got = complete(&e, words);
            assert!(got.iter().any(|c| c == want), "{words:?}: {got:?}");
            got
        };
        for cmd in [
            "start", "stop", "rm", "inspect", "logs", "attach", "kill", "wait", "restart",
        ] {
            has(&[cmd, ""], "web");
            has(&[cmd, ""], short);
        }
        // 친 앞부분으로 거른다. exec는 첫 인자만 샌드박스다.
        assert_eq!(complete(&e, &["exec", "w"]), ["web"]);
        assert!(!complete(&e, &["exec", "web", ""]).contains(&"web".to_owned()));
        assert_eq!(complete(&e, &["cp", "we"]), ["web:"]);
        // 이미지: 줄인 참조. run과 create는 첫 인자만.
        for cmd in ["run", "create", "rmi", "prepare", "pull"] {
            has(&[cmd, ""], &image);
        }
        assert_eq!(complete(&e, &["run", "--rm", "e2e"]), [image.as_str()]);
        assert!(!complete(&e, &["run", &image, ""]).contains(&image));
        // 줄에 친 --data-dir이 SBX_DATA_DIR보다 앞선다. 없는 디렉터리는 후보가 없고, 만들지도 않는다.
        let none = e.path("none");
        let none = none.to_str().unwrap();
        assert!(!complete(&e, &["--data-dir", none, "rm", ""]).contains(&"web".to_owned()));
        assert!(!complete(&e, &[&format!("--cache-dir={none}"), "run", ""]).contains(&image));
        assert!(!e.path("none").exists());
        // `sbx completion <셸>`은 `COMPLETE=<셸> sbx`와 같은 등록 스크립트다.
        for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
            let a = e.sbx(&["completion", shell]);
            let b = e.cmd(&[]).env("COMPLETE", shell).output().unwrap();
            assert_eq!(a.status.code(), Some(0), "{}", text(&a.stderr));
            assert_eq!(text(&a.stdout), text(&b.stdout), "{shell}");
            assert!(text(&a.stdout).contains("COMPLETE"), "{shell}");
        }
    }

    fn names_work_in_place_of_ids() {
        let e = Env::new();
        let o = e.sbx(&["create", "--name", "web", IMAGE, "sh", "-c", "echo hi"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        assert_eq!(e.ps()[0]["name"], "web");
        assert!(
            e.ps()[0]["created"].as_str().unwrap().ends_with('Z'),
            "{:?}",
            e.ps()
        );
        let o = e.sbx(&["ps", "-a"]);
        let header = text(&o.stdout).lines().next().unwrap().to_owned();
        assert!(header.trim_end().ends_with("NAMES"), "{header}");
        assert!(text(&o.stdout).contains(" ago "), "{}", text(&o.stdout));
        assert!(
            text(&o.stdout)
                .lines()
                .nth(1)
                .unwrap()
                .trim_end()
                .ends_with(" web")
        );
        // -q는 ID만(Docker처럼 앞 12자). 실행 중인 것이 없으면 -a 없이는 비어 있다.
        assert_eq!(
            text(&e.sbx(&["ps", "-aq"]).stdout),
            format!("{}\n", &id[..12])
        );
        assert_eq!(text(&e.sbx(&["ps", "-q"]).stdout), "");
        let o = e.sbx(&["images", "-q"]);
        assert_eq!(
            text(&o.stdout),
            format!("{}\n", "e2".repeat(6)),
            "{}",
            text(&o.stderr)
        );

        // 같은 이름은 거절된다. run이면 125다.
        let o = e.sbx(&["create", "--name", "web", IMAGE]);
        assert_eq!(o.status.code(), Some(1), "{}", text(&o.stderr));
        let o = e.sbx(&["run", "--name", "web", IMAGE]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("already in use"),
            "{}",
            text(&o.stderr)
        );
        assert_eq!(e.ps().len(), 1);
        assert_ne!(
            e.sbx(&["create", "--name", "a b", IMAGE]).status.code(),
            Some(0)
        );

        // 이름은 ID 대신 쓸 수 있다.
        let o = e.sbx(&["start", "-a", "web"]);
        assert_eq!(text(&o.stdout), "hi\n", "{}", text(&o.stderr));
        let o = e.sbx(&["inspect", "web"]);
        let info: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(info["id"], id.as_str());
        let o = e.sbx(&["rm", "web"]);
        assert_eq!(text(&o.stdout).trim(), "web", "{}", text(&o.stderr));

        // --env-file: 파일 다음에 -e가 덮는다. 파일을 읽을 수 없으면 run 실패(125).
        let f = e.path("env");
        std::fs::write(&f, "# c\nA=from-file\nB=b c\n").unwrap();
        let f = f.to_str().unwrap();
        let o = e.sbx(&[
            "run",
            "--rm",
            "--env-file",
            f,
            "-e",
            "A=flag",
            IMAGE,
            "sh",
            "-c",
            "echo \"$A/$B\"",
        ]);
        assert_eq!(text(&o.stdout), "flag/b c\n", "{}", text(&o.stderr));
        let o = e.sbx(&["run", "--rm", "--env-file", "/no/such", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125));
        assert!(
            text(&o.stderr).contains("--env-file /no/such"),
            "{}",
            text(&o.stderr)
        );

        // 지우면 이름을 다시 쓸 수 있다.
        assert_eq!(
            e.sbx(&["create", "--name", "web", IMAGE]).status.code(),
            Some(0)
        );
    }

    fn exec_cp_stop_on_a_running_sandbox() {
        let e = Env::new();
        let main = e.spawn(&[
            "run",
            "--name",
            "Box.1",
            IMAGE,
            "sh",
            "-c",
            "echo up; sleep 60",
        ]);
        let id = e.wait_running();

        let o = e.sbx(&["exec", &id, "sh", "-c", "echo in; echo e >&2; exit 4"]);
        assert_eq!(o.status.code(), Some(4), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "in\n");
        assert_eq!(text(&o.stderr), "e\n");
        let o = e.sbx_with_stdin(&["exec", "-i", &id, "cat"], b"piped");
        assert_eq!(text(&o.stdout), "piped");
        assert_eq!(
            e.sbx(&["exec", &id, "no-such-command-xyz"]).status.code(),
            Some(127)
        );
        // 표준 출력이 터미널이 아니면 게스트 기본 크기다. 터미널이 줄 끝을 \r\n으로 바꾼다.
        let o = e.sbx(&["exec", "-t", &id, "sh", "-c", "stty size; echo e >&2"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "24 80\r\ne\r\n");
        assert_eq!(e.sbx(&["exec", "-it", &id, "sh"]).status.code(), Some(1));

        // 가짜 실행기의 게스트 경로는 호스트 경로다.
        let src = e.path("src.txt");
        std::fs::write(&src, b"copied").unwrap();
        let guest_dir = e.path("guest");
        std::fs::create_dir(&guest_dir).unwrap();
        let o = e.sbx(&[
            "cp",
            src.to_str().unwrap(),
            &format!("{id}:{}/", guest_dir.display()),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(std::fs::read(guest_dir.join("src.txt")).unwrap(), b"copied");
        // 꺼낼 때는 이름으로(ID 규칙에 맞지 않는 이름도 된다).
        let back = e.path("back.txt");
        let o = e.sbx(&[
            "cp",
            &format!("Box.1:{}/src.txt", guest_dir.display()),
            back.to_str().unwrap(),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(std::fs::read(&back).unwrap(), b"copied");

        // 실행 중이면 -f 없이 지우지 않는다.
        assert_eq!(e.sbx(&["rm", &id]).status.code(), Some(1));

        let o = e.sbx(&["stop", "-t", "10", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let out = wait_with_timeout(main, Duration::from_secs(20));
        // stop은 기본 프로세스에 SIGTERM을 보낸다. 그것으로 끝나면 143.
        assert_eq!(out.status.code(), Some(143), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "up\n");
        assert_eq!(e.ps()[0]["exit_code"], 143);
        assert_eq!(e.sbx(&["rm", &id]).status.code(), Some(0));
    }

    fn signals_are_forwarded_to_the_main_process() {
        let e = Env::new();
        let main = e.spawn(&[
            "run",
            "--rm",
            IMAGE,
            "sh",
            "-c",
            "trap 'echo got; exit 42' TERM; echo ready; sleep 60 & wait",
        ]);
        e.wait_running();
        // 기본 프로세스가 trap을 걸 때까지 잠깐 둔다.
        std::thread::sleep(Duration::from_millis(300));
        let pid = main.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        let out = wait_with_timeout(main, Duration::from_secs(20));
        assert_eq!(out.status.code(), Some(42), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "ready\ngot\n");
    }

    fn commands_that_do_not_launch_need_no_executor() {
        let e = Env::new();
        let missing = Path::new("/nonexistent/sbx-executor");
        let ok = |args: &[&str]| {
            let o = e.cmd(args).env("SBX_EXECUTOR", missing).output().unwrap();
            assert_eq!(o.status.code(), Some(0), "{args:?}: {}", text(&o.stderr));
        };
        ok(&["ps", "-a"]);
        ok(&["images"]);
        ok(&["completion", "bash"]);
        // info는 실행기를 부를 수 없어도 나머지를 보여준다.
        let o = e
            .cmd(&["info", "--format", "json"])
            .env("SBX_EXECUTOR", missing)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["images"], 1);
        assert!(v["executor"]["error"].is_string(), "{v}");
        assert!(v["executor"]["capabilities"].is_null(), "{v}");
        let _: sbx_cli::output::Info = serde_json::from_value(v).unwrap();
        let o = e.sbx(&["info"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stdout).contains("Supports:    fake"),
            "{}",
            text(&o.stdout)
        );
        let o = e.sbx(&["completion", "zsh"]);
        assert!(
            text(&o.stdout).starts_with("#compdef sbx"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["create", IMAGE]);
        let id = text(&o.stdout).trim().to_owned();
        ok(&["inspect", &id]);
        ok(&["rm", &id]);
    }

    fn run_failures_before_the_command_are_125() {
        let e = Env::new();
        let o = e
            .cmd(&["run", "--rm", IMAGE, "true"])
            .env("SBX_EXECUTOR", "/nonexistent/sbx-executor")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        // -it인데 표준 입력이 터미널이 아니다.
        let o = e.sbx(&["run", "--rm", "-it", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125));
        assert!(text(&o.stderr).contains("not a TTY"), "{}", text(&o.stderr));
        // -dit는 터미널이 없어도 된다(붙는 쪽이 없으므로, Docker와 같다).
        let o = e.sbx(&["run", "-d", "-it", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        wait_state(&e, &id, "stopped");
        assert_eq!(e.sbx(&["rm", &id]).status.code(), Some(0));
        assert_eq!(
            e.sbx(&["run", "--pull", "never", "no-such-image", "true"])
                .status
                .code(),
            Some(125)
        );
        let o = e
            .cmd(&["run", "--rm", IMAGE, "true"])
            .env("SBX_FAKE_FAIL_LAUNCH", "boom")
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(125));
        assert!(text(&o.stderr).contains("boom"), "{}", text(&o.stderr));
        assert!(e.ps().is_empty());
    }

    /// `logs` 출력이 기대한 것이 될 때까지 기다린다(shim은 시작을 알린 뒤 출력을 적는다).
    fn wait_logs(e: &Env, id: &str, stdout: &str, stderr: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let o = e.sbx(&["logs", id]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            if text(&o.stdout) == stdout && text(&o.stderr) == stderr {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "logs: {:?} / {:?}",
                text(&o.stdout),
                text(&o.stderr)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn state_of(e: &Env, id: &str) -> Option<serde_json::Value> {
        e.ps().into_iter().find(|s| s["id"] == id)
    }

    fn wait_state(e: &Env, id: &str, state: &str) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(s) = state_of(e, id).filter(|s| s["state"] == state) {
                return s;
            }
            assert!(Instant::now() < deadline, "{id} is not {state}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn detached_run_outlives_sbx_and_keeps_logs() {
        let e = Env::new();
        let o = e.sbx(&[
            "run",
            "-d",
            IMAGE,
            "sh",
            "-c",
            "echo started; echo e >&2; sleep 60",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        // sbx는 끝났지만 샌드박스는 실행 중이다(shim이 소유한다).
        assert_eq!(state_of(&e, &id).unwrap()["state"], "running");
        wait_logs(&e, &id, "started\n", "e\n");
        let o = e.sbx(&["exec", &id, "echo", "inside"]);
        assert_eq!(text(&o.stdout), "inside\n");

        let o = e.sbx(&["stop", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(state_of(&e, &id).unwrap()["exit_code"], 143);
        // 멈춘 뒤에도 로그는 남는다.
        wait_logs(&e, &id, "started\n", "e\n");

        // start는 Docker처럼 붙지 않고 시작한다. 로그는 이어 쓴다.
        let o = e.sbx(&["start", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), id);
        assert_eq!(state_of(&e, &id).unwrap()["state"], "running");
        wait_logs(&e, &id, "started\nstarted\n", "e\ne\n");
        assert_eq!(e.sbx(&["rm", "-f", &id]).status.code(), Some(0));
        assert!(state_of(&e, &id).is_none());
    }

    fn detached_rm_and_logs_follow() {
        let e = Env::new();
        // -f는 샌드박스가 멈출 때까지 따라간다.
        let o = e.sbx(&["run", "-d", IMAGE, "sh", "-c", "echo a; sleep 1; echo b"]);
        let id = text(&o.stdout).trim().to_owned();
        let o = e.sbx(&["logs", "-f", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "a\nb\n");
        assert_eq!(wait_state(&e, &id, "stopped")["exit_code"], 0);
        // --tail N: 마지막 N줄만.
        let o = e.sbx(&["logs", "--tail", "1", &id]);
        assert_eq!(text(&o.stdout), "b\n", "{}", text(&o.stderr));
        assert_eq!(text(&e.sbx(&["logs", "-n", "0", &id]).stdout), "");
        assert_ne!(e.sbx(&["logs", "--tail", "x", &id]).status.code(), Some(0));

        // --rm: 끝나면 shim이 지운다.
        let o = e.sbx(&["run", "-d", "--rm", IMAGE, "sh", "-c", "exit 0"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        let deadline = Instant::now() + Duration::from_secs(20);
        while state_of(&e, &id).is_some() {
            assert!(Instant::now() < deadline, "{id} was not removed");
            std::thread::sleep(Duration::from_millis(50));
        }

        // 시작에 실패하면 그 이유로 125, --rm이면 남지 않는다.
        let o = e.sbx(&["run", "-d", "--rm", IMAGE, "no-such-command-xyz"]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("executable file not found"),
            "{}",
            text(&o.stderr)
        );
        let failed = |s: &serde_json::Value| s["process"]["args"][0] == "no-such-command-xyz";
        assert!(!e.ps().iter().any(failed), "{:?}", e.ps());
    }

    /// `logs -t`, `--since`, `--until`, `--format json`: shim이 기록마다 남긴 시각으로 고르고 붙인다.
    fn logs_timestamps_since_until_and_json() {
        use sbx_cli::output::LogLine;
        use std::io::BufRead;
        let e = Env::new();
        let script = r"echo one; sleep 1; echo two; sleep 0.2; echo err >&2; sleep 0.2; printf '\377\n'; printf partial";
        let o = e.sbx(&["run", "-d", IMAGE, "sh", "-c", script]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        assert_eq!(e.sbx(&["logs", "-f", &id]).status.code(), Some(0));
        wait_state(&e, &id, "stopped");

        // -t: 줄마다 `RFC 3339 나노초 + 공백` (Docker와 같은 모양). 흐름은 그대로 나뉜다.
        let o = e.sbx(&["logs", "--timestamps", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let split = |l: &str| -> (String, String) {
            let (t, rest) = l.split_once(' ').unwrap_or_else(|| panic!("{l:?}"));
            assert_eq!(t.len(), "2026-11-10T13:20:14.123456789Z".len(), "{l:?}");
            assert!(t.ends_with('Z') && t.as_bytes()[19] == b'.', "{l:?}");
            (t.to_owned(), rest.to_owned())
        };
        let out: Vec<(String, String)> = text(&o.stdout).lines().map(split).collect();
        let lines: Vec<&str> = out.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(lines, ["one", "two", "\u{fffd}", "partial"], "{out:?}");
        let err: Vec<(String, String)> = text(&o.stderr).lines().map(split).collect();
        assert_eq!(err[0].1, "err");
        let (one, two) = (&out[0].0, &out[1].0);
        assert!(one < two && two < &err[0].0, "{out:?} {err:?}");

        // --since, --until: 그 시각을 넣는다. 받는 형식은 events와 같다.
        let o = e.sbx(&["logs", "--since", two, &id]);
        assert_eq!(text(&o.stdout), "two\n\u{fffd}\npartial");
        assert_eq!(text(&o.stderr), "err\n");
        let o = e.sbx(&["logs", "--until", one, &id]);
        assert_eq!(
            (text(&o.stdout), text(&o.stderr)),
            ("one\n".into(), "".into())
        );
        let o = e.sbx(&["logs", "--since", "1h", "--until", one, "-t", &id]);
        assert_eq!(text(&o.stdout), format!("{one} one\n"));
        // --tail은 범위 안에서 센다.
        let o = e.sbx(&["logs", "--until", two, "-n", "1", &id]);
        assert_eq!(text(&o.stdout), "two\n");
        let o = e.sbx(&["logs", "--since", "yesterday", &id]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("--since"), "{}", text(&o.stderr));

        // --format json: 줄 조각마다 LogLine 하나. UTF-8이 아니면 base64, 끝나지 않은 줄은 줄바꿈 없이.
        let o = e.sbx(&["--format", "json", "logs", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
        let got: Vec<LogLine> = text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|err| panic!("{err}: {l:?}")))
            .collect();
        let short: Vec<(&str, &str, &str)> = got
            .iter()
            .map(|l| (l.stream.as_str(), l.data.as_str(), l.encoding.as_str()))
            .collect();
        assert_eq!(
            short,
            [
                ("stdout", "one\n", "utf-8"),
                ("stdout", "two\n", "utf-8"),
                ("stderr", "err\n", "utf-8"),
                ("stdout", "/wo=", "base64"),
                ("stdout", "partial", "utf-8"),
            ]
        );
        assert_eq!(got[0].time.as_deref(), Some(one.as_str()));
        let o = e.sbx(&["--format", "json", "logs", "-n", "1", "--since", two, &id]);
        let last: LogLine = serde_json::from_str(text(&o.stdout).trim()).unwrap();
        assert_eq!(last.data, "partial");

        // 이 판 전의 sbx가 쓴 로그(머리 없음, 시각 없음): 그대로 보이고, 시각이 필요한 옵션은 오류다.
        let log = e.path("data/sandboxes").join(&id).join("logs/main.log");
        std::fs::write(&log, [1, 0, 0, 0, 4, b'o', b'l', b'd', b'\n']).unwrap();
        assert_eq!(text(&e.sbx(&["logs", &id]).stdout), "old\n");
        for flag in [&["-t"][..], &["--since", "1h"], &["--until", "1h"]] {
            let mut args = vec!["logs"];
            args.extend_from_slice(flag);
            args.push(&id);
            let o = e.sbx(&args);
            assert_eq!(o.status.code(), Some(1), "{flag:?}");
            assert!(o.stdout.is_empty(), "{flag:?}");
            assert!(text(&o.stderr).contains("older sbx"), "{}", text(&o.stderr));
        }
        let o = e.sbx(&["--format", "json", "logs", "-t", &id]);
        let old: LogLine = serde_json::from_str(text(&o.stdout).trim()).unwrap();
        assert_eq!((old.time, old.data.as_str()), (None, "old\n"));

        // -f --until: 그 시각이 지나면 따라가기를 끝낸다. stdout이 닫혀도 조용히 끝난다.
        let o = e.sbx(&[
            "run",
            "-d",
            IMAGE,
            "sh",
            "-c",
            "while :; do echo y; sleep 0.05; done",
        ]);
        let id = text(&o.stdout).trim().to_owned();
        let soon = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 2;
        let child = e.spawn(&["logs", "-f", "--until", &soon.to_string(), &id]);
        let o = wait_with_timeout(child, Duration::from_secs(15));
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(text(&o.stdout).starts_with("y\n"), "{}", text(&o.stdout));
        let mut child = e.spawn(&["--format", "json", "logs", "-f", &id]);
        let mut first = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut first)
            .unwrap();
        let _: LogLine = serde_json::from_str(&first).unwrap();
        let o = wait_with_timeout(child, Duration::from_secs(15));
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(o.stderr.is_empty(), "{}", text(&o.stderr));
        assert_eq!(e.sbx(&["rm", "-f", &id]).status.code(), Some(0));
    }

    /// 시그널 `sig`를 받으면 `code`로 끝나고 TERM은 무시하는 기본 프로세스를 detached로 시작한다.
    /// 트랩을 건 뒤("ready"가 로그에 나온 뒤) ID를 돌려준다.
    fn run_trapping(e: &Env, extra: &[&str], sig: &str, code: i32) -> String {
        let script = format!(
            "trap '' TERM; trap 'exit {code}' {sig}; echo ready; while :; do sleep 0.02; done"
        );
        let mut args = vec!["run", "-d"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[IMAGE, "sh", "-c", &script]);
        let o = e.sbx(&args);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        wait_logs(e, &id, "ready\n", "");
        id
    }

    fn stop_sends_the_stop_signal_then_powers_off() {
        let e = Env::new();
        // --stop-signal로 정한 시그널을 보낸다.
        let id = run_trapping(&e, &["--stop-signal", "SIGINT"], "INT", 3);
        assert_eq!(e.sbx(&["inspect", &id]).status.code(), Some(0));
        let o = e.sbx(&["stop", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(state_of(&e, &id).unwrap()["exit_code"], 3);

        // -s가 이긴다. 받지 않으면(TERM은 무시) -t 뒤에 게스트를 끈다: 137.
        let id = run_trapping(&e, &["--stop-timeout", "30"], "USR2", 4);
        let t = Instant::now();
        let o = e.sbx(&["stop", "-s", "TERM", "-t", "1", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let took = t.elapsed();
        assert!(
            took >= Duration::from_secs(1) && took < Duration::from_secs(15),
            "{took:?}"
        );
        assert_eq!(state_of(&e, &id).unwrap()["exit_code"], 137);

        // 잘못된 시그널은 아무것도 하지 않고 오류.
        let o = e.sbx(&["stop", "-s", "NOPE", &id]);
        assert_ne!(o.status.code(), Some(0));
        let o = e.sbx(&["run", "-d", "--stop-signal", "NOPE", IMAGE, "true"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("signal"), "{}", text(&o.stderr));
    }

    fn kill_wait_and_restart() {
        let e = Env::new();
        let id = run_trapping(&e, &[], "USR1", 42);
        let waiter = e.spawn(&["wait", &id]);
        // TERM은 무시하므로 멈추지 않는다.
        // ID 앞부분으로도 된다(ps가 보여주는 12자). 받은 그대로 출력한다.
        let short = &id[..12];
        let o = e.sbx(&["kill", "-s", "TERM", short]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), short);
        let ps = text(&e.sbx(&["ps"]).stdout);
        assert!(
            ps.lines().any(|l| l.starts_with(&format!("{short} "))),
            "{ps}"
        );
        assert!(!ps.contains(&id), "{ps}");
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(state_of(&e, &id).unwrap()["state"], "running");
        assert_eq!(e.sbx(&["kill", "-s", "usr1", &id]).status.code(), Some(0));
        let out = wait_with_timeout(waiter, Duration::from_secs(20));
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "42\n");
        // 멈춘 샌드박스: wait은 곧바로, kill은 오류.
        assert_eq!(text(&e.sbx(&["wait", &id]).stdout), "42\n");
        assert_ne!(e.sbx(&["kill", &id]).status.code(), Some(0));

        // restart는 멈춘 것도 다시 시작한다(붙지 않고).
        let o = e.sbx(&["restart", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).trim(), id);
        assert_eq!(state_of(&e, &id).unwrap()["state"], "running");
        // 실행 중이면 멈추고 다시 시작한다. 기본 kill은 KILL: 137.
        wait_logs(&e, &id, "ready\nready\n", "");
        let o = e.sbx(&["restart", "-t", "0", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(state_of(&e, &id).unwrap()["state"], "running");
        wait_logs(&e, &id, "ready\nready\nready\n", "");
        assert_eq!(e.sbx(&["kill", &id]).status.code(), Some(0));
        assert_eq!(text(&e.sbx(&["wait", &id]).stdout), "137\n");
        assert_eq!(e.sbx(&["rm", &id]).status.code(), Some(0));
    }

    /// 자식의 표준 출력을 따로 읽어 모은다.
    struct Reader(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Reader {
        fn start(child: &mut Child) -> Self {
            use std::io::Read;
            let mut out = child.stdout.take().unwrap();
            let buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let b = buf.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 4096];
                while let Ok(n) = out.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    b.lock().unwrap().extend_from_slice(&chunk[..n]);
                }
            });
            Self(buf)
        }

        fn text(&self) -> String {
            text(&self.0.lock().unwrap())
        }

        fn wait_for(&self, needle: &str) {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !self.text().contains(needle) {
                assert!(
                    Instant::now() < deadline,
                    "no {needle:?} in {:?}",
                    self.text()
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn attach_to_a_detached_sandbox() {
        let e = Env::new();
        let o = e.sbx(&[
            "run",
            "-d",
            "-i",
            IMAGE,
            "sh",
            "-c",
            "read a; echo got $a; read b; echo got $b >&2; exit 5",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        assert_eq!(e.sbx(&["inspect", &id]).status.code(), Some(0));
        // 붙은 쪽의 입력이 기본 프로세스로 간다. 붙은 쪽의 입력이 끝나도 표준 입력은 닫히지 않는다.
        let mut first = e
            .cmd(&["attach", &id])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        first.stdin.take().unwrap().write_all(b"x\n").unwrap();
        let reader = Reader::start(&mut first);
        reader.wait_for("got x\n");
        // 둘째로 붙은 쪽의 입력도 간다. 둘 다 종료 코드를 받는다.
        let second = e.sbx_with_stdin(&["attach", &id], b"y\n");
        assert_eq!(second.status.code(), Some(5), "{}", text(&second.stderr));
        assert_eq!(text(&second.stderr), "got y\n");
        let first = wait_with_timeout(first, Duration::from_secs(20));
        assert_eq!(first.status.code(), Some(5), "{}", text(&first.stderr));
        assert_eq!(reader.text(), "got x\n");
        assert_eq!(text(&first.stderr), "got y\n");
        assert_eq!(wait_state(&e, &id, "stopped")["exit_code"], 5);
        wait_logs(&e, &id, "got x\n", "got y\n");

        // 멈춘 샌드박스에는 붙지 않는다.
        let o = e.sbx(&["attach", &id]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("not running"),
            "{}",
            text(&o.stderr)
        );
        // 다른 sbx가 붙어서 실행 중인 샌드박스(shim 없음)에도 붙지 않는다.
        let mut owner = e.spawn(&["run", IMAGE, "sleep", "30"]);
        let running = e.wait_running();
        let o = e.sbx(&["attach", &running]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("detached"), "{}", text(&o.stderr));
        assert_eq!(e.sbx(&["stop", &running]).status.code(), Some(0));
        let _ = owner.wait();
    }

    fn attach_tty_and_signal_proxy() {
        let e = Env::new();
        // -t: 터미널을 붙여 시작하고, 붙은 쪽은 터미널 출력을 받는다(\n은 \r\n이 된다).
        // 붙은 쪽이 끊겨도 샌드박스는 계속 돈다.
        let o = e.sbx(&[
            "run",
            "-d",
            "-t",
            IMAGE,
            "sh",
            "-c",
            "while :; do test -t 1 && echo tty; sleep 0.05; done",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        let mut client = e.spawn(&["attach", &id]);
        let reader = Reader::start(&mut client);
        reader.wait_for("tty\r\n");
        client.kill().unwrap();
        let _ = client.wait();
        assert_eq!(state_of(&e, &id).unwrap()["state"], "running");
        assert_eq!(e.sbx(&["kill", &id]).status.code(), Some(0));
        assert_eq!(text(&e.sbx(&["wait", &id]).stdout), "137\n");

        // 터미널이 아니면 받은 시그널을 기본 프로세스에 전한다(--sig-proxy).
        let o = e.sbx(&[
            "run",
            "-d",
            IMAGE,
            "sh",
            "-c",
            "trap 'echo term; exit 7' TERM; while :; do echo tick; sleep 0.05; done",
        ]);
        let id = text(&o.stdout).trim().to_owned();
        let mut client = e.spawn(&["attach", &id]);
        let reader = Reader::start(&mut client);
        reader.wait_for("tick\n");
        let pid = client.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        let out = wait_with_timeout(client, Duration::from_secs(20));
        assert_eq!(out.status.code(), Some(7), "{}", text(&out.stderr));
        assert!(reader.text().ends_with("term\n"), "{:?}", reader.text());
    }

    fn installed_executor_is_used_by_default() {
        let e = Env::new();
        let me = std::env::current_exe().unwrap();
        let without = |args: &[&str]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR");
            c.output().unwrap()
        };
        // 설치한 것이 없으면 PATH의 sbx-executor (여기에는 없다).
        let o = without(&["run", "--rm", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        // 설치: 규약을 확인하고, 처음 설치한 것이 기본이 된다.
        let o = without(&[
            "executor",
            "install",
            "--from",
            me.to_str().unwrap(),
            "fake@1.0",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "fake@1.0\n");
        let o = without(&["executor", "ls"]);
        assert!(text(&o.stdout).contains("* fake"), "{}", text(&o.stdout));
        let o = without(&["run", "--rm", IMAGE, "echo", "via", "installed"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "via installed\n");
        // 규약을 말하지 않는 것은 설치하지 않는다.
        let bad = e.path("not-an-executor");
        std::fs::write(&bad, "#!/bin/sh\nexit 3\n").unwrap();
        let o = without(&[
            "executor",
            "install",
            "--from",
            bad.to_str().unwrap(),
            "bad@1",
        ]);
        assert_ne!(o.status.code(), Some(0));
        assert!(!text(&without(&["executor", "ls"]).stdout).contains("bad"));
        // 지우면 기본도 없어진다.
        assert_eq!(
            without(&["executor", "rm", "fake@1.0"]).status.code(),
            Some(0)
        );
        let o = without(&["run", "--rm", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125));
    }

    /// 이 실행 파일(가짜 실행기)을 `sbx-executor`로 담은 묶음 압축 파일. (내용, SHA-256)
    fn bundle_of_me() -> (Vec<u8>, String) {
        use sha2::Digest as _;
        let me = std::fs::read(std::env::current_exe().unwrap()).unwrap();
        let mut b = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(me.len() as u64);
        h.set_mode(0o755);
        b.append_data(&mut h, "fake/sbx-executor", me.as_slice())
            .unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::none());
        gz.write_all(&b.into_inner().unwrap()).unwrap();
        let bytes = gz.finish().unwrap();
        let sha = sha2::Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        (bytes, sha)
    }

    fn executor_is_installed_from_a_repository() {
        let e = Env::new();
        let without = |args: &[&str]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR");
            c.output().unwrap()
        };
        // 처음에는 공식 저장소가 있다. 빼면 그대로 남고, 저장소가 없으면 어떻게 더하는지 알려 준다.
        let o = without(&["executor", "repo", "ls"]);
        assert!(
            text(&o.stdout).contains("official  ") && text(&o.stdout).contains("https://"),
            "{}",
            text(&o.stdout)
        );
        let o = without(&["executor", "repo", "rm", "official"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = without(&["executor", "repo", "ls"]);
        assert!(!text(&o.stdout).contains("official"), "{}", text(&o.stdout));
        let o = without(&["executor", "install", "fake"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("executor repo add"),
            "{}",
            text(&o.stderr)
        );

        // file:// 저장소: fake 1.0(설치할 것), 0.5(내용이 색인과 다르다), 2.0(다른 규약 버전), 3.0(다른 플랫폼).
        let repo = e.path("repo");
        std::fs::create_dir_all(repo.join("fake")).unwrap();
        let (bytes, sha) = bundle_of_me();
        std::fs::write(repo.join("fake/1.0.tar.gz"), &bytes).unwrap();
        let host = sbx_executor_repo::Platform::host();
        let art = |url: &str, sha: &str, os: &str| serde_json::json!([{"os": os, "architecture": host.architecture, "url": url, "sha256": sha, "size": bytes.len()}]);
        let index = serde_json::json!({"schema": 1, "executors": {"fake": {"versions": {
            "1.0": {"protocol": sbx_executor_proto::VERSION, "artifacts": art("fake/1.0.tar.gz", &sha, &host.os)},
            "0.5": {"protocol": sbx_executor_proto::VERSION, "artifacts": art("fake/1.0.tar.gz", &"0".repeat(64), &host.os)},
            "2.0": {"protocol": sbx_executor_proto::VERSION + 1, "artifacts": art("fake/1.0.tar.gz", &sha, &host.os)},
            "3.0": {"protocol": sbx_executor_proto::VERSION, "artifacts": art("fake/1.0.tar.gz", &sha, "plan9")},
        }}}});
        std::fs::write(repo.join("index.json"), index.to_string()).unwrap();
        let url = format!("file://{}", repo.display());
        // 앞 저장소는 읽을 수 없다: 경고하고 건너뛴다.
        let missing = format!("file://{}", e.path("missing").display());
        for (name, u) in [("broken", missing.as_str()), ("local", url.as_str())] {
            let o = without(&["executor", "repo", "add", name, u]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        }
        let o = without(&["executor", "repo", "ls"]);
        assert!(text(&o.stdout).contains("local"), "{}", text(&o.stdout));

        let o = without(&["executor", "search"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("warning: cannot fetch"),
            "{}",
            text(&o.stderr)
        );
        let rows: Vec<String> = text(&o.stdout)
            .lines()
            .skip(1)
            .map(|l| l.split_whitespace().take(2).collect::<Vec<_>>().join("@"))
            .collect();
        assert_eq!(rows, ["fake@1.0", "fake@0.5"]);

        // 이름만: 이 호스트와 규약 버전으로 되는 가장 높은 버전. 처음 설치한 것이 기본이 된다.
        let o = without(&["executor", "install", "fake"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "fake@1.0\n");
        assert!(
            text(&o.stderr).contains("downloading fake@1.0"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&["run", "--rm", IMAGE, "echo", "from", "repo"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "from repo\n");
        // 다시 하면 아무것도 하지 않는다.
        let o = without(&["executor", "install", "fake"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(text(&o.stderr).contains("already installed"));
        // 색인과 다른 내용은 설치하지 않는다. 받던 것도 남지 않는다.
        let o = without(&["executor", "install", "fake@0.5"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("sha256"), "{}", text(&o.stderr));
        // 이유를 알려 준다.
        let o = without(&["executor", "install", "fake@2.0"]);
        assert!(
            text(&o.stderr).contains("executor protocol"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&["executor", "install", "fake@3.0"]);
        assert!(
            text(&o.stderr).contains("no build for"),
            "{}",
            text(&o.stderr)
        );
        let downloads = e.path("cache/executor-downloads");
        let left = std::fs::read_dir(&downloads)
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(left, 0);

        // 묶음 압축 파일을 직접 설치.
        let o = without(&[
            "executor",
            "install",
            "--from",
            repo.join("fake/1.0.tar.gz").to_str().unwrap(),
            "fake@local",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = without(&["executor", "ls"]);
        let ls = text(&o.stdout);
        assert!(ls.contains("local") && !ls.contains("0.5"), "{ls}");

        assert_eq!(
            without(&["executor", "repo", "rm", "broken"]).status.code(),
            Some(0)
        );
        assert_ne!(
            without(&["executor", "repo", "rm", "broken"]).status.code(),
            Some(0)
        );
    }

    fn image_kernel_is_given_to_the_executor() {
        let e = Env::new();
        // 커널을 담은 이미지 (관리 쪽이 prepare로 꺼내 두었을 상태).
        let layout = CacheLayout::new(e.path("cache"));
        let id = ImageId::new(Digest::parse(&format!("sha256:{}", "4b".repeat(32))).unwrap());
        let mut config = ImageConfig {
            cmd: vec!["sh".into()],
            user: Some(format!("{}:{}", id_output("-u"), id_output("-g"))),
            ..ImageConfig::default()
        };
        config
            .labels
            .insert(ImageConfig::KERNEL_LABEL.into(), "/boot/vmlinux".into());
        let record = ImageRecord {
            id: id.clone(),
            reference: "docker.io/library/e2e-kernel:latest".into(),
            source: "test".into(),
            arch: Arch::host().unwrap(),
            config,
            layers: vec![],
            ignored_fields: vec![],
        };
        write::write_image_record(&layout, &record).unwrap();
        write::set_ref(&layout, &record.reference, &id).unwrap();
        std::fs::write(layout.rootfs(&id, RootfsFormat::Erofs), b"fs").unwrap();
        std::fs::write(layout.kernel(&id), b"kernel").unwrap();

        let run = |image: &str| {
            let rec = e.path("launch.json");
            let _ = std::fs::remove_file(&rec);
            let o = e
                .cmd(&["run", "--rm", image, "true"])
                .env("SBX_FAKE_RECORD", &rec)
                .output()
                .unwrap();
            let vm: serde_json::Value = std::fs::read(&rec)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            (o, vm)
        };
        let (o, vm) = run("e2e-kernel");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(
            vm["kernel"].as_str(),
            Some(layout.kernel(&id).to_str().unwrap()),
            "{vm}"
        );
        // 보통 이미지에는 커널을 보내지 않는다 (실행기의 기본 커널).
        let (o, vm) = run(IMAGE);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            vm.get("kernel").is_none() && vm.get("guest_init").is_some(),
            "{vm}"
        );
        // 커널을 꺼내 두지 않았으면 prepare가 다시 꺼내려 하고, 이미지에 없으면 띄우지 않는다.
        std::fs::remove_file(layout.kernel(&id)).unwrap();
        let (o, _) = run("e2e-kernel");
        assert_eq!(o.status.code(), Some(125));
        assert!(
            text(&o.stderr).contains("image kernel /boot/vmlinux"),
            "{}",
            text(&o.stderr)
        );
    }

    /// `sbx mcp`의 한 연결. 요청을 보내고 그 답 한 줄을 읽는다.
    struct Mcp {
        child: Child,
        out: std::io::BufReader<std::process::ChildStdout>,
        next: u64,
    }

    impl Mcp {
        fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
            use std::io::BufRead;
            self.next += 1;
            let msg = serde_json::json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params});
            let stdin = self.child.stdin.as_mut().unwrap();
            writeln!(stdin, "{msg}").unwrap();
            stdin.flush().unwrap();
            let mut line = String::new();
            self.out.read_line(&mut line).unwrap();
            let v: serde_json::Value =
                serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"));
            assert_eq!(v["id"], self.next, "{v}");
            v
        }

        /// 한 줄을 그대로 보낸다.
        fn write_raw(&mut self, line: &[u8]) {
            let stdin = self.child.stdin.as_mut().unwrap();
            stdin.write_all(line).unwrap();
            stdin.write_all(b"\n").unwrap();
            stdin.flush().unwrap();
        }

        /// 다음 한 줄을 JSON으로 읽는다.
        fn read(&mut self) -> serde_json::Value {
            use std::io::BufRead;
            let mut line = String::new();
            self.out.read_line(&mut line).unwrap();
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
        }

        /// 도구를 부르고 `structuredContent`(실패면 글)를 돌려준다.
        fn call(
            &mut self,
            name: &str,
            args: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            let r = self.request(
                "tools/call",
                serde_json::json!({"name": name, "arguments": args}),
            );
            let r = &r["result"];
            if r["isError"] == true {
                return Err(r["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned());
            }
            Ok(r["structuredContent"].clone())
        }
    }

    fn mcp_serves_sandboxes_to_agents() {
        let e = Env::new();
        let mut child = e
            .cmd(&["mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            out,
            next: 0,
        };

        let init = m.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
        assert_eq!(init["result"]["serverInfo"]["name"], "sbx", "{init}");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        // 알림에는 답하지 않는다(다음 답이 다음 요청의 것이다).
        writeln!(
            m.child.stdin.as_mut().unwrap(),
            r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#
        )
        .unwrap();
        let tools = m.request("tools/list", serde_json::json!({}));
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 7);
        let unknown = m.request("resources/list", serde_json::json!({}));
        assert_eq!(unknown["error"]["code"], -32601);
        // 없는 도구는 JSON-RPC 오류 -32602다.
        let r = m.request(
            "tools/call",
            serde_json::json!({"name": "nope", "arguments": {}}),
        );
        assert_eq!(r["error"]["code"], -32602, "{r}");
        // 잘못된 UTF-8과 batch에도 서버는 죽지 않고 답한다.
        m.write_raw(&[0xff, 0xfe]);
        assert_eq!(m.read()["error"]["code"], -32700);
        m.write_raw(br#"[{"jsonrpc":"2.0","id":99,"method":"ping"}]"#);
        assert_eq!(m.read()["error"]["code"], -32600);
        assert!(m.request("ping", serde_json::json!({}))["result"].is_object());

        // 출력은 끝을 남긴다(오류는 보통 끝에 있다).
        let r = m.call("run", serde_json::json!({"image": IMAGE, "command": ["sh", "-c", "i=0; while [ $i -lt 40000 ]; do echo xxxxxxxxx; i=$((i+1)); done; echo END"]})).unwrap();
        let out = r["stdout"].as_str().unwrap();
        assert!(
            out.ends_with("END\n") && r["truncated"] == true,
            "{}",
            &out[out.len().saturating_sub(40)..]
        );
        assert!(out.len() <= sbx_cli::mcp::OUTPUT_LIMIT, "{}", out.len());

        // 취소하면 답하지 않고, VM을 끄고 지운다.
        m.next += 1;
        let cancelled = m.next;
        let call = serde_json::json!({"jsonrpc": "2.0", "id": cancelled, "method": "tools/call",
            "params": {"name": "run", "arguments": {"image": IMAGE, "command": ["sleep", "30"]}}});
        m.write_raw(call.to_string().as_bytes());
        let started = Instant::now();
        while e.ps().is_empty() && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        m.write_raw(
            serde_json::json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
            "params": {"requestId": cancelled}})
            .to_string()
            .as_bytes(),
        );
        while !e.ps().is_empty() && started.elapsed() < Duration::from_secs(15) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(e.ps().is_empty(), "{:?}", e.ps());
        assert!(started.elapsed() < Duration::from_secs(15));
        // 다음 답은 다음 요청의 것이다(취소한 호출의 답은 오지 않는다).
        let pong = m.request("ping", serde_json::json!({}));
        assert!(pong["result"].is_object(), "{pong}");

        // run: 종료 코드와 두 출력. 끝나면 지운다.
        let r = m.call("run", serde_json::json!({"image": IMAGE, "command": ["sh", "-c", "echo out; echo err >&2; exit 3"]})).unwrap();
        assert_eq!(
            (
                r["exit_code"].as_i64(),
                r["stdout"].as_str(),
                r["stderr"].as_str()
            ),
            (Some(3), Some("out\n"), Some("err\n")),
            "{r}"
        );
        assert_eq!(r["timed_out"], false);
        let r = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["cat"], "stdin": "piped in"}),
            )
            .unwrap();
        assert_eq!(r["stdout"], "piped in");
        // 시간이 넘으면 끄고 지운다.
        let started = Instant::now();
        let r = m.call("run", serde_json::json!({"image": IMAGE, "command": ["sh", "-c", "echo before; sleep 30"], "timeout_secs": 1})).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(
            (
                r["timed_out"].as_bool(),
                r["exit_code"].is_null(),
                r["stdout"].as_str()
            ),
            (Some(true), true, Some("before\n")),
            "{r}"
        );
        assert!(e.ps().is_empty(), "{:?}", e.ps());
        // 틀린 인자는 도구 오류다.
        let err = m
            .call("run", serde_json::json!({"image": "-v"}))
            .unwrap_err();
        assert!(err.contains("invalid image"), "{err}");

        // CLI로 만든 샌드박스는 MCP 도구에 보이지 않는다(호스트 폴더를 붙였을 수 있다).
        let o = e.sbx(&["run", "-d", "--name", "by-cli", IMAGE, "sleep", "600"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        for (tool, args) in [
            (
                "exec",
                serde_json::json!({"id": "by-cli", "command": ["true"]}),
            ),
            (
                "read_file",
                serde_json::json!({"id": "by-cli", "path": "/etc/hostname"}),
            ),
            (
                "write_file",
                serde_json::json!({"id": "by-cli", "path": "/tmp/x", "content": "x"}),
            ),
            ("remove", serde_json::json!({"id": "by-cli"})),
        ] {
            let err = m.call(tool, args).unwrap_err();
            assert!(err.contains("created through MCP"), "{tool}: {err}");
        }
        let r = m.call("list", serde_json::json!({})).unwrap();
        assert_eq!(r["sandboxes"].as_array().unwrap().len(), 0, "{r}");
        let o = e.sbx(&["rm", "-f", "by-cli"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

        // create, exec, write_file, read_file, list, remove.
        let id = m
            .call(
                "create",
                serde_json::json!({"image": IMAGE, "command": ["sleep", "600"]}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let r = m.call("exec", serde_json::json!({"id": id, "command": ["sh", "-c", "echo $X; exit 5"], "env": {"X": "y"}})).unwrap();
        assert_eq!(
            (r["exit_code"].as_i64(), r["stdout"].as_str()),
            (Some(5), Some("y\n")),
            "{r}"
        );
        let r = m
            .call(
                "exec",
                serde_json::json!({"id": &id[..6], "command": ["sleep", "30"], "timeout_secs": 1}),
            )
            .unwrap();
        assert_eq!(r["timed_out"], true, "{r}");
        // 가짜 실행기의 게스트 경로는 호스트 경로다.
        let dir = e.path("g");
        std::fs::create_dir(&dir).unwrap();
        let text_path = dir.join("a.txt").to_string_lossy().into_owned();
        m.call(
            "write_file",
            serde_json::json!({"id": id, "path": text_path, "content": "hello"}),
        )
        .unwrap();
        assert_eq!(std::fs::read(&text_path).unwrap(), b"hello");
        let r = m
            .call(
                "read_file",
                serde_json::json!({"id": id, "path": text_path}),
            )
            .unwrap();
        assert_eq!(
            (r["encoding"].as_str(), r["content"].as_str()),
            (Some("utf-8"), Some("hello"))
        );
        // 링크는 게스트 경로로 따라간다(상대 경로도). 고리는 끝이 있다.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("a.txt", dir.join("rel")).unwrap();
            std::os::unix::fs::symlink(dir.join("rel"), dir.join("abs")).unwrap();
            let r = m
                .call(
                    "read_file",
                    serde_json::json!({"id": id, "path": dir.join("abs")}),
                )
                .unwrap();
            assert_eq!(r["content"], "hello", "{r}");
            std::os::unix::fs::symlink("loop", dir.join("loop")).unwrap();
            let err = m
                .call(
                    "read_file",
                    serde_json::json!({"id": id, "path": dir.join("loop")}),
                )
                .unwrap_err();
            assert!(err.contains("symbolic links"), "{err}");
        }
        // 한도를 넘는 파일과 디렉터리는 내용을 옮기기 전에 거절한다.
        let big = std::fs::File::create(dir.join("big")).unwrap();
        big.set_len(sbx_cli::mcp::FILE_LIMIT + 1).unwrap();
        let err = m
            .call(
                "read_file",
                serde_json::json!({"id": id, "path": dir.join("big")}),
            )
            .unwrap_err();
        assert!(err.contains("the limit is"), "{err}");
        let err = m
            .call("read_file", serde_json::json!({"id": id, "path": dir}))
            .unwrap_err();
        assert!(err.contains("is a directory"), "{err}");
        // 없는 부모 디렉터리는 만든다.
        let nested = dir.join("x/y/z.txt").to_string_lossy().into_owned();
        m.call(
            "write_file",
            serde_json::json!({"id": id, "path": nested, "content": "deep"}),
        )
        .unwrap();
        assert_eq!(std::fs::read(&nested).unwrap(), b"deep");
        let bin_path = dir.join("b.bin").to_string_lossy().into_owned();
        m.call("write_file", serde_json::json!({"id": id, "path": bin_path, "content": "AP8=", "encoding": "base64"})).unwrap();
        assert_eq!(std::fs::read(&bin_path).unwrap(), [0u8, 255]);
        let r = m
            .call("read_file", serde_json::json!({"id": id, "path": bin_path}))
            .unwrap();
        assert_eq!(
            (r["encoding"].as_str(), r["content"].as_str()),
            (Some("base64"), Some("AP8="))
        );
        let r = m.call("list", serde_json::json!({})).unwrap();
        let all = r["sandboxes"].as_array().unwrap();
        assert_eq!(
            (all.len(), all[0]["id"].as_str(), all[0]["state"].as_str()),
            (1, Some(id.as_str()), Some("running")),
            "{r}"
        );
        m.call("remove", serde_json::json!({"id": id})).unwrap();
        assert!(e.ps().is_empty());
        assert!(
            m.call("exec", serde_json::json!({"id": id, "command": ["true"]}))
                .is_err()
        );

        // 표준 입력을 닫으면 끝난다.
        drop(m.child.stdin.take());
        let out = wait_with_timeout(m.child, Duration::from_secs(10));
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    }
}
