//! 빌드 계획: 빌드 정의(Dockerfile)를 해석한 결과. 빌더가 실행할 스테이지와 단계의 목록이다.
//!
//! 문법, 변수 풀기, `ARG` 범위, `--target`으로 스테이지 고르기는 해석하는 쪽(`sbx-dockerfile`)이 끝낸다.
//! 여기 값은 모두 풀린 값이고, 빌더는 Dockerfile 문법을 모른다. 빌더에게 남은 일은 각 자료형의 문서에 적는다.
//!
//! - 스테이지는 원래 순서를 지킨다. 스테이지가 기대는 스테이지(`FROM 스테이지`, `COPY --from`)는 언제나 앞에 있다.
//! - 마지막 스테이지가 결과 이미지다([`BuildPlan::output`]).
//! - 단계의 `text`는 진행 표시(`[2/7] RUN npm ci`)와 캐시 키에 쓰는 풀린 명령 문자열이다. 같은 단계는 같은 문자열이다.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::Arch;

/// `PATH`가 없는 바탕(`FROM scratch`)에서 시작하는 값. BuildKit과 같다.
///
/// `scratch` 스테이지의 config는 `PATH=`이 값 하나로 시작한다. 빌더도 config에 이 값을 쓴다.
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// 해석에 넘기는 값. `sbx build`의 옵션에 대응한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildOptions {
    /// `--build-arg`. 같은 이름이 여러 번이면 마지막 값이다.
    pub build_args: Vec<(String, String)>,
    /// `--target`. 없으면 마지막 스테이지다.
    pub target: Option<String>,
    /// 만들 이미지의 아키텍처. 언제나 호스트 아키텍처다(다른 아키텍처는 에뮬레이션이 필요하다).
    pub arch: Arch,
}

/// 빌드 계획.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildPlan {
    /// 결과에 필요한 스테이지만, 원래 순서로. 비어 있지 않다.
    pub stages: Vec<Stage>,
    /// 빌드는 하지만 호출자가 사용자에게 보여야 하는 것(쓰지 않는 지시어, 적용하지 않는 명령).
    pub warnings: Vec<PlanWarning>,
}

impl BuildPlan {
    /// 결과 이미지가 되는 스테이지(마지막 스테이지).
    ///
    /// # Panics
    ///
    /// `stages`가 비어 있으면. 해석기가 만든 계획은 비어 있지 않다.
    pub fn output(&self) -> &Stage {
        self.stages
            .last()
            .expect("a build plan has at least one stage")
    }
}

/// 스테이지 하나. `FROM`에서 다음 `FROM` 앞까지다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    /// `AS` 이름(소문자).
    pub name: Option<String>,
    /// Dockerfile 안에서 몇 번째 스테이지인지(0부터). 빠진 스테이지가 있어도 원래 번호다.
    pub position: usize,
    /// `FROM`이 시작하는 줄(1부터).
    pub line: usize,
    /// 풀린 `FROM` 명령(`FROM node:22 AS build`).
    pub text: String,
    /// 바탕.
    pub base: StageBase,
    /// 단계들. 순서대로 실행한다.
    pub steps: Vec<Step>,
}

/// 스테이지의 바탕.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageBase {
    /// 이미지 참조(풀린 문자열 그대로). 참조 해석과 받기는 빌더가 한다.
    Image(String),
    /// 앞 스테이지(`BuildPlan::stages`의 번호). 그 스테이지의 결과(층과 config)에서 시작한다.
    Stage(usize),
    /// 빈 바탕. config는 `PATH=`[`DEFAULT_PATH`] 하나로 시작한다.
    Scratch,
}

/// 단계 하나. Dockerfile의 명령 하나다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// 명령이 시작하는 줄(1부터).
    pub line: usize,
    /// 풀린 명령 문자열(`RUN npm ci`, `COPY --chown=node:node . /app`).
    pub text: String,
    /// 무엇을 하는지.
    pub kind: StepKind,
}

