//! CLI 동작 규칙. Docker CLI를 기준으로 하고, 다른 점은 [`DOCKER_DIFFERENCES`]에 적는다.
//!
//! 여기 있는 함수는 입출력 없이 "무엇을 어떤 순서로 부를지"만 정한다. 테스트가 곧 규칙이다.

use std::fmt;
use std::str::FromStr;

use sandbox_model::{ParseError, SandboxId};

/// `run`과 `create`의 `--pull`. Docker와 같은 값과 같은 기본값(`missing`)이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullPolicy {
    /// 이미지가 없을 때만 받는다.
    #[default]
    Missing,
    /// 매번 받는다. digest가 같으면 내려받지 않는다.
    Always,
    /// 받지 않는다. 없으면 오류.
    Never,
}

impl FromStr for PullPolicy {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "missing" => Ok(PullPolicy::Missing),
            "always" => Ok(PullPolicy::Always),
            "never" => Ok(PullPolicy::Never),
            _ => Err(ParseError::new("pull policy", s)),
        }
    }
}

/// `run`과 `create`가 차례로 부르는 코어 동작.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// `Manager::pull`.
    Pull,
    /// `Manager::prepare`. 이미 있으면 아무것도 하지 않는다.
    Prepare,
    /// `Runtime::create`.
    Create,
}

/// `--pull=never`인데 준비가 안 된 경우.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReady {
    /// 이미지가 없음.
    Image,
    /// 이미지는 있지만 그 형식의 rootfs가 없음.
    Rootfs,
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotReady::Image => {
                f.write_str("image is not pulled (use `sbx pull` or --pull=missing)")
            }
            NotReady::Rootfs => {
                f.write_str("rootfs is not prepared (use `sbx prepare` or --pull=missing)")
            }
        }
    }
}

/// `run`과 `create`가 부를 동작 순서.
///
/// | `--pull` | 이미지 없음 | 이미지만 있음 | 둘 다 있음 |
/// |---|---|---|---|
/// | missing | pull, prepare, create | prepare, create | create |
/// | always | pull, prepare, create | pull, prepare, create | pull, prepare, create |
/// | never | 오류(이미지) | 오류(rootfs) | create |
pub fn create_steps(
    policy: PullPolicy,
    image_present: bool,
    rootfs_present: bool,
) -> Result<Vec<Step>, NotReady> {
    use Step::*;
    Ok(match (policy, image_present, rootfs_present) {
        (PullPolicy::Always, _, _) | (PullPolicy::Missing, false, _) => vec![Pull, Prepare, Create],
        (PullPolicy::Missing, true, false) => vec![Prepare, Create],
        (PullPolicy::Missing, true, true) | (PullPolicy::Never, true, true) => vec![Create],
        (PullPolicy::Never, false, _) => return Err(NotReady::Image),
        (PullPolicy::Never, true, false) => return Err(NotReady::Rootfs),
    })
}

/// `rmi`의 판단.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmiDecision {
    /// `Manager::remove`를 부른다. 다른 data_dir의 실행 중 샌드박스는 그 안의 잠금이 막는다.
    Remove,
    /// 실행 중인 샌드박스가 쓴다. `-f`로도 지울 수 없다.
    RefuseRunning(Vec<SandboxId>),
    /// 정지된 샌드박스가 쓴다. `-f`로 지울 수 있다.
    RefuseStopped(Vec<SandboxId>),
}

/// `rmi` 보호 규칙. Docker `rmi`와 같다.
///
/// | 이미지를 쓰는 샌드박스 (같은 data_dir) | 기본 | `-f` |
/// |---|---|---|
/// | 없음 | 삭제 | 삭제 |
/// | 정지된 샌드박스 | 거부 | 삭제 |
/// | 실행 중인 샌드박스 | 거부 | 거부 |
///
/// 다른 data_dir의 정지된 샌드박스는 알 수 없으므로 보호 범위 밖이다.
/// 강제로 지운 이미지를 쓰던 샌드박스를 `start`하면 이미지 없음 오류가 난다.
pub fn rmi_decision(running: Vec<SandboxId>, stopped: Vec<SandboxId>, force: bool) -> RmiDecision {
    if !running.is_empty() {
        RmiDecision::RefuseRunning(running)
    } else if !stopped.is_empty() && !force {
        RmiDecision::RefuseStopped(stopped)
    } else {
        RmiDecision::Remove
    }
}

/// Docker와 다른 점: (항목, 이 프로젝트의 동작, 이유).
pub const DOCKER_DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "sbx prepare",
        "Docker에 없는 명령",
        "이미지를 VM용 rootfs 디스크로 만드는 단계가 따로 있다",
    ),
    (
        "--rootfs-format",
        "Docker에 없는 옵션. 생략하면 CLI 설정의 기본값(처음에는 erofs)",
        "rootfs 형식을 여러 개 지원한다",
    ),
    (
        "--mount type=copy",
        "Docker에 없는 마운트 종류",
        "시작할 때 복사해 넣고 결과는 cp로 꺼내는 공유 방식",
    ),
    (
        "-v, --mount type=bind",
        "실행기가 실시간 공유를 지원하지 않으면 오류",
        "실행기마다 기능이 다르다. 다른 방식으로 바꾸지 않는다",
    ),
    (
        "rmi의 정지 샌드박스 보호",
        "같은 data_dir 안에서만",
        "다른 data_dir은 알 수 없다",
    ),
    (
        "stopped 상태",
        "Docker처럼 다시 start할 수 있다 (OCI Runtime Spec과 다름)",
        "CLI가 Docker를 기준으로 한다",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use Step::*;

    #[test]
    fn pull_policy_default_matches_docker() {
        assert_eq!(PullPolicy::default(), PullPolicy::Missing);
        assert_eq!("never".parse::<PullPolicy>().unwrap(), PullPolicy::Never);
        assert!("sometimes".parse::<PullPolicy>().is_err());
    }

    #[test]
    fn create_steps_table() {
        use PullPolicy::*;
        let cases = [
            (Missing, false, false, Ok(vec![Pull, Prepare, Create])),
            (Missing, true, false, Ok(vec![Prepare, Create])),
            (Missing, true, true, Ok(vec![Create])),
            (Always, false, false, Ok(vec![Pull, Prepare, Create])),
            (Always, true, false, Ok(vec![Pull, Prepare, Create])),
            (Always, true, true, Ok(vec![Pull, Prepare, Create])),
            (Never, false, false, Err(NotReady::Image)),
            (Never, true, false, Err(NotReady::Rootfs)),
            (Never, true, true, Ok(vec![Create])),
        ];
        for (policy, image, rootfs, expected) in cases {
            assert_eq!(
                create_steps(policy, image, rootfs),
                expected,
                "{policy:?} image={image} rootfs={rootfs}"
            );
        }
    }

    #[test]
    fn rmi_table() {
        let id = || vec![SandboxId::new("a").unwrap()];
        assert_eq!(rmi_decision(vec![], vec![], false), RmiDecision::Remove);
        assert_eq!(rmi_decision(vec![], vec![], true), RmiDecision::Remove);
        assert_eq!(
            rmi_decision(vec![], id(), false),
            RmiDecision::RefuseStopped(id())
        );
        assert_eq!(rmi_decision(vec![], id(), true), RmiDecision::Remove);
        assert_eq!(
            rmi_decision(id(), vec![], false),
            RmiDecision::RefuseRunning(id())
        );
        assert_eq!(
            rmi_decision(id(), vec![], true),
            RmiDecision::RefuseRunning(id())
        );
    }
}
