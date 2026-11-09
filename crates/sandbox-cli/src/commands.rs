//! 명령 목록. 이름과 설명의 정본이다. clap 정의(`cli`)는 설명을 여기서 가져오고,
//! 테스트가 두 목록이 같은지 확인한다.

/// 명령이 어느 코어를 부르는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// 관리 코어: 이미지와 캐시.
    Manage,
    /// 실행 코어: 샌드박스.
    Run,
    /// 위 명령들을 AI 에이전트용 규약으로 내보낸다(정책 없이).
    Agent,
}

impl Group {
    /// 도움말에 붙이는 이름.
    pub const fn label(self) -> &'static str {
        match self {
            Group::Manage => "setup",
            Group::Run => "sandbox",
            Group::Agent => "agent",
        }
    }
}

/// 명령 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// 이름.
    pub name: &'static str,
    /// 부르는 코어.
    pub group: Group,
    /// 설명.
    pub summary: &'static str,
}

const fn cmd(name: &'static str, group: Group, summary: &'static str) -> Command {
    Command {
        name,
        group,
        summary,
    }
}

/// 모든 명령.
pub const COMMANDS: &[Command] = &[
    cmd("pull", Group::Manage, "Download an image (only downloads)"),
    cmd(
        "prepare",
        Group::Manage,
        "Build the root filesystem disk for an image (no-op if it exists)",
    ),
    cmd(
        "images",
        Group::Manage,
        "List downloaded images and their prepared root filesystems",
    ),
    cmd(
        "rmi",
        Group::Manage,
        "Remove image references or images (refused while in use; -f ignores stopped sandboxes)",
    ),
    cmd(
        "executor",
        Group::Manage,
        "Install and choose executors (ls, install, search, use, rm, repo)",
    ),
    cmd(
        "run",
        Group::Run,
        "Create and start a sandbox (pulls missing images by default)",
    ),
    cmd("create", Group::Run, "Create a sandbox"),
    cmd(
        "start",
        Group::Run,
        "Start a sandbox (-a attaches and waits for it to exit)",
    ),
    cmd("exec", Group::Run, "Run a command in a running sandbox"),
    cmd(
        "cp",
        Group::Run,
        "Copy files between the host and a sandbox (ID:PATH on the sandbox side)",
    ),
    cmd(
        "stop",
        Group::Run,
        "Stop sandboxes (signal the main process, power off after -t seconds)",
    ),
    cmd("restart", Group::Run, "Stop and start sandboxes again"),
    cmd(
        "kill",
        Group::Run,
        "Send a signal to the main process (default KILL)",
    ),
    cmd(
        "wait",
        Group::Run,
        "Wait for sandboxes to stop and print their exit codes",
    ),
    cmd(
        "rm",
        Group::Run,
        "Remove sandboxes (-f stops running ones first)",
    ),
    cmd(
        "attach",
        Group::Run,
        "Attach to the main process of a detached sandbox (detach with Ctrl-P Ctrl-Q)",
    ),
    cmd("ps", Group::Run, "List sandboxes"),
    cmd("inspect", Group::Run, "Show a sandbox's settings and state"),
    cmd(
        "logs",
        Group::Run,
        "Show the main process output of a detached sandbox",
    ),
    cmd(
        "mcp",
        Group::Agent,
        "Serve sandboxes to AI agents over the Model Context Protocol (stdio)",
    ),
];

/// 이름으로 찾는다.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// clap 도움말에 쓰는 설명: `[그룹] 설명`.
///
/// 목록에 없는 이름이면 패닉한다. 테스트가 모든 clap 명령이 목록에 있는지 확인한다.
pub fn about(name: &str) -> String {
    let c = find(name).unwrap_or_else(|| panic!("command {name} is missing from COMMANDS"));
    format!("[{}] {}", c.group.label(), c.summary)
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
                sub.get_about().map(|a| a.to_string()),
                Some(about(sub.get_name()))
            );
        }
    }
}
