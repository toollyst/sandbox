//! 퍼징 시작 입력(`fuzz/seeds/<대상>/`)을 만든다. 저장소의 테스트가 쓰는 것과 같은 모양의 메시지, 문서, 압축 파일을
//! 실제 인코더(`encode`, `tar::Builder`, gzip, zstd)로 만든다. `cargo xtask fuzz --seeds`가 부른다.
//!
//! 구조화된 입력(`arbitrary`)을 받는 대상(`copy_from_guest`, `rootfs_merge`, `shim_log`)은 사람이 만든 바이트가
//! 뜻이 없으므로 시작 입력이 없다. 퍼저가 빈 입력에서 금방 키운다.
//!
//! 사용: `cargo run --manifest-path fuzz/Cargo.toml --example seeds -- <seeds 디렉터리>`

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use sbx_guest_proto::{
    Entry, Error, ErrorKind, Exec, ExitStatus, FileKind, GuestMsg, HostMsg, Ident, User, WinSize,
};

fn put(dir: &Path, target: &str, name: &str, bytes: &[u8]) {
    let d = dir.join(target);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join(name), bytes).unwrap();
}

fn entry(path: &str, kind: FileKind, mode: u32) -> Entry {
    Entry {
        path: path.into(),
        kind,
        mode,
        uid: 1000,
        gid: 1000,
        mtime: 1_700_000_000,
    }
}

fn guest_msgs(out: &Path) {
    let guest = [
        GuestMsg::HelloAck { version: 2 },
        GuestMsg::Started { pid: 42 },
        GuestMsg::Stdout(b"hi\n".to_vec()),
        GuestMsg::Stderr(vec![0xFF]),
        GuestMsg::Exited(ExitStatus::Code(1)),
        GuestMsg::Exited(ExitStatus::Signal(9)),
        GuestMsg::Entry(entry("t", FileKind::Dir, 0o755)),
        GuestMsg::Entry(entry("t/a", FileKind::File { size: 3 }, 0o644)),
        GuestMsg::Entry(entry(
            "t/l",
            FileKind::Symlink {
                target: "../x".into(),
            },
            0o777,
        )),
        GuestMsg::Data(b"abc".to_vec()),
        GuestMsg::End,
        GuestMsg::Done,
        GuestMsg::Error(Error::new(ErrorKind::NotFound, "no such file")),
        GuestMsg::Stat(None),
        GuestMsg::Stat(Some(entry("w", FileKind::Dir, 0o755))),
        GuestMsg::Listening { port: 3128 },
        GuestMsg::Connect {
            host: "pypi.org".into(),
            port: 443,
        },
    ];
    let host = [
        HostMsg::Hello { version: 2 },
        HostMsg::Exec(Exec {
            args: vec!["/bin/sh".into(), "-c".into(), "echo 한글".into()],
            env: vec![("PATH".into(), "/bin".into())],
            cwd: "/".into(),
            user: User {
                user: Ident::Name("nobody".into()),
                group: Some(Ident::Id(65534)),
            },
            tty: true,
            stdin: true,
            size: Some(WinSize { rows: 24, cols: 80 }),
            main: true,
        }),
        HostMsg::Stdin(b"in".to_vec()),
        HostMsg::Signal(15),
        HostMsg::CopyOut {
            path: "/etc".into(),
        },
        HostMsg::Share {
            tag: "sbx0".into(),
            target: "/data".into(),
            readonly: true,
        },
        HostMsg::Refused {
            reason: "not allowed".into(),
        },
    ];
    let mut stream = Vec::new();
    for (i, m) in guest.iter().enumerate() {
        put(out, "guest_msg", &format!("guest-{i:02}"), &m.encode());
        m.write_to(&mut stream).unwrap();
    }
    for (i, m) in host.iter().enumerate() {
        put(out, "guest_msg", &format!("host-{i:02}"), &m.encode());
    }
    // `cp` 한 번의 답 전체 (프레임으로).
    put(out, "guest_msg", "stream", &stream);
}

fn egress(out: &Path) {
    let cases: &[(&str, &str)] = &[
        (
            "names",
            "pypi.org:443\n*.github.com:*\nlocalhost:8080\napi.github.com 443",
        ),
        ("ipv4", "10.0.0.5:22\n1.1.1.1:53\n10.0.0.5 22"),
        (
            "ipv6",
            "[::1]:8080\n[2001:db8::1]:*\n[2606:4700::1111]:443\n[::1] 8080",
        ),
        (
            "mapped",
            "[::ffff:10.0.0.1]:80\n[64:ff9b::a00:1]:80\n[::ffff:10.0.0.1] 80",
        ),
        ("refused", "example.com:443\nexample.org 443"),
    ];
    for (name, text) in cases {
        put(out, "egress_rules", name, text.as_bytes());
    }
}

