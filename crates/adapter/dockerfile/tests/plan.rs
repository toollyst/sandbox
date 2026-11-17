//! 빌드 계획 규칙: 스테이지, `ARG` 범위, 변수 풀기, `--target`, 값 검사, 경고. 공개 함수(`plan`)로만 본다.

use sbx_dockerfile::plan;
use sbx_model::Arch;
use sbx_model::build::{
    BuildOptions, BuildPlan, Command, CopyKind, CopySource, DEFAULT_PATH, PlanError, Stage,
    StageBase, StepKind,
};

fn opts() -> BuildOptions {
    BuildOptions {
        build_args: vec![],
        target: None,
        arch: Arch::Aarch64,
    }
}

/// 바탕 이미지마다 `PATH`와 `HOME`이 있는 것으로 본다. `env:` 이미지는 그 뒤가 값이다(`env:A=1,B=2`).
fn base(image: &str) -> Result<Vec<(String, String)>, String> {
    if image == "broken" {
        return Err("manifest unknown".into());
    }
    if let Some(list) = image.strip_prefix("env:") {
        return Ok(list
            .split(',')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.into(), v.into()))
            .collect());
    }
    Ok(vec![
        ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
        ("HOME".into(), "/root".into()),
    ])
}

fn p_with(text: &str, o: &BuildOptions) -> Result<BuildPlan, PlanError> {
    plan(text, o, &mut base)
}

fn p(text: &str) -> BuildPlan {
    p_with(text, &opts()).unwrap_or_else(|e| panic!("{e}\n{text}"))
}

fn err(text: &str) -> PlanError {
    p_with(text, &opts()).expect_err(text)
}

fn with_args(text: &str, args: &[(&str, &str)]) -> BuildPlan {
    let o = BuildOptions {
        build_args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..opts()
    };
    p_with(text, &o).unwrap()
}

fn texts(s: &Stage) -> Vec<&str> {
    s.steps.iter().map(|s| s.text.as_str()).collect()
}

