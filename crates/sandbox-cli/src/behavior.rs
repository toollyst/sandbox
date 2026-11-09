//! CLI 동작 규칙. Docker CLI를 기준으로 하고, 다른 점은 [`DOCKER_DIFFERENCES`]에 적는다.
//!
//! 여기 있는 함수는 입출력 없이 "무엇을 어떤 순서로 부를지"만 정한다. 테스트가 곧 규칙이다.

use std::fmt;
use std::str::FromStr;

use std::collections::BTreeMap;

use sandbox_model::{ImageId, ParseError, RootfsFormat, SandboxId};

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

/// `--rootfs-format`을 주지 않았을 때의 형식.
pub const DEFAULT_ROOTFS_FORMAT: RootfsFormat = RootfsFormat::Erofs;

/// `--cpus`를 주지 않았을 때의 vCPU 개수.
pub const DEFAULT_VCPUS: u8 = 1;

/// `--memory`를 주지 않았을 때의 메모리(MiB).
pub const DEFAULT_MEMORY_MIB: u32 = 512;

/// 샌드박스별 쓰기 가능 디스크 크기(MiB).
pub const DEFAULT_SCRATCH_MIB: u32 = 1024;

/// 이미지 ID 앞부분으로 찾을 때 필요한 최소 길이. `images`의 IMAGE ID 열과 같은 12자리다.
/// 짧은 문자열이 우연히 ID 앞부분과 맞아 엉뚱한 이미지를 지우는 일을 막는다.
pub const MIN_ID_PREFIX: usize = 12;

/// 이미지 이름이 무엇으로 맞았는지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageMatch {
    /// 참조(태그 또는 digest 참조)로 맞음.
    Ref {
        /// 정규화된 참조.
        reference: String,
        /// 이미지.
        id: ImageId,
    },
    /// 이미지 ID(전체 또는 앞부분)로 맞음.
    Id(ImageId),
}

impl ImageMatch {
    /// 이미지 ID.
    pub fn id(&self) -> &ImageId {
        match self {
            ImageMatch::Ref { id, .. } | ImageMatch::Id(id) => id,
        }
    }
}

/// 이미지를 찾지 못한 이유.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// 없음.
    NotFound(String),
    /// ID 앞부분이 여러 이미지와 맞음.
    Ambiguous(String),
}

impl std::error::Error for LookupError {}

impl fmt::Display for LookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LookupError::NotFound(n) => write!(f, "no such image: {n}"),
            LookupError::Ambiguous(n) => write!(f, "image ID prefix {n} matches several images"),
        }
    }
}

/// 이미지 이름을 찾는다. Docker와 같은 순서다.
///
/// 1. 참조로 해석해 태그 기록에서 찾는다(`normalized`는 정규화된 참조, 해석할 수 없으면 `None`).
/// 2. `sha256:`으로 시작하거나 hex만으로 된 이름은 이미지 ID로 본다. 전체 또는
///    [`MIN_ID_PREFIX`]자리 이상의 앞부분이 정확히 하나와 맞아야 한다.
pub fn match_image(
    name: &str,
    normalized: Option<&str>,
    refs: &BTreeMap<String, ImageId>,
    ids: &[ImageId],
) -> Result<ImageMatch, LookupError> {
    if let Some((reference, id)) = normalized.and_then(|n| refs.get_key_value(n)) {
        return Ok(ImageMatch::Ref {
            reference: reference.clone(),
            id: id.clone(),
        });
    }
    let hex = name.strip_prefix("sha256:").unwrap_or(name);
    let is_hex = !hex.is_empty()
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if is_hex && hex.len() >= MIN_ID_PREFIX {
        let found: Vec<&ImageId> = ids
            .iter()
            .filter(|id| id.digest().hex().starts_with(hex))
            .collect();
        return match found.as_slice() {
            [one] => Ok(ImageMatch::Id((*one).clone())),
            [] => Err(LookupError::NotFound(name.to_owned())),
            _ => Err(LookupError::Ambiguous(name.to_owned())),
        };
    }
    Err(LookupError::NotFound(name.to_owned()))
}

/// `rmi` 이름 하나에 대해 할 일.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RmiAction {
    /// 참조만 지운다. 이미지는 남는다.
    Untag(String),
    /// 이미지를 지운다. [`rmi_decision`]의 보호 규칙을 거친다.
    Delete(ImageId),
    /// ID로 지정했는데 참조가 여럿이다. `-f`가 필요하다.
    RefuseMultipleRefs(ImageId),
}

