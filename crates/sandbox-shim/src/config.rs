//! shim에 넘기는 설정과 그 명령줄 형식.
//!
//! ```text
//! <shim> start --data-dir <D> --cache-dir <C> --socket-dir <S> --guest-init <G> --executor <E> [--rm] <샌드박스 ID>
//! ```
//!
//! `--rm` 외의 옵션은 모두 있어야 한다. shim은 기본값을 정하지 않는다. 정하는 것은 shim을 띄우는 쪽이다.
//! `--rm`이면 기본 프로세스가 끝난 뒤 샌드박스를 지운다(`run -d --rm`).

use std::ffi::OsString;
use std::path::PathBuf;

use sandbox_model::SandboxId;
use sandbox_runtime::RuntimeConfig;

/// shim 명령의 첫 인자.
pub const START: &str = "start";

/// shim이 실행 코어를 만드는 데 필요한 것. 띄운 쪽과 같은 값이어야 같은 샌드박스를 본다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimConfig {
    /// 실행 코어 설정.
    pub runtime: RuntimeConfig,
    /// 실행기 실행 파일 (`executor-proto` 규약).
    pub executor: PathBuf,
    /// 기본 프로세스가 끝나면 샌드박스를 지운다.
    pub remove_on_exit: bool,
}

const OPTIONS: [&str; 5] = [
    "--data-dir",
    "--cache-dir",
    "--socket-dir",
    "--guest-init",
    "--executor",
];

impl ShimConfig {
    /// 명령줄 인자 (프로그램 이름 뒤).
    pub fn to_args(&self, id: &SandboxId) -> Vec<OsString> {
        let values = [
            &self.runtime.data_dir,
            &self.runtime.cache_dir,
            &self.runtime.socket_dir,
            &self.runtime.guest_init,
            &self.executor,
        ];
        let mut args = vec![OsString::from(START)];
        for (name, value) in OPTIONS.iter().zip(values) {
            args.push(name.into());
            args.push(value.into());
        }
        if self.remove_on_exit {
            args.push("--rm".into());
        }
        args.push(id.as_str().into());
        args
    }

    /// 명령줄 인자(프로그램 이름 뒤)를 읽는다.
    pub fn parse(args: &[OsString]) -> Result<(Self, SandboxId), String> {
        let mut it = args.iter();
        if it.next().and_then(|a| a.to_str()) != Some(START) {
            return Err(format!("usage: sbx-shim {START} <options> <id>"));
        }
        let mut values: [Option<PathBuf>; 5] = Default::default();
        let mut id = None;
        let mut remove_on_exit = false;
        while let Some(a) = it.next() {
            let name = a.to_str().unwrap_or("");
            if name == "--rm" {
                remove_on_exit = true;
                continue;
            }
            match OPTIONS.iter().position(|o| *o == name) {
                Some(i) => {
                    let v = it.next().ok_or_else(|| format!("{name} needs a value"))?;
                    values[i] = Some(PathBuf::from(v));
                }
                None if id.is_none() && !name.starts_with('-') => {
                    id = Some(SandboxId::new(name).map_err(|e| e.to_string())?);
                }
                None => return Err(format!("unexpected argument {a:?}")),
            }
        }
        let [data_dir, cache_dir, socket_dir, guest_init, executor] = values;
        let need =
            |v: Option<PathBuf>, i: usize| v.ok_or_else(|| format!("{} is required", OPTIONS[i]));
        let config = ShimConfig {
            runtime: RuntimeConfig {
                data_dir: need(data_dir, 0)?,
                cache_dir: need(cache_dir, 1)?,
                socket_dir: need(socket_dir, 2)?,
                guest_init: need(guest_init, 3)?,
            },
            executor: need(executor, 4)?,
            remove_on_exit,
        };
        Ok((config, id.ok_or("sandbox id is required")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_roundtrip() {
        let c = ShimConfig {
            runtime: RuntimeConfig {
                data_dir: "/d ir".into(),
                cache_dir: "/c".into(),
                socket_dir: "/s".into(),
                guest_init: "/g".into(),
            },
            executor: "/e".into(),
            remove_on_exit: true,
        };
        let id = SandboxId::new("abc").unwrap();
        assert_eq!(
            ShimConfig::parse(&c.to_args(&id)).unwrap(),
            (c.clone(), id.clone())
        );
        let mut missing = c.to_args(&id);
        missing.drain(1..3);
        assert!(ShimConfig::parse(&missing).is_err());
        assert!(ShimConfig::parse(&[OsString::from("run")]).is_err());
    }
}
