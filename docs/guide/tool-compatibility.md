# Compatibility with Docker, Podman and nerdctl

<!-- Generated from crates/app/cli/src/compat.rs. Do not edit: run `SBX_BLESS=1 cargo test -p sbx-cli --lib compat`. -->

`sbx` takes Docker's commands and flags, so what you know from `docker` carries over. This page compares sbx with what people install to run containers: the Docker CLI and Compose that Docker Desktop ships, Podman, nerdctl, and the features of Docker Desktop and of the products that stand in for it (Podman Desktop, OrbStack, Colima, Rancher Desktop). OrbStack, Colima and Rancher Desktop run Docker's or nerdctl's own command line, so they appear only under [product features](#product-features).

For every command of the three command lines, every flag of the commands sbx has, every Dockerfile instruction, the common Compose keys and the main product features, it says whether sbx does the same, what is different, and the test that shows it. This page is about other tools; what stays stable between sbx releases is in [COMPATIBILITY.md](../../COMPATIBILITY.md).

Each sandbox is its own microVM, there is no daemon, and the network is closed until you open it. So some features mean nothing here, a few are left out on purpose, and some work a little differently. Where sbx differs, it's deliberate, and the row says why.

The table is code, [`compat.rs`](../../crates/app/cli/src/compat.rs), and a test writes this page from it. The same tests check the table against sbx's command-line definition: a row can't say a flag works unless sbx accepts it and the test it names exists, a row can't say a flag is missing once sbx accepts it, whichever tool the row comes from, and every sbx command and flag needs a row. So the page can't drift from the code.