fn env_of(plan: &BuildPlan) -> Vec<(String, String)> {
    plan.output()
        .steps
        .iter()
        .filter_map(|s| match &s.kind {
            StepKind::Env(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn run_args(plan: &BuildPlan, n: usize) -> Vec<(String, String)> {
    let runs: Vec<_> = plan
        .output()
        .steps
        .iter()
        .filter_map(|s| match &s.kind {
            StepKind::Run(r) => Some(r.args.clone()),
            _ => None,
        })
        .collect();
    runs[n].clone()
}

fn kv(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---- 스테이지와 FROM ----

#[test]
fn from_bases() {
    let plan =
        p("FROM scratch\nFROM alpine:3 AS Base\nFROM BASE\nFROM base AS final\nCOPY --from=0 / /");
    // 0번 스테이지는 COPY --from=0이 쓰므로 남는다. 2번은 아무도 쓰지 않으므로 빠진다.
    let names: Vec<_> = plan
        .stages
        .iter()
        .map(|s| (s.position, s.name.clone()))
        .collect();
    assert_eq!(
        names,
        [
            (0, None),
            (1, Some("base".into())),
            (3, Some("final".into()))
        ]
    );
    assert_eq!(plan.stages[0].base, StageBase::Scratch);
    assert_eq!(plan.stages[1].base, StageBase::Image("alpine:3".into()));
    assert_eq!(plan.stages[2].base, StageBase::Stage(1));
    assert_eq!(plan.stages[1].text, "FROM alpine:3 AS base");
    assert_eq!(plan.stages[2].line, 4);
    let StepKind::Copy(c) = &plan.output().steps[0].kind else {
        panic!()
    };
    assert_eq!(c.from, CopySource::Stage(0));
}

#[test]
fn a_stage_named_like_its_image_uses_the_image() {
    let plan = p("FROM golang AS golang\nFROM golang\n");
    assert_eq!(plan.stages[0].base, StageBase::Image("golang".into()));
    assert_eq!(plan.stages[1].base, StageBase::Stage(0));
}

#[test]
fn stages_can_only_use_earlier_stages() {
    let e = err("FROM later AS x\nFROM alpine AS later\nFROM x");
    assert_eq!(e.line, Some(1));
    // 필요 없는 스테이지면 오류가 아니다.
    assert_eq!(p("FROM later\nFROM alpine AS later").stages.len(), 1);
    assert!(e.message.contains("later build stage"), "{e}");
    let e = err("FROM alpine AS a\nCOPY --from=b /x /x\nFROM alpine AS b\nFROM a");
    assert_eq!(e.line, Some(2));
    let e = err("FROM alpine AS a\nCOPY --from=a /x /x");
    assert_eq!(e.line, Some(2));
    let e = err("FROM alpine\nCOPY --from=0 /x /x");
    assert!(e.message.contains("current or a later"), "{e}");
    assert_eq!(err("FROM a AS x\nFROM b AS X").line, Some(2));
}

#[test]
fn from_is_expanded_with_global_args_only() {
    let text =
        "ARG BASE=alpine\nARG TAG=3.20\nFROM ${BASE}:${TAG:-latest} AS b\nARG TAG\nRUN echo $TAG";
    let plan = p(text);
    assert_eq!(plan.stages[0].base, StageBase::Image("alpine:3.20".into()));
    let plan = with_args(text, &[("TAG", "edge")]);
    assert_eq!(plan.stages[0].base, StageBase::Image("alpine:edge".into()));
    // ENV는 FROM에 쓰이지 않는다.
    let plan = p("FROM alpine AS a\nENV X=nope\nFROM ${X:-busybox}");
    assert_eq!(plan.output().base, StageBase::Image("busybox".into()));
    let e = err("ARG NOPE\nFROM ${NOPE}");
    assert_eq!(e.line, Some(2));
    assert!(e.message.contains("blank"), "{e}");
}

#[test]
fn platform_must_be_the_host() {
    let plan =
        p("FROM --platform=$BUILDPLATFORM golang AS build\nFROM --platform=linux/arm64/v8 alpine");
    assert_eq!(plan.stages.len(), 1);
    assert_eq!(plan.output().text, "FROM --platform=linux/arm64/v8 alpine");
    let e = err("FROM --platform=linux/amd64 alpine");
    assert_eq!(e.line, Some(1));
    assert!(e.message.contains("host platform linux/arm64"), "{e}");
    let x86 = BuildOptions {
        arch: Arch::X86_64,
        ..opts()
    };
    assert!(p_with("FROM --platform=$TARGETPLATFORM alpine", &x86).is_ok());
    assert!(p_with("FROM --platform=linux/x86_64 alpine", &x86).is_ok());
    assert!(p_with("FROM --platform=windows/amd64 alpine", &x86).is_err());
    // 필요 없는 스테이지의 플랫폼은 보지 않는다.
    assert_eq!(
        p("FROM --platform=linux/amd64 alpine AS cross\nFROM alpine")
            .stages
            .len(),
        1
    );
}

#[test]
fn files_without_from_and_instructions_before_it() {
    assert_eq!(
        err("# only a comment\n"),
        PlanError::whole("the Dockerfile has no FROM instruction")
    );
    assert_eq!(
        err("ARG A=1\n"),
        PlanError::whole("the Dockerfile has no FROM instruction")
    );
    let e = err("ARG A\nRUN x\nFROM a");
    assert_eq!(e.line, Some(2));
    assert_eq!(
        e.message,
        "RUN cannot come before the first FROM (only ARG can)"
    );
}

#[test]
fn base_image_errors_point_at_the_from_line() {
    let e = err("FROM alpine\nFROM broken\nRUN x");
    assert_eq!(e, PlanError::at(2, "broken: manifest unknown"));
}

#[test]
fn base_env_is_asked_once_per_image_and_only_for_needed_stages() {
    let mut asked = Vec::new();
    let text = "FROM node AS a\nFROM python AS unused\nFROM node AS b\nCOPY --from=a /x /x\nFROM b";
    plan(text, &opts(), &mut |r: &str| {
        asked.push(r.to_owned());
        Ok(vec![])
    })
    .unwrap();
    assert_eq!(asked, ["node"]);
}

// ---- --target ----

#[test]
fn target_keeps_only_what_it_needs() {
    let text = "\
FROM node AS deps
RUN npm ci
FROM deps AS build
RUN npm run build
FROM node AS test
COPY --from=build /app /app
RUN npm test
FROM nginx AS prod
COPY --from=build /app/dist /usr/share/nginx/html
";
    let o = |t: &str| BuildOptions {
        target: Some(t.into()),
        ..opts()
    };
    let plan = p_with(text, &o("TEST")).unwrap();
    let names: Vec<_> = plan
        .stages
        .iter()
        .map(|s| s.name.as_deref().unwrap())
        .collect();
    assert_eq!(names, ["deps", "build", "test"]);
    assert_eq!(plan.stages[1].base, StageBase::Stage(0));
    let StepKind::Copy(c) = &plan.output().steps[0].kind else {
        panic!()
    };
    assert_eq!(c.from, CopySource::Stage(1));

    let plan = p(text);
    let names: Vec<_> = plan
        .stages
        .iter()
        .map(|s| s.name.as_deref().unwrap())
        .collect();
    assert_eq!(names, ["deps", "build", "prod"]);
    let StepKind::Copy(c) = &plan.output().steps[0].kind else {
        panic!()
    };
    assert_eq!(c.from, CopySource::Stage(1));

    let plan = p_with(text, &o("deps")).unwrap();
    assert_eq!(plan.stages.len(), 1);

    assert_eq!(
        p_with(text, &o("nope")).unwrap_err(),
        PlanError::whole("target stage \"nope\" could not be found")
    );
}

#[test]
fn unneeded_stages_are_not_expanded_but_are_still_checked() {
    // 필요 없는 스테이지의 풀기 오류는 나지 않는다(BuildKit과 같다).
    let plan = p("FROM alpine AS dev\nENV A=\"${MISSING:?must be set}\"\nFROM alpine\nRUN true");
    assert_eq!(plan.stages.len(), 1);
    // 정적 검사는 파일 전체에 한다.
    let e = err("FROM alpine AS dev\nRUN --mount=type=cache,target=/x true\nFROM alpine\nRUN true");
    assert_eq!(e, PlanError::at(2, "RUN --mount is not supported yet"));
}

// ---- ARG ----

#[test]
fn global_args_must_be_redeclared_in_a_stage() {
    let text = "ARG V=1\nFROM alpine\nENV A=$V\nARG V\nENV B=$V";
    assert_eq!(env_of(&p(text)), kv(&[("A", ""), ("B", "1")]));
    assert_eq!(
        env_of(&with_args(text, &[("V", "2")])),
        kv(&[("A", ""), ("B", "2")])
    );
}

#[test]
fn stage_args_take_build_args_then_defaults_then_globals() {
    let text = "ARG G=global\nFROM alpine\nARG G=stage\nARG U\nENV G=$G U=${U-unset}";
    assert_eq!(env_of(&p(text)), kv(&[("G", "stage"), ("U", "unset")]));
    assert_eq!(
        env_of(&with_args(text, &[("G", "cli"), ("U", "")])),
        kv(&[("G", "cli"), ("U", "")])
    );
}

#[test]
fn arg_scope_ends_with_the_stage() {
    // 다른 이미지에서 시작한 스테이지에는 이어지지 않는다.
    let plan = p("FROM alpine AS a\nARG X=1\nFROM alpine\nENV Y=${X:-gone}");
    assert_eq!(env_of(&plan), kv(&[("Y", "gone")]));
}

#[test]
fn args_in_one_instruction_expand_in_order_and_env_pairs_do_not() {
    let plan = p("FROM alpine\nARG A=1 B=${A}2\nENV A=x C=$A\nENV D=$B");
    assert_eq!(env_of(&plan), kv(&[("A", "x"), ("C", "1"), ("D", "12")]));
}

#[test]
fn predefined_platform_args() {
    let plan = p(
        "FROM alpine\nARG TARGETARCH\nARG TARGETPLATFORM\nARG TARGETVARIANT\nENV A=$TARGETARCH P=$TARGETPLATFORM V=${TARGETVARIANT-unset} B=${BUILDOS:-none}",
    );
    assert_eq!(
        env_of(&plan),
        kv(&[
            ("A", "arm64"),
            ("P", "linux/arm64"),
            ("V", ""),
            ("B", "none")
        ])
    );
    let o = BuildOptions {
        build_args: vec![("TARGETARCH".into(), "amd64".into())],
        ..opts()
    };
    let e = p_with("FROM alpine", &o).unwrap_err();
    assert_eq!(e.line, None);
    assert!(e.message.contains("TARGETARCH is set by sbx"), "{e}");
}

#[test]
fn unused_build_args_are_a_warning() {
    let plan = with_args(
        "ARG USED\nFROM alpine AS x\nARG ALSO\nFROM alpine\nARG LAST\n",
        &[
            ("USED", "1"),
            ("ALSO", "2"),
            ("LAST", "3"),
            ("NOPE", "4"),
            ("SOURCE_DATE_EPOCH", "0"),
        ],
    );
    // ALSO는 빠진 스테이지에만 있다. SOURCE_DATE_EPOCH는 빌드가 직접 쓰므로 알리지 않는다.
    let w: Vec<_> = plan.warnings.iter().map(|w| w.to_string()).collect();
    assert_eq!(
        w,
        ["build arguments not used by any ARG in the stages built: ALSO, NOPE"]
    );
}

#[test]
fn run_gets_the_args_in_scope() {
    let plan =
        p("FROM alpine\nARG A=1\nARG B\nRUN one\nENV A=env\nRUN two\nARG A=again\nRUN three");
    assert_eq!(run_args(&plan, 0), kv(&[("A", "1")]));
    // ENV가 나중에 쓰면 ENV가 이긴다.
    assert_eq!(run_args(&plan, 1), kv(&[]));
    // ARG가 나중에 쓰면 ARG가 이긴다(BuildKit의 순서).
    assert_eq!(run_args(&plan, 2), kv(&[("A", "again")]));
    let plan = with_args("FROM alpine\nARG B\nRUN x", &[("B", "cli")]);
    assert_eq!(run_args(&plan, 0), kv(&[("B", "cli")]));
}

// ---- ENV와 바탕 환경 ----

#[test]
fn env_expands_with_the_base_image_env() {
    let plan = p("FROM python:3.12\nENV PATH=/opt/venv/bin:$PATH\nENV HOME_DIR=${HOME}/app");
    assert_eq!(
        env_of(&plan),
        kv(&[
            ("PATH", "/opt/venv/bin:/usr/local/bin:/usr/bin:/bin"),
            ("HOME_DIR", "/root/app")
        ])
    );
    let plan = p("FROM scratch\nENV P=$PATH");
    assert_eq!(env_of(&plan), kv(&[("P", DEFAULT_PATH)]));
    // 스테이지 바탕이면 그 스테이지의 최종 Env를 물려받는다.
    let plan = p("FROM env:A=1 AS a\nENV B=2\nFROM a\nENV C=$A$B");
    assert_eq!(env_of(&plan), kv(&[("C", "12")]));
}

#[test]
fn env_and_label_forms() {
    let plan = p(
        "FROM alpine\nENV A=1 B=\"two words\" C='$A' D=a\\ b\nENV LEGACY  hello   world\nLABEL org.opencontainers.image.title=\"My App\" \"key with space\"=v",
    );
    assert_eq!(
        env_of(&plan),
        kv(&[
            ("A", "1"),
            ("B", "two words"),
            ("C", "$A"),
            ("D", "a b"),
            ("LEGACY", "hello   world")
        ])
    );
    let s = plan.output();
    assert_eq!(s.steps[0].text, "ENV A=1 B=\"two words\" C=$A D=\"a b\"");
    let StepKind::Label(l) = &s.steps[2].kind else {
        panic!()
    };
    assert_eq!(
        l,
        &kv(&[
            ("org.opencontainers.image.title", "My App"),
            ("key with space", "v")
        ])
    );
    assert_eq!(
        err("FROM a\nENV $NOPE=1").message,
        "ENV names can not be blank"
    );
}

// ---- RUN, CMD, ENTRYPOINT, SHELL ----

#[test]
fn shell_and_exec_forms() {
    let plan = p(
        "FROM alpine AS a\nRUN echo $HOME\nSHELL [\"/bin/bash\", \"-euo\", \"pipefail\", \"-c\"]\nRUN [\"echo\", \"$HOME\"]\nRUN echo hi\nCMD echo cmd\nFROM a\nENTRYPOINT exec app\nCMD [\"--flag\"]",
    );
    let a = &plan.stages[0];
    let argv = |s: &StepKind| match s {
        StepKind::Run(r) => r.command.argv(),
        StepKind::Cmd(c) | StepKind::Entrypoint(c) => c.argv(),
        _ => panic!(),
    };
    assert_eq!(argv(&a.steps[0].kind), ["/bin/sh", "-c", "echo $HOME"]);
    assert_eq!(argv(&a.steps[2].kind), ["echo", "$HOME"]);
    assert_eq!(
        argv(&a.steps[3].kind),
        ["/bin/bash", "-euo", "pipefail", "-c", "echo hi"]
    );
    assert_eq!(
        argv(&a.steps[4].kind),
        ["/bin/bash", "-euo", "pipefail", "-c", "echo cmd"]
    );
    assert_eq!(
        texts(a),
        [
            "RUN echo $HOME",
            "SHELL [\"/bin/bash\", \"-euo\", \"pipefail\", \"-c\"]",
            "RUN [\"echo\", \"$HOME\"]",
            "RUN echo hi",
            "CMD echo cmd"
        ]
    );
    // SHELL은 FROM 스테이지로 물려받는다.
    let out = plan.output();
    assert_eq!(
        argv(&out.steps[0].kind),
        ["/bin/bash", "-euo", "pipefail", "-c", "exec app"]
    );
    assert_eq!(
        out.steps[1].kind,
        StepKind::Cmd(Command::Exec(vec!["--flag".into()]))
    );
}

#[test]
fn line_continuations_keep_the_script() {
    let plan = p(
        "FROM debian\nRUN apt-get update \\\n    # a comment inside\n    && apt-get install -y curl \\\n    && rm -rf /var/lib/apt/lists/*\nUSER nobody",
    );
    let s = &plan.output().steps;
    assert_eq!(s[0].line, 2);
    assert_eq!(
        s[0].text,
        "RUN apt-get update     && apt-get install -y curl     && rm -rf /var/lib/apt/lists/*"
    );
    assert_eq!(s[1].line, 6);
}

// ---- COPY, ADD ----

#[test]
fn copy_values() {
    let plan = p(
        "ARG IMG=busybox\nFROM alpine AS build\nFROM alpine\nARG IMG\nARG OWNER=1000:app\nARG SRC=dist\nWORKDIR /app\nCOPY --from=build --chown=$OWNER --chmod=0755 /out/${SRC} ./\nCOPY --from=${IMG}:1 /bin/sh /sh\nCOPY [\"a b\", \"c\", \"/dest/\"]\nADD --chmod=644 archive.tar.gz /opt/",
    );
    let copies: Vec<_> = plan
        .output()
        .steps
        .iter()
        .filter_map(|s| match &s.kind {
            StepKind::Copy(c) => Some((s.text.clone(), c.clone())),
            _ => None,
        })
        .collect();
    let (text, c) = &copies[0];
    assert_eq!(
        text,
        "COPY --from=build --chown=1000:app --chmod=0755 /out/dist ./"
    );
    assert_eq!(c.from, CopySource::Stage(0));
    assert_eq!(c.chown.as_ref().unwrap().to_string(), "1000:app");
    assert_eq!(c.chmod, Some(0o755));
    assert_eq!(
        (c.sources.as_slice(), c.dest.as_str()),
        (&["/out/dist".to_string()][..], "./")
    );
    assert_eq!(copies[1].1.from, CopySource::Image("busybox:1".into()));
    assert_eq!(copies[2].0, "COPY [\"a b\", \"c\", \"/dest/\"]");
    assert_eq!(copies[2].1.sources, ["a b", "c"]);
    assert_eq!(copies[3].1.kind, CopyKind::Add);
    assert_eq!(copies[3].1.chmod, Some(0o644));
    assert_eq!(copies[3].1.from, CopySource::Context);
}

#[test]
fn copy_checks() {
    for (text, want) in [
        (
            "COPY a b /c",
            "the destination must be a directory and end with a /",
        ),
        (
            "ADD https://example.com/x.tgz /x",
            "ADD <URL> is not supported yet",
        ),
        (
            "ADD git@github.com:a/b.git /b",
            "ADD <git URL> is not supported yet",
        ),
        ("COPY http://example.com/x /x", "COPY does not take URLs"),
        ("COPY --chmod=u+x a /a", "only octal modes"),
        ("COPY --chmod=17777 a /a", "only octal modes"),
        ("COPY --chown=a:b:c a /a", "invalid --chown"),
        ("COPY --chown=: a /a", "invalid --chown"),
        ("ARG E\nCOPY $E /a", "source can not be empty"),
        (
            "ARG U=https://x/y\nADD $U /y",
            "ADD <URL> is not supported yet",
        ),
    ] {
        let e = err(&format!("FROM alpine\n{text}"));
        assert!(e.message.contains(want), "{text}: {e}");
        assert_eq!(e.line, Some(text.lines().count() + 1), "{text}");
    }
}

// ---- 나머지 명령 ----

#[test]
fn config_steps() {
    let plan = p(
        "FROM alpine\nARG PORT=8080\nARG DIR=/srv\nEXPOSE $PORT 53/UDP 9000-9002/tcp\nVOLUME $DIR/data /logs\nVOLUME [\"/a b\"]\nWORKDIR $DIR\nUSER ${UID:-1000}:1000\nSTOPSIGNAL SIGQUIT",
    );
    let k: Vec<_> = plan.output().steps.iter().map(|s| s.kind.clone()).collect();
    assert_eq!(
        k[0],
        StepKind::Expose(
            ["8080/tcp", "53/udp", "9000/tcp", "9001/tcp", "9002/tcp"]
                .map(String::from)
                .to_vec()
        )
    );
    assert_eq!(
        k[1],
        StepKind::Volume(vec!["/srv/data".into(), "/logs".into()])
    );
    assert_eq!(k[2], StepKind::Volume(vec!["/a b".into()]));
    assert_eq!(k[3], StepKind::Workdir("/srv".into()));
    assert_eq!(k[4], StepKind::User("1000:1000".into()));
    assert_eq!(k[5], StepKind::StopSignal("SIGQUIT".into()));
    assert_eq!(
        texts(plan.output()),
        [
            "EXPOSE 8080 53/UDP 9000-9002/tcp",
            "VOLUME /srv/data /logs",
            "VOLUME [\"/a b\"]",
            "WORKDIR /srv",
            "USER 1000:1000",
            "STOPSIGNAL SIGQUIT"
        ]
    );
    // EXPOSE는 풀린 값을 공백으로 다시 나눈다.
    let plan = p("FROM alpine\nARG PORTS=\"80 443\"\nEXPOSE $PORTS");
    assert_eq!(
        plan.output().steps[0].kind,
        StepKind::Expose(vec!["80/tcp".into(), "443/tcp".into()])
    );
}

#[test]
fn config_checks() {
    for (text, want) in [
        ("EXPOSE 80/http", "invalid protocol"),
        ("EXPOSE 8080:80", "host ports are not allowed"),
        ("EXPOSE 90-80", "invalid port"),
        ("EXPOSE 70000", "invalid port"),
        ("ARG E\nEXPOSE $E", "EXPOSE requires at least one argument"),
        ("STOPSIGNAL SIGNOPE", "invalid STOPSIGNAL"),
        (
            "ARG E\nVOLUME $E",
            "VOLUME specified can not be an empty string",
        ),
        ("ARG E\nWORKDIR $E", "WORKDIR can not be empty"),
        ("ENV A=\"${B:?B is required}\"", "B: B is required"),
        // Docker처럼 따옴표 없는 공백은 ENV 쌍을 나눈다.
        ("ENV A=${B:?B is required}", "can't find ="),
        ("ENV A=${B#x}", "pattern substitution is not supported yet"),
        ("ENV A=\"unterminated", "double-quote"),
    ] {
        let e = err(&format!("FROM alpine\n{text}"));
        assert!(e.message.contains(want), "{text}: {e}");
        assert_eq!(e.line, Some(text.lines().count() + 1), "{text}");
    }
}

// ---- 경고 ----

#[test]
fn warnings() {
    let plan = p(
        "# syntax=docker/dockerfile:1.7\n# check=error=true\nFROM alpine AS unused\nHEALTHCHECK CMD true\nFROM alpine\nMAINTAINER someone@example.com\nHEALTHCHECK --interval=5s CMD [\"/check\"]\nRUN a \\\n\n  b\n",
    );
    let w: Vec<_> = plan
        .warnings
        .iter()
        .map(|w| (w.line, w.message.as_str()))
        .collect();
    assert_eq!(w.len(), 5, "{w:?}");
    assert_eq!(w[0].0, Some(1));
    assert!(w[0].1.contains("docker/dockerfile:1.7"));
    assert_eq!(w[1].0, Some(2));
    assert_eq!(
        w[2],
        (Some(8), "empty continuation line (Docker deprecates these)")
    );
    assert_eq!(w[3].0, Some(6));
    assert!(w[3].1.starts_with("MAINTAINER is deprecated"));
    // 필요한 스테이지의 HEALTHCHECK만 알린다.
    assert_eq!(w[4].0, Some(7));
    assert!(w[4].1.contains("sbx does not run health checks"));
    let s = &plan.output().steps;
    let StepKind::Healthcheck(h) = &s[1].kind else {
        panic!()
    };
    assert_eq!(h.test, ["CMD", "/check"]);
    assert_eq!(h.interval_ns, Some(5_000_000_000));
    assert_eq!(s[1].text, "HEALTHCHECK --interval=5s CMD [\"/check\"]");
    assert_eq!(
        s[0].kind,
        StepKind::Maintainer("someone@example.com".into())
    );
}

// ---- 줄 번호와 오류 ----

#[test]
fn errors_carry_the_starting_line() {
    let e = err("FROM alpine\nRUN echo \\\n  a\nFOO bar\n");
    assert_eq!(e, PlanError::at(4, "unknown instruction: FOO"));
    let e = err("FROM alpine\nRUN a \\\n  && cat <<EOF \\\n  x\n");
    assert_eq!(
        e,
        PlanError::at(2, "heredocs (<<EOF) are not supported yet")
    );
    let e = err("FROM alpine\nONBUILD RUN x");
    assert_eq!(e, PlanError::at(2, "ONBUILD is not supported yet"));
    let e = err("FROM alpine\nCOPY --link . /");
    assert_eq!(e, PlanError::at(2, "COPY --link is not supported yet"));
    assert_eq!(e.to_string(), "line 2: COPY --link is not supported yet");
}

#[test]
fn escape_directive() {
    let plan = p("# escape=`\nFROM alpine\nWORKDIR C:\\app\nRUN echo a `\n  b\nENV A=x`$y");
    assert_eq!(
        texts(plan.output()),
        ["WORKDIR \"C:\\\\app\"", "RUN echo a   b", "ENV A=x$y"]
    );
}

#[test]
fn exponential_expansion_stops() {
    let mut text = String::from("FROM alpine\nENV A=0123456789abcdef\n");
    for _ in 0..30 {
        text.push_str("ENV A=$A$A\n");
    }
    let e = err(&text);
    assert!(e.message.contains("bytes"), "{e}");
    assert!(e.line.unwrap() > 2);
}

#[test]
fn plans_are_deterministic() {
    let text = "FROM node AS a\nRUN x\nFROM a\nCOPY --from=a / /";
    assert_eq!(p(text), p(text));
}

// ---- Docker로 확인한 규칙 ----
//
// 아래 기대값은 Docker 29.9.2(buildx 0.38.1, BuildKit 0.34.0, linux/arm64)에 같은 Dockerfile을
// `docker build --no-cache`로 돌려 본 결과다(2026-11-13). RUN 환경은 `RUN echo`와 `RUN ["/usr/bin/env"]`로,
// 풀기는 config `Env`로 보았다. CI에는 Docker가 필요 없다.

#[test]
fn docker_env_after_arg_wins_and_ignores_the_build_arg() {
    let text = "FROM alpine\nARG A=arg\nENV A=env\nRUN x\nENV B=$A";
    for plan in [p(text), with_args(text, &[("A", "cli")])] {
        assert_eq!(run_args(&plan, 0), kv(&[]));
        assert_eq!(env_of(&plan), kv(&[("A", "env"), ("B", "env")]));
    }
}

#[test]
fn docker_arg_after_env_wins_in_run_and_expansion_but_not_in_config() {
    let text = "FROM alpine\nENV A=env\nARG A=again\nRUN x\nENV B=$A";
    let plan = p(text);
    assert_eq!(run_args(&plan, 0), kv(&[("A", "again")]));
    assert_eq!(env_of(&plan), kv(&[("A", "env"), ("B", "again")]));
    let plan = with_args(text, &[("A", "cli")]);
    assert_eq!(run_args(&plan, 0), kv(&[("A", "cli")]));
    assert_eq!(env_of(&plan), kv(&[("A", "env"), ("B", "cli")]));
}

#[test]
fn docker_arg_overrides_a_base_image_env() {
    // 바탕 이미지의 PATH 뒤의 ARG PATH는 RUN의 PATH를 바꾼다. config의 PATH는 바탕 것 그대로다.
    let plan = p("FROM python:3.12\nARG PATH=/x\nRUN x\nENV P2=$PATH");
    assert_eq!(run_args(&plan, 0), kv(&[("PATH", "/x")]));
    // ENV 단계는 P2 하나다. 바탕의 PATH는 config에 그대로 남는다(빌더가 바탕 Env에 단계를 더한다).
    assert_eq!(env_of(&plan), kv(&[("P2", "/x")]));
}

#[test]
fn docker_args_carry_over_through_from_stage() {
    let text = "FROM alpine AS a\nARG X=1\nFROM a\nENV Y=${X:-gone}\nRUN x";
    let plan = p(text);
    assert_eq!(env_of(&plan), kv(&[("Y", "1")]));
    assert_eq!(run_args(&plan, 0), kv(&[("X", "1")]));
    let plan = with_args(text, &[("X", "cli")]);
    assert_eq!(env_of(&plan), kv(&[("Y", "cli")]));
    assert_eq!(run_args(&plan, 0), kv(&[("X", "cli")]));
}

#[test]
fn docker_arg_pairs_expand_in_order_env_pairs_against_the_state_before() {
    let plan = p("FROM alpine\nARG A=1 B=${A}2\nENV A=1\nENV A=x C=$A\nENV D=$B");
    assert_eq!(
        env_of(&plan),
        kv(&[("A", "1"), ("A", "x"), ("C", "1"), ("D", "12")])
    );
    let plan = p("FROM alpine\nENV a=1 b=$a");
    assert_eq!(env_of(&plan), kv(&[("a", "1"), ("b", "")]));
    let plan = p("FROM alpine\nENV a=0\nENV a=1 b=$a");
    assert_eq!(env_of(&plan), kv(&[("a", "0"), ("a", "1"), ("b", "0")]));
}

#[test]
fn docker_stage_args_resolve_cli_then_default_then_global() {
    let text = "ARG G=global\nFROM alpine\nARG G=stage\nRUN x";
    assert_eq!(run_args(&p(text), 0), kv(&[("G", "stage")]));
    assert_eq!(
        run_args(&with_args(text, &[("G", "cli")]), 0),
        kv(&[("G", "cli")])
    );
    let text = "ARG G=global\nFROM alpine\nARG G\nRUN x";
    assert_eq!(run_args(&p(text), 0), kv(&[("G", "global")]));
    assert_eq!(
        run_args(&with_args(text, &[("G", "cli")]), 0),
        kv(&[("G", "cli")])
    );
    // 다시 선언하지 않은 전역 ARG는 보이지 않는다.
    let text = "ARG V=1\nFROM alpine\nENV A=$V\nRUN x";
    assert_eq!(env_of(&p(text)), kv(&[("A", "")]));
    assert_eq!(run_args(&with_args(text, &[("V", "cli")]), 0), kv(&[]));
}

#[test]
fn docker_global_defaults_expand_with_earlier_globals() {
    let text = "ARG A=1\nARG B=${A}x\nFROM alpine\nARG B\nRUN x";
    assert_eq!(run_args(&p(text), 0), kv(&[("B", "1x")]));
    assert_eq!(
        run_args(&with_args(text, &[("B", "cli")]), 0),
        kv(&[("B", "cli")])
    );
    assert_eq!(
        run_args(&with_args(text, &[("A", "cli")]), 0),
        kv(&[("B", "clix")])
    );
}

#[test]
fn docker_platform_args_need_declaring_and_variants_are_empty() {
    let plan = p("FROM alpine\nARG TARGETVARIANT BUILDVARIANT TARGETOS\nRUN x");
    assert_eq!(
        run_args(&plan, 0),
        kv(&[
            ("TARGETVARIANT", ""),
            ("BUILDVARIANT", ""),
            ("TARGETOS", "linux")
        ])
    );
    let plan = p("FROM alpine\nENV E_BO=${BUILDOS:-none} E_TA=${TARGETARCH:-none}\nRUN x");
    assert_eq!(env_of(&plan), kv(&[("E_BO", "none"), ("E_TA", "none")]));
    assert_eq!(run_args(&plan, 0), kv(&[]));
}
