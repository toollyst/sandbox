//! `volume create|ls|inspect|rm|prune`와 `run -v NAME:/path`가 쓰는 볼륨 다루기(Docker `volume`).
//!
//! 규칙(디스크, 잠금, 지우기)은 실행 코어(`sbx_runtime::volume`)에 있다. 여기서 정하는 것(명시적 동작):
//!
//! - **처음 쓸 때 만들기**: `run`·`create`의 `-v NAME:/path`, `--mount type=volume`에 없는 이름이 오면 기본 크기
//!   ([`behavior::DEFAULT_VOLUME_MIB`])로 만든다(Docker와 같다). 이미지를 받기 전이 아니라 샌드박스를 만들기 바로 전이다.
//! - **이름 없는 볼륨**(`-v /path`)은 만들지 않고 거부한다(`parse::volume`). 그래서 sbx에는 Docker가 `--rm`과 함께
//!   지우는 이름 없는 볼륨이 없고, `volume prune`은 `-a`일 때만 무엇을 지운다(Docker 23 이후와 같은 뜻).
//! - **범위**([`VolumeScope`]): `sbx mcp`는 MCP로 만든 볼륨(라벨 `OWNER_LABEL=mcp`)만 쓰고 만든다. 사용자가 만든 같은
//!   이름의 볼륨이 있으면 쓰지 않고 오류다. CLI는 모든 볼륨을 쓴다.
//! - `volume rm`은 샌드박스 기록이 가리키는 볼륨(멈춘 샌드박스도)을 지우지 않는다(Docker와 같다). `-f`는 없는 이름을
//!   오류로 보지 않을 뿐이다. 샌드박스를 지워도 볼륨은 남는다.
//! - `volume prune -a`와 `system prune -a --volumes`는 어느 샌드박스도 가리키지 않는 볼륨을 지운다. 묻고 지운다.
//!   터미널이 아니면 `-f`가 있어야 한다.

use std::io::{BufRead, IsTerminal};

use sbx_model::VolumeMount;
use sbx_runtime::volume::VolumeInfo;

use super::*;
use crate::cli::VolumeCommand;

/// 이 Ctx가 쓰고 만들 수 있는 볼륨.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VolumeScope {
    /// 모두(CLI).
    All,
    /// 이 라벨이 있는 것만. 처음 쓸 때 만드는 볼륨에 이 라벨을 붙이고, 그런 볼륨은 `max`개까지다(`sbx mcp`).
    Labeled {
        /// 라벨 키.
        key: &'static str,
        /// 라벨 값.
        value: &'static str,
        /// 그 라벨의 볼륨 수 상한.
        max: u32,
        /// 만드는 볼륨의 크기(MiB).
        size_mib: u32,
    },
}

impl VolumeScope {
    fn allows(&self, v: &VolumeInfo) -> bool {
        match self {
            VolumeScope::All => true,
            VolumeScope::Labeled { key, value, .. } => {
                v.record.labels.get(*key).map(String::as_str) == Some(*value)
            }
        }
    }
}

/// `--opt KEY=VALUE`(Docker의 `local` 드라이버 옵션 자리). 지금 키는 `size`뿐이다. 모르는 키는 오류다.
fn size_opt(opts: &[String]) -> AppResult<Option<u32>> {
    let mut size = None;
    for o in opts {
        let (k, v) = o
            .split_once('=')
            .ok_or_else(|| msg(format!("--opt takes KEY=VALUE, not {o:?}")))?;
        if k.trim() != "size" {
            return Err(msg(format!(
                "unknown volume option {k:?} (the only one is size, e.g. size=50g)"
            )));
        }
        size = Some(parse::disk_mib(v.trim()).map_err(msg)?);
    }
    Ok(size)
}

