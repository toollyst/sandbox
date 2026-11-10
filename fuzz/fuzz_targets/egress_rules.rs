//! 나가는 연결 허용 목록: 규칙 해석(`sbx-model`의 `AllowRule`), 맞추기, 그리고 게스트가 보낸 `Connect { host, port }`를
//! 호스트가 확인하는 `sbx-runtime`의 `egress::check`. 게스트는 아무 `host` 문자열이나 보낼 수 있다.
//!
//! 입력은 텍스트다. 마지막 줄이 요청(`host port`)이고 나머지 줄이 규칙(`--network allow=` 값 하나씩)이다.
//! `check`는 이름을 DNS로 풀기 때문에, 퍼징에서는 IP 주소인 요청과 어떤 규칙에도 맞지 않는 요청만 넘긴다.
//!
//! 확인하는 것:
//!
//! - 해석된 규칙은 표시한 문자열을 다시 해석하면 같은 규칙이다.
//! - 와일드카드가 아닌 규칙은 자기 호스트에 맞는다. `*.접미사`는 접미사 자체에는 맞지 않고 그 아래 이름에는 맞는다.
//! - 맞추기는 대소문자와 끝의 `.`을 가리지 않는다.
//! - `check`가 허락하면 맞는 규칙이 있고, IP 요청이면 그 주소 하나로만 연결한다. 맞는 규칙이 없으면 거부한다.
//! - `is_public_ip`는 IPv4와 그 IPv4-mapped IPv6에 같은 답을 한다.

#![no_main]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use libfuzzer_sys::fuzz_target;
use sbx_model::{AllowRule, is_public_ip};
use sbx_runtime::egress;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let mut lines: Vec<&str> = text.split('\n').collect();
    let request = lines.pop().unwrap_or_default();
    let (host, port) = match request.rsplit_once(' ') {
        Some((h, p)) => (h, p.parse::<u16>().unwrap_or(443)),
        None => (request, 443),
    };

    let mut rules = Vec::new();
    for line in &lines {
        let Ok(rule) = line.parse::<AllowRule>() else {
            continue;
        };
        let shown = rule.to_string();
        assert_eq!(
            shown.parse::<AllowRule>().as_ref().ok(),
            Some(&rule),
            "{line:?} → {rule:?} → {shown:?} does not read back"
        );
        let p = rule.port.unwrap_or(port);
        match rule.host.strip_prefix("*.") {
            None => assert!(
                rule.matches(&rule.host, p),
                "{rule} does not match its own host"
            ),
            Some(suffix) => {
                assert!(!rule.matches(suffix, p), "{rule} matches its suffix itself");
                assert!(
                    rule.matches(&format!("x.{suffix}"), p),
                    "{rule} does not match a name under it"
                );
            }
        }
        if let Some(rp) = rule.port {
            assert!(!rule.matches(host, rp.wrapping_add(1)));
        }
        let m = rule.matches(host, port);
        assert_eq!(m, rule.matches(&host.to_ascii_uppercase(), port));
        assert_eq!(m, rule.matches(&host.to_ascii_lowercase(), port));
        assert_eq!(m, rule.matches(&format!("{host}."), port));
        rules.push(rule);
    }

    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let any_match = rules.iter().any(|r| r.matches(bare, port));
    let ip = bare.parse::<IpAddr>().ok();
    if ip.is_some() || !any_match {
        match egress::check(&rules, host, port) {
            Ok(addrs) => {
                assert!(any_match, "{host}:{port} allowed with no matching rule");
                let ip = ip.expect("only IP requests reach a successful check here");
                assert_eq!(addrs, vec![SocketAddr::new(ip, port)]);
            }
            Err(_) => assert!(!any_match || ip.is_none()),
        }
    }

    if let Some(ip) = ip {
        let public = is_public_ip(ip);
        if let IpAddr::V4(v4) = ip {
            assert_eq!(public, is_public_ip(IpAddr::V6(v4.to_ipv6_mapped())));
        }
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
            assert!(!public, "{ip} is public");
        }
        if let IpAddr::V4(v4) = ip
            && (v4.is_private() || v4.is_link_local() || v4 == Ipv4Addr::BROADCAST)
        {
            assert!(!public, "{ip} is public");
        }
    }
});
