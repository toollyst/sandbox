//! 명령들을 빌드 계획으로: 스테이지 나누기, `ARG` 범위, 변수 풀기, `--target`으로 스테이지 고르기.
//!
//! # 스테이지
//!
//! - 첫 `FROM` 앞에는 `ARG`만 온다(전역 `ARG`). 다른 명령은 오류다. `FROM`이 없으면 오류다.
//! - `FROM 이름`은 **앞** 스테이지의 `AS` 이름(대소문자 무관)이면 그 스테이지, `scratch`면 빈 바탕, 아니면 이미지다.
//!   뒤 스테이지의 이름이면 오류다(Docker는 경우에 따라 앞으로 당겨 읽는다. 여기서는 순서를 지킨다).
//! - `COPY --from=값`도 같다. 숫자면 스테이지 번호(0부터)이고 지금 스테이지보다 앞이어야 한다.
//! - 같은 `AS` 이름이 둘이면 오류다.
//! - 결과는 `--target` 스테이지(없으면 마지막)와, 그것이 `FROM`과 `COPY --from`으로 기대는 스테이지들이다.
//!   나머지는 계획에 넣지 않고 변수도 풀지 않는다(BuildKit처럼 필요 없는 스테이지의 풀기 오류는 나지 않는다).
//!
//! # 변수와 `ARG`
//!
//! - 푸는 명령: `FROM`(이미지, `--platform`), `COPY`·`ADD`(원본, 대상, `--from`, `--chown`, `--chmod`), `ENV`, `LABEL`,
//!   `ARG`(기본값), `WORKDIR`, `USER`, `EXPOSE`, `VOLUME`, `STOPSIGNAL`. 풀지 않는 명령(셸이나 런타임이 푼다): `RUN`,
//!   `CMD`, `ENTRYPOINT`, `SHELL`, `HEALTHCHECK`, `MAINTAINER`. 규칙은 [`crate::shell`]에 있다.
//! - `FROM`은 전역 `ARG`로만 푼다.
//! - 스테이지 안에서는 그 자리의 환경으로 푼다. 환경은 바탕의 config `Env`(스테이지 바탕이면 그 스테이지의 최종
//!   `Env`, `scratch`면 `PATH`)에서 시작하고, `ENV`와 값이 있는 `ARG`가 차례로 쓴다. 같은 이름은 나중에 쓴 것이
//!   이긴다(BuildKit과 같다). `ARG` 값은 `RUN`의 환경에 들어가지만([`RunStep::args`]) 이미지 config에는 남지 않는다.
//! - 한 `ENV`·`LABEL` 안의 여러 쌍은 모두 그 명령 앞의 환경으로 푼다(`ENV a=1 b=$a`의 `b`는 앞의 `a`).
//!   `ARG`는 한 쌍씩 차례로 푼다(BuildKit과 같다).
//! - 전역 `ARG`의 값: `--build-arg`가 있으면 그것, 없으면 기본값(앞의 전역 `ARG`로 푼다), 없으면 값이 없다.
//! - 스테이지의 `ARG 이름`: `--build-arg`, 기본값, 같은 이름의 전역 `ARG` 값 순서로 찾는다. 범위는 그 스테이지
//!   끝까지다. 전역 `ARG`는 스테이지 안에서 다시 선언해야 보인다.
//! - 이미 정해진 전역 `ARG`(BuildKit의 플랫폼 인자): `TARGETPLATFORM`, `TARGETOS`, `TARGETARCH`, `TARGETVARIANT`,
//!   `BUILDPLATFORM`, `BUILDOS`, `BUILDARCH`, `BUILDVARIANT`. sbx는 호스트 아키텍처로만 빌드하므로 둘 다
//!   `linux/<호스트 아키텍처>`이고 변형(variant)은 빈 값이다(`linux/arm64`의 `v8`은 BuildKit도 비운다).
//!   `--build-arg`로 바꿀 수 없다(오류다). 프록시 인자(`HTTP_PROXY` 등)는 따로 대하지 않는다: 다른 이름과 같이
//!   `ARG`로 선언해야 쓰인다.
//! - 어느 `ARG`로도 쓰이지 않은 `--build-arg`는 경고다(Docker와 같다).
//!
//! # 값 검사
//!
//! - `FROM --platform`: `linux/<호스트 아키텍처>`만(`x86_64`·`aarch64` 표기, arm64의 `v8`도 받는다).
//! - `ADD`는 로컬 파일만이다. 원본이 `http://`·`https://`·`git@`·`git://`로 시작하면 오류다. `COPY`의 URL도 오류다.
//! - 원본이 둘 이상이면 대상은 `/`로 끝나야 한다(Docker는 빌드할 때 거부한다. 여기서 미리 거부한다).
//! - `--chown`은 `사용자[:그룹]`(이름이나 숫자), `--chmod`는 8진수(`0`–`7777`)만이다. 기호 모드(`u+x`)는 아직 받지 않는다.
//! - `EXPOSE`: `포트[/tcp|udp|sctp]`, `시작-끝` 범위는 펼친다. 호스트 주소나 호스트 포트(`:`)는 오류다.
//! - `STOPSIGNAL`: [`sbx_model::Signal`]로 읽혀야 한다.
//! - `VOLUME`의 빈 경로, 풀어서 빈 `FROM` 이미지, 빈 `WORKDIR`, 빈 `ENV` 이름은 오류다.
//!
//! # 경고
//!
//! - `# syntax=`: 다른 프런트엔드를 고르지 않는다. sbx가 Dockerfile을 직접 읽는다.
//! - `# check=`: 검사(lint)를 돌리지 않는다.
//! - `HEALTHCHECK`: config에 쓰지만 sbx는 검사를 돌리지 않는다. 필요한 스테이지에 있을 때만 알린다.
//! - `MAINTAINER`: Docker가 폐기한 명령이다. config `Author`에 쓴다.
//! - 이어지는 줄 가운데 빈 줄(Docker가 폐기 예정으로 경고한다).
//!
//! # 크기
//!
//! 풀어서 나오는 값의 총합이 [`MAX_EXPANDED_BYTES`]를 넘으면 오류다(`ENV a=$a$a`를 되풀이하면 지수로 커진다).

