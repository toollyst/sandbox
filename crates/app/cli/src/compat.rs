//! 호환 표. sbx가 Docker, Podman, nerdctl과 그 제품들의 무엇을 어떻게 하는지의 정본이다.
//!
//! - 견주는 대상([`Target`])은 사람들이 실제로 까는 것이다: Docker CLI(Docker Desktop이 싣는 것), Podman, nerdctl,
//!   Dockerfile, Compose, 그리고 Docker Desktop과 그것을 대신하는 제품(Podman Desktop, OrbStack, Colima, Rancher
//!   Desktop)의 제품 기능. OrbStack·Colima·Rancher Desktop의 CLI는 Docker(나 nerdctl)의 것이므로 제품 기능에만 나온다.
//! - 명령과 옵션, Dockerfile 명령, Compose 키, 제품 기능마다 [`Row`] 한 줄이다. 상태([`Status`]), 다른 점과 그 이유,
//!   되는 줄이면 그것을 보이는 테스트 이름, 아직 없는 줄이면 그것을 더할 로드맵 단계를 적는다.
//! - CLI 줄은 그것을 가진 도구([`Tools`])를 적는다. 여러 도구에 같은 이름으로 있는 것은 한 줄이다. 한 도구에만 있는
//!   명령은 그 도구의 절([`Area::PodmanCommands`], [`Area::NerdctlCommands`])에, 옵션은 그 명령의 절에 둔다.
//! - 제품 기능 줄([`Area::Features`])은 제품마다 있는지([`Offer`])와 그 출처(공식 문서의 주소)를 적는다. 공식 문서로
//!   확인하지 못한 것은 적지 않는다. 라이선스와 지원 OS는 할 수 있는 일이 아니므로 [`FACTS`]에 따로 둔다.
//! - 다른 점은 모두 여기 있다. 따로 적는 목록은 없다.
//! - 상태는 지금 코드가 하는 것이다. 기획서가 약속하는 것이 아니다.
//! - 문서 `docs/guide/tool-compatibility.md`와 README의 진행률 한 줄은 여기서 만든다. 테스트가 둘이 지금 표와 같은지
//!   확인하고, `SBX_BLESS=1 cargo test -p sbx-cli --lib compat`이 다시 쓴다. 최상위 `COMPATIBILITY.md`는 sbx 판 사이의
//!   약속이고 이 표와는 다른 것이다.
//! - 테스트가 표를 clap 정의와 맞춰 본다. 되는 줄(🟢, 🟡, ➕)의 명령과 옵션은 clap에 있어야 하고 근거 테스트가 실제로
//!   있어야 한다. 되지 않는 줄(🔴, ⚫, ⚪)의 명령과 옵션은 clap에 없어야 한다. 어느 도구의 줄이든 같다. clap의 모든
//!   명령과 옵션에는 줄이 있어야 한다. 그래서 기능을 넣거나 빼면 표를 고치지 않고는 CI가 통과하지 않는다.
//! - 줄의 글은 영어다. 사용자가 읽는 문서가 되기 때문이다.
//!
//! [`Row::name`]의 모양은 절([`Area`])의 종류([`Kind`])가 정한다.
//!
//! - [`Kind::Commands`]: 명령 이름(`run`, `system df`). 루트 옵션이면 그 옵션(`--version`).
//! - [`Kind::Flags`]: 옵션 철자를 `, `로 잇는다(`-m, --memory`). 뒤에 공백과 값 이야기가 올 수 있다
//!   (`-v, --volume NAME:/path`). 값 이야기가 있는 줄은 옵션은 있되 그 값이 되는지를 말하므로, 되지 않는 줄이어도
//!   clap에 옵션이 없는지는 보지 않는다.
//! - [`Kind::Text`]: 아무 글. clap과 맞춰 보지 않는다(Dockerfile, Compose 파일, 동작, 제품 기능).
//!
//! 이름은 그 도구의 이름이다. sbx의 이름이 다르면 [`Row`]의 `sbx`에 sbx의 이름을 같은 모양으로 적는다. ➕ 줄은
//! `name`에 sbx의 이름을 적는다.

/// 줄의 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// 그것을 가진 도구와 같다.
    Same,
    /// 되지만 다르다. 다른 점은 `note`에 있다.
    Differs,
    /// 아직 없다. 더할 단계는 `step`에 있다.
    NotYet,
    /// 하지 않는다. 이유는 `why`에 있다.
    WontDo,
    /// 샌드박스마다 VM이고 데몬이 없는 sbx에서는 의미가 없다. 이유는 `why`에 있다.
    NotApplicable,
    /// 견주는 어느 도구에도 없는 sbx만의 것.
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
            Status::Same => "Same",
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

/// CLI 줄의 명령이나 옵션을 가진 도구들.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tools(u8);

impl Tools {
    /// 어느 도구에도 없다(➕, 그리고 CLI가 아닌 절).
    pub const NONE: Tools = Tools(0);
    /// Docker CLI(Docker Desktop이 싣는 것).
    pub const DOCKER: Tools = Tools(1);
    /// Podman.
    pub const PODMAN: Tools = Tools(2);
    /// nerdctl.
    pub const NERDCTL: Tools = Tools(4);
    /// 셋 다.
    pub const ALL: Tools = Tools(7);
    /// 문서의 순서와 이름.
    pub const EACH: [(Tools, &'static str); 3] = [
        (Tools::DOCKER, "Docker"),
        (Tools::PODMAN, "Podman"),
        (Tools::NERDCTL, "nerdctl"),
    ];

    /// 둘을 합친다.
    pub const fn and(self, other: Tools) -> Tools {
        Tools(self.0 | other.0)
    }

    /// `other`를 뺀다.
    pub const fn without(self, other: Tools) -> Tools {
        Tools(self.0 & !other.0)
    }

    /// `other`를 모두 가졌는지.
    pub const fn has(self, other: Tools) -> bool {
        self.0 & other.0 == other.0 && other.0 != 0
    }

    /// 아무것도 없는지.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// 진행률을 따로 세는 대상.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Docker CLI의 명령과 옵션(`build` 포함).
    DockerCli,
    /// Podman의 명령과 옵션.
    Podman,
    /// nerdctl의 명령과 옵션.
    Nerdctl,
    /// Dockerfile.
    Dockerfile,
    /// `docker compose`와 `compose.yaml`.
    Compose,
    /// Docker Desktop과 그것을 대신하는 제품의 제품 기능.
    Products,
}

impl Target {
    /// 문서와 README의 순서.
    pub const ALL: [Target; 6] = [
        Target::DockerCli,
        Target::Podman,
        Target::Nerdctl,
        Target::Dockerfile,
        Target::Compose,
        Target::Products,
    ];

    /// 문서의 이름.
    pub const fn title(self) -> &'static str {
        match self {
            Target::DockerCli => "Docker CLI",
            Target::Podman => "Podman",
            Target::Nerdctl => "nerdctl",
            Target::Dockerfile => "Dockerfile",
            Target::Compose => "Compose",
            Target::Products => "Product features",
        }
    }

    /// 줄이 이 대상에 드는지.
    pub const fn has(self, r: &Row) -> bool {
        match self {
            Target::DockerCli => r.area.is_cli() && r.tools.has(Tools::DOCKER),
            Target::Podman => r.area.is_cli() && r.tools.has(Tools::PODMAN),
            Target::Nerdctl => r.area.is_cli() && r.tools.has(Tools::NERDCTL),
            Target::Dockerfile => matches!(r.area, Area::Dockerfile),
            Target::Compose => matches!(r.area, Area::Compose | Area::ComposeFile),
            Target::Products => matches!(r.area, Area::Features),
        }
    }
}

/// 제품 기능 절에서 견주는 제품.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Product {
    /// Docker Desktop.
    DockerDesktop,
    /// Podman Desktop(과 그것이 쓰는 `podman machine`).
    PodmanDesktop,
    /// OrbStack.
    OrbStack,
    /// Colima.
    Colima,
    /// Rancher Desktop.
    RancherDesktop,
}

impl Product {
    /// 문서의 열 순서.
    pub const ALL: [Product; 5] = [
        Product::DockerDesktop,
        Product::PodmanDesktop,
        Product::OrbStack,
        Product::Colima,
        Product::RancherDesktop,
    ];

    /// 문서의 이름.
    pub const fn title(self) -> &'static str {
        match self {
            Product::DockerDesktop => "Docker Desktop",
            Product::PodmanDesktop => "Podman Desktop",
            Product::OrbStack => "OrbStack",
            Product::Colima => "Colima",
            Product::RancherDesktop => "Rancher Desktop",
        }
    }
}

/// 공식 문서가 그 기능에 대해 말하는 것.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Has {
    /// 있다고 말한다.
    Yes,
    /// 없다고 말한다. `detail`은 그것을 말하는 문서의 글을 따옴표로 그대로 옮긴 것이다(추론한 "없다"는 쓰지 않는다).
    No,
    /// 있다 없다 대신 다른 것을 말한다(`detail`이 그것이다). 예: "`brew upgrade colima`로 올린다".
    Says,
}

/// 제품 하나가 그 기능에 대해 공식 문서(공개 저장소면 그 README와 문서)에서 말하는 것. 문서가 말하지 않는 것은 적지
/// 않는다(칸을 비운다).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    /// 제품.
    pub product: Product,
    /// 문서가 말하는 것.
    pub has: Has,
    /// 짧은 설명. [`Has::Yes`]면 없어도 되고, 그 밖에는 있어야 한다.
    pub detail: &'static str,
    /// 그것을 말하는 공식 문서의 주소.
    pub source: &'static str,
}

const fn yes(product: Product, detail: &'static str, source: &'static str) -> Offer {
    Offer {
        product,
        has: Has::Yes,
        detail,
        source,
    }
}
const fn says(product: Product, detail: &'static str, source: &'static str) -> Offer {
    Offer {
        product,
        has: Has::Says,
        detail,
        source,
    }
}

