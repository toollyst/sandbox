//! 크레이트 배치: `crates/<계층>/<이름>`(모델만 `crates/model`)과 계층 사이 방향. 맨 아래 모듈이다.
//!
//! - 계층은 [`LAYERS`]의 순서다. 크레이트는 자기보다 아래이거나 같은 계층에만 의존한다(일반 의존만 센다.
//!   dev 의존은 테스트가 위 계층의 fake를 쓰게 둔다). 같은 계층 안의 허용은 `RULES`가 정한다.
//! - 패키지 이름은 `sbx-` 접두사다. 폴더 이름은 계층 안에서 짧은 이름이다.
//! - [`render`]가 만드는 의존 표는 `docs/developers/crates.md`의 표시 사이에 들어간다. `check-deps`는 그 내용이
//!   지금 코드와 같은지 보고, 다르면 `cargo xtask deps-doc`으로 고치라고 한다.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 계층. 아래에서 위로. (폴더, 설명)
pub const LAYERS: &[(&str, &str)] = &[
    ("model", "Shared data types. No I/O."),
    (
        "spec",
        "Public protocols and formats that other programs implement. No workspace dependencies beyond `spec`.",
    ),
    (
        "port",
        "What the cores build on: interfaces (executor, image source) and shared building blocks (rootfs, store, events).",
    ),
    ("adapter", "Implementations of the interfaces in `port`."),
    (
        "core",
        "Sandbox lifecycle (`runtime`) and images (`manage`). They never pick implementations.",
    ),
    (
        "app",
        "Binaries that pick implementations and put the pieces together.",
    ),
    (
        "executor",
        "Executors: programs that speak the executor protocol and run VMs.",
    ),
];

/// 같은 순위의 계층. `app`과 `executor`는 둘 다 맨 위(조립하는 쪽)다.
fn rank(layer: &str) -> Option<usize> {
    let i = LAYERS.iter().position(|(l, _)| *l == layer)?;
    Some(if layer == "executor" { i - 1 } else { i })
}

/// workspace 크레이트 하나.
#[derive(Debug, Clone)]
pub struct Crate {
    pub name: String,
    /// `crates/` 아래 경로 (예: `core/runtime`).
    pub folder: String,
    pub layer: String,
    pub dir: PathBuf,
    /// 직접 일반 의존 (workspace 크레이트만).
    pub deps: BTreeSet<String>,
    /// 실행 파일 이름.
    pub bins: Vec<String>,
}

/// `cargo metadata`로 `crates/` 아래 크레이트를 읽는다. xtask는 뺀다.
pub fn crates(root: &Path) -> Result<Vec<Crate>, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(root)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: {e}"))?;
    let crates_dir = root.join("crates");
    let mut list = Vec::new();
    for p in meta["packages"].as_array().into_iter().flatten() {
        let manifest = PathBuf::from(p["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest.parent().unwrap_or(Path::new("")).to_path_buf();
        let Ok(rel) = dir.strip_prefix(&crates_dir) else {
            continue;
        };
        let folder = rel.to_string_lossy().replace('\\', "/");
        let layer = folder.split('/').next().unwrap_or_default().to_owned();
        let deps = p["dependencies"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["path"].is_string() && d["kind"].is_null())
            .filter_map(|d| d["name"].as_str().map(str::to_owned))
            .collect();
        let bins = p["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| {
                t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "bin"))
            })
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        list.push(Crate {
            name: p["name"].as_str().unwrap_or_default().to_owned(),
            folder,
            layer,
            dir,
            deps,
            bins,
        });
    }
    list.sort_by(|a, b| {
        (rank(&a.layer), &a.layer, &a.folder).cmp(&(rank(&b.layer), &b.layer, &b.folder))
    });
    Ok(list)
}

