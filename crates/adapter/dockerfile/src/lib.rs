//! Dockerfile 해석. Dockerfile 내용과 빌드 옵션을 `sbx-model`의 빌드 계획([`BuildPlan`])으로 바꾼다.
//! `.dockerignore` 패턴도 여기서 맞춘다([`DockerIgnore`]).
//!
//! Dockerfile은 입력 형식이다. Dockerfile 자료형(명령, 플래그, 낱말)은 이 크레이트 밖으로 나가지 않는다. 밖으로는
//! 빌드 계획(스테이지와 단계)과 그 오류·경고만 나간다. 그래서 Dockerfile 범위를 넓힐 때는 이 크레이트만 고친다.
//!
//! 알면 안 되는 것: 파일시스템(컨텍스트를 읽거나 돌지 않는다), 레지스트리(바탕 이미지의 `Env`는 호출자가 준다),
//! VM과 빌더, 캐시. 이미지 참조의 문법도 모른다(문자열 그대로 넘긴다. 해석은 `sbx-oci`가 한다).
//!
//! 규칙은 모듈마다 문서 주석에 있다(모듈은 밖에 열지 않는다).
//!
//! - 줄, 주석, 줄 이음, 파서 지시어: `lines`
//! - 명령 하나의 문법과 플래그: `syntax`, `instruction`(JSON 배열은 `json`, `HEALTHCHECK` 시간은 `duration`)
//! - 변수 풀기: `shell`
//! - 스테이지, `ARG` 범위, `--target`, 값 검사, 경고, 크기 상한: `plan`
//! - `.dockerignore`: `ignore`
//!
//! # 범위
//!
//! 받는 것(기획 4.5의 "처음"): `FROM`(`AS`, `scratch`, `FROM` 앞 `ARG`, `--platform`은 호스트 플랫폼만), `RUN`(셸·exec
//! 형식), `COPY`(`--from`, `--chown`, `--chmod`), `ADD`(로컬 파일만, `--chown`, `--chmod`), `ENV`(`k=v`와 옛 형식
//! `k v`), `ARG`, `WORKDIR`, `USER`, `LABEL`, `EXPOSE`, `VOLUME`, `ENTRYPOINT`, `CMD`, `STOPSIGNAL`, `SHELL`, 파서 지시어
//! `escape`, 줄 이음, 주석, 변수 풀기.
//!
//! 받지 않는 것은 **줄 번호와 함께 오류**다. 조용히 무시하지 않는다.
//!
//! - 아직(기획의 "나중"): `RUN --mount`, heredoc(`<<EOF`), `ADD <URL>`과 git URL, `ONBUILD`, `COPY --link`.
//! - Docker에 있지만 범위 밖인 플래그: `RUN --network`·`--security`, `COPY --parents`·`--exclude`, `ADD --checksum`·
//!   `--keep-git-dir`·`--exclude`·`--unpack`·`--link`, 기호 `--chmod`(`u+x`), `${VAR#..}` 같은 패턴 치환.
//! - 모르는 명령과 모르는 플래그.
//!
//! 빌드는 하지만 경고로 알리는 것([`BuildPlan::warnings`], 호출자가 보인다):
//!
//! - `# syntax=`: 다른 프런트엔드를 고르지 않는다. 기획 4.5의 "무시하지 않고 알린다"다. 거의 모든 최신 Dockerfile이
//!   `# syntax=docker/dockerfile:1`로 시작하므로 오류로 하지 않는다.
//! - `HEALTHCHECK`: 기획 4.5("config에 쓰지만 sbx는 적용하지 않는다고 알린다")대로 오류가 아니다. 단계
//!   ([`StepKind::Healthcheck`](sbx_model::build::StepKind::Healthcheck))로 config에 쓰고 경고를 남긴다. 이미지를
//!   `docker run`으로 돌리면 Docker는 검사를 돌리므로, 거부하면 이식성을 잃는다.
//! - `MAINTAINER`: 기획에 없다. Docker가 폐기했지만 오래된 공개 Dockerfile에 흔하고 뜻이 분명하다(config `Author`).
//!   그래서 받고, 폐기된 명령이라고 경고한다.
//! - `# check=`, 이어지는 줄 사이의 빈 줄, 쓰이지 않은 `--build-arg`.
//!
//! 정적 검사(명령 이름, 플래그, 인자 개수, heredoc, JSON)는 파일 전체에 한다. 풀어야 아는 검사는 필요한 스테이지에만 한다
//! (`plan` 모듈).
//!
//! - 의존: `sbx-model`

use sbx_model::build::{BuildOptions, BuildPlan, PlanError};

mod duration;
mod ignore;
mod instruction;
mod json;
mod lines;
mod plan;
mod shell;
mod syntax;

pub use ignore::DockerIgnore;
pub use plan::{BaseEnv, MAX_EXPANDED_BYTES};

/// Dockerfile을 빌드 계획으로 바꾼다.
///
/// `base_env`는 바탕 이미지의 config `Env`를 돌려준다. 필요한 스테이지 가운데 이미지에서 시작하는 스테이지마다, 그
/// 스테이지를 풀기 전에 부른다(같은 참조는 한 번만). 바탕의 `Env`가 있어야 `ENV PATH=/x:$PATH`를 Docker처럼 풀 수 있다.
/// 받기 전에 계획부터 보고 싶으면 빈 목록을 돌려주는 함수로 한 번 부르고, 받은 뒤 다시 부르면 된다.
pub fn plan(
    dockerfile: &str,
    options: &BuildOptions,
    base_env: BaseEnv<'_>,
) -> Result<BuildPlan, PlanError> {
    plan::plan(dockerfile, options, base_env)
}
