//! `sbx` 실행 파일 끝-끝 테스트. 가짜 실행기로 VM 없이 CLI → 실행 코어 → 실행기 규약 → 게스트 처리를 모두 지난다.
//!
//! 이 테스트 실행 파일이 곧 가짜 실행기이자 shim이다: `sbx --executor <이 파일>`이 `<이 파일> capabilities|run`으로
//! 부르면 `sbx_executor_fake::process::main`을, `sbx --shim <이 파일>`이 `<이 파일> start ...`로 부르면
//! `sbx_shim::main`을 돈다. 그래서 다른 패키지의 실행 파일을 빌드할 필요가 없다(`harness = false`).
//!
//! 가짜 실행기의 게스트 처리는 호스트에서 지금 사용자로 돈다. 그래서 이미지의 사용자는 지금 사용자이고,
//! 게스트 경로는 호스트 경로다. 실제 VM 검증은 실행기 쪽 E2E(`cargo xtask smoke`)가 맡는다.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if matches!(
        args.get(1).map(String::as_str),
        Some("capabilities" | "run")
    ) {
        // 설치한 어댑터로 불렸으면 VMM 묶음 디렉터리를 받아야 한다(테스트의 묶음에는 `vmm` 파일이 있다).
        let as_adapter = std::env::current_exe()
            .is_ok_and(|p| p.components().any(|c| c.as_os_str() == "adapters"));
        let dir = std::env::var_os(sbx_executor_proto::VMM_DIR_ENV);
        let has_vmm = dir
            .as_ref()
            .is_some_and(|d| std::path::Path::new(d).join("vmm").is_file());
        if (as_adapter || dir.is_some()) && !has_vmm {
            // 실제 어댑터(firecracker)처럼 VMM 없이는 capabilities에도 답하지 않는다.
            eprintln!("fake adapter: no VMM package in {dir:?}");
            return ExitCode::FAILURE;
        }
        #[cfg(unix)]
        return sbx_executor_fake::process::main();
    }
    if args.get(1).map(String::as_str) == Some(sbx_shim::config::START) {
        return sbx_shim::main();
    }
    // 샌드박스 안에서 프록시를 모르는 프로그램 노릇(투명 TCP). `transparent_tcp_reaches_allowed_names`가 부른다.
    #[cfg(unix)]
    if args.get(1).map(String::as_str) == Some("net-probe") {
        return unix::net_probe(&args[2..]);
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
            "docker_options_reach_the_sandbox",
            docker_options_reach_the_sandbox,
        ),
        (
            "completion_offers_sandboxes_and_images",
            completion_offers_sandboxes_and_images,
        ),
        ("json_output_contract", json_output_contract),
        (
            "system_df_and_prune_free_what_nothing_uses",
            system_df_and_prune_free_what_nothing_uses,
        ),
        ("events_replay_and_follow", events_replay_and_follow),
        ("filters", filters),
        ("jq_shapes_json_output", jq_shapes_json_output),
        ("tui_needs_a_terminal", tui_needs_a_terminal),
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
        ("stats_reports_guest_usage", stats_reports_guest_usage),
        (
            "published_ports_reach_the_sandbox",
            published_ports_reach_the_sandbox,
        ),
        (
            "login_and_logout_use_dockers_config",
            login_and_logout_use_dockers_config,
        ),
        ("tag_and_push_to_a_registry", tag_and_push_to_a_registry),
        (
            "load_and_save_round_trip_an_oci_archive",
            load_and_save_round_trip_an_oci_archive,
        ),
        (
            "images_on_one_base_store_it_once",
            images_on_one_base_store_it_once,
        ),
        (
            "more_layers_than_disks_run_through_a_merged_disk",
            more_layers_than_disks_run_through_a_merged_disk,
        ),
        (
            "a_layout_1_cache_is_migrated_on_first_use",
            a_layout_1_cache_is_migrated_on_first_use,
        ),
        ("policy_json_is_followed", policy_json_is_followed),
        (
            "registries_conf_is_read_for_pulls",
            registries_conf_is_read_for_pulls,
        ),
        (
            "a_detached_sandbox_stops_after_its_timeout",
            a_detached_sandbox_stops_after_its_timeout,
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
            "a_repository_that_vanishes_or_turns_hostile_breaks_nothing_installed",
            a_repository_that_vanishes_or_turns_hostile_breaks_nothing_installed,
        ),
        (
            "vmm_packages_pins_and_version_files",
            vmm_packages_pins_and_version_files,
        ),
        ("executors_in_use_are_kept", executors_in_use_are_kept),
        (
            "signed_repositories_and_names_that_stay_put",
            signed_repositories_and_names_that_stay_put,
        ),
        (
            "the_default_executor_is_installed_on_first_use",
            the_default_executor_is_installed_on_first_use,
        ),
        (
            "image_kernel_is_given_to_the_executor",
            image_kernel_is_given_to_the_executor,
        ),
        (
            "mcp_limits_what_an_agent_can_ask_for",
            mcp_limits_what_an_agent_can_ask_for,
        ),
        (
            "mcp_limits_hold_under_concurrent_calls",
            mcp_limits_hold_under_concurrent_calls,
        ),
        (
            "mcp_serves_sandboxes_to_agents",
            mcp_serves_sandboxes_to_agents,
        ),
        ("attach_tty_and_signal_proxy", attach_tty_and_signal_proxy),
        ("volume_is_shared_live", volume_is_shared_live),
        (
            "live_share_tells_the_guest_or_points_elsewhere",
            live_share_tells_the_guest_or_points_elsewhere,
        ),
        ("network_allow_list", network_allow_list),
        (
            "transparent_tcp_reaches_allowed_names",
            transparent_tcp_reaches_allowed_names,
        ),
        (
            "connect_reaches_another_sandbox_by_name",
            connect_reaches_another_sandbox_by_name,
        ),
        (
            "mcp_connect_stays_among_mcp_sandboxes",
            mcp_connect_stays_among_mcp_sandboxes,
        ),
        (
            "egress_limit_and_network_events",
            egress_limit_and_network_events,
        ),
        (
            "new_version_notice_and_upgrade",
            new_version_notice_and_upgrade,
        ),
        (
            "embedded_guest_init_is_unpacked_into_the_cache",
            embedded_guest_init_is_unpacked_into_the_cache,
        ),
        (
            "build_runs_stages_and_caches_steps",
            build_runs_stages_and_caches_steps,
        ),
        (
            "build_failures_targets_and_build_args",
            build_failures_targets_and_build_args,
        ),
        (
            "build_scratch_without_a_vm_and_shared_base_layers",
            build_scratch_without_a_vm_and_shared_base_layers,
        ),
        ("build_shows_blocked_names", build_shows_blocked_names),
        (
            "build_vm_takes_cpus_and_memory",
            build_vm_takes_cpus_and_memory,
        ),
        (
            "build_writes_the_image_config",
            build_writes_the_image_config,
        ),
        (
            "build_outputs_export_one_image_everywhere",
            build_outputs_export_one_image_everywhere,
        ),
        (
            "build_source_date_epoch_fixes_the_digest",
            build_source_date_epoch_fixes_the_digest,
        ),
        (
            "build_cache_stays_under_its_cap",
            build_cache_stays_under_its_cap,
        ),
        (
            "a_missing_layer_disk_stops_the_start",
            a_missing_layer_disk_stops_the_start,
        ),
        (
            "the_guest_clock_follows_the_host",
            the_guest_clock_follows_the_host,
        ),
        (
            "volume_data_outlives_the_sandbox",
            volume_data_outlives_the_sandbox,
        ),
        (
            "a_volume_has_one_writer_at_a_time",
            a_volume_has_one_writer_at_a_time,
        ),
        (
            "volume_disks_limits_and_old_executors",
            volume_disks_limits_and_old_executors,
        ),
        (
            "mcp_uses_only_its_own_volumes",
            mcp_uses_only_its_own_volumes,
        ),
        (
            "a_volume_waits_for_the_vm_of_a_dead_owner",
            a_volume_waits_for_the_vm_of_a_dead_owner,
        ),
        ("volumes_and_connect_together", volumes_and_connect_together),
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
        let leaked = LEAKED.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if !leaked.is_empty() {
            println!(
                "\ntest result: FAILED. processes outlived {} test environment(s): {}\n",
                leaked.len(),
                leaked.join("; ")
            );
            return ExitCode::FAILURE;
        }
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

    /// 층 없는 이미지의 rootfs(빈 rootfs 하나, `sbx_store::stack`). 관리 쪽이 prepare로 했을 일.
    fn plant_empty_rootfs(layout: &CacheLayout) {
        let path = layout.merged_disk(&sbx_store::stack::empty_chain(), RootfsFormat::Erofs);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"fs").unwrap();
    }

    /// `net-probe ROOT NAME PORT`: fake 실행기의 투명 TCP 흉내(`sbx_executor_fake::probe`)로 `NAME`의 A를 묻고,
    /// 주소가 있으면 `PORT`로 붙어 한 줄 보내고 받은 것을 적는다. `net-probe ROOT --resolv`: 게스트가 쓴 `resolv.conf`.
    /// `ROOT`는 데이터 디렉터리다(그 아래에서 흉내 디렉터리를 찾는다).
    pub fn net_probe(args: &[String]) -> ExitCode {
        use sbx_executor_fake::probe;
        use std::io::Read;
        let Some(dir) = args.first().and_then(|r| probe::find(Path::new(r))) else {
            println!("no fake net directory");
            return ExitCode::FAILURE;
        };
        if args.get(1).map(String::as_str) == Some("--resolv") {
            match std::fs::read_to_string(dir.join("etc/resolv.conf")) {
                Ok(t) => print!("{t}"),
                Err(_) => println!("no resolv.conf"),
            }
            return ExitCode::SUCCESS;
        }
        let (Some(name), Some(port)) =
            (args.get(1), args.get(2).and_then(|p| p.parse::<u16>().ok()))
        else {
            println!("usage: net-probe ROOT NAME PORT");
            return ExitCode::FAILURE;
        };
        let r = match probe::lookup(&dir, name, 1) {
            Ok(r) => r,
            Err(e) => {
                println!("{name}: {e}");
                return ExitCode::FAILURE;
            }
        };
        let addrs: Vec<String> = r.addrs.iter().map(|a| a.to_string()).collect();
        print!("{name}: rcode {} [{}]", r.rcode, addrs.join(","));
        if let Some(ip) = r.addrs.first() {
            match probe::route(&dir, std::net::SocketAddrV4::new(*ip, port)) {
                None => print!(" port {port}: nothing listens"),
                Some(to) => {
                    let mut c = std::net::TcpStream::connect(to).unwrap();
                    let _ = c.write_all(b"hello\n");
                    let _ = c.shutdown(std::net::Shutdown::Write);
                    let mut got = String::new();
                    match c.read_to_string(&mut got) {
                        Ok(_) if !got.is_empty() => print!(" port {port}: got {got:?}"),
                        _ => print!(" port {port}: closed"),
                    }
                }
            }
        }
        println!();
        ExitCode::SUCCESS
    }

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
            plant_empty_rootfs(&layout);
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
                // 실행기를 빼는 테스트도 진짜 공식 저장소에 닿지 않게. 자동 설치를 보는 테스트만 지우고 로컬 저장소를 준다.
                .env("SBX_NO_AUTO_EXECUTOR", "1")
                // 레지스트리 자격 증명은 테스트 안의 것만 본다.
                .env("DOCKER_CONFIG", self.root.join("docker"))
                .env("XDG_CONFIG_HOME", self.root.join("xdg-config"))
                .env_remove("REGISTRY_AUTH_FILE")
                // 빌드의 시각과 캐시 상한은 테스트가 정한다.
                .env_remove("SOURCE_DATE_EPOCH")
                .env_remove("SBX_BUILD_CACHE_MAX")
                // registries.conf도(사용자 drop-in은 HOME 아래).
                .env(
                    "CONTAINERS_REGISTRIES_CONF",
                    self.root.join("registries.conf"),
                )
                .env("HOME", self.root.join("home"))
                // 가짜 실행기가 자기와 소유 프로세스, 샌드박스의 프로세스 그룹을 여기 적는다(끝날 때 남은 것을 본다).
                .env("SBX_FAKE_PROCS", self.root.join("procs"))
                // 이 테스트 프로세스가 kill -9로 죽어도 가짜 실행기가 VM을 끈다.
                .env("SBX_FAKE_WATCH_PID", std::process::id().to_string())
                .stdin(Stdio::null());
            c
        }

        /// 테스트가 남긴 샌드박스를 지우고(제품의 길), 가짜 실행기가 기록한 프로세스(실행기, 소유 프로세스,
        /// 샌드박스의 프로세스 그룹)가 모두 사라졌는지 본다. 남은 것은 SIGKILL로 끝내고 돌려준다.
        fn reap(&self) -> Vec<sbx_executor_fake::leftover::Left> {
            use sbx_executor_fake::leftover;
            if let Ok(o) = self.cmd(&["ps", "-aq"]).output() {
                let ids: Vec<String> = text(&o.stdout)
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect();
                if !ids.is_empty() {
                    let mut args = vec!["rm", "-f"];
                    args.extend(ids.iter().map(String::as_str));
                    let _ = self.cmd(&args).output();
                }
            }
            let record = self.root.join("procs");
            let me = [std::process::id()];
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let left = leftover::alive(&record, &me);
                if left.is_empty() {
                    return left;
                }
                if Instant::now() >= deadline {
                    leftover::kill(&left);
                    return left;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
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

    /// 테스트가 끝난 뒤 프로세스가 남았다(새는 프로세스). 실행 전체를 실패로 끝낸다.
    static LEAKED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    impl Drop for Env {
        fn drop(&mut self) {
            // 패닉 때도 이 테스트가 띄운 것을 거둔다. 남은 것은 조용히 지우지 않고 알린다.
            let left = self.reap();
            if !left.is_empty() {
                let list: Vec<String> = left.iter().map(|l| l.to_string()).collect();
                eprintln!(
                    "e2e: processes outlived the test in {}: {}",
                    self.root.display(),
                    list.join(", ")
                );
                if !std::thread::panicking() {
                    LEAKED
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(format!("{} ({})", self.root.display(), list.join(", ")));
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn system_df_and_prune_free_what_nothing_uses() {
        use sbx_cli::output::{DiskUsage, PruneReport};
        let e = Env::new();
        let o = e.sbx(&["create", "--label", "keep=1", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let kept = text(&o.stdout).trim().to_owned();
        // 볼륨 둘: 실행 중인 샌드박스가 쓰는 것과 아무도 쓰지 않는 것.
        let busy = format!("busy:{}", e.path("busy").display());
        let o = e.sbx(&["run", "-d", "-v", &busy, IMAGE, "sleep", "30"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let running = text(&o.stdout).trim().to_owned();
        let o = e.sbx(&["volume", "create", "loose"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["create", IMAGE, "true"]);
        let stopped = text(&o.stdout).trim().to_owned();

        let o = e.sbx(&["--format", "json", "system", "df"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let rows: Vec<DiskUsage> = serde_json::from_slice(&o.stdout).unwrap();
        let sb = rows.iter().find(|r| r.kind == "sandboxes").unwrap();
        assert_eq!((sb.total, sb.active), (3, 1));
        let im = rows.iter().find(|r| r.kind == "images").unwrap();
        assert_eq!((im.total, im.active), (1, 1));
        let vol = rows.iter().find(|r| r.kind == "volumes").unwrap();
        assert_eq!((vol.total, vol.active), (2, 1));

        // 터미널이 아니면 -f가 있어야 한다.
        let o = e.sbx(&["system", "prune"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("-f"), "{}", text(&o.stderr));
        // 하나를 지우다 멈춘 샌드박스 디렉터리(오래된 것만 지운다).
        let half = e.path("data/sandboxes/.gone-1");
        std::fs::create_dir_all(&half).unwrap();
        let fresh = e.path("data/sandboxes/.busy-1");
        std::fs::create_dir_all(&fresh).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(&half)
            .unwrap()
            .set_modified(old)
            .unwrap();

        let o = e.sbx(&[
            "--format",
            "json",
            "system",
            "prune",
            "-f",
            "--filter",
            "label=keep",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let r: PruneReport = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(r.sandboxes, std::slice::from_ref(&kept));
        assert!(r.leftovers.iter().any(|l| l.ends_with(".gone-1")), "{r:?}");
        assert!(!half.exists() && fresh.exists());
        // -a: 멈춘 것을 모두, 쓰는 샌드박스가 없는 이미지도. 실행 중인 것과 그 이미지는 남는다.
        let o = e.sbx(&["--format", "json", "system", "prune", "-a", "-f"]);
        let r: PruneReport = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(r.sandboxes, [stopped]);
        assert!(
            r.images.is_empty(),
            "the running sandbox uses the image: {r:?}"
        );
        assert!(r.volumes.is_empty(), "volumes only with --volumes: {r:?}");
        // --volumes: 어느 샌드박스도 쓰지 않는 볼륨도.
        let o = e.sbx(&[
            "--format",
            "json",
            "system",
            "prune",
            "-a",
            "-f",
            "--volumes",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let r: PruneReport = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(r.volumes, ["loose"], "{r:?}");
        let ids: Vec<String> = e
            .ps()
            .iter()
            .map(|s| s["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(ids, std::slice::from_ref(&running));
        e.sbx(&["rm", "-f", &running]);
        let o = e.sbx(&["--format", "json", "system", "prune", "-a", "-f"]);
        let r: PruneReport = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(r.images.len(), 1, "{r:?}");
        let o = e.sbx(&["images", "-q"]);
        assert!(text(&o.stdout).trim().is_empty());
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

    /// 게스트 시계(fake 실행기: 호스트 시계는 그대로 두고 scratch 옆 파일이 게스트 시계다)가 틀려 있으면 시작 직후 맞고,
    /// 소유 프로세스가 멈췄다 풀리면(잠자기에서 깬 것처럼 타이머가 늦게 깬다) 1분 주기를 기다리지 않고 곧바로 맞는다.
    /// attached(`start -a`, sbx가 소유)와 detached(`start`, shim이 소유) 둘 다.
    fn the_guest_clock_follows_the_host() {
        use sbx_executor_fake::clock;
        let e = Env::new();
        let sandbox_dir = |id: &str| e.path("data").join("sandboxes").join(id);
        let clock_of =
            |id: &str| sbx_executor_fake::clock_file(&sandbox_dir(id).join("scratch.img"));
        let wait_right = |file: &Path, steps: u64| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let st = clock::read(file).unwrap();
                if st.offset.unsigned_abs() < 50_000_000 && st.steps >= steps {
                    return st;
                }
                assert!(
                    Instant::now() < deadline,
                    "the guest clock is still off: {st:?}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let signal = |sig: &str, pid: u32| {
            let ok = Command::new("kill")
                .args([sig, &pid.to_string()])
                .status()
                .unwrap()
                .success();
            assert!(ok, "kill {sig} {pid}");
        };
        let create = || {
            let o = e.sbx(&["create", IMAGE, "sleep", "60"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            let id = text(&o.stdout).trim().to_owned();
            // 부팅한 게스트 시계가 한 시간 늦다.
            clock::skew(&clock_of(&id), -3_600_000_000_000).unwrap();
            id
        };
        let check = |id: &str, owner: u32| {
            let file = clock_of(id);
            let st = wait_right(&file, 1);
            assert_eq!(st.steps, 1, "{st:?}");
            // 게스트 안에서 누가 시계를 10초 뒤로 돌렸다(`date -s`). 다음 주기 맞추기는 1분 뒤다.
            clock::skew(&file, -10_000_000_000).unwrap();
            let t = Instant::now();
            signal("-STOP", owner);
            std::thread::sleep(Duration::from_millis(2500));
            signal("-CONT", owner);
            let st = wait_right(&file, 2);
            assert_eq!(st.steps, 2, "{st:?}");
            assert!(t.elapsed() < Duration::from_secs(20), "{:?}", t.elapsed());
            let log = std::fs::read_to_string(sandbox_dir(id).join("logs/executor.log"))
                .unwrap_or_default();
            assert!(!log.contains("guest clock"), "{log}");
        };

        // attached: sbx가 소유한다.
        let id = create();
        let child = e.spawn(&["start", "-a", &id]);
        assert_eq!(e.wait_running(), id);
        check(&id, child.id());
        assert_eq!(e.sbx(&["stop", "-t", "0", &id]).status.code(), Some(0));
        let out = wait_with_timeout(child, Duration::from_secs(20));
        assert_eq!(text(&out.stdout), "", "{}", text(&out.stderr));
        assert_eq!(e.sbx(&["rm", &id]).status.code(), Some(0));

        // detached: shim이 소유한다. shim은 이 테스트 실행 파일이고 인자는 `start … <ID>`다.
        let id = create();
        let o = e.sbx(&["start", &id]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let me = std::env::current_exe().unwrap();
        let o = Command::new("ps")
            .args(["-ax", "-o", "pid=,command="])
            .output()
            .unwrap();
        let ps = text(&o.stdout);
        let shims: Vec<u32> = ps
            .lines()
            .map(str::trim)
            .filter(|l| l.contains(&format!("{} start ", me.display())) && l.ends_with(id.as_str()))
            .filter_map(|l| l.split_whitespace().next()?.parse().ok())
            .collect();
        assert_eq!(shims.len(), 1, "{ps}");
        check(&id, shims[0]);
        assert_eq!(e.sbx(&["rm", "-f", &id]).status.code(), Some(0));
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

    /// 투명 TCP: 프록시를 모르는 프로그램(`net-probe`)이 이름을 묻고 붙는다. 이름은 `.invalid`라 호스트가 풀 수 없다.
    /// 그래서 허용된 이름의 연결은 호스트의 이름 확인에서 거부되고("cannot resolve"), 그 기록이 연결이 이름으로
    /// 넘어갔다는 증거다(데이터가 오가는 것은 게스트 테스트와 같은 splice다). 허용하지 않은 이름은 DNS에서 거부되고
    /// 호스트는 풀지 않는다(거부 이유가 이름 풀기가 아니라 목록이다. 풀지 않았다는 직접 증거는 실행 코어 테스트
    /// `transparent_tcp_resolves_from_the_list_and_passes_names_to_the_host_check`).
    fn transparent_tcp_reaches_allowed_names() {
        let e = Env::new();
        let me = std::env::current_exe().unwrap();
        let data = e.path("data");
        let probe = |name: &str, port: u16| {
            format!(
                "{} net-probe {} {name} {port}",
                me.display(),
                data.display()
            )
        };
        let script = [
            format!("{} net-probe {} --resolv", me.display(), data.display()),
            probe("blocked.invalid", 22),
            probe("ok.invalid", 5432),
            probe("a.wild.invalid", 443),
            probe("b.wild.invalid", 443),
            probe("wild.invalid", 443),
            probe("any.invalid", 80),
            "curl -s -o /dev/null -w 'proxy %{http_code}' -x \"$HTTP_PROXY\" --noproxy '' http://any.invalid/"
                .into(),
        ]
        .join("; ");
        let o = e.sbx(&[
            "run",
            "--rm",
            "--network",
            "allow",
            "--allow",
            "ok.invalid:5432",
            "--allow",
            "*.wild.invalid:443",
            "--allow",
            "any.invalid:*",
            IMAGE,
            "sh",
            "-c",
            &script,
        ]);
        let out = text(&o.stdout);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{out}\n{err}");
        assert!(
            err.contains(
                "warning: --allow any.invalid:*: a rule for every port (*) cannot be listened for"
            ),
            "{err}"
        );
        assert!(!err.contains("ok.invalid:5432:"), "{err}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            [
                "# Written by sbx (--network allow or --connect): names are answered by the sandbox's loopback DNS.",
                "nameserver 127.0.0.1",
                "blocked.invalid: rcode 3 []",
                "ok.invalid: rcode 0 [198.18.0.1] port 5432: closed",
                "a.wild.invalid: rcode 0 [198.18.0.2] port 443: closed",
                "b.wild.invalid: rcode 0 [198.18.0.3] port 443: closed",
                "wild.invalid: rcode 3 []",
                "any.invalid: rcode 0 [198.18.0.4] port 80: nothing listens",
                "proxy 403",
            ],
            "{out}\n{err}"
        );

        let o = e.sbx(&[
            "--format",
            "json",
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "--filter",
            "type=network",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let events: Vec<serde_json::Value> = text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let find = |action: &str, host: &str, port: Option<&str>| {
            events
                .iter()
                .find(|ev| {
                    let a = &ev["attributes"];
                    ev["action"] == action
                        && a["host"] == host
                        && a.get("port").and_then(|p| p.as_str()) == port
                })
                .unwrap_or_else(|| panic!("no {action} {host} {port:?} in {events:?}"))
        };
        assert!(
            find("proxy_only", "any.invalid", Some("*"))["attributes"]["reason"]
                .as_str()
                .unwrap()
                .contains("HTTP_PROXY")
        );
        let blocked = find("refuse", "blocked.invalid", None);
        assert_eq!(blocked["attributes"]["via"], "dns");
        assert!(
            blocked["attributes"]["reason"]
                .as_str()
                .unwrap()
                .contains("not allowed by the network policy")
        );
        assert_eq!(
            find("refuse", "wild.invalid", None)["attributes"]["via"],
            "dns"
        );
        assert_eq!(
            find("lookup", "ok.invalid", None)["attributes"]["ports"],
            "5432"
        );
        assert_eq!(
            find("lookup", "b.wild.invalid", None)["attributes"]["ports"],
            "443"
        );
        assert_eq!(
            find("lookup", "any.invalid", None)["attributes"]["any_port"],
            "true"
        );
        // 가짜 주소로 온 연결은 이름으로 호스트 확인을 지났다.
        for (host, port) in [
            ("ok.invalid", "5432"),
            ("a.wild.invalid", "443"),
            ("any.invalid", "80"),
        ] {
            let r = find("refuse", host, Some(port));
            assert!(
                r["attributes"]["reason"]
                    .as_str()
                    .unwrap()
                    .contains("resolve"),
                "{r}"
            );
        }
        assert!(
            !events.iter().any(|ev| ev["action"] == "notice"),
            "{events:?}"
        );
        // 이름 묻기 기록은 동작으로도 고를 수 있다.
        let o = e.sbx(&[
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "--filter",
            "event=lookup",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).lines().count(), 4, "{}", text(&o.stdout));

        // --network allow가 아니면 지금과 같다: DNS는 모두 NXDOMAIN이고 resolv.conf를 쓰지 않는다.
        let script = format!(
            "{} net-probe {} --resolv; {}",
            me.display(),
            data.display(),
            probe("ok.invalid", 5432)
        );
        let o = e.sbx(&["run", "--rm", IMAGE, "sh", "-c", &script]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "no resolv.conf\nok.invalid: rcode 3 []\n");
    }

    /// 받은 것을 그대로 돌려주는 TCP 서비스를 호스트의 빈 포트에 띄운다(fake 게스트는 호스트에서 돌므로 대상 샌드박스
    /// 안의 서비스 노릇이다). 그 포트.
    fn echo_service() -> u16 {
        use std::io::Read;
        let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = service.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in service.incoming() {
                let Ok(mut c) = c else { return };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while let Ok(n) = c.read(&mut b) {
                        if n == 0 || c.write_all(&b[..n]).is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    /// `ROOT/names/NAME`이 가리키는 샌드박스 디렉터리에서 `net-probe`를 부르는 셸 줄들(샌드박스 안에서 돈다).
    fn probe_script(data: &Path, me: &str, probes: &[(&str, u16)]) -> String {
        let exe = std::env::current_exe().unwrap();
        let mut lines = vec![format!(
            "r={}/sandboxes/$(cat {}/names/{me})",
            data.display(),
            data.display()
        )];
        for (name, port) in probes {
            lines.push(format!("{} net-probe \"$r\" {name} {port}", exe.display()));
        }
        lines.join("; ")
    }

    /// 네트워크 기록(JSON). `filters`는 `--filter`의 값들이다.
    fn network_events(e: &Env, filters: &[&str]) -> Vec<serde_json::Value> {
        let mut args = vec![
            "--format",
            "json",
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "--filter",
            "type=network",
        ];
        for f in filters {
            args.extend(["--filter", f]);
        }
        let o = e.sbx(&args);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// `--connect`: 앱 샌드박스 안에서 프록시를 모르는 클라이언트(`net-probe`)가 `db`라는 이름으로 다른 샌드박스의
    /// TCP 포트에 닿아 보낸 바이트를 돌려받는다. 열지 않은 포트, 다른 샌드박스, 바깥 이름은 닿지 않고, 프록시도 없다.
    /// 대상이 멈추면 그 연결만 곧바로 끊겨 기록되고, 다시 시작하면 다음 연결은 된다. inspect, events --filter, 오류도 본다.
    fn connect_reaches_another_sandbox_by_name() {
        let e = Env::new();
        let data = e.path("data");
        let port = echo_service();
        for name in ["db", "other"] {
            let o = e.sbx(&["run", "-d", "--name", name, IMAGE, "sleep", "60"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        }
        let script = format!(
            "{}; printf 'proxy=[%s]\\n' \"$HTTP_PROXY\"",
            probe_script(
                &data,
                "app",
                &[
                    ("db", port),
                    ("db", port.wrapping_add(1)),
                    ("other", port),
                    ("app", port),
                    ("example.invalid", 443),
                ]
            )
        );
        let connect = format!("db:{port}");
        let o = e.sbx(&[
            "create",
            "--name",
            "app",
            "--connect",
            &connect,
            IMAGE,
            "sh",
            "-c",
            &script,
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["start", "-a", "app"]);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{out}\n{}", text(&o.stderr));
        assert_eq!(
            out.lines().collect::<Vec<_>>(),
            [
                format!("db: rcode 0 [198.18.0.1] port {port}: got \"hello\\n\""),
                format!(
                    "db: rcode 0 [198.18.0.1] port {}: nothing listens",
                    port.wrapping_add(1)
                ),
                "other: rcode 3 []".into(),
                "app: rcode 3 []".into(),
                "example.invalid: rcode 3 []".into(),
                "proxy=[]".into(),
            ],
            "{out}"
        );

        // inspect와 ps는 잇기 목록을 공개 자료형으로 보인다. 네트워크는 여전히 none이다.
        let o = e.sbx(&["--format", "json", "inspect", "app"]);
        let s: sbx_cli::output::Sandbox = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(s.network.mode, sbx_cli::output::NetworkMode::None);
        assert_eq!(
            s.network.connect,
            [sbx_cli::output::Connect {
                name: "db".into(),
                sandbox: "db".into(),
                port
            }]
        );
        let ps = e.ps();
        let app = ps.iter().find(|s| s["name"] == "app").unwrap();
        assert_eq!(app["network"]["connect"][0]["port"], port);
        assert!(
            ps.iter().find(|s| s["name"] == "db").unwrap()["network"]
                .get("connect")
                .is_none()
        );

        // 대상이 멈추면 그 연결만 곧바로 끊기고 이유가 남는다.
        assert_eq!(e.sbx(&["stop", "db"]).status.code(), Some(0));
        let started = Instant::now();
        let o = e.sbx(&["start", "-a", "app"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(
            text(&o.stdout).lines().next(),
            Some(format!("db: rcode 0 [198.18.0.1] port {port}: closed").as_str())
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "{:?}",
            started.elapsed()
        );
        // 다시 시작하면 다음 연결은 된다.
        assert_eq!(e.sbx(&["start", "db"]).status.code(), Some(0));
        let o = e.sbx(&["start", "-a", "app"]);
        assert_eq!(
            text(&o.stdout).lines().next(),
            Some(format!("db: rcode 0 [198.18.0.1] port {port}: got \"hello\\n\"").as_str()),
            "{}",
            text(&o.stderr)
        );

        // 기록: 잇는 쪽의 ID로 남고, 대상(이름이나 ID)으로도 고를 수 있다.
        let db_id = e.ps().iter().find(|s| s["name"] == "db").unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let app_id = app["id"].as_str().unwrap().to_owned();
        let events = network_events(&e, &["sandbox=db"]);
        let actions: Vec<&str> = events
            .iter()
            .map(|ev| ev["action"].as_str().unwrap())
            .collect();
        for want in ["lookup", "connect", "close", "refuse"] {
            assert!(actions.contains(&want), "{want}: {events:?}");
        }
        assert!(
            events.iter().all(|ev| ev["id"] == app_id.as_str()),
            "{events:?}"
        );
        let refused = events.iter().find(|ev| ev["action"] == "refuse").unwrap();
        assert_eq!(refused["attributes"]["target"], "db");
        assert_eq!(refused["attributes"]["target_id"], db_id.as_str());
        assert_eq!(refused["attributes"]["port"], port.to_string());
        assert!(
            refused["attributes"]["reason"]
                .as_str()
                .unwrap()
                .contains("not running"),
            "{refused}"
        );
        let closed = events.iter().find(|ev| ev["action"] == "close").unwrap();
        assert_eq!(
            (
                closed["attributes"]["sent"].as_str(),
                closed["attributes"]["received"].as_str()
            ),
            (Some("6"), Some("6"))
        );
        assert_eq!(
            network_events(&e, &[&format!("sandbox={}", &db_id[..12]), "event=refuse"]).len(),
            1
        );
        // 이름으로 고르면 잇는 쪽의 기록도 나온다(네트워크 기록에는 이름이 없다).
        let mine = network_events(&e, &["sandbox=app"]);
        assert!(
            mine.len() > events.len() && mine.iter().all(|ev| ev["id"] == app_id.as_str()),
            "{mine:?}"
        );
        assert!(network_events(&e, &["sandbox=other"]).is_empty());

        // ID 앞부분으로 주면 전체 ID로 기록하고, 안의 이름은 대상의 이름이다.
        let by_id = format!("{}:{port}", &db_id[..8]);
        let o = e.sbx(&[
            "create",
            "--name",
            "app2",
            "--connect",
            &by_id,
            IMAGE,
            "true",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["--format", "json", "inspect", "app2"]);
        let s: sbx_cli::output::Sandbox = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            (
                s.network.connect[0].name.as_str(),
                s.network.connect[0].sandbox.as_str()
            ),
            ("db", db_id.as_str())
        );
        // 잇기만으로도 전송량 상한을 받는다.
        let o = e.sbx(&[
            "create",
            "--connect",
            &connect,
            "--egress-limit",
            "1m",
            IMAGE,
            "true",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

        // 오류: 없는 대상, 허용 목록과 같은 이름, 꼴이 틀린 값, build에는 없다.
        let o = e.sbx(&["create", "--connect", "nothing:80", IMAGE, "true"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("--connect nothing:80"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&[
            "create",
            "--network",
            "allow",
            "--allow",
            "db:443",
            "--connect",
            &connect,
            IMAGE,
            "true",
        ]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("also allowed by --allow db:443"),
            "{}",
            text(&o.stderr)
        );
        for bad in ["db", "db:0", "localhost=db:80"] {
            let o = e.sbx(&["create", "--connect", bad, IMAGE, "true"]);
            assert_eq!(o.status.code(), Some(2), "{bad}: {}", text(&o.stderr));
        }
        let o = e.sbx(&["build", "--connect", &connect, "."]);
        assert_eq!(o.status.code(), Some(2), "{}", text(&o.stderr));
        assert!(text(&o.stderr).contains("--connect"), "{}", text(&o.stderr));
        for name in ["db", "other"] {
            let _ = e.sbx(&["rm", "-f", name]);
        }
    }

    /// 가짜 릴리스(시험용 키로 서명한 file:// 색인과 묶음)로 새 버전 알림과 `sbx upgrade`를 본다.
    fn new_version_notice_and_upgrade() {
        use sha2::{Digest, Sha256};
        let e = Env::new();
        let cur = env!("CARGO_PKG_VERSION");
        let mut parts: Vec<u64> = cur.split('.').map(|p| p.parse().unwrap()).collect();
        parts[2] += 1;
        let next = format!("{}.{}.{}", parts[0], parts[1], parts[2]);
        let os = if std::env::consts::OS == "macos" {
            "darwin"
        } else {
            std::env::consts::OS
        };
        let platform = format!("{os}-{}", std::env::consts::ARCH);
        let index_key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let archive_key = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let rel = e.path("rel");
        std::fs::create_dir_all(&rel).unwrap();
        let sign = |sk: &minisign::SecretKey, file: &Path, body: &[u8]| {
            let sig = minisign::sign(None, sk, body, None, None).unwrap();
            std::fs::write(file, body).unwrap();
            let mut s = file.as_os_str().to_owned();
            s.push(".minisig");
            std::fs::write(s, sig.into_string()).unwrap();
        };
        // 새 "sbx"는 --version에 그 버전을 말하는 스크립트다.
        let archive = {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            {
                let mut t = tar::Builder::new(&mut gz);
                for (name, body) in [
                    ("sbx", format!("#!/bin/sh\necho \"sbx {next}\"\n")),
                    ("sbx-shim", "#!/bin/sh\nexit 0\n".to_owned()),
                ] {
                    let mut h = tar::Header::new_gnu();
                    h.set_size(body.len() as u64);
                    h.set_mode(0o755);
                    h.set_cksum();
                    t.append_data(&mut h, format!("sbx-{platform}/{name}"), body.as_bytes())
                        .unwrap();
                }
                t.finish().unwrap();
            }
            gz.finish().unwrap()
        };
        let archive_file = rel.join(format!("sbx-{platform}.tar.gz"));
        sign(&archive_key.sk, &archive_file, &archive);
        let sha: String = Sha256::digest(&archive)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let asset = serde_json::json!({ platform.clone(): {
            "url": format!("file://{}", archive_file.display()), "sha256": sha, "size": archive.len() } });
        let index = serde_json::json!({
            "schema": 1, "serial": 1, "expires": 4_000_000_000u64,
            "releases": [
                { "version": cur, "assets": asset },
                { "version": next, "assets": asset },
            ]
        });
        let index_file = rel.join("releases.json");
        sign(&index_key.sk, &index_file, index.to_string().as_bytes());

        // 설치 디렉터리: 이 sbx의 사본과 sbx-shim.
        let bin = e.path("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_sbx"), bin.join("sbx")).unwrap();
        std::fs::write(bin.join("sbx-shim"), "#!/bin/sh\n").unwrap();
        let run = |program: &Path, args: &[&str], extra: &[(&str, &str)]| {
            let mut c = Command::new(program);
            c.args(args)
                .env("SBX_DATA_DIR", e.path("data"))
                .env("SBX_CACHE_DIR", e.path("cache"))
                .env(
                    "SBX_TEST_RELEASES_URL",
                    format!("file://{}", index_file.display()),
                )
                .env("SBX_TEST_RELEASE_INDEX_KEY", index_key.pk.to_base64())
                .env("SBX_TEST_RELEASE_ARCHIVE_KEY", archive_key.pk.to_base64())
                .env("SBX_VERIFY", "off")
                .env_remove("CI")
                .env_remove("SBX_NO_UPDATE_CHECK")
                .stdin(Stdio::null());
            for (k, v) in extra {
                c.env(k, v);
            }
            c.output().unwrap()
        };
        let sbx = Path::new(env!("CARGO_BIN_EXE_sbx"));

        // 알림: 명령 출력 뒤 stderr에 한 줄, 하루 한 번.
        let o = run(sbx, &["images", "-q"], &[]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let want = format!("sbx {cur} ({next} available): run 'sbx upgrade'");
        assert!(text(&o.stderr).contains(&want), "{}", text(&o.stderr));
        let o = run(sbx, &["images", "-q"], &[]);
        assert!(
            !text(&o.stderr).contains("available"),
            "once a day: {}",
            text(&o.stderr)
        );
        // json, 끈 환경에서는 없다.
        let o = run(
            sbx,
            &["images", "--format", "json"],
            &[("SBX_CACHE_DIR", e.path("c2").to_str().unwrap())],
        );
        assert!(
            !text(&o.stderr).contains("available"),
            "{}",
            text(&o.stderr)
        );
        let o = run(
            sbx,
            &["images", "-q"],
            &[
                ("SBX_CACHE_DIR", e.path("c3").to_str().unwrap()),
                ("SBX_NO_UPDATE_CHECK", "1"),
            ],
        );
        assert!(
            !text(&o.stderr).contains("available"),
            "{}",
            text(&o.stderr)
        );
        let o = run(
            sbx,
            &["images", "-q"],
            &[
                ("SBX_CACHE_DIR", e.path("c4").to_str().unwrap()),
                ("CI", "true"),
            ],
        );
        assert!(
            !text(&o.stderr).contains("available"),
            "{}",
            text(&o.stderr)
        );
        // info는 기록을 본다.
        let o = run(sbx, &["info", "--format", "json"], &[]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["update"]["latest"], serde_json::json!(next), "{v}");

        // --check: 종료 코드 10.
        let o = run(&bin.join("sbx"), &["upgrade", "--check"], &[]);
        assert_eq!(o.status.code(), Some(10), "{}", text(&o.stderr));
        assert!(
            text(&o.stdout).contains(&format!("{next} available")),
            "{}",
            text(&o.stdout)
        );

        // 묶음 서명이 다른 내용의 것이면 아무것도 바꾸지 않는다.
        let good_sig = std::fs::read(rel.join(format!("sbx-{platform}.tar.gz.minisig"))).unwrap();
        let bad = minisign::sign(None, &archive_key.sk, &b"other"[..], None, None).unwrap();
        std::fs::write(
            rel.join(format!("sbx-{platform}.tar.gz.minisig")),
            bad.into_string(),
        )
        .unwrap();
        let o = run(&bin.join("sbx"), &["upgrade", "-y"], &[]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("untrusted"), "{}", text(&o.stderr));
        let o = run(&bin.join("sbx"), &["--version"], &[]);
        assert_eq!(text(&o.stdout).trim(), format!("sbx {cur}"));
        // 다른 키로 서명한 묶음도.
        std::fs::write(
            rel.join(format!("sbx-{platform}.tar.gz.minisig")),
            &good_sig,
        )
        .unwrap();
        let other = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let o = run(
            &bin.join("sbx"),
            &["upgrade", "-y"],
            &[("SBX_TEST_RELEASE_ARCHIVE_KEY", &other.pk.to_base64())],
        );
        assert_ne!(o.status.code(), Some(0), "{}", text(&o.stderr));

        // 바꾼다.
        let o = run(
            &bin.join("sbx"),
            &["upgrade", "-y", "--format", "json"],
            &[],
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["installed"], serde_json::json!(next), "{v}");
        let o = Command::new(bin.join("sbx"))
            .arg("--version")
            .output()
            .unwrap();
        assert_eq!(text(&o.stdout).trim(), format!("sbx {next}"));
        let left: Vec<_> = std::fs::read_dir(&bin)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.') && !n.ends_with(".lock"))
            .collect();
        assert!(left.is_empty(), "leftovers: {left:?}");
    }

    fn egress_limit_and_network_events() {
        use std::io::{BufRead, BufReader};
        // 목적지: 본문 4000바이트로 답하는 HTTP 서버.
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
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: 4000\r\nConnection: close\r\n\r\n{}",
                        "x".repeat(4000)
                    )
                    .as_bytes(),
                );
            }
        });
        let e = Env::new();
        let allow = format!("127.0.0.1:{port}");
        let script = format!(
            "curl -s -x \"$HTTP_PROXY\" --noproxy '' http://127.0.0.1:{port}/ | wc -c; \
             curl -s -o /dev/null -w '%{{http_code}}' -x \"$HTTP_PROXY\" --noproxy '' http://127.0.0.1:{port}/"
        );
        let o = e.sbx(&[
            "run",
            "--rm",
            "--network",
            "allow",
            "--allow",
            &allow,
            "--egress-limit",
            "1k",
            IMAGE,
            "sh",
            "-c",
            &script,
        ]);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{out} {}", text(&o.stderr));
        let mut lines = out.lines();
        let got: usize = lines.next().unwrap().trim().parse().unwrap();
        assert!(got < 1024, "{out}");
        assert_eq!(lines.next(), Some("403"), "{out}");

        let o = e.sbx(&[
            "--format",
            "json",
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "--filter",
            "type=network",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let events: Vec<serde_json::Value> = text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let actions: Vec<&str> = events
            .iter()
            .map(|e| e["action"].as_str().unwrap())
            .collect();
        // 루프백 주소 규칙은 프록시로만 닿는다(시작할 때 한 번 기록한다).
        assert_eq!(
            actions,
            ["proxy_only", "connect", "close", "refuse"],
            "{events:?}"
        );
        let attrs = |i: usize| &events[i + 1]["attributes"];
        assert_eq!(attrs(0)["host"], "127.0.0.1");
        assert_eq!(attrs(0)["addr"], allow.as_str());
        assert_eq!(attrs(1)["limit"], "1024");
        assert!(
            attrs(1)["received"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
                <= 1024
        );
        assert!(
            attrs(2)["reason"]
                .as_str()
                .unwrap()
                .contains("egress limit")
        );
        // 나가는 연결이 없으면 상한은 받지 않는다.
        let o = e.sbx(&["run", "--rm", "--egress-limit", "1k", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("needs network access"),
            "{}",
            text(&o.stderr)
        );
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

    /// 실시간 공유가 없는 실행기에서는 `-v` 호스트 폴더만 오류이고 오류가 다른 길(그대로 붙여 쓸 수 있는 copy 마운트,
    /// 실시간 공유가 있는 실행기)을 알린다. 있는 실행기에서는 호스트에서 고친 파일을 게스트에 알린다(detached: 소유
    /// 프로세스는 shim). fake 게스트의 경로는 호스트 경로라 게스트의 알림 연산(수정 시각을 그대로 다시 쓴다)은 호스트
    /// 파일의 ctime만 바꾼다. 그것으로 알림이 닿았는지, 되도는 고리가 없는지 본다.
    fn live_share_tells_the_guest_or_points_elsewhere() {
        use std::os::unix::fs::MetadataExt;
        let e = Env::new();
        let host = e.path("src");
        std::fs::create_dir(&host).unwrap();
        std::fs::write(host.join("in.txt"), b"hi").unwrap();
        let guest = e.path("app");
        let v = format!("{}:{}", host.display(), guest.display());
        let without = |args: &[&str]| {
            e.cmd(args)
                .env("SBX_FAKE_LIVE_SHARE", "0")
                .output()
                .unwrap()
        };
        let o = without(&["run", "--rm", "-v", &v, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        let copy = format!(
            "--mount type=copy,source={},target={}",
            host.display(),
            guest.display()
        );
        let err = text(&o.stderr);
        assert!(err.contains(&copy) && err.contains("live share"), "{err}");
        assert!(e.ps().is_empty());
        // 알린 길은 그대로 된다. 공유가 없는 다른 것도 그대로다.
        let mut args: Vec<&str> = vec!["run", "--rm"];
        args.extend(copy.split_once(' ').map(|(a, b)| [a, b]).unwrap());
        let cat = format!("{}/in.txt", guest.display());
        args.extend([IMAGE, "cat", &cat]);
        let o = without(&args);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "hi");
        std::fs::remove_dir_all(&guest).unwrap();

        let o = e.sbx(&["run", "-d", "-v", &v, IMAGE, "sleep", "30"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        // 감시는 공유를 마운트한 뒤 열린다. FSEvents는 연 직후의 사건을 놓칠 수 있다.
        std::thread::sleep(Duration::from_millis(500));
        let f = host.join("main.js");
        let ctime = || {
            let m = std::fs::metadata(&f).unwrap();
            (m.ctime(), m.ctime_nsec())
        };
        std::fs::write(&f, b"1").unwrap();
        let written = ctime();
        let modified = std::fs::metadata(&f).unwrap().modified().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while ctime() == written {
            assert!(Instant::now() < deadline, "the guest was not told");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), modified);
        std::thread::sleep(Duration::from_millis(1500));
        let told = ctime();
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(ctime(), told, "the change bounced back and forth");
        assert_eq!(e.sbx(&["rm", "-f", &id]).status.code(), Some(0));
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

    /// `sbx tui`는 터미널이 아니면 화면을 열지 않고 오류다(파이프에 제어 문자를 쏟지 않는다). JSON도 받지 않는다.
    /// 화면 자체는 단위 테스트(`tui::model`, `tui::view`)가 본다.
    fn tui_needs_a_terminal() {
        let e = Env::new();
        let o = e.sbx(&["tui"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("needs a terminal"),
            "{}",
            text(&o.stderr)
        );
        assert!(o.stdout.is_empty(), "{:?}", text(&o.stdout));
        let o = e.sbx(&["--format", "json", "tui"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("interactive screen"),
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
        for cmd in ["run", "create", "rmi", "prepare", "pull", "push", "tag"] {
            has(&[cmd, ""], &image);
        }
        // tag는 둘째 인자(새 이름)도 이미지 참조에서 고쳐 쓴다.
        has(&["tag", &image, ""], &image);
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

    /// Docker 호환 표(`sbx_cli::compat`)가 되는 것이라고 적은 옵션 중 다른 테스트가 보지 않는 것들.
    /// `run`·`create`의 프로세스·자원·파일 옵션, `start -i`, `exec`의 옵션, `attach --no-stdin`, `rmi -f`, `--version`.
    fn docker_options_reach_the_sandbox() {
        let e = Env::new();
        let me = format!("{}:{}", id_output("-u"), id_output("-g"));
        // 가짜 실행기의 게스트 경로는 호스트 경로다.
        let work = e.path("work");
        std::fs::create_dir(&work).unwrap();
        let proj = e.path("proj");
        std::fs::create_dir(&proj).unwrap();
        std::fs::write(proj.join("in.txt"), b"copied in").unwrap();
        let target = e.path("guest-proj");

        // -w, -u, --entrypoint, --mount type=copy. 이미지에는 ENTRYPOINT가 없으므로 sh가 받는 -c는 --entrypoint 덕이다.
        let o = e.sbx(&[
            "run",
            "--rm",
            "-w",
            work.to_str().unwrap(),
            "-u",
            &me,
            "--entrypoint",
            "sh",
            "--mount",
            &format!(
                "type=copy,source={},target={}",
                proj.display(),
                target.display()
            ),
            IMAGE,
            "-c",
            &format!(
                "id -u; cat {}/in.txt; echo here > rel.txt",
                target.display()
            ),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), format!("{}\ncopied in", id_output("-u")));
        assert_eq!(std::fs::read(work.join("rel.txt")).unwrap(), b"here\n");
        // 빈 --entrypoint는 이미지의 것을 지운다(명령만 남는다).
        let o = e.sbx(&["run", "--rm", "--entrypoint", "", IMAGE, "echo", "plain"]);
        assert_eq!(text(&o.stdout), "plain\n", "{}", text(&o.stderr));

        // --cpus, -m, --storage-opt, --hostname, -l는 샌드박스 설정에 남는다.
        let o = e.sbx(&[
            "create",
            "--cpus",
            "2",
            "-m",
            "1g",
            "--storage-opt",
            "size=2g",
            "--hostname",
            "box",
            "-l",
            "team=a",
            IMAGE,
            "cat",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let id = text(&o.stdout).trim().to_owned();
        let o = e.sbx(&["--format", "json", "inspect", &id]);
        let s: sbx_cli::output::Sandbox = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            (
                s.resources.vcpus,
                s.resources.memory_mib,
                s.resources.scratch_disk_mib
            ),
            (2, 1024, 2048)
        );
        assert_eq!(s.hostname.as_deref(), Some("box"));
        assert_eq!(s.labels.get("team").map(String::as_str), Some("a"));
        // start -i: create에 -i가 없어도 이번 시작에 입력을 보낸다.
        let o = e.sbx_with_stdin(&["start", "-a", "-i", &id], b"typed");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "typed");

        // exec -e, --env-file, -w, -u.
        let o = e.sbx(&["run", "-d", "--name", "ex", IMAGE, "sleep", "30"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let env_file = e.path("exec.env");
        std::fs::write(&env_file, "B=from-file\n").unwrap();
        let o = e.sbx(&[
            "exec",
            "-e",
            "A=flag",
            "--env-file",
            env_file.to_str().unwrap(),
            "-w",
            work.to_str().unwrap(),
            "-u",
            &me,
            "ex",
            "sh",
            "-c",
            "echo $A $B; id -u; echo x > exec.txt",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(
            text(&o.stdout),
            format!("flag from-file\n{}\n", id_output("-u"))
        );
        assert!(work.join("exec.txt").exists());
        assert_eq!(e.sbx(&["rm", "-f", "ex"]).status.code(), Some(0));

        // attach --no-stdin: 그 쪽의 입력은 가지 않는다. 다른 쪽이 붙어 보낸 것만 간다.
        let o = e.sbx(&[
            "run",
            "-d",
            "-i",
            "--name",
            "at",
            IMAGE,
            "sh",
            "-c",
            "read a; echo got $a",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let mut quiet = e
            .cmd(&["attach", "--no-stdin", "at"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        quiet.stdin.take().unwrap().write_all(b"ignored\n").unwrap();
        let reader = Reader::start(&mut quiet);
        // 먼저 붙은 쪽이 보냈다면 이미 끝났을 시간을 준다.
        std::thread::sleep(Duration::from_millis(300));
        let other = e.sbx_with_stdin(&["attach", "at"], b"sent\n");
        assert_eq!(other.status.code(), Some(0), "{}", text(&other.stderr));
        let quiet = wait_with_timeout(quiet, Duration::from_secs(20));
        assert_eq!(quiet.status.code(), Some(0), "{}", text(&quiet.stderr));
        assert_eq!(reader.text(), "got sent\n");
        assert_eq!(e.sbx(&["rm", "at"]).status.code(), Some(0));

        // rmi: 멈춘 샌드박스가 쓰는 이미지는 -f가 있어야 지운다.
        let o = e.sbx(&["rmi", IMAGE]);
        assert_ne!(o.status.code(), Some(0), "{}", text(&o.stdout));
        let o = e.sbx(&["rmi", "-f", IMAGE]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&e.sbx(&["images", "-q"]).stdout), "");

        // docker version 대신 sbx --version.
        let o = e.sbx(&["--version"]);
        assert_eq!(
            text(&o.stdout),
            format!("sbx {}\n", env!("CARGO_PKG_VERSION"))
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

        // -d: 곧바로 돌아오고, 명령은 sbx가 끝난 뒤에도 돈다. 출력은 버린다.
        let mark = e.path("detached-done");
        let script = format!("sleep 0.3; echo lost; touch {}", mark.display());
        let o = e.sbx(&["exec", "-d", &id, "sh", "-c", &script]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !mark.exists() {
            assert!(
                Instant::now() < deadline,
                "the detached command did not run"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            e.sbx(&["exec", "-d", &id, "no-such-command-xyz"])
                .status
                .code(),
            Some(127)
        );
        assert_eq!(
            e.sbx(&["exec", "-d", "-t", &id, "true"]).status.code(),
            Some(2)
        );
        // --timeout: 넘으면 죽이고 124. 일찍 끝나면 그 코드.
        let o = e.sbx(&[
            "exec",
            "--timeout",
            "1",
            &id,
            "sh",
            "-c",
            "echo start; sleep 30",
        ]);
        assert_eq!(o.status.code(), Some(124), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "start\n");
        assert!(text(&o.stderr).contains("--timeout"), "{}", text(&o.stderr));
        let o = e.sbx(&["exec", "--timeout", "30", &id, "sh", "-c", "exit 3"]);
        assert_eq!(o.status.code(), Some(3), "{}", text(&o.stderr));

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

        // 0.1.x가 쓴 판 1 로그(머리 없음)는 읽지 않고 이유를 알린다.
        let log = e.path("data/sandboxes").join(&id).join("logs/main.log");
        std::fs::write(&log, [1, 0, 0, 0, 4, b'o', b'l', b'd', b'\n']).unwrap();
        let o = e.sbx(&["logs", &id]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("not an sbx log"),
            "{}",
            text(&o.stderr)
        );

        // -f --until: 그 시각이 지나면 따라가기를 끝낸다. stdout이 닫혀도 조용히 끝난다.
        let o = e.sbx(&[
            "run",
            "-d",
            IMAGE,
            "sh",
            "-c",
            "while kill -0 $PPID 2>/dev/null; do echo y; sleep 0.05; done",
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

    /// 셸이 시그널을 기다리는 곳: 1초짜리 `sleep`을 기다린다(`wait`는 트랩한 시그널에 곧바로 돌아와 트랩을 돈다).
    /// 바쁜 루프가 아니다(초당 fork 한 번). 부모(게스트 처리)가 사라지면 스스로 끝나므로 무엇이 새도 1초 뒤에는 없다.
    const BLOCK: &str =
        "while kill -0 $PPID 2>/dev/null; do sleep 1 >/dev/null 2>&1 & wait $!; done";

    /// 시그널 `sig`를 받으면 `code`로 끝나고 TERM은 무시하는 기본 프로세스를 detached로 시작한다.
    /// 트랩을 건 뒤("ready"가 로그에 나온 뒤) ID를 돌려준다.
    fn run_trapping(e: &Env, extra: &[&str], sig: &str, code: i32) -> String {
        let script = format!("trap '' TERM; trap 'exit {code}' {sig}; echo ready; {BLOCK}");
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

    /// Basic 인증만 받는 레지스트리 흉내. `me:pw`만 통과한다.
    fn basic_registry() -> u16 {
        use std::io::{BufRead, BufReader};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { return };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut ok = false;
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    // base64("me:pw")
                    ok |= h
                        .trim()
                        .eq_ignore_ascii_case("authorization: basic bWU6cHc=");
                }
                let reply = if ok {
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"r\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = c.write_all(reply.as_bytes());
            }
        });
        port
    }

    fn published_ports_reach_the_sandbox() {
        use std::io::Read;
        let e = Env::new();
        // 가짜 실행기의 게스트는 호스트에서 돈다: 게스트 안 서비스 흉내를 호스트 포트에 띄운다.
        let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let guest_port = service.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for c in service.incoming() {
                let Ok(mut c) = c else { return };
                let _ = c.write_all(b"hi from inside\n");
            }
        });
        let o = e.sbx(&[
            "run",
            "-d",
            "--name",
            "web",
            "-p",
            &guest_port.to_string(),
            IMAGE,
            "sleep",
            "30",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["--format", "json", "inspect", "web"]);
        let s: sbx_cli::output::Sandbox = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(s.ports.len(), 1, "{:?}", s.ports);
        let (host, guest) = s.ports[0].split_once("->").unwrap();
        assert_eq!(guest, format!("{guest_port}/tcp"));
        assert!(
            host.starts_with("127.0.0.1:") && !host.ends_with(":0"),
            "{host}"
        );
        let mut c = std::net::TcpStream::connect(host).unwrap();
        let mut got = String::new();
        c.read_to_string(&mut got).unwrap();
        assert_eq!(got, "hi from inside\n");
        assert_eq!(e.sbx(&["rm", "-f", "web"]).status.code(), Some(0));

        // 잘못된 매핑과 UDP는 만들기 전에 거부한다.
        for bad in ["80/udp", "x:80", "8080:0"] {
            let o = e.sbx(&["create", "-p", bad, IMAGE, "true"]);
            assert_eq!(o.status.code(), Some(2), "{bad}: {}", text(&o.stderr));
        }
    }

    fn stats_reports_guest_usage() {
        let e = Env::new();
        // 실행 중인 것이 없으면 머리줄만.
        let o = e.sbx(&["stats", "--no-stream"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout).lines().count(), 1);
        let o = e.sbx(&["run", "-d", "--name", "st", IMAGE, "sleep", "30"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["--format", "json", "stats", "st"]);
        // 가짜 실행기의 게스트는 호스트에서 돈다: Linux면 호스트의 /proc를 읽고, 아니면 /proc가 없다.
        if cfg!(target_os = "linux") {
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            let rows: Vec<sbx_cli::output::Stats> = serde_json::from_slice(&o.stdout).unwrap();
            assert_eq!(rows.len(), 1);
            let r = &rows[0];
            assert_eq!(r.name.as_deref(), Some("st"));
            assert!(
                r.cpus >= 1 && r.memory_total > 0 && r.memory_used <= r.memory_total,
                "{r:?}"
            );
            assert!(r.pids > 0 && r.cpu_percent >= 0.0, "{r:?}");
        } else {
            assert_ne!(o.status.code(), Some(0));
            assert!(text(&o.stderr).contains("usage"), "{}", text(&o.stderr));
        }
        assert_eq!(e.sbx(&["rm", "-f", "st"]).status.code(), Some(0));
        // 멈춘 샌드박스를 고르면 오류다.
        let o = e.sbx(&["create", "--name", "idle", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_ne!(
            e.sbx(&["stats", "--no-stream", "idle"]).status.code(),
            Some(0)
        );
    }

    /// `docker save`(Docker 25 이후)가 만드는 모양의 OCI layout tar: 레이어 하나, 이름 하나.
    fn oci_archive(name: &str) -> (Vec<u8>, String) {
        oci_archive_layers(name, &[&[("hello.txt", b"hello")]])
    }

    /// 층들(아래부터, 층마다 (경로, 내용) 파일들)로 만든 OCI layout tar와 이미지 ID.
    fn oci_archive_layers(name: &str, layers: &[&[(&str, &[u8])]]) -> (Vec<u8>, String) {
        use sha2::Digest as _;
        let hex = |b: &[u8]| {
            sha2::Sha256::digest(b)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect::<String>()
        };
        let mut tars = Vec::new();
        let mut blobs = Vec::new();
        for files in layers {
            let mut layer_tar = tar::Builder::new(Vec::new());
            for (path, data) in files.iter() {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_uid(0);
                h.set_gid(0);
                h.set_mtime(0);
                h.set_cksum();
                layer_tar.append_data(&mut h, path, *data).unwrap();
            }
            let layer_tar = layer_tar.into_inner().unwrap();
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(&layer_tar).unwrap();
            blobs.push(gz.finish().unwrap());
            tars.push(layer_tar);
        }
        let arch = Arch::host().unwrap().oci_name();
        let diff_ids: Vec<String> = tars
            .iter()
            .map(|t| format!("\"sha256:{}\"", hex(t)))
            .collect();
        let config = format!(
            r#"{{"architecture":"{arch}","os":"linux","config":{{"Cmd":["sh"],"User":"{}:{}","Labels":{{"name":"{name}"}}}},"rootfs":{{"type":"layers","diff_ids":[{}]}}}}"#,
            id_output("-u"),
            id_output("-g"),
            diff_ids.join(",")
        );
        let descs: Vec<String> = blobs
            .iter()
            .map(|b| {
                format!(
                    r#"{{"mediaType":"application/vnd.oci.image.layer.v1.tar+gzip","digest":"sha256:{}","size":{}}}"#,
                    hex(b),
                    b.len()
                )
            })
            .collect();
        let manifest = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json","config":{{"mediaType":"application/vnd.oci.image.config.v1+json","digest":"sha256:{}","size":{}}},"layers":[{}]}}"#,
            hex(config.as_bytes()),
            config.len(),
            descs.join(",")
        );
        let index = format!(
            r#"{{"schemaVersion":2,"manifests":[{{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:{}","size":{},"annotations":{{"io.containerd.image.name":"{name}","org.opencontainers.image.ref.name":"latest"}}}}]}}"#,
            hex(manifest.as_bytes()),
            manifest.len()
        );
        let mut t = tar::Builder::new(Vec::new());
        let mut add = |path: String, data: &[u8]| {
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            t.append_data(&mut h, path, data).unwrap();
        };
        // Docker가 함께 넣는 것(건너뛴다).
        add("manifest.json".into(), b"[]");
        add("oci-layout".into(), br#"{"imageLayoutVersion":"1.0.0"}"#);
        add("index.json".into(), index.as_bytes());
        for b in [config.as_bytes(), manifest.as_bytes()]
            .into_iter()
            .chain(blobs.iter().map(Vec::as_slice))
        {
            add(format!("blobs/sha256/{}", hex(b)), b);
        }
        (
            t.into_inner().unwrap(),
            format!("sha256:{}", hex(manifest.as_bytes())),
        )
    }

    const BASE: &[(&str, &[u8])] = &[("base.bin", &[7u8; 64 * 1024])];

    /// 아카이브를 들인다.
    fn load(e: &Env, file: &str, archive: &[u8]) {
        std::fs::write(e.path(file), archive).unwrap();
        let o = e.sbx(&["load", "-i", e.path(file).to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    }

    /// `run --rm IMAGE true`를 하고 실행기가 받은 VM 설정을 돌려준다.
    fn run_recorded(e: &Env, image: &str, max_disks: Option<&str>) -> serde_json::Value {
        let rec = e.path("launch.json");
        let _ = std::fs::remove_file(&rec);
        let mut c = e.cmd(&["run", "--rm", image, "true"]);
        c.env("SBX_FAKE_RECORD", &rec);
        if let Some(n) = max_disks {
            c.env("SBX_FAKE_MAX_DISKS", n);
        }
        let o = c.output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        serde_json::from_slice(&std::fs::read(&rec).unwrap()).unwrap()
    }

    fn entries(dir: &Path) -> usize {
        std::fs::read_dir(dir).map_or(0, |d| d.count())
    }

    /// 같은 바탕 층을 쓰는 이미지 둘: 바탕 층 디스크는 하나이고(`system df`가 한 번 센다), 한 이미지를 지워도 남고, 둘을
    /// 지우면 지워진다. VM에는 층마다 디스크 하나가 붙는다(맨 아래 층, scratch, 위 층).
    fn images_on_one_base_store_it_once() {
        use sbx_cli::output::{DiskUsage, Image};
        let e = Env::new();
        let (a, _) = oci_archive_layers("local/a:1", &[BASE, &[("a.txt", b"a")]]);
        let (b, _) = oci_archive_layers("local/b:1", &[BASE, &[("b.txt", b"b")]]);
        load(&e, "a.tar", &a);
        load(&e, "b.tar", &b);
        let vm = run_recorded(&e, "local/a:1", None);
        let disks = vm["disks"].as_array().unwrap();
        let roles: Vec<&str> = disks.iter().map(|d| d["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["rootfs", "scratch", "rootfs"], "{vm}");
        assert!(
            disks[0]["path"].as_str().unwrap().contains("/layers/"),
            "{vm}"
        );
        run_recorded(&e, "local/b:1", None);
        let layers = e.path("cache/layers");
        assert_eq!(entries(&layers), 3, "base, a, b");
        // 합친 것은 층 없는 기본 이미지(`IMAGE`)의 빈 rootfs 하나뿐이다.
        assert_eq!(entries(&e.path("cache/merged")), 1);

        let o = e.sbx(&["--format", "json", "images"]);
        let list: Vec<Image> = serde_json::from_slice(&o.stdout).unwrap();
        let mine: Vec<&Image> = list
            .iter()
            .filter(|i| i.references.iter().any(|r| r.contains("local/")))
            .collect();
        assert_eq!(mine.len(), 2);
        for i in &mine {
            assert!(i.shared_size >= 64 * 1024, "{i:?}");
            assert_eq!(i.disk_size, i.shared_size + i.unique_size, "{i:?}");
        }
        let o = e.sbx(&["--format", "json", "system", "df"]);
        let rows: Vec<DiskUsage> = serde_json::from_slice(&o.stdout).unwrap();
        let im = rows.iter().find(|r| r.kind == "images").unwrap();
        // 바탕은 한 번: 전체는 두 이미지 크기의 합에서 함께 쓰는 것 하나를 뺀 것이다.
        let sum: u64 = mine.iter().map(|i| i.disk_size).sum();
        assert_eq!(
            im.size + mine[0].shared_size,
            sum + e2e_fake_size(&list),
            "{rows:?} {mine:?}"
        );
        assert_eq!(im.shared, mine[0].shared_size, "{rows:?}");
        let o = e.sbx(&["images"]);
        assert!(text(&o.stdout).contains("SHARED"), "{}", text(&o.stdout));

        let o = e.sbx(&["rmi", "local/a:1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(entries(&layers), 2, "base and b stay");
        let vm = run_recorded(&e, "local/b:1", None);
        assert_eq!(vm["disks"].as_array().unwrap().len(), 3);
        let o = e.sbx(&["rmi", "local/b:1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(entries(&layers), 0);
    }

    /// 기본 이미지(`IMAGE`, 층 없음)가 차지하는 것.
    fn e2e_fake_size(list: &[sbx_cli::output::Image]) -> u64 {
        list.iter()
            .filter(|i| !i.references.iter().any(|r| r.contains("local/")))
            .map(|i| i.disk_size)
            .sum()
    }

    /// 실행기가 붙일 수 있는 디스크보다 층이 많으면 아래 층들을 합친 디스크 하나로 띄운다. 디스크가 넉넉한 실행기로
    /// 다시 띄우면 층마다 디스크를 만들어 쓴다.
    fn more_layers_than_disks_run_through_a_merged_disk() {
        let e = Env::new();
        let (a, _) = oci_archive_layers(
            "local/deep:1",
            &[
                &[("0", b"0")],
                &[("1", b"1")],
                &[("2", b"2")],
                &[("3", b"3")],
            ],
        );
        load(&e, "deep.tar", &a);
        let vm = run_recorded(&e, "local/deep:1", Some("3"));
        let disks = vm["disks"].as_array().unwrap();
        assert_eq!(disks.len(), 3, "{vm}");
        assert!(
            disks[0]["path"].as_str().unwrap().contains("/merged/"),
            "{vm}"
        );
        assert!(
            disks[2]["path"].as_str().unwrap().contains("/layers/"),
            "{vm}"
        );
        assert_eq!(entries(&e.path("cache/layers")), 1, "only the top layer");
        // 디스크 둘이면(지금까지와 같이) 모두 합친 것 하나와 scratch.
        let vm = run_recorded(&e, "local/deep:1", Some("2"));
        let roles: Vec<&str> = vm["disks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, ["rootfs", "scratch"]);
        let vm = run_recorded(&e, "local/deep:1", None);
        assert_eq!(vm["disks"].as_array().unwrap().len(), 5, "{vm}");
        // 합친 것은 캐시다. prune이 쓰지 않는 것을 지우지는 않지만 이미지를 지우면 함께 지워진다.
        let o = e.sbx(&["rmi", "local/deep:1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(
            entries(&e.path("cache/merged")),
            1,
            "the empty rootfs of IMAGE"
        );
        assert_eq!(entries(&e.path("cache/layers")), 0);
    }

    /// 0.2(배치 1)가 쓴 cache_dir: 이미지별 rootfs를 지우고, 다음 `run`이 받아 둔 blob에서 층 디스크를 만든다.
    /// 그 전에 만들어 둔 샌드박스도 `start`가 그렇게 띄운다.
    fn a_layout_1_cache_is_migrated_on_first_use() {
        let e = Env::new();
        let (a, id) = oci_archive_layers("local/old:1", &[BASE, &[("x", b"x")]]);
        load(&e, "old.tar", &a);
        let o = e.sbx(&["create", "--name", "kept", "local/old:1", "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // 0.2가 남겼을 모습으로 되돌린다.
        let cache = e.path("cache");
        std::fs::remove_dir_all(cache.join("layers")).unwrap();
        let old = cache
            .join("images")
            .join(id.strip_prefix("sha256:").unwrap())
            .join("rootfs.erofs");
        std::fs::write(&old, b"merged by 0.2").unwrap();
        std::fs::write(cache.join("layout-version"), "1\n").unwrap();

        let o = e.sbx(&["start", "-a", "kept"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!old.exists());
        assert_eq!(
            std::fs::read_to_string(cache.join("layout-version")).unwrap(),
            "2\n"
        );
        assert_eq!(entries(&cache.join("layers")), 2);
        run_recorded(&e, "local/old:1", None);
    }

    fn load_and_save_round_trip_an_oci_archive() {
        let e = Env::new();
        let (archive, id) = oci_archive("example.com/team/tool:1");
        std::fs::write(e.path("in.tar"), &archive).unwrap();

        let o = e.sbx(&[
            "--format",
            "json",
            "load",
            "-i",
            e.path("in.tar").to_str().unwrap(),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v[0]["reference"], "example.com/team/tool:1");
        assert_eq!(v[0]["id"], id.as_str());

        // 표준 입력으로도, --tag로 다른 이름을.
        let o = e.sbx_with_stdin(&["load", "--tag", "local/tool:copy"], &archive);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stdout).contains("docker.io/local/tool:copy: loaded"),
            "{}",
            text(&o.stdout)
        );

        // 내보내고 지우고 다시 들이면 같은 ID다.
        let out = e.path("out.tar");
        let o = e.sbx(&[
            "save",
            "-o",
            out.to_str().unwrap(),
            "example.com/team/tool:1",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(o.stdout.is_empty());
        // docker load가 읽는 manifest.json도 있다.
        let mut t = tar::Archive::new(std::fs::File::open(&out).unwrap());
        let docker: serde_json::Value = t
            .entries()
            .unwrap()
            .map(Result::unwrap)
            .find(|e| e.path().unwrap().to_str() == Some("manifest.json"))
            .map(|e| serde_json::from_reader(e).unwrap())
            .unwrap();
        assert_eq!(docker[0]["RepoTags"][0], "example.com/team/tool:1");
        let o = e.sbx(&["rmi", "example.com/team/tool:1", "local/tool:copy"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["--format", "json", "load", "-i", out.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v[0]["reference"], "example.com/team/tool:1");
        assert_eq!(v[0]["id"], id.as_str());
        // 표준 출력으로도(터미널이 아니면).
        let o = e.sbx(&["save", "example.com/team/tool:1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(std::fs::read(&out).unwrap(), o.stdout);

        // 받은 문서가 없는 이미지(0.2.1 전에 받은 것)는 다시 받으라고 한다.
        let o = e.sbx(&["save", "-o", e.path("old.tar").to_str().unwrap(), IMAGE]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("pull it again"),
            "{}",
            text(&o.stderr)
        );
        assert!(!e.path("old.tar").exists());

        // 망가진 blob은 들이지 않는다.
        let mut bad = archive.clone();
        let at = bad.windows(5).position(|w| w == b"Cmd\":").unwrap();
        bad[at] ^= 1;
        std::fs::write(e.path("bad.tar"), &bad).unwrap();
        let o = e.sbx(&["load", "-i", e.path("bad.tar").to_str().unwrap()]);
        assert_ne!(o.status.code(), Some(0));
        // tar가 아니면 OCI layout이 아니라고 한다.
        std::fs::write(e.path("junk.tar"), b"not a tar").unwrap();
        let o = e.sbx(&["load", "-i", e.path("junk.tar").to_str().unwrap()]);
        assert_ne!(o.status.code(), Some(0));
        // 작업 공간에 남은 것이 없다.
        let tmp = e.path("cache/tmp");
        assert!(
            std::fs::read_dir(&tmp).map_or(true, |d| d.count() == 0),
            "{:?}",
            std::fs::read_dir(&tmp).map(|d| d.collect::<Vec<_>>())
        );
    }

    fn policy_json_is_followed() {
        let e = Env::new();
        let (archive, _) = oci_archive("example.com/team/tool:1");
        std::fs::write(e.path("in.tar"), &archive).unwrap();
        let policy = e.path("home/.config/containers/policy.json");
        std::fs::create_dir_all(policy.parent().unwrap()).unwrap();
        std::fs::write(&policy, r#"{"default":[{"type":"reject"}]}"#).unwrap();
        // 거절은 레지스트리에 가기 전에(닫힌 포트라 가면 다른 오류다).
        let o = e.sbx(&["pull", "127.0.0.1:9/a:1"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("rejected by"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["load", "-i", e.path("in.tar").to_str().unwrap()]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("rejected by"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["images", "-q"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // archive만 받는 정책. 서명을 요구하는 레지스트리 정책은 서명이 없으면 거절이다.
        std::fs::write(
            &policy,
            r#"{"default":[{"type":"reject"}],"transports":{
                "oci-archive":{"":[{"type":"insecureAcceptAnything"}]},
                "docker":{"127.0.0.1:9":[{"type":"signedBy","keyType":"GPGKeys","keyPath":"/k"}]}}}"#,
        )
        .unwrap();
        let o = e.sbx(&["load", "-i", e.path("in.tar").to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["pull", "127.0.0.1:9/a:1"]);
        assert!(text(&o.stderr).contains("GPG"), "{}", text(&o.stderr));
    }

    fn registries_conf_is_read_for_pulls() {
        let e = Env::new();
        let (archive, _) = oci_archive("example.com/team/tool:1");
        // 미러가 없고 주 위치는 닫힌 포트: 차단이 먼저, 그다음 바꾼 곳으로 간다.
        std::fs::write(
            e.path("registries.conf"),
            "[[registry]]\nlocation = \"blocked.invalid\"\nblocked = true\n\
             [[registry]]\nprefix = \"example.invalid\"\nlocation = \"127.0.0.1:9/elsewhere\"\n",
        )
        .unwrap();
        let o = e.sbx(&["pull", "blocked.invalid/a:1"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("blocked by registries.conf"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["pull", "example.invalid/a:1"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("127.0.0.1:9/v2/elsewhere/a"),
            "{}",
            text(&o.stderr)
        );
        // 사용자 drop-in도 읽는다. 틀린 설정은 넘기지 않고 오류다.
        let d = e.path("home/.config/containers/registries.conf.d");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("x.conf"), "[[registry]]\nprefix = \"a.example\"\n").unwrap();
        let o = e.sbx(&["pull", "alpine"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("x.conf"), "{}", text(&o.stderr));
        let o = e.sbx(&["images", "-q"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // load는 레지스트리를 쓰지 않는다.
        std::fs::write(e.path("in.tar"), &archive).unwrap();
        let o = e.sbx(&["load", "-i", e.path("in.tar").to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    }

    /// 인증 없는 OCI 레지스트리 흉내(메모리): blob `HEAD`·`GET`, 올리기(`POST`, mount, 한 번에 `PUT`), manifest
    /// `PUT`·`GET`. 받은 요청을 `메서드 경로?쿼리`로 남긴다.
    struct Registry {
        port: u16,
        log: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        manifests: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>>,
    }

    fn registry() -> Registry {
        use std::collections::HashMap;
        use std::io::{BufRead, BufReader, Read};
        use std::sync::{Arc, Mutex};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let manifests: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        // (저장소, digest) → 내용. 올리는 중인 것은 id → 저장소.
        type Blobs = Mutex<HashMap<(String, String), Vec<u8>>>;
        let blobs: Arc<Blobs> = Arc::default();
        let uploads: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let (log2, man2) = (log.clone(), manifests.clone());
        std::thread::spawn(move || {
            for (n, c) in l.incoming().enumerate() {
                let Ok(mut c) = c else { return };
                let mut r = BufReader::new(c.try_clone().unwrap());
                let mut first = String::new();
                if r.read_line(&mut first).is_err() {
                    continue;
                }
                let mut len = 0usize;
                let mut ctype = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    if let Some((k, v)) = h.split_once(':') {
                        match k.trim().to_ascii_lowercase().as_str() {
                            "content-length" => len = v.trim().parse().unwrap_or(0),
                            "content-type" => ctype = v.trim().to_owned(),
                            _ => {}
                        }
                    }
                }
                let mut body = vec![0; len];
                if r.read_exact(&mut body).is_err() {
                    continue;
                }
                let mut parts = first.split_whitespace();
                let (method, target) = (
                    parts.next().unwrap_or("").to_owned(),
                    parts.next().unwrap_or("").to_owned(),
                );
                log2.lock().unwrap().push(format!("{method} {target}"));
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let q = |key: &str| {
                    query.split('&').find_map(|kv| {
                        let (k, v) = kv.split_once('=')?;
                        (k == key).then(|| v.replace("%3A", ":").replace("%2F", "/"))
                    })
                };
                let send = |c: &mut std::net::TcpStream,
                            status: &str,
                            headers: &str,
                            body: &[u8]| {
                    let head = format!(
                        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = c.write_all(head.as_bytes());
                    if method != "HEAD" {
                        let _ = c.write_all(body);
                    }
                };
                let rest = path.strip_prefix("/v2/").unwrap_or("");
                let hex = |b: &[u8]| {
                    use sha2::Digest as _;
                    sha2::Sha256::digest(b)
                        .iter()
                        .map(|x| format!("{x:02x}"))
                        .collect::<String>()
                };
                if let Some((repo, id)) = rest.split_once("/blobs/uploads/") {
                    if method == "POST" {
                        if let (Some(d), Some(from)) = (q("mount"), q("from")) {
                            let have = blobs.lock().unwrap().get(&(from, d.clone())).cloned();
                            if let Some(b) = have {
                                blobs.lock().unwrap().insert((repo.to_owned(), d), b);
                                send(&mut c, "201 Created", "", b"");
                                continue;
                            }
                        }
                        uploads
                            .lock()
                            .unwrap()
                            .insert(n.to_string(), repo.to_owned());
                        let loc = format!("Location: /v2/{repo}/blobs/uploads/{n}?_state=x\r\n");
                        send(&mut c, "202 Accepted", &loc, b"");
                    } else if method == "PUT" {
                        let d = q("digest").unwrap_or_default();
                        if uploads.lock().unwrap().remove(id).is_none()
                            || format!("sha256:{}", hex(&body)) != d
                        {
                            send(
                                &mut c,
                                "400 Bad Request",
                                "",
                                br#"{"errors":[{"code":"DIGEST_INVALID"}]}"#,
                            );
                            continue;
                        }
                        blobs.lock().unwrap().insert((repo.to_owned(), d), body);
                        send(&mut c, "201 Created", "", b"");
                    } else {
                        uploads.lock().unwrap().remove(id);
                        send(&mut c, "204 No Content", "", b"");
                    }
                } else if let Some((repo, d)) = rest.split_once("/blobs/") {
                    match blobs.lock().unwrap().get(&(repo.to_owned(), d.to_owned())) {
                        Some(b) => send(&mut c, "200 OK", "", b),
                        None => send(&mut c, "404 Not Found", "", b""),
                    }
                } else if let Some((repo, tag)) = rest.split_once("/manifests/") {
                    let key = format!("{repo}:{tag}");
                    if method == "PUT" {
                        let d = format!("sha256:{}", hex(&body));
                        let mut m = man2.lock().unwrap();
                        m.insert(format!("{repo}:{d}"), body.clone());
                        m.insert(key, body);
                        let _ = ctype;
                        send(
                            &mut c,
                            "201 Created",
                            &format!("Docker-Content-Digest: {d}\r\n"),
                            b"",
                        );
                    } else {
                        match man2.lock().unwrap().get(&key) {
                            Some(b) => send(
                                &mut c,
                                "200 OK",
                                "Content-Type: application/vnd.oci.image.manifest.v1+json\r\n",
                                b,
                            ),
                            None => send(&mut c, "404 Not Found", "", b""),
                        }
                    }
                } else {
                    send(&mut c, "404 Not Found", "", b"");
                }
            }
        });
        Registry {
            port,
            log,
            manifests,
        }
    }

    fn tag_and_push_to_a_registry() {
        let e = Env::new();
        let reg = registry();
        let host = format!("127.0.0.1:{}", reg.port);
        let (archive, id) = oci_archive("example.com/team/tool:1");
        std::fs::write(e.path("in.tar"), &archive).unwrap();
        let o = e.sbx(&["load", "-i", e.path("in.tar").to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

        // tag: 출력 없음(Docker와 같다). 참조는 정규화한다. JSON이면 Tagged.
        let target = format!("{host}/team/tool:2");
        let o = e.sbx(&["tag", "example.com/team/tool:1", &target]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(o.stdout.is_empty(), "{}", text(&o.stdout));
        let o = e.sbx(&["--format", "json", "tag", &id[7..19], "tool:copy"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let t: sbx_cli::output::Tagged = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            (t.reference.as_str(), t.id.as_str()),
            ("docker.io/library/tool:copy", id.as_str())
        );
        let o = e.sbx(&["--format", "json", "images"]);
        let images: Vec<sbx_cli::output::Image> = serde_json::from_slice(&o.stdout).unwrap();
        let img = images.iter().find(|i| i.id == id).unwrap();
        assert!(img.references.contains(&target), "{img:?}");
        // 없는 원본, digest 대상은 거부한다.
        let o = e.sbx(&["tag", "nope:1", "x:1"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("no such image"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["tag", "tool:copy", &format!("x@sha256:{}", "a".repeat(64))]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("digest"), "{}", text(&o.stderr));

        // push: Docker 같은 줄. 받은 manifest 그대로라 레지스트리의 digest가 이미지 ID다.
        let o = e.sbx(&["push", &target]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let out = text(&o.stdout);
        assert!(
            out.starts_with(&format!(
                "The push refers to repository [{host}/team/tool]\n"
            )),
            "{out}"
        );
        assert!(out.contains(": Pushed\n"), "{out}");
        assert!(out.contains(&format!("2: digest: {id} size: ")), "{out}");
        let stored = reg
            .manifests
            .lock()
            .unwrap()
            .get("team/tool:2")
            .cloned()
            .unwrap();
        {
            use sha2::Digest as _;
            let hex: String = sha2::Sha256::digest(&stored)
                .iter()
                .map(|x| format!("{x:02x}"))
                .collect();
            assert_eq!(format!("sha256:{hex}"), id);
        }
        // 다시 올리면 보내지 않는다.
        let o = e.sbx(&["push", &target]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stdout).contains(": Layer already exists\n"),
            "{}",
            text(&o.stdout)
        );
        // -q는 참조만, JSON은 Pushed 한 줄.
        let o = e.sbx(&["push", "-q", &target]);
        assert_eq!(text(&o.stdout), format!("{target}\n"));
        let o = e.sbx(&["--format", "json", "push", "-q", &target]);
        assert_eq!(text(&o.stdout), format!("{target}\n"));
        let o = e.sbx(&["--format", "json", "push", &target]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let p: sbx_cli::output::Pushed = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            (p.reference.as_str(), p.id.as_str()),
            (target.as_str(), id.as_str())
        );
        assert_eq!(p.blobs.len(), 2, "{p:?}");
        assert!(p.blobs.iter().all(|b| b.status == "exists"), "{p:?}");

        // 다른 sbx(다른 캐시)가 그 레지스트리에서 받으면 같은 ID다.
        let other = Env::new();
        let o = other.sbx(&["--format", "json", "pull", &target]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let pulled: sbx_cli::output::Pulled = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(pulled.id, id);
        // 같은 레지스트리의 다른 저장소로는 받아 온 저장소에서 끌어온다(mount).
        let copy = format!("{host}/copies/tool:1");
        assert_eq!(other.sbx(&["tag", &target, &copy]).status.code(), Some(0));
        let o = other.sbx(&["push", &copy]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stdout).contains(": Mounted from team/tool\n"),
            "{}",
            text(&o.stdout)
        );
        assert!(
            reg.log
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("from=team")),
            "{:?}",
            reg.log.lock().unwrap()
        );

        // -a: 그 저장소의 로컬 태그를 모두. 태그를 함께 주면 거부한다.
        let three = format!("{host}/team/tool:3");
        assert_eq!(e.sbx(&["tag", &target, &three]).status.code(), Some(0));
        let o = e.sbx(&["push", "-a", "-q", &format!("{host}/team/tool")]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), format!("{target}\n{three}\n"));
        assert!(reg.manifests.lock().unwrap().contains_key("team/tool:3"));
        let o = e.sbx(&["push", "-a", &target]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("--all-tags"),
            "{}",
            text(&o.stderr)
        );
        // 로컬에 없는 태그, ID로는 올리지 않는다.
        let o = e.sbx(&["push", &format!("{host}/team/tool:9")]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("sbx tag"), "{}", text(&o.stderr));

        // registries.conf의 차단: 레지스트리에 가지 않는다. 미러는 push에 쓰지 않는다.
        std::fs::write(
            e.path("registries.conf"),
            format!(
                "[[registry]]\nprefix = \"{host}/blocked\"\nlocation = \"{host}/blocked\"\nblocked = true\n\
                 [[registry]]\nprefix = \"{host}/team\"\nlocation = \"{host}/team\"\n\
                 [[registry.mirror]]\nlocation = \"127.0.0.1:9/mirror\"\n"
            ),
        )
        .unwrap();
        let blocked = format!("{host}/blocked/tool:1");
        assert_eq!(e.sbx(&["tag", &target, &blocked]).status.code(), Some(0));
        let before = reg.log.lock().unwrap().len();
        let o = e.sbx(&["--format", "json", "push", &blocked]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("blocked by registries.conf"),
            "{}",
            text(&o.stderr)
        );
        assert_eq!(reg.log.lock().unwrap().len(), before);
        let o = e.sbx(&["push", "-q", &target]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

        // 이벤트: tag와 push.
        let o = e.sbx(&[
            "events", "--since", "1h", "--until", "0s", "--format", "json",
        ]);
        let actions: Vec<(String, String)> = text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .filter(|v| v["type"] == "image")
            .map(|v| {
                (
                    v["action"].as_str().unwrap().to_owned(),
                    v["attributes"]["reference"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                )
            })
            .collect();
        assert!(
            actions.contains(&("tag".into(), target.clone())),
            "{actions:?}"
        );
        assert!(
            actions.contains(&("push".into(), target.clone())),
            "{actions:?}"
        );
    }

    fn login_and_logout_use_dockers_config() {
        let e = Env::new();
        let registry = format!("127.0.0.1:{}", basic_registry());
        let config = e.path("docker/config.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, r#"{"proxies":{"default":{"noProxy":"x"}}}"#).unwrap();

        // 틀린 비밀번호는 쓰지 않는다.
        let o = e.sbx_with_stdin(
            &["login", "-u", "me", "--password-stdin", &registry],
            b"no\n",
        );
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("refused"), "{}", text(&o.stderr));
        assert!(!std::fs::read_to_string(&config).unwrap().contains("auths"));
        // 이름 없이는 표준 입력을 비밀번호로 읽을 수 없다.
        let o = e.sbx_with_stdin(&["login", "--password-stdin", &registry], b"pw\n");
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("-u"), "{}", text(&o.stderr));

        let o = e.sbx_with_stdin(
            &[
                "--format",
                "json",
                "login",
                "-u",
                "me",
                "--password-stdin",
                &format!("https://{registry}/"),
            ],
            b"pw\n",
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let l: sbx_cli::output::Login = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            (l.registry.as_str(), l.stored.as_str()),
            (registry.as_str(), "file")
        );
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
        assert_eq!(v["auths"][&registry]["auth"], "bWU6cHc=");
        assert_eq!(
            v["proxies"]["default"]["noProxy"], "x",
            "other settings stay"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&config).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let o = e.sbx(&["--format", "json", "logout", &registry]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let l: sbx_cli::output::Logout = serde_json::from_slice(&o.stdout).unwrap();
        assert!(l.removed);
        let o = e.sbx(&["logout", &registry]);
        assert_eq!(text(&o.stdout), format!("Not logged in to {registry}\n"));
    }

    fn a_detached_sandbox_stops_after_its_timeout() {
        let e = Env::new();
        // 수명이 다 되면 멈춤 시그널을 받는다. 그 시그널로 끝나면 그 종료 코드.
        let id = run_trapping(&e, &["--timeout", "1", "--stop-signal", "USR1"], "USR1", 5);
        let s = state_of(&e, &id).unwrap();
        assert_eq!(s["timeout_secs"], 1, "{s}");
        let s = wait_state(&e, &id, "stopped");
        assert_eq!(s["exit_code"], 5, "{s}");

        // 시그널을 무시하면 멈춤 기다림 뒤에 전원을 끈다: 137과 그 까닭.
        let id = run_trapping(&e, &["--timeout", "1", "--stop-timeout", "1"], "USR2", 4);
        let s = wait_state(&e, &id, "stopped");
        assert_eq!(s["exit_code"], 137, "{s}");
        assert!(
            s["error"].as_str().unwrap_or("").contains("--timeout"),
            "{s}"
        );

        let o = e.sbx(&["run", "-d", "--timeout", "0", IMAGE, "true"]);
        assert_ne!(o.status.code(), Some(0));
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
            "while kill -0 $PPID 2>/dev/null; do test -t 1 && echo tty; sleep 0.05; done",
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
            "trap 'echo term; exit 7' TERM; while kill -0 $PPID 2>/dev/null; do echo tick; sleep 0.05; done",
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
        // system df: 실행 중인 샌드박스가 쓰는 실행기는 active다. 멈추면 아니다.
        let executors_row = || {
            let o = without(&["--format", "json", "system", "df"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            let rows: Vec<sbx_cli::output::DiskUsage> = serde_json::from_slice(&o.stdout).unwrap();
            let r = rows.into_iter().find(|r| r.kind == "executors").unwrap();
            (r.total, r.active)
        };
        assert_eq!(executors_row(), (1, 0));
        let o = without(&["run", "-d", "--name", "busy", IMAGE, "sleep", "30"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        wait_named(&e, "busy");
        assert_eq!(executors_row(), (1, 1));
        assert_eq!(without(&["rm", "-f", "busy"]).status.code(), Some(0));
        assert_eq!(executors_row(), (1, 0));
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

    /// VMM 묶음(어댑터가 돌린다), 샌드박스 고정, 폴더 파일, 지우기 보호, prune.
    fn vmm_packages_pins_and_version_files() {
        let e = Env::new();
        let me = std::env::current_exe().unwrap();
        let me = me.to_str().unwrap();
        let in_dir = |dir: &Path, args: &[&str]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR").current_dir(dir);
            c.output().unwrap()
        };
        let plain = e.path("plain");
        std::fs::create_dir_all(&plain).unwrap();
        let without = |args: &[&str]| in_dir(&plain, args);
        let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // sbx만 있으면 아무것도 설치돼 있지 않다.
        let o = without(&["executor", "ls"]);
        ok(&o);
        assert_eq!(text(&o.stdout).lines().count(), 1, "{}", text(&o.stdout));
        let o = without(&["executor", "current"]);
        assert!(
            text(&o.stderr).contains("no executor is installed")
                && !text(&o.stderr).contains("wick"),
            "{}",
            text(&o.stderr)
        );

        // VMM 묶음은 어댑터가 있어야 설치된다.
        let vmm = e.path("wick-src");
        std::fs::create_dir_all(&vmm).unwrap();
        std::fs::write(vmm.join("vmm"), "x").unwrap();
        let vmm = vmm.to_str().unwrap();
        let o = without(&[
            "executor",
            "install",
            "--from",
            vmm,
            "--adapter",
            "fakead",
            "wick@1.0",
        ]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("adapter install fakead"),
            "{}",
            text(&o.stderr)
        );
        ok(&without(&[
            "executor",
            "adapter",
            "install",
            "--from",
            me,
            "fakead@0.1",
        ]));
        let o = without(&[
            "executor",
            "install",
            "--from",
            vmm,
            "--adapter",
            "fakead",
            "wick@1.0",
        ]);
        ok(&o);
        let o = without(&["executor", "ls"]);
        assert!(
            text(&o.stdout).contains("* wick") && text(&o.stdout).contains("fakead"),
            "{}",
            text(&o.stdout)
        );
        let o = without(&["executor", "adapter", "ls", "--format", "json"]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v[0]["used_by"], serde_json::json!(["wick@1.0"]), "{v}");
        let o = without(&["executor", "current", "--format", "json"]);
        ok(&o);
        let v: sbx_cli::output::ChosenExecutor = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            (v.name.as_deref(), v.adapter.as_deref()),
            (Some("wick"), Some("fakead@0.1"))
        );
        assert!(v.vmm_dir.unwrap().ends_with("executors/wick/1.0"));
        let o = without(&["executor", "which"]);
        assert!(
            text(&o.stdout)
                .trim_end()
                .ends_with("adapters/fakead/0.1/sbx-executor"),
            "{}",
            text(&o.stdout)
        );
        // 어댑터가 VMM 묶음 디렉터리를 받아 돌린다(붙어서, 그리고 shim으로).
        let o = without(&["run", "--rm", IMAGE, "echo", "via", "wick"]);
        ok(&o);
        assert_eq!(text(&o.stdout), "via wick\n");
        // system df: 실행 중인 샌드박스가 쓰는 VMM 묶음과 그 어댑터는 둘 다 active다(shim이 띄운 것도).
        let o = without(&["run", "-d", "--name", "via-shim", IMAGE, "sleep", "30"]);
        ok(&o);
        wait_named(&e, "via-shim");
        let o = without(&["--format", "json", "system", "df"]);
        ok(&o);
        let rows: Vec<sbx_cli::output::DiskUsage> = serde_json::from_slice(&o.stdout).unwrap();
        let r = rows.iter().find(|r| r.kind == "executors").unwrap();
        assert_eq!((r.total, r.active), (2, 2), "{rows:?}");
        ok(&without(&["rm", "-f", "via-shim"]));

        // 만든 샌드박스는 그 실행기에 고정된다. 기본을 바꿔도 그것으로 시작한다.
        let o = without(&["create", "--name", "pinned", IMAGE, "echo", "pinned"]);
        ok(&o);
        let o = without(&["inspect", "pinned", "--format", "json"]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(
            v["labels"]["io.github.toollyst.sandbox.executor"], "wick@1.0",
            "{v}"
        );
        ok(&without(&["executor", "install", "--from", me, "fake@2.0"]));
        ok(&without(&["executor", "use", "fake@2.0"]));
        let o = without(&["start", "-a", "pinned"]);
        ok(&o);
        assert_eq!(text(&o.stdout), "pinned\n");
        // shim으로(붙지 않고) 시작해도 같다.
        ok(&without(&["start", "pinned"]));
        let o = without(&["wait", "pinned"]);
        ok(&o);
        assert_eq!(text(&o.stdout), "0\n");
        // 고정한 샌드박스가 있으면 지우지 않는다. 마지막 어댑터도.
        let o = without(&["executor", "rm", "wick@1.0"]);
        assert!(
            text(&o.stderr).contains("pinned by sandbox pinned"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&["executor", "adapter", "rm", "fakead@0.1"]);
        assert!(
            text(&o.stderr).contains("wick@1.0 needs it"),
            "{}",
            text(&o.stderr)
        );
        ok(&without(&["executor", "rm", "-f", "wick@1.0"]));
        let o = without(&["start", "-a", "pinned"]);
        assert!(
            text(&o.stderr).contains("no longer installed"),
            "{}",
            text(&o.stderr)
        );
        // --executor로 다른 것을 고를 수 있다(명령마다).
        let o = without(&["--executor", "fake@2.0", "start", "-a", "pinned"]);
        ok(&o);

        // 폴더 파일: 지금 폴더부터 위로.
        let project = e.path("project/sub");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            e.path("project/.sbx-executor"),
            "# this project\nfake@1.5\n",
        )
        .unwrap();
        let o = in_dir(&project, &["run", "--rm", IMAGE, "true"]);
        assert!(
            text(&o.stderr).contains("sbx executor install fake@1.5"),
            "{}",
            text(&o.stderr)
        );
        ok(&without(&["executor", "install", "--from", me, "fake@1.5"]));
        let o = in_dir(&project, &["executor", "current"]);
        assert!(
            text(&o.stdout).starts_with("fake@1.5 ("),
            "{}",
            text(&o.stdout)
        );
        assert!(
            text(&o.stdout).contains(".sbx-executor"),
            "{}",
            text(&o.stdout)
        );
        let o = in_dir(&project, &["executor", "current", "--format", "json"]);
        let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(v["version"], "1.5", "{v}");

        // prune: 기본(2.0)과 가장 높은 버전(2.0)은 남고 1.5는 지운다. 먼저 미리 보기.
        ok(&without(&["executor", "install", "--from", me, "fake@1.0"]));
        let o = without(&["executor", "prune", "-n"]);
        ok(&o);
        assert_eq!(
            text(&o.stdout),
            "would remove executor fake@1.0\nwould remove executor fake@1.5\n"
        );
        ok(&without(&["executor", "prune"]));
        let o = without(&["executor", "ls"]);
        let ls = text(&o.stdout);
        assert!(
            ls.contains("2.0") && !ls.contains("1.5") && !ls.contains("1.0"),
            "{ls}"
        );
        ok(&without(&["rm", "pinned"]));
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

    fn signed_repositories_and_names_that_stay_put() {
        let e = Env::new();
        let sbx = |args: &[&str]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR");
            c.output().unwrap()
        };
        let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        ok(&sbx(&["executor", "repo", "rm", "official"]));
        let (bytes, sha) = bundle_of_me();
        let host = sbx_executor_repo::Platform::host();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 저장소 하나: v2 색인(fake 버전들, serial), 서명, 공개키.
        let make = |name: &str, versions: &[&str], serial: u64, kp: &minisign::KeyPair| {
            let dir = e.path(name);
            std::fs::create_dir_all(dir.join("v2")).unwrap();
            std::fs::write(dir.join("v2/fake.tar.gz"), &bytes).unwrap();
            let mut vs = serde_json::Map::new();
            for v in versions {
                vs.insert((*v).into(), serde_json::json!({"protocol": sbx_executor_proto::VERSION, "artifacts": [
                    {"os": host.os, "architecture": host.architecture, "url": "fake.tar.gz", "sha256": sha, "size": bytes.len()}]}));
            }
            let index = serde_json::json!({"schema": 2, "serial": serial, "expires": now + 3600,
                "executors": {"fake": {"versions": vs}}})
            .to_string();
            std::fs::write(dir.join("v2/index.json"), &index).unwrap();
            let sig = minisign::sign(None, &kp.sk, index.as_bytes(), None, None).unwrap();
            std::fs::write(dir.join("v2/index.json.minisig"), sig.into_string()).unwrap();
            format!("file://{}", dir.display())
        };
        let ka = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let kb = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let a = make("a", &["1.0"], 10, &ka);
        let b = make("b", &["1.0", "9.0"], 1, &kb);

        // 맞는 키로 더한 저장소에서 설치한다. ls는 출처를 보인다.
        let o = sbx(&[
            "executor",
            "repo",
            "add",
            "a",
            &a,
            "--key",
            &kb.pk.to_base64(),
        ]);
        ok(&o);
        let o = sbx(&["executor", "install", "fake"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("matches none"),
            "{}",
            text(&o.stderr)
        );
        ok(&sbx(&["executor", "repo", "rm", "a"]));
        ok(&sbx(&[
            "executor",
            "repo",
            "add",
            "a",
            &a,
            "--key",
            &ka.pk.to_base64(),
        ]));
        ok(&sbx(&[
            "executor",
            "repo",
            "add",
            "b",
            &b,
            "--key",
            &kb.pk.to_base64(),
        ]));
        let o = sbx(&["executor", "repo", "ls"]);
        assert!(text(&o.stdout).contains("SIGNED"), "{}", text(&o.stdout));
        let o = sbx(&["executor", "install", "fake"]);
        ok(&o);
        assert!(
            !text(&o.stderr).contains("not signed"),
            "{}",
            text(&o.stderr)
        );
        assert_eq!(text(&o.stdout), "fake@1.0\n");
        let o = sbx(&["executor", "ls"]);
        assert!(
            text(&o.stdout).contains("FROM") && text(&o.stdout).contains(" a "),
            "{}",
            text(&o.stdout)
        );

        // 이름은 그것을 내는 첫 저장소(a)의 것이다. 뒤 저장소 b의 9.0은 고르지 않는다.
        let o = sbx(&["executor", "install", "fake@9.0"]);
        assert_ne!(o.status.code(), Some(0));
        // --repo b로 고를 수는 있지만, 이미 a에서 받은 이름이라 섞지 않는다.
        let o = sbx(&["executor", "install", "--repo", "b", "fake@9.0"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("fake is installed from a, not b"),
            "{}",
            text(&o.stderr)
        );

        // 이 기계가 본 것보다 오래된 색인(롤백)과 기한이 지난 색인은 거부한다.
        make("a", &["1.0"], 9, &ka);
        let o = sbx(&["executor", "search"]);
        assert!(
            text(&o.stderr).contains("older than 10"),
            "{}",
            text(&o.stderr)
        );
        let dir = e.path("a");
        let index =
            serde_json::json!({"schema": 2, "serial": 11, "expires": now - 1, "executors": {}})
                .to_string();
        std::fs::write(dir.join("v2/index.json"), &index).unwrap();
        let sig = minisign::sign(None, &ka.sk, index.as_bytes(), None, None).unwrap();
        std::fs::write(dir.join("v2/index.json.minisig"), sig.into_string()).unwrap();
        let o = sbx(&["executor", "search"]);
        assert!(text(&o.stderr).contains("expired"), "{}", text(&o.stderr));
    }

    /// 실행기가 없으면 VM을 띄우는 명령이 공식 저장소(여기서는 로컬 서명 저장소를 `official`로 둔다)의 기본
    /// 실행기를 받아 깔고 이어서 한다. 끄는 길, 서명되지 않은 저장소, 닿지 않는 저장소, 동시에 두 번.
    fn the_default_executor_is_installed_on_first_use() {
        let e = Env::new();
        let sbx = |args: &[&str], extra: &[(&str, &str)]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR")
                .env_remove("SBX_NO_AUTO_EXECUTOR");
            for (k, v) in extra {
                c.env(k, v);
            }
            c.output().unwrap()
        };
        let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let nothing_installed = || {
            let o = sbx(&["executor", "ls", "--format", "json"], &[]);
            assert_eq!(text(&o.stdout).trim(), "[]", "{}", text(&o.stderr));
        };
        let (bytes, sha) = bundle_of_me();
        let host = sbx_executor_repo::Platform::host();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        // 실행기가 둘이다: 어느 것이 기본인지는 색인의 `default`가 말한다.
        let dir = e.path("official");
        std::fs::create_dir_all(dir.join("v2")).unwrap();
        std::fs::write(dir.join("v2/fake.tar.gz"), &bytes).unwrap();
        let art = serde_json::json!([{"os": host.os, "architecture": host.architecture,
            "url": "fake.tar.gz", "sha256": sha, "size": bytes.len()}]);
        let entry = |v: &str| serde_json::json!({"versions": {v: {"protocol": sbx_executor_proto::VERSION, "artifacts": art}}});
        let index =
            serde_json::json!({"schema": 2, "serial": 1, "expires": now + 3600, "default": "fake",
            "executors": {"fake": entry("1.0"), "zzz": entry("9.0")}})
            .to_string();
        std::fs::write(dir.join("v2/index.json"), &index).unwrap();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let sig = minisign::sign(None, &kp.sk, index.as_bytes(), None, None).unwrap();
        std::fs::write(dir.join("v2/index.json.minisig"), sig.into_string()).unwrap();
        let url = format!("file://{}", dir.display());
        let key = kp.pk.to_base64();
        let set_official = |url: &str, key: Option<&str>| {
            ok(&sbx(&["executor", "repo", "rm", "official"], &[]));
            let mut args = vec!["executor", "repo", "add", "official", url];
            match key {
                Some(k) => args.extend(["--key", k]),
                None => args.push("--unsigned"),
            }
            ok(&sbx(&args, &[]));
        };
        let run = ["run", "--rm", IMAGE, "echo", "hi"];

        // 이 sbx에 공식 키가 없으면(지금의 빌드) 받지 않는다. 표준 출력은 비어 있다.
        set_official(&url, None);
        let o = sbx(&run, &[]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("is not signed"),
            "{}",
            text(&o.stderr)
        );
        nothing_installed();

        set_official(&url, Some(&key));
        // 받기 전의 안내: 첫 run이 받는다.
        let o = sbx(&["executor", "current"], &[]);
        assert!(
            text(&o.stderr).contains("the first `sbx run`"),
            "{}",
            text(&o.stderr)
        );
        // 끄는 길 둘.
        for (k, v, why) in [
            ("SBX_NO_AUTO_EXECUTOR", "1", "SBX_NO_AUTO_EXECUTOR is set"),
            ("SBX_EXECUTOR", "none", "SBX_EXECUTOR=none"),
        ] {
            let o = sbx(&run, &[(k, v)]);
            assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
            assert!(text(&o.stderr).contains(why), "{k}: {}", text(&o.stderr));
            nothing_installed();
        }
        // `SBX_NO_AUTO_EXECUTOR=0`은 끄지 않는다(아래에서 받는다).

        // 닿지 않으면 까닭과 손으로 하는 길을 알리고 아무것도 깔지 않는다.
        set_official(&format!("file://{}", e.path("gone").display()), Some(&key));
        let o = sbx(&run, &[]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        let err = text(&o.stderr);
        assert!(
            err.contains("could not be read") && err.contains("sbx executor install --from DIR"),
            "{err}"
        );
        nothing_installed();

        // 두 명령이 함께 처음 돈다: 하나만 받고 알리며, 둘 다 돈다. 받은 것의 알림은 표준 오류에만.
        set_official(&url, Some(&key));
        let spawn = || {
            let mut c = e.cmd(&run);
            c.env_remove("SBX_EXECUTOR")
                .env("SBX_NO_AUTO_EXECUTOR", "0")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            c.spawn().unwrap()
        };
        let (a, b) = (spawn(), spawn());
        let outs = [
            wait_with_timeout(a, Duration::from_secs(60)),
            wait_with_timeout(b, Duration::from_secs(60)),
        ];
        let mut notices = 0;
        for o in &outs {
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            assert_eq!(text(&o.stdout), "hi\n");
            if text(&o.stderr).contains(&format!(
                "installing the default executor fake@1.0 from official ({url}/)"
            )) {
                notices += 1;
            }
        }
        assert_eq!(
            notices,
            1,
            "{}\n---\n{}",
            text(&outs[0].stderr),
            text(&outs[1].stderr)
        );
        let o = sbx(&["executor", "ls"], &[]);
        assert!(
            text(&o.stdout).contains("* fake") && text(&o.stdout).contains("official"),
            "{}",
            text(&o.stdout)
        );
        // 이제 있으니 다시 받지 않는다.
        let o = sbx(&run, &[]);
        ok(&o);
        assert!(
            !text(&o.stderr).contains("installing"),
            "{}",
            text(&o.stderr)
        );
        // 설치한 것이 있는데 기본이 없으면 받지 않고 고르라고 한다.
        let o = sbx(&["executor", "repo", "ls"], &[]);
        ok(&o);
        std::fs::remove_file(e.path("data/executors/default")).unwrap();
        let o = sbx(&run, &[]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("sbx executor use NAME"),
            "{}",
            text(&o.stderr)
        );
    }

    fn a_repository_that_vanishes_or_turns_hostile_breaks_nothing_installed() {
        let e = Env::new();
        let sbx = |args: &[&str]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR");
            c.output().unwrap()
        };
        let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let not_a_crash = |o: &Output| {
            let err = text(&o.stderr);
            assert!(
                !err.contains("panicked") && o.status.code() != Some(101),
                "{err}"
            );
        };
        ok(&sbx(&["executor", "repo", "rm", "official"]));
        let (bytes, sha) = bundle_of_me();
        let host = sbx_executor_repo::Platform::host();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let dir = e.path("r");
        std::fs::create_dir_all(dir.join("v2")).unwrap();
        std::fs::write(dir.join("v2/fake.tar.gz"), &bytes).unwrap();
        let publish = |body: &str| {
            std::fs::write(dir.join("v2/index.json"), body).unwrap();
            let sig = minisign::sign(None, &kp.sk, body.as_bytes(), None, None).unwrap();
            std::fs::write(dir.join("v2/index.json.minisig"), sig.into_string()).unwrap();
        };
        let art = serde_json::json!([{"os": host.os, "architecture": host.architecture, "url": "fake.tar.gz", "sha256": sha, "size": bytes.len()}]);
        let index = |serial: u64| {
            serde_json::json!({"schema": 2, "serial": serial, "expires": now + 3600, "executors": {"fake": {"versions": {
                "1.0": {"protocol": sbx_executor_proto::VERSION, "artifacts": art},
                "2.0": {"protocol": sbx_executor_proto::VERSION, "artifacts": art}}}}})
            .to_string()
        };
        publish(&index(5));
        let url = format!("file://{}", dir.display());
        ok(&sbx(&[
            "executor",
            "repo",
            "add",
            "r",
            &url,
            "--key",
            &kp.pk.to_base64(),
        ]));
        ok(&sbx(&["executor", "install", "fake@1.0"]));
        ok(&sbx(&["executor", "install", "fake@2.0"]));

        // 저장소가 바뀌어도 설치한 것으로 하는 일은 그대로다: 목록, 고르기, 실행, 지우기.
        let still_works = |what: &str, why: &str| {
            let o = sbx(&["executor", "ls"]);
            ok(&o);
            assert!(
                text(&o.stdout).contains("1.0") && text(&o.stdout).contains("2.0"),
                "{what}: {}",
                text(&o.stdout)
            );
            ok(&sbx(&["executor", "use", "fake@1.0"]));
            let o = sbx(&["run", "--rm", IMAGE, "echo", "still here"]);
            ok(&o);
            assert_eq!(text(&o.stdout), "still here\n", "{what}");
            ok(&sbx(&["executor", "use", "fake@2.0"]));
            ok(&sbx(&["ps", "-a"]));
            // 저장소에 닿는 명령은 실패를 알리되 죽지 않는다.
            let o = sbx(&["executor", "search"]);
            not_a_crash(&o);
            assert!(
                text(&o.stderr).contains(why),
                "{what}: wanted {why:?} in {}",
                text(&o.stderr)
            );
            let o = sbx(&["executor", "install", "fake@3.0"]);
            not_a_crash(&o);
            assert!(
                text(&o.stderr).contains(why),
                "{what} (install): wanted {why:?} in {}",
                text(&o.stderr)
            );
        };
        // 1. 서명이 다른 내용(누군가 색인을 고쳤다).
        let mut tampered = index(6);
        tampered.push(' ');
        std::fs::write(dir.join("v2/index.json"), &tampered).unwrap();
        still_works(
            "tampered",
            "the signature matches none of the repository's keys",
        );
        // 2. 서명된 옛 색인(롤백).
        publish(&index(1));
        still_works("rollback", "older than 5");
        // 3. JSON이 아닌 것, 아주 큰 것.
        publish("<html>not found</html>");
        still_works("garbage", "not a repository index");
        std::fs::write(dir.join("v2/index.json"), vec![b' '; 17 << 20]).unwrap();
        still_works("huge", "larger than the 16777216 bytes");
        // 4. 저장소가 통째로 사라졌다.
        std::fs::remove_dir_all(&dir).unwrap();
        still_works("gone", "v2/index.json not found");
        // 지우기도 저장소 없이 된다.
        ok(&sbx(&["executor", "rm", "fake@1.0"]));
        ok(&sbx(&["executor", "repo", "rm", "r"]));
        let o = sbx(&["run", "--rm", IMAGE, "echo", "after"]);
        ok(&o);
        assert_eq!(text(&o.stdout), "after\n");
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
        // 키도 --unsigned도 없으면 더하지 않는다.
        let o = without(&["executor", "repo", "add", "mine", &url]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("--key"), "{}", text(&o.stderr));
        for (name, u) in [("broken", missing.as_str()), ("mine", url.as_str())] {
            let o = without(&["executor", "repo", "add", name, u, "--unsigned"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        }
        let o = without(&["executor", "repo", "ls"]);
        assert!(text(&o.stdout).contains("mine"), "{}", text(&o.stdout));

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

        // 앞 저장소(broken)를 읽지 못하면, 그 저장소가 이 이름을 가졌을 수 있으므로 뒤 저장소에서 설치하지 않는다.
        let o = without(&["executor", "install", "fake"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("repository broken could not be read")
                && text(&o.stderr).contains("--repo mine"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&["executor", "ls"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!text(&o.stdout).contains("fake"), "{}", text(&o.stdout));
        let o = without(&["executor", "repo", "rm", "broken"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));

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

        // 묶음 압축 파일을 직접 설치. 저장소 mine에서 받은 이름(fake)에는 섞지 않는다.
        let archive = repo.join("fake/1.0.tar.gz");
        let o = without(&[
            "executor",
            "install",
            "--from",
            archive.to_str().unwrap(),
            "fake@9.9",
        ]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("installed from mine"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&[
            "executor",
            "install",
            "--from",
            archive.to_str().unwrap(),
            "direct@1",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = without(&["executor", "ls"]);
        let ls = text(&o.stdout);
        assert!(
            ls.contains("direct") && ls.contains("local") && !ls.contains("0.5"),
            "{ls}"
        );
        // `local`은 직접 설치한 것의 출처라서 저장소 이름으로 쓸 수 없다.
        let o = without(&["executor", "repo", "add", "local", &url, "--unsigned"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("reserved"), "{}", text(&o.stderr));
        // 이미 지운 저장소는 다시 지울 수 없다.
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
        plant_empty_rootfs(&layout);
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

    fn mcp_limits_what_an_agent_can_ask_for() {
        let e = Env::new();
        let child = e
            .cmd(&[
                "mcp",
                "--max-sandboxes",
                "1",
                "--max-cpus",
                "2",
                "--max-memory",
                "1g",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut child = child;
        let out = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut m = Mcp {
            child,
            out,
            next: 0,
        };
        m.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
        // 호출 하나의 vCPU·메모리 상한.
        let e1 = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"], "cpus": 3}),
            )
            .unwrap_err();
        assert!(e1.contains("--max-cpus"), "{e1}");
        let e2 = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"], "memory": "2g"}),
            )
            .unwrap_err();
        assert!(e2.contains("--max-memory"), "{e2}");
        // 샌드박스 수 상한: 하나가 있으면 둘째는 거부하고, 지우면 다시 된다.
        let id = m
            .call(
                "create",
                serde_json::json!({"image": IMAGE, "cpus": 2, "memory": "1g"}),
            )
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let e3 = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"]}),
            )
            .unwrap_err();
        assert!(e3.contains("--max-sandboxes 1"), "{e3}");
        m.call("remove", serde_json::json!({"id": id})).unwrap();
        let r = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["echo", "ok"]}),
            )
            .unwrap();
        assert_eq!(r["stdout"], "ok\n", "{r}");
    }

    fn mcp_limits_hold_under_concurrent_calls() {
        let e = Env::new();
        let mut child = e
            .cmd(&["mcp", "--max-sandboxes", "2"])
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
        m.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
        // 응답을 기다리지 않고 create 다섯 개를 한꺼번에 보낸다. 서버는 이것들을 동시에 처리한다.
        // 지금의 Runtime::list는 기다리는 곳이 없어 자리 예약이 없어도 통과할 수 있다. 이 시험은 admit과 만들기
        // 사이에 await가 끼어들어도(list가 실행기에 묻게 되는 등) 한도가 지켜지는지를 지킨다.
        for id in 100..105 {
            let msg = serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "create", "arguments": {"image": IMAGE}}});
            m.write_raw(msg.to_string().as_bytes());
        }
        let (mut ok, mut refused) = (Vec::new(), 0);
        while ok.len() + refused < 5 {
            let v = m.read();
            if v.get("id").is_none() {
                continue; // 진행 알림
            }
            let r = &v["result"];
            if r["isError"] == true {
                let text = r["content"][0]["text"].as_str().unwrap_or_default();
                assert!(text.contains("--max-sandboxes 2"), "{text}");
                refused += 1;
            } else {
                ok.push(r["structuredContent"]["id"].as_str().unwrap().to_owned());
            }
        }
        assert_eq!((ok.len(), refused), (2, 3), "{ok:?}");
        let listed = m.call("list", serde_json::json!({})).unwrap();
        assert_eq!(listed["sandboxes"].as_array().unwrap().len(), 2, "{listed}");
        // 실패한 호출은 자리를 남기지 않는다: 지우면 그만큼 다시 만들 수 있다.
        for id in &ok {
            m.call("remove", serde_json::json!({"id": id})).unwrap();
        }
        for _ in 0..2 {
            m.call("create", serde_json::json!({"image": IMAGE}))
                .unwrap();
        }
        assert!(
            m.call("create", serde_json::json!({"image": IMAGE}))
                .is_err()
        );
    }

    /// MCP의 잇기: MCP로 만든 샌드박스는 MCP로 만든 샌드박스에만 잇는다. 만들 때도, 연결마다도(이름이 나중에 CLI로
    /// 만든 샌드박스를 가리켜도 닿지 않는다). CLI로 만든 샌드박스는 MCP로 만든 샌드박스에 이을 수 있다.
    fn mcp_connect_stays_among_mcp_sandboxes() {
        use sbx_executor_fake::probe;
        use std::io::Read;
        let e = Env::new();
        let data = e.path("data");
        let port = echo_service();
        // 프록시를 모르는 클라이언트 노릇: 샌드박스 `me` 안에서 `name:port`에 붙어 "hi"를 보내고 받은 것.
        let echo = |me: &str, name: &str| -> String {
            let id = std::fs::read_to_string(data.join("names").join(me)).unwrap();
            let dir = probe::find(&data.join("sandboxes").join(id.trim())).unwrap();
            let mut c = match probe::connect(&dir, name, port) {
                Ok(c) => c,
                Err(e) => return format!("unreachable: {e}"),
            };
            c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let _ = c.write_all(b"hi");
            let _ = c.shutdown(std::net::Shutdown::Write);
            let mut got = String::new();
            let _ = c.read_to_string(&mut got);
            got
        };
        let o = e.sbx(&["run", "-d", "--name", "userdb", IMAGE, "sleep", "60"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
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
        m.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
        m.call(
            "create",
            serde_json::json!({"image": IMAGE, "name": "mdb", "command": ["sleep", "60"]}),
        )
        .unwrap();
        // 사용자의 샌드박스에는 잇지 못한다(MCP에는 없는 것과 같다).
        let err = m
            .call(
                "create",
                serde_json::json!({"image": IMAGE, "name": "agent0", "connect": [format!("userdb:{port}")]}),
            )
            .unwrap_err();
        assert!(err.contains("created through MCP"), "{err}");
        // MCP로 만든 것에는 잇는다.
        m.call(
            "create",
            serde_json::json!({"image": IMAGE, "name": "agent", "command": ["sleep", "60"], "connect": [format!("mdb:{port}")]}),
        )
        .unwrap();
        assert_eq!(
            echo("agent", "mdb"),
            "hi",
            "{:?}",
            network_events(&e, &["sandbox=agent"])
        );
        assert!(echo("agent", "userdb").starts_with("unreachable: userdb: rcode 3"));
        // 그 이름이 나중에 사용자의 샌드박스를 가리켜도 연결마다 다시 보므로 닿지 않는다.
        m.call("remove", serde_json::json!({"id": "mdb"})).unwrap();
        let o = e.sbx(&["run", "-d", "--name", "mdb", IMAGE, "sleep", "60"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(echo("agent", "mdb"), "");
        let refused = network_events(&e, &["sandbox=agent", "event=refuse"]);
        assert!(
            refused.iter().any(|ev| ev["attributes"]["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("lacks the label io.github.toollyst.sandbox.owner=mcp")),
            "{refused:?}"
        );
        // 사용자(CLI)의 샌드박스는 MCP로 만든 샌드박스에 잇는다.
        let to_agent = format!("agent:{port}");
        let o = e.sbx(&[
            "run",
            "-d",
            "--name",
            "user",
            "--connect",
            &to_agent,
            IMAGE,
            "sleep",
            "60",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(echo("user", "agent"), "hi");
        m.call("remove", serde_json::json!({"id": "agent"}))
            .unwrap();
        for name in ["userdb", "mdb", "user"] {
            let _ = e.sbx(&["rm", "-f", name]);
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
        let r = m.call("run", serde_json::json!({"image": IMAGE, "command": ["sh", "-c", "echo before; sleep 30"], "timeout_secs": 3})).unwrap();
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

    // ── build ────────────────────────────────────────────────────────────
    //
    // 가짜 실행기의 빌드 VM은 층 디스크를 디렉터리로 펼치고, 작업 디렉터리와 COPY 대상을 그 아래로 옮긴다. 명령은
    // 호스트에서 돌므로 RUN은 작업 디렉터리 기준 상대 경로를 쓴다.

    /// 바탕 이미지(`local/base:1`, 층 하나)를 들이고 빌드 컨텍스트 디렉터리를 돌려준다.
    fn build_env() -> (Env, PathBuf) {
        let e = Env::new();
        let (base, _) = oci_archive_layers("local/base:1", &[BASE]);
        load(&e, "base.tar", &base);
        let ctx = e.path("ctx");
        std::fs::create_dir_all(ctx.join("src")).unwrap();
        (e, ctx)
    }

    fn write(dir: &Path, name: &str, data: &str) {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    /// `build`을 돌린다. `record`면 실행기가 받은 VM 설정을 그 파일에 적게 한다(VM을 띄웠는지 본다).
    fn build(e: &Env, ctx: &Path, args: &[&str], record: Option<&Path>) -> Output {
        let mut all = vec!["build"];
        all.extend_from_slice(args);
        all.push(ctx.to_str().unwrap());
        let mut c = e.cmd(&all);
        if let Some(r) = record {
            let _ = std::fs::remove_file(r);
            c.env("SBX_FAKE_RECORD", r);
        }
        c.output().unwrap()
    }

    const MULTI_STAGE: &str = "\
FROM local/base:1 AS build
WORKDIR /src
COPY src/ ./
RUN test ! -e secret.txt && cat a.txt > out.txt && echo built >> out.txt
FROM local/base:1
COPY --from=build /src/out.txt /app/out.txt
ENV GREETING=hi
CMD [\"cat\", \"/app/out.txt\"]
";

    /// 여러 스테이지를 만들고, 만든 이미지가 목록에 있고 돌고, 그 층을 다음 빌드가 본다. 두 번째 빌드는 모두 캐시에서
    /// 와 VM을 띄우지 않는다. COPY한 파일 하나를 바꾸면 그 단계부터 다시 만들고, `--no-cache`는 캐시를 쓰지 않는다.
    fn build_runs_stages_and_caches_steps() {
        let (e, ctx) = build_env();
        write(&ctx, "Dockerfile", MULTI_STAGE);
        write(&ctx, ".dockerignore", "src/secret.txt\n");
        write(&ctx, "src/a.txt", "from the context\n");
        write(&ctx, "src/secret.txt", "no\n");
        let rec = e.path("vm.json");
        let o = build(&e, &ctx, &["-t", "local/app:1"], Some(&rec));
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(rec.is_file(), "the RUN step needs a VM");
        assert!(
            err.contains("[build 4/4] RUN test ! -e secret.txt"),
            "{err}"
        );
        assert!(err.contains("[stage-1 2/4] COPY --from=build"), "{err}");
        assert!(text(&o.stdout).contains("Successfully tagged local/app:1"));
        let o = e.sbx(&["--format", "json", "images"]);
        let list: Vec<sbx_cli::output::Image> = serde_json::from_slice(&o.stdout).unwrap();
        let app = list
            .iter()
            .find(|i| i.references.iter().any(|r| r.contains("local/app:1")))
            .expect("the built image is listed");
        // 바탕 층 + COPY 층.
        let o = e.sbx(&["run", "--rm", "local/app:1", "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // 빌드 VM과 그 뿌리 기록은 남지 않는다.
        assert!(e.ps().is_empty());
        assert_eq!(list.len(), 3, "e2e-fake, base, app: {list:?}");

        // 만든 이미지 위에서 그 층을 본다.
        let check = e.path("check");
        std::fs::create_dir_all(&check).unwrap();
        write(
            &check,
            "Dockerfile",
            "FROM local/app:1\nWORKDIR /app\nRUN cat out.txt && echo \"GREETING=$GREETING\"\n",
        );
        let o = build(&e, &check, &["--progress", "plain"], None);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(
            err.contains("from the context\nbuilt\nGREETING=hi\n"),
            "{err}"
        );

        // 두 번째: 모두 캐시, VM 없음, 같은 이미지 ID.
        let o = build(&e, &ctx, &["-q", "-t", "local/app:1"], Some(&rec));
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!rec.exists(), "a fully cached build launches no VM");
        assert_eq!(text(&o.stdout).trim(), app.id);
        let o = build(&e, &ctx, &[], Some(&rec));
        let err = text(&o.stderr);
        assert!(err.contains("[build 4/4] CACHED"), "{err}");

        // 원본 하나를 바꾸면 그 단계부터 다시 만든다(앞 단계는 캐시).
        write(&ctx, "src/a.txt", "changed\n");
        let o = build(&e, &ctx, &[], Some(&rec));
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(err.contains("[build 2/4] CACHED"), "{err}");
        assert!(!err.contains("[build 3/4] CACHED"), "{err}");
        assert!(!err.contains("[build 4/4] CACHED"), "{err}");
        assert!(rec.is_file());
        // --no-cache: 캐시를 읽지 않는다.
        let o = build(&e, &ctx, &["--no-cache"], Some(&rec));
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(!err.contains("CACHED"), "{err}");
        assert!(rec.is_file());
        // 만든 이미지도 save한다(층 디스크에서 내보낸다). 다른 sbx가 들여 돌린다.
        let o = e.sbx(&[
            "save",
            "-o",
            e.path("app.tar").to_str().unwrap(),
            "local/app:1",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let other = Env::new();
        load(
            &other,
            "app.tar",
            &std::fs::read(e.path("app.tar")).unwrap(),
        );
        let o = other.sbx(&["run", "--rm", "local/app:1", "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    }

    /// 실패한 RUN은 그 출력과 코드를 보인다. 백그라운드로 남긴 프로세스는 단계를 붙잡지 않는다. `--target`은 그
    /// 스테이지까지만 만든다. 빌드 인자는 캐시 키다. 받지 않는 명령은 줄 번호와 함께 오류다.
    fn build_failures_targets_and_build_args() {
        let (e, ctx) = build_env();
        write(
            &ctx,
            "Dockerfile",
            "FROM local/base:1\nRUN echo something went wrong && exit 3\n",
        );
        let o = build(&e, &ctx, &[], None);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{err}");
        assert!(err.contains("something went wrong"), "{err}");
        assert!(err.contains("line 2: RUN echo something went wrong && exit 3: the command returned a non-zero code: 3"), "{err}");
        assert!(e.ps().is_empty(), "the build VM is removed");

        write(
            &ctx,
            "Dockerfile",
            "FROM local/base:1\nRUN sleep 60 & echo left running\n",
        );
        let t = Instant::now();
        let o = build(&e, &ctx, &[], None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(t.elapsed() < Duration::from_secs(30), "{:?}", t.elapsed());

        write(
            &ctx,
            "Dockerfile",
            "FROM local/base:1 AS first\nARG V=1\nRUN echo $V > v.txt\nFROM first\nRUN false\n",
        );
        let rec = e.path("vm.json");
        let o = build(
            &e,
            &ctx,
            &["--target", "first", "-t", "local/first:1"],
            Some(&rec),
        );
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(err.contains("[2/2] RUN echo $V > v.txt"), "{err}");
        assert!(!err.contains("RUN false"), "{err}");
        let o = build(&e, &ctx, &["--target", "first"], Some(&rec));
        assert!(
            text(&o.stderr).contains("[2/2] CACHED"),
            "{}",
            text(&o.stderr)
        );
        assert!(!rec.exists());
        let o = build(
            &e,
            &ctx,
            &["--target", "first", "--build-arg", "V=2"],
            Some(&rec),
        );
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(!err.contains("[2/2] CACHED"), "{err}");
        assert!(
            rec.is_file(),
            "another build argument value is another step"
        );

        write(&ctx, "Dockerfile", "FROM local/base:1\nONBUILD RUN make\n");
        let o = build(&e, &ctx, &[], None);
        assert_eq!(o.status.code(), Some(1));
        assert!(text(&o.stderr).contains("line 2"), "{}", text(&o.stderr));
        // JSON: 단계마다 한 줄, 끝에 만든 이미지.
        write(&ctx, "Dockerfile", "FROM local/base:1\nENV A=1\n");
        let o = build(&e, &ctx, &["--format", "json"], None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let lines: Vec<sbx_cli::output::BuildEvent> = text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.first().unwrap().status.as_deref(), Some("started"));
        let last = lines.last().unwrap();
        assert_eq!(last.kind, "built");
        assert!(last.id.as_deref().unwrap().starts_with("sha256:"));
    }

    /// `FROM scratch`와 COPY뿐인 빌드는 VM 없이 만든다. 같은 바탕의 두 빌드는 바탕 층을 함께 쓰고 `system df`는 그것을
    /// 한 번 센다. 빌드 캐시도 한 줄이고, `system prune`이 비운다.
    fn build_scratch_without_a_vm_and_shared_base_layers() {
        use sbx_cli::output::{DiskUsage, PruneReport};
        let (e, ctx) = build_env();
        write(
            &ctx,
            "Dockerfile",
            "FROM scratch\nCOPY src/ /\nCMD [\"/tool\"]\n",
        );
        write(&ctx, "src/tool", "#!/bin/sh\n");
        let rec = e.path("vm.json");
        let o = build(&e, &ctx, &["-t", "local/scratch:1"], Some(&rec));
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!rec.exists(), "FROM scratch with COPY needs no VM");

        for (n, word) in [("one", "1"), ("two", "2")] {
            let dir = e.path(n);
            std::fs::create_dir_all(&dir).unwrap();
            write(
                &dir,
                "Dockerfile",
                &format!("FROM local/base:1\nRUN echo {word} > f\n"),
            );
            let o = build(&e, &dir, &["-t", &format!("local/{n}:1")], None);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        }
        let o = e.sbx(&["--format", "json", "system", "df"]);
        let rows: Vec<DiskUsage> = serde_json::from_slice(&o.stdout).unwrap();
        let im = rows.iter().find(|r| r.kind == "images").unwrap();
        // 바탕 층(64 KiB)은 바탕 이미지와 두 빌드가 함께 쓰고 한 번만 센다.
        assert!(im.shared >= 64 * 1024, "{rows:?}");
        assert!(im.size < 2 * 64 * 1024 + 64 * 1024, "{rows:?}");
        let cache = rows.iter().find(|r| r.kind == "build cache").unwrap();
        assert!(cache.total >= 3, "{rows:?}");
        // 이미지를 지워도 캐시는 남는다. prune이 캐시를 비우면 그 층도 간다.
        let o = e.sbx(&["rmi", "local/one:1", "local/two:1", "local/scratch:1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = build(&e, &e.path("one"), &[], Some(&rec));
        assert!(
            text(&o.stderr).contains("[2/2] CACHED"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["--format", "json", "system", "prune", "-f"]);
        let r: PruneReport = serde_json::from_slice(&o.stdout).unwrap();
        assert!(r.build_cache >= 3, "{r:?}");
        let o = build(&e, &e.path("one"), &[], None);
        assert!(!text(&o.stderr).contains("CACHED"), "{}", text(&o.stderr));
    }

    /// `--cpus`와 `-m`은 스테이지의 빌드 VM 크기다(실행기가 받은 VM 설정에 그대로 간다). 없으면 1 vCPU, 512 MiB.
    fn build_vm_takes_cpus_and_memory() {
        let (e, ctx) = build_env();
        write(&ctx, "Dockerfile", "FROM local/base:1\nRUN true\n");
        let rec = e.path("vm.json");
        let vm = |args: &[&str]| {
            let o = build(&e, &ctx, args, Some(&rec));
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
            let vm: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&rec).expect("the RUN step needs a VM"))
                    .unwrap();
            (vm["vcpus"].as_u64(), vm["memory_mib"].as_u64())
        };
        assert_eq!(vm(&["--no-cache"]), (Some(1), Some(512)));
        assert_eq!(
            vm(&["--no-cache", "--cpus", "2", "-m", "1g"]),
            (Some(2), Some(1024))
        );
        assert_eq!(
            vm(&["--no-cache", "--memory", "768m"]),
            (Some(1), Some(768))
        );
    }

    /// 실패한 단계가 막힌 이름을 썼으면 그것을 열 `--allow`를 보인다(포트는 짐작하지 않는다).
    fn build_shows_blocked_names() {
        let (e, ctx) = build_env();
        let me = std::env::current_exe().unwrap();
        write(
            &ctx,
            "Dockerfile",
            &format!(
                "FROM local/base:1\nRUN {} net-probe {} blocked.invalid 443; exit 1\n",
                me.display(),
                e.path("data").display()
            ),
        );
        let o = build(
            &e,
            &ctx,
            &["--network", "allow", "--allow", "ok.invalid:443"],
            None,
        );
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{err}");
        assert!(
            err.contains("blocked: blocked.invalid — add --allow blocked.invalid:PORT"),
            "{err}"
        );
        assert!(!err.contains("network is closed"), "{err}");
        // 닫힌 네트워크(기본)에서도 물은 이름을 알고, 네트워크가 닫혀 있다고 말한다.
        let o = build(&e, &ctx, &[], None);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(1), "{err}");
        assert!(err.contains("blocked.invalid: rcode 3 []"), "{err}");
        assert!(
            err.contains(
                "blocked: blocked.invalid — add --network allow --allow blocked.invalid:PORT"
            ),
            "{err}"
        );
        assert!(err.contains("the build network is closed"), "{err}");
    }

    /// `sha256:<16진수>`.
    fn sha256_id(b: &[u8]) -> String {
        use sha2::Digest as _;
        let hex: String = sha2::Sha256::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect();
        format!("sha256:{hex}")
    }

    /// tar의 일반 파일들(경로 → 내용).
    fn tar_files(tar: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
        use std::io::Read;
        let mut out = std::collections::BTreeMap::new();
        for entry in tar::Archive::new(tar).entries().unwrap() {
            let mut entry = entry.unwrap();
            if !entry.header().entry_type().is_file() {
                continue;
            }
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut b = Vec::new();
            entry.read_to_end(&mut b).unwrap();
            out.insert(path, b);
        }
        out
    }

    /// `--format json build`의 줄들.
    fn build_events(o: &Output) -> Vec<sbx_cli::output::BuildEvent> {
        text(&o.stdout)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
            .collect()
    }

    /// 만든 이미지 하나를 sbx 저장소, `save`, OCI tar와 layout, `docker load`, 레지스트리로 내보낸다. 어디로 가든
    /// manifest digest가 같다: 다른 sbx가 들인 이미지 ID, 레지스트리의 digest, 두 번 올린 digest가 모두 같다.
    /// `type=docker`만 주면 sbx 저장소에 남지 않고(층 캐시는 남는다), docker가 없으면 빌드 전에 멈춘다.
    fn build_outputs_export_one_image_everywhere() {
        let (e, ctx) = build_env();
        write(
            &ctx,
            "Dockerfile",
            "FROM local/base:1\nRUN echo out > f.txt\nCMD [\"cat\", \"f.txt\"]\n",
        );
        let o = build(&e, &ctx, &["-t", "local/out:1"], None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // save를 두 번 해도 같은 바이트다.
        let (a, b) = (e.path("a.tar"), e.path("b.tar"));
        for p in [&a, &b] {
            let o = e.sbx(&["save", "-o", p.to_str().unwrap(), "local/out:1"]);
            assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        }
        let saved = std::fs::read(&a).unwrap();
        assert_eq!(
            saved,
            std::fs::read(&b).unwrap(),
            "save writes the same bytes"
        );
        // 다른 sbx가 들이면 그 manifest digest가 이미지 ID다.
        let other = Env::new();
        std::fs::write(other.path("a.tar"), &saved).unwrap();
        let o = other.sbx(&[
            "--format",
            "json",
            "load",
            "-i",
            other.path("a.tar").to_str().unwrap(),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let loaded: Vec<sbx_cli::output::Pulled> = serde_json::from_slice(&o.stdout).unwrap();
        let id = loaded[0].id.clone();

        // 저장하지 않는 같은 빌드(모두 캐시)를 OCI tar로: save와 같은 파일이다.
        let oci = e.path("oci.tar");
        let dest = format!("type=oci,dest={}", oci.display());
        let o = build(
            &e,
            &ctx,
            &["--format", "json", "-t", "local/out:1", "-o", &dest],
            None,
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let ev = build_events(&o);
        let built = ev.iter().find(|x| x.kind == "built").unwrap();
        assert_eq!(built.stored, Some(false));
        let exported = ev.iter().find(|x| x.kind == "exported").unwrap();
        assert_eq!(
            (exported.output.as_deref(), exported.id.as_deref()),
            (Some("oci"), Some(id.as_str()))
        );
        assert_eq!(std::fs::read(&oci).unwrap(), saved);
        // layout 디렉터리.
        let layout = e.path("layout");
        let dest = format!("type=oci,dest={}/", layout.display());
        let o = build(&e, &ctx, &["-o", &dest], None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(layout.join("index.json")).unwrap()).unwrap();
        assert_eq!(index["manifests"][0]["digest"], id.as_str());
        assert!(layout.join("oci-layout").is_file());
        assert!(layout.join("blobs/sha256").join(&id[7..]).is_file());
        assert!(!layout.join("manifest.json").exists());

        // type=docker: docker load에 흘린다(가짜 docker가 받은 것을 적는다). sbx 저장소에는 남지 않는다.
        let bin = e.path("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let got = e.path("docker-in.tar");
        std::fs::write(
            bin.join("docker"),
            format!(
                "#!/bin/sh\n[ \"$1\" = load ] || exit 2\ncat > {}\necho 'Loaded image: local/dk:1'\n",
                got.display()
            ),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("docker"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let dk = e.path("dk");
        write(
            &dk,
            "Dockerfile",
            "FROM local/base:1\nRUN echo docker > d.txt\n",
        );
        let rec = e.path("vm.json");
        let run_dk = |extra: &[&str], path: &str| {
            let mut all = vec!["build", "-t", "local/dk:1"];
            all.extend_from_slice(extra);
            all.push(dk.to_str().unwrap());
            let _ = std::fs::remove_file(&rec);
            e.cmd(&all)
                .env("PATH", path)
                .env("SBX_FAKE_RECORD", &rec)
                .output()
                .unwrap()
        };
        let o = run_dk(&["--load"], &path);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(err.contains("Loaded image: local/dk:1"), "{err}");
        let files = tar_files(&std::fs::read(&got).unwrap());
        let manifest = String::from_utf8(files["manifest.json"].clone()).unwrap();
        assert!(manifest.contains("local/dk:1"), "{manifest}");
        let o = e.sbx(&["--format", "json", "images"]);
        let list: Vec<sbx_cli::output::Image> = serde_json::from_slice(&o.stdout).unwrap();
        assert!(
            !list
                .iter()
                .any(|i| i.references.iter().any(|r| r.contains("local/dk"))),
            "{list:?}"
        );
        // 층 캐시는 남는다: 다시 하면 VM 없이.
        let o = run_dk(&["-o", "type=docker"], &path);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!rec.exists(), "the layer cache is still used");
        // docker가 없으면 빌드 전에 멈춘다.
        let empty = e.path("empty-path");
        std::fs::create_dir_all(&empty).unwrap();
        let o = run_dk(&["--load"], empty.to_str().unwrap());
        let err = text(&o.stderr);
        assert_ne!(o.status.code(), Some(0), "{err}");
        assert!(err.contains("docker is not on PATH"), "{err}");
        assert!(err.contains("type=docker,dest=FILE"), "{err}");
        assert!(!rec.exists());

        // 레지스트리: 올린 manifest의 digest가 같은 ID다. 두 번째는 보내지 않는다.
        let reg = registry();
        let host = format!("127.0.0.1:{}", reg.port);
        let one = format!("{host}/team/built:1");
        let o = build(&e, &ctx, &["--push", "-t", &one], None);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            out.contains(&format!(
                "The push refers to repository [{host}/team/built]"
            )),
            "{out}"
        );
        assert!(out.contains(&format!("1: digest: {id} size: ")), "{out}");
        let stored = |tag: &str| {
            reg.manifests
                .lock()
                .unwrap()
                .get(tag)
                .cloned()
                .unwrap_or_else(|| panic!("no {tag}"))
        };
        assert_eq!(sha256_id(&stored("team/built:1")), id);
        let two = format!("type=registry,name={host}/team/built:2");
        let o = build(&e, &ctx, &["-o", &two], None);
        let out = text(&o.stdout);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(out.contains(": Layer already exists"), "{out}");
        assert_eq!(stored("team/built:2"), stored("team/built:1"));
        // 저장해 둔 이미지를 sbx push로 올려도 같다.
        let three = format!("{host}/team/built:3");
        assert_eq!(
            e.sbx(&["tag", "local/out:1", &three]).status.code(),
            Some(0)
        );
        let o = e.sbx(&["push", &three]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(stored("team/built:3"), stored("team/built:1"));
        // 이름이 없으면 올릴 곳이 없다.
        let o = build(&e, &ctx, &["--push"], None);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("needs a name"),
            "{}",
            text(&o.stderr)
        );
        // 바탕 이미지를 지우고 정리해도 만든 이미지는 같은 바이트로 save되고 같은 digest로 올라간다(만든 이미지의
        // 기록이 바탕 층의 blob을 쥔다).
        assert_eq!(e.sbx(&["rmi", "local/base:1"]).status.code(), Some(0));
        let o = e.sbx(&["system", "prune", "-f"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let c = e.path("c.tar");
        let o = e.sbx(&["save", "-o", c.to_str().unwrap(), "local/out:1"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(std::fs::read(&c).unwrap(), saved);
        let four = format!("{host}/team/built:4");
        assert_eq!(e.sbx(&["tag", "local/out:1", &four]).status.code(), Some(0));
        let o = e.sbx(&["push", &four]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(stored("team/built:4"), stored("team/built:1"));
    }

    /// `build -f -`로 Dockerfile을 표준 입력에서 읽는 `build`.
    fn build_stdin(
        e: &Env,
        ctx: &Path,
        dockerfile: &str,
        args: &[&str],
        epoch: Option<&str>,
    ) -> Output {
        let mut all = vec!["build", "-f", "-"];
        all.extend_from_slice(args);
        all.push(ctx.to_str().unwrap());
        let mut c = e.cmd(&all);
        c.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(v) = epoch {
            c.env("SOURCE_DATE_EPOCH", v);
        }
        let mut child = c.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(dockerfile.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    /// `SOURCE_DATE_EPOCH`: config와 history의 시각이 그것이고, 빌드가 만든 층의 파일 시각이 그보다 늦지 않다. 같은
    /// 입력이면 캐시 없이 다시 만들어도 같은 바이트다(가짜 실행기에서; 문서는 같은 캐시까지만 약속한다).
    /// `--build-arg`가 환경 변수를 이긴다.
    fn build_source_date_epoch_fixes_the_digest() {
        use std::io::Read;
        let (e, ctx) = build_env();
        let df = "FROM local/base:1\nRUN echo x > made.txt\n";
        let tars: Vec<PathBuf> = (0..3).map(|i| e.path(&format!("e{i}.tar"))).collect();
        let dest = |p: &Path| format!("type=oci,dest={}", p.display());
        let o = build_stdin(
            &e,
            &ctx,
            df,
            &["--no-cache", "-o", &dest(&tars[0])],
            Some("1000000000"),
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!text(&o.stderr).contains("not used"), "{}", text(&o.stderr));
        let o = build_stdin(
            &e,
            &ctx,
            df,
            &["--no-cache", "-o", &dest(&tars[1])],
            Some("1000000000"),
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let first = std::fs::read(&tars[0]).unwrap();
        assert_eq!(first, std::fs::read(&tars[1]).unwrap());
        // --build-arg로 줘도 같다.
        let o = build_stdin(
            &e,
            &ctx,
            df,
            &[
                "--no-cache",
                "--build-arg",
                "SOURCE_DATE_EPOCH=1000000000",
                "-o",
                &dest(&tars[2]),
            ],
            None,
        );
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(first, std::fs::read(&tars[2]).unwrap());
        // config와 history의 시각, 맨 위 층(RUN)의 파일 시각.
        let files = tar_files(&first);
        let index: serde_json::Value = serde_json::from_slice(&files["index.json"]).unwrap();
        let blob = |d: &str| files[&format!("blobs/sha256/{}", &d[7..])].clone();
        let manifest: serde_json::Value =
            serde_json::from_slice(&blob(index["manifests"][0]["digest"].as_str().unwrap()))
                .unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&blob(manifest["config"]["digest"].as_str().unwrap())).unwrap();
        assert_eq!(config["created"], "2001-09-09T01:46:40Z");
        let history = config["history"].as_array().unwrap();
        assert_eq!(
            history.last().unwrap()["created"],
            "2001-09-09T01:46:40Z",
            "{history:?}"
        );
        let top = manifest["layers"].as_array().unwrap().last().unwrap()["digest"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut tar = Vec::new();
        flate2::read::GzDecoder::new(&blob(&top)[..])
            .read_to_end(&mut tar)
            .unwrap();
        let mut names = Vec::new();
        for entry in tar::Archive::new(&tar[..]).entries().unwrap() {
            let entry = entry.unwrap();
            let name = entry.path().unwrap().to_string_lossy().into_owned();
            assert!(
                entry.header().mtime().unwrap() <= 1_000_000_000,
                "{name}: {}",
                entry.header().mtime().unwrap()
            );
            names.push(name);
        }
        assert!(names.iter().any(|n| n.ends_with("made.txt")), "{names:?}");
        // --build-arg가 환경 변수를 이긴다. 틀린 값은 빌드 전에 거부한다.
        let o = build_stdin(
            &e,
            &ctx,
            df,
            &["--build-arg", "SOURCE_DATE_EPOCH=soon"],
            Some("1000000000"),
        );
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("Unix time"), "{}", text(&o.stderr));
    }

    /// 빌드 캐시 상한(`SBX_BUILD_CACHE_MAX`): 빌드가 끝나면 이미지가 쓰지 않는 캐시를 상한 밑으로 줄인다. 이미지가
    /// 쓰는 층의 캐시는 남는다. `builder prune`은 이미지가 쓰지 않는 캐시를, `-a`는 모두 지운다.
    fn build_cache_stays_under_its_cap() {
        use sbx_cli::output::BuildCachePruned;
        let (e, ctx) = build_env();
        write(
            &ctx,
            "Dockerfile",
            "FROM local/base:1\nRUN echo cap > c.txt\n",
        );
        let rec = e.path("vm.json");
        let tar = format!("type=oci,dest={}", e.path("c.tar").display());
        let run = |args: &[&str], max: &str| {
            let mut all = vec!["build"];
            all.extend_from_slice(args);
            all.push(ctx.to_str().unwrap());
            let _ = std::fs::remove_file(&rec);
            e.cmd(&all)
                .env("SBX_BUILD_CACHE_MAX", max)
                .env("SBX_FAKE_RECORD", &rec)
                .output()
                .unwrap()
        };
        // 저장하지 않는 빌드(층은 캐시만 쥔다), 상한 1바이트: 끝나면 지운다. 다음 빌드는 다시 만든다.
        let o = run(&["-o", &tar], "1");
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        assert!(err.contains("build cache: removed 1 old step(s)"), "{err}");
        let o = run(&["-o", &tar], "none");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(rec.is_file(), "the step was trimmed, so it runs again");
        let o = run(&["-o", &tar], "none");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!rec.exists(), "no cap: the step stays cached");
        // 틀린 상한은 경고만 한다.
        let o = run(&["-o", &tar], "lots");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("SBX_BUILD_CACHE_MAX=lots"),
            "{}",
            text(&o.stderr)
        );
        // 이미지가 쓰는 층의 캐시는 상한이 1이어도 남는다.
        let o = run(&["-t", "local/cap:1"], "1");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = run(&["-t", "local/cap:1"], "1");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(!rec.exists(), "a layer an image uses is not trimmed");
        // builder prune: 이미지가 쓰는 층의 캐시는 기본으로 남고, -a면 지운다.
        let o = e.sbx(&["--format", "json", "builder", "prune", "-f"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let p: BuildCachePruned = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(p.build_cache, 0, "{p:?}");
        let o = e.sbx(&["builder", "prune"]);
        assert_ne!(o.status.code(), Some(0), "asks first; no terminal means -f");
        assert!(text(&o.stderr).contains("-f"), "{}", text(&o.stderr));
        let o = e.sbx(&["--format", "json", "builder", "prune", "-a", "-f"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let p: BuildCachePruned = serde_json::from_slice(&o.stdout).unwrap();
        assert!(p.build_cache >= 1, "{p:?}");
        let o = run(&["-t", "local/cap:1"], "none");
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(rec.is_file(), "the cache is gone, so the step runs again");
        // 이미지를 지우면 그 층은 캐시만 쥔다: --keep-storage 0이면 지운다.
        assert_eq!(e.sbx(&["rmi", "local/cap:1"]).status.code(), Some(0));
        let o = e.sbx(&[
            "--format",
            "json",
            "builder",
            "prune",
            "-f",
            "--keep-storage",
            "0",
        ]);
        let p: BuildCachePruned = serde_json::from_slice(&o.stdout).unwrap();
        assert!(p.build_cache >= 1 && p.reclaimed > 0, "{p:?}");
        let o = e.sbx(&["builder", "prune", "-a", "-f", "--keep-storage", "1g"]);
        assert_ne!(o.status.code(), Some(0));
    }

    /// 실행기가 층 디스크를 빠뜨리면 게스트가 센 수가 달라 시작하지 않는다(보통의 run).
    fn a_missing_layer_disk_stops_the_start() {
        let e = Env::new();
        let (a, _) = oci_archive_layers("local/a:1", &[BASE, &[("a.txt", b"a")]]);
        load(&e, "a.tar", &a);
        let o = e
            .cmd(&["run", "--rm", "local/a:1", "true"])
            .env("SBX_FAKE_DROP_LAYER_DISK", "1")
            .output()
            .unwrap();
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("layer disks"),
            "{}",
            text(&o.stderr)
        );
    }

    /// config만 바꾸는 명령은 이미지 config에 쓰인다. `COPY --chown --chmod`는 층의 소유자와 권한이다. 파서 지시어
    /// `escape`를 따른다.
    fn build_writes_the_image_config() {
        let (e, ctx) = build_env();
        write(
            &ctx,
            "Dockerfile",
            "# escape=`\n\
             FROM local/base:1\n\
             SHELL [\"/bin/sh\", \"-ec\"]\n\
             LABEL a=b `\n      c=d\n\
             EXPOSE 80 53/udp\n\
             VOLUME /data\n\
             STOPSIGNAL SIGINT\n\
             HEALTHCHECK --interval=5s CMD true\n\
             MAINTAINER someone\n\
             COPY --chown=7:8 --chmod=600 src/a.txt /a.txt\n\
             ENTRYPOINT [\"/e\"]\n\
             USER 1:2\n",
        );
        write(&ctx, "src/a.txt", "a");
        let o = build(&e, &ctx, &["-q", "-t", "local/cfg:1"], None);
        let err = text(&o.stderr);
        assert_eq!(o.status.code(), Some(0), "{err}");
        let id = text(&o.stdout).trim().to_owned();
        let hex = id.strip_prefix("sha256:").unwrap();
        let doc: serde_json::Value = serde_json::from_slice(
            &std::fs::read(e.path(&format!("cache/images/{hex}/config.json"))).unwrap(),
        )
        .unwrap();
        let c = &doc["config"];
        assert_eq!(c["Shell"], serde_json::json!(["/bin/sh", "-ec"]));
        assert_eq!(c["Labels"]["a"], "b");
        assert_eq!(c["Labels"]["c"], "d");
        assert_eq!(
            c["ExposedPorts"],
            serde_json::json!({"53/udp": {}, "80/tcp": {}})
        );
        assert_eq!(c["Volumes"], serde_json::json!({"/data": {}}));
        assert_eq!(c["StopSignal"], "SIGINT");
        assert_eq!(
            c["Healthcheck"]["Test"],
            serde_json::json!(["CMD-SHELL", "true"])
        );
        assert_eq!(c["Healthcheck"]["Interval"], 5_000_000_000u64);
        assert_eq!(c["Entrypoint"], serde_json::json!(["/e"]));
        assert_eq!(
            c["Cmd"],
            serde_json::Value::Null,
            "ENTRYPOINT clears the base's Cmd"
        );
        assert_eq!(c["User"], "1:2");
        assert_eq!(doc["author"], "someone");
        // HEALTHCHECK는 쓰지만 돌리지 않는다고 알린다.
        let o = build(&e, &ctx, &[], None);
        assert!(
            text(&o.stderr).contains("HEALTHCHECK"),
            "{}",
            text(&o.stderr)
        );
        // COPY 층: 소유자와 권한.
        let ids = doc["rootfs"]["diff_ids"].as_array().unwrap();
        let top = ids
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .strip_prefix("sha256:")
            .unwrap();
        let disk = e.path(&format!("cache/layers/{top}/layer.erofs"));
        let read = sbx_rootfs::erofs::read::read_layer(&mut std::fs::File::open(&disk).unwrap(), 0)
            .unwrap();
        let a = read.entries.iter().find(|x| x.path == "a.txt").unwrap();
        assert_eq!((a.meta.uid, a.meta.gid, a.meta.mode), (7, 8, 0o600));
        // 만든 이미지의 기본값으로 돈다.
        let o = e.sbx(&["--format", "json", "images"]);
        let list: Vec<sbx_cli::output::Image> = serde_json::from_slice(&o.stdout).unwrap();
        let img = list.iter().find(|i| i.id == id).unwrap();
        assert_eq!(img.exposed_ports, ["53/udp", "80/tcp"]);
    }

    /// 그 이름의 샌드박스가 실행 중일 때까지 기다린다.
    fn wait_named(e: &Env, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !e
            .ps()
            .iter()
            .any(|s| s["name"] == name && s["state"] == "running")
        {
            assert!(Instant::now() < deadline, "{name} is not running");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `--format json volume inspect NAME`의 한 줄.
    fn volume_info(e: &Env, name: &str) -> sbx_cli::output::Volume {
        let o = e.sbx(&["--format", "json", "volume", "inspect", name]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        serde_json::from_slice(&o.stdout)
            .unwrap_or_else(|err| panic!("volume inspect: {err}: {}", text(&o.stdout)))
    }

    /// 볼륨이 샌드박스를 지워도 남고, 처음 붙을 때 이미지의 내용(가짜 실행기에서는 대상 경로의 지금 내용)으로 채워진다.
    /// `volume create|ls|inspect|rm|prune`도 본다.
    fn volume_data_outlives_the_sandbox() {
        let e = Env::new();
        let data = e.path("data-dir");
        // 만들기: 이름, 라벨, 크기. 같은 이름을 같은 옵션으로 다시 만들면 있는 것이다.
        let o = e.sbx(&[
            "volume", "create", "--opt", "size=1g", "--label", "app=db", "pgdata",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "pgdata\n");
        let o = e.sbx(&["volume", "create", "pgdata"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["volume", "create", "--opt", "size=2g", "pgdata"]);
        assert_eq!(
            o.status.code(),
            Some(1),
            "other options for an existing volume"
        );
        let o = e.sbx(&["volume", "create", "--opt", "type=tmpfs", "x"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("unknown volume option"),
            "{}",
            text(&o.stderr)
        );
        let v = volume_info(&e, "pgdata");
        assert_eq!((v.size, v.labels["app"].as_str()), (1 << 30, "db"));
        assert!(v.sandboxes.is_empty() && !v.in_use);
        // 쓰고, 샌드박스를 지우고, 새 샌드박스에서 읽는다.
        let o = e.sbx(&[
            "run",
            "--name",
            "db",
            "-v",
            &format!("pgdata:{}", data.display()),
            IMAGE,
            "sh",
            "-c",
            &format!("echo kept > {}/table", data.display()),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // 멈춘 샌드박스가 가리키는 볼륨은 지우지 않는다. 샌드박스를 지워도 볼륨은 남는다.
        let o = e.sbx(&["volume", "rm", "pgdata"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(
            text(&o.stderr).contains("in use by sandbox db"),
            "{}",
            text(&o.stderr)
        );
        assert_eq!(volume_info(&e, "pgdata").sandboxes.len(), 1);
        let o = e.sbx(&["rm", "db"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&[
            "run",
            "--rm",
            "-v",
            &format!("pgdata:{}:ro", data.display()),
            IMAGE,
            "cat",
            &format!("{}/table", data.display()),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "kept\n");

        // 처음 쓰는 이름은 만들고, 이미지의 내용을 옮겨 담는다(주인과 권한째). 다음부터는 볼륨의 내용이다.
        let seeded = e.path("seeded");
        std::fs::create_dir(&seeded).unwrap();
        std::fs::write(seeded.join("from-image"), b"seed").unwrap();
        let o = e.sbx(&[
            "run",
            "--rm",
            "--mount",
            &format!("type=volume,source=fresh,target={}", seeded.display()),
            IMAGE,
            "sh",
            "-c",
            &format!(
                "cat {s}/from-image; echo new > {s}/written",
                s = seeded.display()
            ),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "seed");
        let v = volume_info(&e, "fresh");
        assert_eq!(v.size, 16 << 30, "the default size");
        // nocopy: 이미지의 내용을 옮기지 않는다.
        let empty = e.path("nocopy");
        std::fs::create_dir(&empty).unwrap();
        std::fs::write(empty.join("from-image"), b"seed").unwrap();
        let o = e.sbx(&[
            "run",
            "--rm",
            "-v",
            &format!("blank:{}:nocopy", empty.display()),
            IMAGE,
            "ls",
            "-A",
            &empty.display().to_string(),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "");
        // 이름 없는 볼륨(`-v /path`)은 만들지 않는다.
        let o = e.sbx(&["run", "--rm", "-v", "/data", IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125));
        assert!(text(&o.stderr).contains("anonymous"), "{}", text(&o.stderr));

        // ls: 이름, -q, 필터, JSON.
        let o = e.sbx(&["volume", "ls", "-q"]);
        assert_eq!(text(&o.stdout), "blank\nfresh\npgdata\n");
        let o = e.sbx(&["volume", "ls", "-q", "--filter", "label=app=db"]);
        assert_eq!(text(&o.stdout), "pgdata\n");
        let o = e.sbx(&["volume", "ls", "--filter", "colour=red"]);
        assert_eq!(o.status.code(), Some(1));
        let o = e.sbx(&["--format", "json", "volume", "ls"]);
        let list: Vec<sbx_cli::output::Volume> = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(list.len(), 3);
        // prune: -a 없이는 지우는 것이 없다(이름 없는 볼륨이 없다). -a는 어느 샌드박스도 쓰지 않는 것을 지운다.
        let o = e.sbx(&["volume", "prune", "-f"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "Total reclaimed space: 0B\n");
        let o = e.sbx(&["volume", "prune", "-a"]);
        assert_ne!(
            o.status.code(),
            Some(0),
            "asks first, and stdin is not a terminal"
        );
        let o = e.sbx(&[
            "create",
            "-v",
            &format!("blank:{}", e.path("b").display()),
            IMAGE,
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["volume", "prune", "-a", "-f", "--filter", "label=app=db"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            text(&o.stdout).starts_with("Deleted Volumes:\npgdata\n"),
            "{}",
            text(&o.stdout)
        );
        let o = e.sbx(&["volume", "prune", "-a", "-f"]);
        assert!(
            text(&o.stdout).starts_with("Deleted Volumes:\nfresh\n"),
            "{}",
            text(&o.stdout)
        );
        let o = e.sbx(&["volume", "ls", "-q"]);
        assert_eq!(
            text(&o.stdout),
            "blank\n",
            "a created sandbox still points at blank"
        );
        // rm -f: 없는 이름은 오류가 아니다.
        let o = e.sbx(&["volume", "rm", "-f", "nothing"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["volume", "rm", "nothing"]);
        assert_eq!(o.status.code(), Some(1));
        // 볼륨 이벤트.
        let o = e.sbx(&[
            "events",
            "--since",
            "1h",
            "--until",
            "0s",
            "--filter",
            "type=volume",
            "--format",
            "json",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let events = text(&o.stdout);
        assert!(
            events.contains("\"create\"") && events.contains("\"remove\""),
            "{events}"
        );
    }

    /// 볼륨을 쓰기로 붙이는 샌드박스는 한 번에 하나다. 둘째는 잡고 있는 샌드박스를 말하며 거부되고, 첫째가 멈추거나
    /// 죽으면 풀린다. 읽기 전용끼리는 함께 붙는다. 실행 중인 샌드박스의 볼륨은 지우지 않는다.
    fn a_volume_has_one_writer_at_a_time() {
        let e = Env::new();
        let p = e.path("shared");
        let rw = format!("shared-vol:{}", p.display());
        let ro = format!("shared-vol:{}:ro", p.display());
        let o = e.sbx(&[
            "run", "-d", "--name", "first", "-v", &rw, IMAGE, "sleep", "30",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        wait_named(&e, "first");
        for spec in [&rw, &ro] {
            let o = e.sbx(&["run", "--rm", "-v", spec, IMAGE, "true"]);
            assert_eq!(o.status.code(), Some(125), "{spec}");
            assert!(
                text(&o.stderr).contains("volume shared-vol is in use by sandbox first"),
                "{}",
                text(&o.stderr)
            );
        }
        let o = e.sbx(&["volume", "rm", "shared-vol"]);
        assert_eq!(o.status.code(), Some(1));
        assert!(text(&o.stderr).contains("first"), "{}", text(&o.stderr));
        assert!(volume_info(&e, "shared-vol").in_use);
        // 멈추면 풀린다.
        let o = e.sbx(&["stop", "-t", "0", "first"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["run", "--rm", "-v", &rw, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // 읽기 전용끼리는 함께. 그동안 쓰기는 거부된다.
        let o = e.sbx(&[
            "run", "-d", "--name", "reader", "-v", &ro, IMAGE, "sleep", "30",
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        wait_named(&e, "reader");
        let o = e.sbx(&["run", "--rm", "-v", &ro, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["run", "--rm", "-v", &rw, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125));
        assert!(
            text(&o.stderr).contains("in use by sandbox reader"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["rm", "-f", "reader"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // 붙인 프로세스가 갑자기 죽어도 잠금이 함께 사라진다(kill -9).
        let mut child = e.spawn(&["run", "--name", "holder", "-v", &rw, IMAGE, "sleep", "30"]);
        wait_named(&e, "holder");
        let o = e.sbx(&["run", "--rm", "-v", &rw, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        child.kill().unwrap();
        child.wait().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let o = e.sbx(&["run", "--rm", "-v", &rw, IMAGE, "true"]);
            if o.status.code() == Some(0) {
                break;
            }
            assert!(Instant::now() < deadline, "{}", text(&o.stderr));
            std::thread::sleep(Duration::from_millis(100));
        }
        let o = e.sbx(&["rm", "-f", "holder", "first"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["volume", "rm", "shared-vol"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    }

    /// 볼륨 디스크는 층 디스크와 scratch 뒤에 역할 `volume`으로 붙는다. 층 디스크가 그만큼 줄어도(아래 층을 합친다)
    /// 게스트의 층 수 확인(`CheckLayers`)은 통과한다. 디스크가 모자라거나 실행기가 옛 규약(2)만 말하면 거부한다.
    fn volume_disks_limits_and_old_executors() {
        let e = Env::new();
        let (a, _) = oci_archive_layers(
            "local/deep:1",
            &[
                &[("0", b"0")],
                &[("1", b"1")],
                &[("2", b"2")],
                &[("3", b"3")],
            ],
        );
        load(&e, "deep.tar", &a);
        // 가짜 실행기의 게스트 경로는 호스트 경로다.
        let (a, b) = (e.path("a"), e.path("b"));
        let va = &*format!("vol-a:{}", a.display());
        let vb = &*format!("vol-b:{}", b.display());
        let vb_ro = &*format!("{vb}:ro");
        let rec = e.path("launch.json");
        let run = |volumes: &[&str], max_disks: Option<&str>, protocol: Option<&str>| {
            let _ = std::fs::remove_file(&rec);
            let mut args = vec!["run", "--rm"];
            for v in volumes {
                args.extend(["-v", v]);
            }
            args.extend(["local/deep:1", "true"]);
            let mut c = e.cmd(&args);
            c.env("SBX_FAKE_RECORD", &rec);
            if let Some(n) = max_disks {
                c.env("SBX_FAKE_MAX_DISKS", n);
            }
            if let Some(v) = protocol {
                c.env("SBX_FAKE_PROTOCOL", v);
            }
            c.output().unwrap()
        };
        let roles = |vm: &serde_json::Value| -> Vec<String> {
            vm["disks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|d| d["role"].as_str().unwrap().to_owned())
                .collect()
        };
        // 디스크 다섯: 층 둘(아래 셋을 합친 것과 맨 위), scratch, 볼륨 둘.
        let o = run(&[va, vb_ro], Some("5"), None);
        // 처음 쓰는 볼륨은 읽기 전용으로 붙지 않는다(읽을 것이 없다).
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("never been used"),
            "{}",
            text(&o.stderr)
        );
        let o = run(&[va, vb], Some("5"), None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let vm: serde_json::Value = serde_json::from_slice(&std::fs::read(&rec).unwrap()).unwrap();
        assert_eq!(
            roles(&vm),
            ["rootfs", "scratch", "rootfs", "volume", "volume"],
            "{vm}"
        );
        let disks = vm["disks"].as_array().unwrap();
        assert!(
            disks[0]["path"].as_str().unwrap().contains("/merged/"),
            "{vm}"
        );
        assert!(
            disks[3]["path"]
                .as_str()
                .unwrap()
                .ends_with("volumes/vol-a/disk.img"),
            "{vm}"
        );
        assert_eq!(disks[3]["readonly"], false);
        let o = run(&[va, vb_ro], Some("5"), None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let vm: serde_json::Value = serde_json::from_slice(&std::fs::read(&rec).unwrap()).unwrap();
        assert_eq!(
            vm["disks"][4]["readonly"], true,
            "read-only on the host too"
        );
        // 루트 파일 시스템 하나와 scratch를 빼고 남는 만큼만.
        let o = run(&[va, vb], Some("3"), None);
        assert_eq!(o.status.code(), Some(125));
        assert!(text(&o.stderr).contains("2 volumes"), "{}", text(&o.stderr));
        let o = run(&[va], Some("3"), None);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let vm: serde_json::Value = serde_json::from_slice(&std::fs::read(&rec).unwrap()).unwrap();
        assert_eq!(roles(&vm), ["rootfs", "scratch", "volume"], "{vm}");
        // 옛 규약만 말하는 실행기: 볼륨은 거부하고, 볼륨 없는 샌드박스는 그대로 돈다.
        let o = run(&[va], None, Some("2"));
        assert_eq!(o.status.code(), Some(125));
        assert!(
            text(&o.stderr).contains("older than 3"),
            "{}",
            text(&o.stderr)
        );
        let o = run(&[], None, Some("2"));
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let o = e.sbx(&["volume", "ls", "-q"]);
        assert_eq!(text(&o.stdout), "vol-a\nvol-b\n");
    }

    /// `sbx mcp`는 MCP로 만든 볼륨만 쓴다. 사용자가 CLI로 만든 볼륨은 붙이지 못하고, 수는 `--max-volumes`까지다.
    fn mcp_uses_only_its_own_volumes() {
        let e = Env::new();
        let o = e.sbx(&["volume", "create", "mine"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let mut child = e
            .cmd(&["mcp", "--max-volumes", "1", "--max-disk", "2g"])
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
        m.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
        let p = e.path("agent-data");
        let path = p.display().to_string();
        let err = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"], "volumes": [{"name": "mine", "path": path}]}),
            )
            .unwrap_err();
        assert!(err.contains("not created through MCP"), "{err}");
        // 호스트 경로는 받지 않는다.
        let err = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"], "volumes": [{"name": "/etc", "path": path}]}),
            )
            .unwrap_err();
        assert!(err.contains("volume name"), "{err}");
        let r = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["sh", "-c", format!("echo hi > {path}/f")],
                    "volumes": [{"name": "work", "path": path}]}),
            )
            .unwrap();
        assert_eq!(r["exit_code"], 0, "{r}");
        let r = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["cat", format!("{path}/f")],
                    "volumes": [{"name": "work", "path": path, "readonly": true}]}),
            )
            .unwrap();
        assert_eq!(r["stdout"], "hi\n", "{r}");
        let err = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"], "volumes": [{"name": "more", "path": path}]}),
            )
            .unwrap_err();
        assert!(err.contains("--max-volumes"), "{err}");
        drop(m);
        let v = volume_info(&e, "work");
        assert_eq!(v.labels["io.github.toollyst.sandbox.owner"], "mcp");
        assert_eq!(v.size, 2 << 30, "--max-disk");
        // 사용자의 볼륨은 그대로다.
        assert!(volume_info(&e, "mine").labels.is_empty());
    }

    /// 소유 프로세스가 죽으면 볼륨 잠금은 곧바로 풀리지만 VM은 실행기가 끌 때까지 조금 더 산다(`SBX_FAKE_LINGER_MS`가
    /// 그 틈을 흉내 낸다). 그사이 쓰기로 붙이려는 샌드박스는 그 VM이 꺼지기를 기다리고, 기다리는 시간(3초) 안에 꺼지지
    /// 않으면 그 샌드박스를 말하며 거부된다. 꺼지면 붙는다.
    fn a_volume_waits_for_the_vm_of_a_dead_owner() {
        let e = Env::new();
        let rw = format!("gap-vol:{}", e.path("gap").display());
        let mut child = e
            .cmd(&["run", "--name", "holder", "-v", &rw, IMAGE, "sleep", "60"])
            .env("SBX_FAKE_LINGER_MS", "5000")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_named(&e, "holder");
        child.kill().unwrap();
        child.wait().unwrap();
        let killed = Instant::now();
        // 기다려도 VM이 남아 있다: 거부.
        let o = e.sbx(&["run", "--rm", "-v", &rw, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(125), "{}", text(&o.stderr));
        assert!(
            text(&o.stderr).contains("still attached to sandbox holder"),
            "{}",
            text(&o.stderr)
        );
        assert!(killed.elapsed() >= Duration::from_secs(3), "it waited");
        // 다시 하면 그 VM이 꺼지기를 기다렸다가 붙는다.
        let o = e.sbx(&["run", "--rm", "-v", &rw, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(
            killed.elapsed() >= Duration::from_secs(5),
            "after the VM went"
        );
        // 읽기 전용끼리는 겹치지 않으므로 기다리지 않는다.
        let ro = format!("{rw}:ro");
        let mut child = e
            .cmd(&["run", "--name", "reader", "-v", &ro, IMAGE, "sleep", "60"])
            .env("SBX_FAKE_LINGER_MS", "5000")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        wait_named(&e, "reader");
        child.kill().unwrap();
        child.wait().unwrap();
        let started = Instant::now();
        let o = e.sbx(&["run", "--rm", "-v", &ro, IMAGE, "true"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert!(started.elapsed() < Duration::from_secs(3), "no wait");
        let o = e.sbx(&["rm", "-f", "holder", "reader"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    }

    /// 볼륨과 `--connect`를 함께: 볼륨을 쓰기로 붙인 `db`에 다른 샌드박스가 이름으로 닿고, 그 샌드박스도 제 볼륨을
    /// 붙인다. `db`의 볼륨은 그동안 다른 쪽이 붙이지 못한다. MCP에서는 볼륨 범위와 잇기 범위가 함께 걸린다.
    fn volumes_and_connect_together() {
        let e = Env::new();
        let data = e.path("data");
        let port = echo_service();
        let db_path = e.path("dbdata");
        let app_path = e.path("appdata");
        let db_vol = format!("dbdata:{}", db_path.display());
        let o = e.sbx(&[
            "run",
            "-d",
            "--name",
            "db",
            "-v",
            &db_vol,
            IMAGE,
            "sh",
            "-c",
            &format!("echo row > {}/t; sleep 60", db_path.display()),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        wait_named(&e, "db");
        let connect = format!("db:{port}");
        let script = format!(
            "{}; echo seen > {}/f",
            probe_script(&data, "app", &[("db", port)]),
            app_path.display()
        );
        let app_vol = format!("appdata:{}", app_path.display());
        let o = e.sbx(&[
            "run",
            "--rm",
            "--name",
            "app",
            "--connect",
            &connect,
            "-v",
            &app_vol,
            IMAGE,
            "sh",
            "-c",
            &script,
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(
            text(&o.stdout),
            format!("db: rcode 0 [198.18.0.1] port {port}: got \"hello\\n\"\n")
        );
        // 잇는 쪽이 대상의 볼륨까지 붙일 수는 없다(쓰는 쪽이 있다).
        let o = e.sbx(&[
            "run",
            "--rm",
            "--connect",
            &connect,
            "-v",
            &format!("{db_vol}:ro"),
            IMAGE,
            "true",
        ]);
        assert_eq!(o.status.code(), Some(125));
        assert!(
            text(&o.stderr).contains("in use by sandbox db"),
            "{}",
            text(&o.stderr)
        );
        let o = e.sbx(&["rm", "-f", "db"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        // 두 볼륨 모두 남는다.
        let o = e.sbx(&[
            "run",
            "--rm",
            "-v",
            &format!("{db_vol}:ro"),
            "-v",
            &format!("{app_vol}:ro"),
            IMAGE,
            "cat",
            &format!("{}/t", db_path.display()),
            &format!("{}/f", app_path.display()),
        ]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        assert_eq!(text(&o.stdout), "row\nseen\n");

        // MCP: 사용자의 볼륨도 사용자의 샌드박스도 쓰지 못하고, MCP로 만든 것끼리는 둘 다 된다.
        let o = e.sbx(&["run", "-d", "--name", "userdb", IMAGE, "sleep", "60"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
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
        m.request("initialize", serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}));
        let mpath = e.path("mcp-data").display().to_string();
        let err = m
            .call(
                "create",
                serde_json::json!({"image": IMAGE, "connect": [format!("userdb:{port}")],
                    "volumes": [{"name": "agentvol", "path": mpath}]}),
            )
            .unwrap_err();
        assert!(err.contains("created through MCP"), "{err}");
        let err = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "command": ["true"], "volumes": [{"name": "dbdata", "path": mpath}]}),
            )
            .unwrap_err();
        assert!(err.contains("not created through MCP"), "{err}");
        m.call(
            "create",
            serde_json::json!({"image": IMAGE, "name": "mdb", "command": ["sleep", "60"],
                "volumes": [{"name": "mdbdata", "path": e.path("mdb").display().to_string()}]}),
        )
        .unwrap();
        let r = m
            .call(
                "run",
                serde_json::json!({"image": IMAGE, "connect": [format!("mdb:{port}")],
                    "command": ["sh", "-c", format!("echo ok > {mpath}/f")],
                    "volumes": [{"name": "agentvol", "path": mpath}]}),
            )
            .unwrap();
        assert_eq!(r["exit_code"], 0, "{r}");
        drop(m);
        let v = volume_info(&e, "agentvol");
        assert_eq!(v.labels["io.github.toollyst.sandbox.owner"], "mcp");
        let o = e.sbx(&["rm", "-f", "userdb", "mdb"]);
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    }

    /// 실행 중인 샌드박스가 쓰는 실행기·어댑터는 `executor prune`이 남기고(알린다) `rm`이 거부한다(그 샌드박스를 댄다).
    /// 새 버전을 깔고 `executor use`로 바꿔도(설치 스크립트가 하는 길) 옛 버전으로 도는 VM은 그대로다. `rm -f`는 경고하고
    /// 지우며, 도는 VM은 멈추기 전까지 계속 쓸 수 있다.
    fn executors_in_use_are_kept() {
        let e = Env::new();
        let me = std::env::current_exe().unwrap();
        let me = me.to_str().unwrap();
        let without = |args: &[&str]| {
            let mut c = e.cmd(args);
            c.env_remove("SBX_EXECUTOR");
            c.output().unwrap()
        };
        let ok = |o: &Output| assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        ok(&without(&["executor", "install", "--from", me, "fake@1.0"]));
        let o = without(&["run", "-d", "--name", "old", IMAGE, "sleep", "60"]);
        ok(&o);
        wait_named(&e, "old");
        // 새 버전으로 바꾼다. 옛 버전으로 도는 샌드박스는 그대로 돈다.
        ok(&without(&["executor", "install", "--from", me, "fake@2.0"]));
        ok(&without(&["executor", "use", "fake@2.0"]));
        let o = without(&["executor", "ls", "--format", "json"]);
        ok(&o);
        let list: Vec<sbx_cli::output::InstalledExecutor> =
            serde_json::from_slice(&o.stdout).unwrap();
        let running: Vec<(&str, Vec<String>)> = list
            .iter()
            .map(|i| (i.version.as_str(), i.running.clone()))
            .collect();
        assert_eq!(running, [("1.0", vec!["old".to_owned()]), ("2.0", vec![])]);
        // prune은 남긴다(샌드박스가 고정한 버전이기도 하다).
        let o = without(&["executor", "prune"]);
        ok(&o);
        assert!(!text(&o.stdout).contains("fake@1.0"), "{}", text(&o.stdout));
        // rm은 거부한다.
        let o = without(&["executor", "rm", "fake@1.0"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("executor fake@1.0 is in use by running sandbox old"),
            "{}",
            text(&o.stderr)
        );
        ok(&without(&["exec", "old", "true"]));
        // -f는 경고하고 지운다. 도는 VM은 멈추기 전까지 그대로 쓴다.
        let o = without(&["executor", "rm", "-f", "fake@1.0"]);
        ok(&o);
        assert!(
            text(&o.stderr).contains("can't be started with it again"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&["exec", "old", "echo", "still"]);
        ok(&o);
        assert_eq!(text(&o.stdout), "still\n");
        ok(&without(&["stop", "-t", "0", "old"]));
        // 그 버전으로는 다시 시작하지 못한다(고정한 실행기가 없다).
        let o = without(&["start", "old"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(text(&o.stderr).contains("fake@1.0"), "{}", text(&o.stderr));
        ok(&without(&["rm", "-f", "old"]));
        ok(&without(&["executor", "rm", "fake@2.0"]));

        // 어댑터: 실행 중인 샌드박스가 쓰는 어댑터 버전은 다른 버전이 있어도 지우지 않는다.
        let vmm = e.path("wick-src");
        std::fs::create_dir_all(&vmm).unwrap();
        std::fs::write(vmm.join("vmm"), "x").unwrap();
        let vmm = vmm.to_str().unwrap();
        ok(&without(&[
            "executor",
            "adapter",
            "install",
            "--from",
            me,
            "fakead@0.1",
        ]));
        ok(&without(&[
            "executor",
            "install",
            "--from",
            vmm,
            "--adapter",
            "fakead",
            "wick@1.0",
        ]));
        ok(&without(&[
            "run", "-d", "--name", "vm", IMAGE, "sleep", "60",
        ]));
        wait_named(&e, "vm");
        ok(&without(&[
            "executor",
            "adapter",
            "install",
            "--from",
            me,
            "fakead@0.2",
        ]));
        let o = without(&["executor", "adapter", "ls", "--format", "json"]);
        ok(&o);
        let list: Vec<sbx_cli::output::InstalledAdapter> =
            serde_json::from_slice(&o.stdout).unwrap();
        let running: Vec<(&str, Vec<String>)> = list
            .iter()
            .map(|i| (i.version.as_str(), i.running.clone()))
            .collect();
        assert_eq!(running, [("0.1", vec!["vm".to_owned()]), ("0.2", vec![])]);
        let o = without(&["executor", "adapter", "rm", "fakead@0.1"]);
        assert_ne!(o.status.code(), Some(0));
        assert!(
            text(&o.stderr).contains("adapter fakead@0.1 is in use by running sandbox vm"),
            "{}",
            text(&o.stderr)
        );
        let o = without(&["executor", "prune"]);
        ok(&o);
        assert!(
            text(&o.stderr).contains("kept adapter fakead@0.1: in use by running sandbox vm"),
            "{}",
            text(&o.stderr)
        );
        // 실행기 쪽에서도 VMM 묶음은 쓰는 중이다.
        let o = without(&["executor", "rm", "wick@1.0"]);
        assert!(
            text(&o.stderr).contains("in use by running sandbox vm"),
            "{}",
            text(&o.stderr)
        );
        ok(&without(&["rm", "-f", "vm"]));
        let o = without(&["executor", "prune"]);
        ok(&o);
        assert!(
            text(&o.stdout).contains("adapter fakead@0.1"),
            "{}",
            text(&o.stdout)
        );
    }
}
