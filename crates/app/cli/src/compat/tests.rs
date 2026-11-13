//! 표를 clap 정의, 테스트 소스, 문서와 맞춰 본다.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::CommandFactory;

use super::*;

/// 줄이 가리키는 sbx 쪽.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// 명령 경로(`["system", "df"]`).
    Command(Vec<&'static str>),
    /// 루트의 옵션 철자.
    RootFlags(Vec<&'static str>),
    /// `paths`의 명령들이 받는 옵션 철자. `qualified`면 값 이야기가 붙은 줄이다.
    Flags {
        paths: &'static [&'static [&'static str]],
        spellings: Vec<&'static str>,
        qualified: bool,
    },
    /// clap과 맞춰 보지 않는다.
    Text,
}

/// `-m, --memory`처럼 `, `로 이은 철자. 철자 뒤에 공백과 글이 오면 거기서 끝나고 값 이야기가 붙은 줄이다.
fn spellings(text: &'static str) -> (Vec<&'static str>, bool) {
    let mut out = Vec::new();
    for part in text.split(", ") {
        let (flag, rest) = part.split_once(' ').unwrap_or((part, ""));
        assert!(
            flag.starts_with('-') && flag.len() > 1,
            "{text:?}: {flag:?} is not a flag"
        );
        out.push(flag);
        if !rest.is_empty() {
            return (out, true);
        }
    }
    (out, false)
}

/// `sbx_side`면 sbx의 이름(`Row::sbx`가 있으면 그것), 아니면 Docker의 이름이 가리키는 것.
fn target(r: &Row, sbx_side: bool) -> Target {
    let text = if sbx_side {
        r.sbx.unwrap_or(r.docker)
    } else {
        r.docker
    };
    match r.area.kind() {
        Kind::Commands { prefix } if text.starts_with('-') => {
            assert!(prefix.is_empty(), "{text}: a root flag outside Commands");
            Target::RootFlags(spellings(text).0)
        }
        Kind::Commands { prefix } => {
            Target::Command(prefix.iter().copied().chain(text.split(' ')).collect())
        }
        Kind::Flags { paths } => {
            let (spellings, qualified) = spellings(text);
            Target::Flags {
                paths,
                spellings,
                qualified,
            }
        }
        Kind::Text => Target::Text,
    }
}

/// 다 만든 clap 정의(전역 옵션이 하위 명령에 퍼지고 `--help`, `--version`이 붙은 것).
fn cli() -> clap::Command {
    let mut c = crate::cli::Cli::command();
    c.build();
    c
}

fn find<'a>(root: &'a clap::Command, path: &[&str]) -> Option<&'a clap::Command> {
    path.iter()
        .try_fold(root, |c, name| c.find_subcommand(name))
}

/// 옵션 `a`가 철자 `spelling`(`-m`, `--memory`)으로 쓰이는지. 숨은 별칭도 센다.
fn spelled(a: &clap::Arg, spelling: &str) -> bool {
    if let Some(long) = spelling.strip_prefix("--") {
        return a.get_long() == Some(long)
            || a.get_all_aliases().is_some_and(|v| v.contains(&long));
    }
    let mut chars = spelling.chars().skip(1);
    let (Some(c), None) = (chars.next(), chars.next()) else {
        panic!("{spelling}: a short flag is one letter");
    };
    a.get_short() == Some(c) || a.get_all_short_aliases().is_some_and(|v| v.contains(&c))
}

fn accepts(cmd: &clap::Command, spelling: &str) -> bool {
    cmd.get_arguments()
        .filter(|a| !a.is_positional())
        .any(|a| spelled(a, spelling))
}

