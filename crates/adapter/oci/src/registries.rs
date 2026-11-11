//! 레지스트리 미러·이름 바꾸기·차단: podman, skopeo, buildah가 함께 쓰는 containers
//! [`registries.conf`](https://github.com/containers/image/blob/main/docs/containers-registries.conf.5.md)(v2와 v1)를
//! 그대로 읽는다. sbx만의 설정 파일은 없다.
//!
//! | 무엇 | 규칙(containers/image와 같다) |
//! |---|---|
//! | 파일 | `$CONTAINERS_REGISTRIES_CONF`, 없으면 `~/.config/containers/registries.conf`(있으면), 없으면 `/etc/containers/registries.conf`. 그 뒤 drop-in(`registries.conf.d/*.conf`, 이름 순): 사용자 파일을 썼으면 사용자 것만, 아니면 시스템 것 다음 사용자 것 |
//! | 합치기 | 한 파일 안에서 같은 prefix는 처음 것, 파일 사이에서는 뒤 파일의 항목이 통째로 바꾼다 |
//! | 고르기 | `prefix`(없으면 `location`)가 참조의 앞부분이고 그 뒤가 끝이나 `/`·`:`·`@`인 것 중 가장 긴 것. `*.example.com`은 그 아래 모든 호스트 |
//! | 순서 | `[[registry.mirror]]`를 적힌 순서로, 마지막에 `location`. 앞의 것이 실패하면(없음, 연결 안 됨, 거부) 다음 것 |
//! | 미러 거르기 | `mirror-by-digest-only`면 digest 참조에만 미러를, 미러의 `pull-from-mirror`는 `all`·`digest-only`·`tag-only` |
//! | 바꾸기 | `location` + 참조에서 prefix 뒤 부분. 와일드카드 prefix에 `location`이 없으면 참조 그대로 |
//! | `blocked` | 받지 않는다(오류) |
//! | `insecure` | 그 호스트에 검증하지 않는 TLS, 안 되면 http([`crate::registry::Registry::with_insecure`]) |
//!
//! 읽지 않는 것: `unqualified-search-registries`, `[aliases]`, `short-name-mode`(sbx는 Docker처럼 짧은 이름을
//! `docker.io`로 정규화한다), `credential-helpers`(자격 증명은 [`crate::auth`]). 받지 않는 값(모르는
//! `pull-from-mirror`, 와일드카드가 아닌 prefix에 빈 `location` 등)은 조용히 넘기지 않고 오류다.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use sbx_image::ImageError;

use crate::reference::Reference;

/// 읽을 파일들. 위치 규칙은 모듈 문서에 있고, 정하는 것은 호출자다(`sbx`의 `dirs`).
#[derive(Debug, Clone, Default)]
pub struct ConfFiles {
    /// 주 파일. 없어도 된다.
    pub main: Option<PathBuf>,
    /// drop-in 디렉터리, 읽을 순서. 없어도 된다.
    pub dropin_dirs: Vec<PathBuf>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
struct File {
    #[serde(default)]
    registry: Vec<RawRegistry>,
    /// v1.
    registries: Option<V1>,
}

#[derive(Deserialize, Default)]
struct V1 {
    #[serde(default)]
    search: V1List,
    #[serde(default)]
    insecure: V1List,
    #[serde(default)]
    block: V1List,
}

#[derive(Deserialize, Default)]
struct V1List {
    #[serde(default)]
    registries: Vec<String>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "kebab-case")]
struct RawRegistry {
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    location: String,
    #[serde(default)]
    insecure: bool,
    #[serde(default)]
    blocked: bool,
    #[serde(default)]
    mirror_by_digest_only: bool,
    #[serde(default)]
    mirror: Vec<RawMirror>,
    #[serde(default)]
    pull_from_mirror: String,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "kebab-case")]
