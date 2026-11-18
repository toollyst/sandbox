//! `build`의 `--output`/`-o`, `--load`, `--push`: 만든 이미지를 어디에 둘지([`parse`]).
//!
//! - 아무것도 주지 않으면 sbx 저장소 하나다(`type=sbx`). 주면 준 것만이다: `-o type=docker`만 주면 저장소에 두지
//!   않는다(빌드 캐시는 그대로 쓰고 남는다). 같은 것을 두 번 주면 한 번이다.
//! - `type=sbx`(`type=image`도): 저장소. `name=REF`는 `-t`처럼 이름을 붙인다. `push=true`면 레지스트리에도.
//! - `type=docker`: `docker load`로 흘린다(sbx 저장소에도 디스크에도 두지 않는다). `dest=FILE`이면 그 파일에 tar로,
//!   `dest=-`이면 stdout으로 쓰고 docker는 부르지 않는다.
//! - `type=oci,dest=…`: `dest`가 있어야 한다. `-`이면 stdout에 tar, `/`로 끝나거나 있는 디렉터리이거나 `tar=false`면
//!   OCI image layout 디렉터리, 아니면 tar 파일.
//! - `type=registry`(`--push`): 이름마다 올린다. 이름(`-t`나 `name=`)이 있어야 한다.
//! - `--load`는 `-o type=docker`, `--push`는 `-o type=registry`와 같다.
//! - `name=REF`가 없으면 `-t`의 이름들을 쓴다. 이름은 정규화한다(`app` → `docker.io/library/app:latest`). digest
//!   참조는 안 된다. `name`은 한 출력에 한 번(쉼표는 키를 나눈다).
//! - stdout에 쓰는 출력은 하나까지. `type=local`, `type=tar`, `-o PATH`(파일 시스템으로 풀기)는 없다.
//!
//! tar(`type=docker`와 `type=oci`)는 같은 것이다: OCI image layout에 Docker의 `manifest.json`을 더한 것(`sbx save`와
//! 같다). 그래서 `docker load`도 OCI 도구도 읽는다.

use std::path::{Path, PathBuf};

use sbx_oci::Reference;

/// 출력이 가는 곳.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// sbx 저장소.
    Store,
    /// `docker load`.
    DockerLoad,
    /// tar. `None`이면 stdout.
    Archive(Option<PathBuf>),
    /// OCI image layout 디렉터리.
    Layout(PathBuf),
    /// 레지스트리.
    Registry,
}

/// 출력 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// 사용자가 고른 종류: `sbx`, `docker`, `oci`, `registry`.
    pub kind: &'static str,
    /// 가는 곳.
    pub target: Target,
    /// `name=`(정규화한 것). 비어 있으면 `-t`의 이름을 쓴다.
    pub names: Vec<String>,
}

impl Output {
    fn new(kind: &'static str, target: Target, names: Vec<String>) -> Self {
        Self {
            kind,
            target,
            names,
        }
    }

    /// 이 출력이 쓰는 이름들: `name=`, 없으면 `tags`.
    pub fn names_or<'a>(&'a self, tags: &'a [String]) -> &'a [String] {
        if self.names.is_empty() {
            tags
        } else {
            &self.names
        }
    }
}

fn reference(what: &str, r: &str) -> Result<String, String> {
    let p = Reference::parse(r).map_err(|e| format!("{what} name={r}: {e}"))?;
    if p.digest.is_some() {
        return Err(format!("{what} name={r}: a name cannot have a digest"));
    }
    Ok(p.to_string())
}

fn flag(what: &str, key: &str, v: &str) -> Result<bool, String> {
    match v {
        "true" | "1" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(format!("{what}: {key}={v}: expected true or false")),
    }
}