Checked on 2026-11-17 against the Docker CLI 29.9.3 and Compose 5.7.0 (as installed with Docker Desktop), Podman 6.2.0 (its [man pages](https://github.com/containers/podman/tree/v6.2.0/docs/source/markdown)), nerdctl 2.4.2 (its [command reference](https://github.com/containerd/nerdctl/blob/v2.4.2/docs/command-reference.md), which says that Docker flags it doesn't list are not implemented), and the products' own documentation as linked in each cell. The *In* column names the tools that have a command or flag under that name; a row that names several flags is marked for a tool that has any of them.

**Legend**

| Icon | Meaning |
| --- | --- |
| 🟢 | Same as in the tools that have it |
| 🟡 | Works, with the differences given |
| 🔴 | Not yet. The last column names the [roadmap](../roadmap.md) step that adds it in 0.3, *after 0.3*, or *not planned yet* |
| ⚫ | Will not do, for the reason given |
| ⚪ | Not applicable: means nothing when each sandbox is its own VM and there is no daemon |
| ➕ | sbx only: in none of the tools compared |

**Evidence.** A name such as `run_i_forwards_stdin` is an end-to-end test in [`tests/e2e.rs`](../../crates/app/cli/tests/e2e.rs): it runs the `sbx` binary with a fake executor, without a VM, on Linux and macOS in CI. A name with `::`, such as `parse::tests::memory_units`, is a unit test in [`crates/app/cli/src`](../../crates/app/cli/src). Real VMs are covered by the smoke tests ([testing](../developers/testing-and-releasing.md)).

## Progress

<!-- compat-progress:begin -->
Counted from the 636 rows below, for each thing sbx is compared with. "What applies to sbx" leaves out ⚫, ⚪ and ➕ rows. A row counts for every tool that has it.

| Compared with | Works (🟢 or 🟡), of what applies to sbx |
| --- | --- |
| **Docker CLI** | **`██████░░░░` 59% (146 of 244)** |
| **Podman** | **`████░░░░░░` 39% (131 of 332)** |
| **nerdctl** | **`█████░░░░░` 54% (128 of 237)** |
| **Dockerfile** | **`██████░░░░` 63% (21 of 33)** |
| **Compose** | **`░░░░░░░░░░` 0% (0 of 62)** |
| **Product features** | **`████████░░` 77% (7 of 9)** |

| Status | Rows |
| --- | --- |
| 🟢 Same | 75 |
| 🟡 Works, with differences | 102 |
| 🔴 Not yet | 329 (0 of them in 0.3) |
| ⚫ Will not do | 32 |
| ⚪ Not applicable in a VM | 77 |
| ➕ sbx only | 21 |

| Area | Works, of what applies to sbx |
| --- | --- |
| [Commands](#commands) | `███████░░░` 65% (40 of 61) |
| [Podman-only commands](#podman-only-commands) | `░░░░░░░░░░` 0% (0 of 23) |
| [nerdctl-only commands](#nerdctl-only-commands) | `░░░░░░░░░░` 0% (0 of 6) |
| [Global options](#global-options) | `█░░░░░░░░░` 12% (2 of 16) |
| [run, create](#run-create) | `███░░░░░░░` 34% (26 of 76) |
| [exec](#exec) | `██████░░░░` 63% (7 of 11) |
| [start](#start) | `███░░░░░░░` 33% (2 of 6) |
| [stop, restart](#stop-restart) | `███████░░░` 66% (2 of 3) |
| [kill](#kill) | `█████░░░░░` 50% (1 of 2) |
| [rm](#rm) | `█████░░░░░` 50% (1 of 2) |
| [ps](#ps) | `████░░░░░░` 44% (4 of 9) |
| [inspect](#inspect) | `██░░░░░░░░` 20% (1 of 5) |
| [stats](#stats) | `████░░░░░░` 40% (2 of 5) |
| [attach](#attach) | `████████░░` 75% (3 of 4) |
| [logs](#logs) | `███████░░░` 71% (5 of 7) |
| [cp](#cp) | `░░░░░░░░░░` 0% (0 of 4) |
| [events](#events) | `████████░░` 80% (4 of 5) |
| [images](#images) | `███░░░░░░░` 33% (3 of 9) |
| [pull](#pull) | `█░░░░░░░░░` 8% (1 of 12) |
| [push](#push) | `█░░░░░░░░░` 12% (2 of 16) |
| [rmi](#rmi) | `███░░░░░░░` 25% (1 of 4) |
| [save](#save) | `█░░░░░░░░░` 14% (1 of 7) |
| [load](#load) | `█████░░░░░` 50% (2 of 4) |
| [login](#login) | `█████░░░░░` 50% (2 of 4) |
| [volume create](#volume-create) | `█████░░░░░` 50% (2 of 4) |
| [volume ls](#volume-ls) | `██████░░░░` 60% (3 of 5) |
| [volume inspect](#volume-inspect) | `███░░░░░░░` 33% (1 of 3) |
| [volume rm](#volume-rm) | `█████░░░░░` 50% (1 of 2) |
| [volume prune](#volume-prune) | `████████░░` 75% (3 of 4) |
| [system df](#system-df) | `█████░░░░░` 50% (1 of 2) |
| [system prune](#system-prune) | `████████░░` 80% (4 of 5) |
| [builder prune](#builder-prune) | `████░░░░░░` 42% (3 of 7) |
| [Behavior](#behavior) | `█████████░` 85% (6 of 7) |
| [build](#build) | `██░░░░░░░░` 20% (13 of 62) |
| [Dockerfile](#dockerfile) | `██████░░░░` 63% (21 of 33) |
| [docker compose](#docker-compose) | `░░░░░░░░░░` 0% (0 of 28) |
| [compose.yaml](#composeyaml) | `░░░░░░░░░░` 0% (0 of 34) |
| [Product features](#product-features) | `████████░░` 77% (7 of 9) |
<!-- compat-progress:end -->

## Commands

Every `docker` command and management command; the *In* column says which of Podman and nerdctl have it under the same name. The flags of the commands sbx has are in the sections below.

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `run` | Docker, Podman, nerdctl | 🟡 | Each sandbox is a microVM. Pulls and prepares the image on first use. | `run_streams_output_and_returns_the_exit_code` |
| `create` | Docker, Podman, nerdctl | 🟡 | `create --rm` is refused: only `run` removes a sandbox when it exits. | `create_start_inspect_rm` |
| `start` | Docker, Podman, nerdctl | 🟢 | A stopped sandbox keeps its disk and starts again, as in Docker (unlike the OCI runtime spec). | `create_start_inspect_rm` |
| `stop` | Docker, Podman, nerdctl | 🟡 | Sends the stop signal, then powers the guest off (sync, exit code 137) instead of SIGKILL. *Why:* A sandbox is a VM: stopping it is shutting the VM down. | `exec_cp_stop_on_a_running_sandbox`, `stop_sends_the_stop_signal_then_powers_off` |
| `restart` | Docker, Podman, nerdctl | 🟡 | Stops as `stop` does, then starts detached. | `kill_wait_and_restart` |
| `kill` | Docker, Podman, nerdctl | 🟢 |  | `kill_wait_and_restart` |
| `wait` | Docker, Podman, nerdctl | 🟢 |  | `kill_wait_and_restart`, `json_output_contract` |
| `rm` | Docker, Podman, nerdctl | 🟡 | `rm -f` powers a running sandbox off without a signal. | `exec_cp_stop_on_a_running_sandbox`, `system_df_and_prune_free_what_nothing_uses` |
| `ps` | Docker, Podman, nerdctl | 🟡 | Sandboxes without `--name` have no name (Docker makes one up). | `names_work_in_place_of_ids` |
| `inspect` | Docker, Podman, nerdctl | 🟡 | Sandboxes only, one at a time, in sbx's own JSON shape ([cli-output.md](../developers/cli-output.md)). | `create_start_inspect_rm`, `json_output_contract` |
| `stats` | Docker, Podman, nerdctl | 🟡 | CPU, memory, writable disk and process count, as the guest reports them. No network or block I/O columns. | `stats_reports_guest_usage`, `app::stats::tests::cpu_percent_is_per_vcpu_like_docker` |
| `exec` | Docker, Podman, nerdctl | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `attach` | Docker, Podman, nerdctl | 🟡 | Only to a detached sandbox (`run -d`, `start`), not to one another `sbx` runs attached. Not on Windows yet. *Why:* The shim that owns a detached sandbox holds its main process's input and output; there is no daemon. | `attach_to_a_detached_sandbox` |
| `logs` | Docker, Podman, nerdctl | 🟡 | Only for detached sandboxes. Up to 256 MiB per sandbox; past that, output is dropped and one line says so (Docker's json-file has no limit). *Why:* No daemon collects the output: the shim writes it. An attached `run` shows it directly and keeps nothing. | `detached_run_outlives_sbx_and_keeps_logs`, `detached_rm_and_logs_follow` |
| `cp` | Docker, Podman, nerdctl | 🟡 | Only with a running sandbox. Copying out never writes outside the destination you name. *Why:* The guest reads and writes its disk, so it has to be running. | `exec_cp_stop_on_a_running_sandbox` |
| `pull` | Docker, Podman, nerdctl | 🟡 | Also follows the containers `registries.conf` (mirrors, blocked registries) and `policy.json` (cosign signatures). | `registries_conf_is_read_for_pulls`, `policy_json_is_followed` |
| `images` | Docker, Podman, nerdctl | 🟡 | Also lists which root filesystem disks are prepared, and SIZE / SHARED / UNIQUE on disk (layers several images use are shared, as in `docker system df -v`). | `names_work_in_place_of_ids`, `json_output_contract`, `images_on_one_base_store_it_once` |
| `rmi` | Docker, Podman, nerdctl | 🟡 | Never removes the image of a running sandbox, even with `-f`. | `json_output_contract`, `docker_options_reach_the_sandbox`, `behavior::tests::rmi_table` |
| `save` | Docker, Podman, nerdctl | 🟡 | One image per archive, written as an OCI image layout (with Docker's `manifest.json`): a pulled image exactly as it was pulled, a built one exported from its layers (the same bytes every time). | `load_and_save_round_trip_an_oci_archive`, `build_outputs_export_one_image_everywhere` |
| `load` | Docker, Podman, nerdctl | 🟡 | Reads OCI image layout archives or directories: `docker save` from Docker 25 on, `podman save --format oci-archive`, skopeo. | `load_and_save_round_trip_an_oci_archive` |
| `login` | Docker, Podman, nerdctl | 🟢 | Writes Docker's `config.json` or its credential helper, so `docker login` works too. | `login_and_logout_use_dockers_config` |
| `logout` | Docker, Podman, nerdctl | 🟢 |  | `login_and_logout_use_dockers_config` |
| `events` | Docker, Podman, nerdctl | 🟡 | Kinds `sandbox`, `image`, `volume` and `network`; actions create, start, stop, remove, `exec_start`, `exec_die`, pull, prepare, tag, untag, push, delete, and for `network` connect, close, refuse, lookup, `proxy_only`, notice. RFC 3339 times with nanoseconds. Keeps the last 1-2 MiB (Docker: the last 256 events). *Why:* No daemon: the process that changes a state records it, so a sandbox whose owner dies suddenly has no stop event. | `events_replay_and_follow`, `tag_and_push_to_a_registry`, `volume_data_outlives_the_sandbox` |
| `info` | Docker, Podman, nerdctl | 🟡 | Shows the host, the executor and the storage sbx uses. | `commands_that_do_not_launch_need_no_executor` |
| `version` | Docker, Podman, nerdctl | 🟡 | sbx: `--version`. There is no `version` command. | `docker_options_reach_the_sandbox` |
| `system` | Docker, Podman, nerdctl | 🟡 | `df` and `prune`. | `system_df_and_prune_free_what_nothing_uses` |
| `system df` | Docker, Podman, nerdctl | 🟡 | Images, sandboxes, volumes and executors, by what is actually on disk. A layer several images use is counted once; `SHARED` is that part. | `system_df_and_prune_free_what_nothing_uses`, `images_on_one_base_store_it_once` |
| `system prune` | Docker, Podman, nerdctl | 🟡 | Stopped sandboxes, unused layers (blobs and layer disks) and leftovers of interrupted pulls older than an hour; volumes only with `--volumes`. | `system_df_and_prune_free_what_nothing_uses` |
| `system events` | Docker, Podman | 🔴 | `sbx events` does this. | not planned yet |
| `system info` | Docker, Podman | 🔴 | `sbx info` does this. | not planned yet |
| `build` | Docker, Podman, nerdctl | 🟡 | Each stage's `RUN` steps run in one microVM with the network closed unless `--allow` opens it; `COPY`, `ADD` and `WORKDIR` layers are made on the host, so a `FROM scratch` stage of copies needs no VM. The image goes to the sbx image store unless `--output` says otherwise; `save` and `push` export a built image like any other. | `build_runs_stages_and_caches_steps`, `build_scratch_without_a_vm_and_shared_base_layers`, `build_outputs_export_one_image_everywhere` |
| `buildx build` | Docker, Podman | ⚫ | *Why:* `sbx build` is the builder and takes buildx's flag names; there is no builder plugin to call. |  |
| `builder` | Docker, nerdctl | 🟡 | `prune` only. The build cache also keeps itself under a cap after every build (`SBX_BUILD_CACHE_MAX`, default 10 GiB), dropping the least recently used steps whose layers no image uses. `sbx system df` shows it; `sbx system prune` clears it. | `build_cache_stays_under_its_cap` |
| `builder prune` | Docker, nerdctl | 🟡 | Removes cached steps whose layers no image uses (`-a`: every cached step; `--keep-storage`: keeps the most recently used up to a size). Without a terminal it needs `-f`. | `build_cache_stays_under_its_cap` |
| `push` | Docker, Podman, nerdctl | 🟡 | Sends the layers, config and manifest exactly as they were pulled, so the image keeps its digest; a built image is exported from its layers, with the same digest on every push. One blob at a time (Docker sends several at once). Only to the registry the name points at: never to a `registries.conf` mirror, and a blocked registry is refused. | `tag_and_push_to_a_registry`, `build_outputs_export_one_image_everywhere`, `app::push::tests::report_reads_like_docker_push` |
| `tag` | Docker, Podman, nerdctl | 🟢 | Moves a reference that pointed at another image, as Docker does, and records an image `tag` event. | `tag_and_push_to_a_registry` |
| `volume` | Docker, Podman, nerdctl | 🟡 | `create`, `ls`, `inspect`, `rm`, `prune`. A volume is a sparse disk that outlives sandboxes. One running sandbox at a time attaches a volume read-write; read-only attaches share it while nobody writes. A second writer is refused and told which sandbox holds it. *Why:* A volume is a block device with one file system on it. Two kernels writing it at once would corrupt it, so the host locks it for the VM's lifetime. | `volume_data_outlives_the_sandbox`, `a_volume_has_one_writer_at_a_time` |
| `volume create` | Docker, Podman, nerdctl | 🟡 | 16 GiB by default (sparse: the host spends only what is written). Creating an existing name again returns it, as Docker does; with another size or labels it is an error. | `volume_data_outlives_the_sandbox`, `app::volume::tests::only_the_size_option_is_known` |
| `volume ls` | Docker, Podman, nerdctl | 🟡 | Also shows the size, what the disk uses on the host and the sandboxes that point at each volume. | `volume_data_outlives_the_sandbox` |
| `volume inspect` | Docker, Podman, nerdctl | 🟡 | Size, labels, the disk file, the sandboxes that point at it and whether one has it attached now. No `Mountpoint`: the data lives inside the disk. | `volume_data_outlives_the_sandbox`, `mcp_uses_only_its_own_volumes` |
| `volume rm` | Docker, Podman, nerdctl | 🟢 | Refused while a sandbox points at the volume, even a stopped one. | `volume_data_outlives_the_sandbox`, `a_volume_has_one_writer_at_a_time` |
| `volume prune` | Docker, Podman, nerdctl | 🟡 | Removes nothing without `-a`, because sbx has no anonymous volumes; `-a` removes every volume no sandbox points at (Docker 23 and later: the same). | `volume_data_outlives_the_sandbox` |
| `volume update` | Docker | 🔴 | Docker's only takes cluster volumes. | not planned yet |
| `compose` | Docker, Podman, nerdctl | 🔴 |  | C1 (after 0.3) |
| `commit` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `diff` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `history` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `rename` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `top` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `port` | Docker, Podman, nerdctl | 🔴 | `sbx inspect` shows the published ports. | after 0.3 |
| `update` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `pause` | Docker, Podman, nerdctl | 🔴 | When an executor can pause a VM. | after 0.3 |
| `unpause` | Docker, Podman, nerdctl | 🔴 | When an executor can pause a VM. | after 0.3 |
| `checkpoint` | Docker, nerdctl | 🔴 | Snapshots, when an executor offers them. | after 0.3 |
| `search` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `export` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `import` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `manifest` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `container` | Docker, Podman, nerdctl | 🔴 | The same commands exist at the top level (`sbx ps`, `sbx rm`). | not planned yet |
| `image` | Docker, Podman, nerdctl | 🔴 | The same commands exist at the top level (`sbx images`, `sbx rmi`). | not planned yet |
| `network` | Docker, Podman, nerdctl | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect SANDBOX:PORT` opens one port of another sandbox instead. |  |
| `context` | Docker | ⚪ | `--executor` and `sbx executor use` pick what runs the VMs. *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `plugin` | Docker | ⚫ | *Why:* Extensions are out of scope. |  |
| `trust` | Docker | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `swarm` | Docker | ⚫ | *Why:* Swarm is out of scope. |  |
| `node` | Docker | ⚫ | *Why:* Swarm is out of scope. |  |
| `service` | Docker | ⚫ | *Why:* Swarm is out of scope. |  |
| `stack` | Docker | ⚫ | *Why:* Swarm is out of scope. |  |
| `secret` | Docker | ⚫ | *Why:* Swarm is out of scope. |  |
| `config` | Docker | ⚫ | *Why:* Swarm is out of scope. |  |
| `prepare` |  | ➕ | Builds an image's layer disks ahead of time, one per layer and shared between images, for the executor `run` would use (`run` does it on first use). |  |
| `tui` |  | ➕ | Sandboxes, images, volumes, events and executors on one screen, like Docker Desktop or lazydocker. Every key runs an `sbx` command and shows its command line. |  |
| `mcp` |  | ➕ | Serves sandboxes to AI agents over the Model Context Protocol ([guide](mcp.md)). Docker Desktop's `docker mcp` plugin is a different tool. |  |
| `executor` |  | ➕ | Installs and picks executors, the programs that run the VMs ([guide](executors.md)). |  |
| `upgrade` |  | ➕ | Replaces sbx with the newest signed release. |  |
| `completion` | Docker, Podman, nerdctl | 🟡 | `sbx completion SHELL` prints a script that calls sbx back: on each TAB the shell runs `COMPLETE=<shell> sbx -- <words>`, which completes sandbox names and IDs, image references and executors, and changes nothing. So with `COMPLETE` set (not empty, not 0), sbx only completes. | `completion_offers_sandboxes_and_images` |
| `bake` | Docker | 🔴 | `docker buildx bake`: builds from a file of targets. | not planned yet |

## Podman-only commands

Podman commands that Docker doesn't have. Podman's versions of Docker's commands are in the section above.

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `pod` | Podman | 🔴 | Containers that share namespaces. `--connect` links sandboxes one port at a time. | not planned yet |
| `machine` | Podman | ⚪ | Manages the Linux VM Podman runs containers in on macOS and Windows. *Why:* Each sandbox is its own VM, so there is no shared Linux VM to set up or connect to. `sbx executor` installs what runs the VMs. |  |
| `kube` | Podman | 🔴 | `play`, `generate`, `down`, `apply`: Kubernetes YAML to containers and pods, and back. | not planned yet |
| `generate` | Podman | 🔴 | `systemd`, `spec`. | not planned yet |
| `quadlet` | Podman | 🔴 | Installs and lists Quadlet files, which run containers as systemd services. | not planned yet |
| `auto-update` | Podman | 🔴 |  | not planned yet |
| `healthcheck` | Podman | 🔴 | `healthcheck run` runs a container's health check now. | after 0.3 |
| `secret` | Podman | 🔴 | Podman's secrets are local (for `run --secret`); Docker's `secret` is a Swarm command. | not planned yet |
| `artifact` | Podman | 🔴 | OCI artifacts in local storage. | not planned yet |
| `farm` | Podman | 🔴 | Builds on several machines at once. | not planned yet |
| `init` | Podman | 🔴 |  | not planned yet |
| `untag` | Podman | 🔴 |  | not planned yet |
| `mount` | Podman | 🔴 |  | not planned yet |
| `unmount` | Podman | 🔴 |  | not planned yet |
| `unshare` | Podman | ⚪ | Runs a command in Podman's rootless user namespace on the host. *Why:* A user namespace maps users of a kernel shared with the host. A sandbox has its own kernel and its own users. |  |
| `system connection` | Podman | ⚪ | Connections to Podman services, such as the one in `podman machine`. *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `system service` | Podman | ⚪ | Serves Podman's API and a Docker-compatible API on a socket. *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `system reset` | Podman | 🔴 |  | not planned yet |
| `system check` | Podman | 🔴 |  | not planned yet |
| `system migrate` | Podman | 🔴 |  | not planned yet |
| `system renumber` | Podman | 🔴 |  | not planned yet |
| `volume export` | Podman | 🔴 |  | not planned yet |
| `volume import` | Podman | 🔴 |  | not planned yet |
| `volume exists` | Podman | 🔴 |  | not planned yet |
| `volume mount` | Podman | 🔴 |  | not planned yet |
| `volume unmount` | Podman | 🔴 |  | not planned yet |
| `volume rename` | Podman | 🔴 |  | not planned yet |
| `volume reload` | Podman | ⚫ | Reloads volumes from volume plugins. *Why:* Extensions are out of scope. |  |

## nerdctl-only commands

nerdctl commands that Docker doesn't have (marked :nerd_face: in its command reference).

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `namespace` | nerdctl | 🔴 | containerd namespaces: separate sets of containers and images. sbx's `--data-dir` and `--cache-dir` keep separate sets. | not planned yet |
| `image convert` | nerdctl | 🔴 | To eStargz, zstd:chunked, SOCI, overlaybd or EROFS layers. sbx makes its own EROFS layer disks (`prepare`). | not planned yet |
| `image encrypt` | nerdctl | 🔴 |  | not planned yet |
| `image decrypt` | nerdctl | 🔴 |  | not planned yet |
| `apparmor` | nerdctl | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `builder debug` | nerdctl | 🔴 |  | not planned yet |
| `ipfs` | nerdctl | 🔴 | `ipfs registry serve`: a registry backed by IPFS. | not planned yet |

## Global options

Options that go before the command (`docker --debug ps`).

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-v, --version` | Docker, Podman | 🟡 | sbx: `-V, --version`. `-v` is not an option. | `docker_options_reach_the_sandbox` |
| `--config` | Docker, Podman | 🔴 | sbx reads `DOCKER_CONFIG` for registry credentials. | not planned yet |
| `-D, --debug` | Docker, nerdctl | 🔴 |  | not planned yet |
| `-l, --log-level` | Docker, Podman | 🔴 |  | not planned yet |
| `-c, --context` | Docker | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `-H, --host` | Docker, nerdctl | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--tls, --tlscacert, --tlscert, --tlskey, --tlsverify` | Docker | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--runtime` | Podman | 🟡 | sbx: `--executor`. Picks the program that runs the VMs, instead of an OCI runtime. | `installed_executor_is_used_by_default`, `vmm_packages_pins_and_version_files` |
| `--runtime-flag, --conmon, --hooks-dir` | Podman | ⚪ | *Why:* They configure an OCI runtime, its monitor and its hooks; sbx has no OCI runtime. |  |
| `--connection, --url, --identity, --ssh, --remote` | Podman | ⚪ | How Podman's client reaches a Podman service, such as the one in `podman machine`. *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--tls-ca, --tls-cert, --tls-key, --tls-details` | Podman | ⚪ | *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--address` | nerdctl | ⚪ | The containerd socket. *Why:* There is no daemon: each `sbx` command reads and writes the data and cache directories itself. |  |
| `--root, --runroot, --imagestore, --volumepath, --tmpdir` | Podman | 🔴 | sbx's `--data-dir` and `--cache-dir` say where its state and images live. | not planned yet |
| `--data-root` | nerdctl | 🔴 | sbx's `--data-dir` and `--cache-dir` say where its state and images live. | not planned yet |
| `--namespace` | nerdctl | 🔴 | containerd namespaces; see `nerdctl namespace` above. | not planned yet |
| `--storage-driver, --storage-opt, --snapshotter` | Podman, nerdctl | ⚪ | *Why:* Storage drivers and snapshotters stack image layers for containers on the host kernel. sbx gives each VM the layers as read-only disks (`--rootfs-format`). |  |
| `--transient-store` | Podman | 🔴 |  | not planned yet |
| `--cgroup-manager` | Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--cdi-spec-dir` | Podman | ⚪ | *Why:* The VM sees only the devices the executor gives it. |  |
| `--network-config-dir, --cni-path, --cni-netconfpath` | Podman, nerdctl | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect SANDBOX:PORT` opens one port of another sandbox instead. |  |
| `--events-backend` | Podman | 🔴 |  | not planned yet |
| `--module` | Podman | 🔴 |  | not planned yet |
| `--out` | Podman | 🔴 |  | not planned yet |
| `--syslog` | Podman | 🔴 |  | not planned yet |
| `--log-file` | nerdctl | 🔴 |  | not planned yet |
| `--insecure-registry` | nerdctl | 🔴 |  | not planned yet |
| `--host-gateway-ip` | nerdctl | 🔴 | With `--add-host`. | after 0.3 |
| `--selinux-enabled` | nerdctl | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--userns-remap` | nerdctl | ⚪ | *Why:* A user namespace maps users of a kernel shared with the host. A sandbox has its own kernel and its own users. |  |
| `--format` |  | ➕ | `text` or `json` for every command; the JSON shapes are a public contract ([cli-output.md](../developers/cli-output.md)). *Why:* A Go template engine can't be reproduced exactly in Rust, and a near copy would surprise. jq is the common language for JSON. |  |
| `--jq` |  | ➕ | With `--format json`: a built-in jq expression picks fields (like `gh`). |  |
| `--data-dir` |  | ➕ | Where sandbox state lives. |  |
| `--cache-dir` |  | ➕ | Where images and their disks are cached. |  |
| `--socket-dir` |  | ➕ | Where guest connection sockets go. |  |
| `--guest-init` |  | ➕ | Another guest init initramfs than the one built into sbx. |  |
| `--executor` |  | ➕ | The executor that runs the VM. |  |
| `--shim` |  | ➕ | The program that owns detached sandboxes. |  |

## run, create

`create` takes the same flags as `run`, except `-d`. Many of these flags tune namespaces and cgroups on a shared kernel; in sbx each sandbox has its own kernel, and the VM's size is the limit.

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-d, --detach` | Docker, Podman, nerdctl | 🟡 | A small `sbx-shim` process owns each detached sandbox. *Why:* There is no daemon; a VM is owned by the process that started it. | `detached_run_outlives_sbx_and_keeps_logs` |
| `--entrypoint` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-e, --env` | Docker, Podman, nerdctl | 🟢 |  | `names_work_in_place_of_ids`, `parse::tests::env_values` |
| `--env-file` | Docker, Podman, nerdctl | 🟢 |  | `names_work_in_place_of_ids`, `parse::tests::env_files_follow_docker` |
| `-h, --hostname` | Docker, Podman, nerdctl | 🟡 | sbx: `--hostname`. `-h` is help. | `docker_options_reach_the_sandbox` |
| `-i, --interactive` | Docker, Podman, nerdctl | 🟢 |  | `run_i_forwards_stdin` |
| `-l, --label` | Docker, Podman, nerdctl | 🟢 |  | `filters`, `docker_options_reach_the_sandbox` |
| `-m, --memory` | Docker, Podman, nerdctl | 🟡 | 512 MiB by default (Docker: no limit). *Why:* A VM is started with a fixed size. | `docker_options_reach_the_sandbox`, `parse::tests::memory_units` |
| `--cpus` | Docker, Podman, nerdctl | 🟡 | Whole vCPUs only; 1 by default (Docker: no limit). *Why:* A VM is started with a fixed number of vCPUs. | `docker_options_reach_the_sandbox`, `parse::tests::cpus_must_be_whole` |
| `--mount` | Docker, Podman, nerdctl | 🟡 | `type=bind` (needs an executor with live sharing; an error otherwise), `type=volume` and `type=copy` (below). No `tmpfs` yet. | `docker_options_reach_the_sandbox`, `parse::tests::mounts` |
| `--name` | Docker, Podman, nerdctl | 🟡 | Without it a sandbox has no name and is found by its ID (Docker makes a name up). *Why:* A made-up name shows up for people who don't use names and only adds clashes. A caller that wants one picks it. | `names_work_in_place_of_ids` |
| `--network` | Docker, Podman, nerdctl | 🟡 | `none` (the default; Docker's is `bridge`) or `allow`: TCP to the `--allow` destinations only, checked by the host by name. Programs that use `HTTP_PROXY` go through a proxy in the guest; others reach `HOST:PORT` rules through the guest's DNS, which answers from the allow list with stand-in addresses the guest listens on. `HOST:*` rules, IPv6 and UDP (beyond DNS) go through the proxy only or not at all. *Why:* The network is closed by default and opened by name. The allow list is enforced on every OS without help from the executor, and the VM still has no network interface. | `network_allow_list`, `transparent_tcp_reaches_allowed_names`, `behavior::tests::network_policy_table` |
| `--network bridge, host, container:NAME, NAME` | Docker, Podman, nerdctl | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect SANDBOX:PORT` opens one port of another sandbox instead. |  |
| `-p, --publish` | Docker, Podman, nerdctl | 🟡 | Without an IP it listens on 127.0.0.1 (Docker: every address). TCP only. The process that owns the VM listens, and connects to 127.0.0.1:PORT in the guest. *Why:* Closed by default: write `0.0.0.0:` to open a port beyond this machine. The VM has no network interface, so the port is reached on the guest's loopback. | `published_ports_reach_the_sandbox` |
| `-p, --publish …/udp` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--pull` | Docker, Podman, nerdctl | 🟢 |  | `run_failures_before_the_command_are_125`, `behavior::tests::pull_policy_default_matches_docker` |
| `--rm` | Docker, Podman, nerdctl | 🟡 | `run` only: `create --rm` is refused. | `run_streams_output_and_returns_the_exit_code`, `create_start_inspect_rm` |
| `--runtime` | Docker, nerdctl | 🟡 | sbx: `--executor`. A global option that picks the program that runs the VM. | `installed_executor_is_used_by_default`, `vmm_packages_pins_and_version_files` |
| `--stop-signal` | Docker, Podman, nerdctl | 🟢 |  | `stop_sends_the_stop_signal_then_powers_off` |
| `--stop-timeout` | Docker, Podman, nerdctl | 🟡 | No negative values (wait forever). *Why:* Whoever waits can stop waiting (Ctrl+C, then `kill`). | `stop_sends_the_stop_signal_then_powers_off` |
| `--storage-opt` | Docker | 🟡 | Only `size=`: the writable disk, 1 GiB by default, at least 64 MiB. Sparse, so the host only spends what the sandbox writes. | `docker_options_reach_the_sandbox`, `parse::tests::storage_opt_sets_the_writable_disk_size` |
| `-t, --tty` | Docker, Podman, nerdctl | 🟢 |  | `run_t_gives_the_main_process_a_terminal` |
| `-u, --user` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-v, --volume` | Docker, Podman, nerdctl | 🟡 | With a host path, live-shares a host directory (`SRC:DST[:ro]`), which needs an executor with virtio-fs; without one it is an error, never a silent copy. With a name, attaches a volume (below). *Why:* Executors differ in what they can do. sbx doesn't switch to another way of sharing behind your back. | `volume_is_shared_live`, `parse::tests::volumes` |
| `-v, --volume NAME:/path` | Docker, Podman, nerdctl | 🟡 | Attaches a named volume; a name used for the first time creates it. The first attach fills an empty volume with what the image has at that path (owner, mode, times), unless `nocopy`. `ro` is read-only on the host too. A volume attached read-write by a running sandbox is refused, naming that sandbox. *Why:* Each volume is a disk of the VM, attached after the root file system and the writable disk, so an executor's disk count limits how many a sandbox takes. | `volume_data_outlives_the_sandbox`, `a_volume_has_one_writer_at_a_time`, `volume_disks_limits_and_old_executors`, `parse::tests::volume_names_are_told_from_paths_like_docker` |
| `-v, --volume /path` | Docker, Podman, nerdctl | ⚫ | *Why:* An anonymous volume is a disk nobody named that lingers after the sandbox. Name it (`-v NAME:/path`); `--storage-opt size=` sizes the writable disk. |  |
| `--mount type=volume` | Docker, Podman, nerdctl | 🟡 | `source`, `target`, `readonly`, `volume-nocopy`. No `volume-driver`, `volume-opt` or `volume-subpath`. | `volume_data_outlives_the_sandbox`, `parse::tests::volume_names_are_told_from_paths_like_docker` |
| `--mount type=tmpfs` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `-w, --workdir` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--add-host` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `-a, --attach` | Docker, Podman, nerdctl | 🔴 | An attached sandbox gets stdin (with `-i`), stdout and stderr. | not planned yet |
| `--cidfile` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--detach-keys` | Docker, Podman, nerdctl | 🔴 | `sbx attach --detach-keys` has it. | not planned yet |
| `--dns, --dns-option, --dns-search` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--domainname` | Docker, nerdctl | 🔴 |  | not planned yet |
| `--gpus` | Docker, Podman, nerdctl | 🔴 | When an executor offers a GPU. | after 0.3 |
| `--group-add` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--health-cmd, --health-interval, --health-retries, --health-start-period, --health-start-interval, --health-timeout` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--no-healthcheck` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--label-file` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--log-driver, --log-opt` | Docker, Podman, nerdctl | 🔴 | The shim writes each detached sandbox's log. | not planned yet |
| `-P, --publish-all` | Docker, Podman | 🔴 |  | not planned yet |
| `-q, --quiet` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--read-only` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--restart` | Docker, Podman, nerdctl | 🔴 | Without a daemon, through launchd or systemd. | after 0.3 |
| `--shm-size` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--sig-proxy` | Docker, Podman, nerdctl | 🔴 | An attached `run` always forwards signals (`sbx attach --sig-proxy` has the flag). | not planned yet |
| `--sysctl` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--tmpfs` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--ulimit` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--volumes-from` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--platform` | Docker, Podman, nerdctl | ⚫ | `sbx pull --platform` downloads another architecture's image. *Why:* Emulating another architecture is out of scope: a sandbox runs the host's architecture. |  |
| `--link` | Docker | ⚫ | `--link NAME:ALIAS` is `--connect ALIAS=NAME:PORT`, one port at a time. *Why:* Legacy in Docker. `--connect SANDBOX:PORT` is sbx's way to reach another sandbox: one port, by name, looked up at each connection. |  |
| `--network-alias` | Docker, Podman | ⚫ | The connecting side picks the name: `--connect ALIAS=SANDBOX:PORT`. *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect SANDBOX:PORT` opens one port of another sandbox instead. |  |
| `--volume-driver` | Docker | ⚫ | *Why:* Extensions are out of scope. |  |
| `--privileged` | Docker, Podman, nerdctl | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--cap-add, --cap-drop` | Docker, Podman, nerdctl | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--security-opt` | Docker, Podman, nerdctl | ⚪ | seccomp, AppArmor and SELinux profiles. *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--userns` | Docker, Podman, nerdctl | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--ipc, --pid, --uts` | Docker, Podman, nerdctl | ⚪ | *Why:* Namespace modes. A sandbox has its own kernel, so nothing is shared with the host or another sandbox. |  |
| `--cgroup-parent, --cgroupns` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `-c, --cpu-shares` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--cpu-period, --cpu-quota, --cpu-rt-period, --cpu-rt-runtime` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--cpuset-cpus, --cpuset-mems` | Docker, Podman, nerdctl | ⚪ | *Why:* Pinning host CPUs and memory nodes is up to the executor. |  |
| `--memory-reservation, --memory-swap, --memory-swappiness` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--oom-kill-disable, --oom-score-adj` | Docker, Podman, nerdctl | ⚪ | *Why:* The host's OOM killer sees the VM, not the processes in it; inside, the guest's own kernel decides. |  |
| `--pids-limit` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--blkio-weight, --blkio-weight-device` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--device-read-bps, --device-read-iops, --device-write-bps, --device-write-iops` | Docker, Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--device, --device-cgroup-rule` | Docker, Podman, nerdctl | ⚪ | *Why:* The VM sees only the devices the executor gives it. |  |
| `--ip, --ip6, --link-local-ip, --mac-address` | Docker, Podman, nerdctl | ⚪ | *Why:* The VM has no network interface. Connections leave through the host, by name. |  |
| `--expose` | Docker, Podman | ⚪ | *Why:* Nothing reaches a sandbox except a port published with `-p` or named by another sandbox's `--connect`, so declaring a port changes nothing. |  |
| `--init` | Docker, Podman, nerdctl | ⚪ | *Why:* Always on: sbx's guest init is PID 1 and reaps orphans. |  |
| `--annotation` | Docker, Podman, nerdctl | ⚪ | *Why:* Annotations go to an OCI runtime; sbx has none. |  |
| `--disable-content-trust` | Docker, Podman | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `--isolation` | Docker, nerdctl | ⚪ | *Why:* Only for Windows containers. |  |
| `--cpu-count, --cpu-percent, --io-maxbandwidth, --io-maxiops` | Docker | ⚪ | *Why:* Only for Windows containers. |  |
| `--umask` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--use-api-socket` | Docker | ⚫ | Mounts the Docker Engine API socket into the container. *Why:* Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`. |  |
| `--timeout` | Podman | 🟡 | Stops the sandbox this long after each start: the stop signal, then a power-off after `--stop-timeout` (Podman kills the container when the time is up). | `a_detached_sandbox_stops_after_its_timeout` |
| `--pod, --pod-id-file` | Podman | 🔴 | `--connect` links sandboxes one port at a time. | not planned yet |
| `--userns keep-id, auto, nomap` | Podman | ⚪ | Podman's values that map your user into the container. *Why:* A user namespace maps users of a kernel shared with the host. A sandbox has its own kernel and its own users. |  |
| `--uidmap, --gidmap, --subuidname, --subgidname` | Podman | ⚪ | *Why:* A user namespace maps users of a kernel shared with the host. A sandbox has its own kernel and its own users. |  |
| `--arch, --os, --variant` | Podman | ⚫ | `sbx pull --platform` downloads another architecture's image. *Why:* Out of scope: a sandbox runs the host's architecture. |  |
| `--authfile, --cert-dir, --creds, --tls-verify` | Podman | 🔴 | Registry access for the pull. sbx uses Docker's `config.json` and the containers `registries.conf`. | not planned yet |
| `--retry, --retry-delay` | Podman | 🔴 |  | not planned yet |
| `--decryption-key` | Podman | 🔴 |  | not planned yet |
| `--verify, --cosign-key, --cosign-certificate-identity, --cosign-certificate-identity-regexp, --cosign-certificate-oidc-issuer, --cosign-certificate-oidc-issuer-regexp` | nerdctl | 🔴 | sbx follows the containers `policy.json`, which can require cosign signatures, on every pull. | not planned yet |
| `--ipfs-address` | nerdctl | 🔴 |  | not planned yet |
| `--replace` | Podman | 🔴 |  | not planned yet |
| `--requires` | Podman | 🔴 |  | not planned yet |
| `--rmi` | Podman | 🔴 |  | not planned yet |
| `--rootfs` | Podman, nerdctl | 🔴 | Runs from a directory instead of an image. | not planned yet |
| `--secret` | Podman | 🔴 |  | not planned yet |
| `--env-host, --env-merge, --unsetenv, --unsetenv-all` | Podman | 🔴 |  | not planned yet |
| `--http-proxy` | Podman | 🔴 |  | not planned yet |
| `--tz` | Podman | 🔴 |  | not planned yet |
| `--passwd, --passwd-entry, --group-entry, --hostuser` | Podman | 🔴 |  | not planned yet |
| `--hosts-file, --no-hosts, --no-hostname, --chrootdirs` | Podman | 🔴 |  | not planned yet |
| `--image-volume` | Podman | 🔴 |  | not planned yet |
| `--read-only-tmpfs` | Podman | 🔴 |  | not planned yet |
| `--shm-size-systemd` | Podman | 🔴 |  | not planned yet |
| `--pidfile` | Podman, nerdctl | 🔴 |  | not planned yet |
| `--systemd` | Podman, nerdctl | 🔴 |  | not planned yet |
| `--sdnotify` | Podman | 🔴 |  | not planned yet |
| `--personality` | Podman | 🔴 |  | not planned yet |
| `--health-on-failure, --health-log-destination, --health-max-log-count, --health-max-log-size` | Podman | 🔴 |  | after 0.3 |
| `--health-startup-cmd, --health-startup-interval, --health-startup-retries, --health-startup-success, --health-startup-timeout` | Podman | 🔴 |  | after 0.3 |
| `--init-path` | Podman | ⚪ | *Why:* Always on: sbx's guest init is PID 1 and reaps orphans. |  |
| `--init-binary` | nerdctl | ⚪ | *Why:* Always on: sbx's guest init is PID 1 and reaps orphans. |  |
| `--conmon-pidfile` | Podman | ⚪ | *Why:* conmon is Podman's container monitor; sbx's `sbx-shim` owns a detached sandbox. |  |
| `--cgroup-conf, --cgroups` | Podman, nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--kernel-memory` | nerdctl | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--rdt-class` | Podman, nerdctl | ⚪ | *Why:* Intel RDT cache allocation for a process on the host. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--seccomp-policy` | Podman | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--preserve-fd, --preserve-fds` | Podman | ⚪ | *Why:* A file descriptor of the host can't be handed to a process in another kernel. |  |
| `--rootfs-format` |  | ➕ | The root filesystem disk format; `erofs` by default. *Why:* Different executors and kernels read different formats. |  |
| `--mount type=copy` |  | ➕ | Copies a host directory in when the sandbox starts; take results out with `sbx cp`. *Why:* Works on every executor, including those without live sharing. |  |
| `--allow` |  | ➕ | With `--network allow`: a `HOST:PORT` the sandbox may connect to (`*.example.com:443`, `10.0.0.5:*`). A `*` port, an IPv6 address or `localhost` works only for programs that use `HTTP_PROXY`; `run` warns about each. |  |
| `--egress-limit` |  | ➕ | With `--network allow` or `--connect`: the most bytes all connections may move in each start, both ways (`--connect` traffic counts for the sandbox that connects). |  |
| `--connect` |  | ➕ | `[NAME=]SANDBOX:PORT`: programs inside reach TCP `PORT` of another running sandbox (same data directory) as `NAME:PORT`, with any protocol, and nothing else opens. The other sandbox needs no option. The target is looked up at each connection, so a restarted one works on the next; a stopped one fails that connection and is recorded in `events`. An MCP sandbox may only connect to MCP sandboxes. *Why:* Docker networks open every port of every container on the network to each other. `--connect` opens one port, by name, like `--allow`, and the VM still has no network interface. |  |

## exec

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-d, --detach` | Docker, Podman, nerdctl | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `-e, --env` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--env-file` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-i, --interactive` | Docker, Podman, nerdctl | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `-t, --tty` | Docker, Podman, nerdctl | 🟢 |  | `exec_cp_stop_on_a_running_sandbox` |
| `-u, --user` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `-w, --workdir` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--detach-keys` | Docker, Podman | 🔴 |  | not planned yet |
| `--privileged` | Docker, Podman, nerdctl | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `--latest` | Podman | 🔴 |  | not planned yet |
| `--cidfile` | Podman | 🔴 |  | not planned yet |
| `--no-session` | Podman | 🔴 |  | not planned yet |
| `--preserve-fd, --preserve-fds` | Podman | ⚪ | *Why:* A file descriptor of the host can't be handed to a process in another kernel. |  |
| `--timeout` |  | ➕ | Kills the command after a time and exits with 124, like `timeout(1)`. |  |

## start

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --attach` | Docker, Podman, nerdctl | 🟢 |  | `create_start_inspect_rm` |
| `-i, --interactive` | Docker, Podman | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--detach-keys` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--checkpoint, --checkpoint-dir` | Docker, nerdctl | 🔴 | Snapshots, when an executor offers them. | after 0.3 |
| `--all, --filter, --latest` | Podman | 🔴 |  | not planned yet |
| `--sig-proxy` | Podman | 🔴 |  | not planned yet |

## stop, restart

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-s, --signal` | Docker, nerdctl | 🟢 |  | `stop_sends_the_stop_signal_then_powers_off` |
| `-t, --timeout` | Docker, Podman, nerdctl | 🟡 | sbx: `-t, --time`. Docker's older name. No negative values. When it runs out, the guest is powered off (exit code 137) instead of SIGKILL. | `exec_cp_stop_on_a_running_sandbox`, `kill_wait_and_restart` |
| `--all, --cidfile, --filter, --ignore, --latest` | Podman | 🔴 |  | not planned yet |

## kill

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-s, --signal` | Docker, Podman, nerdctl | 🟢 |  | `kill_wait_and_restart` |
| `--all, --cidfile, --latest` | Podman | 🔴 |  | not planned yet |

## rm

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --force` | Docker, Podman, nerdctl | 🟡 | Powers the guest off without a signal. | `system_df_and_prune_free_what_nothing_uses`, `docker_options_reach_the_sandbox` |
| `-l, --link` | Docker | ⚫ | *Why:* Legacy in Docker, and sbx has no links. |  |
| `-v, --volumes` | Docker, Podman, nerdctl | ⚪ | *Why:* sbx makes no anonymous volumes: a sandbox's writable disk goes with it, and named volumes stay (as in Docker). |  |
| `--all, --cidfile, --depend, --filter, --ignore, --latest, --time` | Podman | 🔴 |  | not planned yet |

## ps

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --all` | Docker, Podman, nerdctl | 🟢 |  | `names_work_in_place_of_ids` |
| `-f, --filter` | Docker, Podman, nerdctl | 🟡 | Keys `id`, `name`, `label`, `status`, `exited`, `ancestor`. `name` is a substring, not a regular expression. No `before`, `since`, `health` and the others. An unknown key or value is an error (Docker ignores some). *Why:* What is accepted is a table in code (`filter.rs`), without a regular expression engine. | `filters`, `filter::tests::bad_filters_are_errors_that_say_what_is_wanted` |
| `--format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `json_output_contract` |
| `-q, --quiet` | Docker, Podman, nerdctl | 🟢 |  | `names_work_in_place_of_ids` |
| `-n, --last` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `-l, --latest` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--no-trunc` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `-s, --size` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--external, --noheading, --pod, --sort, --sync, --watch` | Podman | 🔴 |  | not planned yet |
| `--namespace, --ns` | Podman | ⚪ | *Why:* Shows each container's namespaces on the host kernel; a sandbox has its own kernel. |  |

## inspect

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `json_output_contract`, `jq_shapes_json_output` |
| `-s, --size` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--type` | Docker, Podman, nerdctl | 🔴 | `inspect` takes sandboxes only. | not planned yet |
| `--latest` | Podman | 🔴 |  | not planned yet |
| `--mode` | nerdctl | 🔴 | nerdctl's `native` mode shows containerd's own fields. | not planned yet |

## stats

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `--format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `stats_reports_guest_usage` |
| `--no-stream` | Docker, Podman, nerdctl | 🟢 |  | `stats_reports_guest_usage` |
| `-a, --all` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--no-trunc` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--interval, --latest, --no-reset` | Podman | 🔴 |  | not planned yet |

## attach

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `--detach-keys` | Docker, Podman, nerdctl | 🟢 |  | `attach::tests::detach_keys_parse_like_docker` |
| `--no-stdin` | Docker, Podman, nerdctl | 🟢 |  | `docker_options_reach_the_sandbox` |
| `--sig-proxy` | Docker, Podman | 🟢 |  | `attach_tty_and_signal_proxy` |
| `--latest` | Podman | 🔴 |  | not planned yet |

## logs

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --follow` | Docker, Podman, nerdctl | 🟢 |  | `detached_rm_and_logs_follow` |
| `-n, --tail` | Docker, Podman, nerdctl | 🟢 |  | `detached_rm_and_logs_follow`, `behavior::tests::tail_counts_lines_across_records` |
| `-t, --timestamps` | Docker, Podman, nerdctl | 🟡 | One time per chunk of output the shim received, not per line. Refused for logs written by an `sbx` that kept no times. *Why:* The shim writes the chunks it receives as they are (`sbx_shim::log`). | `logs_timestamps_since_until_and_json` |
| `--since` | Docker, Podman, nerdctl | 🟡 | A date without a time zone is refused. Refused for logs without times. *Why:* Whether a line without a time is in the range can't be known, so sbx doesn't guess. | `logs_timestamps_since_until_and_json`, `parse::tests::times_take_rfc3339_unix_seconds_and_durations_ago` |
| `--until` | Docker, Podman, nerdctl | 🟡 | As `--since`. | `logs_timestamps_since_until_and_json` |
| `--details` | Docker, nerdctl | 🔴 |  | not planned yet |
| `--color, --latest, --names` | Podman | 🔴 |  | not planned yet |

## cp

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --archive` | Docker, Podman | 🔴 |  | not planned yet |
| `-L, --follow-link` | Docker, nerdctl | 🔴 |  | not planned yet |
| `-q, --quiet` | Docker | 🔴 |  | not planned yet |
| `--overwrite` | Podman | 🔴 |  | not planned yet |

## events

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --filter` | Docker, Podman, nerdctl | 🟡 | Keys `type`, `event`, `sandbox`, `image`. No `container` or `label`. | `filters`, `events_replay_and_follow` |
| `--format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `events_replay_and_follow` |
| `--since` | Docker, Podman | 🟡 | A date without a time zone is refused. | `events_replay_and_follow` |
| `--until` | Docker, Podman | 🟡 | A date without a time zone is refused. | `events_replay_and_follow` |
| `--no-trunc, --stream` | Podman | 🔴 |  | not planned yet |

## images

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --filter` | Docker, Podman, nerdctl | 🟡 | Keys `reference` (only `*` and `?` patterns), `dangling`, `label`. No `before` or `since`. | `filters`, `filter::tests::globs_match_the_whole_text` |
| `--format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `json_output_contract` |
| `-q, --quiet` | Docker, Podman, nerdctl | 🟢 |  | `names_work_in_place_of_ids` |
| `-a, --all` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--digests` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--no-trunc` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--tree` | Docker, nerdctl | 🔴 |  | not planned yet |
| `--history, --noheading, --sort` | Podman | 🔴 |  | not planned yet |
| `--names` | nerdctl | 🔴 |  | not planned yet |

## pull

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `--platform` | Docker, Podman, nerdctl | 🟡 | `linux/amd64` or `linux/arm64`; the host's by default. Only the host's architecture runs. | `parse::tests::platforms` |
| `-a, --all-tags` | Docker, Podman | 🔴 |  | not planned yet |
| `-q, --quiet` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--disable-content-trust` | Docker, Podman | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `--arch, --os, --variant` | Podman | 🔴 | `--platform` does this. | not planned yet |
| `--authfile, --cert-dir, --creds, --tls-verify` | Podman | 🔴 | sbx uses Docker's `config.json` and the containers `registries.conf`. | not planned yet |
| `--retry, --retry-delay` | Podman | 🔴 |  | not planned yet |
| `--policy` | Podman | 🔴 |  | not planned yet |
| `--decryption-key` | Podman | 🔴 |  | not planned yet |
| `--all-platforms, --unpack` | nerdctl | 🔴 |  | not planned yet |
| `--verify, --cosign-key, --cosign-certificate-identity, --cosign-certificate-identity-regexp, --cosign-certificate-oidc-issuer, --cosign-certificate-oidc-issuer-regexp` | nerdctl | 🔴 | sbx follows the containers `policy.json`, which can require cosign signatures, on every pull. | not planned yet |
| `--soci-index-digest` | nerdctl | 🔴 | Lazy pulling; see Behavior. | not planned yet |
| `--ipfs-address` | nerdctl | 🔴 |  | not planned yet |

## push

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --all-tags` | Docker, nerdctl | 🟡 | Pushes the local tags of the repository one after another, in name order; one that fails doesn't stop the others. | `tag_and_push_to_a_registry`, `app::push::tests::push_names_a_local_tag_or_all_tags_of_a_repository` |
| `-q, --quiet` | Docker, Podman, nerdctl | 🟡 | Prints only the pushed reference, one per line. | `tag_and_push_to_a_registry` |
| `--platform` | Docker, Podman, nerdctl | 🔴 | sbx keeps one platform per image and always pushes that single-platform manifest. | not planned yet |
| `--disable-content-trust` | Docker, Podman | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `--authfile, --cert-dir, --creds, --tls-verify` | Podman | 🔴 |  | not planned yet |
| `--retry, --retry-delay` | Podman | 🔴 |  | not planned yet |
| `--compression-format, --compression-level, --force-compression, --dir-compress` | Podman | 🔴 |  | not planned yet |
| `--format oci, v2s2, v2s1` | Podman | 🔴 | sbx pushes the manifest as it was pulled, or OCI for a built image. | not planned yet |
| `--digestfile` | Podman | 🔴 |  | not planned yet |
| `--encrypt-layer, --encryption-key` | Podman | 🔴 |  | not planned yet |
| `--remove-signatures` | Podman | 🔴 |  | not planned yet |
| `--sign-by, --sign-by-sigstore, --sign-by-sigstore-private-key, --sign-by-sq-fingerprint, --sign-passphrase-file` | Podman | 🔴 |  | not planned yet |
| `--sign, --cosign-key, --notation-key-name` | nerdctl | 🔴 |  | not planned yet |
| `--all-platforms` | nerdctl | 🔴 |  | not planned yet |
| `--allow-nondistributable-artifacts` | nerdctl | 🔴 |  | not planned yet |
| `--soci-span-size, --soci-min-layer-size` | nerdctl | 🔴 |  | not planned yet |
| `--ipfs-address` | nerdctl | 🔴 |  | not planned yet |

## rmi

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --force` | Docker, Podman, nerdctl | 🟡 | Also removes images used by stopped sandboxes in the same data directory, or with several references. Never those of running sandboxes. *Why:* sbx can't see sandboxes in another data directory. | `docker_options_reach_the_sandbox`, `behavior::tests::rmi_table` |
| `--no-prune` | Docker, Podman | ⚪ | *Why:* sbx images have no parent images to keep. |  |
| `--platform` | Docker | 🔴 |  | not planned yet |
| `--all, --ignore` | Podman | 🔴 |  | not planned yet |
| `--async` | nerdctl | 🔴 |  | not planned yet |

## save

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-o, --output` | Docker, Podman, nerdctl | 🟢 |  | `load_and_save_round_trip_an_oci_archive` |
| `--platform` | Docker, nerdctl | 🔴 |  | not planned yet |
| `-q, --quiet` | Podman, nerdctl | 🔴 |  | not planned yet |
| `--format oci-archive, oci-dir, docker-archive, docker-dir` | Podman | 🔴 | sbx writes one archive that is an OCI image layout with Docker's `manifest.json`. | not planned yet |
| `--multi-image-archive` | Podman | 🔴 |  | not planned yet |
| `--compress, --uncompressed, --dir-compress` | Podman | 🔴 |  | not planned yet |
| `--all-platforms` | nerdctl | 🔴 |  | not planned yet |

## load

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-i, --input` | Docker, Podman, nerdctl | 🟢 |  | `load_and_save_round_trip_an_oci_archive` |
| `--platform` | Docker, nerdctl | 🟡 | `linux/amd64` or `linux/arm64`; the host's by default. | `parse::tests::platforms` |
| `-q, --quiet` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--tag` |  | ➕ | Names the image, when the archive holds one. |  |
| `--all-platforms` | nerdctl | 🔴 |  | not planned yet |

## login

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-u, --username` | Docker, Podman, nerdctl | 🟢 |  | `login_and_logout_use_dockers_config` |
| `--password-stdin` | Docker, Podman, nerdctl | 🟢 |  | `login_and_logout_use_dockers_config` |
| `-p, --password` | Docker, Podman, nerdctl | ⚫ | *Why:* A password on the command line ends up in shell history and `ps`. Use `--password-stdin`, or let sbx ask. |  |
| `--authfile, --compat-auth-file, --cert-dir, --tls-verify` | Podman | 🔴 |  | not planned yet |
| `--get-login, --secret, --verbose` | Podman | 🔴 |  | not planned yet |

## volume create

A volume is a sparse disk of its own, formatted (ext4 with a journal) by the guest the first time a sandbox attaches it. Only the `local` kind exists.

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `--label` | Docker, Podman, nerdctl | 🟢 |  | `volume_data_outlives_the_sandbox` |
| `-o, --opt` | Docker, Podman | 🟡 | Only `size=` (e.g. `size=50g`, at least 64m). An unknown option is an error. | `volume_data_outlives_the_sandbox`, `app::volume::tests::only_the_size_option_is_known` |
| `-d, --driver` | Docker, Podman | ⚫ | *Why:* Extensions are out of scope: a volume is a disk of the VM. |  |
| `--availability, --group, --limit-bytes, --required-bytes, --scope, --secret, --sharing, --topology-preferred, --topology-required, --type` | Docker | ⚪ | *Why:* Swarm is out of scope. |  |
| `--uid, --gid` | Podman | 🔴 |  | not planned yet |
| `--ignore` | Podman | 🔴 | `sbx volume create` of an existing name returns it, as Docker does. | not planned yet |

## volume ls

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --filter` | Docker, Podman, nerdctl | 🟡 | Keys `name` (a substring), `label`, `dangling`. No `driver`. | `volume_data_outlives_the_sandbox` |
| `--format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `volume_data_outlives_the_sandbox` |
| `-q, --quiet` | Docker, Podman, nerdctl | 🟢 |  | `volume_data_outlives_the_sandbox` |
| `--cluster` | Docker | ⚪ | *Why:* Swarm is out of scope. |  |
| `--noheading` | Podman | 🔴 |  | not planned yet |
| `--size` | nerdctl | 🔴 | `sbx volume ls` always shows the size and what the disk uses. | not planned yet |

## volume inspect

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `volume_data_outlives_the_sandbox` |
| `--all` | Podman | 🔴 |  | not planned yet |
| `--size` | nerdctl | 🔴 |  | not planned yet |

## volume rm

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-f, --force` | Docker, Podman, nerdctl | 🟡 | Only makes a missing volume no error; a volume in use is never removed. *Why:* Docker's `-f` doesn't remove a volume in use either. | `volume_data_outlives_the_sandbox` |
| `--all, --time` | Podman | 🔴 |  | not planned yet |

## volume prune

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --all` | Docker, Podman | 🟢 |  | `volume_data_outlives_the_sandbox` |
| `-f, --force` | Docker, Podman, nerdctl | 🟢 |  | `volume_data_outlives_the_sandbox` |
| `--filter` | Docker, Podman | 🟡 | Key `label` only. | `volume_data_outlives_the_sandbox` |
| `--dry-run` | Podman | 🔴 |  | not planned yet |

## system df

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `--format` | Docker, Podman, nerdctl | 🟡 | sbx: `--format`. `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. | `system_df_and_prune_free_what_nothing_uses` |
| `-v, --verbose` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |

## system prune

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --all` | Docker, Podman, nerdctl | 🟢 |  | `system_df_and_prune_free_what_nothing_uses` |
| `-f, --force` | Docker, Podman, nerdctl | 🟢 |  | `system_df_and_prune_free_what_nothing_uses` |
| `--filter` | Docker, Podman | 🟡 | Keys `until` and `label`, for sandboxes; `label` also for volumes. | `system_df_and_prune_free_what_nothing_uses` |
| `--volumes` | Docker, Podman, nerdctl | 🟡 | Only with `-a` does it remove anything: every volume no sandbox points at (sbx has no anonymous volumes). | `system_df_and_prune_free_what_nothing_uses` |
| `--build, --external` | Podman | 🔴 |  | not planned yet |

## builder prune

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-a, --all` | Docker, nerdctl | 🟡 | Removes every cached step. Layers that images use stay with the images. | `build_cache_stays_under_its_cap` |
| `-f, --force` | Docker, nerdctl | 🟢 |  | `build_cache_stays_under_its_cap` |
| `--keep-storage` | Docker | 🟡 | Keeps the most recently used cache that no image uses, up to SIZE. Not with `-a`. | `build_cache_stays_under_its_cap` |
| `--max-used-space, --min-free-space, --reserved-space` | Docker | 🔴 | `SBX_BUILD_CACHE_MAX` caps the cache after every build. | not planned yet |
| `--filter` | Docker | 🔴 |  | not planned yet |
| `--verbose` | Docker | 🔴 |  | not planned yet |
| `--builder` | Docker | ⚪ | *Why:* There are no builder instances: each build stage gets its own VM. |  |
| `-D, --debug` | Docker | 🔴 |  | not planned yet |
| `--timeout` | Docker | ⚪ | How long to wait for a builder instance. *Why:* There are no builder instances: each build stage gets its own VM. |  |
| `--buildkit-host` | nerdctl | ⚪ | *Why:* There are no builder instances: each build stage gets its own VM. |  |

## Behavior

How sbx behaves where no single flag is involved.

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| Exit codes 125, 126 and 127 | Docker, Podman | 🟢 |  | `run_failures_before_the_command_are_125`, `run_command_not_found_is_127_and_not_executable_is_126`, `behavior::tests::exit_codes_match_docker` |
| Short image names (`alpine` is `docker.io/library/alpine:latest`) | Docker | 🟢 |  | `json_output_contract` |
| Registry credentials from Docker's `config.json` and credential helpers | Docker, Podman, nerdctl | 🟢 |  | `login_and_logout_use_dockers_config` |
| Image ID prefixes | Docker | 🟡 | At least 12 characters (Docker takes shorter ones). *Why:* A short string that happens to match could remove the wrong image. | `behavior::tests::ambiguous_id_prefix` |
| The main process and PID 1 | Docker | 🟡 | The main process is not PID 1 (sbx's guest init is), so it ends on SIGTERM even without a handler. *Why:* PID 1 in the VM is sbx's guest init. | `exec_cp_stop_on_a_running_sandbox` |
| `stop` or `rm -f` of a sandbox another terminal runs | Docker | 🟡 | Powers the guest off and waits; an error if it doesn't go off. *Why:* Only the process that started a VM can end it by force. | `exec_cp_stop_on_a_running_sandbox` |
| Go templates in `--format` (`--format '{{.ID}}'`) | Docker, Podman, nerdctl | ⚫ | *Why:* `--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`. |  |
| The Docker Engine API socket | Docker, Podman | ⚫ | *Why:* Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`. |  |
| Running another architecture's images (emulation) | Docker, Podman, nerdctl | ⚫ | *Why:* Out of scope: a sandbox runs the host's architecture. |  |
| Lazy pulling (eStargz, SOCI and other snapshotters) | nerdctl | 🔴 | A container starts before its image is fully downloaded. sbx prepares an image's layer disks once and shares them between images. | not planned yet |

## build

`sbx build` takes buildx's flag names. It builds inside one VM per stage, with the network closed unless `--allow` opens it, and caches every step that makes a layer.

| Name | In | | Notes | Evidence or step |
| --- | --- | --- | --- | --- |
| `-t, --tag` | Docker, Podman, nerdctl | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `-f, --file` | Docker, Podman, nerdctl | 🟢 | `-f -` reads the Dockerfile from stdin; the context is still the directory. `FILE.dockerignore` next to the file is read before the context's `.dockerignore`, as in BuildKit. | `build_runs_stages_and_caches_steps`, `build_source_date_epoch_fixes_the_digest` |
| `--build-arg` | Docker, Podman, nerdctl | 🟢 |  | `build_failures_targets_and_build_args` |
| `--target` | Docker, Podman, nerdctl | 🟢 |  | `build_failures_targets_and_build_args` |
| `--no-cache` | Docker, Podman, nerdctl | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `--platform` | Docker, Podman, nerdctl | 🔴 | Images are built for the host's architecture. | after 0.3 |
| `--progress` | Docker, nerdctl | 🟡 | `auto` (the default) prints one line per step and shows a `RUN` step's output only when it fails; `plain` streams it too. There is no `tty` or `rawjson`; `--format json` prints JSON Lines. | `build_runs_stages_and_caches_steps` |
| `-o, --output` | Docker, Podman, nerdctl | 🟡 | `type=sbx` (sbx's image store, the default; `type=image` too), `type=docker` (into `docker load`, or a tar with `dest=FILE`), `type=oci,dest=FILE\|DIR/\|-` (an archive, a layout directory, or stdout), `type=registry` (push). Repeat it for several; given outputs replace the default, so `-o type=docker` alone keeps nothing in sbx (the layer cache stays). The archive is the same file `sbx save` writes. No `type=local` or `type=tar`. Layers are turned into tars when exported, and a layer's compressed digest is fixed the first time it is exported, so every export of an image has the same manifest digest. | `build_outputs_export_one_image_everywhere` |
| `--load` | Docker | 🟢 | Needs `docker` on PATH; without it the build stops before it starts and says to write a tar instead. | `build_outputs_export_one_image_everywhere` |
| `--push` | Docker | 🟢 |  | `build_outputs_export_one_image_everywhere` |
| `--secret` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--ssh` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--add-host` | Docker, Podman, nerdctl | 🔴 |  | after 0.3 |
| `--network` | Docker, Podman, nerdctl | 🟡 | `none` (the default; Docker's default is the full network) or `allow` with `--allow` destinations, as with `run`. With `none`, a failed step lists the names it looked up and says the network is closed. | `build_shows_blocked_names` |
| `--annotation` | Docker, Podman | 🔴 |  | not planned yet |
| `--attest, --provenance, --sbom` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--build-context` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--cache-from, --cache-to` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--call, --check` | Docker | 🔴 |  | not planned yet |
| `--iidfile` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--label` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `--metadata-file` | Docker, Podman | 🔴 |  | not planned yet |
| `--no-cache-filter` | Docker | 🔴 |  | not planned yet |
| `--pull` | Docker, Podman, nerdctl | 🔴 |  | not planned yet |
| `-q, --quiet` | Docker, Podman, nerdctl | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `--shm-size` | Docker, Podman | 🔴 |  | not planned yet |
| `--ulimit` | Docker, Podman | 🔴 |  | not planned yet |
| `--allow` | Docker, nerdctl | 🟡 | Names a network destination `HOST:PORT` that `RUN` steps may reach, as with `run --allow`. buildx's entitlements (`network.host`, `security.insecure`) mean nothing in a build VM. A failed step lists the names it was refused, with the `--allow` to add. | `build_shows_blocked_names` |
| `-D, --debug` | Docker | 🔴 |  | not planned yet |
| `--policy` | Docker | 🔴 |  | not planned yet |
| `--resource` | Docker | 🔴 | `--cpus` and `-m` size the build VM. | not planned yet |
| `--layers, --squash, --squash-all` | Podman | 🔴 | sbx caches every step that makes a layer and keeps the layers. | not planned yet |
| `--manifest, --all-platforms, --arch, --os, --os-feature, --os-version, --variant` | Podman | 🔴 | Images are built for the host's architecture. | after 0.3 |
| `--authfile, --cert-dir, --creds, --tls-verify, --retry, --retry-delay` | Podman | 🔴 |  | not planned yet |
| `--build-arg-file` | Podman | 🔴 |  | not planned yet |
| `--ignorefile` | Podman | 🔴 |  | not planned yet |
| `--from` | Podman | 🔴 |  | not planned yet |
| `--stdin` | Podman | 🔴 |  | not planned yet |
| `--jobs` | Podman | 🔴 |  | not planned yet |
| `--cache-ttl` | Podman | 🔴 |  | not planned yet |
| `--timestamp, --source-date-epoch, --rewrite-timestamp` | Podman | 🔴 | `SOURCE_DATE_EPOCH` in the environment or as a `--build-arg` sets the image's times and caps the file times in new layers (see the Dockerfile section). | not planned yet |
| `--format oci, docker` | Podman | 🔴 |  | not planned yet |
| `--volume, --mount` | Podman | 🔴 |  | not planned yet |
| `--env, --unsetenv, --unsetlabel, --unsetannotation, --inherit-labels, --inherit-annotations, --created-annotation, --identity-label, --layer-label, --stage-labels` | Podman | 🔴 |  | not planned yet |
| `--compress, --compression-format, --compression-level, --force-compression, --disable-compression` | Podman | 🔴 |  | not planned yet |
| `--omit-history` | Podman | 🔴 |  | not planned yet |
| `--save-stages, --skip-unused-stages` | Podman | 🔴 |  | not planned yet |
| `--sbom-output, --sbom-image-output, --sbom-purl-output, --sbom-image-purl-output, --sbom-merge-strategy, --sbom-scanner-command, --sbom-scanner-image` | Podman | 🔴 |  | not planned yet |
| `--sign-by` | Podman | 🔴 |  | not planned yet |
| `--decryption-key` | Podman | 🔴 |  | not planned yet |
| `--dns, --dns-option, --dns-search, --no-hosts, --no-hostname` | Podman | 🔴 |  | not planned yet |
| `--group-add` | Podman | 🔴 |  | not planned yet |
| `--http-proxy` | Podman | 🔴 |  | not planned yet |
| `--iidfile-raw` | Podman | 🔴 |  | not planned yet |
| `--logfile, --logsplit` | Podman | 🔴 |  | not planned yet |
| `--cpp-flag` | Podman | 🔴 |  | not planned yet |
| `--compat-volumes` | Podman | 🔴 |  | not planned yet |
| `--cw` | Podman | 🔴 |  | not planned yet |
| `--rm, --force-rm` | Podman | 🔴 |  | not planned yet |
| `--source-policy-file` | Podman, nerdctl | 🔴 |  | not planned yet |
| `--ipfs` | nerdctl | 🔴 |  | not planned yet |
| `--buildkit-host` | nerdctl | ⚪ | *Why:* There are no builder instances: each build stage gets its own VM. |  |
| `--cap-add, --cap-drop, --security-opt, --ipc, --pid, --uts, --cgroupns, --isolation` | Podman | ⚪ | *Why:* A build step already runs as root in its own VM; there is no container sandbox to tune. |  |
| `--userns, --userns-uid-map, --userns-gid-map, --userns-uid-map-user, --userns-gid-map-group` | Podman | ⚪ | *Why:* A user namespace maps users of a kernel shared with the host. A sandbox has its own kernel and its own users. |  |
| `--cpu-period, --cpu-quota, --cpu-shares, --cpuset-cpus, --cpuset-mems, --memory-swap` | Podman | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |
| `--device` | Podman | ⚪ | *Why:* The VM sees only the devices the executor gives it. |  |
| `--runtime, --runtime-flag, --hooks-dir` | Podman | ⚪ | *Why:* They configure an OCI runtime and its hooks; each build stage is a VM. |  |
| `--disable-content-trust` | Podman | ⚪ | *Why:* sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign). |  |
| `--cpus` |  | ➕ | vCPUs of the build VM (default 1). |  |
| `-m, --memory` | Podman | 🟡 | The memory of each stage's build VM (default 512m); Podman's limits the container of each step. | `build_vm_takes_cpus_and_memory` |
| `--builder` | Docker | ⚪ | *Why:* There are no builder instances: each build stage gets its own VM. |  |
| `--cgroup-parent` | Docker, Podman | ⚪ | *Why:* A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit. |  |

## Dockerfile

What `sbx build` reads. An instruction or flag it doesn't support is an error with the line number, never silently ignored. Behaviors were checked against Docker 29 (BuildKit).

| Name | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `FROM` | 🟡 | With `AS name`, `scratch`, another stage, and `ARG` before `FROM`; `--platform` only for the host's platform. A base image with `ONBUILD` triggers is refused. | `build_runs_stages_and_caches_steps`, `build_scratch_without_a_vm_and_shared_base_layers` |
| `RUN` | 🟡 | Shell and exec forms, in the stage's VM; the network is closed unless `--allow` opens it. Processes a step leaves running are ended when it finishes (BuildKit ends them with the step's container). | `build_failures_targets_and_build_args` |
| `RUN --mount=type=cache` | 🔴 | On top of named volumes. | after 0.3 |
| `RUN --mount=type=secret` | 🔴 |  | after 0.3 |
| `RUN --mount=type=ssh` | 🔴 |  | after 0.3 |
| `RUN --mount=type=bind`, `type=tmpfs` | 🔴 |  | not planned yet |
| `RUN --network` | 🔴 |  | not planned yet |
| `RUN --security=insecure` | ⚪ | *Why:* A build step already runs as root in its own VM; there is no container sandbox to loosen. |  |
| `COPY` | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `COPY --from` | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `COPY --chown`, `--chmod` | 🟢 |  | `build_writes_the_image_config` |
| `COPY --link` | 🔴 |  | after 0.3 |
| `ADD` (local files, local tar archives) | 🔴 | `ADD` of a local file that is not an archive copies it like `COPY`. A tar or compressed file, which Docker would extract, is refused for now. | after 0.3 |
| `ADD <URL>` | 🔴 | Through the same allow list, since the host downloads it. | after 0.3 |
| `ADD <git repository>` | 🔴 |  | after 0.3 |
| `ENV` | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `ARG` | 🟡 | As in BuildKit, except that a `--build-arg` nothing declares is reported, and `--build-arg` cannot set the platform arguments (`TARGETARCH`). | `build_failures_targets_and_build_args` |
| `WORKDIR` | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `USER` | 🟢 |  | `build_writes_the_image_config` |
| `LABEL` | 🟢 |  | `build_writes_the_image_config` |
| `EXPOSE` | 🟢 |  | `build_writes_the_image_config` |
| `VOLUME` | 🟢 |  | `build_writes_the_image_config` |
| `ENTRYPOINT` | 🟢 |  | `build_writes_the_image_config` |
| `CMD` | 🟢 |  | `build_runs_stages_and_caches_steps` |
| `STOPSIGNAL` | 🟢 |  | `build_writes_the_image_config` |
| `SHELL` | 🟢 |  | `build_writes_the_image_config` |
| `HEALTHCHECK` | 🟡 | Written to the image config; sbx says it doesn't run health checks. | `build_writes_the_image_config` |
| `ONBUILD` | 🔴 | An `ONBUILD` line is an error, and so is a base image with `ONBUILD` triggers. | after 0.3 |
| `MAINTAINER` | 🟡 | Deprecated in Docker. Written to the image's `author`, with a warning. | `build_writes_the_image_config` |
| Here-documents (`RUN <<EOF`) | 🔴 |  | after 0.3 |
| Parser directive `# escape=` | 🟢 |  | `build_writes_the_image_config` |
| Parser directive `# syntax=` (another frontend) | ⚫ | *Why:* Another frontend needs BuildKit. sbx reports the line instead of ignoring it. |  |
| `.dockerignore` | 🟢 |  | `build_runs_stages_and_caches_steps` |
| A git repository as the build context | 🔴 |  | after 0.3 |
| `SOURCE_DATE_EPOCH` (fixed times in layers) | 🟡 | From the environment variable or `--build-arg SOURCE_DATE_EPOCH=N`, as in BuildKit: it sets the image's `created` and history times and caps the file times in the layers a build makes (so in exported layer tars too). Layers of the base image keep their times. The promise is the same input and the same cache give the same digest. | `build_source_date_epoch_fixes_the_digest` |

## docker compose

`sbx compose` comes after 0.3 (C1). It reads part of `compose.yaml` and turns each service into `sbx` commands. Podman (`podman compose`, which calls an external provider) and nerdctl (`nerdctl compose`) read the same file.

| Name | | Notes | Evidence or step |
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

| Name | | Notes | Evidence or step |
| --- | --- | --- | --- |
| `services` | 🔴 |  | C1 (after 0.3) |
| `volumes` (top level) | 🔴 |  | C1 (after 0.3) |
| `networks` (top level) | ⚫ | Services reach each other through `--connect`, made from their ports. *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect SANDBOX:PORT` opens one port of another sandbox instead. |  |
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
| `services.*.networks`, `network_mode` | ⚫ | *Why:* Docker networks (bridge, overlay) are out of scope: the VM has no network interface. `--connect SANDBOX:PORT` opens one port of another sandbox instead. |  |
| `services.*.platform` | ⚫ | *Why:* Out of scope: a sandbox runs the host's architecture. |  |
| `services.*.logging` | 🔴 | The shim writes each detached sandbox's log. | not planned yet |
| `services.*.privileged`, `cap_add`, `cap_drop`, `security_opt` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `services.*.devices` | ⚪ | *Why:* The VM sees only the devices the executor gives it. |  |
| `services.*.ipc`, `pid`, `userns_mode` | ⚪ | *Why:* It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, and root in the guest owns only the guest. |  |
| `services.*.init` | ⚪ | *Why:* Always on: sbx's guest init is PID 1. |  |

## Product features

What people install Docker Desktop or one of its alternatives for, beyond the command line. Each product's cell links the official page it comes from. *Yes* means the page says the product has it, and *No* is used only where the page says it doesn't (in its own words). A cell without Yes or No gives what the page says instead, and an empty cell means its documentation doesn't say, not that it lacks the feature. The *sbx* column uses the same legend as the rest of the page.

| Feature | Docker Desktop | Podman Desktop | OrbStack | Colima | Rancher Desktop | sbx | Notes | Evidence or step |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Sharing host folders with containers | [Yes](https://docs.docker.com/desktop/settings-and-maintenance/settings/): VirtioFS or gRPC FUSE on macOS | [Yes](https://docs.podman.io/en/latest/markdown/podman-machine-init.1.html): your home folder by default | [Yes](https://docs.orbstack.dev/docker/file-sharing): VirtioFS | [Yes](https://github.com/abiosoft/colima/blob/main/embedded/defaults/colima.yaml): virtiofs, 9p or sshfs | [Yes](https://docs.rancherdesktop.io/ui/preferences/virtual-machine/volumes): reverse-sshfs, 9p or virtiofs | 🟡 | `-v HOST:CONTAINER` shares a folder live through virtio-fs, when the executor offers it (an error otherwise, never a silent copy); `--mount type=copy` copies one in on every executor. | `volume_is_shared_live`, `parse::tests::volumes` |
| Publishing container ports on localhost | [Yes](https://docs.docker.com/desktop/features/networking/) | [Yes](https://podman-desktop.io/docs/containers/starting-a-container) | [Yes](https://docs.orbstack.dev/docker/network) | ["Automatic Port Forwarding"](https://github.com/abiosoft/colima) | [Yes](https://docs.rancherdesktop.io/tutorials/working-with-containers/) | 🟡 | `-p` listens on 127.0.0.1 unless you write `0.0.0.0:`. TCP only. | `published_ports_reach_the_sandbox` |
| Building images from a Dockerfile | [Yes](https://docs.docker.com/desktop/) | [Yes](https://podman-desktop.io/features) | [Yes](https://docs.orbstack.dev/docker/images): BuildKit | [BuildKit enabled by default](https://github.com/abiosoft/colima/blob/main/embedded/defaults/colima.yaml) | [Yes](https://docs.rancherdesktop.io/ui/images) | 🟡 | `sbx build`, one VM per stage, with the network closed unless `--allow` opens it. | `build_runs_stages_and_caches_steps` |
| Compose | [Yes](https://docs.docker.com/desktop/): included | [Yes](https://podman-desktop.io/docs/compose/setting-up-compose): can install the Compose engine | [Yes](https://docs.orbstack.dev/docker/): included |  | [Yes](https://docs.rancherdesktop.io/references/bundled-utilities/): `docker-compose` bundled | 🔴 |  | C1 (after 0.3) |
| A local Kubernetes cluster | [Yes](https://docs.docker.com/desktop/features/kubernetes/): kubeadm or kind | [Yes](https://podman-desktop.io/docs/kind/installing-extension): kind, a built-in extension | [Yes](https://docs.orbstack.dev/kubernetes/): single node | [Yes](https://github.com/abiosoft/colima/blob/main/embedded/defaults/colima.yaml): k3s | [Yes](https://docs.rancherdesktop.io/): k3s | 🔴 | The roadmap's Kubernetes item is sbx as a containerd runtime (RuntimeClass), not a local cluster. | not planned yet |
| A dashboard of containers | [Yes](https://docs.docker.com/desktop/use-desktop/) | [Yes](https://github.com/podman-desktop/podman-desktop) | [Yes](https://docs.orbstack.dev/docker/) | ["Simple CLI interface"](https://github.com/abiosoft/colima) | [Yes](https://docs.rancherdesktop.io/ui/images) | 🟡 | `sbx tui`: sandboxes, images, volumes, events and executors in the terminal, not a graphical app. Every key runs an `sbx` command and shows it. | `tui::view::tests::sandboxes_tab_shows_rows_hints_and_recent_events`, `tui::model::tests::keys_become_sbx_command_lines_for_the_selected_sandbox` |
| Managing images and volumes on screen | [Yes](https://docs.docker.com/desktop/use-desktop/) | [Yes](https://podman-desktop.io/tutorial/managing-your-application-resources) | [Yes](https://docs.orbstack.dev/release-notes) |  | [Yes](https://docs.rancherdesktop.io/ui/volumes): volumes are experimental | 🟡 | The `sbx tui` images and volumes tabs: list, size and disk use, remove, prune, tag and push. | `tui::model::tests::images_tab_prunes_and_shows_disk_usage`, `tui::model::tests::volumes_tab_lists_users_by_name_and_runs_volume_commands` |
| Extensions | [Yes](https://docs.docker.com/extensions/) | [Yes](https://podman-desktop.io/docs/extensions) |  |  | [Yes](https://docs.rancherdesktop.io/ui/extensions) | ⚫ | *Why:* Extensions are out of scope. |  |
| Updating itself | [Yes](https://docs.docker.com/desktop/settings-and-maintenance/settings/) | [Yes](https://podman-desktop.io/docs/configuration/settings-reference) | [Yes](https://docs.orbstack.dev/headless): with the GUI, not headless | [updated with `brew upgrade colima`](https://github.com/abiosoft/colima/blob/main/docs/FAQ.md) | [Yes](https://docs.rancherdesktop.io/ui/preferences/application/general): macOS and Windows | 🟡 | A notice at startup when a newer signed release exists, and `sbx upgrade`; it doesn't replace itself unasked. | `new_version_notice_and_upgrade` |
| Setting the VM's CPUs, memory and disk | [Yes](https://docs.docker.com/desktop/settings-and-maintenance/settings/) | [Yes](https://podman-desktop.io/docs/podman/creating-a-podman-machine): when the machine is created | [Yes](https://docs.orbstack.dev/settings): CPUs and memory | [Yes](https://github.com/abiosoft/colima): `--cpu`, `--memory`, `--disk` | [Yes](https://docs.rancherdesktop.io/ui/preferences/virtual-machine/hardware): CPUs and memory | 🟡 | Per sandbox (`--cpus`, `-m`, `--storage-opt size=`), since each sandbox is its own VM; there is no shared VM to size. | `docker_options_reach_the_sandbox` |
| Running x86-64 images on Apple silicon (Rosetta, QEMU) | [Yes](https://docs.docker.com/desktop/settings-and-maintenance/settings/) | [Yes](https://podman-desktop.io/docs/podman/rosetta): Rosetta, or QEMU | [Yes](https://docs.orbstack.dev/docker/): Rosetta | [Yes](https://github.com/abiosoft/colima/blob/main/embedded/defaults/colima.yaml): Rosetta, or binfmt emulation | [Yes](https://docs.rancherdesktop.io/ui/preferences/virtual-machine/emulation): Rosetta | ⚫ | *Why:* Out of scope: a sandbox runs the host's architecture. |  |
| A Docker Engine API socket (for Testcontainers and other API clients) | [Yes](https://docs.docker.com/desktop/settings-and-maintenance/settings/) | [Yes](https://podman-desktop.io/docs/migrating-from-docker/managing-docker-compatibility) | [Yes](https://docs.orbstack.dev/docker/) | [Yes](https://github.com/abiosoft/colima/blob/main/docs/FAQ.md): `$HOME/.colima/default/docker.sock` | [Yes](https://docs.rancherdesktop.io/how-to-guides/using-testcontainers/): with the moby engine | ⚫ | *Why:* Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`. |  |

Not features, and not counted:

| | Docker Desktop | Podman Desktop | OrbStack | Colima | Rancher Desktop | sbx |
| --- | --- | --- | --- | --- | --- | --- |
| License and price | [Free for personal use, education, non-commercial open source and small businesses (fewer than 250 employees and less than $10 million in annual revenue); a paid subscription otherwise](https://docs.docker.com/subscription/desktop-license/) | [Apache-2.0](https://github.com/podman-desktop/podman-desktop) | [Free for personal, non-commercial use; a paid license per user for commercial use](https://docs.orbstack.dev/licensing) | [MIT](https://github.com/abiosoft/colima) | [Apache-2.0](https://github.com/rancher-sandbox/rancher-desktop) | Apache-2.0 or MIT, free |
| Host operating systems | [macOS, Windows, Linux](https://docs.docker.com/desktop/) | [macOS, Windows, Linux](https://podman-desktop.io/docs/installation) | [macOS 14 and newer](https://docs.orbstack.dev/faq) | [macOS, Linux](https://github.com/abiosoft/colima) | [macOS, Windows, Linux](https://docs.rancherdesktop.io/) | Linux, and macOS 15+ on Apple silicon; Windows installs, but has no tested executor to run sandboxes yet |
