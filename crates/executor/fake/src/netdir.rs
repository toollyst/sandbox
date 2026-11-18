//! 투명 TCP 흉내의 파일 자리. 맨 아래 모듈이다.

/// 루프백 DNS와 투명 TCP 흉내의 디렉터리: scratch 디스크 파일 이름 뒤에 `.net`을 붙인 것. VM을 띄울 때 만든다.
///
/// - [`NET_DNS`]: 루프백 DNS가 듣는 `127.0.0.1`의 포트(10진수).
/// - [`NET_ROUTES`]: 줄마다 `가짜주소:포트 127.0.0.1:실제포트`. 게스트 커널의 local 경로 대신이다.
/// - `etc/resolv.conf`: 게스트가 `/etc/resolv.conf`에 쓰는 것(호스트의 `/etc`는 건드리지 않는다).
pub fn net_dir(scratch: &std::path::Path) -> std::path::PathBuf {
    let mut s = scratch.as_os_str().to_owned();
    s.push(".net");
    s.into()
}

/// [`net_dir`] 안의 DNS 포트 파일.
pub const NET_DNS: &str = "dns-port";

/// [`net_dir`] 안의 경로 표.
pub const NET_ROUTES: &str = "routes";
