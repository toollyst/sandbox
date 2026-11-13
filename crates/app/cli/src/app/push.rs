//! `sbx tag`, `sbx push`.
//!
//! - `tag SOURCE TARGET`: SOURCE는 다른 명령처럼 찾는다(참조나 ID). TARGET은 정규화해 붙인다(`app` →
//!   `docker.io/library/app:latest`). digest 참조는 거부한다(Docker와 같다). 다른 이미지를 가리키던 TARGET은 옮긴다.
//!   출력은 없다(Docker와 같다). JSON이면 [`output::Tagged`].
//! - `push NAME[:TAG]`: 로컬에 그 이름의 태그가 있어야 한다(ID로는 올리지 않는다, Docker와 같다). 그 이름이 가리키는
//!   레지스트리로만 올린다([`sbx_oci::push`]). `-a`면 NAME(태그 없이)의 로컬 태그를 모두, 이름 순으로 올린다
//!   ([`push_targets`]). 하나가 실패해도 나머지를 올리고(JSON이면 그 태그의 오류 줄), 끝에 실패로 끝난다.
//! - 사람에게는 Docker처럼 `The push refers to repository [...]`, 레이어마다 `<12자리>: Pushed`(`Layer already
//!   exists`, `Mounted from <저장소>`), 끝에 `<태그>: digest: <ID> size: <manifest 바이트>`를 쓴다. config는 줄을 내지
//!   않는다(Docker와 같다). `-q`면 올린 참조만 한 줄씩(`--format json`보다 앞선다). 진행 막대는 stderr가
//!   터미널일 때만.
//! - 올릴 곳은 pull과 같은 자격 증명 파일과 `registries.conf`를 쓴다. `policy.json`은 보지 않는다(받을 이미지의
//!   정책이다).

use std::collections::BTreeMap;

use sbx_manage::{BlobPush, Pushed};
use sbx_model::ImageId;
use sbx_oci::{OciImageSource, Reference};

use super::{AppResult, Ctx, msg};
use crate::behavior::familiar;
use crate::output;
use crate::progress::Status;

/// NAME에 태그나 digest를 적었는가(`a/b:1`, `a@sha256:…`; 레지스트리 포트 `localhost:5000/a`는 아니다).
fn names_a_tag(name: &str) -> bool {
    name.contains('@')
        || name
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains(':'))
}

/// 올릴 (참조, 이미지)들. 규칙은 모듈 문서에 있다. `refs`는 로컬 참조 기록이다.
pub fn push_targets(
    name: &str,
    all_tags: bool,
    refs: &BTreeMap<String, ImageId>,
) -> Result<Vec<(String, ImageId)>, String> {
    let wanted = Reference::parse(name).map_err(|e| e.to_string())?;
    if wanted.digest.is_some() {
        return Err(format!(
            "{name}: push needs a tag, not a digest (`sbx tag` names the image first)"
        ));
    }
    if !all_tags {
        let reference = wanted.to_string();
        return match refs.get(&reference) {
            Some(id) => Ok(vec![(reference, id.clone())]),
            None => Err(format!(
                "no local image tagged {reference} (`sbx tag IMAGE {name}` names one)"
            )),
        };
    }
    if names_a_tag(name) {
        return Err("a tag can't be used with --all-tags/-a".into());
    }
    let repo = format!("{}/{}", wanted.registry, wanted.repository);
    let found: Vec<(String, ImageId)> = refs
        .iter()
        .filter(|(r, _)| {
            Reference::parse(r).is_ok_and(|p| {
                p.digest.is_none() && format!("{}/{}", p.registry, p.repository) == repo
            })
        })
        .map(|(r, id)| (r.clone(), id.clone()))
        .collect();
    if found.is_empty() {
        return Err(format!("no local tags of {repo}"));
    }
    Ok(found)
}

/// 사람이 읽는 줄들(모듈 문서). 마지막 blob은 config라 줄을 내지 않는다.
fn report(p: &Pushed) -> Vec<String> {
    let layers = &p.blobs[..p.blobs.len().saturating_sub(1)];
    let mut lines: Vec<String> = layers
        .iter()
        .map(|b| {
            let what = match &b.how {
                BlobPush::Exists => "Layer already exists".to_owned(),
                BlobPush::Mounted(from) => format!(
                    "Mounted from {}",
                    from.split_once('/').map_or(from.as_str(), |(_, r)| r)
                ),
                BlobPush::Uploaded => "Pushed".to_owned(),
            };
            format!("{}: {what}", &b.digest.hex()[..12])
        })
        .collect();
    let tag = p.reference.rsplit_once(':').map_or("latest", |(_, t)| t);
    lines.push(format!("{tag}: digest: {} size: {}", p.id, p.manifest_size));
    lines
}

fn json(p: &Pushed) -> output::Pushed {
    output::Pushed {
        reference: p.reference.clone(),
        id: p.id.to_string(),
        size: p.manifest_size,
        blobs: p
            .blobs
            .iter()
            .map(|b| {
                let (status, from) = match &b.how {
                    BlobPush::Exists => ("exists", None),
                    BlobPush::Mounted(f) => ("mounted", Some(f.clone())),
                    BlobPush::Uploaded => ("uploaded", None),
                };
                output::PushedBlob {
                    digest: b.digest.to_string(),
                    size: b.size,
                    status: status.into(),
                    mounted_from: from,
                }
            })
            .collect(),
    }
}

