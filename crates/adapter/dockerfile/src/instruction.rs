//! 명령 읽기: 이은 줄 하나를 명령 하나로 바꾸고, 받지 않는 것을 줄 번호와 함께 거부한다.
//!
//! 여기서 하는 검사는 변수를 풀지 않고 알 수 있는 것이다(명령 이름, 플래그, 인자 개수, heredoc, JSON 형식).
//! 그래서 `--target`에 필요 없는 스테이지에 있어도 오류다. 풀어야 알 수 있는 것(`ADD`의 URL, 포트, 시그널)은
//! [`crate::plan`]이 필요한 스테이지에서만 본다.

use sbx_model::build::{Healthcheck, PlanError};

use crate::duration;
use crate::lines::Logical;
use crate::shell;
use crate::syntax::{self, FlagKind, Form};

/// 명령 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instr {
    /// 시작 줄.
    pub line: usize,
    /// 명령.
    pub kind: Kind,
}

/// 명령. 값은 풀기 전 원문이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// `FROM [--platform=..] 이미지 [AS 이름]`. 이름은 소문자.
    From {
        image: String,
        name: Option<String>,
        platform: Option<String>,
    },
    /// `ARG 이름[=기본값] ...`.
    Arg(Vec<(String, Option<String>)>),
    /// `RUN`.
    Run(Form),
    /// `CMD`.
    Cmd(Form),
    /// `ENTRYPOINT`.
    Entrypoint(Form),
    /// `SHELL`(JSON만).
    Shell(Vec<String>),
    /// `COPY`, `ADD`.
    Copy {
        add: bool,
        from: Option<String>,
        chown: Option<String>,
        chmod: Option<String>,
        /// 원본들과 마지막의 대상. 둘 이상이다.
        args: Vec<String>,
        /// JSON 형식이었는지.
        json: bool,
    },
    /// `ENV`.
    Env(Vec<(String, String)>),
    /// `LABEL`.
    Label(Vec<(String, String)>),
    /// `WORKDIR`.
    Workdir(String),
    /// `USER`.
    User(String),
    /// `STOPSIGNAL`.
    StopSignal(String),
    /// `MAINTAINER`.
    Maintainer(String),
    /// `EXPOSE`. 공백으로 나눈 원문.
    Expose(Vec<String>),
    /// `VOLUME`.
    Volume { paths: Vec<String>, json: bool },
    /// `HEALTHCHECK`(풀지 않는다). 두 번째 값은 원문(진행 표시용).
    Healthcheck(Healthcheck, String),
}

const RUN_FLAGS: &[(&str, FlagKind)] = &[
    (
        "mount",
        FlagKind::Unsupported("RUN --mount is not supported yet"),
    ),
    (
        "network",
        FlagKind::Unsupported("RUN --network is not supported (use sbx build --allow)"),
    ),
    (
        "security",
        FlagKind::Unsupported("RUN --security is not supported"),
    ),
];

const COPY_FLAGS: &[(&str, FlagKind)] = &[
    ("from", FlagKind::Value),
    ("chown", FlagKind::Value),
    ("chmod", FlagKind::Value),
    (
        "link",
        FlagKind::Unsupported("COPY --link is not supported yet"),
    ),
    (
        "parents",
        FlagKind::Unsupported("COPY --parents is not supported yet"),
    ),
    (
        "exclude",
        FlagKind::Unsupported("COPY --exclude is not supported yet"),
    ),
];

const ADD_FLAGS: &[(&str, FlagKind)] = &[
    ("chown", FlagKind::Value),
    ("chmod", FlagKind::Value),
    (
        "link",
        FlagKind::Unsupported("ADD --link is not supported yet"),
    ),
    (
        "checksum",
        FlagKind::Unsupported(
            "ADD --checksum is not supported yet (ADD <URL> is not supported yet)",
        ),
    ),
    (
        "keep-git-dir",
        FlagKind::Unsupported(
            "ADD --keep-git-dir is not supported yet (ADD <git URL> is not supported yet)",
        ),
    ),
    (
        "exclude",
        FlagKind::Unsupported("ADD --exclude is not supported yet"),
    ),
    (
        "unpack",
        FlagKind::Unsupported("ADD --unpack is not supported yet"),
    ),
];

const FROM_FLAGS: &[(&str, FlagKind)] = &[("platform", FlagKind::Value)];

const HEALTHCHECK_FLAGS: &[(&str, FlagKind)] = &[
    ("interval", FlagKind::Value),
    ("timeout", FlagKind::Value),
    ("start-period", FlagKind::Value),
    ("start-interval", FlagKind::Value),
    ("retries", FlagKind::Value),
];