use std::collections::{BTreeMap, BTreeSet};

use sbx_model::build::{
    BuildOptions, BuildPlan, Command, CopyKind, CopySource, CopyStep, DEFAULT_PATH, PlanError,
    PlanWarning, RunStep, Stage, StageBase, Step, StepKind,
};
use sbx_model::{Arch, Signal, UserSpec};

use crate::instruction::{self, Instr, Kind};
use crate::json;
use crate::lines;
use crate::shell;
use crate::syntax::{self, Form};

/// 풀어서 나오는 값의 총합 상한.
pub const MAX_EXPANDED_BYTES: usize = 64 << 20;

/// 값 하나마다 더 세는 바이트(빈 값이 많아도 상한에 닿게).
const PER_VALUE: usize = 32;

/// 이미지의 config `Env`를 묻는 함수. 이미지 참조(풀린 문자열)를 받는다. 오류는 그 `FROM` 줄의 오류가 된다.
pub type BaseEnv<'a> = &'a mut dyn FnMut(&str) -> Result<Vec<(String, String)>, String>;

struct RawStage {
    position: usize,
    line: usize,
    image: String,
    name: Option<String>,
    platform: Option<String>,
    instrs: Vec<Instr>,
}

/// 풀린 `FROM`.
struct From {
    base: StageBase,
    text: String,
}

/// 바탕 스테이지에서 물려받는 것: config `Env`와 `SHELL`.
type Inherited = (Vec<(String, String)>, Vec<String>);

/// 스테이지 하나를 푼 결과.
struct Done {
    steps: Vec<Step>,
    /// config `Env`(자식 스테이지가 물려받는다).
    config_env: Vec<(String, String)>,
    shell: Vec<String>,
    deps: Vec<usize>,
}

/// 환경의 값이 어디서 왔는지.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Env,
    Arg,
}

struct Planner<'a> {
    escape: char,
    arch: Arch,
    build_args: BTreeMap<String, String>,
    consumed: BTreeSet<String>,
    global: Vec<(String, Option<String>)>,
    budget: usize,
    base_env: BaseEnv<'a>,
    image_env: BTreeMap<String, Vec<(String, String)>>,
    warnings: Vec<PlanWarning>,
}

