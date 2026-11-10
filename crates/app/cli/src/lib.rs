//! Docker형 CLI `sbx`.
//!
//! CLI는 코어를 표현하는 방식 중 하나일 뿐이다. 도메인 로직을 두지 않고, 다음만 한다.
//!
//! - 인자 해석과 출력 형식 ([`cli`], [`parse`], [`filter`])
//! - 구현체 조립과 코어 호출 ([`app`]), 붙어서 실행하기 ([`attach`]), 실행기 설치와 저장소 ([`executors`])
//! - AI 에이전트용 MCP 서버 ([`mcp`]): 같은 코어를 도구로 내보낸다
//! - `--format json` 출력의 모양 ([`output`]): 스크립트와 도구가 기대는 공개 규약
//! - 관리 코어(`sbx-manage`)와 실행 코어(`sbx-runtime`)를 차례로 불러 Docker와 같은 동작을 만든다
//!
//! 동작은 Docker CLI를 기준으로 한다. 사람들이 이미 익숙하기 때문이다.
//! 명령 목록은 [`commands`], 동작 규칙은 [`behavior`], Docker와 다른 점은
//! [`behavior::DOCKER_DIFFERENCES`]에 있다. 기본 디렉터리는 [`dirs`]에 있다.

// 문서는 unix 전용 항목(붙기, 시그널 전달)을 가리킨다. 링크 검사는 Linux와 macOS의 `cargo doc`이 한다.
#![cfg_attr(not(unix), allow(rustdoc::broken_intra_doc_links))]

pub mod app;
pub mod attach;
pub mod behavior;
pub mod cli;
pub mod commands;
pub mod dirs;
pub mod executors;
pub mod filter;
pub mod guest;
pub mod host;
pub mod mcp;
pub mod output;
pub mod parse;
pub mod progress;

/// 명령을 실행한다. `mcp`는 [`mcp::serve`]로, `completion`은 여기서, 나머지는 [`app::run`]으로 간다.
pub async fn run(cli: cli::Cli) -> app::AppResult<u8> {
    if cli.format == cli::Format::Json
        && let Some(why) = output::refusal(&cli.command)
    {
        return Err(why.into());
    }
    match cli.command {
        cli::Command::Mcp { .. } => mcp::serve(&cli).await,
        cli::Command::Completion { shell } => {
            use clap::CommandFactory;
            clap_complete::generate(
                shell,
                &mut cli::Cli::command(),
                "sbx",
                &mut std::io::stdout(),
            );
            Ok(0)
        }
        _ => app::run(cli).await,
    }
}