/// `-o` 하나를 푼다. `push=true`인 `type=image`는 둘이다.
fn one(given: &str, is_dir: &dyn Fn(&Path) -> bool) -> Result<Vec<Output>, String> {
    let what = format!("--output {given}");
    if !given.contains('=') {
        return Err(format!(
            "{what}: writing the files to a directory (type=local) is not supported; use type=oci,dest=DIR/ for an OCI layout"
        ));
    }
    let mut kind = None;
    let mut dest = None;
    let mut name = None;
    let mut tar = None;
    let mut push = false;
    for part in given.split(',') {
        let (k, v) = part
            .split_once('=')
            .ok_or_else(|| format!("{what}: {part:?} is not KEY=VALUE"))?;
        let slot = match k {
            "type" => &mut kind,
            "dest" => &mut dest,
            "name" => &mut name,
            "tar" => {
                tar = Some(flag(&what, k, v)?);
                continue;
            }
            "push" => {
                push = flag(&what, k, v)?;
                continue;
            }
            _ => return Err(format!("{what}: unknown key {k}")),
        };
        if slot.replace(v.to_owned()).is_some() {
            return Err(format!("{what}: {k} is given twice"));
        }
    }
    let kind =
        kind.ok_or_else(|| format!("{what}: type= is missing (sbx, docker, oci or registry)"))?;
    let names = name
        .map(|n| reference(&what, &n))
        .transpose()?
        .into_iter()
        .collect::<Vec<_>>();
    let refuse = |key: &str, ok: bool| {
        if ok {
            Ok(())
        } else {
            Err(format!("{what}: {key} does not apply to type={kind}"))
        }
    };
    let out = match kind.as_str() {
        "sbx" | "image" => {
            refuse("dest", dest.is_none())?;
            refuse("tar", tar.is_none())?;
            let mut v = vec![Output::new("sbx", Target::Store, names.clone())];
            if push {
                v.push(Output::new("registry", Target::Registry, names));
            }
            v
        }
        "docker" => {
            refuse("tar", tar.is_none())?;
            refuse("push", !push)?;
            let target = match dest.as_deref() {
                None => Target::DockerLoad,
                Some("-") => Target::Archive(None),
                Some(d) if is_dir(Path::new(d)) || d.ends_with('/') => {
                    return Err(format!(
                        "{what}: dest is a directory; type=docker writes a tar file (type=oci,dest=DIR/ writes a layout)"
                    ));
                }
                Some(d) => Target::Archive(Some(PathBuf::from(d))),
            };
            vec![Output::new("docker", target, names)]
        }
        "oci" => {
            refuse("push", !push)?;
            let d = dest.ok_or_else(|| {
                format!("{what}: type=oci needs dest=FILE (a tar), dest=DIR/ (a layout) or dest=-")
            })?;
            let target = match (d.as_str(), tar) {
                ("-", Some(false)) => {
                    return Err(format!("{what}: a layout (tar=false) cannot go to stdout"));
                }
                ("-", _) => Target::Archive(None),
                (_, Some(false)) => Target::Layout(PathBuf::from(&d)),
                (_, Some(true)) => Target::Archive(Some(PathBuf::from(&d))),
                (_, None) if d.ends_with('/') || is_dir(Path::new(&d)) => {
                    Target::Layout(PathBuf::from(&d))
                }
                _ => Target::Archive(Some(PathBuf::from(&d))),
            };
            vec![Output::new("oci", target, names)]
        }
        "registry" => {
            refuse("dest", dest.is_none())?;
            refuse("tar", tar.is_none())?;
            vec![Output::new("registry", Target::Registry, names)]
        }
        "local" | "tar" | "cacheonly" => {
            return Err(format!(
                "{what}: type={kind} is not supported (sbx, docker, oci or registry)"
            ));
        }
        _ => {
            return Err(format!(
                "{what}: unknown type {kind} (sbx, docker, oci or registry)"
            ));
        }
    };
    Ok(out)
}