impl Ctx {
    pub(super) fn tag(&self, source: &str, target: &str) -> AppResult<u8> {
        let m = self.manager()?;
        let found = self.require(&m, source)?;
        let r = Reference::parse(target).map_err(|e| msg(e.to_string()))?;
        if r.digest.is_some() {
            return Err(msg(format!(
                "{target}: refusing to create a tag with a digest reference"
            )));
        }
        let reference = r.to_string();
        m.tag(found.id(), &reference)?;
        if self.json() {
            output::print_value(&output::Tagged {
                reference,
                id: found.id().to_string(),
            })?;
        }
        Ok(0)
    }

    pub(super) async fn push(&self, all_tags: bool, quiet: bool, name: &str) -> AppResult<u8> {
        let m = self.manager()?;
        let targets = push_targets(name, all_tags, &m.store().refs()?).map_err(msg)?;
        let dest = OciImageSource::with_auth(crate::dirs::registry_auth())?
            .with_registries(crate::dirs::registries_conf());
        let human = !self.json() && !quiet;
        if human && let Some((first, _)) = targets.first() {
            let r = Reference::parse(first).map_err(|e| msg(e.to_string()))?;
            println!(
                "The push refers to repository [{}/{}]",
                r.registry, r.repository
            );
        }
        let mut failed = 0;
        for (reference, id) in &targets {
            let status = if quiet { None } else { Some(Status::new()) };
            let shown = familiar(reference);
            let r = m
                .push_with(&dest, id, reference, &|p| {
                    if let Some(s) = &status {
                        s.push(shown, p)
                    }
                })
                .await;
            drop(status);
            match r {
                // `-q`가 `--format`을 이긴다(`ps`, `images`와 같다).
                Ok(p) if quiet => println!("{}", p.reference),
                Ok(p) if self.json() => output::print_line(&json(&p))?,
                Ok(p) => report(&p).iter().for_each(|l| println!("{l}")),
                // 하나만 올리면 그 오류가 명령의 오류다.
                Err(e) if !all_tags => return Err(e.into()),
                Err(e) => {
                    self.target_error(reference, &e);
                    failed += 1;
                }
            }
        }
        if failed > 0 {
            return Err(msg(format!(
                "{failed} of {} tag(s) not pushed",
                targets.len()
            )));
        }
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> ImageId {
        ImageId::new(
            sbx_model::Digest::parse(&format!("sha256:{}", c.to_string().repeat(64))).unwrap(),
        )
    }

    #[test]
    fn push_names_a_local_tag_or_all_tags_of_a_repository() {
        let refs: BTreeMap<String, ImageId> = [
            ("docker.io/library/app:1", 'a'),
            ("docker.io/library/app:2", 'b'),
            ("docker.io/library/app2:1", 'c'),
            ("localhost:5000/app:1", 'a'),
        ]
        .into_iter()
        .map(|(r, c)| (r.to_owned(), id(c)))
        .collect();
        let names = |v: Vec<(String, ImageId)>| v.into_iter().map(|(r, _)| r).collect::<Vec<_>>();
        assert_eq!(
            names(push_targets("app:1", false, &refs).unwrap()),
            ["docker.io/library/app:1"]
        );
        // 태그가 없으면 latest다(Docker와 같다).
        let e = push_targets("app", false, &refs).unwrap_err();
        assert!(
            e.contains("docker.io/library/app:latest") && e.contains("sbx tag"),
            "{e}"
        );
        assert_eq!(
            names(push_targets("app", true, &refs).unwrap()),
            ["docker.io/library/app:1", "docker.io/library/app:2"]
        );
        assert_eq!(
            names(push_targets("localhost:5000/app", true, &refs).unwrap()),
            ["localhost:5000/app:1"]
        );
        assert!(
            push_targets("app:1", true, &refs)
                .unwrap_err()
                .contains("--all-tags")
        );
        assert!(
            push_targets("nope", true, &refs)
                .unwrap_err()
                .contains("no local tags")
        );
        let d = format!("app@sha256:{}", "a".repeat(64));
        assert!(
            push_targets(&d, false, &refs)
                .unwrap_err()
                .contains("digest")
        );
        assert!(push_targets("Bad Name", false, &refs).is_err());
        assert!(!names_a_tag("localhost:5000/app") && names_a_tag("localhost:5000/app:1"));
    }

    #[test]
    fn report_reads_like_docker_push() {
        let d = |c: char| {
            sbx_model::Digest::parse(&format!("sha256:{}", c.to_string().repeat(64))).unwrap()
        };
        let p = Pushed {
            reference: "ghcr.io/me/app:1".into(),
            id: id('f'),
            manifest_size: 528,
            blobs: vec![
                sbx_manage::PushedBlob {
                    digest: d('a'),
                    size: 1,
                    how: BlobPush::Uploaded,
                },
                sbx_manage::PushedBlob {
                    digest: d('b'),
                    size: 2,
                    how: BlobPush::Exists,
                },
                sbx_manage::PushedBlob {
                    digest: d('c'),
                    size: 3,
                    how: BlobPush::Mounted("ghcr.io/me/base".into()),
                },
                sbx_manage::PushedBlob {
                    digest: d('e'),
                    size: 4,
                    how: BlobPush::Uploaded,
                },
            ],
        };
        assert_eq!(
            report(&p),
            [
                "aaaaaaaaaaaa: Pushed".to_owned(),
                "bbbbbbbbbbbb: Layer already exists".to_owned(),
                "cccccccccccc: Mounted from me/base".to_owned(),
                format!("1: digest: {} size: 528", id('f')),
            ]
        );
        let j = json(&p);
        assert_eq!(j.blobs.len(), 4);
        assert_eq!(j.blobs[2].status, "mounted");
        assert_eq!(j.blobs[2].mounted_from.as_deref(), Some("ghcr.io/me/base"));
        assert_eq!(j.blobs[3].mounted_from, None);
    }
}
