use std::collections::BTreeSet;

use super::*;

#[derive(Clone, Copy, PartialEq)]
enum DockerState {
    Up,
    Missing,
    NoDaemon,
    OtherArch,
}

/// 두 빌더를 흉내 낸다: 태그가 생기고 지워지는 것, 빌드 시간(sbx는 cold 1000 ms, 그 뒤 100 + i ms, docker는 cold
/// 2000 ms, 그 뒤 200 + i ms), 만든 이미지의 파일 목록, 빌드가 본 `src/main.txt`.
struct Fake {
    log: Vec<String>,
    docker: DockerState,
    sbx_tags: BTreeSet<String>,
    docker_tags: BTreeSet<String>,
    /// 이 경우의 이미지는 Docker 쪽 파일 목록이 다르다.
    differ: Option<&'static str>,
    /// 이 태그의 빌드는 실패한다.
    fail: Option<String>,
    /// (도구, 태그, 빌드가 본 src/main.txt)
    seen: Vec<(String, String, String)>,
}

impl Fake {
    fn new(docker: DockerState) -> Fake {
        Fake {
            log: Vec::new(),
            docker,
            sbx_tags: BTreeSet::new(),
            docker_tags: BTreeSet::new(),
            differ: None,
            fail: None,
            seen: Vec::new(),
        }
    }

    fn builds(&self, program: &str) -> Vec<&String> {
        self.log
            .iter()
            .filter(|l| l.starts_with(&format!("{program} build ")))
            .collect()
    }
}

fn ok(stdout: &str) -> Result<Ran, String> {
    Ok(Ran {
        ok: true,
        stdout: stdout.into(),
        stderr: String::new(),
        elapsed: Duration::ZERO,
    })
}

fn fail(stderr: &str) -> Result<Ran, String> {
    Ok(Ran {
        ok: false,
        stdout: String::new(),
        stderr: stderr.into(),
        elapsed: Duration::ZERO,
    })
}

const BASE: &str = "alpine:3.20";

