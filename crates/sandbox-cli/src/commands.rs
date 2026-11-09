//! 명령 목록. 이름과 설명의 정본이다. clap 정의(`cli`)는 설명을 여기서 가져오고,
//! 테스트가 두 목록이 같은지 확인한다.

/// 명령이 어느 코어를 부르는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// 관리 코어: 이미지와 캐시.
    Manage,
    /// 실행 코어: 샌드박스.
    Run,
}

impl Group {
    /// 도움말에 붙이는 이름.
    pub const fn label(self) -> &'static str {
        match self {
            Group::Manage => "관리",
            Group::Run => "실행",
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
    cmd("pull", Group::Manage, "이미지를 받는다 (받기만 한다)"),
    cmd(
        "prepare",
        Group::Manage,
        "이미지로 rootfs 디스크를 만든다 (이미 있으면 아무것도 하지 않는다)",
    ),
    cmd(
        "images",
        Group::Manage,
        "받아 둔 이미지와 준비된 rootfs 형식을 보여준다",
    ),
    cmd(
        "rmi",
        Group::Manage,
        "참조를 지우거나 이미지를 지운다 (쓰는 샌드박스가 있으면 거부, -f는 정지된 것만 무시)",
    ),
    cmd(
        "run",
        Group::Run,
        "샌드박스를 만들고 시작한다 (--pull=missing이 기본)",
    ),
    cmd("create", Group::Run, "샌드박스를 만든다"),
    cmd(
        "start",
        Group::Run,
        "샌드박스를 시작한다 (-a는 붙어서 끝날 때까지 기다린다)",
    ),
    cmd(
        "exec",
        Group::Run,
        "실행 중인 샌드박스 안에서 명령을 실행한다",
    ),
    cmd(
        "cp",
        Group::Run,
        "호스트와 샌드박스 사이에 파일을 복사한다 (샌드박스 쪽은 ID:경로)",
    ),
    cmd("stop", Group::Run, "샌드박스를 멈춘다"),
    cmd(
        "rm",
        Group::Run,
        "샌드박스를 지운다 (-f는 실행 중이면 멈추고 지운다)",
    ),
    cmd("ps", Group::Run, "샌드박스 목록을 보여준다"),
    cmd("inspect", Group::Run, "샌드박스 설정과 상태를 보여준다"),
    cmd(
        "logs",
        Group::Run,
        "detached로 시작한 샌드박스의 기본 프로세스 출력을 보여준다",
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
