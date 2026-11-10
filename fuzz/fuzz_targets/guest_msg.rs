//! 게스트 ↔ 호스트 메시지 해석 (`sbx-guest-proto`). 호스트가 게스트에게서 받는 모든 것이 이 해석을 거친다.
//!
//! 입력은 프레임 본문 하나이자 프레임이 이어진 바이트열이다. 확인하는 것:
//!
//! - 어떤 바이트에도 패닉하지 않는다(양쪽 방향 모두).
//! - 해석이 되면 인코딩은 표준형이다: 다시 인코딩하면 입력과 바이트까지 같고, 그것을 다시 해석하면 같은 값이다.
//!   그래서 같은 메시지를 두 가지로 읽는 일이 없다.
//! - 복사 항목의 권한은 `0o7777` 안이다.
//! - [`check_relative_path`]를 통과한 경로는 이 OS에서 이름 조각(`Component::Normal`)만으로 나뉜다.
//!   호스트는 통과한 경로만 대상 디렉터리에 붙인다.
//! - 프레임을 이어 읽으면 입력보다 많이 읽지 않고 끝난다.

#![no_main]

use std::path::{Component, Path};

use libfuzzer_sys::fuzz_target;
use sbx_guest_proto::{Entry, GuestMsg, HostMsg, MAX_FRAME_LEN, check_relative_path};

fn entry_ok(e: &Entry) {
    assert_eq!(e.mode & !0o7777, 0, "mode outside 0o7777: {e:?}");
    if check_relative_path(&e.path).is_ok() {
        for c in e.path.split('/') {
            let mut parts = Path::new(c).components();
            assert!(
                matches!(
                    (parts.next(), parts.next()),
                    (Some(Component::Normal(_)), None)
                ),
                "{c:?} in an accepted path {:?} is not one plain name on this OS",
                e.path
            );
        }
    }
}

fuzz_target!(|data: &[u8]| {
    if let Ok(m) = GuestMsg::decode(data) {
        let wire = m.encode();
        assert_eq!(wire, data, "guest message encoding is not canonical: {m:?}");
        assert_eq!(GuestMsg::decode(&wire).as_ref(), Ok(&m));
        if let GuestMsg::Entry(e) | GuestMsg::Stat(Some(e)) = &m {
            entry_ok(e);
        }
    }
    if let Ok(m) = HostMsg::decode(data) {
        let wire = m.encode();
        assert_eq!(wire, data, "host message encoding is not canonical: {m:?}");
        assert_eq!(HostMsg::decode(&wire).as_ref(), Ok(&m));
        if let HostMsg::Entry(e) = &m {
            entry_ok(e);
        }
    }

    // 같은 바이트를 연결에서 읽는 것처럼: 프레임이 끝날 때까지.
    let mut rest = data;
    let mut frames = 0usize;
    loop {
        let before = rest.len();
        match GuestMsg::read_from(&mut rest) {
            Ok(m) => {
                // 머리 4바이트와 본문만큼 읽었다.
                let used = before - rest.len();
                assert_eq!(used, 4 + m.encode().len());
                assert!(used - 4 <= MAX_FRAME_LEN);
                frames += 1;
            }
            Err(_) => break,
        }
    }
    assert!(frames <= data.len() / 5);
});
