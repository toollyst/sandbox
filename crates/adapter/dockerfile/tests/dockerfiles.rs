//! 실제 쓰이는 모양의 Dockerfile 묶음(`tests/dockerfiles/`): Node, Python, Go 다단계, Rust, 공식 이미지 꼴, Docker 문서의
//! 예제. 읽혀야 할 것은 계획의 모양을, 거부해야 할 것은 줄 번호를 본다.

use sbx_dockerfile::plan;
use sbx_model::Arch;
use sbx_model::build::{
    BuildOptions, BuildPlan, Command, CopySource, PlanError, StageBase, Step, StepKind,
};

const BASE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn file(name: &str) -> String {
    let path = format!("{}/tests/dockerfiles/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn build(name: &str, target: Option<&str>, args: &[(&str, &str)]) -> Result<BuildPlan, PlanError> {
    let o = BuildOptions {
        build_args: args
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        target: target.map(str::to_owned),
        arch: Arch::Aarch64,
    };
    plan(&file(name), &o, &mut |_| {
        Ok(vec![("PATH".into(), BASE_PATH.into())])
    })
}

fn ok(name: &str) -> BuildPlan {
    build(name, None, &[]).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn names(p: &BuildPlan) -> Vec<String> {
    p.stages
        .iter()
        .map(|s| s.name.clone().unwrap_or_else(|| s.position.to_string()))
        .collect()
}

fn texts(steps: &[Step]) -> Vec<&str> {
    steps.iter().map(|s| s.text.as_str()).collect()
}

fn env(steps: &[Step]) -> Vec<(String, String)> {
    steps
        .iter()
        .filter_map(|s| match &s.kind {
            StepKind::Env(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn find<'a>(steps: &'a [Step], prefix: &str) -> &'a Step {
    steps
        .iter()
        .find(|s| s.text.starts_with(prefix))
        .unwrap_or_else(|| panic!("no step starting with {prefix:?} in {:#?}", texts(steps)))
}

#[test]
fn node_multi_stage() {
    let p = ok("node.Dockerfile");
    assert_eq!(names(&p), ["base", "deps", "build", "final"]);
    assert_eq!(p.stages[0].text, "FROM node:22-alpine AS base");
    assert_eq!(p.stages[0].base, StageBase::Image("node:22-alpine".into()));
    for s in &p.stages[1..] {
        assert_eq!(s.base, StageBase::Stage(0));
    }
    let fin = p.output();
    assert_eq!(
        texts(&fin.steps),
        [
            "USER node",
            "COPY --chown=node:node package.json .",
            "COPY --from=deps --chown=node:node /usr/src/app/node_modules ./node_modules",
            "COPY --from=build --chown=node:node /usr/src/app/dist ./dist",
            "EXPOSE 3000",
            "HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://localhost:3000/health || exit 1",
            "CMD [\"node\", \"dist/index.js\"]",
        ]
    );
    let StepKind::Copy(c) = &fin.steps[2].kind else {
        panic!()
    };
    assert_eq!(c.from, CopySource::Stage(1));
    let lines: Vec<_> = p.warnings.iter().map(|w| w.line).collect();
    assert_eq!(lines, [Some(1), Some(30)]);

    let p = build("node.Dockerfile", Some("test"), &[("NODE_VERSION", "20")]).unwrap();
    assert_eq!(names(&p), ["base", "build", "test"]);
    assert_eq!(p.stages[0].base, StageBase::Image("node:20-alpine".into()));
    assert_eq!(env(&p.output().steps), [("NODE_ENV".into(), "test".into())]);
    // test 스테이지에는 HEALTHCHECK가 없다.
    assert_eq!(p.warnings.len(), 1);
}

#[test]
fn python_venv() {
    let p = ok("python.Dockerfile");
    assert_eq!(names(&p), ["builder", "1"]);
    let builder = &p.stages[0].steps;
    assert_eq!(
        env(builder),
        [
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ("PYTHONUNBUFFERED".into(), "1".into()),
            ("PIP_NO_CACHE_DIR".into(), "1".into()),
            ("PATH".into(), format!("/opt/venv/bin:{BASE_PATH}")),
        ]
    );
    assert_eq!(
        find(builder, "RUN pip").text,
        "RUN pip install --upgrade pip  && pip install -r requirements.txt"
    );
    let fin = &p.output().steps;
    let StepKind::Run(r) = &find(fin, "RUN adduser").kind else {
        panic!()
    };
    assert_eq!(r.args, [("UID".into(), "10001".into())]);
    let Command::Shell { script, .. } = &r.command else {
        panic!()
    };
    assert!(script.contains("--uid \"${UID}\"     appuser"), "{script}");
    let StepKind::Copy(c) = &find(fin, "COPY --from=builder").kind else {
        panic!()
    };
    assert_eq!(c.from, CopySource::Stage(0));
    assert_eq!(
        find(fin, "COPY --chown").kind,
        StepKind::Copy(sbx_model::build::CopyStep {
            kind: sbx_model::build::CopyKind::Copy,
            from: CopySource::Context,
            sources: vec![".".into()],
            dest: ".".into(),
            chown: Some("appuser".parse().unwrap()),
            chmod: None,
        })
    );
    let StepKind::Cmd(cmd) = &fin.last().unwrap().kind else {
        panic!()
    };
    assert_eq!(
        cmd.argv(),
        ["/bin/sh", "-c", "gunicorn --bind 0.0.0.0:8000 app:app"]
    );
}

#[test]
fn go_cross_compile_pattern_builds_for_the_host() {
    let p = ok("go.Dockerfile");
    assert_eq!(names(&p), ["build", "2"]);
    assert_eq!(
        p.stages[0].text,
        "FROM --platform=linux/arm64 golang:1.23-alpine AS build"
    );
    let StepKind::Run(r) = &find(&p.stages[0].steps, "RUN CGO_ENABLED").kind else {
        panic!()
    };
    assert_eq!(
        r.args,
        [
            ("TARGETOS".into(), "linux".into()),
            ("TARGETARCH".into(), "arm64".into())
        ]
    );
    let fin = p.output();
    assert_eq!(
        fin.base,
        StageBase::Image("gcr.io/distroless/static-debian12:nonroot".into())
    );
    assert_eq!(
        texts(&fin.steps),
        [
            "COPY --from=build /out/server /server",
            "USER nonroot:nonroot",
            "EXPOSE 8080/tcp",
            "ENTRYPOINT [\"/server\"]",
        ]
    );
    let p = build("go.Dockerfile", Some("test"), &[]).unwrap();
    assert_eq!(names(&p), ["build", "test"]);
}

#[test]
fn docs_multi_stage_example() {
    let p = ok("docs-multistage.Dockerfile");
    assert_eq!(names(&p), ["build", "1"]);
    assert_eq!(p.output().base, StageBase::Scratch);
    assert_eq!(
        texts(&p.output().steps),
        [
            "COPY --from=build /bin/hello /bin/hello",
            "CMD [\"/bin/hello\"]"
        ]
    );
    assert!(p.warnings.is_empty());
}

#[test]
fn docs_env_and_label_examples() {
    let p = ok("docs-env.Dockerfile");
    let s = &p.output().steps;
    assert_eq!(s[1].kind, StepKind::Workdir("/bar".into()));
    let StepKind::Copy(add) = &s[2].kind else {
        panic!()
    };
    assert_eq!(
        (add.sources.as_slice(), add.dest.as_str()),
        (&[".".to_string()][..], "/bar")
    );
    let StepKind::Copy(copy) = &s[3].kind else {
        panic!()
    };
    assert_eq!(copy.sources, ["$FOO"]);
    let e = env(s);
    let get = |k: &str| {
        e.iter()
            .rev()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    };
    assert_eq!(get("abc"), Some("bye"));
    assert_eq!(get("def"), Some("hello"));
    assert_eq!(get("ghi"), Some("bye"));
    assert_eq!(get("MY_NAME"), Some("John Doe"));
    assert_eq!(get("MY_DOG"), Some("Rex The Dog"));
    assert_eq!(get("MY_VAR"), Some("my-value"));
    let labels: Vec<(String, String)> = s
        .iter()
        .filter_map(|s| match &s.kind {
            StepKind::Label(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        labels[0],
        ("com.example.vendor".into(), "ACME Incorporated".into())
    );
    assert_eq!(
        labels[3],
        (
            "description".into(),
            "This text illustrates that label-values can span multiple lines.".into()
        )
    );
    assert_eq!(labels.len(), 7);
}

#[test]
fn docs_arg_examples() {
    let p = ok("docs-arg.Dockerfile");
    assert_eq!(names(&p), ["1"]);
    assert_eq!(p.output().base, StageBase::Image("extras:latest".into()));
    let s = &p.output().steps;
    assert_eq!(s[0].kind, StepKind::User("some_user".into()));
    assert_eq!(env(s), [("CONT_IMG_VER".into(), "v1.0.0".into())]);
    let StepKind::Run(r) = &s[2].kind else {
        panic!()
    };
    assert_eq!(r.args, [("CODE_VERSION".into(), "latest".into())]);

    let p = build(
        "docs-arg.Dockerfile",
        None,
        &[
            ("CONT_IMG_VER", "v2.0.1"),
            ("user", "what_user"),
            ("CODE_VERSION", "1.2"),
        ],
    )
    .unwrap();
    assert_eq!(p.output().base, StageBase::Image("extras:1.2".into()));
    let s = &p.output().steps;
    assert_eq!(s[0].kind, StepKind::User("what_user".into()));
    assert_eq!(env(s), [("CONT_IMG_VER".into(), "v2.0.1".into())]);
    let StepKind::Run(r) = &s[2].kind else {
        panic!()
    };
    // ENV CONT_IMG_VER가 ARG 뒤에 쓰므로 RUN의 ARG에는 없다.
    assert_eq!(
        r.args,
        [
            ("CODE_VERSION".into(), "1.2".into()),
            ("user".into(), "what_user".into())
        ]
    );
}

#[test]
fn docs_escape_example() {
    let p = ok("docs-escape.Dockerfile");
    let s = &p.output().steps;
    let StepKind::Copy(c) = &s[0].kind else {
        panic!()
    };
    assert_eq!(c.dest, "c:\\");
    let StepKind::Run(r) = &s[1].kind else {
        panic!()
    };
    assert_eq!(r.command.argv(), ["/bin/sh", "-c", "dir c:\\"]);
    assert_eq!((s[0].line, s[1].line), (4, 5));
}

#[test]
fn docs_best_practices_examples() {
    let p = ok("docs-best-practices.Dockerfile");
    let s = &p.output().steps;
    assert_eq!(
        s[1].text,
        "RUN apt-get update && apt-get install -y --no-install-recommends     bzr     cvs     git     mercurial     subversion     && rm -rf /var/lib/apt/lists/*"
    );
    let StepKind::Run(after_shell) = &find(s, "RUN wget").kind else {
        panic!()
    };
    let StepKind::Run(before) = &find(s, "RUN set -o").kind else {
        panic!()
    };
    assert_eq!(before.command.argv()[0], "/bin/sh");
    assert_eq!(
        after_shell.command.argv()[..4],
        ["/bin/bash", "-o", "pipefail", "-c"]
    );
    let e = env(s);
    assert_eq!(
        e.last().unwrap(),
        &(
            "PATH".into(),
            format!("/usr/local/postgres-9.3/bin:{BASE_PATH}")
        )
    );
    assert_eq!(
        find(s, "VOLUME").kind,
        StepKind::Volume(vec!["/data".into()])
    );
}

#[test]
fn rust_example() {
    let p = ok("rust.Dockerfile");
    assert_eq!(names(&p), ["builder", "1"]);
    assert_eq!(p.output().steps.len(), 3);
}

#[test]
fn official_image_style_with_legacy_env() {
    let p = ok("postgres-style.Dockerfile");
    let s = &p.output().steps;
    assert_eq!(s[0].line, 3);
    let StepKind::Run(r) = &s[0].kind else {
        panic!()
    };
    let Command::Shell { script, .. } = &r.command else {
        panic!()
    };
    assert!(!script.contains("the comment below"), "{script}");
    assert!(script.contains("groupadd -r postgres --gid=999; \tuseradd"));
    let e = env(s);
    assert_eq!(e[1], ("PG_MAJOR".into(), "16".into()));
    assert_eq!(
        e[2],
        (
            "PATH".into(),
            format!("{BASE_PATH}:/usr/lib/postgresql/16/bin")
        )
    );
    let StepKind::Copy(c) = &find(s, "COPY").kind else {
        panic!()
    };
    assert_eq!(c.sources.len(), 2);
    assert_eq!(c.dest, "/usr/local/bin/");
    assert_eq!(
        find(s, "STOPSIGNAL").kind,
        StepKind::StopSignal("SIGINT".into())
    );
    assert_eq!(
        find(s, "VOLUME").kind,
        StepKind::Volume(vec!["/var/lib/postgresql/data".into()])
    );
}

#[test]
fn rejected_with_line_numbers() {
    assert_eq!(
        build("rejected-cache-mount.Dockerfile", None, &[]).unwrap_err(),
        PlanError::at(5, "RUN --mount is not supported yet")
    );
    assert_eq!(
        build("rejected-heredoc.Dockerfile", None, &[]).unwrap_err(),
        PlanError::at(4, "heredocs (<<EOF) are not supported yet")
    );
}

#[test]
fn every_fixture_is_covered() {
    let dir = format!("{}/tests/dockerfiles", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "docs-arg.Dockerfile",
            "docs-best-practices.Dockerfile",
            "docs-env.Dockerfile",
            "docs-escape.Dockerfile",
            "docs-multistage.Dockerfile",
            "go.Dockerfile",
            "node.Dockerfile",
            "postgres-style.Dockerfile",
            "python.Dockerfile",
            "rejected-cache-mount.Dockerfile",
            "rejected-heredoc.Dockerfile",
            "rust.Dockerfile",
        ]
    );
}
