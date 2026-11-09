//! 이미지 기본값과 호출자 값을 합쳐 기본 프로세스를 정한다.
//!
//! Docker `run`과 같은 규칙이다. CLI 사용자가 기대하는 동작과 같아야 하기 때문이다.
//!
//! | 결과 | 규칙 |
//! |---|---|
//! | argv | `Entrypoint` + `Cmd`. 호출자가 인자를 주면 `Cmd`만 대체한다. 호출자가 entrypoint를 주면 `Entrypoint`를 대체하고 `Cmd`는 비운다 |
//! | env | 이미지 `Env`에 호출자 값을 덮어쓴다. 같은 키는 호출자 값이 이긴다. 순서는 이미지 순서 뒤에 새 키. 그래도 `PATH`가 없으면 [`DEFAULT_PATH`] |
//! | cwd | 호출자 값 > `WorkingDir` > `/` |
//! | user | 호출자 값 > `User` > root |
//! | 멈출 때 시그널 ([`stop_signal`]) | 호출자 값 > `StopSignal` > SIGTERM |
//!
//! argv가 비면 [`RuntimeError::NoCommand`]다.
//!
//! `exec`([`for_exec`])도 Docker `exec`와 같다: argv는 호출자 값, env는 기본 프로세스 env에 호출자 값을 덮어쓰고,
//! cwd와 user는 호출자 값이 없으면 기본 프로세스의 값이다.
//!
//! 네트워크를 허용한 샌드박스에서는 실행할 때 [`add_proxy_env`]가 프록시 환경 변수를 더한다(이미 있는 키는 두고).

use sandbox_model::{ExecSpec, ImageConfig, ProcessDefaults, ProcessOverrides, Signal, UserSpec};

use crate::error::RuntimeError;

/// 이미지에 `PATH`가 없을 때 넣는 값 (Docker와 같다).
pub const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

fn overlay_env(base: &mut Vec<(String, String)>, over: &[(String, String)]) {
    for (key, value) in over {
        match base.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value.clone(),
            None => base.push((key.clone(), value.clone())),
        }
    }
}

/// 나가는 연결 프록시 환경 변수. `HTTP_PROXY`, `HTTPS_PROXY`(와 소문자)는 `http://127.0.0.1:<port>`,
/// `NO_PROXY`(와 소문자)는 `localhost,127.0.0.1,::1`. 이미 있는 키는 바꾸지 않는다(호출자 값이 이긴다).
pub const PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "NO_PROXY",
    "no_proxy",
];

/// 프록시 환경 변수를 더한다. 규칙은 [`PROXY_KEYS`].
pub fn add_proxy_env(env: &mut Vec<(String, String)>, port: u16) {
    let proxy = format!("http://127.0.0.1:{port}");
    for key in PROXY_KEYS {
        if env.iter().any(|(k, _)| k == key) {
            continue;
        }
        let value = if key.eq_ignore_ascii_case("NO_PROXY") {
            "localhost,127.0.0.1,::1".to_owned()
        } else {
            proxy.clone()
        };
        env.push((key.to_owned(), value));
    }
}

/// `exec`로 실행할 프로세스를 정한다.
pub fn for_exec(base: &ProcessDefaults, exec: &ExecSpec) -> Result<ProcessDefaults, RuntimeError> {
    if exec.args.is_empty() {
        return Err(RuntimeError::NoCommand);
    }
    let mut env = base.env.clone();
    overlay_env(&mut env, &exec.env);
    Ok(ProcessDefaults {
        args: exec.args.clone(),
        env,
        cwd: exec.cwd.clone().unwrap_or_else(|| base.cwd.clone()),
        user: exec.user.clone().unwrap_or_else(|| base.user.clone()),
    })
}

/// 멈출 때 기본 프로세스에 보낼 시그널. 규칙은 모듈 문서의 표를 따른다. 이미지 값이 잘못되면 오류다(Docker와 같다).
pub fn stop_signal(image: &ImageConfig, given: Option<Signal>) -> Result<Signal, RuntimeError> {
    match (given, image.stop_signal.as_deref()) {
        (Some(s), _) => Ok(s),
        (None, Some(s)) if !s.is_empty() => s.parse().map_err(RuntimeError::InvalidStopSignal),
        _ => Ok(Signal::TERM),
    }
}

