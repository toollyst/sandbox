//! sbx에 내장한 게스트 init (VM 안에서 PID 1로 도는 `sandbox-guest`가 든 initramfs).
//!
//! 배포 빌드(`cargo xtask dist`)는 sbx를 빌드하는 대상 아키텍처의 것을 내장한다(VM 게스트는 호스트와 아키텍처가 같다).
//! 개발 빌드는 내장하지 않는다
//! (`--guest-init`이나 캐시에 직접 둔 파일을 쓴다). 넣는 방법은 `build.rs`에 있다.
//!
//! 사용자 이미지는 바꾸지 않는다. 이 파일은 VM을 켤 때 커널이 메모리에 풀어 실행하고, 그 프로그램이
//! 이미지를 `/`로 마운트한 뒤 그 안에서 명령을 실행한다.

include!(concat!(env!("OUT_DIR"), "/embedded_guest.rs"));