/// 이은 줄 하나를 읽는다.
pub fn parse(l: &Logical, escape: char) -> Result<Instr, PlanError> {
    let err = |m: String| PlanError::at(l.line, m);
    let (cmd, rest) = syntax::split_command(&l.text);
    let upper = cmd.to_ascii_uppercase();
    let (flags, rest) = syntax::extract_flags(rest);
    let rest = rest.trim_start();
    let table: &[(&str, FlagKind)] = match cmd.as_str() {
        "from" => FROM_FLAGS,
        "run" => RUN_FLAGS,
        "copy" => COPY_FLAGS,
        "add" => ADD_FLAGS,
        "healthcheck" => HEALTHCHECK_FLAGS,
        _ => &[],
    };
    let known = matches!(
        cmd.as_str(),
        "from"
            | "arg"
            | "run"
            | "cmd"
            | "entrypoint"
            | "shell"
            | "copy"
            | "add"
            | "env"
            | "label"
            | "workdir"
            | "user"
            | "stopsignal"
            | "maintainer"
            | "expose"
            | "volume"
            | "healthcheck"
    );
    if cmd == "onbuild" {
        return Err(err("ONBUILD is not supported yet".into()));
    }
    if !known {
        return Err(err(format!("unknown instruction: {upper}")));
    }
    let flags = syntax::parse_flags(&flags, table).map_err(err)?;
    let flag = |name: &str| {
        flags
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    };
    let need = |ok: bool, what: &str| {
        if ok {
            Ok(())
        } else {
            Err(err(format!("{upper} {what}")))
        }
    };

    let kind = match cmd.as_str() {
        "from" => {
            let words = syntax::fields(rest);
            let name = match words.as_slice() {
                [_] => None,
                [_, as_, name] if as_.eq_ignore_ascii_case("as") => {
                    let name = name.to_ascii_lowercase();
                    if !valid_stage_name(&name) {
                        return Err(err(format!(
                            "invalid name for build stage: {name:?}, name can't start with a number or contain symbols"
                        )));
                    }
                    Some(name)
                }
                _ => {
                    return Err(err("FROM requires either one or three arguments".into()));
                }
            };
            Kind::From {
                image: words[0].clone(),
                name,
                platform: flag("platform"),
            }
        }
        "arg" => {
            let words = syntax::parse_words(rest, escape);
            need(!words.is_empty(), "requires at least one argument")?;
            let mut args = Vec::with_capacity(words.len());
            for w in words {
                let (k, v) = match w.split_once('=') {
                    Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                    None => (w, None),
                };
                need(!k.is_empty(), "names can not be blank")?;
                args.push((k, v));
            }
            Kind::Arg(args)
        }
        "run" => {
            let form = syntax::maybe_json(rest).map_err(err)?;
            if let Form::Text(t) = &form {
                heredoc_check(t, escape).map_err(err)?;
            }
            need(
                !matches!(&form, Form::Json(v) if v.is_empty())
                    && !matches!(&form, Form::Text(t) if t.is_empty()),
                "requires at least one argument",
            )?;
            Kind::Run(form)
        }
        "cmd" | "entrypoint" => {
            let form = syntax::maybe_json(rest).map_err(err)?;
            need(
                !matches!(&form, Form::Text(t) if t.is_empty()),
                "requires at least one argument",
            )?;
            if cmd == "cmd" {
                Kind::Cmd(form)
            } else {
                Kind::Entrypoint(form)
            }
        }
        "shell" => match syntax::maybe_json(rest).map_err(err)? {
            Form::Json(v) if !v.is_empty() => Kind::Shell(v),
            Form::Json(_) => return Err(err("SHELL requires at least one argument".into())),
            Form::Text(_) => {
                return Err(err("SHELL requires the arguments to be in JSON form".into()));
            }
        },
        "copy" | "add" => {
            let (args, json) = syntax::maybe_json_list(rest).map_err(err)?;
            if !json {
                heredoc_check(rest, escape).map_err(err)?;
            }
            need(
                args.len() >= 2,
                "requires at least two arguments: one or more sources and a destination",
            )?;
            Kind::Copy {
                add: cmd == "add",
                from: flag("from"),
                chown: flag("chown"),
                chmod: flag("chmod"),
                args,
                json,
            }
        }
        "env" | "label" => {
            let pairs = syntax::parse_name_val(rest, &upper, escape).map_err(err)?;
            if cmd == "env" {
                Kind::Env(pairs)
            } else {
                Kind::Label(pairs)
            }
        }
        "workdir" | "user" | "stopsignal" | "maintainer" => {
            need(!rest.is_empty(), "requires exactly one argument")?;
            let v = rest.to_owned();
            match cmd.as_str() {
                "workdir" => Kind::Workdir(v),
                "user" => Kind::User(v),
                "stopsignal" => Kind::StopSignal(v),
                _ => Kind::Maintainer(v),
            }
        }
        "expose" => {
            let ports = syntax::fields(rest);
            need(!ports.is_empty(), "requires at least one argument")?;
            Kind::Expose(ports)
        }
        "volume" => {
            let (paths, json) = syntax::maybe_json_list(rest).map_err(err)?;
            need(!paths.is_empty(), "requires at least one argument")?;
            Kind::Volume { paths, json }
        }
        "healthcheck" => {
            let hc = healthcheck(rest, &flags).map_err(err)?;
            let shown = if flags.is_empty() {
                rest.to_owned()
            } else {
                let f: Vec<String> = flags.iter().map(|(n, v)| format!("--{n}={v}")).collect();
                format!("{} {rest}", f.join(" "))
            };
            Kind::Healthcheck(hc, shown)
        }
        _ => unreachable!("checked above"),
    };
    Ok(Instr { line: l.line, kind })
}