/// 다른 도구와 제품을 언제 무엇과 견주었는지. 문서에 그대로 나간다.
pub const CHECKED: &str = "Checked on 2026-11-17 against the Docker CLI 29.9.3 and Compose 5.7.0 (as installed with \
     Docker Desktop), Podman 6.2.0 (its [man pages](https://github.com/containers/podman/tree/v6.2.0/docs/source/markdown)), \
     nerdctl 2.4.2 (its [command reference](https://github.com/containerd/nerdctl/blob/v2.4.2/docs/command-reference.md), \
     which says that Docker flags it doesn't list are not implemented), and the products' own documentation as linked \
     in each cell.";

/// 줄의 `name`이 무엇을 가리키는지.
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
    PodmanCommands,
    NerdctlCommands,
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
    Features,
}

impl Area {
    /// 문서의 순서.
    pub const ALL: [Area; 38] = [
        Area::Commands,
        Area::PodmanCommands,
        Area::NerdctlCommands,
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
        Area::Features,
    ];

    /// 문서의 절 제목.
    pub const fn title(self) -> &'static str {
        match self {
            Area::Commands => "Commands",
            Area::PodmanCommands => "Podman-only commands",
            Area::NerdctlCommands => "nerdctl-only commands",
            Area::Global => "Global options",
            Area::Run => "run, create",
            Area::Exec => "exec",
            Area::Start => "start",
            Area::Stop => "stop, restart",
            Area::Kill => "kill",
            Area::Rm => "rm",
            Area::Ps => "ps",
            Area::Inspect => "inspect",
            Area::Stats => "stats",
            Area::Attach => "attach",
            Area::Logs => "logs",
            Area::Cp => "cp",
            Area::Events => "events",
            Area::Images => "images",
            Area::Pull => "pull",
            Area::Push => "push",
            Area::Rmi => "rmi",
            Area::Save => "save",
            Area::Load => "load",
            Area::Login => "login",
            Area::VolumeCreate => "volume create",
            Area::VolumeLs => "volume ls",
            Area::VolumeInspect => "volume inspect",
            Area::VolumeRm => "volume rm",
            Area::VolumePrune => "volume prune",
            Area::SystemDf => "system df",
            Area::SystemPrune => "system prune",
            Area::BuilderPrune => "builder prune",
            Area::Behavior => "Behavior",
            Area::Build => "build",
            Area::Dockerfile => "Dockerfile",
            Area::Compose => "docker compose",
            Area::ComposeFile => "compose.yaml",
            Area::Features => "Product features",
        }
    }

    /// 줄의 `name`이 무엇을 가리키는지.
    pub const fn kind(self) -> Kind {
        match self {
            Area::Commands | Area::PodmanCommands | Area::NerdctlCommands => {
                Kind::Commands { prefix: &[] }
            }
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
            Area::Behavior | Area::Dockerfile | Area::ComposeFile | Area::Features => Kind::Text,
        }
    }

    /// 명령줄 도구(Docker CLI, Podman, nerdctl)의 절인지. Dockerfile, Compose, 제품 기능은 따로 센다.
    pub const fn is_cli(self) -> bool {
        !matches!(
            self,
            Area::Dockerfile | Area::Compose | Area::ComposeFile | Area::Features
        )
    }

    /// 줄의 도구가 따로 적혀 있지 않을 때의 도구. CLI 절은 셋 다, 한 도구의 절은 그 도구, 동작은 Docker다. 나머지는
    /// 도구가 아니라 절이 대상을 정한다([`Target::has`]).
    pub const fn tools(self) -> Tools {
        match self {
            Area::PodmanCommands => Tools::PODMAN,
            Area::NerdctlCommands => Tools::NERDCTL,
            Area::Behavior => Tools::DOCKER,
            a if a.is_cli() => Tools::ALL,
            _ => Tools::NONE,
        }
    }

    /// 절 제목 아래의 한두 문장.
    pub const fn intro(self) -> &'static str {
        match self {
            Area::Commands => {
                "Every `docker` command and management command; the *In* column says which of Podman and nerdctl \
                 have it under the same name. The flags of the commands sbx has are in the sections below."
            }
            Area::PodmanCommands => {
                "Podman commands that Docker doesn't have. Podman's versions of Docker's commands are in the \
                 section above."
            }
            Area::NerdctlCommands => {
                "nerdctl commands that Docker doesn't have (marked :nerd_face: in its command reference)."
            }
            Area::Global => "Options that go before the command (`docker --debug ps`).",
            Area::Run => {
                "`create` takes the same flags as `run`, except `-d`. Many of these flags tune namespaces and \
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
                 `sbx` commands. Podman (`podman compose`, which calls an external provider) and nerdctl \
                 (`nerdctl compose`) read the same file."
            }
            Area::ComposeFile => "Keys `sbx compose` will read. An unknown key is an error.",
            Area::VolumeCreate => {
                "A volume is a sparse disk of its own, formatted (ext4 with a journal) by the guest the first time a \
                 sandbox attaches it. Only the `local` kind exists."
            }
            Area::Features => {
                "What people install Docker Desktop or one of its alternatives for, beyond the command line. Each \
                 product's cell links the official page it comes from. *Yes* means the page says the product has \
                 it, and *No* is used only where the page says it doesn't (in its own words). A cell without Yes \
                 or No gives what the page says instead, and an empty cell means its documentation doesn't say, \
                 not that it lacks the feature. The *sbx* column uses the same legend as the rest of the page."
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
    /// 도구의 이름. 모양은 [`Area::kind`]가 정한다(모듈 문서).
    pub name: &'static str,
    /// sbx의 이름이 다를 때 그 이름(같은 모양).
    pub sbx: Option<&'static str>,
    /// 그것을 가진 도구(CLI 절). ➕와 CLI가 아닌 절에는 없다.
    pub tools: Tools,
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
    /// 제품 기능 줄([`Area::Features`])에서 제품마다 가졌는지.
    pub offers: &'static [Offer],
}

/// 0.3 뒤에 하기로 한 것(로드맵 "그 뒤").
pub const LATER: &str = "later";
/// 아직 정하지 않았다.
pub const UNPLANNED: &str = "unplanned";

/// 0.3(로드맵 M9)의 단계. 🔴의 `step`은 이것이거나 `C1`(compose, 0.3 뒤), [`LATER`], [`UNPLANNED`]다.
pub const STEPS_0_3: &[&str] = &[
    "P1", "D1", "D2", "S1", "D3", "N1", "V2", "D4", "D5", "V1", "V4", "V3", "D6", "T4",
];

const fn row(area: Area, name: &'static str, status: Status) -> Row {
    Row {
        area,
        name,
        sbx: None,
        tools: area.tools(),
        status,
        note: "",
        why: "",
        evidence: &[],
        step: "",
        offers: &[],
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
    /// 이 도구들만 가졌다.
    const fn only_in(self, tools: Tools) -> Self {
        Self { tools, ..self }
    }
    /// 이 도구들은 갖지 않았다.
    const fn lacks(self, tools: Tools) -> Self {
        Self {
            tools: self.tools.without(tools),
            ..self
        }
    }
    const fn offers(self, offers: &'static [Offer]) -> Self {
        Self { offers, ..self }
    }
}

const fn same(area: Area, name: &'static str, evidence: &'static [&'static str]) -> Row {
    row(area, name, Status::Same).by(evidence)
}
const fn differs(
    area: Area,
    name: &'static str,
    note: &'static str,
    evidence: &'static [&'static str],
) -> Row {
    row(area, name, Status::Differs).note(note).by(evidence)
}
const fn not_yet(area: Area, name: &'static str, step: &'static str) -> Row {
    row(area, name, Status::NotYet).step(step)
}
const fn wont(area: Area, name: &'static str, why: &'static str) -> Row {
    row(area, name, Status::WontDo).why(why)
}
const fn na(area: Area, name: &'static str, why: &'static str) -> Row {
    row(area, name, Status::NotApplicable).why(why)
}
const fn only(area: Area, sbx: &'static str, note: &'static str) -> Row {
    row(area, sbx, Status::SbxOnly)
        .note(note)
        .only_in(Tools::NONE)
}

use Area::*;
use Product::*;

const D: Tools = Tools::DOCKER;
const P: Tools = Tools::PODMAN;
const N: Tools = Tools::NERDCTL;

// 자주 쓰는 이유.
const NO_DAEMON: &str = "There is no daemon: each `sbx` command reads and writes the data and cache directories itself.";
const OWN_KERNEL: &str = "It hardens a kernel shared with the host. Each sandbox has its own kernel behind the VM, \
                          and root in the guest owns only the guest.";
const USERNS: &str = "A user namespace maps users of a kernel shared with the host. A sandbox has its own kernel \
                      and its own users.";
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
const NO_API: &str =
    "Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`.";
const NO_EMULATION: &str = "Out of scope: a sandbox runs the host's architecture.";
const NO_EXTENSIONS: &str = "Extensions are out of scope.";
const NO_MACHINE: &str = "Each sandbox is its own VM, so there is no shared Linux VM to set up or connect to. \
                          `sbx executor` installs what runs the VMs.";
const NO_FD: &str = "A file descriptor of the host can't be handed to a process in another kernel.";
const NO_BUILDER: &str = "There are no builder instances: each build stage gets its own VM.";
const POLICY_JSON: &str =
    "sbx follows the containers `policy.json`, which can require cosign signatures, on every pull.";

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
    not_yet(Commands, "system events", UNPLANNED).lacks(N).note("`sbx events` does this."),
    not_yet(Commands, "system info", UNPLANNED).lacks(N).note("`sbx info` does this."),
    differs(Commands, "build", "Each stage's `RUN` steps run in one microVM with the network closed unless `--allow` opens it; `COPY`, `ADD` and `WORKDIR` layers are made on the host, so a `FROM scratch` stage of copies needs no VM. The image goes to the sbx image store unless `--output` says otherwise; `save` and `push` export a built image like any other.", &["build_runs_stages_and_caches_steps", "build_scratch_without_a_vm_and_shared_base_layers", "build_outputs_export_one_image_everywhere"]),
    wont(Commands, "buildx build", "`sbx build` is the builder and takes buildx's flag names; there is no builder plugin to call.").lacks(N),
    differs(Commands, "builder", "`prune` only. The build cache also keeps itself under a cap after every build (`SBX_BUILD_CACHE_MAX`, default 10 GiB), dropping the least recently used steps whose layers no image uses. `sbx system df` shows it; `sbx system prune` clears it.", &["build_cache_stays_under_its_cap"]).lacks(P),
    differs(Commands, "builder prune", "Removes cached steps whose layers no image uses (`-a`: every cached step; `--keep-storage`: keeps the most recently used up to a size). Without a terminal it needs `-f`.", &["build_cache_stays_under_its_cap"]).lacks(P),
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
    not_yet(Commands, "volume update", UNPLANNED).only_in(D).note("Docker's only takes cluster volumes."),
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
    not_yet(Commands, "checkpoint", LATER).lacks(P).note("Snapshots, when an executor offers them."),
    not_yet(Commands, "search", UNPLANNED),
    not_yet(Commands, "export", UNPLANNED),
    not_yet(Commands, "import", UNPLANNED),
    not_yet(Commands, "manifest", UNPLANNED),
    not_yet(Commands, "container", UNPLANNED).note("The same commands exist at the top level (`sbx ps`, `sbx rm`)."),
    not_yet(Commands, "image", UNPLANNED).note("The same commands exist at the top level (`sbx images`, `sbx rmi`)."),
    wont(Commands, "network", NO_NETWORKS),
    na(Commands, "context", NO_DAEMON).only_in(D).note("`--executor` and `sbx executor use` pick what runs the VMs."),
    wont(Commands, "plugin", "Extensions are out of scope.").only_in(D),
    na(Commands, "trust", NO_DCT).only_in(D),
    wont(Commands, "swarm", SWARM).only_in(D),
    wont(Commands, "node", SWARM).only_in(D),
    wont(Commands, "service", SWARM).only_in(D),
    wont(Commands, "stack", SWARM).only_in(D),
    wont(Commands, "secret", SWARM).only_in(D),
    wont(Commands, "config", SWARM).only_in(D),
    only(Commands, "prepare", "Builds an image's layer disks ahead of time, one per layer and shared between images, for the executor `run` would use (`run` does it on first use)."),
    only(Commands, "tui", "Sandboxes, images, volumes, events and executors on one screen, like Docker Desktop or lazydocker. Every key runs an `sbx` command and shows its command line."),
    only(Commands, "mcp", "Serves sandboxes to AI agents over the Model Context Protocol ([guide](mcp.md)). Docker Desktop's `docker mcp` plugin is a different tool."),
    only(Commands, "executor", "Installs and picks executors, the programs that run the VMs ([guide](executors.md))."),
    only(Commands, "upgrade", "Replaces sbx with the newest signed release."),
    differs(Commands, "completion", "`sbx completion SHELL` prints a script that calls sbx back: on each TAB the shell runs `COMPLETE=<shell> sbx -- <words>`, which completes sandbox names and IDs, image references and executors, and changes nothing. So with `COMPLETE` set (not empty, not 0), sbx only completes.", &[
        "completion_offers_sandboxes_and_images",
    ]),
    not_yet(Commands, "bake", UNPLANNED).only_in(D).note("`docker buildx bake`: builds from a file of targets."),
    // ── Podman에만 있는 명령 ──────────────────────────────────────────────
    not_yet(PodmanCommands, "pod", UNPLANNED).note("Containers that share namespaces. `--connect` links sandboxes one port at a time."),
    na(PodmanCommands, "machine", NO_MACHINE).note("Manages the Linux VM Podman runs containers in on macOS and Windows."),
    not_yet(PodmanCommands, "kube", UNPLANNED).note("`play`, `generate`, `down`, `apply`: Kubernetes YAML to containers and pods, and back."),
    not_yet(PodmanCommands, "generate", UNPLANNED).note("`systemd`, `spec`."),
    not_yet(PodmanCommands, "quadlet", UNPLANNED).note("Installs and lists Quadlet files, which run containers as systemd services."),
    not_yet(PodmanCommands, "auto-update", UNPLANNED),
    not_yet(PodmanCommands, "healthcheck", LATER).note("`healthcheck run` runs a container's health check now."),
    not_yet(PodmanCommands, "secret", UNPLANNED).note("Podman's secrets are local (for `run --secret`); Docker's `secret` is a Swarm command."),
    not_yet(PodmanCommands, "artifact", UNPLANNED).note("OCI artifacts in local storage."),
    not_yet(PodmanCommands, "farm", UNPLANNED).note("Builds on several machines at once."),
    not_yet(PodmanCommands, "init", UNPLANNED),
    not_yet(PodmanCommands, "untag", UNPLANNED),
    not_yet(PodmanCommands, "mount", UNPLANNED),
    not_yet(PodmanCommands, "unmount", UNPLANNED),
    na(PodmanCommands, "unshare", USERNS).note("Runs a command in Podman's rootless user namespace on the host."),
    na(PodmanCommands, "system connection", NO_DAEMON).note("Connections to Podman services, such as the one in `podman machine`."),
    na(PodmanCommands, "system service", NO_DAEMON).note("Serves Podman's API and a Docker-compatible API on a socket."),
    not_yet(PodmanCommands, "system reset", UNPLANNED),
    not_yet(PodmanCommands, "system check", UNPLANNED),
    not_yet(PodmanCommands, "system migrate", UNPLANNED),
    not_yet(PodmanCommands, "system renumber", UNPLANNED),
    not_yet(PodmanCommands, "volume export", UNPLANNED),
    not_yet(PodmanCommands, "volume import", UNPLANNED),
    not_yet(PodmanCommands, "volume exists", UNPLANNED),
    not_yet(PodmanCommands, "volume mount", UNPLANNED),
    not_yet(PodmanCommands, "volume unmount", UNPLANNED),
    not_yet(PodmanCommands, "volume rename", UNPLANNED),
    wont(PodmanCommands, "volume reload", NO_EXTENSIONS).note("Reloads volumes from volume plugins."),
    // ── nerdctl에만 있는 명령 ─────────────────────────────────────────────
    not_yet(NerdctlCommands, "namespace", UNPLANNED).note("containerd namespaces: separate sets of containers and images. sbx's `--data-dir` and `--cache-dir` keep separate sets."),
    not_yet(NerdctlCommands, "image convert", UNPLANNED).note("To eStargz, zstd:chunked, SOCI, overlaybd or EROFS layers. sbx makes its own EROFS layer disks (`prepare`)."),
    not_yet(NerdctlCommands, "image encrypt", UNPLANNED),
    not_yet(NerdctlCommands, "image decrypt", UNPLANNED),
    na(NerdctlCommands, "apparmor", OWN_KERNEL),
    not_yet(NerdctlCommands, "builder debug", UNPLANNED),
    not_yet(NerdctlCommands, "ipfs", UNPLANNED).note("`ipfs registry serve`: a registry backed by IPFS."),
    // ── 전역 옵션 ─────────────────────────────────────────────────────────
    differs(Global, "-v, --version", "`-v` is not an option.", &["docker_options_reach_the_sandbox"]).lacks(N).sbx("-V, --version"),
    not_yet(Global, "--config", UNPLANNED).lacks(N).note("sbx reads `DOCKER_CONFIG` for registry credentials."),
    not_yet(Global, "-D, --debug", UNPLANNED).lacks(P),
    not_yet(Global, "-l, --log-level", UNPLANNED).lacks(N),
    na(Global, "-c, --context", NO_DAEMON).only_in(D),
    na(Global, "-H, --host", NO_DAEMON).lacks(P),
    na(Global, "--tls, --tlscacert, --tlscert, --tlskey, --tlsverify", NO_DAEMON).only_in(D),
    differs(Global, "--runtime", "Picks the program that runs the VMs, instead of an OCI runtime.", &[
        "installed_executor_is_used_by_default",
        "vmm_packages_pins_and_version_files",
    ])
    .only_in(P)
    .sbx("--executor"),
    na(Global, "--runtime-flag, --conmon, --hooks-dir", "They configure an OCI runtime, its monitor and its hooks; sbx has no OCI runtime.").only_in(P),
    na(Global, "--connection, --url, --identity, --ssh, --remote", NO_DAEMON).only_in(P).note("How Podman's client reaches a Podman service, such as the one in `podman machine`."),
    na(Global, "--tls-ca, --tls-cert, --tls-key, --tls-details", NO_DAEMON).only_in(P),
    na(Global, "--address", NO_DAEMON).only_in(N).note("The containerd socket."),
    not_yet(Global, "--root, --runroot, --imagestore, --volumepath, --tmpdir", UNPLANNED).only_in(P).note("sbx's `--data-dir` and `--cache-dir` say where its state and images live."),
    not_yet(Global, "--data-root", UNPLANNED).only_in(N).note("sbx's `--data-dir` and `--cache-dir` say where its state and images live."),
    not_yet(Global, "--namespace", UNPLANNED).only_in(N).note("containerd namespaces; see `nerdctl namespace` above."),
    na(Global, "--storage-driver, --storage-opt, --snapshotter", "Storage drivers and snapshotters stack image layers for containers on the host kernel. sbx gives each VM the layers as read-only disks (`--rootfs-format`).").only_in(P.and(N)),
    not_yet(Global, "--transient-store", UNPLANNED).only_in(P),
    na(Global, "--cgroup-manager", CGROUP).only_in(P.and(N)),
    na(Global, "--cdi-spec-dir", "The VM sees only the devices the executor gives it.").only_in(P),
    wont(Global, "--network-config-dir, --cni-path, --cni-netconfpath", NO_NETWORKS).only_in(P.and(N)),
    not_yet(Global, "--events-backend", UNPLANNED).only_in(P),
    not_yet(Global, "--module", UNPLANNED).only_in(P),
    not_yet(Global, "--out", UNPLANNED).only_in(P),
    not_yet(Global, "--syslog", UNPLANNED).only_in(P),
    not_yet(Global, "--log-file", UNPLANNED).only_in(N),
    not_yet(Global, "--insecure-registry", UNPLANNED).only_in(N),
    not_yet(Global, "--host-gateway-ip", LATER).only_in(N).note("With `--add-host`."),
    na(Global, "--selinux-enabled", OWN_KERNEL).only_in(N),
    na(Global, "--userns-remap", USERNS).only_in(N),
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
    ]).lacks(P)
    .sbx("--executor"),
    same(Run, "--stop-signal", &["stop_sends_the_stop_signal_then_powers_off"]),
    differs(Run, "--stop-timeout", "No negative values (wait forever).", &["stop_sends_the_stop_signal_then_powers_off"])
        .why("Whoever waits can stop waiting (Ctrl+C, then `kill`)."),
    differs(Run, "--storage-opt", "Only `size=`: the writable disk, 1 GiB by default, at least 64 MiB. Sparse, so the host only spends what the sandbox writes.", &[
        "docker_options_reach_the_sandbox",
        "parse::tests::storage_opt_sets_the_writable_disk_size",
    ]).only_in(D),
    same(Run, "-t, --tty", &["run_t_gives_the_main_process_a_terminal"]),
    same(Run, "-u, --user", &["docker_options_reach_the_sandbox"]),
    differs(Run, "-v, --volume", "With a host path, live-shares a host directory (`SRC:DST[:ro]`), which needs an executor with virtio-fs; without one it is an error, never a silent copy. With a name, attaches a volume (below).", &[
        "volume_is_shared_live",
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
    not_yet(Run, "--domainname", UNPLANNED).lacks(P),
    not_yet(Run, "--gpus", LATER).note("When an executor offers a GPU."),
    not_yet(Run, "--group-add", UNPLANNED),
    not_yet(Run, "--health-cmd, --health-interval, --health-retries, --health-start-period, --health-start-interval, --health-timeout", LATER),
    not_yet(Run, "--no-healthcheck", LATER),
    not_yet(Run, "--label-file", UNPLANNED),
    not_yet(Run, "--log-driver, --log-opt", UNPLANNED).note("The shim writes each detached sandbox's log."),
    not_yet(Run, "-P, --publish-all", UNPLANNED).lacks(N),
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
    wont(Run, "--link", "Legacy in Docker. `--connect SANDBOX:PORT` is sbx's way to reach another sandbox: one port, by name, looked up at each connection.").only_in(D)
        .note("`--link NAME:ALIAS` is `--connect ALIAS=NAME:PORT`, one port at a time."),
    wont(Run, "--network-alias", NO_NETWORKS).lacks(N)
        .note("The connecting side picks the name: `--connect ALIAS=SANDBOX:PORT`."),
    wont(Run, "--volume-driver", "Extensions are out of scope.").only_in(D),
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
    na(Run, "--expose", "Nothing reaches a sandbox except a port published with `-p` or named by another sandbox's `--connect`, so declaring a port changes nothing.").lacks(N),
    na(Run, "--init", "Always on: sbx's guest init is PID 1 and reaps orphans."),
    na(Run, "--annotation", "Annotations go to an OCI runtime; sbx has none."),
    na(Run, "--disable-content-trust", NO_DCT).lacks(N),
    na(Run, "--isolation", WINDOWS_CONTAINERS).lacks(P),
    na(Run, "--cpu-count, --cpu-percent, --io-maxbandwidth, --io-maxiops", WINDOWS_CONTAINERS).only_in(D),
    not_yet(Run, "--umask", UNPLANNED),
    wont(Run, "--use-api-socket", NO_API).only_in(D).note("Mounts the Docker Engine API socket into the container."),
    // Podman과 nerdctl에만 있는 것.
    differs(Run, "--timeout", "Stops the sandbox this long after each start: the stop signal, then a power-off after `--stop-timeout` (Podman kills the container when the time is up).", &[
        "a_detached_sandbox_stops_after_its_timeout",
    ])
    .only_in(P),
    not_yet(Run, "--pod, --pod-id-file", UNPLANNED).only_in(P).note("`--connect` links sandboxes one port at a time."),
    na(Run, "--userns keep-id, auto, nomap", USERNS).only_in(P).note("Podman's values that map your user into the container."),
    na(Run, "--uidmap, --gidmap, --subuidname, --subgidname", USERNS).only_in(P),
    wont(Run, "--arch, --os, --variant", NO_EMULATION).only_in(P).note("`sbx pull --platform` downloads another architecture's image."),
    not_yet(Run, "--authfile, --cert-dir, --creds, --tls-verify", UNPLANNED).only_in(P).note("Registry access for the pull. sbx uses Docker's `config.json` and the containers `registries.conf`."),
    not_yet(Run, "--retry, --retry-delay", UNPLANNED).only_in(P),
    not_yet(Run, "--decryption-key", UNPLANNED).only_in(P),
    not_yet(Run, "--verify, --cosign-key, --cosign-certificate-identity, --cosign-certificate-identity-regexp, --cosign-certificate-oidc-issuer, --cosign-certificate-oidc-issuer-regexp", UNPLANNED).only_in(N).note(POLICY_JSON),
    not_yet(Run, "--ipfs-address", UNPLANNED).only_in(N),
    not_yet(Run, "--replace", UNPLANNED).only_in(P),
    not_yet(Run, "--requires", UNPLANNED).only_in(P),
    not_yet(Run, "--rmi", UNPLANNED).only_in(P),
    not_yet(Run, "--rootfs", UNPLANNED).only_in(P.and(N)).note("Runs from a directory instead of an image."),
    not_yet(Run, "--secret", UNPLANNED).only_in(P),
    not_yet(Run, "--env-host, --env-merge, --unsetenv, --unsetenv-all", UNPLANNED).only_in(P),
    not_yet(Run, "--http-proxy", UNPLANNED).only_in(P),
    not_yet(Run, "--tz", UNPLANNED).only_in(P),
    not_yet(Run, "--passwd, --passwd-entry, --group-entry, --hostuser", UNPLANNED).only_in(P),
    not_yet(Run, "--hosts-file, --no-hosts, --no-hostname, --chrootdirs", UNPLANNED).only_in(P),
    not_yet(Run, "--image-volume", UNPLANNED).only_in(P),
    not_yet(Run, "--read-only-tmpfs", UNPLANNED).only_in(P),
    not_yet(Run, "--shm-size-systemd", UNPLANNED).only_in(P),
    not_yet(Run, "--pidfile", UNPLANNED).only_in(P.and(N)),
    not_yet(Run, "--systemd", UNPLANNED).only_in(P.and(N)),
    not_yet(Run, "--sdnotify", UNPLANNED).only_in(P),
    not_yet(Run, "--personality", UNPLANNED).only_in(P),
    not_yet(Run, "--health-on-failure, --health-log-destination, --health-max-log-count, --health-max-log-size", LATER).only_in(P),
    not_yet(Run, "--health-startup-cmd, --health-startup-interval, --health-startup-retries, --health-startup-success, --health-startup-timeout", LATER).only_in(P),
    na(Run, "--init-path", "Always on: sbx's guest init is PID 1 and reaps orphans.").only_in(P),
    na(Run, "--init-binary", "Always on: sbx's guest init is PID 1 and reaps orphans.").only_in(N),
    na(Run, "--conmon-pidfile", "conmon is Podman's container monitor; sbx's `sbx-shim` owns a detached sandbox.").only_in(P),
    na(Run, "--cgroup-conf, --cgroups", CGROUP).only_in(P.and(N)),
    na(Run, "--kernel-memory", CGROUP).only_in(N),
    na(Run, "--rdt-class", "Intel RDT cache allocation for a process on the host. The VM's size (`--cpus`, `-m`) is the limit.").only_in(P.and(N)),
    na(Run, "--seccomp-policy", OWN_KERNEL).only_in(P),
    na(Run, "--preserve-fd, --preserve-fds", NO_FD).only_in(P),
    only(Run, "--rootfs-format","The root filesystem disk format; `erofs` by default.")
        .why("Different executors and kernels read different formats."),
    only(Run, "--mount type=copy", "Copies a host directory in when the sandbox starts; take results out with `sbx cp`.")
        .why("Works on every executor, including those without live sharing."),
    only(Run, "--allow", "With `--network allow`: a `HOST:PORT` the sandbox may connect to (`*.example.com:443`, `10.0.0.5:*`). A `*` port, an IPv6 address or `localhost` works only for programs that use `HTTP_PROXY`; `run` warns about each."),
    only(Run, "--egress-limit", "With `--network allow` or `--connect`: the most bytes all connections may move in each start, both ways (`--connect` traffic counts for the sandbox that connects)."),
    only(Run, "--connect", "`[NAME=]SANDBOX:PORT`: programs inside reach TCP `PORT` of another running sandbox (same data directory) as `NAME:PORT`, with any protocol, and nothing else opens. The other sandbox needs no option. The target is looked up at each connection, so a restarted one works on the next; a stopped one fails that connection and is recorded in `events`. An MCP sandbox may only connect to MCP sandboxes.")
        .why("Docker networks open every port of every container on the network to each other. `--connect` opens one port, by name, like `--allow`, and the VM still has no network interface."),
    // ── exec ─────────────────────────────────────────────────────────────
    same(Exec, "-d, --detach", &["exec_cp_stop_on_a_running_sandbox"]),
    same(Exec, "-e, --env", &["docker_options_reach_the_sandbox"]),
    same(Exec, "--env-file", &["docker_options_reach_the_sandbox"]),
    same(Exec, "-i, --interactive", &["exec_cp_stop_on_a_running_sandbox"]),
    same(Exec, "-t, --tty", &["exec_cp_stop_on_a_running_sandbox"]),
    same(Exec, "-u, --user", &["docker_options_reach_the_sandbox"]),
    same(Exec, "-w, --workdir", &["docker_options_reach_the_sandbox"]),
    not_yet(Exec, "--detach-keys", UNPLANNED).lacks(N),
    na(Exec, "--privileged", OWN_KERNEL),
    not_yet(Exec, "--latest", UNPLANNED).only_in(P),
    not_yet(Exec, "--cidfile", UNPLANNED).only_in(P),
    not_yet(Exec, "--no-session", UNPLANNED).only_in(P),
    na(Exec, "--preserve-fd, --preserve-fds", NO_FD).only_in(P),
    only(Exec, "--timeout", "Kills the command after a time and exits with 124, like `timeout(1)`."),
    // ── start ────────────────────────────────────────────────────────────
    same(Start, "-a, --attach", &["create_start_inspect_rm"]),
    same(Start, "-i, --interactive", &["docker_options_reach_the_sandbox"]).lacks(N),
    not_yet(Start, "--detach-keys", UNPLANNED),
    not_yet(Start, "--checkpoint, --checkpoint-dir", LATER).lacks(P).note("Snapshots, when an executor offers them."),
    not_yet(Start, "--all, --filter, --latest", UNPLANNED).only_in(P),
    not_yet(Start, "--sig-proxy", UNPLANNED).only_in(P),
    // ── stop, restart ────────────────────────────────────────────────────
    same(Stop, "-s, --signal", &["stop_sends_the_stop_signal_then_powers_off"]).lacks(P),
    differs(Stop, "-t, --timeout", "Docker's older name. No negative values. When it runs out, the guest is powered off (exit code 137) instead of SIGKILL.", &[
        "exec_cp_stop_on_a_running_sandbox",
        "kill_wait_and_restart",
    ])
    .sbx("-t, --time"),
    not_yet(Stop, "--all, --cidfile, --filter, --ignore, --latest", UNPLANNED).only_in(P),
    // ── kill ─────────────────────────────────────────────────────────────
    same(Kill, "-s, --signal", &["kill_wait_and_restart"]),
    not_yet(Kill, "--all, --cidfile, --latest", UNPLANNED).only_in(P),
    // ── rm ───────────────────────────────────────────────────────────────
    differs(Rm, "-f, --force", "Powers the guest off without a signal.", &[
        "system_df_and_prune_free_what_nothing_uses",
        "docker_options_reach_the_sandbox",
    ]),
    wont(Rm, "-l, --link", "Legacy in Docker, and sbx has no links.").only_in(D),
    na(Rm, "-v, --volumes", "sbx makes no anonymous volumes: a sandbox's writable disk goes with it, and named volumes stay (as in Docker)."),
    not_yet(Rm, "--all, --cidfile, --depend, --filter, --ignore, --latest, --time", UNPLANNED).only_in(P),
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
    not_yet(Ps, "--external, --noheading, --pod, --sort, --sync, --watch", UNPLANNED).only_in(P),
    na(Ps, "--namespace, --ns", "Shows each container's namespaces on the host kernel; a sandbox has its own kernel.").only_in(P),
    // ── inspect ──────────────────────────────────────────────────────────
    differs(Inspect, "-f, --format", GO_TEMPLATES, &["json_output_contract", "jq_shapes_json_output"]).sbx("--format"),
    not_yet(Inspect, "-s, --size", UNPLANNED),
    not_yet(Inspect, "--type", UNPLANNED).note("`inspect` takes sandboxes only."),
    not_yet(Inspect, "--latest", UNPLANNED).only_in(P),
    not_yet(Inspect, "--mode", UNPLANNED).only_in(N).note("nerdctl's `native` mode shows containerd's own fields."),
    // ── stats ────────────────────────────────────────────────────────────
    differs(Stats, "--format", GO_TEMPLATES, &["stats_reports_guest_usage"]).sbx("--format"),
    same(Stats, "--no-stream", &["stats_reports_guest_usage"]),
    not_yet(Stats, "-a, --all", UNPLANNED),
    not_yet(Stats, "--no-trunc", UNPLANNED),
    not_yet(Stats, "--interval, --latest, --no-reset", UNPLANNED).only_in(P),
    // ── attach ───────────────────────────────────────────────────────────
    same(Attach, "--detach-keys", &["attach::tests::detach_keys_parse_like_docker"]),
    same(Attach, "--no-stdin", &["docker_options_reach_the_sandbox"]),
    same(Attach, "--sig-proxy", &["attach_tty_and_signal_proxy"]).lacks(N),
    not_yet(Attach, "--latest", UNPLANNED).only_in(P),
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
    not_yet(Logs, "--details", UNPLANNED).lacks(P),
    not_yet(Logs, "--color, --latest, --names", UNPLANNED).only_in(P),
    // ── cp ───────────────────────────────────────────────────────────────
    not_yet(Cp, "-a, --archive", UNPLANNED).lacks(N),
    not_yet(Cp, "-L, --follow-link", UNPLANNED).lacks(P),
    not_yet(Cp, "-q, --quiet", UNPLANNED).only_in(D),
    not_yet(Cp, "--overwrite", UNPLANNED).only_in(P),
    // ── events ───────────────────────────────────────────────────────────
    differs(Events, "-f, --filter", "Keys `type`, `event`, `sandbox`, `image`. No `container` or `label`.", &[
        "filters",
        "events_replay_and_follow",
    ]),
    differs(Events, "--format", GO_TEMPLATES, &["events_replay_and_follow"]).sbx("--format"),
    differs(Events, "--since", "A date without a time zone is refused.", &["events_replay_and_follow"]).lacks(N),
    differs(Events, "--until", "A date without a time zone is refused.", &["events_replay_and_follow"]).lacks(N),
    not_yet(Events, "--no-trunc, --stream", UNPLANNED).only_in(P),
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
    not_yet(Images, "--tree", UNPLANNED).lacks(P),
    not_yet(Images, "--history, --noheading, --sort", UNPLANNED).only_in(P),
    not_yet(Images, "--names", UNPLANNED).only_in(N),
    // ── pull ─────────────────────────────────────────────────────────────
    differs(Pull, "--platform", "`linux/amd64` or `linux/arm64`; the host's by default. Only the host's architecture runs.", &[
        "parse::tests::platforms",
    ]),
    not_yet(Pull, "-a, --all-tags", UNPLANNED).lacks(N),
    not_yet(Pull, "-q, --quiet", UNPLANNED),
    na(Pull, "--disable-content-trust", NO_DCT).lacks(N),
    not_yet(Pull, "--arch, --os, --variant", UNPLANNED).only_in(P).note("`--platform` does this."),
    not_yet(Pull, "--authfile, --cert-dir, --creds, --tls-verify", UNPLANNED).only_in(P).note("sbx uses Docker's `config.json` and the containers `registries.conf`."),
    not_yet(Pull, "--retry, --retry-delay", UNPLANNED).only_in(P),
    not_yet(Pull, "--policy", UNPLANNED).only_in(P),
    not_yet(Pull, "--decryption-key", UNPLANNED).only_in(P),
    not_yet(Pull, "--all-platforms, --unpack", UNPLANNED).only_in(N),
    not_yet(Pull, "--verify, --cosign-key, --cosign-certificate-identity, --cosign-certificate-identity-regexp, --cosign-certificate-oidc-issuer, --cosign-certificate-oidc-issuer-regexp", UNPLANNED).only_in(N).note(POLICY_JSON),
    not_yet(Pull, "--soci-index-digest", UNPLANNED).only_in(N).note("Lazy pulling; see Behavior."),
    not_yet(Pull, "--ipfs-address", UNPLANNED).only_in(N),
    // ── push ─────────────────────────────────────────────────────────────
    differs(Push, "-a, --all-tags", "Pushes the local tags of the repository one after another, in name order; one that fails doesn't stop the others.", &[
        "tag_and_push_to_a_registry",
        "app::push::tests::push_names_a_local_tag_or_all_tags_of_a_repository",
    ]).lacks(P),
    differs(Push, "-q, --quiet", "Prints only the pushed reference, one per line.", &["tag_and_push_to_a_registry"]),
    not_yet(Push, "--platform", UNPLANNED)
        .note("sbx keeps one platform per image and always pushes that single-platform manifest."),
    na(Push, "--disable-content-trust", NO_DCT).lacks(N),
    not_yet(Push, "--authfile, --cert-dir, --creds, --tls-verify", UNPLANNED).only_in(P),
    not_yet(Push, "--retry, --retry-delay", UNPLANNED).only_in(P),
    not_yet(Push, "--compression-format, --compression-level, --force-compression, --dir-compress", UNPLANNED).only_in(P),
    not_yet(Push, "--format oci, v2s2, v2s1", UNPLANNED).only_in(P).note("sbx pushes the manifest as it was pulled, or OCI for a built image."),
    not_yet(Push, "--digestfile", UNPLANNED).only_in(P),
    not_yet(Push, "--encrypt-layer, --encryption-key", UNPLANNED).only_in(P),
    not_yet(Push, "--remove-signatures", UNPLANNED).only_in(P),
    not_yet(Push, "--sign-by, --sign-by-sigstore, --sign-by-sigstore-private-key, --sign-by-sq-fingerprint, --sign-passphrase-file", UNPLANNED).only_in(P),
    not_yet(Push, "--sign, --cosign-key, --notation-key-name", UNPLANNED).only_in(N),
    not_yet(Push, "--all-platforms", UNPLANNED).only_in(N),
    not_yet(Push, "--allow-nondistributable-artifacts", UNPLANNED).only_in(N),
    not_yet(Push, "--soci-span-size, --soci-min-layer-size", UNPLANNED).only_in(N),
    not_yet(Push, "--ipfs-address", UNPLANNED).only_in(N),
    // ── rmi ──────────────────────────────────────────────────────────────
    differs(Rmi, "-f, --force", "Also removes images used by stopped sandboxes in the same data directory, or with several references. Never those of running sandboxes.", &[
        "docker_options_reach_the_sandbox",
        "behavior::tests::rmi_table",
    ])
    .why("sbx can't see sandboxes in another data directory."),
    na(Rmi, "--no-prune", "sbx images have no parent images to keep.").lacks(N),
    not_yet(Rmi, "--platform", UNPLANNED).only_in(D),
    not_yet(Rmi, "--all, --ignore", UNPLANNED).only_in(P),
    not_yet(Rmi, "--async", UNPLANNED).only_in(N),
    // ── save ─────────────────────────────────────────────────────────────
    same(Save, "-o, --output", &["load_and_save_round_trip_an_oci_archive"]),
    not_yet(Save, "--platform", UNPLANNED).lacks(P),
    not_yet(Save, "-q, --quiet", UNPLANNED).only_in(P.and(N)),
    not_yet(Save, "--format oci-archive, oci-dir, docker-archive, docker-dir", UNPLANNED).only_in(P).note("sbx writes one archive that is an OCI image layout with Docker's `manifest.json`."),
    not_yet(Save, "--multi-image-archive", UNPLANNED).only_in(P),
    not_yet(Save, "--compress, --uncompressed, --dir-compress", UNPLANNED).only_in(P),
    not_yet(Save, "--all-platforms", UNPLANNED).only_in(N),
    // ── load ─────────────────────────────────────────────────────────────
    same(Load, "-i, --input", &["load_and_save_round_trip_an_oci_archive"]),
    differs(Load, "--platform", "`linux/amd64` or `linux/arm64`; the host's by default.", &["parse::tests::platforms"]).lacks(P),
    not_yet(Load, "-q, --quiet", UNPLANNED),
    only(Load, "--tag", "Names the image, when the archive holds one."),
    not_yet(Load, "--all-platforms", UNPLANNED).only_in(N),
    // ── login ────────────────────────────────────────────────────────────
    same(Login, "-u, --username", &["login_and_logout_use_dockers_config"]),
    same(Login, "--password-stdin", &["login_and_logout_use_dockers_config"]),
    wont(Login, "-p, --password", "A password on the command line ends up in shell history and `ps`. Use `--password-stdin`, or let sbx ask."),
    not_yet(Login, "--authfile, --compat-auth-file, --cert-dir, --tls-verify", UNPLANNED).only_in(P),
    not_yet(Login, "--get-login, --secret, --verbose", UNPLANNED).only_in(P),
    // ── volume create ────────────────────────────────────────────────────
    same(VolumeCreate, "--label", &["volume_data_outlives_the_sandbox"]),
    differs(VolumeCreate, "-o, --opt", "Only `size=` (e.g. `size=50g`, at least 64m). An unknown option is an error.", &[
        "volume_data_outlives_the_sandbox",
        "app::volume::tests::only_the_size_option_is_known",
    ]).lacks(N),
    wont(VolumeCreate, "-d, --driver", "Extensions are out of scope: a volume is a disk of the VM.").lacks(N),
    na(VolumeCreate, "--availability, --group, --limit-bytes, --required-bytes, --scope, --secret, --sharing, --topology-preferred, --topology-required, --type", SWARM).only_in(D),
    not_yet(VolumeCreate, "--uid, --gid", UNPLANNED).only_in(P),
    not_yet(VolumeCreate, "--ignore", UNPLANNED).only_in(P).note("`sbx volume create` of an existing name returns it, as Docker does."),
    // ── volume ls ────────────────────────────────────────────────────────
    differs(VolumeLs, "-f, --filter", "Keys `name` (a substring), `label`, `dangling`. No `driver`.", &[
        "volume_data_outlives_the_sandbox",
    ]),
    differs(VolumeLs, "--format", GO_TEMPLATES, &["volume_data_outlives_the_sandbox"]).sbx("--format"),
    same(VolumeLs, "-q, --quiet", &["volume_data_outlives_the_sandbox"]),
    na(VolumeLs, "--cluster", SWARM).only_in(D),
    not_yet(VolumeLs, "--noheading", UNPLANNED).only_in(P),
    not_yet(VolumeLs, "--size", UNPLANNED).only_in(N).note("`sbx volume ls` always shows the size and what the disk uses."),
    // ── volume inspect ───────────────────────────────────────────────────
    differs(VolumeInspect, "-f, --format", GO_TEMPLATES, &["volume_data_outlives_the_sandbox"]).sbx("--format"),
    not_yet(VolumeInspect, "--all", UNPLANNED).only_in(P),
    not_yet(VolumeInspect, "--size", UNPLANNED).only_in(N),
    // ── volume rm ────────────────────────────────────────────────────────
    differs(VolumeRm, "-f, --force", "Only makes a missing volume no error; a volume in use is never removed.", &[
        "volume_data_outlives_the_sandbox",
    ])
    .why("Docker's `-f` doesn't remove a volume in use either."),
    not_yet(VolumeRm, "--all, --time", UNPLANNED).only_in(P),
    // ── volume prune ─────────────────────────────────────────────────────
    same(VolumePrune, "-a, --all", &["volume_data_outlives_the_sandbox"]).lacks(N),
    same(VolumePrune, "-f, --force", &["volume_data_outlives_the_sandbox"]),
    differs(VolumePrune, "--filter", "Key `label` only.", &["volume_data_outlives_the_sandbox"]).lacks(N),
    not_yet(VolumePrune, "--dry-run", UNPLANNED).only_in(P),
    // ── system df ────────────────────────────────────────────────────────
    differs(SystemDf, "--format", GO_TEMPLATES, &["system_df_and_prune_free_what_nothing_uses"]).sbx("--format"),
    not_yet(SystemDf, "-v, --verbose", UNPLANNED),
    // ── system prune ─────────────────────────────────────────────────────
    same(SystemPrune, "-a, --all", &["system_df_and_prune_free_what_nothing_uses"]),
    same(SystemPrune, "-f, --force", &["system_df_and_prune_free_what_nothing_uses"]),
    differs(SystemPrune, "--filter", "Keys `until` and `label`, for sandboxes; `label` also for volumes.", &["system_df_and_prune_free_what_nothing_uses"]).lacks(N),
    differs(SystemPrune, "--volumes", "Only with `-a` does it remove anything: every volume no sandbox points at (sbx has no anonymous volumes).", &[
        "system_df_and_prune_free_what_nothing_uses",
    ]),
    not_yet(SystemPrune, "--build, --external", UNPLANNED).only_in(P),
    // ── builder prune ────────────────────────────────────────────────────
    differs(BuilderPrune, "-a, --all", "Removes every cached step. Layers that images use stay with the images.", &["build_cache_stays_under_its_cap"]).lacks(P),
    same(BuilderPrune, "-f, --force", &["build_cache_stays_under_its_cap"]).lacks(P),
    differs(BuilderPrune, "--keep-storage", "Keeps the most recently used cache that no image uses, up to SIZE. Not with `-a`.", &["build_cache_stays_under_its_cap"]).only_in(D),
    not_yet(BuilderPrune, "--max-used-space, --min-free-space, --reserved-space", UNPLANNED).only_in(D).note("`SBX_BUILD_CACHE_MAX` caps the cache after every build."),
    not_yet(BuilderPrune, "--filter", UNPLANNED).only_in(D),
    not_yet(BuilderPrune, "--verbose", UNPLANNED).only_in(D),
    na(BuilderPrune, "--builder", "There are no builder instances: each build stage gets its own VM.").only_in(D),
    not_yet(BuilderPrune, "-D, --debug", UNPLANNED).only_in(D),
    na(BuilderPrune, "--timeout", NO_BUILDER).only_in(D).note("How long to wait for a builder instance."),
    na(BuilderPrune, "--buildkit-host", NO_BUILDER).only_in(N),
    // ── 동작 ─────────────────────────────────────────────────────────────
    same(Behavior, "Exit codes 125, 126 and 127", &[
        "run_failures_before_the_command_are_125",
        "run_command_not_found_is_127_and_not_executable_is_126",
        "behavior::tests::exit_codes_match_docker",
    ]).only_in(D.and(P)),
    same(Behavior, "Short image names (`alpine` is `docker.io/library/alpine:latest`)", &["json_output_contract"]),
    same(Behavior, "Registry credentials from Docker's `config.json` and credential helpers", &[
        "login_and_logout_use_dockers_config",
    ]).only_in(Tools::ALL),
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
    wont(Behavior, "Go templates in `--format` (`--format '{{.ID}}'`)", GO_TEMPLATES).only_in(Tools::ALL),
    wont(Behavior, "The Docker Engine API socket", "Out of scope: sbx has no daemon. Scripts use `--format json`; agents use `sbx mcp`.").only_in(D.and(P)),
    wont(Behavior, "Running another architecture's images (emulation)", "Out of scope: a sandbox runs the host's architecture.").only_in(Tools::ALL),
    not_yet(Behavior, "Lazy pulling (eStargz, SOCI and other snapshotters)", UNPLANNED).only_in(N).note("A container starts before its image is fully downloaded. sbx prepares an image's layer disks once and shares them between images."),
    // ── build ────────────────────────────────────────────────────────────
    same(Build, "-t, --tag", &["build_runs_stages_and_caches_steps"]),
    same(Build, "-f, --file", &["build_runs_stages_and_caches_steps", "build_source_date_epoch_fixes_the_digest"]).note("`-f -` reads the Dockerfile from stdin; the context is still the directory. `FILE.dockerignore` next to the file is read before the context's `.dockerignore`, as in BuildKit."),
    same(Build, "--build-arg", &["build_failures_targets_and_build_args"]),
    same(Build, "--target", &["build_failures_targets_and_build_args"]),
    same(Build, "--no-cache", &["build_runs_stages_and_caches_steps"]),
    not_yet(Build, "--platform", LATER).note("Images are built for the host's architecture."),
    differs(Build, "--progress", "`auto` (the default) prints one line per step and shows a `RUN` step's output only when it fails; `plain` streams it too. There is no `tty` or `rawjson`; `--format json` prints JSON Lines.", &["build_runs_stages_and_caches_steps"]).lacks(P),
    differs(Build, "-o, --output", "`type=sbx` (sbx's image store, the default; `type=image` too), `type=docker` (into `docker load`, or a tar with `dest=FILE`), `type=oci,dest=FILE|DIR/|-` (an archive, a layout directory, or stdout), `type=registry` (push). Repeat it for several; given outputs replace the default, so `-o type=docker` alone keeps nothing in sbx (the layer cache stays). The archive is the same file `sbx save` writes. No `type=local` or `type=tar`. Layers are turned into tars when exported, and a layer's compressed digest is fixed the first time it is exported, so every export of an image has the same manifest digest.", &["build_outputs_export_one_image_everywhere"]),
    same(Build, "--load", &["build_outputs_export_one_image_everywhere"]).only_in(D).note("Needs `docker` on PATH; without it the build stops before it starts and says to write a tar instead."),
    same(Build, "--push", &["build_outputs_export_one_image_everywhere"]).only_in(D),
    not_yet(Build, "--secret", LATER),
    not_yet(Build, "--ssh", LATER),
    not_yet(Build, "--add-host", LATER),
    differs(Build, "--network", "`none` (the default; Docker's default is the full network) or `allow` with `--allow` destinations, as with `run`. With `none`, a failed step lists the names it looked up and says the network is closed.", &["build_shows_blocked_names"]),
    not_yet(Build, "--annotation", UNPLANNED).lacks(N),
    not_yet(Build, "--attest, --provenance, --sbom", UNPLANNED),
    not_yet(Build, "--build-context", UNPLANNED),
    not_yet(Build, "--cache-from, --cache-to", UNPLANNED),
    not_yet(Build, "--call, --check", UNPLANNED).only_in(D),
    not_yet(Build, "--iidfile", UNPLANNED),
    not_yet(Build, "--label", UNPLANNED),
    not_yet(Build, "--metadata-file", UNPLANNED).lacks(N),
    not_yet(Build, "--no-cache-filter", UNPLANNED).only_in(D),
    not_yet(Build, "--pull", UNPLANNED),
    same(Build, "-q, --quiet", &["build_runs_stages_and_caches_steps"]),
    not_yet(Build, "--shm-size", UNPLANNED).lacks(N),
    not_yet(Build, "--ulimit", UNPLANNED).lacks(N),
    differs(Build, "--allow", "Names a network destination `HOST:PORT` that `RUN` steps may reach, as with `run --allow`. buildx's entitlements (`network.host`, `security.insecure`) mean nothing in a build VM. A failed step lists the names it was refused, with the `--allow` to add.", &["build_shows_blocked_names"]).lacks(P),
    not_yet(Build, "-D, --debug", UNPLANNED).only_in(D),
    not_yet(Build, "--policy", UNPLANNED).only_in(D),
    not_yet(Build, "--resource", UNPLANNED).only_in(D).note("`--cpus` and `-m` size the build VM."),
    // Podman과 nerdctl에만 있는 것.
    not_yet(Build, "--layers, --squash, --squash-all", UNPLANNED).only_in(P).note("sbx caches every step that makes a layer and keeps the layers."),
    not_yet(Build, "--manifest, --all-platforms, --arch, --os, --os-feature, --os-version, --variant", LATER).only_in(P).note("Images are built for the host's architecture."),
    not_yet(Build, "--authfile, --cert-dir, --creds, --tls-verify, --retry, --retry-delay", UNPLANNED).only_in(P),
    not_yet(Build, "--build-arg-file", UNPLANNED).only_in(P),
    not_yet(Build, "--ignorefile", UNPLANNED).only_in(P),
    not_yet(Build, "--from", UNPLANNED).only_in(P),
    not_yet(Build, "--stdin", UNPLANNED).only_in(P),
    not_yet(Build, "--jobs", UNPLANNED).only_in(P),
    not_yet(Build, "--cache-ttl", UNPLANNED).only_in(P),
    not_yet(Build, "--timestamp, --source-date-epoch, --rewrite-timestamp", UNPLANNED).only_in(P).note("`SOURCE_DATE_EPOCH` in the environment or as a `--build-arg` sets the image's times and caps the file times in new layers (see the Dockerfile section)."),
    not_yet(Build, "--format oci, docker", UNPLANNED).only_in(P),
    not_yet(Build, "--volume, --mount", UNPLANNED).only_in(P),
    not_yet(Build, "--env, --unsetenv, --unsetlabel, --unsetannotation, --inherit-labels, --inherit-annotations, --created-annotation, --identity-label, --layer-label, --stage-labels", UNPLANNED).only_in(P),
    not_yet(Build, "--compress, --compression-format, --compression-level, --force-compression, --disable-compression", UNPLANNED).only_in(P),
    not_yet(Build, "--omit-history", UNPLANNED).only_in(P),
    not_yet(Build, "--save-stages, --skip-unused-stages", UNPLANNED).only_in(P),
    not_yet(Build, "--sbom-output, --sbom-image-output, --sbom-purl-output, --sbom-image-purl-output, --sbom-merge-strategy, --sbom-scanner-command, --sbom-scanner-image", UNPLANNED).only_in(P),
    not_yet(Build, "--sign-by", UNPLANNED).only_in(P),
    not_yet(Build, "--decryption-key", UNPLANNED).only_in(P),
    not_yet(Build, "--dns, --dns-option, --dns-search, --no-hosts, --no-hostname", UNPLANNED).only_in(P),
    not_yet(Build, "--group-add", UNPLANNED).only_in(P),
    not_yet(Build, "--http-proxy", UNPLANNED).only_in(P),
    not_yet(Build, "--iidfile-raw", UNPLANNED).only_in(P),
    not_yet(Build, "--logfile, --logsplit", UNPLANNED).only_in(P),
    not_yet(Build, "--cpp-flag", UNPLANNED).only_in(P),
    not_yet(Build, "--compat-volumes", UNPLANNED).only_in(P),
    not_yet(Build, "--cw", UNPLANNED).only_in(P),
    not_yet(Build, "--rm, --force-rm", UNPLANNED).only_in(P),
    not_yet(Build, "--source-policy-file", UNPLANNED).only_in(P.and(N)),
    not_yet(Build, "--ipfs", UNPLANNED).only_in(N),
    na(Build, "--buildkit-host", NO_BUILDER).only_in(N),
    na(Build, "--cap-add, --cap-drop, --security-opt, --ipc, --pid, --uts, --cgroupns, --isolation", "A build step already runs as root in its own VM; there is no container sandbox to tune.").only_in(P),
    na(Build, "--userns, --userns-uid-map, --userns-gid-map, --userns-uid-map-user, --userns-gid-map-group", USERNS).only_in(P),
    na(Build, "--cpu-period, --cpu-quota, --cpu-shares, --cpuset-cpus, --cpuset-mems, --memory-swap", CGROUP).only_in(P),
    na(Build, "--device", "The VM sees only the devices the executor gives it.").only_in(P),
    na(Build, "--runtime, --runtime-flag, --hooks-dir", "They configure an OCI runtime and its hooks; each build stage is a VM.").only_in(P),
    na(Build, "--disable-content-trust", NO_DCT).only_in(P),
    only(Build, "--cpus", "vCPUs of the build VM (default 1)."),
    differs(Build, "-m, --memory", "The memory of each stage's build VM (default 512m); Podman's limits the container of each step.", &[
        "build_vm_takes_cpus_and_memory",
    ])
    .only_in(P),
    na(Build, "--builder", "There are no builder instances: each build stage gets its own VM.").only_in(D),
    na(Build, "--cgroup-parent", CGROUP).lacks(N),
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
    // ── 제품 기능 ────────────────────────────────────────────────────────
    differs(Features, "Sharing host folders with containers", "`-v HOST:CONTAINER` shares a folder live through virtio-fs, when the executor offers it (an error otherwise, never a silent copy); `--mount type=copy` copies one in on every executor.", &[
        "volume_is_shared_live",
        "parse::tests::volumes",
    ])
    .offers(&[
        yes(DockerDesktop, "VirtioFS or gRPC FUSE on macOS", DD_SETTINGS),
        yes(PodmanDesktop, "your home folder by default", PODMAN_MACHINE_INIT),
        yes(OrbStack, "VirtioFS", "https://docs.orbstack.dev/docker/file-sharing"),
        yes(Colima, "virtiofs, 9p or sshfs", COLIMA_DEFAULTS),
        yes(RancherDesktop, "reverse-sshfs, 9p or virtiofs", "https://docs.rancherdesktop.io/ui/preferences/virtual-machine/volumes"),
    ]),
    differs(Features, "Publishing container ports on localhost", "`-p` listens on 127.0.0.1 unless you write `0.0.0.0:`. TCP only.", &[
        "published_ports_reach_the_sandbox",
    ])
    .offers(&[
        yes(DockerDesktop, "", "https://docs.docker.com/desktop/features/networking/"),
        yes(PodmanDesktop, "", "https://podman-desktop.io/docs/containers/starting-a-container"),
        yes(OrbStack, "", "https://docs.orbstack.dev/docker/network"),
        says(Colima, "\"Automatic Port Forwarding\"", COLIMA),
        yes(RancherDesktop, "", "https://docs.rancherdesktop.io/tutorials/working-with-containers/"),
    ]),
    differs(Features, "Building images from a Dockerfile", "`sbx build`, one VM per stage, with the network closed unless `--allow` opens it.", &[
        "build_runs_stages_and_caches_steps",
    ])
    .offers(&[
        yes(DockerDesktop, "", DD_OVERVIEW),
        yes(PodmanDesktop, "", "https://podman-desktop.io/features"),
        yes(OrbStack, "BuildKit", "https://docs.orbstack.dev/docker/images"),
        says(Colima, "BuildKit enabled by default", COLIMA_DEFAULTS),
        yes(RancherDesktop, "", "https://docs.rancherdesktop.io/ui/images"),
    ]),
    not_yet(Features, "Compose", "C1")
        .offers(&[
            yes(DockerDesktop, "included", DD_OVERVIEW),
            yes(PodmanDesktop, "can install the Compose engine", "https://podman-desktop.io/docs/compose/setting-up-compose"),
            yes(OrbStack, "included", "https://docs.orbstack.dev/docker/"),
            yes(RancherDesktop, "`docker-compose` bundled", "https://docs.rancherdesktop.io/references/bundled-utilities/"),
        ]),
    not_yet(Features, "A local Kubernetes cluster", UNPLANNED)
        .note("The roadmap's Kubernetes item is sbx as a containerd runtime (RuntimeClass), not a local cluster.")
        .offers(&[
            yes(DockerDesktop, "kubeadm or kind", "https://docs.docker.com/desktop/features/kubernetes/"),
            yes(PodmanDesktop, "kind, a built-in extension", "https://podman-desktop.io/docs/kind/installing-extension"),
            yes(OrbStack, "single node", "https://docs.orbstack.dev/kubernetes/"),
            yes(Colima, "k3s", COLIMA_DEFAULTS),
            yes(RancherDesktop, "k3s", "https://docs.rancherdesktop.io/"),
        ]),
    differs(Features, "A dashboard of containers", "`sbx tui`: sandboxes, images, volumes, events and executors in the terminal, not a graphical app. Every key runs an `sbx` command and shows it.", &[
        "tui::view::tests::sandboxes_tab_shows_rows_hints_and_recent_events",
        "tui::model::tests::keys_become_sbx_command_lines_for_the_selected_sandbox",
    ])
    .offers(&[
        yes(DockerDesktop, "", "https://docs.docker.com/desktop/use-desktop/"),
        yes(PodmanDesktop, "", "https://github.com/podman-desktop/podman-desktop"),
        yes(OrbStack, "", "https://docs.orbstack.dev/docker/"),
        says(Colima, "\"Simple CLI interface\"", COLIMA),
        yes(RancherDesktop, "", "https://docs.rancherdesktop.io/ui/images"),
    ]),
    differs(Features, "Managing images and volumes on screen", "The `sbx tui` images and volumes tabs: list, size and disk use, remove, prune, tag and push.", &[
        "tui::model::tests::images_tab_prunes_and_shows_disk_usage",
        "tui::model::tests::volumes_tab_lists_users_by_name_and_runs_volume_commands",
    ])
    .offers(&[
        yes(DockerDesktop, "", "https://docs.docker.com/desktop/use-desktop/"),
        yes(PodmanDesktop, "", "https://podman-desktop.io/tutorial/managing-your-application-resources"),
        yes(OrbStack, "", "https://docs.orbstack.dev/release-notes"),
        yes(RancherDesktop, "volumes are experimental", "https://docs.rancherdesktop.io/ui/volumes"),
    ]),
    wont(Features, "Extensions", NO_EXTENSIONS).offers(&[
        yes(DockerDesktop, "", "https://docs.docker.com/extensions/"),
        yes(PodmanDesktop, "", "https://podman-desktop.io/docs/extensions"),
        yes(RancherDesktop, "", "https://docs.rancherdesktop.io/ui/extensions"),
    ]),
    differs(Features, "Updating itself", "A notice at startup when a newer signed release exists, and `sbx upgrade`; it doesn't replace itself unasked.", &[
        "new_version_notice_and_upgrade",
    ])
    .offers(&[
        yes(DockerDesktop, "", DD_SETTINGS),
        yes(PodmanDesktop, "", "https://podman-desktop.io/docs/configuration/settings-reference"),
        yes(OrbStack, "with the GUI, not headless", "https://docs.orbstack.dev/headless"),
        says(Colima, "updated with `brew upgrade colima`", COLIMA_FAQ),
        yes(RancherDesktop, "macOS and Windows", "https://docs.rancherdesktop.io/ui/preferences/application/general"),
    ]),
    differs(Features, "Setting the VM's CPUs, memory and disk", "Per sandbox (`--cpus`, `-m`, `--storage-opt size=`), since each sandbox is its own VM; there is no shared VM to size.", &[
        "docker_options_reach_the_sandbox",
    ])
    .offers(&[
        yes(DockerDesktop, "", DD_SETTINGS),
        yes(PodmanDesktop, "when the machine is created", "https://podman-desktop.io/docs/podman/creating-a-podman-machine"),
        yes(OrbStack, "CPUs and memory", "https://docs.orbstack.dev/settings"),
        yes(Colima, "`--cpu`, `--memory`, `--disk`", COLIMA),
        yes(RancherDesktop, "CPUs and memory", "https://docs.rancherdesktop.io/ui/preferences/virtual-machine/hardware"),
    ]),
    wont(Features, "Running x86-64 images on Apple silicon (Rosetta, QEMU)", NO_EMULATION).offers(&[
        yes(DockerDesktop, "", DD_SETTINGS),
        yes(PodmanDesktop, "Rosetta, or QEMU", "https://podman-desktop.io/docs/podman/rosetta"),
        yes(OrbStack, "Rosetta", "https://docs.orbstack.dev/docker/"),
        yes(Colima, "Rosetta, or binfmt emulation", COLIMA_DEFAULTS),
        yes(RancherDesktop, "Rosetta", "https://docs.rancherdesktop.io/ui/preferences/virtual-machine/emulation"),
    ]),
    wont(Features, "A Docker Engine API socket (for Testcontainers and other API clients)", NO_API).offers(&[
        yes(DockerDesktop, "", DD_SETTINGS),
        yes(PodmanDesktop, "", "https://podman-desktop.io/docs/migrating-from-docker/managing-docker-compatibility"),
        yes(OrbStack, "", "https://docs.orbstack.dev/docker/"),
        yes(Colima, "`$HOME/.colima/default/docker.sock`", COLIMA_FAQ),
        yes(RancherDesktop, "with the moby engine", "https://docs.rancherdesktop.io/how-to-guides/using-testcontainers/"),
    ]),
];

// 제품 기능 줄에서 여러 번 쓰는 출처.
const DD_SETTINGS: &str = "https://docs.docker.com/desktop/settings-and-maintenance/settings/";
const DD_OVERVIEW: &str = "https://docs.docker.com/desktop/";
const PODMAN_MACHINE_INIT: &str =
    "https://docs.podman.io/en/latest/markdown/podman-machine-init.1.html";
const COLIMA: &str = "https://github.com/abiosoft/colima";
const COLIMA_FAQ: &str = "https://github.com/abiosoft/colima/blob/main/docs/FAQ.md";
const COLIMA_DEFAULTS: &str =
    "https://github.com/abiosoft/colima/blob/main/embedded/defaults/colima.yaml";

/// 할 수 있는 일이 아니라 사실인 것(라이선스, 지원 OS). 진행률에 세지 않는다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fact {
    /// 무엇.
    pub what: &'static str,
    /// sbx의 것.
    pub sbx: &'static str,
    /// 제품마다의 것. 모두 [`Has::Says`]이고 `detail`이 내용이다.
    pub offers: &'static [Offer],
}

