//! `installer`: 설치 스크립트(`install.sh`, `install.ps1`)의 버전과 그것이 까는 것. 정본은 저장소 맨 위의
//! `installer.env` 한 곳이다.
//!
//! ```text
//! cargo xtask installer                       installer.env의 값을 두 스크립트의 핀 블록에 쓴다
//! cargo xtask installer --check               두 스크립트가 installer.env와 같은지만 본다(cargo test도 본다)
//! cargo xtask installer --check-published DIR DIR(gh-pages)에 이미 올린 이 버전이 지금 스크립트와 같은지 본다
//! cargo xtask installer --publish DIR         DIR(gh-pages)에 install-<버전>.sh/.ps1을 올리고 install.sh/.ps1을
//!                                             가장 새 버전의 사본으로 둔다
//! ```
//!
//! 규칙:
//!
//! - **버전은 셋이고 따로 간다**: 설치 스크립트(`INSTALLER_VERSION`), sbx(`SBX`), 실행기와 그 어댑터
//!   (`EXECUTOR`, `ADAPTER`, `NAME@VERSION`). 스크립트는 핀 블록의 값을 그대로 깐다. 고정은 권고다. 깐 뒤에 짝을
//!   맞추는 것은 없고, sbx 바이너리는 실행기 이름도 버전도 모른다.
//! - `EXECUTOR`가 비어 있으면 스크립트는 실행기를 깔지 않는다(sbx가 처음 쓸 때 공식 저장소의 가장 새 기본
//!   실행기를 받는다). `ADAPTER`는 `EXECUTOR`가 있을 때만 쓴다.
//! - 핀 블록은 `# BEGIN INSTALLER PINS`와 `# END INSTALLER PINS` 사이다. 그 안은 손으로 고치지 않는다.
//! - **올린 것은 바꾸지 않는다**: `install-<버전>.sh`가 이미 있는데 내용이 다르면 `--publish`와
//!   `--check-published`가 실패한다(스크립트를 고쳤으면 `INSTALLER_VERSION`을 올린다). 같으면 아무것도 하지 않는다.
//! - **latest**: `install.sh`와 `install.ps1`은 DIR에 있는 `install-<버전>.*` 중 가장 높은 버전의 사본이다(Pages는
//!   서버 쪽 리다이렉트를 하지 못하고, `curl | sh`가 받는 것이 파일 그대로여야 한다).

use std::path::{Path, PathBuf};

use sbx_executor_install::{Spec, version_cmp};

/// 정본 파일(저장소 맨 위).
pub const ENV_FILE: &str = "installer.env";

const BEGIN: &str = "# BEGIN INSTALLER PINS";
const END: &str = "# END INSTALLER PINS";

/// `installer.env`의 값.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pins {
    /// 설치 스크립트의 버전.
    pub installer: String,
    /// 깔 sbx 버전(`v` 없이).
    pub sbx: String,
    /// 깔 실행기 `NAME@VERSION`. 없으면 깔지 않는다.
    pub executor: Option<String>,
    /// 실행기가 쓰는 어댑터 `NAME@VERSION`.
    pub adapter: Option<String>,
}

/// 버전 글자: 숫자로 시작하고 `[0-9A-Za-z.+-]`. 스크립트에 따옴표 없이 들어가므로 그 밖의 글자는 받지 않는다.
fn version_ok(v: &str) -> bool {
    v.starts_with(|c: char| c.is_ascii_digit())
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'-'))
}

/// `NAME@VERSION`(버전이 있어야 한다).
fn spec_ok(s: &str) -> bool {
    s.parse::<Spec>()
        .is_ok_and(|p| p.version.is_some() && p.to_string() == s)
}