/// BuildKit의 스테이지 이름 규칙(`^[a-z][a-z0-9-_.]*$`).
fn valid_stage_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-_.".contains(c))
}

/// heredoc(`RUN <<EOF`, `COPY <<EOF /x`)은 아직 받지 않는다. 아래 줄들이 명령이 아니므로 어디에 있든 오류다.
///
/// BuildKit처럼 셸 낱말로 나눈 뒤(따옴표를 벗긴 뒤) `<<` 또는 `<<-`로 시작하고 나머지에 `<`가 없는 낱말을 찾는다.
fn heredoc_check(rest: &str, escape: char) -> Result<(), String> {
    let Ok(words) = shell::words(rest, escape, &|_| None, usize::MAX) else {
        return Ok(());
    };
    for w in words {
        if let Some(name) = w.strip_prefix("<<") {
            let name = name.strip_prefix('-').unwrap_or(name).trim_start();
            if !name.is_empty() && !name.contains('<') {
                return Err("heredocs (<<EOF) are not supported yet".into());
            }
        }
    }
    Ok(())
}

fn healthcheck(rest: &str, flags: &[(String, String)]) -> Result<Healthcheck, String> {
    let (typ, args) = match rest.find(char::is_whitespace) {
        Some(i) => (&rest[..i], rest[i..].trim_start()),
        None => (rest, ""),
    };
    let typ = typ.to_ascii_uppercase();
    match typ.as_str() {
        "NONE" => {
            if !args.is_empty() || !flags.is_empty() {
                return Err("HEALTHCHECK NONE takes no arguments".into());
            }
            Ok(Healthcheck {
                test: vec!["NONE".into()],
                interval_ns: None,
                timeout_ns: None,
                start_period_ns: None,
                start_interval_ns: None,
                retries: None,
            })
        }
        "CMD" => {
            let test = match syntax::maybe_json(args)? {
                Form::Json(v) if !v.is_empty() => {
                    let mut t = vec!["CMD".to_owned()];
                    t.extend(v);
                    t
                }
                Form::Text(t) if !t.is_empty() => vec!["CMD-SHELL".into(), t],
                _ => return Err("Missing command after HEALTHCHECK CMD".into()),
            };
            let dur = |name: &str| -> Result<Option<u64>, String> {
                let Some((_, v)) = flags.iter().find(|(n, _)| n == name) else {
                    return Ok(None);
                };
                let d = duration::parse(v).map_err(|e| format!("--{name}: {e}"))?;
                if d == 0 {
                    return Ok(None);
                }
                if d < 1_000_000 {
                    return Err(format!("--{name} cannot be less than 1ms"));
                }
                Ok(Some(d as u64))
            };
            let retries = match flags.iter().find(|(n, _)| n == "retries") {
                None => None,
                Some((_, v)) => {
                    let n: i64 = v
                        .parse()
                        .map_err(|_| format!("--retries: invalid number {v:?}"))?;
                    if n < 0 {
                        return Err("--retries cannot be negative".into());
                    }
                    let n = u32::try_from(n).map_err(|_| format!("--retries: {n} is too large"))?;
                    (n != 0).then_some(n)
                }
            };
            Ok(Healthcheck {
                test,
                interval_ns: dur("interval")?,
                timeout_ns: dur("timeout")?,
                start_period_ns: dur("start-period")?,
                start_interval_ns: dur("start-interval")?,
                retries,
            })
        }
        "" => Err("HEALTHCHECK requires an argument".into()),
        _ => Err(format!("Unknown type {typ:?} in HEALTHCHECK (try CMD)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(text: &str) -> Result<Kind, PlanError> {
        let l = Logical {
            line: 7,
            text: text.into(),
            empty_continuation: false,
        };
        parse(&l, '\\').map(|i| i.kind)
    }

    fn e(text: &str) -> String {
        let err = p(text).unwrap_err();
        assert_eq!(err.line, Some(7), "{text}");
        err.message
    }

    #[test]
    fn from_forms() {
        assert_eq!(
            p("FROM --platform=$BUILDPLATFORM golang:1.23 As Build").unwrap(),
            Kind::From {
                image: "golang:1.23".into(),
                name: Some("build".into()),
                platform: Some("$BUILDPLATFORM".into()),
            }
        );
        assert_eq!(e("FROM a b"), "FROM requires either one or three arguments");
        assert_eq!(e("FROM"), "FROM requires either one or three arguments");
        assert_eq!(
            e("FROM a TO b"),
            "FROM requires either one or three arguments"
        );
        assert!(e("FROM a AS 1x").contains("invalid name"));
        assert!(e("FROM a AS b$").contains("invalid name"));
        assert_eq!(e("FROM --mount=x a"), "unknown flag: mount");
    }

    #[test]
    fn run_cmd_entrypoint_shell() {
        assert_eq!(
            p("RUN [\"npm\", \"ci\"]").unwrap(),
            Kind::Run(Form::Json(vec!["npm".into(), "ci".into()]))
        );
        assert_eq!(
            p("run [ -f x ] && echo $A").unwrap(),
            Kind::Run(Form::Text("[ -f x ] && echo $A".into()))
        );
        assert_eq!(e("RUN"), "RUN requires at least one argument");
        assert_eq!(e("RUN []"), "RUN requires at least one argument");
        assert_eq!(p("CMD []").unwrap(), Kind::Cmd(Form::Json(vec![])));
        assert_eq!(e("CMD"), "CMD requires at least one argument");
        assert_eq!(
            e("ENTRYPOINT [1]"),
            "Dockerfile JSON arrays must contain only strings"
        );
        assert_eq!(
            p("SHELL [\"bash\", \"-c\"]").unwrap(),
            Kind::Shell(vec!["bash".into(), "-c".into()])
        );
        assert_eq!(
            e("SHELL bash -c"),
            "SHELL requires the arguments to be in JSON form"
        );
        assert_eq!(e("SHELL []"), "SHELL requires at least one argument");
        // Docker처럼 셸 형식 CMD의 앞 `--`도 플래그로 읽는다.
        assert_eq!(e("CMD --version"), "unknown flag: version");
    }

    #[test]
    fn unsupported_things_fail_with_their_line() {
        assert_eq!(
            e("RUN --mount=type=cache,target=/root/.npm npm ci"),
            "RUN --mount is not supported yet"
        );
        assert!(e("RUN --network=host curl x").starts_with("RUN --network"));
        assert_eq!(e("COPY --link . /app"), "COPY --link is not supported yet");
        assert_eq!(
            e("COPY --parents a /b/"),
            "COPY --parents is not supported yet"
        );
        assert!(e("ADD --checksum=sha256:x https://a/b /c").starts_with("ADD --checksum"));
        assert_eq!(e("COPY --nope . /"), "unknown flag: nope");
        assert_eq!(e("ADD --from=x a /b"), "unknown flag: from");
        assert_eq!(e("ONBUILD RUN x"), "ONBUILD is not supported yet");
        assert_eq!(e("FOO bar"), "unknown instruction: FOO");
        assert_eq!(e("RUN <<EOF"), "heredocs (<<EOF) are not supported yet");
        assert_eq!(
            e("RUN cat <<-\"EOF\" > /x"),
            "heredocs (<<EOF) are not supported yet"
        );
        assert_eq!(e("COPY <<EOF /x"), "heredocs (<<EOF) are not supported yet");
        // heredoc이 아닌 것.
        assert!(p("RUN cat <<< hi").is_ok());
        assert!(p("RUN echo \"a <<b\"").is_ok());
        assert!(p("RUN echo << x").is_ok());
        assert!(p("RUN [\"sh\", \"-c\", \"cat <<EOF\"]").is_ok());
    }

    #[test]
    fn copy_and_add() {
        assert_eq!(
            p("COPY --from=build --chown=node:node --chmod=644 a b /app/").unwrap(),
            Kind::Copy {
                add: false,
                from: Some("build".into()),
                chown: Some("node:node".into()),
                chmod: Some("644".into()),
                args: vec!["a".into(), "b".into(), "/app/".into()],
                json: false,
            }
        );
        assert!(matches!(
            p("ADD [\"a b\", \"/c\"]").unwrap(),
            Kind::Copy {
                add: true,
                json: true,
                ..
            }
        ));
        assert!(e("COPY a").contains("requires at least two arguments"));
        assert_eq!(e("COPY --from a b"), "missing a value on flag: from");
        assert_eq!(
            e("COPY --from=a --from=b c d"),
            "duplicate flag specified: from"
        );
    }

    #[test]
    fn arg_env_label() {
        assert_eq!(
            p("ARG a b=1 c=").unwrap(),
            Kind::Arg(vec![
                ("a".into(), None),
                ("b".into(), Some("1".into())),
                ("c".into(), Some(String::new()))
            ])
        );
        assert_eq!(e("ARG"), "ARG requires at least one argument");
        assert_eq!(e("ARG =1"), "ARG names can not be blank");
        assert_eq!(
            p("ENV PATH /x:$PATH").unwrap(),
            Kind::Env(vec![("PATH".into(), "/x:$PATH".into())])
        );
        assert_eq!(
            p("LABEL a=1 \"b c\"=2").unwrap(),
            Kind::Label(vec![
                ("a".into(), "1".into()),
                ("\"b c\"".into(), "2".into())
            ])
        );
        assert_eq!(e("ENV --x=1 A=1"), "unknown flag: x");
    }

    #[test]
    fn single_value_instructions() {
        assert_eq!(
            p("WORKDIR /my app").unwrap(),
            Kind::Workdir("/my app".into())
        );
        assert_eq!(p("USER 1000:1000").unwrap(), Kind::User("1000:1000".into()));
        assert_eq!(
            p("STOPSIGNAL SIGINT").unwrap(),
            Kind::StopSignal("SIGINT".into())
        );
        assert_eq!(p("MAINTAINER me").unwrap(), Kind::Maintainer("me".into()));
        assert_eq!(e("WORKDIR"), "WORKDIR requires exactly one argument");
        assert_eq!(
            p("EXPOSE 80 443/udp").unwrap(),
            Kind::Expose(vec!["80".into(), "443/udp".into()])
        );
        assert_eq!(e("EXPOSE"), "EXPOSE requires at least one argument");
        assert_eq!(
            p("VOLUME [\"/a\", \"/b\"]").unwrap(),
            Kind::Volume {
                paths: vec!["/a".into(), "/b".into()],
                json: true
            }
        );
        assert_eq!(e("VOLUME []"), "VOLUME requires at least one argument");
    }

    #[test]
    fn healthchecks() {
        let Kind::Healthcheck(h, shown) = p(
            "HEALTHCHECK --interval=30s --timeout=3s --start-period=1m --retries=3 CMD curl -f http://localhost/ || exit 1",
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(h.test, ["CMD-SHELL", "curl -f http://localhost/ || exit 1"]);
        assert_eq!(h.interval_ns, Some(30_000_000_000));
        assert_eq!(h.timeout_ns, Some(3_000_000_000));
        assert_eq!(h.start_period_ns, Some(60_000_000_000));
        assert_eq!(h.start_interval_ns, None);
        assert_eq!(h.retries, Some(3));
        assert!(shown.starts_with("--interval=30s --timeout=3s"));
        let Kind::Healthcheck(h, _) = p("HEALTHCHECK CMD [\"/bin/check\", \"-q\"]").unwrap() else {
            panic!()
        };
        assert_eq!(h.test, ["CMD", "/bin/check", "-q"]);
        let Kind::Healthcheck(h, _) = p("HEALTHCHECK none").unwrap() else {
            panic!()
        };
        assert_eq!(h.test, ["NONE"]);
        assert_eq!(
            e("HEALTHCHECK NONE x"),
            "HEALTHCHECK NONE takes no arguments"
        );
        assert_eq!(
            e("HEALTHCHECK CMD"),
            "Missing command after HEALTHCHECK CMD"
        );
        assert!(e("HEALTHCHECK RUN x").starts_with("Unknown type"));
        assert_eq!(
            e("HEALTHCHECK --interval=1ns CMD x"),
            "--interval cannot be less than 1ms"
        );
        assert_eq!(
            e("HEALTHCHECK --retries=-1 CMD x"),
            "--retries cannot be negative"
        );
        assert!(e("HEALTHCHECK --timeout=3 CMD x").contains("missing unit"));
        assert_eq!(e("HEALTHCHECK --nope=1 CMD x"), "unknown flag: nope");
    }
}
