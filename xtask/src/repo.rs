//! `repo-add`: 실행기 묶음 디렉터리를 실행기 저장소(정적 디렉터리)에 더한다. 형식은 `executor-repo` 크레이트 문서다.
//!
//! ```text
//! cargo xtask repo-add --repo <디렉터리> --bundle <디렉터리> --name <이름> --version <버전>
//!     [--os <os>] [--capabilities <파일>] [--url <접두사>] [--description <글>]
//! ```
//!
//! - 묶음의 `sbx-executor capabilities`를 돌려 규약 버전(`protocol`)과 아키텍처(`architecture`)를 읽는다.
//!   여기서 돌릴 수 없는 묶음(다른 OS, 아키텍처)은 그 호스트에서 저장해 둔 출력을 `--capabilities`로 준다.
//!   `--os`의 기본은 이 호스트 OS(OCI 이름: `linux`, `darwin`)다.
//! - 압축 파일 `<이름>/<버전>/<이름>-<버전>-<os>-<arch>.tar.gz`를 만들고(맨 위 디렉터리 `<이름>-<버전>/`),
//!   크기와 SHA-256을 재서 `index.json`에 적는다. URL은 저장소 기준 상대 경로다. 없으면 `index.json`을 만든다.
//! - `--url <접두사>`: 압축 파일을 다른 곳(예: GitHub Releases)에 올릴 때. URL은 `<접두사>/<파일 이름>`이 되고,
//!   파일은 여전히 저장소 디렉터리의 같은 자리에 만든다(그것을 올린다). 파일 이름에 이름, 버전, 플랫폼이 다 있어
//!   한 곳에 모아도 겹치지 않는다.
//! - 같은 이름, 버전, 플랫폼이 이미 있으면 거부한다(올린 파일은 바꾸지 않는다).
//!
//! 그 디렉터리를 그대로 어디든(GitHub Pages, S3, 웹 서버) 올리면 저장소다.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct Options {
    repo: PathBuf,
    bundle: PathBuf,
    name: String,
    version: String,
    os: String,
    capabilities: Option<PathBuf>,
    url: Option<String>,
    description: Option<String>,
}

fn host_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let (mut repo, mut bundle, mut name, mut version, mut os, mut description) =
        (None, None, None, None, None, None);
    let (mut capabilities, mut url) = (None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut v = || it.next().cloned().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--repo" => repo = Some(PathBuf::from(v()?)),
            "--bundle" => bundle = Some(PathBuf::from(v()?)),
            "--name" => name = Some(v()?),
            "--version" => version = Some(v()?),
            "--os" => os = Some(v()?),
            "--description" => description = Some(v()?),
            "--capabilities" => capabilities = Some(PathBuf::from(v()?)),
            "--url" => url = Some(v()?),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('.')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'))
    };
    let name = name.ok_or("--name is required")?;
    let version = version.ok_or("--version is required")?;
    if !ok(&name) || !ok(&version) {
        return Err(format!("invalid name or version {name}@{version}"));
    }
    Ok(Options {
        repo: repo.ok_or("--repo is required")?,
        bundle: bundle.ok_or("--bundle is required")?,
        name,
        version,
        os: os.unwrap_or_else(|| host_os().into()),
        capabilities,
        url,
        description,
    })
}