fn name(path: &[&str]) -> String {
    std::iter::once("sbx")
        .chain(path.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn rows_are_unique() {
    for (i, a) in ROWS.iter().enumerate() {
        assert!(
            ROWS[i + 1..]
                .iter()
                .all(|b| (b.area, b.docker) != (a.area, a.docker)),
            "{:?} {} is listed twice",
            a.area,
            a.docker
        );
    }
    // 같은 절의 두 줄이 같은 옵션을 말하면 안 된다(값 이야기가 붙은 줄은 옵션의 한 값을 말한다).
    for area in Area::ALL {
        let mut seen = BTreeSet::new();
        for r in ROWS.iter().filter(|r| r.area == area) {
            if let Target::Flags {
                spellings,
                qualified: false,
                ..
            } = target(r, false)
            {
                for s in spellings {
                    assert!(seen.insert(s), "{}: {s} is in two rows", area.title());
                }
            }
        }
    }
}

#[test]
fn rows_are_grouped_by_area_in_page_order() {
    let mut last = 0;
    for r in ROWS {
        let at = Area::ALL.iter().position(|a| *a == r.area).unwrap();
        assert!(
            at >= last,
            "{}: keep rows of an area together, in Area::ALL order",
            r.docker
        );
        last = at;
    }
}

#[test]
fn rows_say_what_their_status_needs() {
    let steps: Vec<&str> = STEPS_0_3
        .iter()
        .copied()
        .chain(["C1", LATER, UNPLANNED])
        .collect();
    for r in ROWS {
        let what = format!("{}: {}", r.area.title(), r.docker);
        for text in [r.docker, r.note, r.why] {
            assert!(!text.contains('\n'), "{what}: one line");
        }
        if r.status.works() {
            assert!(
                !r.evidence.is_empty(),
                "{what}: name the test that shows it"
            );
        }
        if r.status == Status::Differs {
            assert!(!r.note.is_empty(), "{what}: say what is different");
        }
        if matches!(r.status, Status::WontDo | Status::NotApplicable) {
            assert!(!r.why.is_empty(), "{what}: say why");
        }
        if r.status == Status::NotYet {
            assert!(steps.contains(&r.step), "{what}: step {:?}", r.step);
            assert!(r.evidence.is_empty(), "{what}: nothing to show yet");
        } else {
            assert!(
                r.step.is_empty(),
                "{what}: a step only for rows not done yet"
            );
        }
        if r.sbx.is_some() {
            assert!(
                r.status.works(),
                "{what}: an sbx name only for rows that work"
            );
        }
    }
}

/// 되는 줄이 말하는 명령과 옵션은 sbx가 받는다.
#[test]
fn rows_that_work_name_what_sbx_accepts() {
    let root = cli();
    for r in ROWS.iter().filter(|r| r.status.accepted()) {
        match target(r, true) {
            Target::Command(path) => assert!(
                find(&root, &path).is_some(),
                "{}: `{}` is not an sbx command",
                r.docker,
                name(&path)
            ),
            Target::RootFlags(spellings) => {
                for s in spellings {
                    assert!(accepts(&root, s), "{}: `sbx {s}` is not accepted", r.docker);
                }
            }
            Target::Flags {
                paths, spellings, ..
            } => {
                // 대표 명령이 받아야 한다. 나머지(create, restart)가 받는 옵션은 아래 테스트가 본다.
                let cmd = find(&root, paths[0])
                    .unwrap_or_else(|| panic!("{}: no `{}`", r.docker, name(paths[0])));
                for s in spellings {
                    assert!(
                        accepts(cmd, s),
                        "{}: `{} {s}` is not accepted; fix the row in compat.rs",
                        r.docker,
                        name(paths[0])
                    );
                }
            }
            Target::Text => {}
        }
    }
}

/// 되지 않는 줄이 말하는 명령과 옵션은 sbx가 받지 않는다. 받게 되면 줄을 고쳐야 한다.
#[test]
fn rows_not_done_name_what_sbx_refuses() {
    let root = cli();
    for r in ROWS.iter().filter(|r| !r.status.accepted()) {
        let accepted: Vec<String> = match target(r, false) {
            Target::Command(path) => find(&root, &path)
                .map(|_| name(&path))
                .into_iter()
                .collect(),
            Target::RootFlags(spellings) => spellings
                .into_iter()
                .filter(|s| accepts(&root, s))
                .map(|s| format!("sbx {s}"))
                .collect(),
            Target::Flags {
                paths,
                spellings,
                qualified: false,
            } => paths
                .iter()
                .filter_map(|path| find(&root, path).map(|cmd| (path, cmd)))
                .flat_map(|(path, cmd)| {
                    spellings
                        .iter()
                        .filter(|s| accepts(cmd, s))
                        .map(|s| format!("{} {s}", name(path)))
                        .collect::<Vec<_>>()
                })
                .collect(),
            // 옵션은 있고 그 값 하나를 말한다(`-v NAME:/path`). 값은 clap이 모른다.
            Target::Flags {
                qualified: true, ..
            }
            | Target::Text => Vec::new(),
        };
        assert!(
            accepted.is_empty(),
            "{}: sbx accepts {accepted:?} now; update its row in compat.rs ({} {})",
            r.area.title(),
            r.status.icon(),
            r.docker
        );
    }
}

/// sbx의 모든 명령과 옵션에 줄이 있다. ➕ 명령(Docker에 없는 것)의 옵션과 하위 명령은 보지 않는다.
#[test]
fn every_sbx_command_and_flag_has_a_row() {
    fn walk<'a>(cmd: &'a clap::Command, path: &[&'a str]) {
        for arg in cmd.get_arguments() {
            if arg.is_positional() || arg.get_id() == "help" {
                continue;
            }
            // 전역 옵션은 루트에서 본다.
            if !path.is_empty() && arg.is_global_set() {
                continue;
            }
            let covered =
                ROWS.iter()
                    .filter(|r| r.status.accepted())
                    .any(|r| match target(r, true) {
                        Target::Flags {
                            paths, spellings, ..
                        } => paths.contains(&path) && spellings.iter().any(|s| spelled(arg, s)),
                        Target::RootFlags(spellings) => {
                            path.is_empty() && spellings.iter().any(|s| spelled(arg, s))
                        }
                        _ => false,
                    });
            assert!(
                covered,
                "`{}` --{}: no row in compat.rs says what it is next to Docker",
                name(path),
                arg.get_long().unwrap_or(arg.get_id().as_str())
            );
        }
        for sub in cmd.get_subcommands() {
            if sub.get_name() == "help" {
                continue;
            }
            let mut p = path.to_vec();
            p.push(sub.get_name());
            let row = ROWS
                .iter()
                .filter(|r| r.status.accepted())
                .find(|r| matches!(target(r, true), Target::Command(c) if c == p))
                .unwrap_or_else(|| panic!("`{}`: no row in compat.rs", name(&p)));
            if row.status != Status::SbxOnly {
                walk(sub, &p);
            }
        }
    }
    walk(&cli(), &[]);
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    // Windows의 git은 줄 끝을 CRLF로 꺼낼 수 있다. 내용만 비교한다.
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .replace("\r\n", "\n")
}

/// `tests/e2e.rs`의 `TESTS` 목록에 든 이름.
fn e2e_tests() -> BTreeSet<String> {
    let src = read(&crate_dir().join("tests/e2e.rs"));
    let start = src
        .find("const TESTS: &[Test] = &[")
        .expect("tests/e2e.rs has a TESTS list");
    let list = &src[start..];
    let list = &list[..list.find("\n    ];").expect("the TESTS list ends")];
    list.split('"')
        .skip(1)
        .step_by(2)
        .filter(|name| src.contains(&format!("fn {name}()")))
        .map(str::to_owned)
        .collect()
}

/// `parse::tests::memory_units` 같은 이 크레이트의 단위 테스트가 있는지: 그 모듈 파일에 `#[test]`가 붙은 함수가 있다.
fn unit_test_exists(path: &str) -> bool {
    let parts: Vec<&str> = path.split("::").collect();
    let Some((func, modules)) = parts.split_last() else {
        return false;
    };
    let src = crate_dir().join("src");
    // `a::b::tests::f`는 `a/b.rs`(안의 `mod tests`)이거나 `a/b/tests.rs`다.
    let candidates = (1..=modules.len()).rev().flat_map(|n| {
        let dir = modules[..n].join("/");
        [
            src.join(format!("{dir}.rs")),
            src.join(format!("{dir}/mod.rs")),
        ]
    });
    candidates.filter(|f| f.is_file()).any(|f| {
        let text = read(&f);
        let lines: Vec<&str> = text.lines().collect();
        lines.iter().enumerate().any(|(i, l)| {
            l.trim_start().starts_with(&format!("fn {func}("))
                && lines[i.saturating_sub(3)..i]
                    .iter()
                    .any(|a| a.contains("#[test]") || a.contains("#[tokio::test"))
        })
    })
}

#[test]
fn evidence_names_tests_that_exist() {
    let e2e = e2e_tests();
    assert!(e2e.len() > 10, "{e2e:?}");
    for r in ROWS {
        for t in r.evidence {
            let found = if t.contains("::") {
                unit_test_exists(t)
            } else {
                e2e.contains(*t)
            };
            assert!(
                found,
                "{}: evidence {t} is not a test in tests/e2e.rs (TESTS) or crates/app/cli/src",
                r.docker
            );
        }
    }
}

/// 진행률: (된다, 할 수 있는 것).
fn works_of(rows: impl Iterator<Item = &'static Row> + Clone) -> (usize, usize) {
    (
        rows.clone().filter(|r| r.status.works()).count(),
        rows.filter(|r| r.status.counted()).count(),
    )
}

fn percent((works, of): (usize, usize)) -> usize {
    // 내림한다: 실제보다 크게 말하지 않는다.
    works * 100 / of.max(1)
}

fn bar((works, of): (usize, usize)) -> String {
    let full = (works * 10 + of / 2) / of.max(1);
    format!("`{}{}`", "█".repeat(full), "░".repeat(10 - full))
}

/// GitHub이 제목에 붙이는 앵커.
fn anchor(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect()
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|")
}

fn step_text(step: &str) -> String {
    match step {
        LATER => "after 0.3".to_owned(),
        UNPLANNED => "not planned yet".to_owned(),
        "C1" => "C1 (after 0.3)".to_owned(),
        s => format!("{s} (0.3)"),
    }
}

fn docker_cell(r: &Row) -> String {
    match r.area.kind() {
        Kind::Text => cell(r.docker),
        _ => format!("`{}`", cell(r.docker)),
    }
}

fn notes_cell(r: &Row) -> String {
    let mut s = String::new();
    if let Some(sbx) = r.sbx {
        let _ = write!(s, "sbx: `{sbx}`. ");
    }
    s.push_str(r.note);
    if !r.why.is_empty() {
        if !s.is_empty() {
            s.push(' ');
        }
        let _ = write!(s, "*Why:* {}", r.why);
    }
    cell(s.trim_end())
}

fn evidence_cell(r: &Row) -> String {
    if !r.evidence.is_empty() {
        let names: Vec<String> = r.evidence.iter().map(|t| format!("`{t}`")).collect();
        names.join(", ")
    } else if r.status == Status::NotYet {
        step_text(r.step)
    } else {
        String::new()
    }
}

const GENERATED: &str = "Generated from crates/app/cli/src/compat.rs. Do not edit: run \
                         `SBX_BLESS=1 cargo test -p sbx-cli --lib compat`.";

fn progress() -> String {
    let all = || ROWS.iter();
    let total = works_of(all());
    let count = |s: Status| all().filter(|r| r.status == s).count();
    let soon = all()
        .filter(|r| r.status == Status::NotYet && STEPS_0_3.contains(&r.step))
        .count();
    let cli = works_of(all().filter(|r| r.area.is_cli()));
    let mut md = format!(
        "<!-- compat-progress:begin -->\nCounted from the {} rows below. \"What applies to sbx\" leaves out \
         ⚫, ⚪ and ➕ rows.\n\n| | |\n| --- | --- |\n\
         | **Works (🟢 or 🟡), of what applies to sbx** | **{} {}% ({} of {})** |\n\
         | **The Docker CLI alone** (without `build`, the Dockerfile and Compose, which come in 0.3 and after) \
         | **{} {}% ({} of {})** |\n",
        ROWS.len(),
        bar(total),
        percent(total),
        total.0,
        total.1,
        bar(cli),
        percent(cli),
        cli.0,
        cli.1
    );
    for s in Status::ALL {
        let extra = if s == Status::NotYet {
            format!(" ({soon} of them in 0.3)")
        } else {
            String::new()
        };
        let _ = writeln!(md, "| {} {} | {}{extra} |", s.icon(), s.label(), count(s));
    }
    md.push_str("\n| Area | Works, of what applies to sbx |\n| --- | --- |\n");
    for area in Area::ALL {
        let n = works_of(all().filter(|r| r.area == area));
        if n.1 == 0 {
            continue;
        }
        let _ = writeln!(
            md,
            "| [{}](#{}) | {} {}% ({} of {}) |",
            area.title(),
            anchor(area.title()),
            bar(n),
            percent(n),
            n.0,
            n.1
        );
    }
    md.push_str("<!-- compat-progress:end -->\n");
    md
}

/// `docs/guide/docker-compatibility.md`.
fn page() -> String {
    let mut md = format!(
        "# Docker compatibility\n\n<!-- {GENERATED} -->\n\n\
         `sbx` takes Docker's commands and flags, so what you know from `docker` carries over. This page lists, \
         for every Docker command, every flag of the commands sbx has, every Dockerfile instruction and the common \
         Compose keys, whether sbx does the same, what is different, and the test that shows it.\n\n\
         Each sandbox is its own microVM, there is no daemon, and the network is closed until you open it. So \
         some Docker features mean nothing here, a few are left out on purpose, and some work a little \
         differently. Where sbx differs, it's deliberate, and the row says why.\n\n\
         The table is code, [`compat.rs`](../../crates/app/cli/src/compat.rs), and a test writes this page from \
         it. The same tests check the table against sbx's command-line definition: a row can't say a flag works \
         unless sbx accepts it and the test it names exists, a row can't say a flag is missing once sbx accepts \
         it, and every sbx command and flag needs a row. So the page can't drift from the code.\n\n\
         **Legend**\n\n| Icon | Meaning |\n| --- | --- |\n\
         | 🟢 | Same as Docker |\n\
         | 🟡 | Works, with the differences given |\n\
         | 🔴 | Not yet. The last column names the [roadmap](../roadmap.md) step that adds it in 0.3, *after 0.3*, \
         or *not planned yet* |\n\
         | ⚫ | Will not do, for the reason given |\n\
         | ⚪ | Not applicable: means nothing when each sandbox is its own VM and there is no daemon |\n\
         | ➕ | sbx only: not in Docker |\n\n\
         **Evidence.** A name such as `run_i_forwards_stdin` is an end-to-end test in \
         [`tests/e2e.rs`](../../crates/app/cli/tests/e2e.rs): it runs the `sbx` binary with a fake executor, \
         without a VM, on Linux and macOS in CI. A name with `::`, such as `parse::tests::memory_units`, is a \
         unit test in [`crates/app/cli/src`](../../crates/app/cli/src). Real VMs are covered by the smoke tests \
         ([testing](../developers/testing-and-releasing.md)).\n\n\
         ## Progress\n\n{}",
        progress()
    );
    for area in Area::ALL {
        let _ = write!(md, "\n## {}\n\n", area.title());
        if !area.intro().is_empty() {
            let _ = write!(md, "{}\n\n", area.intro());
        }
        md.push_str("| Docker | | Notes | Evidence or step |\n| --- | --- | --- | --- |\n");
        for r in ROWS.iter().filter(|r| r.area == area) {
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} |",
                docker_cell(r),
                r.status.icon(),
                notes_cell(r),
                evidence_cell(r)
            );
        }
    }
    md
}

