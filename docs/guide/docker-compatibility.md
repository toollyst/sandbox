# Docker compatibility

<!-- Generated from crates/app/cli/src/compat.rs. Do not edit: run `SBX_BLESS=1 cargo test -p sbx-cli --lib compat`. -->

`sbx` takes Docker's commands and flags, so what you know from `docker` carries over. This page lists, for every Docker command, every flag of the commands sbx has, every Dockerfile instruction and the common Compose keys, whether sbx does the same, what is different, and the test that shows it.

Each sandbox is its own microVM, there is no daemon, and the network is closed until you open it. So some Docker features mean nothing here, a few are left out on purpose, and some work a little differently. Where sbx differs, it's deliberate, and the row says why.

The table is code, [`compat.rs`](../../crates/app/cli/src/compat.rs), and a test writes this page from it. The same tests check the table against sbx's command-line definition: a row can't say a flag works unless sbx accepts it and the test it names exists, a row can't say a flag is missing once sbx accepts it, and every sbx command and flag needs a row. So the page can't drift from the code.

**Legend**

| Icon | Meaning |
| --- | --- |
| 🟢 | Same as Docker |
| 🟡 | Works, with the differences given |
| 🔴 | Not yet. The last column names the [roadmap](../roadmap.md) step that adds it in 0.3, *after 0.3*, or *not planned yet* |
| ⚫ | Will not do, for the reason given |
| ⚪ | Not applicable: means nothing when each sandbox is its own VM and there is no daemon |
| ➕ | sbx only: not in Docker |

**Evidence.** A name such as `run_i_forwards_stdin` is an end-to-end test in [`tests/e2e.rs`](../../crates/app/cli/tests/e2e.rs): it runs the `sbx` binary with a fake executor, without a VM, on Linux and macOS in CI. A name with `::`, such as `parse::tests::memory_units`, is a unit test in [`crates/app/cli/src`](../../crates/app/cli/src). Real VMs are covered by the smoke tests ([testing](../developers/testing-and-releasing.md)).

## Progress

<!-- compat-progress:begin -->
Counted from the 395 rows below. "What applies to sbx" leaves out ⚫, ⚪ and ➕ rows.

| | |
| --- | --- |
| **Works (🟢 or 🟡), of what applies to sbx** | **`███░░░░░░░` 34% (108 of 312)** |
| **The Docker CLI alone** (without `build`, the Dockerfile and Compose, which come in 0.3 and after) | **`██████░░░░` 56% (108 of 190)** |
| 🟢 Same as Docker | 46 |
| 🟡 Works, with differences | 62 |
| 🔴 Not yet | 204 (42 of them in 0.3) |
| ⚫ Will not do | 23 |
| ⚪ Not applicable in a VM | 39 |
| ➕ sbx only | 21 |

