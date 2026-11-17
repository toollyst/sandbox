//! Dockerfile 해석(`sbx-dockerfile`의 `plan`). Dockerfile은 사용자가 주지만, 남이 만든 저장소를 받아 빌드하면 그
//! 저장소가 쓴 것이다.
//!
//! 입력은 텍스트다. 첫 NUL 앞이 Dockerfile이고, 뒤는 줄마다: 첫 줄이 `--target`(비면 없음), 나머지가
//! `--build-arg`의 `이름=값`이다. 바탕 이미지의 `Env`는 언제나 같은 값이고, 이름이 `fail`인 이미지는 오류다.
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다. 같은 입력이면 같은 답이다.
//! - 바탕 `Env`는 이미지마다 한 번만 묻는다.
//! - 오류와 경고의 줄 번호는 파일 안에 있다.
//! - 계획: 스테이지가 있고, 원래 순서와 줄 순서를 지킨다. 단계의 줄은 그 스테이지의 `FROM`과 다음 스테이지 사이에 있고
//!   늘어나기만 한다. `FROM 스테이지`와 `COPY --from=스테이지`는 앞 스테이지만 가리킨다. `--target`을 주면 마지막
//!   스테이지가 그것이다.
//! - 단계의 `text`는 그 단계 종류의 명령 이름으로 시작한다. `RUN`의 argv는 비지 않는다. `COPY`의 원본과 대상은 비지
//!   않는다. 원본이 둘 이상이면 해석은 대상을 그대로 두고, 빌드(`sbx_build::copy`)가 빈 뿌리에서 놓아 보면 대상이
//!   `.`, 끝의 `/`·`/.`, 또는 `/`일 때만 그 디렉터리 안에 원본마다 놓이고 아니면 거부한다. `--chmod`는 `0o7777` 이하, `EXPOSE`는 `포트/tcp|udp|sctp`,
//!   `STOPSIGNAL`은 시그널로 읽힌다.
//! - 끝에 주석 줄을 더해도 답이 같다.
//! - 계획의 이름 있는 스테이지를 `--target`으로 다시 해석하면, 그 스테이지가 마지막이고 단계가 같다.

#![no_main]

use std::collections::BTreeSet;

use libfuzzer_sys::fuzz_target;
use sbx_dockerfile::plan;
use sbx_model::build::{
    BuildOptions, BuildPlan, CopyKind, CopySource, PlanError, StageBase, StepKind,
};
use sbx_model::{Arch, Signal};

fn run(text: &str, o: &BuildOptions) -> Result<BuildPlan, PlanError> {
    let mut asked = BTreeSet::new();
    plan(text, o, &mut |image: &str| {
        assert!(
            asked.insert(image.to_owned()),
            "base env of {image:?} asked twice"
        );
        if image == "fail" {
            return Err("no such image".into());
        }
        Ok(vec![
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("HOME".into(), "/root".into()),
        ])
    })
}

fn keyword(kind: &StepKind) -> &'static [&'static str] {
    match kind {
        StepKind::Run(_) => &["RUN "],
        StepKind::Copy(c) if c.kind == CopyKind::Add => &["ADD "],
        StepKind::Copy(_) => &["COPY "],
        StepKind::Env(_) => &["ENV "],
        StepKind::Workdir(_) => &["WORKDIR "],
        StepKind::User(_) => &["USER "],
        StepKind::Label(_) => &["LABEL "],
        StepKind::Expose(_) => &["EXPOSE "],
        StepKind::Volume(_) => &["VOLUME "],
        StepKind::Entrypoint(_) => &["ENTRYPOINT "],
        StepKind::Cmd(_) => &["CMD "],
        StepKind::StopSignal(_) => &["STOPSIGNAL "],
        StepKind::Shell(_) => &["SHELL "],
        StepKind::Healthcheck(_) => &["HEALTHCHECK "],
        StepKind::Maintainer(_) => &["MAINTAINER "],
    }
}

/// 원본이 여럿인 `COPY`: 해석은 대상을 쓴 그대로 두고, 디렉터리인지는 빌드가 정한다. 작업 디렉터리 `/w`, 빈 뿌리
/// (이미 있는 디렉터리는 `/`뿐)에서 빌드가 놓아 본다: 대상이 `.`이거나 `/`·`/.`로 끝나거나 정리해서 `/`이면 그
/// 디렉터리 안에 원본마다 제 이름으로 놓이고, 아니면 거부한다(없는 경로에 여럿을 덮어쓰지 않는다).
fn several_sources_need_a_directory(n: usize, raw: &str) {
    use sbx_build::copy;
    use sbx_build::source::{Content, Src, SrcEntry, SrcKind};
    let dest = copy::dest("/w", raw);
    let syntax = raw == "." || raw.ends_with('/') || raw.ends_with("/.");
    assert_eq!(
        dest.ends_with('/'),
        syntax || dest == "/",
        "{raw:?} became {dest:?}"
    );
    let srcs: Vec<Src> = (0..n)
        .map(|i| Src {
            name: format!("s{i}"),
            entries: vec![SrcEntry {
                rel: String::new(),
                kind: SrcKind::File {
                    size: 0,
                    content: Content::Host("/nonexistent".into(), None),
                },
                mode: 0o644,
                mtime: 0,
            }],
        })
        .collect();
    let opts = copy::Options {
        owner: None,
        chmod: None,
        now: 0,
    };
    match copy::place(&sbx_rootfs::MergedTree::new(), &srcs, &dest, opts) {
        Ok(p) => {
            assert!(
                dest.ends_with('/'),
                "{raw:?}: several sources placed into {dest:?}"
            );
            let dir = dest.trim_matches('/');
            for i in 0..n {
                let want = if dir.is_empty() {
                    format!("s{i}")
                } else {
                    format!("{dir}/s{i}")
                };
                assert!(
                    p.contains_key(&want),
                    "{raw:?}: no {want:?} in {:?}",
                    p.keys()
                );
            }
        }
        Err(e) => assert!(!dest.ends_with('/'), "{raw:?} ({dest:?}): {e}"),
    }
}

