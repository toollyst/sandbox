//! Docker 호환 표. sbx가 Docker의 무엇을 어떻게 하는지의 정본이다.
//!
//! - Docker CLI의 명령과 옵션, Dockerfile 명령, Compose 키마다 [`Row`] 한 줄이다. 상태([`Status`]), 다른 점과 그
//!   이유, 되는 줄이면 그것을 보이는 테스트 이름, 아직 없는 줄이면 그것을 더할 로드맵 단계를 적는다.
//! - Docker와 다른 점은 모두 여기 있다. 따로 적는 목록은 없다.
//! - 상태는 지금 코드가 하는 것이다. 기획서가 약속하는 것이 아니다.
//! - 문서 `docs/guide/docker-compatibility.md`와 README의 진행률 한 줄은 여기서 만든다. 테스트가 둘이 지금 표와 같은지
//!   확인하고, `SBX_BLESS=1 cargo test -p sbx-cli --lib compat`이 다시 쓴다.
//! - 테스트가 표를 clap 정의와 맞춰 본다. 되는 줄(🟢, 🟡, ➕)의 명령과 옵션은 clap에 있어야 하고 근거 테스트가 실제로
//!   있어야 한다. 되지 않는 줄(🔴, ⚫, ⚪)의 명령과 옵션은 clap에 없어야 한다. clap의 모든 명령과 옵션에는 줄이 있어야
//!   한다. 그래서 기능을 넣거나 빼면 표를 고치지 않고는 CI가 통과하지 않는다.
//! - 줄의 글은 영어다. 사용자가 읽는 문서가 되기 때문이다.
//!
//! [`Row::docker`]의 모양은 절([`Area`])의 종류([`Kind`])가 정한다.
//!
//! - [`Kind::Commands`]: 명령 이름(`run`, `system df`). 루트 옵션이면 그 옵션(`--version`).
//! - [`Kind::Flags`]: 옵션 철자를 `, `로 잇는다(`-m, --memory`). 뒤에 공백과 값 이야기가 올 수 있다
//!   (`-v, --volume NAME:/path`). 값 이야기가 있는 줄은 옵션은 있되 그 값이 되는지를 말하므로, 되지 않는 줄이어도
//!   clap에 옵션이 없는지는 보지 않는다.
//! - [`Kind::Text`]: 아무 글. clap과 맞춰 보지 않는다(Dockerfile, Compose 파일, 동작).
//!
//! sbx의 이름이 Docker와 다르면 [`Row`]의 `sbx`에 sbx의 이름을 같은 모양으로 적는다. ➕ 줄은 `docker`에 sbx의 이름을 적는다.

/// 줄의 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Docker와 같다.
    Same,
    /// 되지만 다르다. 다른 점은 `note`에 있다.
    Differs,
    /// 아직 없다. 더할 단계는 `step`에 있다.
    NotYet,
    /// 하지 않는다. 이유는 `why`에 있다.
    WontDo,
    /// 샌드박스마다 VM이고 데몬이 없는 sbx에서는 의미가 없다. 이유는 `why`에 있다.
    NotApplicable,
    /// Docker에 없는 sbx만의 것.
    SbxOnly,
}

impl Status {
    /// 모든 상태. 문서의 범례와 진행률이 이 순서다.
    pub const ALL: [Status; 6] = [
        Status::Same,
        Status::Differs,
        Status::NotYet,
        Status::WontDo,
        Status::NotApplicable,
        Status::SbxOnly,
    ];

    /// 문서의 아이콘.
    pub const fn icon(self) -> &'static str {
        match self {
            Status::Same => "🟢",
            Status::Differs => "🟡",
            Status::NotYet => "🔴",
            Status::WontDo => "⚫",
            Status::NotApplicable => "⚪",
            Status::SbxOnly => "➕",
        }
    }

    /// 문서의 이름.
    pub const fn label(self) -> &'static str {
        match self {
            Status::Same => "Same as Docker",
            Status::Differs => "Works, with differences",
            Status::NotYet => "Not yet",
            Status::WontDo => "Will not do",
            Status::NotApplicable => "Not applicable in a VM",
            Status::SbxOnly => "sbx only",
        }
    }

    /// 된다(🟢, 🟡). 근거 테스트가 있어야 한다.
    pub const fn works(self) -> bool {
        matches!(self, Status::Same | Status::Differs)
    }

    /// sbx가 받는다(🟢, 🟡, ➕). 그 명령과 옵션이 clap에 있어야 한다.
    pub const fn accepted(self) -> bool {
        matches!(self, Status::Same | Status::Differs | Status::SbxOnly)
    }

    /// 진행률의 분모에 든다: sbx가 의미 있게 할 수 있는 것(🟢, 🟡, 🔴).
    pub const fn counted(self) -> bool {
        matches!(self, Status::Same | Status::Differs | Status::NotYet)
    }
}

/// 줄의 `docker`가 무엇을 가리키는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 명령. sbx에서는 `prefix` 아래의 명령이다(Compose는 `compose`).
    Commands {
        /// sbx 명령 경로의 앞부분.
        prefix: &'static [&'static str],
    },
    /// 옵션. sbx에서는 `paths`의 명령들이 받는 옵션이다. 빈 경로는 루트(`sbx`)다.
    Flags {
        /// sbx 명령 경로들. 첫째가 대표다.
        paths: &'static [&'static [&'static str]],
    },
    /// clap과 맞춰 보지 않는 글.
    Text,
}

/// 표의 절. 문서의 순서가 [`Area::ALL`]이다.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Area {
    Commands,
    Global,
    Run,
    Exec,
    Start,
    Stop,
    Kill,
    Rm,
    Ps,
    Inspect,
    Stats,
    Attach,
    Logs,
    Cp,
    Events,
    Images,
    Pull,
    Push,
    Rmi,
    Save,
    Load,
    Login,
    VolumeCreate,
    VolumeLs,
    VolumeInspect,
    VolumeRm,
    VolumePrune,
    SystemDf,
    SystemPrune,
    BuilderPrune,
    Behavior,
    Build,
    Dockerfile,
    Compose,
    ComposeFile,
}

impl Area {
    /// 문서의 순서.
    pub const ALL: [Area; 35] = [
        Area::Commands,
        Area::Global,
        Area::Run,
        Area::Exec,
        Area::Start,
        Area::Stop,
        Area::Kill,
        Area::Rm,
        Area::Ps,
        Area::Inspect,
        Area::Stats,
        Area::Attach,
        Area::Logs,
        Area::Cp,
        Area::Events,
        Area::Images,
        Area::Pull,
        Area::Push,
        Area::Rmi,
        Area::Save,
        Area::Load,
        Area::Login,
        Area::VolumeCreate,
        Area::VolumeLs,
        Area::VolumeInspect,
        Area::VolumeRm,
        Area::VolumePrune,
        Area::SystemDf,
        Area::SystemPrune,
        Area::BuilderPrune,
        Area::Behavior,
        Area::Build,
        Area::Dockerfile,
        Area::Compose,
        Area::ComposeFile,
    ];

