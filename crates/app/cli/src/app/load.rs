//! `sbx load`, `sbx save`. 레지스트리 없이 OCI image layout으로 이미지를 들이고 내보낸다.
//!
//! - `load`: tar(gzip이어도 된다, 표준 입력도 된다)는 캐시의 작업 공간에 풀고([`sbx_oci::layout::unpack`]),
//!   디렉터리는 그대로 연다. 그 뒤는 `pull`과 같다: 같은 검증(digest, 플랫폼, 레이어 수)과 같은 저장.
//!   이름은 archive의 것을 쓰고, `--tag`는 이미지가 하나일 때만 받는다.
//! - `save`: 받은 문서와 blob 그대로 쓴다([`Manager::save`]). 파일이면 옆에 쓰고 이름을 바꾼다(반쯤 쓴 tar가
//!   남지 않는다). 표준 출력이 터미널이면 거절한다.

use std::fs::File;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sbx_image::Platform;
use sbx_manage::Manager;
use sbx_oci::layout::{self, OciLayoutSource};
use sbx_rootfs::erofs::ErofsBuilder;

use super::{AppResult, Ctx, host_arch, msg};
use crate::behavior::ImageMatch;
use crate::output;
use crate::parse;
use crate::progress::Status;

/// 끝나면(실패해도) 지우는 작업 디렉터리.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_dir(tmp: &Path) -> io::Result<Scratch> {
    std::fs::create_dir_all(tmp)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = tmp.join(format!("load-{}-{nanos}", std::process::id()));
    std::fs::create_dir(&dir)?;
    Ok(Scratch(dir))
}

impl Ctx {
    pub(super) async fn load(
        &self,
        input: Option<&Path>,
        tag: Option<&str>,
        platform: Option<&str>,
    ) -> AppResult<u8> {
        let arch = match platform {
            Some(p) => parse::platform(p)?,
            None => host_arch()?,
        };
        let store_layout = self.manager()?.store().layout().clone();
        let mut _scratch = None;
        let dir = match input {
            Some(p) if p.is_dir() => p.to_owned(),
            _ => {
                let s = scratch_dir(&store_layout.tmp_dir())?;
                let status = Status::new();
                status.text("Reading the archive…");
                let r = match input {
                    Some(p) => {
                        let mut f =
                            File::open(p).map_err(|e| msg(format!("{}: {e}", p.display())))?;
                        layout::unpack(&mut f, &s.0)
                    }
                    None => {
                        if io::stdin().is_terminal() {
                            return Err(msg("give the archive with -i FILE or on standard input"));
                        }
                        layout::unpack(&mut io::stdin().lock(), &s.0)
                    }
                };
                status.clear();
                r?;
                let d = s.0.clone();
                _scratch = Some(s);
                d
            }
        };
        let source = OciLayoutSource::open(&dir)?;
        let mut names = source.names()?;
        if let Some(t) = tag {
            if names.len() > 1 {
                return Err(msg(format!(
                    "--tag needs an archive with one image; this one has {}",
                    names.join(", ")
                )));
            }
            names = vec![t.to_owned()];
        }
        if names.is_empty() {
            return Err(msg(
                "the archive's images have no full names; name it with --tag",
            ));
        }
        let m = Manager::new(
            &self.cache_dir,
            Arc::new(source),
            vec![Arc::new(ErofsBuilder)],
        )?;
        let status = Status::new();
        let mut loaded = Vec::new();
        for name in &names {
            let info = m
                .pull_with(name, Platform { arch }, &|p| status.pull(name, p))
                .await?;
            loaded.push(info);
        }
        status.clear();
        if self.json() {
            let v: Vec<output::Pulled> = loaded
                .iter()
                .map(|i| output::Pulled {
                    reference: i.record.reference.clone(),
                    id: i.record.id.to_string(),
                    arch: i.record.arch.oci_name().to_owned(),
                })
                .collect();
            output::print_value(&v)?;
        } else {
            for i in &loaded {
                println!("{}: loaded {}", i.record.reference, i.record.id);
            }
        }
        Ok(0)
    }

    pub(super) fn save(&self, output: Option<&Path>, image: &str) -> AppResult<u8> {
        let m = self.manager()?;
        let found = self.require(&m, image)?;
        let reference = match &found {
            ImageMatch::Ref { reference, .. } => Some(reference.as_str()),
            ImageMatch::Id(_) => None,
        };
        match output {
            None => {
                if io::stdout().is_terminal() {
                    return Err(msg(
                        "refusing to write a tar to the terminal; use -o FILE or redirect",
                    ));
                }
                let mut out = io::BufWriter::new(io::stdout().lock());
                m.save(found.id(), reference, &mut out)?;
                out.flush()?;
            }
            Some(path) => {
                let name = path
                    .file_name()
                    .ok_or_else(|| msg(format!("{}: not a file name", path.display())))?;
                let mut tmp_name = std::ffi::OsString::from(".");
                tmp_name.push(name);
                tmp_name.push(format!(".sbx-{}", std::process::id()));
                let tmp = path.with_file_name(tmp_name);
                let r = (|| -> AppResult<()> {
                    let f =
                        File::create(&tmp).map_err(|e| msg(format!("{}: {e}", tmp.display())))?;
                    let mut out = io::BufWriter::new(f);
                    m.save(found.id(), reference, &mut out)?;
                    out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                    std::fs::rename(&tmp, path)?;
                    Ok(())
                })();
                if r.is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
                r?;
            }
        }
        Ok(0)
    }
}