/// 기본 프로세스를 정한다. 규칙은 모듈 문서의 표를 따른다.
pub fn resolve(
    image: &ImageConfig,
    overrides: &ProcessOverrides,
) -> Result<ProcessDefaults, RuntimeError> {
    let entrypoint = overrides
        .entrypoint
        .clone()
        .unwrap_or_else(|| image.entrypoint.clone());
    let cmd = if !overrides.args.is_empty() {
        overrides.args.clone()
    } else if overrides.entrypoint.is_some() {
        Vec::new()
    } else {
        image.cmd.clone()
    };
    let args: Vec<String> = entrypoint.into_iter().chain(cmd).collect();
    if args.is_empty() {
        return Err(RuntimeError::NoCommand);
    }

    let mut env = image.env.clone();
    overlay_env(&mut env, &overrides.env);
    if !env.iter().any(|(k, _)| k == "PATH") {
        env.push(("PATH".into(), DEFAULT_PATH.into()));
    }

    let cwd = overrides
        .cwd
        .clone()
        .or_else(|| image.working_dir.clone().filter(|d| !d.is_empty()))
        .unwrap_or_else(|| "/".to_owned());

    let user = match (&overrides.user, &image.user) {
        (Some(u), _) => u.clone(),
        (None, Some(u)) if !u.is_empty() => {
            u.parse::<UserSpec>().map_err(RuntimeError::InvalidUser)?
        }
        _ => UserSpec::root(),
    };

    Ok(ProcessDefaults {
        args,
        env,
        cwd,
        user,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandbox_model::Ident;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn image() -> ImageConfig {
        ImageConfig {
            entrypoint: s(&["/entry"]),
            cmd: s(&["--serve"]),
            env: vec![("PATH".into(), "/bin".into()), ("A".into(), "1".into())],
            working_dir: Some("/app".into()),
            user: Some("app".into()),
            stop_signal: None,
        }
    }

    #[test]
    fn stop_signal_given_then_image_then_term() {
        let mut i = image();
        assert_eq!(stop_signal(&i, None).unwrap(), Signal::TERM);
        i.stop_signal = Some("SIGINT".into());
        assert_eq!(stop_signal(&i, None).unwrap().number(), 2);
        assert_eq!(stop_signal(&i, Some(Signal::KILL)).unwrap(), Signal::KILL);
        i.stop_signal = Some(String::new());
        assert_eq!(stop_signal(&i, None).unwrap(), Signal::TERM);
        i.stop_signal = Some("SIGNOPE".into());
        assert!(matches!(
            stop_signal(&i, None),
            Err(RuntimeError::InvalidStopSignal(_))
        ));
    }

    #[test]
    fn image_defaults_only() {
        let p = resolve(&image(), &ProcessOverrides::default()).unwrap();
        assert_eq!(p.args, s(&["/entry", "--serve"]));
        assert_eq!(p.cwd, "/app");
        assert_eq!(p.user.user, Ident::Name("app".into()));
    }

    #[test]
    fn args_replace_cmd_only() {
        let o = ProcessOverrides {
            args: s(&["--once"]),
            ..Default::default()
        };
        assert_eq!(
            resolve(&image(), &o).unwrap().args,
            s(&["/entry", "--once"])
        );
    }

    #[test]
    fn entrypoint_replaces_entrypoint_and_clears_cmd() {
        let o = ProcessOverrides {
            entrypoint: Some(s(&["sh"])),
            ..Default::default()
        };
        assert_eq!(resolve(&image(), &o).unwrap().args, s(&["sh"]));

        let o = ProcessOverrides {
            entrypoint: Some(s(&["sh"])),
            args: s(&["-c", "ls"]),
            ..Default::default()
        };
        assert_eq!(resolve(&image(), &o).unwrap().args, s(&["sh", "-c", "ls"]));
    }

    #[test]
    fn empty_entrypoint_override_with_no_args_is_no_command() {
        let o = ProcessOverrides {
            entrypoint: Some(vec![]),
            ..Default::default()
        };
        assert!(matches!(
            resolve(&image(), &o),
            Err(RuntimeError::NoCommand)
        ));
    }

    #[test]
    fn env_overrides_by_key_and_appends_new() {
        let o = ProcessOverrides {
            env: vec![("A".into(), "2".into()), ("B".into(), "3".into())],
            ..Default::default()
        };
        let env = resolve(&image(), &o).unwrap().env;
        assert_eq!(
            env,
            vec![
                ("PATH".into(), "/bin".into()),
                ("A".into(), "2".into()),
                ("B".into(), "3".into())
            ]
        );
    }

    #[test]
    fn cwd_and_user_fallbacks() {
        let bare = ImageConfig {
            cmd: s(&["sh"]),
            ..Default::default()
        };
        let p = resolve(&bare, &ProcessOverrides::default()).unwrap();
        assert_eq!(p.cwd, "/");
        assert_eq!(p.user, UserSpec::root());

        let o = ProcessOverrides {
            cwd: Some("/tmp".into()),
            user: Some("1000:1000".parse().unwrap()),
            ..Default::default()
        };
        let p = resolve(&image(), &o).unwrap();
        assert_eq!(p.cwd, "/tmp");
        assert_eq!(p.user.user, Ident::Id(1000));
    }

    #[test]
    fn invalid_image_user_is_an_error() {
        let bad = ImageConfig {
            cmd: s(&["sh"]),
            user: Some(":x".into()),
            ..Default::default()
        };
        assert!(matches!(
            resolve(&bad, &ProcessOverrides::default()),
            Err(RuntimeError::InvalidUser(_))
        ));
    }

    #[test]
    fn missing_path_gets_docker_default() {
        let bare = ImageConfig {
            cmd: s(&["sh"]),
            ..Default::default()
        };
        let env = resolve(&bare, &ProcessOverrides::default()).unwrap().env;
        assert_eq!(env, vec![("PATH".into(), DEFAULT_PATH.into())]);
    }

    #[test]
    fn exec_inherits_then_overrides() {
        let base = resolve(&image(), &ProcessOverrides::default()).unwrap();
        let x = ExecSpec {
            args: s(&["id"]),
            env: vec![("PATH".into(), "/x".into()), ("N".into(), "1".into())],
            cwd: None,
            user: None,
            tty: false,
        };
        let p = for_exec(&base, &x).unwrap();
        assert_eq!(p.args, s(&["id"]));
        assert_eq!(p.cwd, base.cwd);
        assert_eq!(p.user, base.user);
        assert_eq!(p.env[0], ("PATH".into(), "/x".into()));
        assert_eq!(p.env.last().unwrap(), &("N".into(), "1".into()));
        let empty = ExecSpec { args: vec![], ..x };
        assert!(matches!(
            for_exec(&base, &empty),
            Err(RuntimeError::NoCommand)
        ));
    }
}