/// Dockerfile을 빌드 계획으로 바꾼다. 규칙은 이 모듈과 [`crate`] 문서에 있다.
pub fn plan(
    dockerfile: &str,
    options: &BuildOptions,
    base_env: BaseEnv<'_>,
) -> Result<BuildPlan, PlanError> {
    let (directives, logical) = lines::split(dockerfile)?;
    let escape = directives.escape;
    let mut warnings = Vec::new();
    if let Some((line, value)) = &directives.syntax {
        warnings.push(PlanWarning {
            line: Some(*line),
            message: format!(
                "the syntax directive ({value}) is not used: sbx reads the Dockerfile itself"
            ),
        });
    }
    if let Some((line, _)) = &directives.check {
        warnings.push(PlanWarning {
            line: Some(*line),
            message: "the check directive is not used: sbx does not run Dockerfile checks".into(),
        });
    }

    let mut meta = Vec::new();
    let mut stages: Vec<RawStage> = Vec::new();
    for l in &logical {
        let ins = instruction::parse(l, escape)?;
        if l.empty_continuation {
            warnings.push(PlanWarning {
                line: Some(l.line),
                message: "empty continuation line (Docker deprecates these)".into(),
            });
        }
        match ins.kind {
            Kind::From {
                image,
                name,
                platform,
            } => {
                if let Some(n) = &name
                    && let Some(other) = stages.iter().find(|s| s.name.as_ref() == Some(n))
                {
                    return Err(PlanError::at(
                        l.line,
                        format!(
                            "duplicate stage name {n:?} (first used on line {})",
                            other.line
                        ),
                    ));
                }
                stages.push(RawStage {
                    position: stages.len(),
                    line: l.line,
                    image,
                    name,
                    platform,
                    instrs: Vec::new(),
                });
            }
            kind => match stages.last_mut() {
                Some(s) => s.instrs.push(Instr {
                    line: ins.line,
                    kind,
                }),
                None if matches!(kind, Kind::Arg(_)) => meta.push(Instr {
                    line: ins.line,
                    kind,
                }),
                None => {
                    let (cmd, _) = syntax::split_command(&l.text);
                    return Err(PlanError::at(
                        l.line,
                        format!(
                            "{} cannot come before the first FROM (only ARG can)",
                            cmd.to_ascii_uppercase()
                        ),
                    ));
                }
            },
        }
    }
    if stages.is_empty() {
        return Err(PlanError::whole("the Dockerfile has no FROM instruction"));
    }

    let mut p = Planner {
        escape,
        arch: options.arch,
        build_args: options.build_args.iter().cloned().collect(),
        consumed: BTreeSet::new(),
        global: Vec::new(),
        budget: MAX_EXPANDED_BYTES,
        base_env,
        image_env: BTreeMap::new(),
        warnings,
    };
    p.global_args(&meta)?;
    let froms = stages
        .iter()
        .map(|s| p.from(s, &stages))
        .collect::<Result<Vec<_>, _>>()?;

    let target = match &options.target {
        None => stages.len() - 1,
        Some(t) => {
            let lower = t.to_ascii_lowercase();
            stages
                .iter()
                .position(|s| s.name.as_deref() == Some(lower.as_str()))
                .ok_or_else(|| PlanError::whole(format!("target stage {t:?} could not be found")))?
        }
    };

    // 필요한 스테이지를 뒤에서부터 찾는다. 스테이지를 풀려면 바탕 스테이지가 먼저 풀려 있어야 한다.
    let mut done: Vec<Option<Done>> = (0..stages.len()).map(|_| None).collect();
    let mut stack = vec![target];
    while let Some(&i) = stack.last() {
        if done[i].is_some() {
            stack.pop();
            continue;
        }
        let parent = match froms[i].base {
            StageBase::Stage(j) => match &done[j] {
                Some(d) => Some((d.config_env.clone(), d.shell.clone())),
                None => {
                    stack.push(j);
                    continue;
                }
            },
            _ => None,
        };
        stack.pop();
        let d = p.stage(&stages[i], &froms[i].base, parent, &stages)?;
        for &dep in &d.deps {
            if done[dep].is_none() {
                stack.push(dep);
            }
        }
        done[i] = Some(d);
    }

    let mut index = BTreeMap::new();
    for (i, d) in done.iter().enumerate() {
        if d.is_some() {
            index.insert(i, index.len());
        }
    }
    let remap = |i: &usize| index[i];
    let mut out = Vec::with_capacity(index.len());
    for (i, (raw, from)) in stages.iter().zip(froms).enumerate() {
        let Some(d) = done[i].take() else {
            continue;
        };
        let base = match from.base {
            StageBase::Stage(j) => StageBase::Stage(remap(&j)),
            b => b,
        };
        let steps = d
            .steps
            .into_iter()
            .map(|mut s| {
                if let StepKind::Copy(c) = &mut s.kind
                    && let CopySource::Stage(j) = &c.from
                {
                    c.from = CopySource::Stage(remap(j));
                }
                s
            })
            .collect();
        out.push(Stage {
            name: raw.name.clone(),
            position: raw.position,
            line: raw.line,
            text: from.text,
            base,
            steps,
        });
    }

    let mut unused: Vec<&String> = p
        .build_args
        .keys()
        .filter(|k| !p.consumed.contains(*k))
        .collect();
    unused.sort();
    if !unused.is_empty() {
        let names: Vec<&str> = unused.iter().map(|s| s.as_str()).collect();
        p.warnings.push(PlanWarning {
            line: None,
            message: format!(
                "build arguments not used by any ARG in the stages built: {}",
                names.join(", ")
            ),
        });
    }
    Ok(BuildPlan {
        stages: out,
        warnings: p.warnings,
    })
}