/// 배치 규칙 위반.
pub fn check(crates: &[Crate]) -> Vec<String> {
    let mut problems = Vec::new();
    let by_name: BTreeMap<&str, &Crate> = crates.iter().map(|c| (c.name.as_str(), c)).collect();
    for c in crates {
        let Some(r) = rank(&c.layer) else {
            problems.push(format!(
                "crates/{} is not in a layer folder ({})",
                c.folder,
                LAYERS
                    .iter()
                    .map(|(l, _)| *l)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            continue;
        };
        let depth = c.folder.split('/').count();
        let want = if c.layer == "model" { 1 } else { 2 };
        if depth != want {
            problems.push(format!(
                "crates/{} should be crates/<layer>/<name> (only the model sits at crates/model)",
                c.folder
            ));
        }
        if !c.name.starts_with("sbx-") {
            problems.push(format!(
                "{} (crates/{}) should be named sbx-…",
                c.name, c.folder
            ));
        }
        for d in &c.deps {
            if let Some(dep) = by_name.get(d.as_str())
                && rank(&dep.layer).is_some_and(|dr| dr > r)
            {
                problems.push(format!(
                    "{} ({}) must not depend on {} from the higher layer {}",
                    c.name, c.layer, d, dep.layer
                ));
            }
        }
    }
    problems
}

pub const DOC: &str = "docs/developers/crates.md";
const BEGIN: &str = "<!-- BEGIN GENERATED: cargo xtask deps-doc -->";
const END: &str = "<!-- END GENERATED -->";

/// 의존 그림과 표 (Markdown).
pub fn render(crates: &[Crate]) -> String {
    let id = |n: &str| n.replace('-', "_");
    let mut s = String::new();
    s.push_str(BEGIN);
    s.push_str("\n\n| Layer | What lives there |\n| --- | --- |\n");
    for (layer, what) in LAYERS {
        s.push_str(&format!("| `{layer}` | {what} |\n"));
    }
    s.push_str("\n```mermaid\nflowchart BT\n");
    for (layer, _) in LAYERS {
        let members: Vec<&Crate> = crates.iter().filter(|c| c.layer == *layer).collect();
        if members.is_empty() {
            continue;
        }
        s.push_str(&format!("  subgraph {layer}\n"));
        for c in members {
            s.push_str(&format!("    {}[{}]\n", id(&c.name), c.name));
        }
        s.push_str("  end\n");
    }
    for c in crates {
        for d in &c.deps {
            s.push_str(&format!("  {} --> {}\n", id(&c.name), id(d)));
        }
    }
    s.push_str("```\n\n");
    s.push_str(
        "| Layer | Folder | Crate | Depends on | Used by |\n| --- | --- | --- | --- | --- |\n",
    );
    for c in crates {
        let used: Vec<String> = crates
            .iter()
            .filter(|o| o.deps.contains(&c.name))
            .map(|o| format!("`{}`", o.name))
            .collect();
        let deps: Vec<String> = c.deps.iter().map(|d| format!("`{d}`")).collect();
        let bins = if c.bins.is_empty() {
            String::new()
        } else {
            format!(
                " (bin {})",
                c.bins
                    .iter()
                    .map(|b| format!("`{b}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let dash = |v: Vec<String>| {
            if v.is_empty() {
                "–".to_owned()
            } else {
                v.join(", ")
            }
        };
        s.push_str(&format!(
            "| {} | [`{}`](../../crates/{}) | `{}`{} | {} | {} |\n",
            c.layer,
            c.folder,
            c.folder,
            c.name,
            bins,
            dash(deps),
            dash(used)
        ));
    }
    s.push('\n');
    s.push_str(END);
    s
}

/// 문서의 생성 부분을 `generated`로 바꾼 전체 내용. 표시가 없으면 오류.
pub fn splice(doc: &str, generated: &str) -> Result<String, String> {
    let start = doc
        .find(BEGIN)
        .ok_or_else(|| format!("{DOC} has no `{BEGIN}` marker"))?;
    let end = doc[start..]
        .find(END)
        .map(|i| start + i + END.len())
        .ok_or_else(|| format!("{DOC} has no `{END}` marker"))?;
    Ok(format!("{}{}{}", &doc[..start], generated, &doc[end..]))
}

/// `cargo xtask deps-doc`: 문서의 생성 부분을 다시 쓴다.
pub fn write_doc(root: &Path) -> Result<PathBuf, String> {
    let crates = crates(root)?;
    let path = root.join(DOC);
    let doc = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {DOC}: {e}"))?;
    let new = splice(&doc, &render(&crates))?;
    if new != doc {
        std::fs::write(&path, new).map_err(|e| format!("cannot write {DOC}: {e}"))?;
    }
    Ok(path)
}

/// 문서가 지금 코드와 같은지. 줄 끝은 보지 않는다(Windows의 git은 꺼낼 때 CRLF로 바꾼다).
pub fn check_doc(root: &Path, crates: &[Crate]) -> Option<String> {
    let doc = match std::fs::read_to_string(root.join(DOC)) {
        Ok(d) => d.replace("\r\n", "\n"),
        Err(e) => return Some(format!("cannot read {DOC}: {e}")),
    };
    match splice(&doc, &render(crates)) {
        Ok(new) if new == doc => None,
        Ok(_) => Some(format!(
            "{DOC} is out of date with the crates: run `cargo xtask deps-doc`"
        )),
        Err(e) => Some(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, folder: &str, deps: &[&str]) -> Crate {
        Crate {
            name: name.into(),
            folder: folder.into(),
            layer: folder.split('/').next().unwrap().into(),
            dir: PathBuf::new(),
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            bins: Vec::new(),
        }
    }

    #[test]
    fn lower_layers_only() {
        let ok = [
            c("sbx-model", "model", &[]),
            c("sbx-runtime", "core/runtime", &["sbx-model"]),
            c("sbx-guest", "app/guest", &[]),
            c(
                "sbx-executor-fake",
                "executor/fake",
                &["sbx-guest", "sbx-model"],
            ),
        ];
        assert!(check(&ok).is_empty(), "{:?}", check(&ok));

        let bad = [
            c("sbx-model", "model", &["sbx-runtime"]),
            c("sbx-runtime", "core/runtime", &[]),
        ];
        assert_eq!(check(&bad).len(), 1);
    }

    #[test]
    fn folders_and_names() {
        let bad = [
            c("sandbox-x", "core/x", &[]),
            c("sbx-y", "misc/y", &[]),
            c("sbx-z", "core/z/deep", &[]),
        ];
        assert_eq!(check(&bad).len(), 3, "{:?}", check(&bad));
    }

    #[test]
    fn splice_replaces_only_the_generated_part() {
        let doc = format!("intro\n{BEGIN}\nold\n{END}\noutro\n");
        let new = splice(&doc, &format!("{BEGIN}\nnew\n{END}")).unwrap();
        assert_eq!(new, format!("intro\n{BEGIN}\nnew\n{END}\noutro\n"));
        assert!(splice("no markers", "x").is_err());
    }
}