struct RawMirror {
    location: String,
    #[serde(default)]
    insecure: bool,
    #[serde(default)]
    pull_from_mirror: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PullFrom {
    All,
    DigestOnly,
    TagOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Mirror {
    location: String,
    insecure: bool,
    pull_from: PullFrom,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    prefix: String,
    location: String,
    insecure: bool,
    blocked: bool,
    mirror_by_digest_only: bool,
    mirrors: Vec<Mirror>,
}

/// 받을 곳 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// 바꾼 참조(이 곳의 호스트와 저장소, 원래의 태그·digest).
    pub reference: Reference,
    /// 미러인가(아니면 주 위치).
    pub mirror: bool,
}

/// 읽은 설정. 비어 있으면 모든 참조가 그대로다.
#[derive(Debug, Clone, Default)]
pub struct Registries {
    entries: Vec<Entry>,
}

fn invalid(path: &Path, why: impl std::fmt::Display) -> ImageError {
    ImageError::Invalid(format!("{}: {why}", path.display()))
}

fn pull_from(path: &Path, s: &str) -> Result<PullFrom, ImageError> {
    match s {
        "" | "all" => Ok(PullFrom::All),
        "digest-only" => Ok(PullFrom::DigestOnly),
        "tag-only" => Ok(PullFrom::TagOnly),
        other => Err(invalid(path, format!("unknown pull-from-mirror {other:?}"))),
    }
}

/// 한 파일의 항목들(한 파일 안에서 같은 prefix는 처음 것).
fn parse_file(path: &Path, text: &str, dropin: bool) -> Result<Vec<Entry>, ImageError> {
    let f: File = toml::from_str(text).map_err(|e| invalid(path, e.message().to_owned()))?;
    let mut raw = f.registry;
    if let Some(v1) = f.registries {
        if dropin || !raw.is_empty() {
            return Err(invalid(path, "mixes the v1 [registries.*] tables with v2"));
        }
        let _ = v1.search;
        let hosts: BTreeSet<&String> = v1
            .insecure
            .registries
            .iter()
            .chain(&v1.block.registries)
            .collect();
        raw = hosts
            .into_iter()
            .map(|h| RawRegistry {
                location: h.clone(),
                insecure: v1.insecure.registries.contains(h),
                blocked: v1.block.registries.contains(h),
                ..RawRegistry::default()
            })
            .collect();
    }
    let mut out: Vec<Entry> = Vec::new();
    for r in raw {
        if !r.pull_from_mirror.is_empty() {
            return Err(invalid(
                path,
                "pull-from-mirror belongs in [[registry.mirror]], not [[registry]]",
            ));
        }
        let prefix = if r.prefix.is_empty() {
            r.location.clone()
        } else {
            r.prefix.clone()
        };
        if prefix.is_empty() {
            return Err(invalid(path, "a [[registry]] needs a prefix or a location"));
        }
        let wildcard = prefix.starts_with("*.");
        if prefix.contains('*') && (!wildcard || prefix[2..].contains(['*', '/'])) {
            return Err(invalid(
                path,
                format!("prefix {prefix:?}: only a leading *. on a host name is supported"),
            ));
        }
        if r.location.is_empty() && !wildcard {
            return Err(invalid(path, format!("prefix {prefix:?} has no location")));
        }
        let mut mirrors = Vec::new();
        for m in &r.mirror {
            if r.mirror_by_digest_only && !m.pull_from_mirror.is_empty() {
                return Err(invalid(
                    path,
                    format!(
                        "{prefix}: mirror-by-digest-only and pull-from-mirror can't be combined"
                    ),
                ));
            }
            if m.location.is_empty() {
                return Err(invalid(path, format!("{prefix}: a mirror has no location")));
            }
            mirrors.push(Mirror {
                location: m.location.clone(),
                insecure: m.insecure,
                pull_from: pull_from(path, &m.pull_from_mirror)?,
            });
        }
        if out.iter().any(|e| e.prefix == prefix) {
            continue;
        }
        out.push(Entry {
            prefix,
            location: r.location,
            insecure: r.insecure,
            blocked: r.blocked,
            mirror_by_digest_only: r.mirror_by_digest_only,
            mirrors,
        });
    }
    Ok(out)
}

/// 와일드카드가 아닌 prefix가 맞는 길이.
fn matching_prefix(reference: &str, prefix: &str) -> Option<usize> {
    let rest = reference.strip_prefix(prefix)?;
    (rest.is_empty() || rest.starts_with(['/', ':', '@'])).then_some(prefix.len())
}

/// `*.example.com`이 맞는 길이(호스트 끝까지).
fn matching_subdomain(reference: &str, prefix: &str) -> Option<usize> {
    let suffix = &prefix[1..]; // ".example.com"
    let at = reference.find(suffix)?;
    if at == 0 || reference[..at].contains('/') {
        return None;
    }
    let end = at + suffix.len();
    let rest = &reference[end..];
    (rest.is_empty() || rest.starts_with(['/', ':', '@'])).then_some(end)
}

impl Registries {
    /// 파일들을 읽는다. 없는 파일·디렉터리는 건너뛴다.
    pub fn load(files: &ConfFiles) -> Result<Self, ImageError> {
        let mut entries: Vec<Entry> = Vec::new();
        let mut merge = |path: &Path, dropin: bool| -> Result<(), ImageError> {
            let text = match std::fs::read_to_string(path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(invalid(path, e)),
            };
            for e in parse_file(path, &text, dropin)? {
                match entries.iter_mut().find(|x| x.prefix == e.prefix) {
                    Some(x) => *x = e,
                    None => entries.push(e),
                }
            }
            Ok(())
        };
        if let Some(main) = &files.main {
            merge(main, false)?;
        }
        for dir in &files.dropin_dirs {
            let mut confs: Vec<PathBuf> = match std::fs::read_dir(dir) {
                Ok(d) => d
                    .filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "conf"))
                    .collect(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(invalid(dir, e)),
            };
            confs.sort();
            for c in confs {
                merge(&c, true)?;
            }
        }
        Ok(Self { entries })
    }

