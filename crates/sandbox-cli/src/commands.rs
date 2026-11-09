//! 명령 목록. `sbx help`는 이 표를 그대로 출력한다.

/// 명령이 어느 코어를 부르는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// 관리 코어: 이미지와 캐시.
    Manage,
    /// 실행 코어: 샌드박스.
    Run,
}

/// 명령 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// 이름.
    pub name: &'static str,
    /// 부르는 코어.
    pub group: Group,
    /// 사용법.
    pub usage: &'static str,
    /// 설명.
    pub summary: &'static str,
}

/// 모든 명령.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "pull",
        group: Group::Manage,
        usage: "sbx pull [--platform linux/ARCH] IMAGE",
        summary: "이미지를 받는다 (받기만 한다)",
    },
    Command {
        name: "prepare",
        group: Group::Manage,
        usage: "sbx prepare [--rootfs-format FORMAT] IMAGE",
        summary: "이미지로 rootfs 디스크를 만든다 (이미 있으면 아무것도 하지 않는다)",
    },
    Command {
        name: "images",
        group: Group::Manage,
        usage: "sbx images",
        summary: "받아 둔 이미지와 준비된 rootfs 형식을 보여준다",
    },
    Command {
        name: "rmi",
        group: Group::Manage,
        usage: "sbx rmi [-f] IMAGE",
        summary: "이미지를 지운다 (쓰는 샌드박스가 있으면 거부, -f는 정지된 것만 무시)",
    },
    Command {
        name: "run",
        group: Group::Run,
        usage: "sbx run [OPTIONS] IMAGE [CMD...]",
        summary: "샌드박스를 만들고 시작한다 (--pull=missing이 기본)",
    },
    Command {
        name: "create",
        group: Group::Run,
        usage: "sbx create [OPTIONS] IMAGE [CMD...]",
        summary: "샌드박스를 만든다",
    },
    Command {
        name: "start",
        group: Group::Run,
        usage: "sbx start ID",
        summary: "샌드박스를 시작한다",
    },
    Command {
        name: "exec",
        group: Group::Run,
        usage: "sbx exec [-i] [-t] [-e K=V] [-w DIR] [-u USER] ID CMD...",
        summary: "실행 중인 샌드박스 안에서 명령을 실행한다",
    },
    Command {
        name: "cp",
        group: Group::Run,
        usage: "sbx cp SRC DST   (샌드박스 쪽은 ID:경로)",
        summary: "호스트와 샌드박스 사이에 파일을 복사한다",
    },
    Command {
        name: "stop",
        group: Group::Run,
        usage: "sbx stop [-t SECONDS] ID",
        summary: "샌드박스를 멈춘다",
    },
    Command {
        name: "rm",
        group: Group::Run,
        usage: "sbx rm [-f] ID",
        summary: "샌드박스를 지운다 (-f는 실행 중이면 멈추고 지운다)",
    },
    Command {
        name: "ps",
        group: Group::Run,
        usage: "sbx ps [-a]",
        summary: "샌드박스 목록을 보여준다",
    },
    Command {
        name: "inspect",
        group: Group::Run,
        usage: "sbx inspect ID",
        summary: "샌드박스 설정과 상태를 보여준다",
    },
];

/// 이름으로 찾는다.
pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// `sbx help` 출력.
pub fn help_text() -> String {
    let mut out = String::from(
        "사용법: sbx [--data-dir DIR] [--cache-dir DIR] [--executor NAME] [--format json] COMMAND\n",
    );
    for (group, title) in [(Group::Manage, "관리"), (Group::Run, "실행")] {
        out.push_str(&format!("\n{title}:\n"));
        for c in COMMANDS.iter().filter(|c| c.group == group) {
            out.push_str(&format!(
                "  {:<9}{}\n           {}\n",
                c.name, c.summary, c.usage
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn help_lists_every_command() {
        let help = help_text();
        for c in COMMANDS {
            assert!(help.contains(c.usage));
        }
    }
}