/// 묶음의 `capabilities`(또는 저장해 둔 그 출력): (규약 버전, OCI 아키텍처 이름).
fn capabilities(bundle: &Path, saved: Option<&Path>) -> Result<(u64, String), String> {
    let (what, stdout) = match saved {
        Some(f) => (
            f.display().to_string(),
            std::fs::read(f).map_err(|e| format!("{}: {e}", f.display()))?,
        ),
        None => {
            let exe = bundle.join(format!("sbx-executor{}", std::env::consts::EXE_SUFFIX));
            let out = Command::new(&exe)
                .arg("capabilities")
                .output()
                .map_err(|e| format!("cannot run {}: {e}", exe.display()))?;
            if !out.status.success() {
                return Err(format!(
                    "{} capabilities failed: {}",
                    exe.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            (format!("{} capabilities", exe.display()), out.stdout)
        }
    };
    let caps: Value = serde_json::from_slice(&stdout).map_err(|e| format!("{what}: {e}"))?;
    let version = caps["version"]
        .as_u64()
        .ok_or("capabilities has no version")?;
    let arch = caps["arch"]
        .as_str()
        .ok_or("capabilities has no arch")?
        .to_owned();
    Ok((version, arch))
}

/// `dir`의 내용을 맨 위 디렉터리 `top/` 아래에 담은 tar.gz. 일반 파일과 디렉터리만, 이름 순서로.
fn archive(dir: &Path, top: &str) -> Result<Vec<u8>, String> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    let mut b = tar::Builder::new(gz);
    b.mode(tar::HeaderMode::Deterministic);
    b.follow_symlinks(true);
    b.append_dir_all(top, dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    let gz = b.into_inner().map_err(|e| e.to_string())?;
    gz.finish().map_err(|e| e.to_string())
}

/// 더하고 압축 파일 경로를 돌려준다.
pub fn add(args: &[String]) -> Result<PathBuf, String> {
    let o = parse(args)?;
    let (protocol, arch) = capabilities(&o.bundle, o.capabilities.as_deref())?;
    let file_name = format!(
        "{n}-{v}-{os}-{arch}.tar.gz",
        n = o.name,
        v = o.version,
        os = o.os
    );
    let rel = format!("{}/{}/{file_name}", o.name, o.version);
    let url = match &o.url {
        Some(prefix) => format!("{}/{file_name}", prefix.trim_end_matches('/')),
        None => rel.clone(),
    };
    let index_path = o.repo.join("index.json");
    let mut index: Value = match std::fs::read(&index_path) {
        Ok(b) => {
            serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", index_path.display()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({"schema": 1, "executors": {}}),
        Err(e) => return Err(format!("{}: {e}", index_path.display())),
    };
    if index["schema"] != 1 {
        return Err(format!("{}: schema is not 1", index_path.display()));
    }
    let exec = &mut index["executors"][&o.name];
    if let Some(d) = &o.description {
        exec["description"] = json!(d);
    }
    let ver = &mut exec["versions"][&o.version];
    if !ver["protocol"].is_null() && ver["protocol"] != protocol {
        return Err(format!(
            "{}@{} is already published with protocol {}",
            o.name, o.version, ver["protocol"]
        ));
    }
    ver["protocol"] = json!(protocol);
    let artifacts = ver["artifacts"].as_array().cloned().unwrap_or_default();
    if artifacts
        .iter()
        .any(|a| a["os"] == o.os.as_str() && a["architecture"] == arch.as_str())
    {
        return Err(format!(
            "{}@{} for {}/{arch} is already in the repository",
            o.name, o.version, o.os
        ));
    }
    let bytes = archive(&o.bundle, &format!("{}-{}", o.name, o.version))?;
    let sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let file = o.repo.join(&rel);
    std::fs::create_dir_all(file.parent().expect("has a parent")).map_err(|e| e.to_string())?;
    std::fs::write(&file, &bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut artifacts = artifacts;
    artifacts.push(json!({
        "os": o.os,
        "architecture": arch,
        "url": url,
        "sha256": sha,
        "size": bytes.len(),
    }));
    ver["artifacts"] = Value::Array(artifacts);
    let tmp = index_path.with_extension("json.tmp");
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut f, &index).map_err(|e| e.to_string())?;
    f.write_all(b"\n").map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &index_path).map_err(|e| e.to_string())?;
    Ok(file)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn published_bundle_is_a_valid_repository() {
        let root = std::env::temp_dir().join(format!("xtask-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundle = root.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        let exe = bundle.join("sbx-executor");
        std::fs::write(
            &exe,
            "#!/bin/sh\necho '{\"version\":2,\"name\":\"t\",\"arch\":\"arm64\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(bundle.join("kernel"), b"k").unwrap();
        let repo = root.join("repo");
        let args = |v: &str, os: &str| -> Vec<String> {
            [
                "--repo",
                repo.to_str().unwrap(),
                "--bundle",
                bundle.to_str().unwrap(),
                "--name",
                "fc",
                "--version",
                v,
                "--os",
                os,
            ]
            .iter()
            .map(|s| s.to_string())
            .collect()
        };
        add(&args("1.0", "linux")).unwrap();
        // 다른 호스트에서 저장해 둔 capabilities, 압축 파일은 다른 곳에.
        let caps = root.join("caps.json");
        std::fs::write(&caps, r#"{"version":2,"name":"t","arch":"amd64"}"#).unwrap();
        let mut elsewhere = args("1.0", "darwin");
        elsewhere.extend(
            [
                "--capabilities",
                caps.to_str().unwrap(),
                "--url",
                "https://dl.example/r/",
            ]
            .map(String::from),
        );
        add(&elsewhere).unwrap();
        // 같은 플랫폼은 다시 올리지 않는다.
        assert!(add(&args("1.0", "linux")).is_err());

        let bytes = std::fs::read(repo.join("index.json")).unwrap();
        let url = url::Url::from_directory_path(&repo)
            .unwrap()
            .join("index.json")
            .unwrap();
        let index = executor_repo::Index::parse(&bytes, &url).unwrap();
        let v = &index.executor("fc").unwrap().versions[0];
        assert_eq!((v.version.as_str(), v.protocol), ("1.0", 2));
        assert_eq!(v.artifacts.len(), 2);
        let d = &v.artifacts[1];
        assert_eq!(
            (d.platform.architecture.as_str(), d.url.as_str()),
            ("amd64", "https://dl.example/r/fc-1.0-darwin-amd64.tar.gz")
        );
        assert!(repo.join("fc/1.0/fc-1.0-darwin-amd64.tar.gz").is_file());
        // 묶음은 저장소 클라이언트로 풀 수 있고, 맨 위 디렉터리 안에 실행 파일이 있다.
        let a = &v.artifacts[0];
        let file = a.url.to_file_path().unwrap();
        let got = executor_repo::unpack::unpack(&file, &root.join("out")).unwrap();
        assert!(got.ends_with("fc-1.0") && got.join("kernel").is_file());
        std::fs::remove_dir_all(root).unwrap();
    }
}