fn d(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}

fn oci(out: &Path) {
    let index = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[
  {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"amd64","os":"linux"}}}},
  {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"arm64","os":"linux","variant":"v8"}}}},
  {{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"{}","size":1,"platform":{{"architecture":"unknown","os":"unknown"}}}}
]}}"#,
        d('1'),
        d('2'),
        d('3')
    );
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json",
  "config":{{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"{}","size":10}},
  "layers":[{{"mediaType":"application/vnd.docker.image.rootfs.diff.tar.gzip","digest":"{}","size":20}},
            {{"mediaType":"application/vnd.oci.image.layer.v1.tar+zstd","digest":"{}","size":30}}]}}"#,
        d('a'),
        d('b'),
        d('c')
    );
    let config = format!(
        r#"{{"architecture":"arm64","os":"linux",
  "config":{{"Env":["PATH=/usr/bin","EMPTY=","BARE"],"Cmd":["/bin/sh"],"Entrypoint":null,
            "WorkingDir":"/app","User":"app:staff","Labels":{{"io.github.toollyst.sandbox.kernel":"/boot/vmlinuz"}},
            "StopSignal":"SIGRTMIN+3","Volumes":null,"ExposedPorts":{{"80/tcp":{{}}}}}},
  "rootfs":{{"type":"layers","diff_ids":["{}"]}}}}"#,
        d('c')
    );
    // 첫 바이트는 Content-Type을 고른다(0: 헤더 없음).
    for (name, ct, body) in [
        ("index", 0u8, index.as_str()),
        ("manifest", 0, manifest.as_str()),
        ("manifest-header", 2, manifest.as_str()),
        ("config", 0, config.as_str()),
        (
            "challenge",
            0,
            r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#,
        ),
        (
            "references",
            0,
            "alpine\nalpine:3.20\nghcr.io/o/r:t\nlocalhost:5000/r\nr@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nindex.docker.io/library/x",
        ),
        (
            "values",
            0,
            "SIGTERM\nRTMIN+3\nRTMAX-1\n15\napp:staff\n1000:1000\nroot",
        ),
    ] {
        let mut b = vec![ct];
        b.extend_from_slice(body.as_bytes());
        put(out, "oci_documents", name, &b);
    }
}

fn header(path: &str, kind: tar::EntryType, mode: u32, size: u64) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(kind);
    h.set_mode(mode);
    h.set_size(size);
    h.set_mtime(1_700_000_000);
    h.set_uid(0);
    h.set_gid(0);
    h.set_device_major(1).unwrap();
    h.set_device_minor(3).unwrap();
    // set_path는 `..`를 거부하므로 이름을 직접 쓴다.
    let name = &mut h.as_old_mut().name;
    name[..path.len()].copy_from_slice(path.as_bytes());
    h.set_cksum();
    h
}

fn link(path: &str, kind: tar::EntryType, target: &str) -> tar::Header {
    let mut h = header(path, kind, 0o777, 0);
    h.set_link_name(target).unwrap();
    h.set_cksum();
    h
}

fn layer_tar() -> Vec<u8> {
    use tar::EntryType as T;
    let mut b = tar::Builder::new(Vec::new());
    b.append(&header("./", T::Directory, 0o755, 0), &[][..])
        .unwrap();
    b.append(&header("etc/", T::Directory, 0o755, 0), &[][..])
        .unwrap();
    b.append(
        &header("etc/hosts", T::Regular, 0o644, 9),
        &b"127.0.0.1"[..],
    )
    .unwrap();
    b.append(&link("etc/hosts.bak", T::Link, "etc/hosts"), &[][..])
        .unwrap();
    b.append(&link("boot/vmlinuz", T::Symlink, "../kernel"), &[][..])
        .unwrap();
    b.append(&header("kernel", T::Regular, 0o4755, 1), &b"k"[..])
        .unwrap();
    b.append(&header("dev/null", T::Char, 0o666, 0), &[][..])
        .unwrap();
    b.append(&header("run/fifo", T::Fifo, 0o600, 0), &[][..])
        .unwrap();
    b.append(&header("old/.wh..wh..opq", T::Regular, 0o644, 0), &[][..])
        .unwrap();
    b.append(&header(".wh.gone", T::Regular, 0o644, 0), &[][..])
        .unwrap();
    b.append_pax_extensions([("SCHILY.xattr.user.a", &b"v"[..])])
        .unwrap();
    b.append(&header("xattr", T::Regular, 0o644, 0), &[][..])
        .unwrap();
    b.into_inner().unwrap()
}