/// 출력들을 푼다(규칙은 모듈 문서). `tags`는 정규화한 `-t`다. `is_dir`은 경로가 있는 디렉터리인지 본다.
pub fn parse(
    given: &[String],
    load: bool,
    push: bool,
    tags: &[String],
    is_dir: &dyn Fn(&Path) -> bool,
) -> Result<Vec<Output>, String> {
    let mut all = Vec::new();
    for g in given {
        all.extend(one(g, is_dir)?);
    }
    if load {
        all.push(Output::new("docker", Target::DockerLoad, Vec::new()));
    }
    if push {
        all.push(Output::new("registry", Target::Registry, Vec::new()));
    }
    if all.is_empty() {
        all.push(Output::new("sbx", Target::Store, Vec::new()));
    }
    let mut out: Vec<Output> = Vec::new();
    for o in all {
        if !out.contains(&o) {
            out.push(o);
        }
    }
    if out
        .iter()
        .filter(|o| o.target == Target::Archive(None))
        .count()
        > 1
    {
        return Err("only one output can go to stdout (dest=-)".into());
    }
    if out
        .iter()
        .any(|o| o.target == Target::Registry && o.names_or(tags).is_empty())
    {
        return Err(
            "pushing needs a name: -t NAME:TAG (or name=NAME:TAG in --output type=registry)".into(),
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(given: &[&str], load: bool, push: bool, tags: &[&str]) -> Result<Vec<Output>, String> {
        let given: Vec<String> = given.iter().map(|s| s.to_string()).collect();
        let tags: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        parse(&given, load, push, &tags, &|d| d == Path::new("/exists"))
    }

    fn targets(v: Vec<Output>) -> Vec<Target> {
        v.into_iter().map(|o| o.target).collect()
    }

    #[test]
    fn the_default_is_the_store_and_given_outputs_are_exactly_those() {
        assert_eq!(targets(p(&[], false, false, &[]).unwrap()), [Target::Store]);
        assert_eq!(
            targets(p(&["type=docker"], false, false, &[]).unwrap()),
            [Target::DockerLoad]
        );
        assert_eq!(
            targets(p(&["type=docker"], true, false, &[]).unwrap()),
            [Target::DockerLoad],
            "--load and type=docker are one output"
        );
        assert_eq!(
            targets(p(&["type=sbx", "type=oci,dest=x.tar"], false, true, &["a:1"]).unwrap()),
            [
                Target::Store,
                Target::Archive(Some("x.tar".into())),
                Target::Registry
            ]
        );
        assert_eq!(
            targets(p(&["type=image,push=true,name=a:1"], false, false, &[]).unwrap()),
            [Target::Store, Target::Registry]
        );
    }

    #[test]
    fn oci_dest_picks_a_tar_or_a_layout() {
        let t = |o: &str| targets(p(&[o], false, false, &[]).unwrap()).remove(0);
        assert_eq!(
            t("type=oci,dest=out.tar"),
            Target::Archive(Some("out.tar".into()))
        );
        assert_eq!(t("type=oci,dest=-"), Target::Archive(None));
        assert_eq!(t("type=oci,dest=out/"), Target::Layout("out/".into()));
        assert_eq!(t("type=oci,dest=/exists"), Target::Layout("/exists".into()));
        assert_eq!(
            t("type=oci,dest=out,tar=false"),
            Target::Layout("out".into())
        );
        assert_eq!(
            t("type=oci,dest=/exists,tar=true"),
            Target::Archive(Some("/exists".into()))
        );
        assert_eq!(
            t("type=docker,dest=d.tar"),
            Target::Archive(Some("d.tar".into()))
        );
        assert_eq!(t("type=docker,dest=-"), Target::Archive(None));
    }

    #[test]
    fn names_are_normalized_and_fall_back_to_tags() {
        let v = p(
            &["type=registry,name=localhost:5000/app:1"],
            false,
            false,
            &["b:2"],
        )
        .unwrap();
        assert_eq!(v[0].names, ["localhost:5000/app:1"]);
        let tags = ["docker.io/library/b:2".to_owned()];
        assert_eq!(v[0].names_or(&tags), ["localhost:5000/app:1"]);
        let v = p(&["type=docker,name=app"], false, false, &[]).unwrap();
        assert_eq!(v[0].names, ["docker.io/library/app:latest"]);
        let v = p(&[], false, true, &["docker.io/library/b:2"]).unwrap();
        assert_eq!(v[0].names_or(&tags), tags);
    }

    #[test]
    fn mistakes_are_refused_with_a_reason() {
        let e =
            |given: &[&str], push: bool, tags: &[&str]| p(given, false, push, tags).unwrap_err();
        assert!(e(&["./out"], false, &[]).contains("type=local"));
        assert!(e(&["dest=x"], false, &[]).contains("type= is missing"));
        assert!(e(&["type=local,dest=x"], false, &[]).contains("not supported"));
        assert!(e(&["type=nope"], false, &[]).contains("unknown type"));
        assert!(e(&["type=oci"], false, &[]).contains("needs dest"));
        assert!(e(&["type=oci,dest=-,tar=false"], false, &[]).contains("stdout"));
        assert!(e(&["type=docker,dest=/exists"], false, &[]).contains("directory"));
        assert!(e(&["type=oci,dest=x,color=red"], false, &[]).contains("unknown key color"));
        assert!(e(&["type=oci,dest=x,dest=y"], false, &[]).contains("twice"));
        assert!(e(&["type=sbx,dest=x"], false, &[]).contains("does not apply"));
        assert!(e(&["type=oci,dest=x,tar=maybe"], false, &[]).contains("true or false"));
        assert!(e(&["type=registry"], false, &[]).contains("needs a name"));
        assert!(e(&[], true, &[]).contains("needs a name"));
        assert!(e(&["type=oci,dest=-", "type=docker,dest=-"], false, &[]).contains("one output"));
        let d = format!("type=docker,name=app@sha256:{}", "a".repeat(64));
        assert!(e(&[&d], false, &[]).contains("digest"));
    }
}
