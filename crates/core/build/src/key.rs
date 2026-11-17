//! 캐시 키(기획 4.4). 단계의 키는 앞 키, 풀린 명령, 그 단계가 쓰는 값으로 정해진다.
//!
//! - 스테이지의 첫 키: `FROM 이미지`는 그 이미지 ID, `FROM 스테이지`는 그 스테이지의 마지막 키, `FROM scratch`는 고정 값.
//! - 단계의 키: `sha256(판, 앞 키, 풀린 명령, 덧붙임...)`. config만 바꾸는 단계도 키를 잇는다(뒤의 `RUN`이 그 환경에서
//!   돈다). 덧붙임은 `RUN`이면 그 자리에서 값이 있는 `ARG`들, `COPY`·`ADD`면 원본의 내용 해시(컨텍스트), 원본 스테이지의
//!   마지막 키(`--from=스테이지`), 원본 이미지 ID(`--from=이미지`)다.
//! - 내용 해시에는 수정 시각과 호스트의 소유자가 없다([`crate::source::content_hash`]). 파일을 `touch`만 해서는 다시
//!   만들지 않는다(Docker와 같다).
//! - 판(`VERSION`)은 키를 만드는 규칙이나 층을 만드는 방법이 바뀌면 올린다. 그러면 옛 기록은 맞지 않아 쓰이지 않는다.
//!   (판 2: 층의 diff_id가 층 디스크에서 다시 만든 tar의 sha256이 되었다.)
//! - `SOURCE_DATE_EPOCH`가 있으면 스테이지의 첫 키에 [`epoch`]로 잇는다. 층의 시각이 달라지므로 다른 층이다.

use sha2::{Digest as _, Sha256};

use crate::source::hex;

/// 키 규칙의 판.
pub const VERSION: &str = "sbx-build-cache-v2";

fn hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    h.update(VERSION.as_bytes());
    for p in parts {
        h.update([0u8]);
        h.update(p.len().to_le_bytes());
        h.update(p.as_bytes());
    }
    hex(&h.finalize())
}

/// `FROM 이미지`의 첫 키.
pub fn image(id: &str) -> String {
    hash(&["image", id])
}

/// `FROM scratch`의 첫 키.
pub fn scratch() -> String {
    hash(&["scratch"])
}

/// `SOURCE_DATE_EPOCH`를 첫 키에 잇는다.
pub fn epoch(parent: &str, secs: i64) -> String {
    hash(&[parent, "SOURCE_DATE_EPOCH", &secs.to_string()])
}

/// 단계 하나의 키.
pub fn step(parent: &str, text: &str, extra: &[String]) -> String {
    let mut parts = vec![parent, text];
    parts.extend(extra.iter().map(String::as_str));
    hash(&parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_depend_on_every_part_and_its_boundaries() {
        let base = image("sha256:aa");
        assert_ne!(base, image("sha256:ab"));
        assert_ne!(base, scratch());
        let k = step(&base, "RUN make", &[]);
        assert_eq!(k, step(&base, "RUN make", &[]));
        assert_ne!(k, step(&scratch(), "RUN make", &[]));
        assert_ne!(k, step(&base, "RUN make ", &[]));
        assert_ne!(
            step(&base, "RUN make", &["A=1".into()]),
            step(&base, "RUN make", &["A=2".into()])
        );
        // 조각의 경계가 키에 들어 있다.
        assert_ne!(
            step(&base, "ab", &["c".into()]),
            step(&base, "a", &["bc".into()])
        );
        assert_eq!(k.len(), 64);
    }
}