fn gzip(raw: &[u8]) -> Vec<u8> {
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(raw).unwrap();
    gz.finish().unwrap()
}

fn image_layer(out: &Path) {
    let raw = layer_tar();
    let zstd = ruzstd::encoding::compress_to_vec(
        raw.as_slice(),
        ruzstd::encoding::CompressionLevel::Fastest,
    );
    // 첫 바이트는 압축을 고른다: 0 없음, 1 gzip, 2 zstd.
    for (name, kind, blob) in [
        ("tar", 0u8, raw.clone()),
        ("gzip", 1, gzip(&raw)),
        ("zstd", 2, zstd),
    ] {
        let mut b = vec![kind];
        b.extend_from_slice(&blob);
        put(out, "image_layer", name, &b);
    }
}

fn executor_repo(out: &Path) {
    let sha = "a".repeat(64);
    let artifact = |os: &str, arch: &str, url: &str| serde_json::json!({"os": os, "architecture": arch, "url": url, "sha256": sha, "size": 1});
    let v1 = serde_json::json!({
        "schema": 1,
        "executors": {
            "fc": {
                "description": "d",
                "versions": {
                    "1.9": {"protocol": 1, "artifacts": [artifact("darwin", "arm64", "fc/1.9.tar.gz")]},
                    "1.10": {"protocol": 1, "artifacts": [
                        artifact("darwin", "arm64", "https://cdn.example.org/fc-1.10.tar.gz"),
                        artifact("linux", "amd64", "../other/fc.tar.gz"),
                    ]},
                    "2.0-rc1": {"protocol": 2, "artifacts": [artifact("darwin", "arm64", "fc/2.0.tar.gz")]},
                }
            }
        }
    });
    let v2 = serde_json::json!({
        "schema": 2,
        "executors": {
            "wick": {"versions": {
                "1.18.0": {"adapter": "firecracker", "artifacts": [artifact("darwin", "arm64", "wick/1.18.0.tar.gz")]},
                "1.18.0+wick.1": {"adapter": "firecracker", "artifacts": [artifact("linux", "amd64", "file:///srv/wick.tar.gz")]},
            }},
            "solo": {"versions": {"1": {"protocol": 2, "artifacts": [artifact("linux", "arm64", "http://localhost:8000/solo.tar.gz")]}}}
        },
        "adapters": {
            "firecracker": {"versions": {
                "0.1.0": {"protocol": 2, "artifacts": [artifact("darwin", "arm64", "a/0.1.0.tar.gz")]},
                "0.2.0": {"protocol": 3, "artifacts": [artifact("darwin", "arm64", "a/0.2.0.tar.gz")]},
            }}
        }
    });
    put(out, "executor_index", "v1.json", v1.to_string().as_bytes());
    put(
        out,
        "executor_index",
        "v2.json",
        serde_json::to_string_pretty(&v2).unwrap().as_bytes(),
    );

    put(
        out,
        "executor_spec",
        "versions",
        b"1.18.0\n1.18.0+wick.10\n1.19.0-rc1\nwick@1.18.0+wick.1",
    );
    put(
        out,
        "executor_spec",
        "repository",
        b"1.9\n1.10\n01.9\nhttps://toollyst.github.io/sandbox/executors",
    );
    put(
        out,
        "executor_spec",
        "local",
        b"latest\n0\n\nhttp://localhost:8000/repo",
    );

    // 묶음: 실행 파일, 디렉터리, pax sparse 1.0과 0.1. 첫 바이트는 종류(짝수 Executor, 홀수 VMM).
    use tar::EntryType as T;
    let mut b = tar::Builder::new(Vec::new());
    b.append(&header("pkg/", T::Directory, 0o755, 0), &[][..])
        .unwrap();
    b.append(
        &header("pkg/sbx-executor", T::Regular, 0o755, 10),
        &b"#!/bin/sh\n"[..],
    )
    .unwrap();
    let mut v10 = b"2\n0\n3\n10\n2\n".to_vec();
    v10.resize(512, 0);
    v10.extend_from_slice(b"abcxy");
    b.append_pax_extensions([
        ("GNU.sparse.major", &b"1"[..]),
        ("GNU.sparse.minor", &b"0"[..]),
        ("GNU.sparse.name", &b"pkg/kernel"[..]),
        ("GNU.sparse.realsize", &b"16"[..]),
    ])
    .unwrap();
    b.append(
        &header(
            "pkg/GNUSparseFile.0/kernel",
            T::Regular,
            0o644,
            v10.len() as u64,
        ),
        v10.as_slice(),
    )
    .unwrap();
    b.append_pax_extensions([
        ("GNU.sparse.name", &b"pkg/sub/disk"[..]),
        ("GNU.sparse.size", &b"8"[..]),
        ("GNU.sparse.map", &b"2,2,6,1"[..]),
    ])
    .unwrap();
    b.append(
        &header("pkg/GNUSparseFile.0/disk", T::Regular, 0o644, 3),
        &b"hiz"[..],
    )
    .unwrap();
    let mut pkg = vec![0u8];
    pkg.extend_from_slice(&b.into_inner().unwrap());
    put(out, "executor_unpack", "executor", &pkg);

    let mut b = tar::Builder::new(Vec::new());
    b.append(&header("firecracker", T::Regular, 0o755, 2), &b"fc"[..])
        .unwrap();
    b.append(&header("../escape", T::Regular, 0o644, 1), &b"x"[..])
        .unwrap();
    let mut vmm = vec![1u8];
    vmm.extend_from_slice(&b.into_inner().unwrap());
    put(out, "executor_unpack", "vmm", &vmm);
}