/// 이름 목록에서 찾는다.
fn find<'v, T>(list: &'v [(String, T)], name: &str) -> Option<&'v T> {
    list.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v)
}

/// 이름 목록에 넣는다. 있으면 그 자리에서 바꾼다.
fn set<T>(list: &mut Vec<(String, T)>, name: String, value: T) {
    match list.iter_mut().find(|(k, _)| *k == name) {
        Some(slot) => slot.1 = value,
        None => list.push((name, value)),
    }
}

/// 진행 표시와 캐시 키에 쓰는 값 하나. 비었거나 공백, 따옴표, 역슬래시가 있으면 JSON 문자열로 쓴다.
fn shown(v: &str) -> String {
    if v.is_empty() || v.chars().any(|c| c.is_whitespace() || "\"'\\".contains(c)) {
        json::quote(v)
    } else {
        v.to_owned()
    }
}

/// 값 목록. JSON이었거나 따로 써야 하는 값이 있으면 JSON 배열로 쓴다.
fn shown_list(items: &[String], json_form: bool) -> String {
    if json_form || items.iter().any(|v| shown(v) != *v) {
        json::array(items)
    } else {
        items.join(" ")
    }
}

fn shown_pairs(pairs: &[(String, String)]) -> String {
    let parts: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", shown(k), shown(v)))
        .collect();
    parts.join(" ")
}

fn platform_args(arch: Arch) -> Vec<(String, Option<String>)> {
    let a = arch.oci_name();
    let platform = format!("linux/{a}");
    let mut out = Vec::new();
    for side in ["TARGET", "BUILD"] {
        out.push((format!("{side}PLATFORM"), Some(platform.clone())));
        out.push((format!("{side}OS"), Some("linux".into())));
        out.push((format!("{side}ARCH"), Some(a.into())));
        out.push((format!("{side}VARIANT"), Some(String::new())));
    }
    out
}

fn platform_arch(p: &str) -> Option<Arch> {
    let lower = p.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('/').collect();
    match parts.as_slice() {
        ["linux", "amd64" | "x86_64" | "x86-64"] => Some(Arch::X86_64),
        ["linux", "arm64" | "aarch64"] | ["linux", "arm64" | "aarch64", "v8"] => {
            Some(Arch::Aarch64)
        }
        _ => None,
    }
}

fn url_like(s: &str) -> Option<&'static str> {
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        Some("URL")
    } else if lower.starts_with("git@") || lower.starts_with("git://") {
        Some("git URL")
    } else {
        None
    }
}

fn expose(spec: &str) -> Result<Vec<String>, String> {
    let bad = || format!("invalid port {spec:?} in EXPOSE");
    if spec.contains(':') {
        return Err(format!(
            "EXPOSE {spec}: host addresses and host ports are not allowed (EXPOSE only names container ports)"
        ));
    }
    let (ports, proto) = match spec.split_once('/') {
        Some((p, proto)) => (p, proto.to_ascii_lowercase()),
        None => (spec, "tcp".into()),
    };
    if !matches!(proto.as_str(), "tcp" | "udp" | "sctp") {
        return Err(format!("invalid protocol {proto:?} in EXPOSE {spec}"));
    }
    let (start, end) = match ports.split_once('-') {
        Some((a, b)) => (a, b),
        None => (ports, ports),
    };
    let start: u16 = start.parse().map_err(|_| bad())?;
    let end: u16 = end.parse().map_err(|_| bad())?;
    if start > end {
        return Err(bad());
    }
    Ok((start..=end).map(|p| format!("{p}/{proto}")).collect())
}

