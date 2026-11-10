//! 레지스트리가 보낸 문서와 이미지 config (`sbx-oci`), 그리고 그 값으로 기본 프로세스를 정하는 곳
//! (`sbx-runtime`의 `process`). 레지스트리와 이미지는 믿지 않는 입력이다.
//!
//! 첫 바이트가 `Content-Type` 헤더를 고르고, 나머지가 문서 본문이다. 같은 본문을 manifest/index로, image config로,
//! `WWW-Authenticate` 헤더로, 이미지 참조와 config 문자열 값(`StopSignal`, `User`)으로 읽는다.
//!
//! 확인하는 것:
//!
//! - 패닉하지 않는다.
//! - index에서 고른 manifest는 linux/그 아키텍처이고 index가 아니다.
//! - config로 정한 기본 프로세스는 argv가 비지 않고 `PATH`가 있으며 cwd가 비지 않는다. `StopSignal`은 1..=64다.
//! - 이미지 참조, 시그널, 사용자, digest는 표시한 문자열을 다시 읽으면 같은 값이다.

#![no_main]

use libfuzzer_sys::fuzz_target;
use sbx_model::{Arch, Digest, ProcessOverrides, Signal, UserSpec};
use sbx_oci::manifest::{Document, parse_config, parse_document, select};
use sbx_oci::reference::Reference;
use sbx_oci::registry::parse_challenge;
use sbx_oci::scope;
use sbx_runtime::process;

fn content_type(b: u8) -> Option<&'static str> {
    let types = scope::MANIFEST_MEDIA_TYPES
        .iter()
        .chain(scope::REJECTED_MANIFEST_MEDIA_TYPES);
    match b {
        0 => None,
        1 => Some("application/json"),
        2 => Some("application/vnd.oci.image.manifest.v1+json; charset=utf-8"),
        3 => Some("text/plain"),
        n => types.cycle().nth(usize::from(n)).copied(),
    }
}

fn round_trips(s: &str) {
    if let Ok(r) = Reference::parse(s) {
        let shown = r.to_string();
        assert_eq!(
            Reference::parse(&shown).as_ref().ok(),
            Some(&r),
            "{s:?} → {shown:?}"
        );
    }
    if let Ok(sig) = s.parse::<Signal>() {
        assert!((1..=64).contains(&sig.number()));
        let shown = sig.to_string();
        assert_eq!(shown.parse::<Signal>().ok(), Some(sig), "{s:?} → {shown:?}");
    }
    if let Ok(u) = s.parse::<UserSpec>() {
        let shown = u.to_string();
        assert_eq!(
            shown.parse::<UserSpec>().as_ref().ok(),
            Some(&u),
            "{s:?} → {shown:?}"
        );
    }
    if let Ok(d) = Digest::parse(s) {
        assert_eq!(d.as_str(), s);
        assert_eq!(d.hex().len(), 64);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&first, body)) = data.split_first() else {
        return;
    };

    if let Ok(doc) = parse_document(content_type(first), body) {
        match doc {
            Document::Index(entries) => {
                for arch in [Arch::X86_64, Arch::Aarch64] {
                    if let Some(e) = select(&entries, arch) {
                        assert_eq!(e.os.as_deref(), Some("linux"));
                        assert_eq!(e.arch.as_deref(), Some(arch.oci_name()));
                        assert!(scope::MANIFEST_MEDIA_TYPES.contains(&e.media_type.as_str()));
                        assert!(!e.media_type.contains("index") && !e.media_type.contains("list"));
                    }
                }
            }
            Document::Manifest(m) => {
                for l in &m.layers {
                    assert!(
                        scope::LAYER_MEDIA_TYPES
                            .iter()
                            .any(|(_, c)| *c == l.compression)
                    );
                }
            }
        }
    }

    if let Ok(parsed) = parse_config(body) {
        let config = &parsed.config;
        match process::resolve(config, &ProcessOverrides::default()) {
            Ok(p) => {
                assert!(!p.args.is_empty());
                assert!(p.env.iter().any(|(k, _)| k == "PATH"));
                assert!(!p.cwd.is_empty());
            }
            Err(e) => {
                let s = e.to_string();
                assert!(!s.is_empty());
            }
        }
        if let Ok(sig) = process::stop_signal(config, None) {
            assert!((1..=64).contains(&sig.number()));
        }
        for v in [&config.user, &config.stop_signal, &config.working_dir]
            .into_iter()
            .flatten()
        {
            round_trips(v);
        }
        let _ = config.kernel();
    }

    if let Ok(text) = std::str::from_utf8(body) {
        if let Some(c) = parse_challenge(text) {
            let _ = (c.realm, c.service, c.scope);
        }
        round_trips(text);
        for line in text.lines() {
            round_trips(line);
        }
    }
});