    /// `insecure`로 표시된 호스트(주 위치와 미러).
    pub fn insecure_hosts(&self) -> BTreeSet<String> {
        let host = |loc: &str| loc.split('/').next().unwrap_or_default().to_owned();
        let mut out = BTreeSet::new();
        for e in &self.entries {
            if e.insecure && !e.location.is_empty() {
                out.insert(host(&e.location));
            }
            out.extend(
                e.mirrors
                    .iter()
                    .filter(|m| m.insecure)
                    .map(|m| host(&m.location)),
            );
        }
        out
    }

    fn entry_for(&self, reference: &str) -> Option<(&Entry, usize)> {
        self.entries
            .iter()
            .filter_map(|e| {
                let n = if e.prefix.starts_with("*.") {
                    matching_subdomain(reference, &e.prefix)
                } else {
                    matching_prefix(reference, &e.prefix)
                }?;
                Some((e, n, e.prefix.len()))
            })
            .max_by_key(|(_, _, len)| *len)
            .map(|(e, n, _)| (e, n))
    }

    /// 이 참조를 받을 곳들, 시도할 순서. 설정에 없으면 참조 그대로 하나.
    pub fn endpoints(&self, r: &Reference) -> Result<Vec<Endpoint>, ImageError> {
        let full = r.to_string();
        let Some((e, n)) = self.entry_for(&full) else {
            return Ok(vec![Endpoint {
                reference: r.clone(),
                mirror: false,
            }]);
        };
        if e.blocked {
            return Err(ImageError::Unsupported(format!(
                "{full}: blocked by registries.conf (prefix {})",
                e.prefix
            )));
        }
        let rewrite = |location: &str| -> Result<Reference, ImageError> {
            if location.is_empty() {
                return Ok(r.clone());
            }
            let s = format!("{location}{}", &full[n..]);
            Reference::parse(&s).map_err(|err| {
                ImageError::Invalid(format!("registries.conf rewrites {full} to {s}: {err}"))
            })
        };
        let by_digest = r.digest.is_some();
        let mut out = Vec::new();
        for m in &e.mirrors {
            let use_it = if e.mirror_by_digest_only {
                by_digest
            } else {
                match m.pull_from {
                    PullFrom::All => true,
                    PullFrom::DigestOnly => by_digest,
                    PullFrom::TagOnly => !by_digest,
                }
            };
            if use_it {
                out.push(Endpoint {
                    reference: rewrite(&m.location)?,
                    mirror: true,
                });
            }
        }
        out.push(Endpoint {
            reference: rewrite(&e.location)?,
            mirror: false,
        });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conf(text: &str) -> Registries {
        Registries {
            entries: parse_file(Path::new("t.conf"), text, false).unwrap(),
        }
    }

    fn hosts(reg: &Registries, r: &str) -> Vec<String> {
        reg.endpoints(&Reference::parse(r).unwrap())
            .unwrap()
            .into_iter()
            .map(|e| e.reference.to_string())
            .collect()
    }

    const D: &str = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn docker_hub_mirrors_come_first_then_the_registry() {
        let r = conf(
            r#"
unqualified-search-registries = ["docker.io"]
[[registry]]
location = "docker.io"
[[registry.mirror]]
location = "mirror.gcr.io"
[[registry.mirror]]
location = "harbor.lan/proxy"
pull-from-mirror = "digest-only"
"#,
        );
        assert_eq!(
            hosts(&r, "alpine:3.20"),
            [
                "mirror.gcr.io/library/alpine:3.20",
                "docker.io/library/alpine:3.20"
            ]
        );
        assert_eq!(
            hosts(&r, &format!("alpine@{D}")),
            [
                format!("mirror.gcr.io/library/alpine@{D}"),
                format!("harbor.lan/proxy/library/alpine@{D}"),
                format!("docker.io/library/alpine@{D}"),
            ]
        );
        // 다른 레지스트리는 그대로.
        assert_eq!(hosts(&r, "ghcr.io/a/b:1"), ["ghcr.io/a/b:1"]);
    }

    #[test]
    fn longest_prefix_wins_and_rewrites_the_rest() {
        let r = conf(
            r#"
[[registry]]
prefix = "example.com"
location = "a.lan"
[[registry]]
prefix = "example.com/foo"
location = "internal.lan/bar"
mirror-by-digest-only = true
[[registry.mirror]]
location = "m.lan/bar"
"#,
        );
        assert_eq!(
            hosts(&r, "example.com/foo/img:1"),
            ["internal.lan/bar/img:1"]
        );
        assert_eq!(hosts(&r, "example.com/foobar:1"), ["a.lan/foobar:1"]);
        assert_eq!(
            hosts(&r, &format!("example.com/foo/img@{D}")),
            [
                format!("m.lan/bar/img@{D}"),
                format!("internal.lan/bar/img@{D}")
            ]
        );
        // `:`도 구분자라 example.com:5000에도 맞는다(containers/image와 같다).
        assert_eq!(hosts(&r, "example.com:5000/x:1"), ["a.lan:5000/x:1"]);
    }

    #[test]
    fn wildcards_blocked_and_insecure() {
        let r = conf(
            r#"
[[registry]]
prefix = "*.corp.example"
insecure = true
[[registry]]
location = "evil.example"
blocked = true
"#,
        );
        assert_eq!(
            hosts(&r, "reg.eu.corp.example/a:1"),
            ["reg.eu.corp.example/a:1"]
        );
        assert_eq!(hosts(&r, "corp.example/a:1"), ["corp.example/a:1"]);
        let e = r
            .endpoints(&Reference::parse("evil.example/x:1").unwrap())
            .unwrap_err();
        assert!(e.to_string().contains("blocked"), "{e}");
        // 와일드카드 항목의 location이 비어 있어 호스트는 모른다: 미러의 insecure만 모인다.
        assert!(r.insecure_hosts().is_empty());
        let r = conf(
            "[[registry]]\nlocation = \"reg.lan:5000\"\ninsecure = true\n[[registry.mirror]]\nlocation = \"m.lan/x\"\ninsecure = true\n",
        );
        assert_eq!(
            r.insecure_hosts().into_iter().collect::<Vec<_>>(),
            ["m.lan", "reg.lan:5000"]
        );
    }

    #[test]
    fn v1_lists_become_entries() {
        let r = conf(
            "[registries.search]\nregistries = ['docker.io']\n[registries.insecure]\nregistries = ['reg.lan']\n[registries.block]\nregistries = ['bad.example']\n",
        );
        assert_eq!(hosts(&r, "reg.lan/a:1"), ["reg.lan/a:1"]);
        assert!(r.insecure_hosts().contains("reg.lan"));
        assert!(
            r.endpoints(&Reference::parse("bad.example/a:1").unwrap())
                .is_err()
        );
    }

    #[test]
    fn bad_values_are_errors_not_ignored() {
        for bad in [
            "[[registry]]\nprefix = \"example.com\"\n",
            "[[registry]]\nprefix = \"example.*.com\"\nlocation = \"x\"\n",
            "[[registry]]\nlocation = \"x\"\npull-from-mirror = \"all\"\n",
            "[[registry]]\nlocation = \"x\"\n[[registry.mirror]]\nlocation = \"m\"\npull-from-mirror = \"sometimes\"\n",
            "[[registry]]\nlocation = \"x\"\nmirror-by-digest-only = true\n[[registry.mirror]]\nlocation = \"m\"\npull-from-mirror = \"all\"\n",
            "[[registry]\n",
        ] {
            assert!(parse_file(Path::new("t"), bad, false).is_err(), "{bad}");
        }
    }

    #[test]
    fn later_files_replace_whole_entries() {
        let dir = std::env::temp_dir().join(format!("sbx-regconf-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("d")).unwrap();
        std::fs::write(
            dir.join("main.conf"),
            "[[registry]]\nlocation = \"docker.io\"\n[[registry.mirror]]\nlocation = \"old.lan\"\n[[registry]]\nlocation = \"keep.lan\"\nblocked = true\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("d/20.conf"),
            "[[registry]]\nlocation = \"docker.io\"\n[[registry.mirror]]\nlocation = \"new.lan\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("d/10.conf"),
            "[[registry]]\nlocation = \"docker.io\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("d/ignored.txt"), "not toml [").unwrap();
        let r = Registries::load(&ConfFiles {
            main: Some(dir.join("main.conf")),
            dropin_dirs: vec![dir.join("d"), dir.join("missing")],
        })
        .unwrap();
        assert_eq!(
            hosts(&r, "alpine:1"),
            ["new.lan/library/alpine:1", "docker.io/library/alpine:1"]
        );
        assert!(
            r.endpoints(&Reference::parse("keep.lan/a:1").unwrap())
                .is_err()
        );
        // 없는 파일은 빈 설정.
        assert!(
            Registries::load(&ConfFiles {
                main: Some(dir.join("nope.conf")),
                dropin_dirs: vec![],
            })
            .unwrap()
            .entries
            .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
