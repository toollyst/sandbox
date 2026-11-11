//! MCP 도구 인자 → `sbx create` 인자(`sbx-cli`의 `mcp::run_args`). 에이전트는 아무 JSON이나 보낼 수 있다.
//!
//! 확인하는 것([`sbx_cli::mcp::fuzz_tool_arguments`]):
//!
//! - 패닉하지 않는다.
//! - 받아들였으면, 이미지·명령·허용 목록·환경·이름·작업 디렉터리는 받은 그대로이고, 호스트에 닿는 옵션
//!   (`-v`, `--mount`, `--env-file`)과 사람이 정하는 옵션(`--entrypoint`, `-u`, `--rm`, 멈춤 시그널)은 생기지 않는다.
//!   레이블은 MCP 소유 표시 하나다. 도구 입력이 `--`로 시작해도 다른 옵션이 되지 않는다.
//!
//! 입력은 JSON 바이트다. 사전(`fuzz/dict/mcp_args.dict`)이 키 이름과 `--` 꼴의 값을 준다.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&flag, json)) = data.split_first() else {
        return;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(json) else {
        return;
    };
    if !v.is_object() {
        return;
    }
    sbx_cli::mcp::fuzz_tool_arguments(&v, flag & 1 == 1);
});