fn main() {
    let out: PathBuf = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("seeds"));
    for t in [
        "guest_msg",
        "egress_rules",
        "oci_documents",
        "image_layer",
        "executor_index",
        "executor_spec",
        "executor_unpack",
    ] {
        let _ = fs::remove_dir_all(out.join(t));
    }
    guest_msgs(&out);
    egress(&out);
    oci(&out);
    image_layer(&out);
    executor_repo(&out);
    check(&out);
    println!("{}", out.display());
}

/// 시작 입력이 해석의 깊은 곳까지 닿는지 본다(첫 단계에서 거부되는 입력은 시작점으로 쓸모가 적다).
fn check(out: &Path) {
    let read = |t: &str, n: &str| fs::read(out.join(t).join(n)).unwrap();

    let url = url::Url::parse("https://repo.example/executors/v2/index.json").unwrap();
    for n in ["v1.json", "v2.json"] {
        sbx_executor_repo::Index::parse(&read("executor_index", n), &url).expect(n);
    }

    let body = read("oci_documents", "config");
    sbx_oci::manifest::parse_config(&body[1..]).expect("config seed");
    for n in ["index", "manifest", "manifest-header"] {
        let body = read("oci_documents", n);
        sbx_oci::manifest::parse_document(None, &body[1..]).expect(n);
    }

    let tmp = std::env::temp_dir().join(format!("sbx-fuzz-seeds-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
    for n in ["tar", "gzip", "zstd"] {
        let blob = read("image_layer", n);
        let r: Box<dyn std::io::Read> = match blob[0] {
            0 => Box::new(&blob[1..]),
            1 => Box::new(flate2::read::GzDecoder::new(&blob[1..])),
            _ => Box::new(sbx_manage::fuzzing::ZstdReader::new(&blob[1..])),
        };
        let entries = sbx_manage::fuzzing::read_layer(0, r, &tmp).expect(n);
        let mut tree = sbx_rootfs::MergedTree::new();
        tree.apply_layer(entries).expect(n);
        assert_eq!(tree.resolve("/boot/vmlinuz").as_deref(), Some("kernel"));
    }

    let pkg = read("executor_unpack", "executor");
    let archive = tmp.join("pkg.tar.gz");
    fs::write(&archive, gzip(&pkg[1..])).unwrap();
    let root = sbx_executor_repo::unpack::unpack(
        &archive,
        &tmp.join("dest"),
        sbx_executor_repo::unpack::Layout::Executor,
    )
    .expect("executor seed unpacks");
    assert_eq!(
        fs::read(root.join("kernel")).unwrap(),
        b"abc\0\0\0\0\0\0\0xy\0\0\0\0"
    );
    fs::remove_dir_all(&tmp).unwrap();
}