/// 라이선스와 지원 OS.
pub const FACTS: &[Fact] = &[
    Fact {
        what: "License and price",
        sbx: "Apache-2.0 or MIT, free",
        offers: &[
            says(
                DockerDesktop,
                "Free for personal use, education, non-commercial open source and small businesses (fewer than 250 employees and less than $10 million in annual revenue); a paid subscription otherwise",
                "https://docs.docker.com/subscription/desktop-license/",
            ),
            says(
                PodmanDesktop,
                "Apache-2.0",
                "https://github.com/podman-desktop/podman-desktop",
            ),
            says(
                OrbStack,
                "Free for personal, non-commercial use; a paid license per user for commercial use",
                "https://docs.orbstack.dev/licensing",
            ),
            says(Colima, "MIT", COLIMA),
            says(
                RancherDesktop,
                "Apache-2.0",
                "https://github.com/rancher-sandbox/rancher-desktop",
            ),
        ],
    },
    Fact {
        what: "Host operating systems",
        sbx: "Linux, and macOS 15+ on Apple silicon; Windows installs, but has no tested executor to run sandboxes yet",
        offers: &[
            says(DockerDesktop, "macOS, Windows, Linux", DD_OVERVIEW),
            says(
                PodmanDesktop,
                "macOS, Windows, Linux",
                "https://podman-desktop.io/docs/installation",
            ),
            says(
                OrbStack,
                "macOS 14 and newer",
                "https://docs.orbstack.dev/faq",
            ),
            says(Colima, "macOS, Linux", COLIMA),
            says(
                RancherDesktop,
                "macOS, Windows, Linux",
                "https://docs.rancherdesktop.io/",
            ),
        ],
    },
];

#[cfg(test)]
mod tests;