    /// 문서의 절 제목.
    pub const fn title(self) -> &'static str {
        match self {
            Area::Commands => "Commands",
            Area::Global => "Global options",
            Area::Run => "docker run, docker create",
            Area::Exec => "docker exec",
            Area::Start => "docker start",
            Area::Stop => "docker stop, docker restart",
            Area::Kill => "docker kill",
            Area::Rm => "docker rm",
            Area::Ps => "docker ps",
            Area::Inspect => "docker inspect",
            Area::Stats => "docker stats",
            Area::Attach => "docker attach",
            Area::Logs => "docker logs",
            Area::Cp => "docker cp",
            Area::Events => "docker events",
            Area::Images => "docker images",
            Area::Pull => "docker pull",
            Area::Push => "docker push",
            Area::Rmi => "docker rmi",
            Area::Save => "docker save",
            Area::Load => "docker load",
            Area::Login => "docker login",
            Area::VolumeCreate => "docker volume create",
            Area::VolumeLs => "docker volume ls",
            Area::VolumeInspect => "docker volume inspect",
            Area::VolumeRm => "docker volume rm",
            Area::VolumePrune => "docker volume prune",
            Area::SystemDf => "docker system df",
            Area::SystemPrune => "docker system prune",
            Area::BuilderPrune => "docker builder prune",
            Area::Behavior => "Behavior",
            Area::Build => "docker build",
            Area::Dockerfile => "Dockerfile",
            Area::Compose => "docker compose",
            Area::ComposeFile => "compose.yaml",
        }
    }

    /// 줄의 `docker`가 무엇을 가리키는지.
    pub const fn kind(self) -> Kind {
        match self {
            Area::Commands => Kind::Commands { prefix: &[] },
            Area::Compose => Kind::Commands {
                prefix: &["compose"],
            },
            Area::Global => flags(&[&[]]),
            Area::Run => flags(&[&["run"], &["create"]]),
            Area::Exec => flags(&[&["exec"]]),
            Area::Start => flags(&[&["start"]]),
            Area::Stop => flags(&[&["stop"], &["restart"]]),
            Area::Kill => flags(&[&["kill"]]),
            Area::Rm => flags(&[&["rm"]]),
            Area::Ps => flags(&[&["ps"]]),
            Area::Inspect => flags(&[&["inspect"]]),
            Area::Stats => flags(&[&["stats"]]),
            Area::Attach => flags(&[&["attach"]]),
            Area::Logs => flags(&[&["logs"]]),
            Area::Cp => flags(&[&["cp"]]),
            Area::Events => flags(&[&["events"]]),
            Area::Images => flags(&[&["images"]]),
            Area::Pull => flags(&[&["pull"]]),
            Area::Push => flags(&[&["push"]]),
            Area::Rmi => flags(&[&["rmi"]]),
            Area::Save => flags(&[&["save"]]),
            Area::Load => flags(&[&["load"]]),
            Area::Login => flags(&[&["login"]]),
            Area::VolumeCreate => flags(&[&["volume", "create"]]),
            Area::VolumeLs => flags(&[&["volume", "ls"]]),
            Area::VolumeInspect => flags(&[&["volume", "inspect"]]),
            Area::VolumeRm => flags(&[&["volume", "rm"]]),
            Area::VolumePrune => flags(&[&["volume", "prune"]]),
            Area::SystemDf => flags(&[&["system", "df"]]),
            Area::SystemPrune => flags(&[&["system", "prune"]]),
            Area::BuilderPrune => flags(&[&["builder", "prune"]]),
            Area::Build => flags(&[&["build"]]),
            Area::Behavior | Area::Dockerfile | Area::ComposeFile => Kind::Text,
        }
    }

    /// Docker CLI의 절인지. `build`, Dockerfile, Compose(0.3과 그 뒤에 오는 것)가 아니다. 진행률을 따로 센다.
    pub const fn is_cli(self) -> bool {
        !matches!(
            self,
            Area::Build | Area::Dockerfile | Area::Compose | Area::ComposeFile
        )
    }

    /// 절 제목 아래의 한두 문장.
    pub const fn intro(self) -> &'static str {
        match self {
            Area::Commands => {
                "Every `docker` command and management command. The flags of the commands sbx has are in the \
                 sections below."
            }
            Area::Global => "Options that go before the command (`docker --debug ps`).",
            Area::Run => {
                "`create` takes the same flags as `run`, except `-d`. Many of Docker's flags tune namespaces and \
                 cgroups on a shared kernel; in sbx each sandbox has its own kernel, and the VM's size is the \
                 limit."
            }
            Area::Behavior => "How sbx behaves where no single flag is involved.",
            Area::Build => {
                "`sbx build` takes buildx's flag names. It builds inside one VM per stage, with the network \
                 closed unless `--allow` opens it, and caches every step that makes a layer."
            }
            Area::Dockerfile => {
                "What `sbx build` reads. An instruction or flag it doesn't support is an error with the line \
                 number, never silently ignored. Behaviors were checked against Docker 29 (BuildKit)."
            }
            Area::Compose => {
                "`sbx compose` comes after 0.3 (C1). It reads part of `compose.yaml` and turns each service into \
                 `sbx` commands."
            }
            Area::ComposeFile => "Keys `sbx compose` will read. An unknown key is an error.",
            Area::VolumeCreate => {
                "A volume is a sparse disk of its own, formatted (ext4 with a journal) by the guest the first time a \
                 sandbox attaches it. Only the `local` kind exists."
            }
            _ => "",
        }
    }
}

const fn flags(paths: &'static [&'static [&'static str]]) -> Kind {
    Kind::Flags { paths }
}

/// 표의 한 줄.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// 절.
    pub area: Area,
    /// Docker의 이름. 모양은 [`Area::kind`]가 정한다(모듈 문서).
    pub docker: &'static str,
    /// sbx의 이름이 다를 때 그 이름(같은 모양).
    pub sbx: Option<&'static str>,
    /// 상태.
    pub status: Status,
    /// 무엇이 다른지, 또는 짧은 설명.
    pub note: &'static str,
    /// 왜 그런지. ⚫·⚪에는 있어야 한다.
    pub why: &'static str,
    /// 근거 테스트. 이름만이면 `tests/e2e.rs`의 끝-끝 테스트, `::`가 있으면 이 크레이트의 단위 테스트
    /// (`parse::tests::memory_units`). 🟢·🟡에는 있어야 한다.
    pub evidence: &'static [&'static str],
    /// 🔴의 단계: 로드맵 단계(`D3`), [`LATER`], [`UNPLANNED`].
    pub step: &'static str,
}

/// 0.3 뒤에 하기로 한 것(로드맵 "그 뒤").
pub const LATER: &str = "later";
/// 아직 정하지 않았다.
pub const UNPLANNED: &str = "unplanned";

/// 0.3(로드맵 M9)의 단계. 🔴의 `step`은 이것이거나 `C1`(compose, 0.3 뒤), [`LATER`], [`UNPLANNED`]다.
pub const STEPS_0_3: &[&str] = &[
    "P1", "D1", "D2", "S1", "D3", "N1", "V2", "D4", "D5", "V1", "V4", "V3", "D6", "T4",
];

const fn row(area: Area, docker: &'static str, status: Status) -> Row {
    Row {
        area,
        docker,
        sbx: None,
        status,
        note: "",
        why: "",
        evidence: &[],
        step: "",
    }
}

impl Row {
    const fn note(self, note: &'static str) -> Self {
        Self { note, ..self }
    }
    const fn why(self, why: &'static str) -> Self {
        Self { why, ..self }
    }
    const fn by(self, evidence: &'static [&'static str]) -> Self {
        Self { evidence, ..self }
    }
    const fn step(self, step: &'static str) -> Self {
        Self { step, ..self }
    }
    const fn sbx(self, sbx: &'static str) -> Self {
        Self {
            sbx: Some(sbx),
            ..self
        }
    }
}

const fn same(area: Area, docker: &'static str, evidence: &'static [&'static str]) -> Row {
    row(area, docker, Status::Same).by(evidence)
}
const fn differs(
    area: Area,
    docker: &'static str,
    note: &'static str,
    evidence: &'static [&'static str],
) -> Row {
    row(area, docker, Status::Differs).note(note).by(evidence)
}
const fn not_yet(area: Area, docker: &'static str, step: &'static str) -> Row {
    row(area, docker, Status::NotYet).step(step)
}
const fn wont(area: Area, docker: &'static str, why: &'static str) -> Row {
    row(area, docker, Status::WontDo).why(why)
}
const fn na(area: Area, docker: &'static str, why: &'static str) -> Row {
    row(area, docker, Status::NotApplicable).why(why)
}
const fn only(area: Area, sbx: &'static str, note: &'static str) -> Row {
    row(area, sbx, Status::SbxOnly).note(note)
}

use Area::*;

// 자주 쓰는 이유.
const NO_DAEMON: &str = "There is no daemon: each `sbx` command reads and writes the data and cache directories itself.";
const OWN_KERNEL: &str = "It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, \
                          and root in the guest owns only the guest.";
const CGROUP: &str =
    "A cgroup knob for a process on a shared kernel. The VM's size (`--cpus`, `-m`) is the limit.";
const NO_NIC: &str =
    "The VM has no network interface. Connections leave through the host, by name.";
const WINDOWS_CONTAINERS: &str = "Only for Windows containers.";
const NO_DCT: &str =
    "sbx has no Docker Content Trust. Signatures follow the containers `policy.json` (cosign).";
const NO_NETWORKS: &str = "Docker networks (bridge, overlay) are out of scope: the VM has no network interface. \
                           `--connect SANDBOX:PORT` opens one port of another sandbox instead.";
const GO_TEMPLATES: &str =
    "`--format` takes `text` or `json`, not a Go template. Pick fields with `--jq`.";
const SWARM: &str = "Swarm is out of scope.";

