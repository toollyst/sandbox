//! 이미지 기본값과 호출자 값을 합쳐 기본 프로세스를 정한다.
//!
//! Docker `run`과 같은 규칙이다. CLI 사용자가 기대하는 동작과 같아야 하기 때문이다.
//!
//! | 결과 | 규칙 |
//! |---|---|
//! | argv | `Entrypoint` + `Cmd`. 호출자가 인자를 주면 `Cmd`만 대체한다. 호출자가 entrypoint를 주면 `Entrypoint`를 대체하고 `Cmd`는 비운다 |
//! | env | 이미지 `Env`에 호출자 값을 덮어쓴다. 같은 키는 호출자 값이 이긴다. 순서는 이미지 순서 뒤에 새 키 |
//! | cwd | 호출자 값 > `WorkingDir` > `/` |
//! | user | 호출자 값 > `User` > root |
//!
//! argv가 비면 [`RuntimeError::NoCommand`]다.

use sandbox_model::{ImageConfig, ProcessDefaults, ProcessOverrides, UserSpec};

use crate::error::RuntimeError;

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
    for (key, value) in &overrides.env {
        match env.iter_mut().find(|(k, _)| k == key) {
            Some(slot) => slot.1 = value.clone(),
            None => env.push((key.clone(), value.clone())),
        }
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
        }
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
}