const README_BEGIN: &str = "<!-- docker-compat:begin -->";
const README_END: &str = "<!-- docker-compat:end -->";

/// README의 진행률 한 줄(표시 사이).
fn readme_line() -> String {
    let total = works_of(ROWS.iter());
    let cli = works_of(ROWS.iter().filter(|r| r.area.is_cli()));
    format!(
        "{README_BEGIN}\n**Docker compatibility:** {}% of the Docker CLI's commands and flags that apply to a VM \
         work today ({} of {}); with `build`, the Dockerfile and Compose, which come in 0.3 and after, {}% ({} of \
         {}). [The table](docs/guide/docker-compatibility.md) gives every row, its difference and its test, and CI \
         keeps it in step with the code.\n{README_END}",
        percent(cli),
        cli.0,
        cli.1,
        percent(total),
        total.0,
        total.1
    )
}

#[test]
fn docs_are_current() {
    let root = crate_dir().join("../../..");
    let bless = std::env::var_os("SBX_BLESS").is_some();
    let stale = "is out of date: run `SBX_BLESS=1 cargo test -p sbx-cli --lib compat`";

    let path = root.join("docs/guide/docker-compatibility.md");
    let want = page();
    if bless {
        std::fs::write(&path, &want).unwrap();
    } else {
        assert!(
            read(&path) == want,
            "docs/guide/docker-compatibility.md {stale}"
        );
    }

    let path = root.join("README.md");
    let readme = read(&path);
    let (Some(begin), Some(end)) = (readme.find(README_BEGIN), readme.find(README_END)) else {
        panic!("README.md has no {README_BEGIN} … {README_END}");
    };
    let have = &readme[begin..end + README_END.len()];
    let want = readme_line();
    if bless {
        if have != want {
            std::fs::write(&path, readme.replacen(have, &want, 1)).unwrap();
        }
    } else {
        assert!(
            have == want,
            "the Docker compatibility line in README.md {stale}"
        );
    }
}