| Area | Works, of what applies to sbx |
| --- | --- |
| [Commands](#commands) | `█████░░░░░` 51% (30 of 58) |
| [Global options](#global-options) | `███░░░░░░░` 25% (1 of 4) |
| [docker run, docker create](#docker-run-docker-create) | `█████░░░░░` 46% (23 of 49) |
| [docker exec](#docker-exec) | `█████████░` 87% (7 of 8) |
| [docker start](#docker-start) | `█████░░░░░` 50% (2 of 4) |
| [docker stop, docker restart](#docker-stop-docker-restart) | `██████████` 100% (2 of 2) |
| [docker kill](#docker-kill) | `██████████` 100% (1 of 1) |
| [docker rm](#docker-rm) | `██████████` 100% (1 of 1) |
| [docker ps](#docker-ps) | `█████░░░░░` 50% (4 of 8) |
| [docker inspect](#docker-inspect) | `███░░░░░░░` 33% (1 of 3) |
| [docker stats](#docker-stats) | `█████░░░░░` 50% (2 of 4) |
| [docker attach](#docker-attach) | `██████████` 100% (3 of 3) |
| [docker logs](#docker-logs) | `████████░░` 83% (5 of 6) |
| [docker cp](#docker-cp) | `░░░░░░░░░░` 0% (0 of 3) |
| [docker events](#docker-events) | `██████████` 100% (4 of 4) |
| [docker images](#docker-images) | `█████░░░░░` 50% (3 of 6) |
| [docker pull](#docker-pull) | `███░░░░░░░` 33% (1 of 3) |
| [docker push](#docker-push) | `███████░░░` 66% (2 of 3) |
| [docker rmi](#docker-rmi) | `██████████` 100% (1 of 1) |
| [docker save](#docker-save) | `█████░░░░░` 50% (1 of 2) |
| [docker load](#docker-load) | `███████░░░` 66% (2 of 3) |
| [docker login](#docker-login) | `██████████` 100% (2 of 2) |
| [docker system df](#docker-system-df) | `█████░░░░░` 50% (1 of 2) |
| [docker system prune](#docker-system-prune) | `████████░░` 75% (3 of 4) |
| [Behavior](#behavior) | `██████████` 100% (6 of 6) |
| [docker build](#docker-build) | `░░░░░░░░░░` 0% (0 of 27) |
| [Dockerfile](#dockerfile) | `░░░░░░░░░░` 0% (0 of 33) |
| [docker compose](#docker-compose) | `░░░░░░░░░░` 0% (0 of 28) |
| [compose.yaml](#composeyaml) | `░░░░░░░░░░` 0% (0 of 34) |
<!-- compat-progress:end -->

## Commands

Every `docker` command and management command. The flags of the commands sbx has are in the sections below.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `run` | 🟡 | Each sandbox is a microVM. Pulls and prepares the image on first use. | `run_streams_output_and_returns_the_exit_code` |
| `create` | 🟡 | `create --rm` is refused: only `run` removes a sandbox when it exits. | `create_start_inspect_rm` |
| `start` | 🟢 | A stopped sandbox keeps its disk and starts again, as in Docker (unlike the OCI runtime spec). | `create_start_inspect_rm` |
| `stop` | 🟡 | Sends the stop signal, then powers the guest off (sync, exit code 137) instead of SIGKILL. *Why:* A sandbox is a VM: stopping it is shutting the VM down. | `exec_cp_stop_on_a_running_sandbox`, `stop_sends_the_stop_signal_then_powers_off` |
| `restart` | 🟡 | Stops as `stop` does, then starts detached. | `kill_wait_and_restart` |
| `kill` | 🟢 |  | `kill_wait_and_restart` |
| `wait` | 🟢 |  | `kill_wait_and_restart`, `json_output_contract` |
| `rm` | 🟡 | `rm -f` powers a running sandbox off without a signal. | `exec_cp_stop_on_a_running_sandbox`, `system_df_and_prune_free_what_nothing_uses` |
| `ps` | 🟡 | Sandboxes without `--name` have no name (Docker makes one up). | `names_work_in_place_of_ids` |
| `inspect` | 🟡 | Sandboxes only, one at a time, in sbx's own JSON shape ([cli-output.md](../developers/cli-output.md)). | `create_start_inspect_rm`, `json_output_contract` |
| `stats` | 🟡 | CPU, memory, writable disk and process count, as the guest reports them. No network or block I/O columns. | `stats_reports_guest_usage`, `app::stats::tests::cpu_percent_is_per_vcpu_like_docker` |
| `exec` | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `attach` | 🟡 | Only to a detached sandbox (`run -d`, `start`), not to one another `sbx` runs attached. Not on Windows yet. *Why:* The shim that owns a detached sandbox holds its main process's input and output; there is no daemon. | `attach_to_a_detached_sandbox` |
| `logs` | 🟡 | Only for detached sandboxes. Up to 256 MiB per sandbox; past that, output is dropped and one line says so (Docker's json-file has no limit). *Why:* No daemon collects the output: the shim writes it. An attached `run` shows it directly and keeps nothing. | `detached_run_outlives_sbx_and_keeps_logs`, `detached_rm_and_logs_follow` |
| `cp` | 🟡 | Only with a running sandbox. Copying out never writes outside the destination you name. *Why:* The guest reads and writes its disk, so it has to be running. | `exec_cp_stop_on_a_running_sandbox` |
| `pull` | 🟡 | Also follows the containers `registries.conf` (mirrors, blocked registries) and `policy.json` (cosign signatures). | `registries_conf_is_read_for_pulls`, `policy_json_is_followed` |
| `images` | 🟡 | Also lists which root filesystem disks are prepared, and SIZE / SHARED / UNIQUE on disk (layers several images use are shared, as in `docker system df -v`). | `names_work_in_place_of_ids`, `json_output_contract`, `images_on_one_base_store_it_once` |
| `rmi` | 🟡 | Never removes the image of a running sandbox, even with `-f`. | `json_output_contract`, `docker_options_reach_the_sandbox`, `behavior::tests::rmi_table` |
| `save` | 🟡 | One image per archive, written as an OCI image layout (with Docker's `manifest.json`), exactly as it was pulled. | `load_and_save_round_trip_an_oci_archive` |
| `load` | 🟡 | Reads OCI image layout archives or directories: `docker save` from Docker 25 on, `podman save --format oci-archive`, skopeo. | `load_and_save_round_trip_an_oci_archive` |
| `login` | 🟢 | Writes Docker's `config.json` or its credential helper, so `docker login` works too. | `login_and_logout_use_dockers_config` |
| `logout` | 🟢 |  | `login_and_logout_use_dockers_config` |
| `events` | 🟡 | Kinds `sandbox`, `image` and `network`; actions create, start, stop, remove, `exec_start`, `exec_die`, pull, prepare, tag, untag, push, delete. RFC 3339 times with nanoseconds. Keeps the last 1-2 MiB (Docker: the last 256 events). *Why:* No daemon: the process that changes a state records it, so a sandbox whose owner dies suddenly has no stop event. | `events_replay_and_follow`, `tag_and_push_to_a_registry` |
| `info` | 🟡 | Shows the host, the executor and the storage sbx uses. | `commands_that_do_not_launch_need_no_executor` |
| `version` | 🟡 | sbx: `--version`. There is no `version` command. | `docker_options_reach_the_sandbox` |
| `system` | 🟡 | `df` and `prune`. | `system_df_and_prune_free_what_nothing_uses` |
| `system df` | 🟡 | Images, sandboxes and executors, by what is actually on disk. A layer several images use is counted once; `SHARED` is that part. | `system_df_and_prune_free_what_nothing_uses`, `images_on_one_base_store_it_once` |
| `system prune` | 🟡 | Stopped sandboxes, unused layers (blobs and layer disks) and leftovers of interrupted pulls older than an hour. | `system_df_and_prune_free_what_nothing_uses` |
| `system events` | 🔴 | `sbx events` does this. | not planned yet |
| `system info` | 🔴 | `sbx info` does this. | not planned yet |
| `build` | 🔴 |  | D3 (0.3) |
| `buildx build` | 🔴 | `sbx build` takes buildx's flags. | D3 (0.3) |
| `builder` | 🔴 | `sbx system prune` will clear the build cache (D3). | not planned yet |
| `push` | 🟡 | Sends the layers, config and manifest exactly as they were pulled, so the image keeps its digest; one blob at a time (Docker sends several at once). Only to the registry the name points at: never to a `registries.conf` mirror, and a blocked registry is refused. | `tag_and_push_to_a_registry`, `app::push::tests::report_reads_like_docker_push` |
| `tag` | 🟢 | Moves a reference that pointed at another image, as Docker does, and records an image `tag` event. | `tag_and_push_to_a_registry` |
| `volume` | 🔴 | Named volumes: a sparse disk per volume. | V1 (0.3) |
| `volume create` | 🔴 |  | V1 (0.3) |
| `volume ls` | 🔴 |  | V1 (0.3) |
| `volume inspect` | 🔴 |  | V1 (0.3) |
| `volume rm` | 🔴 |  | V1 (0.3) |
| `volume prune` | 🔴 |  | V1 (0.3) |
| `compose` | 🔴 |  | C1 (after 0.3) |
| `commit` | 🔴 |  | after 0.3 |
| `diff` | 🔴 |  | after 0.3 |
| `history` | 🔴 |  | after 0.3 |
| `rename` | 🔴 |  | after 0.3 |
| `top` | 🔴 |  | after 0.3 |
| `port` | 🔴 | `sbx inspect` shows the published ports. | after 0.3 |
| `update` | 🔴 |  | after 0.3 |
| `pause` | 🔴 | When an executor can pause a VM. | after 0.3 |
| `unpause` | 🔴 | When an executor can pause a VM. | after 0.3 |
| `checkpoint` | 🔴 | Snapshots, when an executor offers them. | after 0.3 |
| `search` | 🔴 |  | not planned yet |
| `export` | 🔴 |  | not planned yet |
| `import` | 🔴 |  | not planned yet |
| `manifest` | 🔴 |  | not planned yet |
| `container` | 🔴 | The same commands exist at the top level (`sbx ps`, `sbx rm`). | not planned yet |
| `image` | 🔴 | The same commands exist at the top level (`sbx images`, `sbx rmi`). | not planned yet |
| `network` | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect NAME:PORT` (V2) opens one port of another sandbox instead. |  |
| `context` | ⚪ | `--executor` and `sbx executor use` pick what runs the VMs. *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `plugin` | ⚫ | *Why:* Extensions are out of scope. |  |
| `trust` | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `swarm` | ⚫ | *Why:* Swarm is out of scope. |  |
| `node` | ⚫ | *Why:* Swarm is out of scope. |  |
| `service` | ⚫ | *Why:* Swarm is out of scope. |  |
| `stack` | ⚫ | *Why:* Swarm is out of scope. |  |
| `secret` | ⚫ | *Why:* Swarm is out of scope. |  |
| `config` | ⚫ | *Why:* Swarm is out of scope. |  |
| `prepare` | ➕ | Builds an image's layer disks ahead of time, one per layer and shared between images, for the executor `run` would use (`run` does it on first use). |  |
| `tui` | ➕ | Sandboxes, images, events and executors on one screen, like Docker Desktop or lazydocker. Every key runs an `sbx` command and shows its command line. |  |
| `mcp` | ➕ | Serves sandboxes to AI agents over the Model Context Protocol ([guide](mcp.md)). |  |
| `executor` | ➕ | Installs and picks executors, the programs that run the VMs ([guide](executors.md)). |  |
| `upgrade` | ➕ | Replaces sbx with the newest signed release. |  |
| `completion` | ➕ | Prints a shell completion script. On each TAB the shell runs `COMPLETE=<shell> sbx -- <words>`, which completes sandbox names and IDs, image references and executors, and changes nothing. So with `COMPLETE` set (not empty, not 0), sbx only completes. |  |

## Global options

Options that go before the command (`docker --debug ps`).

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-v, --version` | 🟡 | sbx: `-V, --version`. `-v` is not an option. | `docker_options_reach_the_sandbox` |
| `--config` | 🔴 | sbx reads `DOCKER_CONFIG` for registry credentials. | not planned yet |
| `-D, --debug` | 🔴 |  | not planned yet |
| `-l, --log-level` | 🔴 |  | not planned yet |
| `-c, --context` | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `-H, --host` | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--tls, --tlscacert, --tlscert, --tlskey, --tlsverify` | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--format` | ➕ | `text` or `json` for every command; the JSON shapes are a public contract ([cli-output.md](../developers/cli-output.md)). *Why:* A Go template engine can't be reproduced exactly in Rust, and a near copy would surprise. jq is the common language for JSON. |  |
| `--jq` | ➕ | With `--format json`: a built-in jq expression picks fields (like `gh`). |  |
| `--data-dir` | ➕ | Where sandbox state lives. |  |
| `--cache-dir` | ➕ | Where images and their disks are cached. |  |
| `--socket-dir` | ➕ | Where guest connection sockets go. |  |
| `--guest-init` | ➕ | Another guest init initramfs than the one built into sbx. |  |
| `--executor` | ➕ | The executor that runs the VM. |  |
| `--shim` | ➕ | The program that owns detached sandboxes. |  |

## docker run, docker create

`create` takes the same flags as `run`, except `-d`. Many of Docker's flags tune namespaces and cgroups on a shared kernel; in sbx each sandbox has its own kernel, and the VM's size is the limit.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-d, --detach` | 🟡 | A small `sbx-shim` process owns each detached sandbox. *Why:* There is no daemon; a VM is owned by the process that started it. | `detached_run_outlives_sbx_and_keeps_logs` |
| `--entrypoint` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-e, --env` | 🟢 |  | `names_work_in_place_of_ids`, `parse::tests::env_values` |
| `--env-file` | 🟢 |  | `names_work_in_place_of_ids`, `parse::tests::env_files_follow_docker` |
| `-h, --hostname` | 🟡 | sbx: `--hostname`. `-h` is help. | `docker_options_reach_the_sandbox` |
| `-i, --interactive` | 🟢 |  | `run_i_forwards_stdin` |
| `-l, --label` | 🟢 |  | `filters`, `docker_options_reach_the_sandbox` |
| `-m, --memory` | 🟡 | 512 MiB by default (Docker: no limit). *Why:* A VM is started with a fixed size. | `docker_options_reach_the_sandbox`, `parse::tests::memory_units` |
| `--cpus` | 🟡 | Whole vCPUs only; 1 by default (Docker: no limit). *Why:* A VM is started with a fixed number of vCPUs. | `docker_options_reach_the_sandbox`, `parse::tests::cpus_must_be_whole` |
| `--mount` | 🟡 | `type=bind` (needs an executor with live sharing; an error otherwise) and `type=copy` (below). No `volume` or `tmpfs` yet. | `docker_options_reach_the_sandbox`, `parse::tests::mounts` |
| `--name` | 🟡 | Without it a sandbox has no name and is found by its ID (Docker makes a name up). *Why:* A made-up name shows up for people who don't use names and only adds clashes. A caller that wants one picks it. | `names_work_in_place_of_ids` |
| `--network` | 🟡 | `none` (the default; Docker's is `bridge`) or `allow`: TCP to the `--allow` destinations only, through an HTTP proxy in the guest that the host checks. *Why:* The network is closed by default and opened by name. The allow list is enforced on every OS without help from the executor. | `network_allow_list`, `behavior::tests::network_policy_table` |
| `--network bridge, host, container:NAME, NAME` | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect NAME:PORT` (V2) opens one port of another sandbox instead. |  |
| `-p, --publish` | 🟡 | Without an IP it listens on 127.0.0.1 (Docker: every address). TCP only. The process that owns the VM listens, and connects to 127.0.0.1:PORT in the guest. *Why:* Closed by default: write `0.0.0.0:` to open a port beyond this machine. The VM has no network interface, so the port is reached on the guest's loopback. | `published_ports_reach_the_sandbox` |
| `-p, --publish …/udp` | 🔴 |  | after 0.3 |
| `--pull` | 🟢 |  | `run_failures_before_the_command_are_125`, `behavior::tests::pull_policy_default_matches_docker` |
| `--rm` | 🟡 | `run` only: `create --rm` is refused. | `run_streams_output_and_returns_the_exit_code`, `create_start_inspect_rm` |
| `--runtime` | 🟡 | sbx: `--executor`. A global option that picks the program that runs the VM. | `installed_executor_is_used_by_default`, `vmm_packages_pins_and_version_files` |
| `--stop-signal` | 🟢 |  | `stop_sends_the_stop_signal_then_powers_off` |
| `--stop-timeout` | 🟡 | No negative values (wait forever). *Why:* Whoever waits can stop waiting (Ctrl+C, then `kill`). | `stop_sends_the_stop_signal_then_powers_off` |
| `--storage-opt` | 🟡 | Only `size=`: the writable disk, 1 GiB by default, at least 64 MiB. Sparse, so the host only spends what the sandbox writes. | `docker_options_reach_the_sandbox`, `parse::tests::storage_opt_sets_the_writable_disk_size` |
| `-t, --tty` | 🟢 |  | `run_t_gives_the_main_process_a_terminal` |
| `-u, --user` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-v, --volume` | 🟡 | Live-shares a host directory (`SRC:DST[:ro]`), which needs an executor with virtio-fs; without one it is an error, never a silent copy. *Why:* Executors differ in what they can do. sbx doesn't switch to another way of sharing behind your back. | `volume_is_shared_live`, `parse::tests::volumes` |
| `-v, --volume NAME:/path` | 🔴 | Today a name is an error. | V1 (0.3) |
| `--mount type=volume` | 🔴 |  | V1 (0.3) |
| `--mount type=tmpfs` | 🔴 |  | not planned yet |
| `-w, --workdir` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--add-host` | 🔴 |  | after 0.3 |
| `-a, --attach` | 🔴 | An attached sandbox gets stdin (with `-i`), stdout and stderr. | not planned yet |
| `--cidfile` | 🔴 |  | not planned yet |
| `--detach-keys` | 🔴 | `sbx attach --detach-keys` has it. | not planned yet |
| `--dns, --dns-option, --dns-search` | 🔴 |  | after 0.3 |
| `--domainname` | 🔴 |  | not planned yet |
| `--gpus` | 🔴 | When an executor offers a GPU. | after 0.3 |
| `--group-add` | 🔴 |  | not planned yet |
| `--health-cmd, --health-interval, --health-retries, --health-start-period, --health-start-interval, --health-timeout` | 🔴 |  | after 0.3 |
| `--no-healthcheck` | 🔴 |  | after 0.3 |
| `--label-file` | 🔴 |  | not planned yet |
| `--log-driver, --log-opt` | 🔴 | The shim writes each detached sandbox's log. | not planned yet |
| `-P, --publish-all` | 🔴 |  | not planned yet |
| `-q, --quiet` | 🔴 |  | not planned yet |
| `--read-only` | 🔴 |  | not planned yet |
| `--restart` | 🔴 | Without a daemon, through launchd or systemd. | after 0.3 |
| `--shm-size` | 🔴 |  | not planned yet |
| `--sig-proxy` | 🔴 | An attached `run` always forwards signals (`sbx attach --sig-proxy` has the flag). | not planned yet |
| `--sysctl` | 🔴 |  | not planned yet |
| `--tmpfs` | 🔴 |  | not planned yet |
| `--ulimit` | 🔴 |  | not planned yet |
| `--volumes-from` | 🔴 |  | not planned yet |
| `--platform` | ⚫ | `sbx pull --platform` downloads another architecture's image. *Why:* Emulating another architecture is out of scope: a sandbox runs the host's architecture. |  |
| `--link` | ⚫ | *Why:* Legacy in Docker. `--connect NAME:PORT` (V2) is sbx's way to reach another sandbox. |  |
| `--network-alias` | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect NAME:PORT` (V2) opens one port of another sandbox instead. |  |
| `--volume-driver` | ⚫ | *Why:* Extensions are out of scope. |  |
| `--privileged` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--cap-add, --cap-drop` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--security-opt` | ⚪ | seccomp, AppArmor and SELinux profiles. *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--userns` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--ipc, --pid, --uts` | ⚪ | *Why:* Namespace modes. A sandbox has its own kernel, so nothing is shared with the host or another sandbox. |  |
| `--cgroup-parent, --cgroupns` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `-c, --cpu-shares` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--cpu-period, --cpu-quota, --cpu-rt-period, --cpu-rt-runtime` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--cpuset-cpus, --cpuset-mems` | ⚪ | *Why:* Pinning host CPUs and memory nodes is up to the executor. |  |
| `--memory-reservation, --memory-swap, --memory-swappiness` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--oom-kill-disable, --oom-score-adj` | ⚪ | *Why:* The host's OOM killer sees the VM, not the processes in it; inside, the guest's own kernel decides. |  |
| `--pids-limit` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--blkio-weight, --blkio-weight-device` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--device-read-bps, --device-read-iops, --device-write-bps, --device-write-iops` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--device, --device-cgroup-rule` | ⚪ | *Why:* The VM sees only the devices the executor gives it. |  |
| `--ip, --ip6, --link-local-ip, --mac-address` | ⚪ | *Why:* The VM has no network interface. Connections leave through the host, by name. |  |
| `--expose` | ⚪ | *Why:* Nothing reaches a sandbox except a port published with `-p`, so declaring a port changes nothing. |  |
| `--init` | ⚪ | *Why:* Always on: sbx's guest init is PID 1 and reaps orphans. |  |
| `--annotation` | ⚪ | *Why:* Annotations go to an OCI runtime; sbx has none. |  |
| `--disable-content-trust` | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `--isolation` | ⚪ | *Why:* Only for Windows containers. |  |
| `--cpu-count, --cpu-percent, --io-maxbandwidth, --io-maxiops` | ⚪ | *Why:* Only for Windows containers. |  |
| `--rootfs-format` | ➕ | The root filesystem disk format; `erofs` by default. *Why:* Different executors and kernels read different formats. |  |
| `--mount type=copy` | ➕ | Copies a host directory in when the sandbox starts; take results out with `sbx cp`. *Why:* Works on every executor, including those without live sharing. |  |
| `--allow` | ➕ | With `--network allow`: a `HOST:PORT` the sandbox may connect to (`*.example.com:443`, `10.0.0.5:*`). |  |
| `--egress-limit` | ➕ | With `--network allow`: the most bytes all connections may move in each start. |  |
| `--timeout` | ➕ | Stops the sandbox this long after each start (like podman's `--timeout`). |  |

## docker exec

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-d, --detach` | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `-e, --env` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--env-file` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-i, --interactive` | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `-t, --tty` | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `-u, --user` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-w, --workdir` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--detach-keys` | 🔴 |  | not planned yet |
| `--privileged` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--timeout` | ➕ | Kills the command after a time and exits with 124, like `timeout(1)`. |  |

## docker start

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-a, --attach` | 🟢 |  | `create_start_inspect_rm` |
| `-i, --interactive` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--detach-keys` | 🔴 |  | not planned yet |
| `--checkpoint, --checkpoint-dir` | 🔴 | Snapshots, when an executor offers them. | after 0.3 |

## docker stop, docker restart

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-s, --signal` | 🟢 |  | `stop_sends_the_stop_signal_then_powers_off` |
| `-t, --timeout` | 🟡 | sbx: `-t, --time`. Docker's older name. No negative values. When it runs out, the guest is powered off (exit code 137) instead of SIGKILL. | `exec_cp_stop_on_a_running_sandbox`, `kill_wait_and_restart` |

## docker kill

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-s, --signal` | 🟢 |  | `kill_wait_and_restart` |

## docker rm

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-f, --force` | 🟡 | Powers the guest off without a signal. | `system_df_and_prune_free_what_nothing_uses`, `docker_options_reach_the_sandbox` |
| `-l, --link` | ⚫ | *Why:* Legacy in Docker, and sbx has no links. |  |
| `-v, --volumes` | ⚪ | *Why:* sbx makes no anonymous volumes: a sandbox's writable disk goes with it. |  |

## docker ps

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-a, --all` | 🟢 |  | `names_work_in_place_of_ids` |
| `-f, --filter` | 🟡 | Keys `id`, `name`, `label`, `status`, `exited`, `ancestor`. `name` is a substring, not a regular expression. No `before`, `since`, `health` and the others. An unknown key or value is an error (Docker ignores some). *Why:* What is accepted is a table in code (`filter.rs`), without a regular expression engine. | `filters`, `filter::tests::bad_filters_are_errors_that_say_what_is_wanted` |
| `--format` | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `json_output_contract` |
| `-q, --quiet` | 🟢 |  | `names_work_in_place_of_ids` |
| `-n, --last` | 🔴 |  | not planned yet |
| `-l, --latest` | 🔴 |  | not planned yet |
| `--no-trunc` | 🔴 |  | not planned yet |
| `-s, --size` | 🔴 |  | not planned yet |

## docker inspect

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-f, --format` | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `json_output_contract`, `jq_shapes_json_output` |
| `-s, --size` | 🔴 |  | not planned yet |
| `--type` | 🔴 | `inspect` takes sandboxes only. | not planned yet |

## docker stats

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `--format` | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `stats_reports_guest_usage` |
| `--no-stream` | 🟢 |  | `stats_reports_guest_usage` |
| `-a, --all` | 🔴 |  | not planned yet |
| `--no-trunc` | 🔴 |  | not planned yet |

## docker attach

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `--detach-keys` | 🟢 |  | `attach::tests::detach_keys_parse_like_docker` |
| `--no-stdin` | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--sig-proxy` | 🟢 |  | `attach_tty_and_signal_proxy` |

## docker logs

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-f, --follow` | 🟢 |  | `detached_rm_and_logs_follow` |
| `-n, --tail` | 🟢 |  | `detached_rm_and_logs_follow`, `behavior::tests::tail_counts_lines_across_records` |
| `-t, --timestamps` | 🟡 | One time per chunk of output the shim received, not per line. Refused for logs written by an `sbx` that kept no times. *Why:* The shim writes the chunks it receives as they are (`sbx_shim::log`). | `logs_timestamps_since_until_and_json` |
| `--since` | 🟡 | A date without a time zone is refused. Refused for logs without times. *Why:* Whether a line without a time is in the range can't be known, so sbx doesn't guess. | `logs_timestamps_since_until_and_json`, `parse::tests::times_take_rfc3339_unix_seconds_and_durations_ago` |
| `--until` | 🟡 | As `--since`. | `logs_timestamps_since_until_and_json` |
| `--details` | 🔴 |  | not planned yet |

## docker cp

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-a, --archive` | 🔴 |  | not planned yet |
| `-L, --follow-link` | 🔴 |  | not planned yet |
| `-q, --quiet` | 🔴 |  | not planned yet |

## docker events

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-f, --filter` | 🟡 | Keys `type`, `event`, `sandbox`, `image`. No `container` or `label`. | `filters`, `events_replay_and_follow` |
| `--format` | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `events_replay_and_follow` |
| `--since` | 🟡 | A date without a time zone is refused. | `events_replay_and_follow` |
| `--until` | 🟡 | A date without a time zone is refused. | `events_replay_and_follow` |

## docker images

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-f, --filter` | 🟡 | Keys `reference` (only `*` and `?` patterns), `dangling`, `label`. No `before` or `since`. | `filters`, `filter::tests::globs_match_the_whole_text` |
| `--format` | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `json_output_contract` |
| `-q, --quiet` | 🟢 |  | `names_work_in_place_of_ids` |
| `-a, --all` | 🔴 |  | not planned yet |
| `--digests` | 🔴 |  | not planned yet |
| `--no-trunc` | 🔴 |  | not planned yet |

## docker pull

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `--platform` | 🟡 | `linux/amd64` or `linux/arm64`; the host's by default. Only the host's architecture runs. | `parse::tests::platforms` |
| `-a, --all-tags` | 🔴 |  | not planned yet |
| `-q, --quiet` | 🔴 |  | not planned yet |
| `--disable-content-trust` | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |

## docker push

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-a, --all-tags` | 🟡 | Pushes the local tags of the repository one after another, in name order; one that fails doesn't stop the others. | `tag_and_push_to_a_registry`, `app::push::tests::push_names_a_local_tag_or_all_tags_of_a_repository` |
| `-q, --quiet` | 🟡 | Prints only the pushed reference, one per line. | `tag_and_push_to_a_registry` |
| `--platform` | 🔴 | sbx keeps one platform per image and always pushes that single-platform manifest. | not planned yet |
| `--disable-content-trust` | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |

## docker rmi

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-f, --force` | 🟡 | Also removes images used by stopped sandboxes in the same data directory, or with several references. Never those of running sandboxes. *Why:* sbx can't see sandboxes in another data directory. | `docker_options_reach_the_sandbox`, `behavior::tests::rmi_table` |
| `--no-prune` | ⚪ | *Why:* sbx images have no parent images to keep. |  |

## docker save

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-o, --output` | 🟢 |  | `load_and_save_round_trip_an_oci_archive` |
| `--platform` | 🔴 |  | not planned yet |

## docker load

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-i, --input` | 🟢 |  | `load_and_save_round_trip_an_oci_archive` |
| `--platform` | 🟡 | `linux/amd64` or `linux/arm64`; the host's by default. | `parse::tests::platforms` |
| `-q, --quiet` | 🔴 |  | not planned yet |
| `--tag` | ➕ | Names the image, when the archive holds one. |  |

## docker login

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-u, --username` | 🟢 |  | `login_and_logout_use_dockers_config` |
| `--password-stdin` | 🟢 |  | `login_and_logout_use_dockers_config` |
| `-p, --password` | ⚫ | *Why:* A password on the command line ends up in shell history and `ps`. Use `--password-stdin`, or let sbx ask. |  |

## docker system df

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `--format` | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `system_df_and_prune_free_what_nothing_uses` |
| `-v, --verbose` | 🔴 |  | not planned yet |

## docker system prune

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-a, --all` | 🟢 |  | `system_df_and_prune_free_what_nothing_uses` |
| `-f, --force` | 🟢 |  | `system_df_and_prune_free_what_nothing_uses` |
| `--filter` | 🟡 | Keys `until` and `label`, for sandboxes. | `system_df_and_prune_free_what_nothing_uses` |
| `--volumes` | 🔴 |  | V1 (0.3) |

## Behavior

How sbx behaves where no single flag is involved.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| Exit codes 125, 126 and 127 | 🟢 |  | `run_failures_before_the_command_are_125`, `run_command_not_found_is_127_and_not_executable_is_126`, `behavior::tests::exit_codes_match_docker` |
| Short image names (`alpine` is `docker.io/library/alpine:latest`) | 🟢 |  | `json_output_contract` |
| Registry credentials from Docker's `config.json` and credential helpers | 🟢 |  | `login_and_logout_use_dockers_config` |
| Image ID prefixes | 🟡 | At least 12 characters (Docker takes shorter ones). *Why:* A short string that happens to match could remove the wrong image. | `behavior::tests::ambiguous_id_prefix` |
| The main process and PID 1 | 🟡 | The main process is not PID 1 (sbx's guest init is), so it ends on SIGTERM even without a handler. *Why:* PID 1 in the VM is sbx's guest init. | `exec_cp_stop_on_a_running_sandbox` |
| `stop` or `rm -f` of a sandbox another terminal runs | 🟡 | Powers the guest off and waits; an error if it doesn't go off. *Why:* Only the process that started a VM can end it by force. | `exec_cp_stop_on_a_running_sandbox` |
| Go templates in `--format` (`--format '{{.ID}}'`) | ⚫ | *Why:* `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. |  |
| The Docker Engine API socket | ⚫ | *Why:* Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`. |  |
| Running another architecture's images (emulation) | ⚫ | *Why:* Out of scope: a sandbox runs the host's architecture. |  |

## docker build

`sbx build` comes in 0.3 (roadmap D3), with buildx's flag names. It builds inside a VM per stage, with the network closed unless `--allow` opens it.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `-t, --tag` | 🔴 |  | D3 (0.3) |
| `-f, --file` | 🔴 | `-f -` reads the Dockerfile from stdin. | D3 (0.3) |
| `--build-arg` | 🔴 |  | D3 (0.3) |
| `--target` | 🔴 |  | D3 (0.3) |
| `--no-cache` | 🔴 |  | D3 (0.3) |
| `--platform` | 🔴 | The host's architecture only. | D3 (0.3) |
| `--progress` | 🔴 |  | D3 (0.3) |
| `-o, --output` | 🔴 | `type=sbx` (the default) in D3; `type=docker` and `type=oci` in D4. | D3 (0.3) |
| `--load` | 🔴 |  | D4 (0.3) |
| `--push` | 🔴 |  | D5 (0.3) |
| `--secret` | 🔴 |  | after 0.3 |
| `--ssh` | 🔴 |  | after 0.3 |
| `--add-host` | 🔴 |  | after 0.3 |
| `--network` | 🔴 | A build's network is closed; `--allow HOST:PORT` opens it, as with `run` (D3). | not planned yet |
| `--annotation` | 🔴 |  | not planned yet |
| `--attest, --provenance, --sbom` | 🔴 |  | not planned yet |
| `--build-context` | 🔴 |  | not planned yet |
| `--cache-from, --cache-to` | 🔴 |  | not planned yet |
| `--call, --check` | 🔴 |  | not planned yet |
| `--iidfile` | 🔴 |  | not planned yet |
| `--label` | 🔴 |  | not planned yet |
| `--metadata-file` | 🔴 |  | not planned yet |
| `--no-cache-filter` | 🔴 |  | not planned yet |
| `--pull` | 🔴 |  | not planned yet |
| `-q, --quiet` | 🔴 |  | not planned yet |
| `--shm-size` | 🔴 |  | not planned yet |
| `--ulimit` | 🔴 |  | not planned yet |
| `--allow` | ⚫ | *Why:* buildx's entitlements (`network.host`, `security.insecure`) mean nothing in a build VM. sbx's `--allow` will name network destinations, as with `run`. |  |
| `--builder` | ⚪ | *Why:* There are no builder instances: each build stage gets its own VM. |  |
| `--cgroup-parent` | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |

## Dockerfile

Comes with `sbx build`. An instruction or flag it doesn't support is an error with the line number, never silently ignored.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `FROM` | 🔴 | With `AS name`, `scratch`, and `ARG` before `FROM`. The host's architecture only. | D3 (0.3) |
| `RUN` | 🔴 | Shell and exec forms, in the stage's VM; the network is closed unless `--allow` opens it. | D3 (0.3) |
| `RUN --mount=type=cache` | 🔴 | On top of named volumes. | after 0.3 |
| `RUN --mount=type=secret` | 🔴 |  | after 0.3 |
| `RUN --mount=type=ssh` | 🔴 |  | after 0.3 |
| `RUN --mount=type=bind`, `type=tmpfs` | 🔴 |  | not planned yet |
| `RUN --network` | 🔴 |  | not planned yet |
| `RUN --security=insecure` | ⚪ | *Why:* A build step already runs as root in its own VM; there is no container sandbox to loosen. |  |
| `COPY` | 🔴 | Files picked from the context with `.dockerignore`. | D3 (0.3) |
| `COPY --from` | 🔴 | From another stage, without a VM. | D3 (0.3) |
| `COPY --chown`, `--chmod` | 🔴 |  | D3 (0.3) |
| `COPY --link` | 🔴 |  | after 0.3 |
| `ADD` (local files, local tar archives) | 🔴 |  | D3 (0.3) |
| `ADD <URL>` | 🔴 | Through the same allow list, since the host downloads it. | after 0.3 |
| `ADD <git repository>` | 🔴 |  | after 0.3 |
| `ENV` | 🔴 |  | D3 (0.3) |
| `ARG` | 🔴 |  | D3 (0.3) |
| `WORKDIR` | 🔴 |  | D3 (0.3) |
| `USER` | 🔴 |  | D3 (0.3) |
| `LABEL` | 🔴 |  | D3 (0.3) |
| `EXPOSE` | 🔴 |  | D3 (0.3) |
| `VOLUME` | 🔴 |  | D3 (0.3) |
| `ENTRYPOINT` | 🔴 |  | D3 (0.3) |
| `CMD` | 🔴 |  | D3 (0.3) |
| `STOPSIGNAL` | 🔴 |  | D3 (0.3) |
| `SHELL` | 🔴 |  | D3 (0.3) |
| `HEALTHCHECK` | 🔴 | Written to the image config; sbx says it doesn't run health checks. | D3 (0.3) |
| `ONBUILD` | 🔴 |  | after 0.3 |
| `MAINTAINER` | 🔴 | Deprecated in Docker; use `LABEL`. | not planned yet |
| Here-documents (`RUN <<EOF`) | 🔴 |  | after 0.3 |
| Parser directive `# escape=` | 🔴 |  | D3 (0.3) |
| Parser directive `# syntax=` (another frontend) | ⚫ | *Why:* Another frontend needs BuildKit. sbx reports the line instead of ignoring it. |  |
| `.dockerignore` | 🔴 |  | D3 (0.3) |
| A git repository as the build context | 🔴 |  | after 0.3 |
| `SOURCE_DATE_EPOCH` (fixed times in layers) | 🔴 |  | D4 (0.3) |

## docker compose

`sbx compose` comes after 0.3 (C1). It reads part of `compose.yaml` and turns each service into `sbx` commands.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `up` | 🔴 |  | C1 (after 0.3) |
| `down` | 🔴 |  | C1 (after 0.3) |
| `ps` | 🔴 |  | C1 (after 0.3) |
| `logs` | 🔴 |  | C1 (after 0.3) |
| `build` | 🔴 |  | C1 (after 0.3) |
| `config` | 🔴 | Also shows which ports each service may reach. | C1 (after 0.3) |
| `create` | 🔴 |  | after 0.3 |
| `start` | 🔴 |  | after 0.3 |
| `stop` | 🔴 |  | after 0.3 |
| `restart` | 🔴 |  | after 0.3 |
| `rm` | 🔴 |  | after 0.3 |
| `kill` | 🔴 |  | after 0.3 |
| `exec` | 🔴 |  | after 0.3 |
| `run` | 🔴 |  | after 0.3 |
| `pull` | 🔴 |  | after 0.3 |
| `push` | 🔴 |  | after 0.3 |
| `images` | 🔴 |  | after 0.3 |
| `events` | 🔴 |  | after 0.3 |
| `top` | 🔴 |  | after 0.3 |
| `port` | 🔴 |  | after 0.3 |
| `pause` | 🔴 |  | after 0.3 |
| `unpause` | 🔴 |  | after 0.3 |
| `ls` | 🔴 |  | after 0.3 |
| `cp` | 🔴 |  | after 0.3 |
| `scale` | 🔴 |  | after 0.3 |
| `wait` | 🔴 |  | after 0.3 |
| `watch` | 🔴 |  | after 0.3 |
| `version` | 🔴 |  | after 0.3 |

## compose.yaml

Keys `sbx compose` will read. An unknown key is an error.

| Docker | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `services` | 🔴 |  | C1 (after 0.3) |
| `volumes` (top level) | 🔴 |  | C1 (after 0.3) |
| `networks` (top level) | ⚫ | Services reach each other through `--connect`, made from their ports. *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect NAME:PORT` (V2) opens one port of another sandbox instead. |  |
| `configs`, `secrets` (top level) | 🔴 |  | after 0.3 |
| `name` | 🔴 |  | after 0.3 |
| `include` | 🔴 |  | after 0.3 |
| `version` | 🔴 |  | after 0.3 |
| Extension fields (`x-…`) | 🔴 |  | after 0.3 |
| `services.*.image` | 🔴 |  | C1 (after 0.3) |
| `services.*.build` | 🔴 |  | C1 (after 0.3) |
| `services.*.command` | 🔴 |  | C1 (after 0.3) |
| `services.*.entrypoint` | 🔴 |  | C1 (after 0.3) |
| `services.*.environment` | 🔴 |  | C1 (after 0.3) |
| `services.*.env_file` | 🔴 |  | C1 (after 0.3) |
| `services.*.ports` | 🔴 |  | C1 (after 0.3) |
| `services.*.expose` | 🔴 | Used to open `--connect` between services. | C1 (after 0.3) |
| `services.*.volumes` | 🔴 | Named volumes and host paths. | C1 (after 0.3) |
| `services.*.depends_on` | 🔴 | Start order only. | C1 (after 0.3) |
| `services.*.container_name` | 🔴 |  | after 0.3 |
| `services.*.hostname` | 🔴 |  | after 0.3 |
| `services.*.working_dir` | 🔴 |  | after 0.3 |
| `services.*.user` | 🔴 |  | after 0.3 |
| `services.*.labels` | 🔴 |  | after 0.3 |
| `services.*.stdin_open`, `tty` | 🔴 |  | after 0.3 |
| `services.*.stop_signal`, `stop_grace_period` | 🔴 |  | after 0.3 |
| `services.*.cpus`, `mem_limit`, `deploy.resources` | 🔴 |  | after 0.3 |
| `services.*.restart` | 🔴 |  | after 0.3 |
| `services.*.healthcheck` | 🔴 |  | after 0.3 |
| `services.*.profiles` | 🔴 |  | after 0.3 |
| `services.*.extends` | 🔴 |  | after 0.3 |
| `services.*.secrets`, `configs` | 🔴 |  | after 0.3 |
| `services.*.tmpfs` | 🔴 |  | after 0.3 |
| `services.*.extra_hosts`, `dns` | 🔴 |  | after 0.3 |
| `services.*.develop.watch` | 🔴 |  | after 0.3 |
| `services.*.networks`, `network_mode` | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect NAME:PORT` (V2) opens one port of another sandbox instead. |  |
| `services.*.platform` | ⚫ | *Why:* Out of scope: a sandbox runs the host's architecture. |  |
| `services.*.logging` | 🔴 | The shim writes each detached sandbox's log. | not planned yet |
| `services.*.privileged`, `cap_add`, `cap_drop`, `security_opt` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `services.*.devices` | ⚪ | *Why:* The VM sees only the devices the executor gives it. |  |
| `services.*.ipc`, `pid`, `userns_mode` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `services.*.init` | ⚪ | *Why:* Always on: sbx's guest init is PID 1. |  |