/// 모든 줄. 절마다 붙여 둔다(문서의 순서다).
pub const ROWS: &[Row] = &[
    // ── 명령 ─────────────────────────────────────────────────────────────
    differs(Commands, "run", "Each sandbox is a microVM. Pulls and prepares the image on first use.", &[
        "run_streams_output_and_returns_the_exit_code",
    ]),
    differs(Commands, "create", "`create --rm` is refused: only `run` removes a sandbox when it exits.", &[
        "create_start_inspect_rm",
    ]),
    same(Commands, "start", &["create_start_inspect_rm"])
        .note("A stopped sandbox keeps its disk and starts again, as in Docker (unlike the OCI runtime spec)."),
    differs(Commands, "stop", "Sends the stop signal, then powers the guest off (sync, exit code 137) instead of SIGKILL.", &[
        "exec_cp_stop_on_a_running_sandbox",
        "stop_sends_the_stop_signal_then_powers_off",
    ])
    .why("A sandbox is a VM: stopping it is shutting the VM down."),
    differs(Commands, "restart", "Stops as `stop` does, then starts detached.", &["kill_wait_and_restart"]),
    same(Commands, "kill", &["kill_wait_and_restart"]),
    same(Commands, "wait", &["kill_wait_and_restart", "json_output_contract"]),
    differs(Commands, "rm", "`rm -f` powers a running sandbox off without a signal.", &[
        "exec_cp_stop_on_a_running_sandbox",
        "system_df_and_prune_free_what_nothing_uses",
    ]),
    differs(Commands, "ps", "Sandboxes without `--name` have no name (Docker makes one up).", &["names_work_in_place_of_ids"]),
    differs(Commands, "inspect", "Sandboxes only, one at a time, in sbx's own JSON shape ([cli-output.md](../developers/cli-output.md)).", &[
        "create_start_inspect_rm",
        "json_output_contract",
    ]),
    differs(Commands, "stats", "CPU, memory, writable disk and process count, as the guest reports them. No network or block I/O columns.", &[
        "stats_reports_guest_usage",
        "app::stats::tests::cpu_percent_is_per_vcpu_like_docker",
    ]),
    same(Commands, "exec", &["exec_cp_stop_on_a_running_sandbox"]),
    differs(Commands, "attach", "Only to a detached sandbox (`run -d`, `start`), not to one another `sbx` runs attached. Not on Windows yet.", &[
        "attach_to_a_detached_sandbox",
    ])
    .why("The shim that owns a detached sandbox holds its main process's input and output; there is no daemon."),
    differs(Commands, "logs", "Only for detached sandboxes. Up to 256 MiB per sandbox; past that, output is dropped and one line says so (Docker's json-file has no limit).", &[
        "detached_run_outlives_sbx_and_keeps_logs",
        "detached_rm_and_logs_follow",
    ])
    .why("No daemon collects the output: the shim writes it. An attached `run` shows it directly and keeps nothing."),
    differs(Commands, "cp", "Only with a running sandbox. Copying out never writes outside the destination you name.", &[
        "exec_cp_stop_on_a_running_sandbox",
    ])
    .why("The guest reads and writes its disk, so it has to be running."),
    differs(Commands, "pull", "Also follows the containers `registries.conf` (mirrors, blocked registries) and `policy.json` (cosign signatures).", &[
        "registries_conf_is_read_for_pulls",
        "policy_json_is_followed",
    ]),
    differs(Commands, "images", "Also lists which root filesystem disks are prepared, and SIZE / SHARED / UNIQUE on disk (layers several images use are shared, as in `docker system df -v`).", &[
        "names_work_in_place_of_ids",
        "json_output_contract",
        "images_on_one_base_store_it_once",
    ]),
    differs(Commands, "rmi", "Never removes the image of a running sandbox, even with `-f`.", &[
        "json_output_contract",
        "docker_options_reach_the_sandbox",
        "behavior::tests::rmi_table",
    ]),
    differs(Commands, "save", "One image per archive, written as an OCI image layout (with Docker's `manifest.json`): a pulled image exactly as it was pulled, a built one exported from its layers (the same bytes every time).", &[
        "load_and_save_round_trip_an_oci_archive",
        "build_outputs_export_one_image_everywhere",
    ]),
    differs(Commands, "load", "Reads OCI image layout archives or directories: `docker save` from Docker 25 on, `podman save --format oci-archive`, skopeo.", &[
        "load_and_save_round_trip_an_oci_archive",
    ]),
    same(Commands, "login", &["login_and_logout_use_dockers_config"])
        .note("Writes Docker's `config.json` or its credential helper, so `docker login` works too."),
    same(Commands, "logout", &["login_and_logout_use_dockers_config"]),
    differs(Commands, "events", "Kinds `sandbox`, `image`, `volume` and `network`; actions create, start, stop, remove, `exec_start`, `exec_die`, pull, prepare, tag, untag, push, delete, and for `network` connect, close, refuse, lookup, `proxy_only`, notice. RFC 3339 times with nanoseconds. Keeps the last 1-2 MiB (Docker: the last 256 events).", &[
        "events_replay_and_follow",
        "tag_and_push_to_a_registry",
        "volume_data_outlives_the_sandbox",
    ])
    .why("No daemon: the process that changes a state records it, so a sandbox whose owner dies suddenly has no stop event."),
    differs(Commands, "info", "Shows the host, the executor and the storage sbx uses.", &["commands_that_do_not_launch_need_no_executor"]),
    differs(Commands, "version", "There is no `version` command.", &["docker_options_reach_the_sandbox"])
        .sbx("--version"),
    differs(Commands, "system", "`df` and `prune`.", &["system_df_and_prune_free_what_nothing_uses"]),
    differs(Commands, "system df", "Images, sandboxes, volumes and executors, by what is actually on disk. A layer several images use is counted once; `SHARED` is that part.", &[
        "system_df_and_prune_free_what_nothing_uses",
        "images_on_one_base_store_it_once",
    ]),
    differs(Commands, "system prune", "Stopped sandboxes, unused layers (blobs and layer disks) and leftovers of interrupted pulls older than an hour; volumes only with `--volumes`.", &[
        "system_df_and_prune_free_what_nothing_uses",
    ]),
    not_yet(Commands, "system events", UNPLANNED).note("`sbx events` does this."),
    not_yet(Commands, "system info", UNPLANNED).note("`sbx info` does this."),
    differs(Commands, "build", "Each stage's `RUN` steps run in one microVM with the network closed unless `--allow` opens it; `COPY`, `ADD` and `WORKDIR` layers are made on the host, so a `FROM scratch` stage of copies needs no VM. The image goes to the sbx image store unless `--output` says otherwise; `save` and `push` export a built image like any other.", &["build_runs_stages_and_caches_steps", "build_scratch_without_a_vm_and_shared_base_layers", "build_outputs_export_one_image_everywhere"]),
    wont(Commands, "buildx build", "`sbx build` is the builder and takes buildx's flag names; there is no builder plugin to call."),
    differs(Commands, "builder", "`prune` only. The build cache also keeps itself under a cap after every build (`SBX_BUILD_CACHE_MAX`, default 10 GiB), dropping the least recently used steps whose layers no image uses. `sbx system df` shows it; `sbx system prune` clears it.", &["build_cache_stays_under_its_cap"]),
    differs(Commands, "builder prune", "Removes cached steps whose layers no image uses (`-a`: every cached step; `--keep-storage`: keeps the most recently used up to a size). Without a terminal it needs `-f`.", &["build_cache_stays_under_its_cap"]),
    differs(Commands, "push", "Sends the layers, config and manifest exactly as they were pulled, so the image keeps its digest; a built image is exported from its layers, with the same digest on every push. One blob at a time (Docker sends several at once). Only to the registry the name points at: never to a `registries.conf` mirror, and a blocked registry is refused.", &[
        "tag_and_push_to_a_registry",
        "build_outputs_export_one_image_everywhere",
        "app::push::tests::report_reads_like_docker_push",
    ]),
    same(Commands, "tag", &["tag_and_push_to_a_registry"])
        .note("Moves a reference that pointed at another image, as Docker does, and records an image `tag` event."),
    differs(Commands, "volume", "`create`, `ls`, `inspect`, `rm`, `prune`. A volume is a sparse disk that outlives sandboxes. One running sandbox at a time attaches a volume read-write; read-only attaches share it while nobody writes. A second writer is refused and told which sandbox holds it.", &[
        "volume_data_outlives_the_sandbox",
        "a_volume_has_one_writer_at_a_time",
    ])
    .why("A volume is a block device with one file system on it. Two kernels writing it at once would corrupt it, so the host locks it for the VM's lifetime."),
    differs(Commands, "volume create", "16 GiB by default (sparse: the host spends only what is written). Creating an existing name again returns it, as Docker does; with another size or labels it is an error.", &[
        "volume_data_outlives_the_sandbox",
        "app::volume::tests::only_the_size_option_is_known",
    ]),
    differs(Commands, "volume ls", "Also shows the size, what the disk uses on the host and the sandboxes that point at each volume.", &[
        "volume_data_outlives_the_sandbox",
    ]),
    differs(Commands, "volume inspect", "Size, labels, the disk file, the sandboxes that point at it and whether one has it attached now. No `Mountpoint`: the data lives inside the disk.", &[
        "volume_data_outlives_the_sandbox",
        "mcp_uses_only_its_own_volumes",
    ]),
    same(Commands, "volume rm", &["volume_data_outlives_the_sandbox", "a_volume_has_one_writer_at_a_time"])
        .note("Refused while a sandbox points at the volume, even a stopped one."),
    differs(Commands, "volume prune", "Removes nothing without `-a`, because sbx has no anonymous volumes; `-a` removes every volume no sandbox points at (Docker 23 and later: the same).", &[
        "volume_data_outlives_the_sandbox",
    ]),
    not_yet(Commands, "volume update", UNPLANNED).note("Docker's only takes cluster volumes."),
    not_yet(Commands, "compose", "C1"),
    not_yet(Commands, "commit", LATER),
    not_yet(Commands, "diff", LATER),
    not_yet(Commands, "history", LATER),
    not_yet(Commands, "rename", LATER),
    not_yet(Commands, "top", LATER),
    not_yet(Commands, "port", LATER).note("`sbx inspect` shows the published ports."),
    not_yet(Commands, "update", LATER),
    not_yet(Commands, "pause", LATER).note("When an executor can pause a VM."),
    not_yet(Commands, "unpause", LATER).note("When an executor can pause a VM."),
    not_yet(Commands, "checkpoint", LATER).note("Snapshots, when an executor offers them."),
    not_yet(Commands, "search", UNPLANNED),
    not_yet(Commands, "export", UNPLANNED),
    not_yet(Commands, "import", UNPLANNED),
    not_yet(Commands, "manifest", UNPLANNED),
    not_yet(Commands, "container", UNPLANNED).note("The same commands exist at the top level (`sbx ps`, `sbx rm`)."),
    not_yet(Commands, "image", UNPLANNED).note("The same commands exist at the top level (`sbx images`, `sbx rmi`)."),
    wont(Commands, "network", NO_NETWORKS),
    na(Commands, "context", NO_DAEMON).note("`--executor` and `sbx executor use` pick what runs the VMs."),
    wont(Commands, "plugin", "Extensions are out of scope."),
    na(Commands, "trust", NO_DCT),
    wont(Commands, "swarm", SWARM),
    wont(Commands, "node", SWARM),
    wont(Commands, "service", SWARM),
    wont(Commands, "stack", SWARM),
    wont(Commands, "secret", SWARM),
    wont(Commands, "config", SWARM),
    only(Commands, "prepare", "Builds an image's layer disks ahead of time, one per layer and shared between images, for the executor `run` would use (`run` does it on first use)."),
    only(Commands, "tui", "Sandboxes, images, volumes, events and executors on one screen, like Docker Desktop or lazydocker. Every key runs an `sbx` command and shows its command line."),
    only(Commands, "mcp", "Serves sandboxes to AI agents over the Model Context Protocol ([guide](mcp.md))."),
    only(Commands, "executor", "Installs and picks executors, the programs that run the VMs ([guide](executors.md))."),
    only(Commands, "upgrade", "Replaces sbx with the newest signed release."),
    only(Commands, "completion", "Prints a shell completion script. On each TAB the shell runs `COMPLETE=<shell> sbx -- <words>`, which completes sandbox names and IDs, image references and executors, and changes nothing. So with `COMPLETE` set (not empty, not 0), sbx only completes."),
    // ── 전역 옵션 ─────────────────────────────────────────────────────────
    differs(Global, "-v, --version", "`-v` is not an option.", &["docker_options_reach_the_sandbox"]).sbx("-V, --version"),
    not_yet(Global, "--config", UNPLANNED).note("sbx reads `DOCKER_CONFIG` for registry credentials."),
    not_yet(Global, "-D, --debug", UNPLANNED),
    not_yet(Global, "-l, --log-level", UNPLANNED),
    na(Global, "-c, --context", NO_DAEMON),
    na(Global, "-H, --host", NO_DAEMON),
    na(Global, "--tls, --tlscacert, --tlscert, --tlskey, --tlsverify", NO_DAEMON),
    only(Global, "--format", "`text` or `json` for every command; the JSON shapes are a public contract ([cli-output.md](../developers/cli-output.md)).")
        .why("A Go template engine can't be reproduced exactly in Rust, and a near copy would surprise. jq is the common language for JSON."),
    only(Global, "--jq", "With `--format json`: a built-in jq expression picks fields (like `gh`)."),
    only(Global, "--data-dir", "Where sandbox state lives."),
    only(Global, "--cache-dir", "Where images and their disks are cached."),
    only(Global, "--socket-dir", "Where guest connection sockets go."),
    only(Global, "--guest-init", "Another guest init initramfs than the one built into sbx."),
    only(Global, "--executor", "The executor that runs the VM."),
    only(Global, "--shim", "The program that owns detached sandboxes."),
    // ── run, create ──────────────────────────────────────────────────────
    differs(Run, "-d, --detach", "A small `sbx-shim` process owns each detached sandbox.", &[
        "detached_run_outlives_sbx_and_keeps_logs",
    ])
    .why("There is no daemon; a VM is owned by the process that started it."),
    same(Run, "--entrypoint", &["docker_options_reach_the_sandbox"]),
    same(Run, "-e, --env", &["names_work_in_place_of_ids", "parse::tests::env_values"]),
    same(Run, "--env-file", &["names_work_in_place_of_ids", "parse::tests::env_files_follow_docker"]),
    differs(Run, "-h, --hostname", "`-h` is help.", &["docker_options_reach_the_sandbox"]).sbx("--hostname"),
    same(Run, "-i, --interactive", &["run_i_forwards_stdin"]),
    same(Run, "-l, --label", &["filters", "docker_options_reach_the_sandbox"]),
    differs(Run, "-m, --memory", "512 MiB by default (Docker: no limit).", &[
        "docker_options_reach_the_sandbox",
        "parse::tests::memory_units",
    ])
    .why("A VM is started with a fixed size."),
    differs(Run, "--cpus", "Whole vCPUs only; 1 by default (Docker: no limit).", &[
        "docker_options_reach_the_sandbox",
        "parse::tests::cpus_must_be_whole",
    ])
    .why("A VM is started with a fixed number of vCPUs."),
    differs(Run, "--mount", "`type=bind` (needs an executor with live sharing; an error otherwise), `type=volume` and `type=copy` (below). No `tmpfs` yet.", &[
        "docker_options_reach_the_sandbox",
        "parse::tests::mounts",
    ]),
    differs(Run, "--name", "Without it a sandbox has no name and is found by its ID (Docker makes a name up).", &[
        "names_work_in_place_of_ids",
    ])
    .why("A made-up name shows up for people who don't use names and only adds clashes. A caller that wants one picks it."),
    differs(Run, "--network", "`none` (the default; Docker's is `bridge`) or `allow`: TCP to the `--allow` destinations only, checked by the host by name. Programs that use `HTTP_PROXY` go through a proxy in the guest; others reach `HOST:PORT` rules through the guest's DNS, which answers from the allow list with stand-in addresses the guest listens on. `HOST:*` rules, IPv6 and UDP (beyond DNS) go through the proxy only or not at all.", &[
        "network_allow_list",
        "transparent_tcp_reaches_allowed_names",
        "behavior::tests::network_policy_table",
    ])
    .why("The network is closed by default and opened by name. The allow list is enforced on every OS without help from the executor, and the VM still has no network interface."),
    wont(Run, "--network bridge, host, container:NAME, NAME", NO_NETWORKS),
    differs(Run, "-p, --publish", "Without an IP it listens on 127.0.0.1 (Docker: every address). TCP only. The process that owns the VM listens, and connects to 127.0.0.1:PORT in the guest.", &[
        "published_ports_reach_the_sandbox",
    ])
    .why("Closed by default: write `0.0.0.0:` to open a port beyond this machine. The VM has no network interface, so the port is reached on the guest's loopback."),
    not_yet(Run, "-p, --publish …/udp", LATER),
    same(Run, "--pull", &["run_failures_before_the_command_are_125", "behavior::tests::pull_policy_default_matches_docker"]),
    differs(Run, "--rm", "`run` only: `create --rm` is refused.", &[
        "run_streams_output_and_returns_the_exit_code",
        "create_start_inspect_rm",
    ]),
    differs(Run, "--runtime", "A global option that picks the program that runs the VM.", &[
        "installed_executor_is_used_by_default",
        "vmm_packages_pins_and_version_files",
    ])
    .sbx("--executor"),
    same(Run, "--stop-signal", &["stop_sends_the_stop_signal_then_powers_off"]),
    differs(Run, "--stop-timeout", "No negative values (wait forever).", &["stop_sends_the_stop_signal_then_powers_off"])
        .why("Whoever waits can stop waiting (Ctrl+C, then `kill`)."),
    differs(Run, "--storage-opt", "Only `size=`: the writable disk, 1 GiB by default, at least 64 MiB. Sparse, so the host only spends what the sandbox writes.", &[
        "docker_options_reach_the_sandbox",
        "parse::tests::storage_opt_sets_the_writable_disk_size",
    ]),
    same(Run, "-t, --tty", &["run_t_gives_the_main_process_a_terminal"]),
    same(Run, "-u, --user", &["docker_options_reach_the_sandbox"]),
    differs(Run, "-v, --volume", "With a host path, live-shares a host directory (`SRC:DST[:ro]`), which needs an executor with virtio-fs; without one it is an error that names the alternatives (`--mount type=copy`, an executor with live sharing), never a silent copy. Host edits reach file watchers in the sandbox (hot reload) as a modify event on the changed file or, for a removed one, its folder; there are no create or delete events, and none on a `:ro` share. With a name, attaches a volume (below).", &[
        "volume_is_shared_live",
        "live_share_tells_the_guest_or_points_elsewhere",
        "parse::tests::volumes",
    ])
    .why("Executors differ in what they can do. sbx doesn't switch to another way of sharing behind your back."),
    differs(Run, "-v, --volume NAME:/path", "Attaches a named volume; a name used for the first time creates it. The first attach fills an empty volume with what the image has at that path (owner, mode, times), unless `nocopy`. `ro` is read-only on the host too. A volume attached read-write by a running sandbox is refused, naming that sandbox.", &[
        "volume_data_outlives_the_sandbox",
        "a_volume_has_one_writer_at_a_time",
        "volume_disks_limits_and_old_executors",
        "parse::tests::volume_names_are_told_from_paths_like_docker",
    ])
    .why("Each volume is a disk of the VM, attached after the root file system and the writable disk, so an executor's disk count limits how many a sandbox takes."),
    wont(Run, "-v, --volume /path", "An anonymous volume is a disk nobody named that lingers after the sandbox. Name it (`-v NAME:/path`); `--storage-opt size=` sizes the writable disk."),
    differs(Run, "--mount type=volume", "`source`, `target`, `readonly`, `volume-nocopy`. No `volume-driver`, `volume-opt` or `volume-subpath`.", &[
        "volume_data_outlives_the_sandbox",
        "parse::tests::volume_names_are_told_from_paths_like_docker",
    ]),
    not_yet(Run, "--mount type=tmpfs", UNPLANNED),
    same(Run, "-w, --workdir", &["docker_options_reach_the_sandbox"]),
    not_yet(Run, "--add-host", LATER),
    not_yet(Run, "-a, --attach", UNPLANNED).note("An attached sandbox gets stdin (with `-i`), stdout and stderr."),
    not_yet(Run, "--cidfile", UNPLANNED),
    not_yet(Run, "--detach-keys", UNPLANNED).note("`sbx attach --detach-keys` has it."),
    not_yet(Run, "--dns, --dns-option, --dns-search", LATER),
    not_yet(Run, "--domainname", UNPLANNED),
    not_yet(Run, "--gpus", LATER).note("When an executor offers a GPU."),
    not_yet(Run, "--group-add", UNPLANNED),
    not_yet(Run, "--health-cmd, --health-interval, --health-retries, --health-start-period, --health-start-interval, --health-timeout", LATER),
    not_yet(Run, "--no-healthcheck", LATER),
    not_yet(Run, "--label-file", UNPLANNED),
    not_yet(Run, "--log-driver, --log-opt", UNPLANNED).note("The shim writes each detached sandbox's log."),
    not_yet(Run, "-P, --publish-all", UNPLANNED),
    not_yet(Run, "-q, --quiet", UNPLANNED),
    not_yet(Run, "--read-only", UNPLANNED),
    not_yet(Run, "--restart", LATER).note("Without a daemon, through launchd or systemd."),
    not_yet(Run, "--shm-size", UNPLANNED),
    not_yet(Run, "--sig-proxy", UNPLANNED).note("An attached `run` always forwards signals (`sbx attach --sig-proxy` has the flag)."),
    not_yet(Run, "--sysctl", UNPLANNED),
    not_yet(Run, "--tmpfs", UNPLANNED),
    not_yet(Run, "--ulimit", UNPLANNED),
    not_yet(Run, "--volumes-from", UNPLANNED),
    wont(Run, "--platform", "Emulating another architecture is out of scope: a sandbox runs the host's architecture.")
        .note("`sbx pull --platform` downloads another architecture's image."),
    wont(Run, "--link", "Legacy in Docker. `--connect SANDBOX:PORT` is sbx's way to reach another sandbox: one port, by name, looked up at each connection.")
        .note("`--link NAME:ALIAS` is `--connect ALIAS=NAME:PORT`, one port at a time."),
    wont(Run, "--network-alias", NO_NETWORKS)
        .note("The connecting side picks the name: `--connect ALIAS=SANDBOX:PORT`."),
    wont(Run, "--volume-driver", "Extensions are out of scope."),
    na(Run, "--privileged", OWN_KERNEL),
    na(Run, "--cap-add, --cap-drop", OWN_KERNEL),
    na(Run, "--security-opt", OWN_KERNEL).note("seccomp, AppArmor and SELinux profiles."),
    na(Run, "--userns", OWN_KERNEL),
    na(Run, "--ipc, --pid, --uts", "Namespace modes. A sandbox has its own kernel, so nothing is shared with the host or another sandbox."),
    na(Run, "--cgroup-parent, --cgroupns", CGROUP),
    na(Run, "-c, --cpu-shares", CGROUP),
    na(Run, "--cpu-period, --cpu-quota, --cpu-rt-period, --cpu-rt-runtime", CGROUP),
    na(Run, "--cpuset-cpus, --cpuset-mems", "Pinning host CPUs and memory nodes is up to the executor."),
    na(Run, "--memory-reservation, --memory-swap, --memory-swappiness", CGROUP),
    na(Run, "--oom-kill-disable, --oom-score-adj", "The host's OOM killer sees the VM, not the processes in it; inside, the guest's own kernel decides."),
    na(Run, "--pids-limit", CGROUP),
    na(Run, "--blkio-weight, --blkio-weight-device", CGROUP),
    na(Run, "--device-read-bps, --device-read-iops, --device-write-bps, --device-write-iops", CGROUP),
    na(Run, "--device, --device-cgroup-rule", "The VM sees only the devices the executor gives it."),
    na(Run, "--ip, --ip6, --link-local-ip, --mac-address", NO_NIC),
    na(Run, "--expose", "Nothing reaches a sandbox except a port published with `-p` or named by another sandbox's `--connect`, so declaring a port changes nothing."),
    na(Run, "--init", "Always on: sbx's guest init is PID 1 and reaps orphans."),
    na(Run, "--annotation", "Annotations go to an OCI runtime; sbx has none."),
    na(Run, "--disable-content-trust", NO_DCT),
    na(Run, "--isolation", WINDOWS_CONTAINERS),
    na(Run, "--cpu-count, --cpu-percent, --io-maxbandwidth, --io-maxiops", WINDOWS_CONTAINERS),
    only(Run, "--rootfs-format", "The root filesystem disk format; `erofs` by default.")
        .why("Different executors and kernels read different formats."),
    only(Run, "--mount type=copy", "Copies a host directory in when the sandbox starts; take results out with `sbx cp`.")
        .why("Works on every executor, including those without live sharing."),
    only(Run, "--allow", "With `--network allow`: a `HOST:PORT` the sandbox may connect to (`*.example.com:443`, `10.0.0.5:*`). A `*` port, an IPv6 address or `localhost` works only for programs that use `HTTP_PROXY`; `run` warns about each."),
    only(Run, "--egress-limit", "With `--network allow` or `--connect`: the most bytes all connections may move in each start, both ways (`--connect` traffic counts for the sandbox that connects)."),
    only(Run, "--connect", "`[NAME=]SANDBOX:PORT`: programs inside reach TCP `PORT` of another running sandbox (same data directory) as `NAME:PORT`, with any protocol, and nothing else opens. The other sandbox needs no option. The target is looked up at each connection, so a restarted one works on the next; a stopped one fails that connection and is recorded in `events`. An MCP sandbox may only connect to MCP sandboxes.")
        .why("Docker networks open every port of every container on the network to each other. `--connect` opens one port, by name, like `--allow`, and the VM still has no network interface."),
    only(Run, "--timeout", "Stops the sandbox this long after each start (like podman's `--timeout`)."),
    // ── exec ─────────────────────────────────────────────────────────────
    same(Exec, "-d, --detach", &["exec_cp_stop_on_a_running_sandbox"]),
    same(Exec, "-e, --env", &["docker_options_reach_the_sandbox"]),
    same(Exec, "--env-file", &["docker_options_reach_the_sandbox"]),
    same(Exec, "-i, --interactive", &["exec_cp_stop_on_a_running_sandbox"]),
    same(Exec, "-t, --tty", &["exec_cp_stop_on_a_running_sandbox"]),
    same(Exec, "-u, --user", &["docker_options_reach_the_sandbox"]),
    same(Exec, "-w, --workdir", &["docker_options_reach_the_sandbox"]),
    not_yet(Exec, "--detach-keys", UNPLANNED),
    na(Exec, "--privileged", OWN_KERNEL),
    only(Exec, "--timeout", "Kills the command after a time and exits with 124, like `timeout(1)`."),
    // ── start ────────────────────────────────────────────────────────────
    same(Start, "-a, --attach", &["create_start_inspect_rm"]),
    same(Start, "-i, --interactive", &["docker_options_reach_the_sandbox"]),
    not_yet(Start, "--detach-keys", UNPLANNED),
    not_yet(Start, "--checkpoint, --checkpoint-dir", LATER).note("Snapshots, when an executor offers them."),
    // ── stop, restart ────────────────────────────────────────────────────
    same(Stop, "-s, --signal", &["stop_sends_the_stop_signal_then_powers_off"]),
    differs(Stop, "-t, --timeout", "Docker's older name. No negative values. When it runs out, the guest is powered off (exit code 137) instead of SIGKILL.", &[
        "exec_cp_stop_on_a_running_sandbox",
        "kill_wait_and_restart",
    ])
    .sbx("-t, --time"),
    // ── kill ─────────────────────────────────────────────────────────────
    same(Kill, "-s, --signal", &["kill_wait_and_restart"]),
    // ── rm ───────────────────────────────────────────────────────────────
    differs(Rm, "-f, --force", "Powers the guest off without a signal.", &[
        "system_df_and_prune_free_what_nothing_uses",
        "docker_options_reach_the_sandbox",
    ]),
    wont(Rm, "-l, --link", "Legacy in Docker, and sbx has no links."),
    na(Rm, "-v, --volumes", "sbx makes no anonymous volumes: a sandbox's writable disk goes with it, and named volumes stay (as in Docker)."),
    // ── ps ───────────────────────────────────────────────────────────────
    same(Ps, "-a, --all", &["names_work_in_place_of_ids"]),
    differs(Ps, "-f, --filter", "Keys `id`, `name`, `label`, `status`, `exited`, `ancestor`. `name` is a substring, not a regular expression. No `before`, `since`, `health` and the others. An unknown key or value is an error (Docker ignores some).", &[
        "filters",
        "filter::tests::bad_filters_are_errors_that_say_what_is_wanted",
    ])
    .why("What is accepted is a table in code (`filter.rs`), without a regular expression engine."),
    differs(Ps, "--format", GO_TEMPLATES, &["json_output_contract"]).sbx("--format"),
    same(Ps, "-q, --quiet", &["names_work_in_place_of_ids"]),
    not_yet(Ps, "-n, --last", UNPLANNED),
    not_yet(Ps, "-l, --latest", UNPLANNED),
    not_yet(Ps, "--no-trunc", UNPLANNED),
    not_yet(Ps, "-s, --size", UNPLANNED),
    // ── inspect ──────────────────────────────────────────────────────────
    differs(Inspect, "-f, --format", GO_TEMPLATES, &["json_output_contract", "jq_shapes_json_output"]).sbx("--format"),
    not_yet(Inspect, "-s, --size", UNPLANNED),
    not_yet(Inspect, "--type", UNPLANNED).note("`inspect` takes sandboxes only."),
    // ── stats ────────────────────────────────────────────────────────────
    differs(Stats, "--format", GO_TEMPLATES, &["stats_reports_guest_usage"]).sbx("--format"),
    same(Stats, "--no-stream", &["stats_reports_guest_usage"]),
    not_yet(Stats, "-a, --all", UNPLANNED),
    not_yet(Stats, "--no-trunc", UNPLANNED),
    // ── attach ───────────────────────────────────────────────────────────
    same(Attach, "--detach-keys", &["attach::tests::detach_keys_parse_like_docker"]),
    same(Attach, "--no-stdin", &["docker_options_reach_the_sandbox"]),
    same(Attach, "--sig-proxy", &["attach_tty_and_signal_proxy"]),
    // ── logs ─────────────────────────────────────────────────────────────
    same(Logs, "-f, --follow", &["detached_rm_and_logs_follow"]),
    same(Logs, "-n, --tail", &["detached_rm_and_logs_follow", "behavior::tests::tail_counts_lines_across_records"]),
    differs(Logs, "-t, --timestamps", "One time per chunk of output the shim received, not per line. Refused for logs written by an `sbx` that kept no times.", &[
        "logs_timestamps_since_until_and_json",
    ])
    .why("The shim writes the chunks it receives as they are (`sbx_shim::log`)."),
    differs(Logs, "--since", "A date without a time zone is refused. Refused for logs without times.", &[
        "logs_timestamps_since_until_and_json",
        "parse::tests::times_take_rfc3339_unix_seconds_and_durations_ago",
    ])
    .why("Whether a line without a time is in the range can't be known, so sbx doesn't guess."),
    differs(Logs, "--until", "As `--since`.", &["logs_timestamps_since_until_and_json"]),
    not_yet(Logs, "--details", UNPLANNED),
    // ── cp ───────────────────────────────────────────────────────────────
    not_yet(Cp, "-a, --archive", UNPLANNED),
    not_yet(Cp, "-L, --follow-link", UNPLANNED),
    not_yet(Cp, "-q, --quiet", UNPLANNED),
    // ── events ───────────────────────────────────────────────────────────
    differs(Events, "-f, --filter", "Keys `type`, `event`, `sandbox`, `image`. No `container` or `label`.", &[
        "filters",
        "events_replay_and_follow",
    ]),
    differs(Events, "--format", GO_TEMPLATES, &["events_replay_and_follow"]).sbx("--format"),
    differs(Events, "--since", "A date without a time zone is refused.", &["events_replay_and_follow"]),
    differs(Events, "--until", "A date without a time zone is refused.", &["events_replay_and_follow"]),
    // ── images ───────────────────────────────────────────────────────────
    differs(Images, "-f, --filter", "Keys `reference` (only `*` and `?` patterns), `dangling`, `label`. No `before` or `since`.", &[
        "filters",
        "filter::tests::globs_match_the_whole_text",
    ]),
    differs(Images, "--format", GO_TEMPLATES, &["json_output_contract"]).sbx("--format"),
    same(Images, "-q, --quiet", &["names_work_in_place_of_ids"]),
    not_yet(Images, "-a, --all", UNPLANNED),
    not_yet(Images, "--digests", UNPLANNED),
    not_yet(Images, "--no-trunc", UNPLANNED),
    // ── pull ─────────────────────────────────────────────────────────────
    differs(Pull, "--platform", "`linux/amd64` or `linux/arm64`; the host's by default. Only the host's architecture runs.", &[
        "parse::tests::platforms",
    ]),
    not_yet(Pull, "-a, --all-tags", UNPLANNED),
    not_yet(Pull, "-q, --quiet", UNPLANNED),
    na(Pull, "--disable-content-trust", NO_DCT),
    // ── push ─────────────────────────────────────────────────────────────
    differs(Push, "-a, --all-tags", "Pushes the local tags of the repository one after another, in name order; one that fails doesn't stop the others.", &[
        "tag_and_push_to_a_registry",
        "app::push::tests::push_names_a_local_tag_or_all_tags_of_a_repository",
    ]),
    differs(Push, "-q, --quiet", "Prints only the pushed reference, one per line.", &["tag_and_push_to_a_registry"]),
    not_yet(Push, "--platform", UNPLANNED)
        .note("sbx keeps one platform per image and always pushes that single-platform manifest."),
    na(Push, "--disable-content-trust", NO_DCT),
    // ── rmi ──────────────────────────────────────────────────────────────
    differs(Rmi, "-f, --force", "Also removes images used by stopped sandboxes in the same data directory, or with several references. Never those of running sandboxes.", &[
        "docker_options_reach_the_sandbox",
        "behavior::tests::rmi_table",
    ])
    .why("sbx can't see sandboxes in another data directory."),
    na(Rmi, "--no-prune", "sbx images have no parent images to keep."),
    // ── save ─────────────────────────────────────────────────────────────
    same(Save, "-o, --output", &["load_and_save_round_trip_an_oci_archive"]),
    not_yet(Save, "--platform", UNPLANNED),
    // ── load ─────────────────────────────────────────────────────────────
    same(Load, "-i, --input", &["load_and_save_round_trip_an_oci_archive"]),
    differs(Load, "--platform", "`linux/amd64` or `linux/arm64`; the host's by default.", &["parse::tests::platforms"]),
    not_yet(Load, "-q, --quiet", UNPLANNED),
    only(Load, "--tag", "Names the image, when the archive holds one."),
    // ── login ────────────────────────────────────────────────────────────
    same(Login, "-u, --username", &["login_and_logout_use_dockers_config"]),
    same(Login, "--password-stdin", &["login_and_logout_use_dockers_config"]),
    wont(Login, "-p, --password", "A password on the command line ends up in shell history and `ps`. Use `--password-stdin`, or let sbx ask."),
    // ── volume create ────────────────────────────────────────────────────
    same(VolumeCreate, "--label", &["volume_data_outlives_the_sandbox"]),
    differs(VolumeCreate, "-o, --opt", "Only `size=` (e.g. `size=50g`, at least 64m). An unknown option is an error.", &[
        "volume_data_outlives_the_sandbox",
        "app::volume::tests::only_the_size_option_is_known",
    ]),
    wont(VolumeCreate, "-d, --driver", "Extensions are out of scope: a volume is a disk of the VM."),
    na(VolumeCreate, "--availability, --group, --limit-bytes, --required-bytes, --scope, --secret, --sharing, --topology-preferred, --topology-required, --type", SWARM),
    // ── volume ls ────────────────────────────────────────────────────────
    differs(VolumeLs, "-f, --filter", "Keys `name` (a substring), `label`, `dangling`. No `driver`.", &[
        "volume_data_outlives_the_sandbox",
    ]),
    differs(VolumeLs, "--format", GO_TEMPLATES, &["volume_data_outlives_the_sandbox"]).sbx("--format"),
    same(VolumeLs, "-q, --quiet", &["volume_data_outlives_the_sandbox"]),
    na(VolumeLs, "--cluster", SWARM),
    // ── volume inspect ───────────────────────────────────────────────────
    differs(VolumeInspect, "-f, --format", GO_TEMPLATES, &["volume_data_outlives_the_sandbox"]).sbx("--format"),
    // ── volume rm ────────────────────────────────────────────────────────
    differs(VolumeRm, "-f, --force", "Only makes a missing volume no error; a volume in use is never removed.", &[
        "volume_data_outlives_the_sandbox",
    ])
    .why("Docker's `-f` doesn't remove a volume in use either."),
    // ── volume prune ─────────────────────────────────────────────────────
    same(VolumePrune, "-a, --all", &["volume_data_outlives_the_sandbox"]),
    same(VolumePrune, "-f, --force", &["volume_data_outlives_the_sandbox"]),
    differs(VolumePrune, "--filter", "Key `label` only.", &["volume_data_outlives_the_sandbox"]),
    // ── system df ────────────────────────────────────────────────────────
    differs(SystemDf, "--format", GO_TEMPLATES, &["system_df_and_prune_free_what_nothing_uses"]).sbx("--format"),
    not_yet(SystemDf, "-v, --verbose", UNPLANNED),
    // ── system prune ─────────────────────────────────────────────────────
    same(SystemPrune, "-a, --all", &["system_df_and_prune_free_what_nothing_uses"]),
    same(SystemPrune, "-f, --force", &["system_df_and_prune_free_what_nothing_uses"]),
    differs(SystemPrune, "--filter", "Keys `until` and `label`, for sandboxes; `label` also for volumes.", &["system_df_and_prune_free_what_nothing_uses"]),
    differs(SystemPrune, "--volumes", "Only with `-a` does it remove anything: every volume no sandbox points at (sbx has no anonymous volumes).", &[
        "system_df_and_prune_free_what_nothing_uses",
    ]),
    // ── builder prune ────────────────────────────────────────────────────
    differs(BuilderPrune, "-a, --all", "Removes every cached step. Layers that images use stay with the images.", &["build_cache_stays_under_its_cap"]),
    same(BuilderPrune, "-f, --force", &["build_cache_stays_under_its_cap"]),
    differs(BuilderPrune, "--keep-storage", "Keeps the most recently used cache that no image uses, up to SIZE. Not with `-a`.", &["build_cache_stays_under_its_cap"]),
    not_yet(BuilderPrune, "--max-used-space, --min-free-space, --reserved-space", UNPLANNED).note("`SBX_BUILD_CACHE_MAX` caps the cache after every build."),
    not_yet(BuilderPrune, "--filter", UNPLANNED),
    not_yet(BuilderPrune, "--verbose", UNPLANNED),
    na(BuilderPrune, "--builder", "There are no builder instances: each build stage gets its own VM."),
    // ── 동작 ─────────────────────────────────────────────────────────────
    same(Behavior, "Exit codes 125, 126 and 127", &[
        "run_failures_before_the_command_are_125",
        "run_command_not_found_is_127_and_not_executable_is_126",
        "behavior::tests::exit_codes_match_docker",
    ]),
    same(Behavior, "Short image names (`alpine` is `docker.io/library/alpine:latest`)", &["json_output_contract"]),
    same(Behavior, "Registry credentials from Docker's `config.json` and credential helpers", &[
        "login_and_logout_use_dockers_config",
    ]),
    differs(Behavior, "Image ID prefixes", "At least 12 characters (Docker takes shorter ones).", &[
        "behavior::tests::ambiguous_id_prefix",
    ])
    .why("A short string that happens to match could remove the wrong image."),
    differs(Behavior, "The main process and PID 1", "The main process is not PID 1 (sbx's guest init is), so it ends on SIGTERM even without a handler.", &[
        "exec_cp_stop_on_a_running_sandbox",
    ])
    .why("PID 1 in the VM is sbx's guest init."),
    differs(Behavior, "`stop` or `rm -f` of a sandbox another terminal runs", "Powers the guest off and waits; an error if it doesn't go off.", &[
        "exec_cp_stop_on_a_running_sandbox",
    ])
    .why("Only the process that started a VM can end it by force."),
    wont(Behavior, "Go templates in `--format` (`--format '{{.ID}}'`)", GO_TEMPLATES),
    wont(Behavior, "The Docker Engine API socket", "Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`."),
    wont(Behavior, "Running another architecture's images (emulation)", "Out of scope: a sandbox runs the host's architecture."),
    // ── build ────────────────────────────────────────────────────────────
    same(Build, "-t, --tag", &["build_runs_stages_and_caches_steps"]),
    same(Build, "-f, --file", &["build_runs_stages_and_caches_steps", "build_source_date_epoch_fixes_the_digest"]).note("`-f -` reads the Dockerfile from stdin; the context is still the directory. `FILE.dockerignore` next to the file is read before the context's `.dockerignore`, as in BuildKit."),
    same(Build, "--build-arg", &["build_failures_targets_and_build_args"]),
    same(Build, "--target", &["build_failures_targets_and_build_args"]),
    same(Build, "--no-cache", &["build_runs_stages_and_caches_steps"]),
    not_yet(Build, "--platform", LATER).note("Images are built for the host's architecture."),
    differs(Build, "--progress", "`auto` (the default) prints one line per step and shows a `RUN` step's output only when it fails; `plain` streams it too. There is no `tty` or `rawjson`; `--format json` prints JSON Lines.", &["build_runs_stages_and_caches_steps"]),
    differs(Build, "-o, --output", "`type=sbx` (sbx's image store, the default; `type=image` too), `type=docker` (into `docker load`, or a tar with `dest=FILE`), `type=oci,dest=FILE|DIR/|-` (an archive, a layout directory, or stdout), `type=registry` (push). Repeat it for several; given outputs replace the default, so `-o type=docker` alone keeps nothing in sbx (the layer cache stays). The archive is the same file `sbx save` writes. No `type=local` or `type=tar`. Layers are turned into tars when exported, and a layer's compressed digest is fixed the first time it is exported, so every export of an image has the same manifest digest.", &["build_outputs_export_one_image_everywhere"]),
    same(Build, "--load", &["build_outputs_export_one_image_everywhere"]).note("Needs `docker` on PATH; without it the build stops before it starts and says to write a tar instead."),
    same(Build, "--push", &["build_outputs_export_one_image_everywhere"]),
    not_yet(Build, "--secret", LATER),
    not_yet(Build, "--ssh", LATER),
    not_yet(Build, "--add-host", LATER),
    differs(Build, "--network", "`none` (the default; Docker's default is the full network) or `allow` with `--allow` destinations, as with `run`. With `none`, a failed step lists the names it looked up and says the network is closed.", &["build_shows_blocked_names"]),
    not_yet(Build, "--annotation", UNPLANNED),
    not_yet(Build, "--attest, --provenance, --sbom", UNPLANNED),
    not_yet(Build, "--build-context", UNPLANNED),
    not_yet(Build, "--cache-from, --cache-to", UNPLANNED),
    not_yet(Build, "--call, --check", UNPLANNED),
    not_yet(Build, "--iidfile", UNPLANNED),
    not_yet(Build, "--label", UNPLANNED),
    not_yet(Build, "--metadata-file", UNPLANNED),
    not_yet(Build, "--no-cache-filter", UNPLANNED),
    not_yet(Build, "--pull", UNPLANNED),
    same(Build, "-q, --quiet", &["build_runs_stages_and_caches_steps"]),
    not_yet(Build, "--shm-size", UNPLANNED),
    not_yet(Build, "--ulimit", UNPLANNED),
    differs(Build, "--allow", "Names a network destination `HOST:PORT` that `RUN` steps may reach, as with `run --allow`. buildx's entitlements (`network.host`, `security.insecure`) mean nothing in a build VM. A failed step lists the names it was refused, with the `--allow` to add.", &["build_shows_blocked_names"]),
    only(Build, "--cpus", "vCPUs of the build VM (default 1)."),
    only(Build, "-m, --memory", "Memory of the build VM (default 512m)."),
    na(Build, "--builder", "There are no builder instances: each build stage gets its own VM."),
    na(Build, "--cgroup-parent", CGROUP),
    // ── Dockerfile ───────────────────────────────────────────────────────
    differs(Dockerfile, "`FROM`", "With `AS name`, `scratch`, another stage, and `ARG` before `FROM`; `--platform` only for the host's platform. A base image with `ONBUILD` triggers is refused.", &["build_runs_stages_and_caches_steps", "build_scratch_without_a_vm_and_shared_base_layers"]),
    differs(Dockerfile, "`RUN`", "Shell and exec forms, in the stage's VM; the network is closed unless `--allow` opens it. Processes a step leaves running are ended when it finishes (BuildKit ends them with the step's container).", &["build_failures_targets_and_build_args"]),
    not_yet(Dockerfile, "`RUN --mount=type=cache`", LATER).note("On top of named volumes."),
    not_yet(Dockerfile, "`RUN --mount=type=secret`", LATER),
    not_yet(Dockerfile, "`RUN --mount=type=ssh`", LATER),
    not_yet(Dockerfile, "`RUN --mount=type=bind`, `type=tmpfs`", UNPLANNED),
    not_yet(Dockerfile, "`RUN --network`", UNPLANNED),
    na(Dockerfile, "`RUN --security=insecure`", "A build step already runs as root in its own VM; there is no container sandbox to loosen."),
    same(Dockerfile, "`COPY`", &["build_runs_stages_and_caches_steps"]),
    same(Dockerfile, "`COPY --from`", &["build_runs_stages_and_caches_steps"]),
    same(Dockerfile, "`COPY --chown`, `--chmod`", &["build_writes_the_image_config"]),
    not_yet(Dockerfile, "`COPY --link`", LATER),
    not_yet(Dockerfile, "`ADD` (local files, local tar archives)", LATER).note("`ADD` of a local file that is not an archive copies it like `COPY`. A tar or compressed file, which Docker would extract, is refused for now."),
    not_yet(Dockerfile, "`ADD <URL>`", LATER).note("Through the same allow list, since the host downloads it."),
    not_yet(Dockerfile, "`ADD <git repository>`", LATER),
    same(Dockerfile, "`ENV`", &["build_runs_stages_and_caches_steps"]),
    differs(Dockerfile, "`ARG`", "As in BuildKit, except that a `--build-arg` nothing declares is reported, and `--build-arg` cannot set the platform arguments (`TARGETARCH`).", &["build_failures_targets_and_build_args"]),
    same(Dockerfile, "`WORKDIR`", &["build_runs_stages_and_caches_steps"]),
    same(Dockerfile, "`USER`", &["build_writes_the_image_config"]),
    same(Dockerfile, "`LABEL`", &["build_writes_the_image_config"]),
    same(Dockerfile, "`EXPOSE`", &["build_writes_the_image_config"]),
    same(Dockerfile, "`VOLUME`", &["build_writes_the_image_config"]),
    same(Dockerfile, "`ENTRYPOINT`", &["build_writes_the_image_config"]),
    same(Dockerfile, "`CMD`", &["build_runs_stages_and_caches_steps"]),
    same(Dockerfile, "`STOPSIGNAL`", &["build_writes_the_image_config"]),
    same(Dockerfile, "`SHELL`", &["build_writes_the_image_config"]),
    differs(Dockerfile, "`HEALTHCHECK`", "Written to the image config; sbx says it doesn't run health checks.", &["build_writes_the_image_config"]),
    not_yet(Dockerfile, "`ONBUILD`", LATER).note("An `ONBUILD` line is an error, and so is a base image with `ONBUILD` triggers."),
    differs(Dockerfile, "`MAINTAINER`", "Deprecated in Docker. Written to the image's `author`, with a warning.", &["build_writes_the_image_config"]),
    not_yet(Dockerfile, "Here-documents (`RUN <<EOF`)", LATER),
    same(Dockerfile, "Parser directive `# escape=`", &["build_writes_the_image_config"]),
    wont(Dockerfile, "Parser directive `# syntax=` (another frontend)", "Another frontend needs BuildKit. sbx reports the line instead of ignoring it."),
    same(Dockerfile, "`.dockerignore`", &["build_runs_stages_and_caches_steps"]),
    not_yet(Dockerfile, "A git repository as the build context", LATER),
    differs(Dockerfile, "`SOURCE_DATE_EPOCH` (fixed times in layers)", "From the environment variable or `--build-arg SOURCE_DATE_EPOCH=N`, as in BuildKit: it sets the image's `created` and history times and caps the file times in the layers a build makes (so in exported layer tars too). Layers of the base image keep their times. The promise is the same input and the same cache give the same digest.", &["build_source_date_epoch_fixes_the_digest"]),
    // ── compose ──────────────────────────────────────────────────────────
    not_yet(Compose, "up", "C1"),
    not_yet(Compose, "down", "C1"),
    not_yet(Compose, "ps", "C1"),
    not_yet(Compose, "logs", "C1"),
    not_yet(Compose, "build", "C1"),
    not_yet(Compose, "config", "C1").note("Also shows which ports each service may reach."),
    not_yet(Compose, "create", LATER),
    not_yet(Compose, "start", LATER),
    not_yet(Compose, "stop", LATER),
    not_yet(Compose, "restart", LATER),
    not_yet(Compose, "rm", LATER),
    not_yet(Compose, "kill", LATER),
    not_yet(Compose, "exec", LATER),
    not_yet(Compose, "run", LATER),
    not_yet(Compose, "pull", LATER),
    not_yet(Compose, "push", LATER),
    not_yet(Compose, "images", LATER),
    not_yet(Compose, "events", LATER),
    not_yet(Compose, "top", LATER),
    not_yet(Compose, "port", LATER),
    not_yet(Compose, "pause", LATER),
    not_yet(Compose, "unpause", LATER),
    not_yet(Compose, "ls", LATER),
    not_yet(Compose, "cp", LATER),
    not_yet(Compose, "scale", LATER),
    not_yet(Compose, "wait", LATER),
    not_yet(Compose, "watch", LATER),
    not_yet(Compose, "version", LATER),
    // ── compose.yaml ─────────────────────────────────────────────────────
    not_yet(ComposeFile, "`services`", "C1"),
    not_yet(ComposeFile, "`volumes` (top level)", "C1"),
    wont(ComposeFile, "`networks` (top level)", NO_NETWORKS)
        .note("Services reach each other through `--connect`, made from their ports."),
    not_yet(ComposeFile, "`configs`, `secrets` (top level)", LATER),
    not_yet(ComposeFile, "`name`", LATER),
    not_yet(ComposeFile, "`include`", LATER),
    not_yet(ComposeFile, "`version`", LATER),
    not_yet(ComposeFile, "Extension fields (`x-…`)", LATER),
    not_yet(ComposeFile, "`services.*.image`", "C1"),
    not_yet(ComposeFile, "`services.*.build`", "C1"),
    not_yet(ComposeFile, "`services.*.command`", "C1"),
    not_yet(ComposeFile, "`services.*.entrypoint`", "C1"),
    not_yet(ComposeFile, "`services.*.environment`", "C1"),
    not_yet(ComposeFile, "`services.*.env_file`", "C1"),
    not_yet(ComposeFile, "`services.*.ports`", "C1"),
    not_yet(ComposeFile, "`services.*.expose`", "C1").note("Used to open `--connect` between services."),
    not_yet(ComposeFile, "`services.*.volumes`", "C1").note("Named volumes and host paths."),
    not_yet(ComposeFile, "`services.*.depends_on`", "C1").note("Start order only."),
    not_yet(ComposeFile, "`services.*.container_name`", LATER),
    not_yet(ComposeFile, "`services.*.hostname`", LATER),
    not_yet(ComposeFile, "`services.*.working_dir`", LATER),
    not_yet(ComposeFile, "`services.*.user`", LATER),
    not_yet(ComposeFile, "`services.*.labels`", LATER),
    not_yet(ComposeFile, "`services.*.stdin_open`, `tty`", LATER),
    not_yet(ComposeFile, "`services.*.stop_signal`, `stop_grace_period`", LATER),
    not_yet(ComposeFile, "`services.*.cpus`, `mem_limit`, `deploy.resources`", LATER),
    not_yet(ComposeFile, "`services.*.restart`", LATER),
    not_yet(ComposeFile, "`services.*.healthcheck`", LATER),
    not_yet(ComposeFile, "`services.*.profiles`", LATER),
    not_yet(ComposeFile, "`services.*.extends`", LATER),
    not_yet(ComposeFile, "`services.*.secrets`, `configs`", LATER),
    not_yet(ComposeFile, "`services.*.tmpfs`", LATER),
    not_yet(ComposeFile, "`services.*.extra_hosts`, `dns`", LATER),
    not_yet(ComposeFile, "`services.*.develop.watch`", LATER),
    wont(ComposeFile, "`services.*.networks`, `network_mode`", NO_NETWORKS),
    wont(ComposeFile, "`services.*.platform`", "Out of scope: a sandbox runs the host's architecture."),
    not_yet(ComposeFile, "`services.*.logging`", UNPLANNED).note("The shim writes each detached sandbox's log."),
    na(ComposeFile, "`services.*.privileged`, `cap_add`, `cap_drop`, `security_opt`", OWN_KERNEL),
    na(ComposeFile, "`services.*.devices`", "The VM sees only the devices the executor gives it."),
    na(ComposeFile, "`services.*.ipc`, `pid`, `userns_mode`", OWN_KERNEL),
    na(ComposeFile, "`services.*.init`", "Always on: sbx's guest init is PID 1."),
];

#[cfg(test)]
mod tests;