impl Pins {
    /// `installer.env`를 읽는다. `#` 줄과 빈 줄은 무시하고, 모르는 키나 빠진 키는 오류다.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (mut installer, mut sbx, mut executor, mut adapter) = (None, None, None, None);
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| format!("{ENV_FILE}:{}: expected KEY=VALUE", n + 1))?;
            let slot = match k {
                "INSTALLER_VERSION" => &mut installer,
                "SBX" => &mut sbx,
                "EXECUTOR" => &mut executor,
                "ADAPTER" => &mut adapter,
                _ => return Err(format!("{ENV_FILE}:{}: unknown key {k}", n + 1)),
            };
            if slot.replace(v.to_owned()).is_some() {
                return Err(format!("{ENV_FILE}:{}: {k} is given twice", n + 1));
            }
        }
        let need =
            |v: Option<String>, k: &str| v.ok_or_else(|| format!("{ENV_FILE}: {k} is missing"));
        let p = Self {
            installer: need(installer, "INSTALLER_VERSION")?,
            sbx: need(sbx, "SBX")?,
            executor: Some(need(executor, "EXECUTOR")?),
            adapter: Some(need(adapter, "ADAPTER")?),
        }
        .normalized();
        p.check()?;
        Ok(p)
    }

    fn normalized(self) -> Self {
        Self {
            executor: self.executor.filter(|s| !s.is_empty()),
            adapter: self.adapter.filter(|s| !s.is_empty()),
            ..self
        }
    }

    fn check(&self) -> Result<(), String> {
        if !version_ok(&self.installer) {
            return Err(format!(
                "INSTALLER_VERSION {:?} is not a version",
                self.installer
            ));
        }
        if !version_ok(&self.sbx) {
            return Err(format!(
                "SBX {:?} is a version without the v, like 0.3.0",
                self.sbx
            ));
        }
        for (k, v) in [("EXECUTOR", &self.executor), ("ADAPTER", &self.adapter)] {
            if let Some(v) = v
                && !spec_ok(v)
            {
                return Err(format!("{k} {v:?} is not NAME@VERSION"));
            }
        }
        if self.adapter.is_some() && self.executor.is_none() {
            return Err("ADAPTER needs EXECUTOR".into());
        }
        Ok(())
    }

    /// `install.sh`의 핀 블록 안 줄들.
    fn shell(&self) -> Vec<String> {
        vec![
            format!("installer_version={}", self.installer),
            format!("pin_sbx={}", self.sbx),
            format!("pin_executor={}", self.executor.as_deref().unwrap_or("")),
            format!("pin_adapter={}", self.adapter.as_deref().unwrap_or("")),
        ]
    }

    /// `install.ps1`의 핀 블록 안 줄들.
    fn powershell(&self) -> Vec<String> {
        vec![
            format!("$installerVersion = '{}'", self.installer),
            format!("$pinSbx = '{}'", self.sbx),
            format!(
                "$pinExecutor = '{}'",
                self.executor.as_deref().unwrap_or("")
            ),
            format!("$pinAdapter = '{}'", self.adapter.as_deref().unwrap_or("")),
        ]
    }
}

/// `text`의 핀 블록 안을 `lines`로 바꾼 것. 들여쓰기는 BEGIN 줄을 따른다.
fn with_pins(text: &str, lines: &[String]) -> Result<String, String> {
    let begin = text.find(BEGIN).ok_or("no BEGIN INSTALLER PINS line")?;
    let body = text[begin..]
        .find('\n')
        .map(|i| begin + i + 1)
        .ok_or("BEGIN INSTALLER PINS is the last line")?;
    let end_rel = text[body..].find(END).ok_or("no END INSTALLER PINS line")?;
    let line_start = text[..begin].rfind('\n').map_or(0, |i| i + 1);
    let indent = &text[line_start..begin];
    let end = body + end_rel;
    let end_line = text[..end].rfind('\n').map_or(0, |i| i + 1);
    if text[begin + BEGIN.len()..].contains(BEGIN) {
        return Err("two BEGIN INSTALLER PINS lines".into());
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..body]);
    for l in lines {
        out.push_str(indent);
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(&text[end_line..]);
    Ok(out)
}

/// 스크립트 하나: (경로, 지금 내용, installer.env대로 쓴 내용).
type Script = (PathBuf, String, String);

/// 두 스크립트와 그것에 쓸 내용.
fn scripts(root: &Path) -> Result<(Pins, Vec<Script>), String> {
    let env = root.join(ENV_FILE);
    let pins = Pins::parse(
        &std::fs::read_to_string(&env).map_err(|e| format!("{}: {e}", env.display()))?,
    )?;
    let mut out = Vec::new();
    for (file, lines) in [
        ("install.sh", pins.shell()),
        ("install.ps1", pins.powershell()),
    ] {
        let path = root.join(file);
        let now = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let want = with_pins(&now, &lines).map_err(|e| format!("{file}: {e}"))?;
        out.push((path, now, want));
    }
    Ok((pins, out))
}

/// 핀 블록을 쓴다. 바꾼 파일 수를 돌려준다.
pub fn write(root: &Path) -> Result<usize, String> {
    let (_, files) = scripts(root)?;
    let mut changed = 0;
    for (path, now, want) in files {
        if now != want {
            std::fs::write(&path, want).map_err(|e| format!("{}: {e}", path.display()))?;
            changed += 1;
        }
    }
    Ok(changed)
}

/// 두 스크립트가 installer.env와 같은지.
pub fn check(root: &Path) -> Result<Pins, String> {
    let (pins, files) = scripts(root)?;
    for (path, now, want) in files {
        if now != want {
            return Err(format!(
                "{} does not match {ENV_FILE}; run `cargo xtask installer`",
                path.display()
            ));
        }
    }
    Ok(pins)
}