/// 단계가 하는 일.
///
/// `Run`과 `Copy`는 층을 만든다. `Workdir`는 디렉터리가 없으면 만든다(BuildKit처럼 층이 생긴다).
/// 나머지는 이미지 config만 바꾼다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// `RUN`.
    Run(RunStep),
    /// `COPY`, `ADD`.
    Copy(CopyStep),
    /// `ENV`. 순서대로 config `Env`에 넣는다. 같은 이름은 그 자리에서 바꾼다.
    Env(Vec<(String, String)>),
    /// `WORKDIR`. 상대 경로면 지금 작업 디렉터리(바탕 이미지의 값일 수 있다)에 잇는 것은 빌더가 한다.
    Workdir(String),
    /// `USER`. config `User`에 그대로 쓴다.
    User(String),
    /// `LABEL`. config `Labels`에 더한다.
    Label(Vec<(String, String)>),
    /// `EXPOSE`. `80/tcp` 꼴로 고르고 범위는 펼친 값이다. config `ExposedPorts`에 더한다.
    Expose(Vec<String>),
    /// `VOLUME`. config `Volumes`에 더한다.
    Volume(Vec<String>),
    /// `ENTRYPOINT`. config `Entrypoint`를 바꾼다.
    ///
    /// 이 스테이지에서 앞서 `CMD`가 없었으면 바탕에서 물려받은 `Cmd`를 비운다(Docker와 같다). 이것은 빌더가 한다.
    Entrypoint(Command),
    /// `CMD`. config `Cmd`를 바꾼다.
    Cmd(Command),
    /// `STOPSIGNAL`. [`Signal`](crate::Signal)로 읽히는 것을 확인한 원문이다.
    StopSignal(String),
    /// `SHELL`. 뒤따르는 셸 형식 명령에 이미 반영되어 있다. config의 `Shell`(Docker 확장)에 쓸지는 빌더가 정한다.
    Shell(Vec<String>),
    /// `HEALTHCHECK`. config `Healthcheck`에 쓴다. sbx는 검사를 돌리지 않는다(계획의 경고에 있다).
    Healthcheck(Healthcheck),
    /// `MAINTAINER`(Docker가 폐기한 명령). config `Author`에 쓴다.
    Maintainer(String),
}

/// 실행할 명령.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    /// exec 형식(`["npm", "ci"]`). 그대로 argv다.
    Exec(Vec<String>),
    /// 셸 형식(`npm ci`). 그때의 `SHELL`(기본 `["/bin/sh", "-c"]`)에 문자열을 붙여 실행한다.
    Shell {
        /// 그때의 `SHELL`.
        shell: Vec<String>,
        /// 명령 문자열. 변수를 풀지 않은 원문이다(셸이 푼다).
        script: String,
    },
}

impl Command {
    /// 실행할 argv. config `Cmd`·`Entrypoint`에도 이 값을 쓴다.
    pub fn argv(&self) -> Vec<String> {
        match self {
            Command::Exec(args) => args.clone(),
            Command::Shell { shell, script } => {
                let mut v = shell.clone();
                v.push(script.clone());
                v
            }
        }
    }
}

/// `RUN` 단계.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStep {
    /// 실행할 명령.
    pub command: Command,
    /// 그 자리에서 값이 있는 `ARG`들. 실행 환경에 config `Env`와 함께 넣는다. 이미지에는 남기지 않는다.
    ///
    /// `ENV`(바탕 이미지의 것 포함)에 같은 이름이 있는 것은 빠져 있다. `ENV`가 이긴다(Docker와 같다).
    /// 캐시 키에 넣는다.
    pub args: Vec<(String, String)>,
}

/// `COPY`인지 `ADD`인지.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyKind {
    /// `COPY`.
    Copy,
    /// `ADD`. 로컬 파일만이다. 원본이 컨텍스트의 tar(압축 포함)면 대상에 푼다(Docker와 같다). 푸는 것은 빌더가 한다.
    Add,
}

/// 복사할 파일이 있는 곳.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopySource {
    /// 빌드 컨텍스트. `.dockerignore`를 적용한다.
    Context,
    /// 앞 스테이지(`BuildPlan::stages`의 번호)의 결과.
    Stage(usize),
    /// 이미지(`COPY --from=alpine:3`). 참조 해석과 받기는 빌더가 한다.
    Image(String),
}

