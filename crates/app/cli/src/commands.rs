//! 명령 목록. 이름, 설명, 도움말에서 묶이는 절의 정본이다. clap 정의(`cli`)는 설명을 여기서 가져오고,
//! `sbx --help`의 명령 목록은 [`root_help_template`]이 절마다 묶어 찍는다. 테스트가 두 목록이 같은지 확인한다.
//!
//! 목록의 순서가 곧 도움말의 순서다(clap 정의의 순서도 같아야 한다). 같은 절의 명령은 붙여 둔다.

use std::fmt::Write as _;

use clap::builder::styling::Styles;

/// `sbx --help`에서 명령을 묶는 절. 사람이 하려는 일로 묶는다(어느 코어를 부르는지가 아니다).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// 샌드박스를 만들고, 돌리고, 멈추고, 지우고, 본다.
    Sandboxes,
    /// 돌고 있는 샌드박스 안에서 일한다.
    Inside,
    /// 이미지.
    Images,
    /// 한 화면에서 보기, 이벤트, 에이전트.
    Watch,
    /// 처음 한 번: 실행기, 진단, 셸 연동.
    Setup,
}

impl Section {
    /// 도움말의 절 제목.
    pub const fn heading(self) -> &'static str {
        match self {
            Section::Sandboxes => "Sandbox Commands",
            Section::Inside => "Work Inside a Sandbox",
            Section::Images => "Image Commands",
            Section::Watch => "Watch and Automate",
            Section::Setup => "Setup",
        }
    }
}

/// 절 순서.
pub const SECTIONS: [Section; 5] = [
    Section::Sandboxes,
    Section::Inside,
    Section::Images,
    Section::Watch,
    Section::Setup,
];

/// 명령 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// 이름.
    pub name: &'static str,
    /// 도움말의 절.
    pub section: Section,
    /// 설명.
    pub summary: &'static str,
}

const fn cmd(name: &'static str, section: Section, summary: &'static str) -> Command {
    Command {
        name,
        section,
        summary,
    }
}

/// 모든 명령.
pub const COMMANDS: &[Command] = &[
    cmd(
        "run",
        Section::Sandboxes,
        "Create and start a sandbox (pulls missing images by default)",
    ),
    cmd("create", Section::Sandboxes, "Create a sandbox"),
    cmd(
        "start",
        Section::Sandboxes,
        "Start a sandbox (-a attaches and waits for it to exit)",
    ),
    cmd(
        "stop",
        Section::Sandboxes,
        "Stop sandboxes (signal the main process, power off after -t seconds)",
    ),
    cmd(
        "restart",
        Section::Sandboxes,
        "Stop and start sandboxes again",
    ),
    cmd(
        "kill",
        Section::Sandboxes,
        "Send a signal to the main process (default KILL)",
    ),
    cmd(
        "wait",
        Section::Sandboxes,
        "Wait for sandboxes to stop and print their exit codes",
    ),
    cmd(
        "rm",
        Section::Sandboxes,
        "Remove sandboxes (-f stops running ones first)",
    ),
    cmd("ps", Section::Sandboxes, "List sandboxes"),
    cmd(
        "inspect",
        Section::Sandboxes,
        "Show a sandbox's settings and state",
    ),
    cmd(
        "stats",
        Section::Sandboxes,
        "Show running sandboxes' CPU, memory, disk and process count",
    ),
    cmd(
        "exec",
        Section::Inside,
        "Run a command in a running sandbox",
    ),
    cmd(
        "attach",
        Section::Inside,
        "Attach to the main process of a detached sandbox (detach with Ctrl-P Ctrl-Q)",
    ),
    cmd(
        "logs",
        Section::Inside,
        "Show the main process output of a detached sandbox",
    ),
    cmd(
        "cp",
        Section::Inside,
        "Copy files between the host and a sandbox (SANDBOX:PATH on the sandbox side)",
    ),
    cmd(
        "pull",
        Section::Images,
        "Download an image (only downloads)",
    ),
    cmd(
        "prepare",
        Section::Images,
        "Build the root filesystem disk for an image (no-op if it exists)",
    ),
    cmd(
        "images",
        Section::Images,
        "List downloaded images and their prepared root filesystems",
    ),
    cmd(
        "rmi",
        Section::Images,
        "Remove image references or images (refused while in use; -f ignores stopped sandboxes)",
    ),
    cmd(
        "load",
        Section::Images,
        "Add images from an OCI archive or directory (docker save, podman save)",
    ),
    cmd(
        "save",
        Section::Images,
        "Write an image as an OCI archive that sbx, docker and podman can load",
    ),
    cmd(
        "login",
        Section::Images,
        "Save credentials for a registry (Docker's config.json or its credential helper)",
    ),
    cmd(
        "logout",
        Section::Images,
        "Remove saved credentials for a registry",
    ),
    cmd(
        "tui",
        Section::Watch,
        "Watch and manage sandboxes, images, events and executors on one screen",
    ),
    cmd(
        "events",
        Section::Watch,
        "Stream sandbox, image and network events (--since replays recent ones)",
    ),
    cmd(
        "mcp",
        Section::Watch,
        "Serve sandboxes to AI agents over the Model Context Protocol (stdio)",
    ),
    cmd(
        "executor",
        Section::Setup,
        "Install, choose and switch executors, adapters and repositories",
    ),
    cmd(
        "info",
        Section::Setup,
        "Show the host, executor and storage sbx uses (include it in bug reports)",
    ),
    cmd(
        "system",
        Section::Setup,
        "Show disk usage and remove what nothing uses (df, prune)",
    ),
    cmd(
        "completion",
        Section::Setup,
        "Print a shell completion script (bash, zsh, fish, powershell, elvish)",
    ),
];