fn chmod(v: &str) -> Result<u32, String> {
    let bad = || format!("invalid --chmod={v} (only octal modes such as 0755 are supported)");
    if v.is_empty() || !v.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err(bad());
    }
    let m = u32::from_str_radix(v, 8).map_err(|_| bad())?;
    if m > 0o7777 {
        return Err(bad());
    }
    Ok(m)
}

impl Planner<'_> {
    fn charge(&mut self, n: usize, line: usize) -> Result<(), PlanError> {
        let n = n.saturating_add(PER_VALUE);
        if n > self.budget {
            return Err(PlanError::at(
                line,
                format!("the Dockerfile expands to more than {MAX_EXPANDED_BYTES} bytes"),
            ));
        }
        self.budget -= n;
        Ok(())
    }

    fn charge_pairs(&mut self, pairs: &[(String, String)], line: usize) -> Result<(), PlanError> {
        let n: usize = pairs
            .iter()
            .map(|(k, v)| k.len() + v.len() + PER_VALUE)
            .sum();
        self.charge(n, line)
    }

    /// `text`를 `lookup`으로 푼다.
    fn word(
        &mut self,
        text: &str,
        line: usize,
        lookup: shell::Lookup<'_>,
    ) -> Result<String, PlanError> {
        let v = shell::word(text, self.escape, lookup, self.budget)
            .map_err(|e| PlanError::at(line, e))?;
        self.charge(v.len(), line)?;
        Ok(v)
    }

    fn words(
        &mut self,
        text: &str,
        line: usize,
        lookup: shell::Lookup<'_>,
    ) -> Result<Vec<String>, PlanError> {
        let v = shell::words(text, self.escape, lookup, self.budget)
            .map_err(|e| PlanError::at(line, e))?;
        self.charge(v.iter().map(|w| w.len() + PER_VALUE).sum(), line)?;
        Ok(v)
    }

    fn build_arg(&mut self, name: &str) -> Option<String> {
        let v = self.build_args.get(name).cloned();
        if v.is_some() {
            self.consumed.insert(name.to_owned());
        }
        v
    }

    fn global_args(&mut self, meta: &[Instr]) -> Result<(), PlanError> {
        self.global = platform_args(self.arch);
        for (name, _) in &self.global {
            if self.build_args.contains_key(name) {
                return Err(PlanError::whole(format!(
                    "--build-arg {name}: {name} is set by sbx (it builds for the host platform only)"
                )));
            }
        }
        for ins in meta {
            let Kind::Arg(pairs) = &ins.kind else {
                continue;
            };
            for (name, default) in pairs {
                self.consumed.insert(name.clone());
                let value = match (self.build_arg(name), default) {
                    (Some(v), _) => Some(v),
                    (None, Some(d)) => {
                        let global = std::mem::take(&mut self.global);
                        let lookup = |n: &str| find(&global, n).cloned().flatten();
                        let v = self.word(d, ins.line, &lookup);
                        self.global = global;
                        Some(v?)
                    }
                    (None, None) => None,
                };
                set(&mut self.global, name.clone(), value);
            }
        }
        Ok(())
    }

    /// `FROM`을 전역 `ARG`로 풀고 바탕을 정한다.
    fn from(&mut self, s: &RawStage, all: &[RawStage]) -> Result<From, PlanError> {
        let global = std::mem::take(&mut self.global);
        let lookup = |n: &str| find(&global, n).cloned().flatten();
        let image = self.word(&s.image, s.line, &lookup);
        let platform = s
            .platform
            .as_ref()
            .map(|p| self.word(p, s.line, &lookup))
            .transpose();
        self.global = global;
        let (image, platform) = (image?, platform?);
        if image.is_empty() {
            return Err(PlanError::at(s.line, "base name should not be blank"));
        }
        if let Some(p) = &platform {
            let host = self.arch.oci_name();
            if platform_arch(p) != Some(self.arch) {
                return Err(PlanError::at(
                    s.line,
                    format!(
                        "FROM --platform={p}: sbx builds only for the host platform linux/{host}"
                    ),
                ));
            }
        }
        let base = if image == "scratch" {
            StageBase::Scratch
        } else {
            match stage_ref(&image, s.position, all, false) {
                Ok(Some(i)) => StageBase::Stage(i),
                Ok(None) => StageBase::Image(image.clone()),
                Err(e) => return Err(PlanError::at(s.line, e)),
            }
        };
        let mut text = String::from("FROM ");
        if let Some(p) = &platform {
            text.push_str(&format!("--platform={} ", shown(p)));
        }
        text.push_str(&shown(&image));
        if let Some(n) = &s.name {
            text.push_str(&format!(" AS {n}"));
        }
        Ok(From { base, text })
    }

    fn image_env(&mut self, image: &str, line: usize) -> Result<Vec<(String, String)>, PlanError> {
        if let Some(env) = self.image_env.get(image) {
            return Ok(env.clone());
        }
        let env =
            (self.base_env)(image).map_err(|e| PlanError::at(line, format!("{image}: {e}")))?;
        self.charge_pairs(&env, line)?;
        self.image_env.insert(image.to_owned(), env.clone());
        Ok(env)
    }

    /// 스테이지 하나를 푼다.
    fn stage(
        &mut self,
        s: &RawStage,
        base: &StageBase,
        parent: Option<Inherited>,
        all: &[RawStage],
    ) -> Result<Done, PlanError> {
        let (mut config_env, mut shell_cmd) = match (base, parent) {
            (_, Some((env, shell))) => {
                self.charge_pairs(&env, s.line)?;
                (env, shell)
            }
            (StageBase::Image(r), None) => (self.image_env(r, s.line)?, default_shell()),
            _ => (vec![("PATH".into(), DEFAULT_PATH.into())], default_shell()),
        };
        // 그 자리의 환경(풀기와 RUN). ENV와 ARG가 차례로 쓴다.
        let mut state: Vec<(String, (String, Origin))> = config_env
            .iter()
            .map(|(k, v)| (k.clone(), (v.clone(), Origin::Env)))
            .collect();
        let mut steps = Vec::new();
        let mut deps = Vec::new();

        for ins in &s.instrs {
            let line = ins.line;
            let lookup = |n: &str| find(&state, n).map(|(v, _)| v.clone());
            let step = |text: String, kind: StepKind| Step { line, text, kind };
            match &ins.kind {
                Kind::From { .. } => unreachable!("FROM starts a stage"),
                Kind::Arg(pairs) => {
                    for (name, default) in pairs {
                        self.consumed.insert(name.clone());
                        let value = match (self.build_arg(name), default) {
                            (Some(v), _) => Some(v),
                            (None, Some(d)) => {
                                let lookup = |n: &str| find(&state, n).map(|(v, _)| v.clone());
                                Some(self.word(d, line, &lookup)?)
                            }
                            (None, None) => find(&self.global, name).cloned().flatten(),
                        };
                        if let Some(v) = value {
                            set(&mut state, name.clone(), (v, Origin::Arg));
                        }
                    }
                }
                Kind::Env(pairs) | Kind::Label(pairs) => {
                    let env = matches!(ins.kind, Kind::Env(_));
                    let mut out = Vec::with_capacity(pairs.len());
                    for (k, v) in pairs {
                        let k = self.word(k, line, &lookup)?;
                        let v = self.word(v, line, &lookup)?;
                        if k.is_empty() {
                            let what = if env { "ENV" } else { "LABEL" };
                            return Err(PlanError::at(
                                line,
                                format!("{what} names can not be blank"),
                            ));
                        }
                        out.push((k, v));
                    }
                    if env {
                        for (k, v) in &out {
                            set(&mut config_env, k.clone(), v.clone());
                            set(&mut state, k.clone(), (v.clone(), Origin::Env));
                        }
                        steps.push(step(
                            format!("ENV {}", shown_pairs(&out)),
                            StepKind::Env(out),
                        ));
                    } else {
                        steps.push(step(
                            format!("LABEL {}", shown_pairs(&out)),
                            StepKind::Label(out),
                        ));
                    }
                }
                Kind::Run(form) => {
                    let (command, text) = self.command(form, &shell_cmd, line)?;
                    let args: Vec<(String, String)> = state
                        .iter()
                        .filter(|(_, (_, o))| *o == Origin::Arg)
                        .map(|(k, (v, _))| (k.clone(), v.clone()))
                        .collect();
                    self.charge_pairs(&args, line)?;
                    steps.push(step(
                        format!("RUN {text}"),
                        StepKind::Run(RunStep { command, args }),
                    ));
                }
                Kind::Cmd(form) => {
                    let (command, text) = self.command(form, &shell_cmd, line)?;
                    steps.push(step(format!("CMD {text}"), StepKind::Cmd(command)));
                }
                Kind::Entrypoint(form) => {
                    let (command, text) = self.command(form, &shell_cmd, line)?;
                    steps.push(step(
                        format!("ENTRYPOINT {text}"),
                        StepKind::Entrypoint(command),
                    ));
                }
                Kind::Shell(v) => {
                    self.charge(v.iter().map(String::len).sum(), line)?;
                    shell_cmd = v.clone();
                    steps.push(step(
                        format!("SHELL {}", json::array(v)),
                        StepKind::Shell(v.clone()),
                    ));
                }
                Kind::Workdir(raw) | Kind::User(raw) | Kind::StopSignal(raw) => {
                    let v = self.word(raw, line, &lookup)?;
                    let (name, kind) = match &ins.kind {
                        Kind::Workdir(_) => {
                            if v.is_empty() {
                                return Err(PlanError::at(line, "WORKDIR can not be empty"));
                            }
                            ("WORKDIR", StepKind::Workdir(v.clone()))
                        }
                        Kind::User(_) => ("USER", StepKind::User(v.clone())),
                        _ => {
                            if v.parse::<Signal>().is_err() {
                                return Err(PlanError::at(
                                    line,
                                    format!("invalid STOPSIGNAL {v:?}"),
                                ));
                            }
                            ("STOPSIGNAL", StepKind::StopSignal(v.clone()))
                        }
                    };
                    steps.push(step(format!("{name} {}", shown(&v)), kind));
                }
                Kind::Maintainer(v) => {
                    self.charge(v.len(), line)?;
                    self.warnings.push(PlanWarning {
                        line: Some(line),
                        message: "MAINTAINER is deprecated (Docker writes it as the image author; use a LABEL)".into(),
                    });
                    steps.push(step(
                        format!("MAINTAINER {v}"),
                        StepKind::Maintainer(v.clone()),
                    ));
                }
                Kind::Expose(raw) => {
                    let mut specs = Vec::new();
                    for r in raw {
                        specs.extend(self.words(r, line, &lookup)?);
                    }
                    let mut ports = Vec::new();
                    for spec in &specs {
                        let p = expose(spec).map_err(|e| PlanError::at(line, e))?;
                        self.charge(p.len() * PER_VALUE, line)?;
                        ports.extend(p);
                    }
                    if ports.is_empty() {
                        return Err(PlanError::at(line, "EXPOSE requires at least one argument"));
                    }
                    steps.push(step(
                        format!("EXPOSE {}", specs.join(" ")),
                        StepKind::Expose(ports),
                    ));
                }
                Kind::Volume { paths, json: j } => {
                    let mut out = Vec::with_capacity(paths.len());
                    for raw in paths {
                        let v = self.word(raw, line, &lookup)?;
                        if v.is_empty() {
                            return Err(PlanError::at(
                                line,
                                "VOLUME specified can not be an empty string",
                            ));
                        }
                        out.push(v);
                    }
                    steps.push(step(
                        format!("VOLUME {}", shown_list(&out, *j)),
                        StepKind::Volume(out),
                    ));
                }
                Kind::Healthcheck(h, raw) => {
                    self.charge(raw.len(), line)?;
                    self.warnings.push(PlanWarning {
                        line: Some(line),
                        message: "HEALTHCHECK is written to the image config, but sbx does not run health checks".into(),
                    });
                    steps.push(step(
                        format!("HEALTHCHECK {raw}"),
                        StepKind::Healthcheck(h.clone()),
                    ));
                }
                Kind::Copy {
                    add,
                    from,
                    chown,
                    chmod: mode,
                    args,
                    json: j,
                } => {
                    let name = if *add { "ADD" } else { "COPY" };
                    let mut text = String::from(name);
                    let source = match from {
                        None => CopySource::Context,
                        Some(raw) => {
                            let v = self.word(raw, line, &lookup)?;
                            text.push_str(&format!(" --from={}", shown(&v)));
                            if v.is_empty() {
                                return Err(PlanError::at(
                                    line,
                                    "COPY --from needs a stage or an image",
                                ));
                            }
                            match stage_ref(&v, s.position, all, true) {
                                Ok(Some(i)) => {
                                    deps.push(i);
                                    CopySource::Stage(i)
                                }
                                Ok(None) => CopySource::Image(v),
                                Err(e) => return Err(PlanError::at(line, e)),
                            }
                        }
                    };
                    let owner = match chown {
                        None => None,
                        Some(raw) => {
                            let v = self.word(raw, line, &lookup)?;
                            text.push_str(&format!(" --chown={}", shown(&v)));
                            let parsed = v
                                .parse::<UserSpec>()
                                .ok()
                                .filter(|_| v.matches(':').count() < 2);
                            Some(parsed.ok_or_else(|| {
                                PlanError::at(
                                    line,
                                    format!("invalid --chown={v} (want USER[:GROUP])"),
                                )
                            })?)
                        }
                    };
                    let mode = match mode {
                        None => None,
                        Some(raw) => {
                            let v = self.word(raw, line, &lookup)?;
                            text.push_str(&format!(" --chmod={}", shown(&v)));
                            Some(chmod(&v).map_err(|e| PlanError::at(line, e))?)
                        }
                    };
                    let mut values = Vec::with_capacity(args.len());
                    for raw in args {
                        values.push(self.word(raw, line, &lookup)?);
                    }
                    text.push(' ');
                    text.push_str(&shown_list(&values, *j));
                    let dest = values.pop().unwrap_or_default();
                    for src in &values {
                        if let Some(what) = url_like(src) {
                            let msg = if *add {
                                format!("ADD <{what}> is not supported yet (source {src})")
                            } else {
                                format!(
                                    "COPY does not take URLs (source {src}); ADD <URL> is not supported yet either"
                                )
                            };
                            return Err(PlanError::at(line, msg));
                        }
                        if src.is_empty() {
                            return Err(PlanError::at(
                                line,
                                format!("{name} source can not be empty"),
                            ));
                        }
                    }
                    if dest.is_empty() {
                        return Err(PlanError::at(
                            line,
                            format!("{name} destination can not be empty"),
                        ));
                    }
                    if values.len() > 1 && !dest.ends_with('/') {
                        return Err(PlanError::at(
                            line,
                            format!(
                                "when using {name} with more than one source file, the destination must be a directory and end with a /"
                            ),
                        ));
                    }
                    steps.push(step(
                        text,
                        StepKind::Copy(CopyStep {
                            kind: if *add { CopyKind::Add } else { CopyKind::Copy },
                            from: source,
                            sources: values,
                            dest,
                            chown: owner,
                            chmod: mode,
                        }),
                    ));
                }
            }
        }
        Ok(Done {
            steps,
            config_env,
            shell: shell_cmd,
            deps,
        })
    }

    fn command(
        &mut self,
        form: &Form,
        shell_cmd: &[String],
        line: usize,
    ) -> Result<(Command, String), PlanError> {
        Ok(match form {
            Form::Json(v) => {
                self.charge(v.iter().map(|s| s.len() + PER_VALUE).sum(), line)?;
                (Command::Exec(v.clone()), json::array(v))
            }
            Form::Text(t) => {
                self.charge(
                    t.len() + shell_cmd.iter().map(|s| s.len() + PER_VALUE).sum::<usize>(),
                    line,
                )?;
                (
                    Command::Shell {
                        shell: shell_cmd.to_vec(),
                        script: t.clone(),
                    },
                    t.clone(),
                )
            }
        })
    }
}

fn default_shell() -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into()]
}

/// 스테이지 참조. 앞 스테이지면 그 번호, 스테이지가 아니면 `None`, 뒤 스테이지면 오류다.
///
/// `FROM`(`copy`가 아님)에서는 지금 스테이지의 이름이 이미지다(`FROM golang AS golang`). `COPY --from`에서는 오류다.
fn stage_ref(
    value: &str,
    position: usize,
    all: &[RawStage],
    copy: bool,
) -> Result<Option<usize>, String> {
    if copy && !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return match value.parse::<usize>() {
            Ok(n) if n < position => Ok(Some(n)),
            _ => Err(format!(
                "--from={value} refers to the current or a later build stage"
            )),
        };
    }
    let lower = value.to_ascii_lowercase();
    match all
        .iter()
        .find(|s| s.name.as_deref() == Some(lower.as_str()))
    {
        Some(s) if s.position < position => Ok(Some(s.position)),
        Some(s) if s.position == position && !copy => Ok(None),
        Some(s) => Err(format!(
            "{value:?} refers to the current or a later build stage (line {}); stages can only use earlier stages",
            s.line
        )),
        None => Ok(None),
    }
}