/// `COPY`, `ADD` 단계.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyStep {
    /// `COPY`인지 `ADD`인지.
    pub kind: CopyKind,
    /// 원본이 있는 곳(`--from`).
    pub from: CopySource,
    /// 원본 경로들. 하나 이상이다. 와일드카드(`*`, `?`, `[...]`)를 풀고, 원본(컨텍스트나 스테이지) 밖으로 나가지
    /// 않게 경로를 정리하는 것은 빌더가 한다.
    pub sources: Vec<String>,
    /// 대상 경로. 원본이 둘 이상이면 `/`로 끝난다. 상대 경로면 작업 디렉터리에 잇는 것은 빌더가 한다.
    pub dest: String,
    /// `--chown`. 그룹이 없으면 그룹 번호도 사용자 번호와 같다(Docker의 `COPY` 규칙. `USER`와 다르다).
    pub chown: Option<crate::UserSpec>,
    /// `--chmod`(8진수).
    pub chmod: Option<u32>,
}

/// `HEALTHCHECK`. 시간은 나노초이고, 없으면 바탕 이미지의 값을 물려받는다(Docker와 같다).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Healthcheck {
    /// `["NONE"]`, `["CMD", 인자...]`, `["CMD-SHELL", 문자열]`. config에 그대로 쓴다.
    pub test: Vec<String>,
    /// `--interval`.
    pub interval_ns: Option<u64>,
    /// `--timeout`.
    pub timeout_ns: Option<u64>,
    /// `--start-period`.
    pub start_period_ns: Option<u64>,
    /// `--start-interval`.
    pub start_interval_ns: Option<u64>,
    /// `--retries`.
    pub retries: Option<u32>,
}

/// 빌드를 막지 않지만 사용자에게 보여야 하는 것.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanWarning {
    /// 줄(1부터). 파일 전체에 대한 것이면 없다.
    pub line: Option<usize>,
    /// 사람에게 보이는 문장(영어, CLI 출력과 같다).
    pub message: String,
}

impl fmt::Display for PlanWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// 해석 실패. 지원하지 않는 명령과 기능도 이것이다(조용히 무시하지 않는다).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError {
    /// 줄(1부터). 파일 전체나 옵션에 대한 것이면 없다.
    pub line: Option<usize>,
    /// 사람에게 보이는 문장(영어, CLI 출력과 같다).
    pub message: String,
}

impl PlanError {
    /// 줄이 있는 오류.
    pub fn at(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            message: message.into(),
        }
    }

    /// 줄이 없는 오류.
    pub fn whole(message: impl Into<String>) -> Self {
        Self {
            line: None,
            message: message.into(),
        }
    }
}

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(line) => write!(f, "line {line}: {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for PlanError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_form_runs_through_the_shell() {
        let c = Command::Shell {
            shell: vec!["/bin/sh".into(), "-c".into()],
            script: "npm ci".into(),
        };
        assert_eq!(c.argv(), ["/bin/sh", "-c", "npm ci"]);
        let c = Command::Exec(vec!["npm".into(), "ci".into()]);
        assert_eq!(c.argv(), ["npm", "ci"]);
    }

    #[test]
    fn errors_and_warnings_show_their_line() {
        assert_eq!(PlanError::at(3, "bad").to_string(), "line 3: bad");
        assert_eq!(PlanError::whole("bad").to_string(), "bad");
        let w = PlanWarning {
            line: None,
            message: "w".into(),
        };
        assert_eq!(w.to_string(), "w");
    }

    #[test]
    fn plans_round_trip_through_json() {
        let plan = BuildPlan {
            stages: vec![Stage {
                name: Some("build".into()),
                position: 0,
                line: 1,
                text: "FROM scratch AS build".into(),
                base: StageBase::Scratch,
                steps: vec![Step {
                    line: 2,
                    text: "COPY --chown=1:2 a /b".into(),
                    kind: StepKind::Copy(CopyStep {
                        kind: CopyKind::Copy,
                        from: CopySource::Context,
                        sources: vec!["a".into()],
                        dest: "/b".into(),
                        chown: Some("1:2".parse().unwrap()),
                        chmod: Some(0o644),
                    }),
                }],
            }],
            warnings: vec![],
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<BuildPlan>(&json).unwrap(), plan);
        assert_eq!(plan.output().name.as_deref(), Some("build"));
    }
}