/// 이름으로 찾는다.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// clap 도움말에 쓰는 설명.
///
/// 목록에 없는 이름이면 패닉한다. 테스트가 모든 clap 명령이 목록에 있는지 확인한다.
pub fn about(name: &str) -> &'static str {
    find(name)
        .unwrap_or_else(|| panic!("command {name} is missing from COMMANDS"))
        .summary
}

/// `sbx --help`의 맨 끝. 처음 쓰는 사람이 할 일을 순서대로(특정 실행기를 권하지 않는다).
const ROOT_AFTER_HELP: &[(&str, &str)] = &[
    ("sbx executor search", "Find an executor to install"),
    ("sbx run -it alpine sh", "Open a shell in a new sandbox"),
    ("sbx tui", "Watch and manage everything on one screen"),
];

/// `sbx --help`의 틀(clap `help_template`). clap은 하위 명령을 한 목록으로만 찍으므로 명령 목록은 여기서
/// [`SECTIONS`]마다 묶어 쓰고, 옵션은 clap의 `{options}`에 맡긴다. 제목과 이름은 clap의 기본 모양(굵게, 밑줄)을 쓴다:
/// 터미널이 아니거나 `NO_COLOR`면 clap이 지운다.
pub fn root_help_template() -> String {
    let styles = Styles::default();
    let (h, lit) = (styles.get_header(), styles.get_literal());
    let width = COMMANDS.iter().map(|c| c.name.len()).max().unwrap_or(0) + 2;
    let mut s = String::from("{about-with-newline}\n{usage-heading} {usage}\n");
    for section in SECTIONS {
        let _ = write!(s, "\n{h}{}:{h:#}\n", section.heading());
        for c in COMMANDS.iter().filter(|c| c.section == section) {
            let pad = width - c.name.len();
            let _ = writeln!(s, "  {lit}{}{lit:#}{:pad$}{}", c.name, "", c.summary);
        }
    }
    let _ = write!(
        s,
        "\n{h}Options:{h:#}\n{{options}}\n\n{h}Get Started:{h:#}\n"
    );
    let w = ROOT_AFTER_HELP
        .iter()
        .map(|(c, _)| c.len())
        .max()
        .unwrap_or(0)
        + 2;
    for (c, what) in ROOT_AFTER_HELP {
        let pad = w - c.len();
        let _ = writeln!(s, "  {lit}{c}{lit:#}{:pad$}{what}", "");
    }
    s.push_str("\nRun 'sbx <COMMAND> --help' for more on a command.");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn names_are_unique() {
        for (i, a) in COMMANDS.iter().enumerate() {
            assert!(
                COMMANDS[i + 1..].iter().all(|b| b.name != a.name),
                "{}",
                a.name
            );
        }
    }

    #[test]
    fn clap_and_table_list_the_same_commands() {
        let cli = crate::cli::Cli::command();
        let clap_names: Vec<&str> = cli.get_subcommands().map(|c| c.get_name()).collect();
        let table_names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(clap_names, table_names);
        for sub in cli.get_subcommands() {
            assert_eq!(
                sub.get_about().map(|a| a.to_string()).as_deref(),
                Some(about(sub.get_name()))
            );
        }
    }

    #[test]
    fn each_section_is_one_run_in_the_table() {
        // 도움말의 절 순서와 clap 정의의 순서가 같으려면 같은 절의 명령이 붙어 있어야 한다.
        let mut seen: Vec<Section> = Vec::new();
        for c in COMMANDS {
            if seen.last() != Some(&c.section) {
                assert!(
                    !seen.contains(&c.section),
                    "{} is apart from its section",
                    c.name
                );
                seen.push(c.section);
            }
        }
        assert_eq!(seen, SECTIONS);
    }

    #[test]
    fn root_help_lists_every_command_under_its_section() {
        let help = crate::cli::Cli::command().render_help().to_string();
        let mut last = 0;
        for section in SECTIONS {
            let at = help
                .find(&format!("{}:", section.heading()))
                .unwrap_or_else(|| panic!("{help}"));
            assert!(at > last, "{help}");
            last = at;
        }
        for c in COMMANDS {
            let line = help
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("{} ", c.name)))
                .unwrap_or_else(|| panic!("{}\n{help}", c.name));
            assert!(line.ends_with(c.summary), "{line}");
        }
        // 한 목록(clap의 Commands:)도, 옛 꼬리표도 없다.
        assert!(
            !help.contains("\nCommands:") && !help.contains("[setup]"),
            "{help}"
        );
        assert!(
            help.contains("--data-dir") && help.contains("Get Started:"),
            "{help}"
        );
    }
}