/// Docker처럼 이름 없이 만들 때의 이름: 64자리 16진수.
fn random_name() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::with_capacity(64);
    for i in 0..4u64 {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(now_ns());
        h.write_u32(std::process::id());
        h.write_u64(i);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

impl Ctx {
    /// 샌드박스가 붙일 볼륨이 있게 한다: 없으면 만들고, 있으면 이 Ctx의 범위인지 본다(모듈 문서).
    pub(super) fn ensure_volumes(&self, rt: &Runtime, mounts: &[VolumeMount]) -> AppResult<()> {
        for m in mounts {
            let found = match rt.volume(&m.name)? {
                Some(v) => Some(v),
                None => self.create_on_first_use(rt, &m.name)?,
            };
            if let Some(v) = found
                && !self.volume_scope.allows(&v)
            {
                return Err(msg(format!(
                    "volume {} was not created through MCP, so MCP tools can't use it \
                     (sbx mcp --all-sandboxes lifts that)",
                    m.name
                )));
            }
        }
        Ok(())
    }

    /// 처음 쓰는 이름의 볼륨을 만든다. 그사이 다른 쪽이 만들었으면 그것을 돌려준다.
    fn create_on_first_use(&self, rt: &Runtime, name: &str) -> AppResult<Option<VolumeInfo>> {
        let (labels, size) = match &self.volume_scope {
            VolumeScope::All => (BTreeMap::new(), behavior::DEFAULT_VOLUME_MIB),
            VolumeScope::Labeled {
                key,
                value,
                max,
                size_mib,
            } => {
                let have = rt
                    .volumes()?
                    .iter()
                    .filter(|v| self.volume_scope.allows(v))
                    .count();
                if have >= *max as usize {
                    return Err(msg(format!(
                        "this server already has {have} volumes, its limit (sbx mcp --max-volumes); \
                         reuse one or ask the user to remove some"
                    )));
                }
                (
                    BTreeMap::from([(key.to_string(), value.to_string())]),
                    *size_mib,
                )
            }
        };
        match rt.create_volume(name, size, labels) {
            Ok(v) => Ok(Some(v)),
            Err(RuntimeError::VolumeExists(_)) => Ok(rt.volume(name)?),
            Err(e) => Err(e.into()),
        }
    }

    /// 볼륨 하나의 JSON. `users`는 그 볼륨을 가리키는 샌드박스.
    fn volume_json(&self, v: &VolumeInfo, users: &[SandboxInfo]) -> AppResult<output::Volume> {
        Ok(output::Volume {
            name: v.name.clone(),
            created: behavior::rfc3339(v.created()),
            size: u64::from(v.record.size_mib) << 20,
            disk_usage: v.allocated_bytes,
            disk: v.disk().display().to_string(),
            labels: v.record.labels.clone(),
            sandboxes: users.iter().map(|s| s.id.to_string()).collect(),
            in_use: sbx_runtime::volume::in_use(&self.data_dir, &v.name)?,
        })
    }

    pub(super) async fn volume_cmd(&self, c: &VolumeCommand) -> AppResult<u8> {
        let rt = self.runtime()?;
        match c {
            VolumeCommand::Create { opt, labels, name } => {
                let size = size_opt(opt)?;
                let name = name.clone().unwrap_or_else(random_name);
                let labels: BTreeMap<String, String> =
                    labels.iter().map(|l| parse::label(l)).collect();
                let v = match rt.create_volume(
                    &name,
                    size.unwrap_or(behavior::DEFAULT_VOLUME_MIB),
                    labels.clone(),
                ) {
                    Ok(v) => v,
                    // Docker처럼 같은 이름을 다시 만들면 있는 것을 돌려준다. 다른 크기나 라벨을 달라고 했으면 오류다.
                    Err(RuntimeError::VolumeExists(_)) => {
                        let v = rt
                            .volume(&name)?
                            .ok_or_else(|| msg(format!("no such volume: {name}")))?;
                        let differs = size.is_some_and(|s| s != v.record.size_mib)
                            || (!labels.is_empty() && labels != v.record.labels);
                        if differs {
                            return Err(msg(format!(
                                "volume {name} already exists with other options \
                                 (size {}m); remove it first or choose another name",
                                v.record.size_mib
                            )));
                        }
                        v
                    }
                    Err(e) => return Err(e.into()),
                };
                if self.json() {
                    let users = rt.volume_users().await?;
                    let u = users.get(&v.name).map(Vec::as_slice).unwrap_or_default();
                    output::print_value(&self.volume_json(&v, u)?)?;
                } else {
                    println!("{}", v.name);
                }
                Ok(0)
            }
            VolumeCommand::Ls { quiet, filter } => {
                let f = Filters::parse(filter, filter::VOLUMES).map_err(msg)?;
                let users = rt.volume_users().await?;
                let list: Vec<VolumeInfo> = rt
                    .volumes()?
                    .into_iter()
                    .filter(|v| {
                        f.allows("name", |n| v.name.contains(n))
                            && f.allows("label", |l| filter::label(&v.record.labels, l))
                            && f.allows("dangling", |d| {
                                (d == "true") != users.contains_key(&v.name)
                            })
                    })
                    .collect();
                if *quiet {
                    list.iter().for_each(|v| println!("{}", v.name));
                    return Ok(0);
                }
                let use_of =
                    |v: &VolumeInfo| users.get(&v.name).map(Vec::as_slice).unwrap_or_default();
                if self.json() {
                    let out = list
                        .iter()
                        .map(|v| self.volume_json(v, use_of(v)))
                        .collect::<AppResult<Vec<_>>>()?;
                    output::print_value(&out)?;
                    return Ok(0);
                }
                println!(
                    "{:<24} {:<10} {:<10} {:<20} USED BY",
                    "VOLUME NAME", "SIZE", "ON DISK", "CREATED"
                );
                let now = std::time::SystemTime::now();
                for v in &list {
                    let by: Vec<String> = use_of(v)
                        .iter()
                        .map(|s| {
                            s.spec
                                .name
                                .clone()
                                .unwrap_or_else(|| short_id(&s.id).to_owned())
                        })
                        .collect();
                    println!(
                        "{:<24} {:<10} {:<10} {:<20} {}",
                        v.name,
                        behavior::human_size(u64::from(v.record.size_mib) << 20),
                        behavior::human_size(v.allocated_bytes),
                        behavior::human_ago(now.duration_since(v.created()).unwrap_or_default()),
                        by.join(", ")
                    );
                }
                Ok(0)
            }
            VolumeCommand::Inspect { names } => {
                let users = rt.volume_users().await?;
                let mut code = 0;
                let mut all = Vec::new();
                for n in names {
                    match rt.volume(n) {
                        Ok(Some(v)) => {
                            let u = users.get(n).map(Vec::as_slice).unwrap_or_default();
                            all.push(self.volume_json(&v, u)?);
                        }
                        Ok(None) => {
                            self.target_error(n, &format!("no such volume: {n}"));
                            code = 1;
                        }
                        Err(e) => {
                            self.target_error(n, &e);
                            code = 1;
                        }
                    }
                }
                if self.json() {
                    all.iter().try_for_each(output::print_line)?;
                } else if !all.is_empty() {
                    println!("{}", serde_json::to_string_pretty(&all)?);
                }
                Ok(code)
            }
            VolumeCommand::Rm { force, names } => {
                let mut code = 0;
                for n in names {
                    match rt.remove_volume(n).await {
                        Ok(()) => self.print_volume_ref(n)?,
                        Err(RuntimeError::VolumeNotFound(_)) if *force => {}
                        Err(e) => {
                            self.target_error(n, &e);
                            code = 1;
                        }
                    }
                }
                Ok(code)
            }
            VolumeCommand::Prune { all, force, filter } => {
                let f = Filters::parse(filter, filter::VOLUME_PRUNE).map_err(msg)?;
                if !*all {
                    if self.json() {
                        output::print_value(&output::VolumePrune::default())?;
                    } else {
                        eprintln!(
                            "sbx has no anonymous volumes, so without -a there is nothing to prune \
                             (-a removes every volume no sandbox uses)"
                        );
                        println!("Total reclaimed space: 0B");
                    }
                    return Ok(0);
                }
                if !*force && !confirm("all volumes not used by at least one sandbox")? {
                    return Ok(1);
                }
                let (volumes, reclaimed) = self.prune_volumes(&rt, &f).await?;
                if self.json() {
                    output::print_value(&output::VolumePrune { volumes, reclaimed })?;
                } else {
                    if !volumes.is_empty() {
                        println!("Deleted Volumes:");
                        volumes.iter().for_each(|v| println!("{v}"));
                        println!();
                    }
                    println!("Total reclaimed space: {}", behavior::human_size(reclaimed));
                }
                Ok(0)
            }
        }
    }

    fn print_volume_ref(&self, name: &str) -> AppResult<()> {
        if self.json() {
            output::print_line(&output::VolumeRef {
                name: name.to_owned(),
            })?;
        } else {
            println!("{name}");
        }
        Ok(())
    }

    /// 어느 샌드박스도 가리키지 않는 볼륨을 지운다(`label=` 필터를 맞는 것만). 지운 이름과 비운 바이트.
    pub(super) async fn prune_volumes(
        &self,
        rt: &Runtime,
        f: &Filters,
    ) -> AppResult<(Vec<String>, u64)> {
        let users = rt.volume_users().await?;
        let (mut gone, mut freed) = (Vec::new(), 0);
        for v in rt.volumes()? {
            if users.contains_key(&v.name)
                || !self.volume_scope.allows(&v)
                || !f.allows("label", |l| filter::label(&v.record.labels, l))
            {
                continue;
            }
            match rt.remove_volume(&v.name).await {
                Ok(()) => {
                    freed += v.allocated_bytes;
                    gone.push(v.name);
                }
                Err(e) => self.target_error(&v.name, &e),
            }
        }
        Ok((gone, freed))
    }
}

/// 지우기 전에 묻는다. 터미널이 아니면 묻지 않고 오류다(`-f`가 있어야 한다).
pub(super) fn confirm(what: &str) -> AppResult<bool> {
    if !std::io::stdin().is_terminal() {
        return Err(msg(format!(
            "this removes {what}; use -f to do it without asking"
        )));
    }
    eprint!("WARNING! This will remove {what}.\nAre you sure you want to continue? [y/N] ");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_size_option_is_known() {
        assert_eq!(size_opt(&[]).unwrap(), None);
        assert_eq!(size_opt(&["size=50g".into()]).unwrap(), Some(50 << 10));
        assert!(
            size_opt(&["type=tmpfs".into()])
                .unwrap_err()
                .to_string()
                .contains("unknown")
        );
        assert!(size_opt(&["size".into()]).is_err());
        assert!(size_opt(&["size=1m".into()]).is_err());
    }

    #[test]
    fn random_names_look_like_dockers() {
        let (a, b) = (random_name(), random_name());
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
        assert!(sbx_model::check_volume_name(&a).is_ok());
    }
}