/// 올릴 파일: (DIR 안의 이름, 내용).
fn published(root: &Path) -> Result<Vec<(String, String)>, String> {
    let pins = check(root)?;
    let mut out = Vec::new();
    for ext in ["sh", "ps1"] {
        let src = root.join(format!("install.{ext}"));
        let text = std::fs::read_to_string(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        out.push((format!("install-{}.{ext}", pins.installer), text));
    }
    Ok(out)
}

/// 이미 올린 이 버전이 지금 스크립트와 다르면 오류. 아직 올리지 않았으면 그대로 된다.
pub fn check_published(root: &Path, pages: &Path) -> Result<Vec<String>, String> {
    let mut notes = Vec::new();
    for (name, text) in published(root)? {
        match std::fs::read_to_string(pages.join(&name)) {
            Ok(have) if have == text => notes.push(format!("{name}: published, unchanged")),
            Ok(_) => {
                return Err(format!(
                    "{name} is already published with other content, and a published installer never changes; \
                     bump INSTALLER_VERSION in {ENV_FILE} and run `cargo xtask installer`"
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                notes.push(format!("{name}: not published yet"));
            }
            Err(e) => return Err(format!("{}: {e}", pages.join(&name).display())),
        }
    }
    Ok(notes)
}

/// `install-<버전>.<ext>` 중 가장 높은 버전의 파일 이름.
fn newest(pages: &Path, ext: &str) -> Result<Option<String>, String> {
    let mut best: Option<(String, String)> = None;
    for entry in std::fs::read_dir(pages).map_err(|e| format!("{}: {e}", pages.display()))? {
        let name = entry.map_err(|e| e.to_string())?.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(v) = name
            .strip_prefix("install-")
            .and_then(|r| r.strip_suffix(&format!(".{ext}")))
            .filter(|v| version_ok(v))
        else {
            continue;
        };
        if best.as_ref().is_none_or(|(b, _)| version_cmp(v, b).is_gt()) {
            best = Some((v.to_owned(), name.to_owned()));
        }
    }
    Ok(best.map(|(_, n)| n))
}

/// 올린다: 새 버전 파일을 더하고(있던 것과 다르면 거부), `install.sh`·`install.ps1`을 가장 새 버전의 사본으로 둔다.
/// 한 일을 줄마다 돌려준다.
pub fn publish(root: &Path, pages: &Path) -> Result<Vec<String>, String> {
    let mut notes = check_published(root, pages)?;
    for (name, text) in published(root)? {
        let dst = pages.join(&name);
        if !dst.exists() {
            std::fs::write(&dst, text).map_err(|e| format!("{}: {e}", dst.display()))?;
        }
    }
    for ext in ["sh", "ps1"] {
        let Some(name) = newest(pages, ext)? else {
            continue;
        };
        let latest = pages.join(format!("install.{ext}"));
        let text = std::fs::read(pages.join(&name)).map_err(|e| e.to_string())?;
        if std::fs::read(&latest).ok().as_deref() != Some(&text[..]) {
            std::fs::write(&latest, text).map_err(|e| format!("{}: {e}", latest.display()))?;
            notes.push(format!("install.{ext} is now {name}"));
        } else {
            notes.push(format!("install.{ext} is {name}"));
        }
    }
    Ok(notes)
}

/// `cargo xtask installer [--check | --check-published DIR | --publish DIR]`.
pub fn run(root: &Path, args: &[String]) -> Result<Vec<String>, String> {
    match args {
        [] => {
            let n = write(root)?;
            let p = check(root)?;
            Ok(vec![format!(
                "installer {}: sbx {}, executor {}, adapter {} ({n} file{} changed)",
                p.installer,
                p.sbx,
                p.executor.as_deref().unwrap_or("-"),
                p.adapter.as_deref().unwrap_or("-"),
                if n == 1 { "" } else { "s" }
            )])
        }
        [c] if c == "--check" => {
            check(root).map(|p| vec![format!("installer {}: ok", p.installer)])
        }
        [c, dir] if c == "--check-published" => check_published(root, Path::new(dir)),
        [c, dir] if c == "--publish" => publish(root, Path::new(dir)),
        _ => Err(
            "usage: cargo xtask installer [--check | --check-published DIR | --publish DIR]".into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask lives inside the workspace")
    }

    /// 저장소의 두 스크립트는 installer.env와 같다(고쳤으면 `cargo xtask installer`).
    #[test]
    fn the_scripts_match_installer_env() {
        if let Err(e) = check(root()) {
            panic!("{e}");
        }
    }

    const ENV: &str = "INSTALLER_VERSION=0.0.2\nSBX=0.3.0\nEXECUTOR=wick@1.18.0+wick.2\nADAPTER=firecracker@0.3.0\n";

    #[test]
    fn installer_env_is_checked() {
        let p = Pins::parse(ENV).unwrap();
        assert_eq!(p.executor.as_deref(), Some("wick@1.18.0+wick.2"));
        let empty =
            Pins::parse("# c\nINSTALLER_VERSION=0.0.1\nSBX=0.3.0\nEXECUTOR=\nADAPTER=\n").unwrap();
        assert_eq!((empty.executor, empty.adapter), (None, None));
        for bad in [
            ENV.replace("SBX=0.3.0", "SBX=v0.3.0"),
            ENV.replace("SBX=0.3.0", "SBX=0.3.0;rm"),
            ENV.replace("0.0.2", "x"),
            ENV.replace("wick@1.18.0+wick.2", "wick"),
            ENV.replace("wick@1.18.0+wick.2", "Wick@1"),
            ENV.replace("wick@1.18.0+wick.2", ""),
            ENV.replace("ADAPTER=firecracker@0.3.0\n", ""),
            format!("{ENV}SBX=0.4.0\n"),
            format!("{ENV}OTHER=1\n"),
        ] {
            assert!(Pins::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_pin_block_is_replaced_and_nothing_else() {
        let p = Pins::parse(ENV).unwrap();
        let sh = format!("#!/bin/sh\nx=1\n{BEGIN}: note\nold=1\n{END}\ny=2\n");
        let got = with_pins(&sh, &p.shell()).unwrap();
        assert_eq!(
            got,
            format!(
                "#!/bin/sh\nx=1\n{BEGIN}: note\ninstaller_version=0.0.2\npin_sbx=0.3.0\n\
                 pin_executor=wick@1.18.0+wick.2\npin_adapter=firecracker@0.3.0\n{END}\ny=2\n"
            )
        );
        assert_eq!(with_pins(&got, &p.shell()).unwrap(), got);
        let ps = format!("function F {{\n    {BEGIN}\n    $old = 1\n    {END}\n}}\n");
        let got = with_pins(&ps, &p.powershell()).unwrap();
        assert!(got.contains("\n    $pinSbx = '0.3.0'\n"), "{got}");
        assert!(got.ends_with(&format!("    {END}\n}}\n")), "{got}");
        assert!(with_pins("no block", &p.shell()).is_err());
        assert!(with_pins(&format!("{BEGIN}\n"), &p.shell()).is_err());
    }

    /// 올린 것은 바꾸지 않고, latest는 가장 높은 버전의 사본이다.
    #[test]
    fn published_installers_never_change_and_latest_is_the_newest() {
        let tmp = std::env::temp_dir().join(format!("xtask-installer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let (repo, pages) = (tmp.join("repo"), tmp.join("pages"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&pages).unwrap();
        let put = |version: &str, body: &str| {
            std::fs::write(repo.join(ENV_FILE), ENV.replace("0.0.2", version)).unwrap();
            std::fs::write(repo.join("install.sh"), format!("{BEGIN}\n{END}\n{body}\n")).unwrap();
            std::fs::write(
                repo.join("install.ps1"),
                format!("  {BEGIN}\n  {END}\n{body}\n"),
            )
            .unwrap();
            write(&repo).unwrap();
        };
        // gh-pages에 옛 설치 스크립트만 있었다(버전 파일이 없다): 그대로 두고 처음 버전을 더한다.
        std::fs::write(pages.join("install.sh"), "old").unwrap();
        put("0.0.9", "a");
        publish(&repo, &pages).unwrap();
        let read = |n: &str| std::fs::read_to_string(pages.join(n)).unwrap();
        assert_eq!(read("install.sh"), read("install-0.0.9.sh"));
        assert_eq!(read("install.ps1"), read("install-0.0.9.ps1"));
        // 같은 버전을 다시 올리는 것은 아무 일도 아니다. 내용을 바꾸면 거부한다.
        assert!(publish(&repo, &pages).is_ok());
        put("0.0.9", "b");
        let e = check_published(&repo, &pages).unwrap_err();
        assert!(e.contains("bump INSTALLER_VERSION"), "{e}");
        assert!(publish(&repo, &pages).is_err());
        assert!(read("install-0.0.9.sh").ends_with("a\n"));
        // 새 버전(숫자로 비교: 0.0.10 > 0.0.9)이 latest가 된다.
        put("0.0.10", "b");
        publish(&repo, &pages).unwrap();
        assert!(read("install.sh").ends_with("b\n") && read("install-0.0.9.sh").ends_with("a\n"));
        assert_eq!(read("install.sh"), read("install-0.0.10.sh"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