fn check(p: &BuildPlan, lines: usize, target: Option<&str>) {
    assert!(!p.stages.is_empty());
    for w in &p.warnings {
        if let Some(l) = w.line {
            assert!((1..=lines).contains(&l), "warning line {l} of {lines}");
        }
    }
    for (i, s) in p.stages.iter().enumerate() {
        assert!(s.text.starts_with("FROM "), "{}", s.text);
        assert!((1..=lines).contains(&s.line));
        let next_line = p.stages.get(i + 1).map_or(usize::MAX, |n| n.line);
        if let Some(n) = p.stages.get(i + 1) {
            assert!(n.position > s.position && n.line > s.line);
        }
        if let StageBase::Stage(j) = s.base {
            assert!(j < i, "stage {i} is based on stage {j}");
        }
        let mut last = s.line;
        for step in &s.steps {
            assert!(step.line > last && step.line < next_line && step.line <= lines);
            last = step.line;
            assert!(
                keyword(&step.kind).iter().any(|k| step.text.starts_with(k)),
                "{:?} for {:?}",
                step.text,
                step.kind
            );
            match &step.kind {
                StepKind::Run(r) => assert!(!r.command.argv().is_empty()),
                StepKind::Copy(c) => {
                    assert!(!c.sources.is_empty() && !c.dest.is_empty());
                    if c.sources.len() > 1 {
                        several_sources_need_a_directory(c.sources.len(), &c.dest);
                    }
                    if let CopySource::Stage(j) = c.from {
                        assert!(j < i, "stage {i} copies from stage {j}");
                    }
                    assert!(c.chmod.is_none_or(|m| m <= 0o7777));
                }
                StepKind::Expose(ports) => {
                    assert!(!ports.is_empty());
                    for port in ports {
                        let (n, proto) = port.split_once('/').expect("port/proto");
                        assert!(n.parse::<u16>().is_ok(), "{port}");
                        assert!(matches!(proto, "tcp" | "udp" | "sctp"), "{port}");
                    }
                }
                StepKind::StopSignal(s) => assert!(s.parse::<Signal>().is_ok(), "{s}"),
                StepKind::Healthcheck(h) => assert!(!h.test.is_empty()),
                StepKind::Shell(v) => assert!(!v.is_empty()),
                StepKind::Workdir(w) => assert!(!w.is_empty()),
                _ => {}
            }
        }
    }
    if let Some(t) = target {
        assert_eq!(
            p.output().name.as_deref(),
            Some(t.to_ascii_lowercase().as_str())
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let (text, trailer) = input.split_once('\0').unwrap_or((input, ""));
    let mut trailer = trailer.split('\n');
    let target = trailer.next().filter(|t| !t.is_empty()).map(str::to_owned);
    let build_args = trailer
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let o = BuildOptions {
        build_args,
        target,
        arch: Arch::Aarch64,
    };
    let lines = text.split('\n').count();

    let first = run(text, &o);
    assert_eq!(first, run(text, &o), "not deterministic");

    let mut more = text.to_owned();
    if !more.is_empty() && !more.ends_with('\n') {
        more.push('\n');
    }
    more.push_str("#sbx-fuzz\n");
    assert_eq!(
        first,
        run(&more, &o),
        "a trailing comment changed the result"
    );

    match &first {
        Err(e) => {
            if let Some(l) = e.line {
                assert!((1..=lines).contains(&l), "error line {l} of {lines}: {e}");
            }
        }
        Ok(p) => {
            check(p, lines, o.target.as_deref());
            for s in p.stages.iter().filter(|s| s.name.is_some()).take(4) {
                let name = s.name.clone().unwrap();
                let again = run(
                    text,
                    &BuildOptions {
                        target: Some(name.clone()),
                        ..o.clone()
                    },
                )
                .unwrap_or_else(|e| panic!("target {name}: {e}"));
                check(&again, lines, Some(&name));
                assert_eq!(again.output().steps, s.steps, "target {name}");
                assert_eq!(again.output().text, s.text);
            }
        }
    }
});