impl Shell for Fake {
    fn run(&mut self, program: &str, args: &[String]) -> Result<Ran, String> {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let docker = program == DOCKER;
        if docker && self.docker == DockerState::Missing {
            return Err("cannot run docker: not found".into());
        }
        self.log.push(command_line(program, args));
        let tags = if docker {
            &mut self.docker_tags
        } else {
            &mut self.sbx_tags
        };
        match a.as_slice() {
            ["info", "--format", "json"] => ok(
                r#"{"version":"0.3.0","arch":"arm64","executor":{"name":"wick@1.0.0","adapter":"firecracker@0.3.0"}}"#,
            ),
            ["--format", "json", "images"] => {
                if tags.contains(BASE) {
                    ok(r#"[{"references":["docker.io/library/alpine:3.20"]}]"#)
                } else {
                    ok("[]")
                }
            }
            ["version", ..] => match self.docker {
                DockerState::NoDaemon => Ok(Ran {
                    ok: false,
                    stdout: r#"{"Client":{"Version":"29.9.3"},"Server":null}"#.into(),
                    stderr: "Cannot connect to the Docker daemon at unix:///var/run/docker.sock\n"
                        .into(),
                    elapsed: Duration::ZERO,
                }),
                state => ok(&format!(
                    r#"{{"Client":{{"Version":"29.9.3"}},"Server":{{"Platform":{{"Name":"Docker Desktop"}},"Version":"29.9.3","Os":"linux","Arch":"{}"}}}}"#,
                    if state == DockerState::OtherArch {
                        "amd64"
                    } else {
                        "arm64"
                    }
                )),
            },
            ["info", "--format", _] => ok(r#"{"NCPU":8,"MemTotal":8589934592}"#),
            ["buildx", "inspect"] => ok("Name: desktop-linux\nDriver: docker\n"),
            ["image", "inspect", .., image] => {
                if tags.contains(*image) {
                    ok("linux/arm64\n")
                } else {
                    fail("No such image")
                }
            }
            ["pull", .., image] => {
                tags.insert((*image).to_owned());
                ok("")
            }
            ["prepare", _] => ok(""),
            ["tag", from, to] => {
                assert!(tags.contains(*from), "{program} tag {from}");
                tags.insert((*to).to_owned());
                ok("")
            }
            ["build", ..] => {
                let tag = a[a.iter().position(|x| *x == "-t").unwrap() + 1].to_owned();
                let ctx = Path::new(a.last().unwrap());
                let dockerfile = std::fs::read_to_string(ctx.join("Dockerfile")).unwrap();
                let from = dockerfile
                    .lines()
                    .next()
                    .unwrap()
                    .trim_start_matches("FROM ");
                let from = from.split(' ').next().unwrap();
                assert!(
                    tags.contains(from),
                    "{program} builds FROM {from}, which is not there"
                );
                let main = std::fs::read_to_string(ctx.join("src/main.txt")).unwrap_or_default();
                self.seen.push((program.to_owned(), tag.clone(), main));
                if self.fail.as_deref() == Some(tag.as_str()) {
                    return fail("the build broke");
                }
                tags.insert(tag.clone());
                let i: u64 = tag.rsplit('-').next().unwrap().parse().unwrap();
                let ms = match (docker, i) {
                    (false, 1) => 1000,
                    (true, 1) => 2000,
                    (false, i) => 100 + i,
                    (true, i) => 200 + i,
                };
                Ok(Ran {
                    ok: true,
                    stdout: "sha256:0123\n".into(),
                    stderr: String::new(),
                    elapsed: Duration::from_millis(ms),
                })
            }
            ["run", "--rm", tag, "sh", "-c", _] => {
                assert!(tags.contains(*tag), "{program} run {tag}");
                let mut out =
                    "APP_MODE=bench\ndir /app 755 0:0\nuser 0:0\nworkdir /app\n".to_owned();
                if docker && self.differ.is_some_and(|c| tag.contains(&format!("/{c}:"))) {
                    out.push_str("entry '/app/extra' regular file 644 0:0 1\n");
                }
                ok(&out)
            }
            ["rmi", t] => {
                if tags.remove(*t) {
                    ok("")
                } else {
                    fail(&format!("no such image: {t}"))
                }
            }
            ["buildx", "history", "rm", ..] => ok(""),
            other => panic!("unexpected command {program} {other:?}"),
        }
    }
}

fn options(args: &[&str]) -> Options {
    let mut o = parse(Path::new("/"), &strings(args)).unwrap();
    o.sbx = "sbx".into();
    o
}

#[test]
fn parse_reads_the_options() {
    let o = parse(Path::new("/w"), &[]).unwrap();
    assert_eq!(o.runs, 10);
    assert!(o.docker && !o.network && !o.dry_run);
    assert_eq!(o.image, "alpine:3.20");
    assert_eq!(o.format, Format::Markdown);
    assert!(o.sbx.ends_with("release/sbx"));
    let o = parse(
        Path::new("/w"),
        &strings(&[
            "--runs",
            "3",
            "--no-docker",
            "--network",
            "--cpus",
            "2",
            "--memory",
            "1g",
            "--format",
            "json",
            "--image",
            "alpine:3.21",
            "--sbx",
            "/x/sbx",
            "--dry-run",
        ]),
    )
    .unwrap();
    assert_eq!(o.runs, 3);
    assert!(!o.docker && o.network && o.dry_run);
    assert_eq!(
        (o.cpus.as_deref(), o.memory.as_deref()),
        (Some("2"), Some("1g"))
    );
    assert_eq!(o.format, Format::Json);
    assert_eq!(o.image, "alpine:3.21");
    assert_eq!(o.sbx, PathBuf::from("/x/sbx"));
    for bad in [
        &["--runs", "0"][..],
        &["--runs", "x"],
        &["--runs"],
        &["--format", "xml"],
        &["--docker"],
    ] {
        assert!(parse(Path::new("/w"), &strings(bad)).is_err(), "{bad:?}");
    }
}

#[test]
fn references_are_normalized_like_sbx_images() {
    for (given, want) in [
        ("alpine:3.20", "docker.io/library/alpine:3.20"),
        ("alpine", "docker.io/library/alpine:latest"),
        ("user/app:1", "docker.io/user/app:1"),
        ("ghcr.io/me/app", "ghcr.io/me/app:latest"),
        ("localhost:5000/x", "localhost:5000/x:latest"),
        ("localhost:5000/x:2", "localhost:5000/x:2"),
        ("alpine@sha256:abc", "docker.io/library/alpine@sha256:abc"),
    ] {
        assert_eq!(normalize(given), want, "{given}");
    }
}

#[test]
fn the_first_run_is_cold_and_the_rest_make_the_statistics() {
    let ms = Duration::from_millis;
    let s = Samples::from_runs(vec![ms(500), ms(30), ms(10), ms(20), ms(40)]);
    assert_eq!(
        s.stats(),
        Stats {
            cold: ms(500),
            n: 4,
            min: ms(10),
            p50: ms(20),
            p90: ms(40),
            max: ms(40),
        }
    );
}

#[test]
fn differences_and_command_lines() {
    assert_eq!(first_difference("a\nb\n", "a\nb\n"), None);
    assert_eq!(
        first_difference("a\nb\n", "a\nc\n").unwrap(),
        r#"line 2: sbx "b", docker "c""#
    );
    assert_eq!(
        first_difference("a\n", "a\nc\n").unwrap(),
        r#"line 2: sbx "(none)", docker "c""#
    );
    assert_eq!(
        command_line(
            "sbx",
            &strings(&["run", "--rm", "x:1", "sh", "-c", "echo 'hi' | sort"])
        ),
        r#"sbx run --rm x:1 sh -c 'echo '\''hi'\'' | sort'"#
    );
}

#[test]
fn dockerfiles_use_the_local_base_and_label_their_images() {
    let base = "sbx-bench-build/base:n1";
    for f in [File::App, File::MultiStage, File::Apk] {
        let d = dockerfile(f, base, "n1");
        assert!(d.starts_with(&format!("FROM {base}")), "{d}");
        assert!(d.contains(&format!("LABEL {LABEL}=n1\n")), "{d}");
    }
    let multi = dockerfile(File::MultiStage, base, "n1");
    assert_eq!(multi.matches("COPY --from=").count(), 2);
    assert_eq!(multi.matches("FROM ").count(), 3);
    assert!(dockerfile(File::Apk, base, "n1").contains("apk add"));
    assert!(!dockerfile(File::App, base, "n1").contains("apk"));
    let script = files_script(&["/usr/bin/jq", "/etc/apk/world"]);
    assert!(script.contains("find /usr/bin/jq ") && script.contains("find /etc/apk/world "));
    assert!(script.ends_with("} | sort"));
    // 네트워크 경우는 물어야만 있다.
    assert!(cases(false).iter().all(|c| !c.network));
    assert_eq!(cases(true).iter().filter(|c| c.network).count(), 1);
}

/// 두 도구로 잰다: 경우마다 cold 하나와 warm N개, 번갈아 먼저, 같은 컨텍스트, 만든 것은 모두 지운다(이 실행이 받은
/// 바탕까지). 보고는 Markdown과 JSON으로.
#[test]
fn both_tools_are_measured_and_everything_made_is_removed() {
    let mut f = Fake::new(DockerState::Up);
    let o = options(&["--runs", "3", "--cpus", "2"]);
    let r = measure(&mut f, &o, "test host".into(), "t1".into()).unwrap();

    assert_eq!(r.sbx.version, "0.3.0");
    assert_eq!(r.sbx.executor, "wick@1.0.0 (adapter firecracker@0.3.0)");
    assert_eq!(r.platform, "linux/arm64");
    let d = r.docker.as_ref().unwrap();
    assert_eq!(
        (d.client.as_str(), d.driver.as_str(), d.cpus.as_str()),
        ("29.9.3", "docker", "8")
    );
    let names: Vec<&str> = r.cases.iter().map(|c| c.case.name).collect();
    assert_eq!(
        names,
        ["no-cache", "cached", "one-file-changed", "multi-stage"]
    );
    for c in &r.cases {
        assert_eq!(c.same, Same::Yes, "{}", c.case.name);
        assert_eq!(c.sides.len(), 2);
        let sbx = c.sides[0].1.stats();
        let docker = c.sides[1].1.stats();
        assert_eq!((c.sides[0].0, c.sides[1].0), (Side::Sbx, Side::Docker));
        assert_eq!(sbx.cold, Duration::from_millis(1000));
        assert_eq!(sbx.n, 3);
        assert_eq!(sbx.p50, Duration::from_millis(103));
        assert_eq!(docker.p50, Duration::from_millis(203));
        assert!((ratio(c).unwrap() - 103.0 / 203.0).abs() < 1e-9);
    }

    // 지울 것은 모두 지웠다: 빌드 태그, 바탕 태그, 이 실행이 받은 바탕.
    assert!(f.sbx_tags.is_empty(), "{:?}", f.sbx_tags);
    assert!(f.docker_tags.is_empty(), "{:?}", f.docker_tags);
    assert!(r.leftovers.is_empty(), "{:?}", r.leftovers);
    assert!(!std::env::temp_dir().join("sbx-bench-build-t1").exists());
    // 태그로만 지운다.
    for l in f.log.iter().filter(|l| l.contains(" rmi ")) {
        assert!(l.ends_with(BASE) || l.contains(&format!("{REPO}/")), "{l}");
    }

    // 빌드: 4 경우 × 4번 + 채우기 2번, 양쪽 같은 수. sbx는 빌드 VM 옵션, docker는 빌드 기록 파일.
    let (sb, db) = (f.builds("sbx"), f.builds("docker"));
    assert_eq!((sb.len(), db.len()), (18, 18));
    assert!(
        sb.iter()
            .all(|l| l.contains(" --cpus 2 ") && !l.contains("--network"))
    );
    assert!(db.iter().all(|l| l.contains(" --metadata-file ")));
    let no_cache = |case: &str| {
        sb.iter()
            .filter(|l| l.contains(&format!("{REPO}/{case}:")))
            .all(|l| l.contains("--no-cache"))
    };
    assert!(no_cache("no-cache") && no_cache("multi-stage"));
    assert!(sb.iter().filter(|l| l.contains("--no-cache")).count() == 8);

    // 번갈아 먼저: 홀수 번째 빌드는 sbx가, 짝수 번째는 docker가 먼저.
    let order: Vec<(String, String)> = f
        .seen
        .iter()
        .filter(|(_, t, _)| t.starts_with(&format!("{REPO}/no-cache:")))
        .map(|(p, t, _)| (p.clone(), t.clone()))
        .collect();
    let step = |p: &str, i: usize| (p.to_owned(), format!("{REPO}/no-cache:t1-{i}"));
    assert_eq!(
        order,
        [
            step("sbx", 1),
            step("docker", 1),
            step("docker", 2),
            step("sbx", 2),
            step("sbx", 3),
            step("docker", 3),
            step("docker", 4),
            step("sbx", 4),
        ]
    );

    // 한 파일 바꿈: 채울 때는 처음 내용, 그 뒤 빌드마다 새 내용이고 양쪽이 같은 내용을 본다.
    let seen: Vec<&(String, String, String)> = f
        .seen
        .iter()
        .filter(|(_, t, _)| t.starts_with(&format!("{REPO}/one-file-changed:")))
        .collect();
    assert_eq!(seen.len(), 10);
    for (_, t, main) in seen {
        let i: usize = t.rsplit('-').next().unwrap().parse().unwrap();
        let want = if i == 0 {
            "main\n".to_owned()
        } else {
            changed("t1", i)
        };
        assert_eq!(*main, want, "{t}");
    }

    let md = render_markdown(&r);
    assert!(md.contains("- host: test host\n"), "{md}");
    assert!(md.contains("wick@1.0.0"), "{md}");
    assert!(
        md.contains(
            "- docker: 29.9.3 (server 29.9.3, Docker Desktop), linux/arm64, builder driver docker"
        ),
        "{md}"
    );
    assert!(
        md.contains(
            "| no-cache | sbx | 1000 ms | 3 | 102 ms | 103 ms | 104 ms | 104 ms | 0.51× | yes |"
        ),
        "{md}"
    );
    assert!(
        md.contains(
            "| no-cache | docker | 2000 ms | 3 | 202 ms | 203 ms | 204 ms | 204 ms |  |  |"
        ),
        "{md}"
    );
    assert!(md.contains("build VM 2 vCPU, 512m\n"), "{md}");
    assert!(!md.contains("Could not remove"), "{md}");

    let v: Value = serde_json::from_str(&render_json(&r)).unwrap();
    assert_eq!(v["kind"], "sbx-bench-build");
    assert_eq!(v["format"], 1);
    assert_eq!(v["docker"]["compared"], true);
    assert_eq!(v["warm_runs"], 3);
    assert_eq!(v["cases"].as_array().unwrap().len(), 4);
    let c = &v["cases"][0];
    assert_eq!(c["name"], "no-cache");
    assert_eq!(c["sbx"]["cold_ms"], 1000.0);
    assert_eq!(c["sbx"]["warm_ms"], json!([102.0, 103.0, 104.0]));
    assert_eq!(c["docker"]["p50_ms"], 203.0);
    assert_eq!(c["same_files"], true);
    assert_eq!(c["difference"], Value::Null);
}

/// 이미 있던 바탕은 받지도 지우지도 않는다. 네트워크 경우는 sbx에 허용 목록을 준다.
#[test]
fn a_base_that_was_there_stays_and_the_network_case_allows_the_mirror() {
    let mut f = Fake::new(DockerState::Up);
    f.sbx_tags.insert(BASE.into());
    f.docker_tags.insert(BASE.into());
    let o = options(&["--runs", "1", "--network"]);
    let r = measure(&mut f, &o, "h".into(), "t2".into()).unwrap();
    assert_eq!(r.cases.len(), 5);
    assert!(!f.log.iter().any(|l| l.contains(" pull ")), "{:?}", f.log);
    assert_eq!(f.sbx_tags, BTreeSet::from([BASE.to_owned()]));
    assert_eq!(f.docker_tags, BTreeSet::from([BASE.to_owned()]));
    let net: Vec<&String> = f
        .builds("sbx")
        .into_iter()
        .filter(|l| l.contains(&format!("{REPO}/network:")))
        .collect();
    assert_eq!(net.len(), 2);
    assert!(
        net.iter()
            .all(|l| l.contains(&format!("--network allow --allow {APK_MIRROR}")))
    );
    assert!(
        f.builds("docker")
            .iter()
            .all(|l| !l.contains("--allow") && !l.contains("--network"))
    );
}

/// Docker가 없거나 데몬에 닿지 않거나 플랫폼이 다르면 까닭을 쓰고 sbx만 잰다. Docker에는 아무것도 하지 않는다.
#[test]
fn without_a_usable_docker_sbx_is_measured_alone() {
    for (state, why) in [
        (DockerState::Missing, "`docker` is not on PATH"),
        (
            DockerState::NoDaemon,
            "the Docker daemon is not reachable (Cannot connect",
        ),
        (
            DockerState::OtherArch,
            "Docker builds for linux/amd64, sbx for linux/arm64",
        ),
    ] {
        let mut f = Fake::new(state);
        let r = measure(&mut f, &options(&["--runs", "2"]), "h".into(), "t3".into()).unwrap();
        let reason = r.docker.as_ref().unwrap_err();
        assert!(reason.starts_with(why), "{reason}");
        assert!(
            f.log
                .iter()
                .filter(|l| l.starts_with("docker "))
                .all(|l| l.starts_with("docker version")),
            "{:?}",
            f.log
        );
        for c in &r.cases {
            assert_eq!(c.sides.len(), 1);
            assert_eq!(c.same, Same::NotChecked);
            assert_eq!(ratio(c), None);
        }
        let md = render_markdown(&r);
        assert!(
            md.contains(&format!("- docker: not compared: {why}")),
            "{md}"
        );
        assert!(
            md.contains(
                "| cached | sbx | 1000 ms | 2 | 102 ms | 102 ms | 103 ms | 103 ms | – | – |"
            ),
            "{md}"
        );
        assert!(!md.contains("| docker |"), "{md}");
        let v: Value = serde_json::from_str(&render_json(&r)).unwrap();
        assert_eq!(v["docker"]["compared"], false);
        assert_eq!(v["cases"][0]["docker"], Value::Null);
        assert_eq!(v["cases"][0]["same_files"], Value::Null);
        assert!(f.sbx_tags.is_empty());
    }
    // --no-docker는 Docker에 묻지도 않는다.
    let mut f = Fake::new(DockerState::Up);
    let r = measure(
        &mut f,
        &options(&["--runs", "1", "--no-docker"]),
        "h".into(),
        "t4".into(),
    )
    .unwrap();
    assert_eq!(r.docker.unwrap_err(), "--no-docker");
    assert!(!f.log.iter().any(|l| l.starts_with("docker ")));
}

/// 만든 이미지의 파일이 다르면 그 경우에 처음 다른 줄을 적는다.
#[test]
fn different_files_are_reported() {
    let mut f = Fake::new(DockerState::Up);
    f.differ = Some("multi-stage");
    let r = measure(&mut f, &options(&["--runs", "1"]), "h".into(), "t5".into()).unwrap();
    let multi = r
        .cases
        .iter()
        .find(|c| c.case.name == "multi-stage")
        .unwrap();
    assert_eq!(
        multi.same,
        Same::No(
            r#"line 5: sbx "(none)", docker "entry '/app/extra' regular file 644 0:0 1""#.into()
        )
    );
    assert!(r.cases.iter().filter(|c| c.same == Same::Yes).count() == 3);
    let md = render_markdown(&r);
    assert!(
        md.contains("| multi-stage | sbx |") && md.contains("| **no** |"),
        "{md}"
    );
    assert!(md.contains("**multi-stage: the images differ**"), "{md}");
    let v: Value = serde_json::from_str(&render_json(&r)).unwrap();
    assert_eq!(v["cases"][3]["same_files"], false);
    assert!(
        v["cases"][3]["difference"]
            .as_str()
            .unwrap()
            .contains("/app/extra")
    );
}

/// 빌드가 실패하면 오류를 돌려주지만, 그때까지 만든 것은 지운다.
#[test]
fn a_failed_build_still_cleans_up() {
    let mut f = Fake::new(DockerState::Up);
    f.fail = Some(format!("{REPO}/cached:t6-1"));
    let e = measure(&mut f, &options(&["--runs", "1"]), "h".into(), "t6".into()).unwrap_err();
    assert!(e.contains("the build broke"), "{e}");
    assert!(
        e.starts_with("sbx build -q -t sbx-bench-build/cached:t6-1 "),
        "{e}"
    );
    assert!(f.sbx_tags.is_empty(), "{:?}", f.sbx_tags);
    assert!(f.docker_tags.is_empty(), "{:?}", f.docker_tags);
    assert!(!std::env::temp_dir().join("sbx-bench-build-t6").exists());
}

/// `--dry-run`은 아무것도 돌리지 않고 Dockerfile과 명령을 쓴다.
#[test]
fn a_dry_run_lists_the_dockerfiles_and_commands() {
    let mut o = options(&["--runs", "1", "--dry-run"]);
    o.sbx = "/bin/sbx".into();
    let out = dry_run(&o).unwrap();
    assert!(out.starts_with("dry run: nothing runs."), "{out}");
    assert!(
        out.contains("# MultiStage\nFROM sbx-bench-build/base:"),
        "{out}"
    );
    assert!(out.contains("$ /bin/sbx pull alpine:3.20\n"), "{out}");
    assert!(
        out.contains("$ /bin/sbx build -q --no-cache -t sbx-bench-build/no-cache:"),
        "{out}"
    );
    assert!(
        out.contains("$ docker build -q --no-cache --metadata-file "),
        "{out}"
    );
    assert!(
        out.contains("$ /bin/sbx rmi sbx-bench-build/base:"),
        "{out}"
    );
    assert!(!out.contains(" run --rm "), "{out}");
}