/// `rmi` 태그 규칙. Docker `rmi`와 같다.
///
/// | 이름이 맞은 방식 | 이미지를 가리키는 참조 수 | 기본 | `-f` |
/// |---|---|---|---|
/// | 참조 | 2개 이상 | 그 참조만 지움 | 같음 |
/// | 참조 | 1개 | 이미지 삭제 | 같음 |
/// | ID | 1개 이하 | 이미지 삭제 | 같음 |
/// | ID | 2개 이상 | 거부 | 이미지 삭제 |
///
/// 이미지 삭제는 [`rmi_decision`]의 샌드박스 보호 규칙을 거친다.
pub fn rmi_action(m: &ImageMatch, ref_count: usize, force: bool) -> RmiAction {
    match m {
        ImageMatch::Ref { reference, .. } if ref_count >= 2 => RmiAction::Untag(reference.clone()),
        ImageMatch::Ref { id, .. } => RmiAction::Delete(id.clone()),
        ImageMatch::Id(id) if ref_count >= 2 && !force => RmiAction::RefuseMultipleRefs(id.clone()),
        ImageMatch::Id(id) => RmiAction::Delete(id.clone()),
    }
}

/// Docker와 다른 점: (항목, 이 프로젝트의 동작, 이유).
pub const DOCKER_DIFFERENCES: &[(&str, &str, &str)] = &[
    (
        "--network 기본값",
        "none (Docker는 bridge). 지금은 none만 받는다",
        "기획서 원칙: 네트워크는 기본 차단, 허용한 것만 연다",
    ),
    (
        "--cpus, --memory 기본값",
        "1 vCPU, 512 MiB (Docker는 제한 없음). --cpus는 정수만 받는다",
        "VM은 크기를 정해서 띄워야 한다",
    ),
    (
        "이미지 ID 앞부분으로 찾기",
        "12자리 이상만 받는다 (Docker는 더 짧아도 받는다)",
        "짧은 문자열이 우연히 맞아 엉뚱한 이미지를 지우는 일을 막는다",
    ),
    (
        "sbx prepare",
        "Docker에 없는 명령",
        "이미지를 VM용 rootfs 디스크로 만드는 단계가 따로 있다",
    ),
    (
        "--rootfs-format",
        "Docker에 없는 옵션. 생략하면 erofs ([`DEFAULT_ROOTFS_FORMAT`])",
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
    fn id(c: char) -> ImageId {
        ImageId::new(
            sandbox_model::Digest::parse(&format!("sha256:{}", c.to_string().repeat(64))).unwrap(),
        )
    }

    #[test]
    fn match_image_order() {
        let mut refs = BTreeMap::new();
        refs.insert("docker.io/library/alpine:latest".to_string(), id('a'));
        let ids = [id('a'), id('b')];
        assert_eq!(
            match_image(
                "alpine",
                Some("docker.io/library/alpine:latest"),
                &refs,
                &ids
            ),
            Ok(ImageMatch::Ref {
                reference: "docker.io/library/alpine:latest".into(),
                id: id('a')
            })
        );
        assert_eq!(
            match_image("bbbbbbbbbbbb", None, &refs, &ids),
            Ok(ImageMatch::Id(id('b')))
        );
        assert_eq!(
            match_image(&format!("sha256:{}", "b".repeat(64)), None, &refs, &ids),
            Ok(ImageMatch::Id(id('b')))
        );
        // 12자리보다 짧은 앞부분은 받지 않는다.
        assert!(matches!(
            match_image("bbbb", None, &refs, &ids),
            Err(LookupError::NotFound(_))
        ));
        assert!(matches!(
            match_image(
                "ubuntu",
                Some("docker.io/library/ubuntu:latest"),
                &refs,
                &ids
            ),
            Err(LookupError::NotFound(_))
        ));
    }

    #[test]
    fn ambiguous_id_prefix() {
        let x = ImageId::new(
            sandbox_model::Digest::parse(&format!("sha256:{}{}", "c".repeat(12), "0".repeat(52)))
                .unwrap(),
        );
        let y = ImageId::new(
            sandbox_model::Digest::parse(&format!("sha256:{}{}", "c".repeat(12), "1".repeat(52)))
                .unwrap(),
        );
        assert!(matches!(
            match_image(&"c".repeat(12), None, &BTreeMap::new(), &[x, y]),
            Err(LookupError::Ambiguous(_))
        ));
    }

    #[test]
    fn rmi_action_table() {
        let by_ref = ImageMatch::Ref {
            reference: "r".into(),
            id: id('a'),
        };
        let by_id = ImageMatch::Id(id('a'));
        assert_eq!(rmi_action(&by_ref, 2, false), RmiAction::Untag("r".into()));
        assert_eq!(rmi_action(&by_ref, 2, true), RmiAction::Untag("r".into()));
        assert_eq!(rmi_action(&by_ref, 1, false), RmiAction::Delete(id('a')));
        assert_eq!(rmi_action(&by_id, 1, false), RmiAction::Delete(id('a')));
        assert_eq!(rmi_action(&by_id, 0, false), RmiAction::Delete(id('a')));
        assert_eq!(
            rmi_action(&by_id, 2, false),
            RmiAction::RefuseMultipleRefs(id('a'))
        );
        assert_eq!(rmi_action(&by_id, 2, true), RmiAction::Delete(id('a')));
    }
}
