//! Docker형 CLI `sbx`.
//!
//! CLI는 코어를 표현하는 방식 중 하나일 뿐이다. 도메인 로직을 두지 않고, 다음만 한다.
//!
//! - 인자 해석과 출력 형식 ([`cli`], [`parse`], [`filter`])
//! - 구현체 조립과 코어 호출 ([`app`]), 붙어서 실행하기 ([`attach`]), 실행기 설치와 저장소 ([`executors`])
//! - AI 에이전트용 MCP 서버 ([`mcp`]): 같은 코어를 도구로 내보낸다
//! - 셸 자동완성 ([`complete`]): 샌드박스 이름·ID와 이미지 참조를 읽기만 해서 후보로 낸다
//! - 전체 화면 TUI ([`tui`]): 위 명령들을 같은 `sbx` 자식으로 불러 한 화면에 보이고 키로 낸다
//! - `--format json` 출력의 모양 ([`output`]): 스크립트와 도구가 기대는 공개 규약
//! - 관리 코어(`sbx-manage`)와 실행 코어(`sbx-runtime`)를 차례로 불러 Docker와 같은 동작을 만든다
//!
//! 동작은 Docker CLI를 기준으로 한다. 사람들이 이미 익숙하기 때문이다.
//! 명령 목록은 [`commands`], 동작 규칙은 [`behavior`], Docker와 다른 점은
//! [`behavior::DOCKER_DIFFERENCES`]에 있다. 기본 디렉터리는 [`dirs`]에, 디스크 배치 버전은 [`layout`]에 있다.

// 문서는 unix 전용 항목(붙기, 시그널 전달)을 가리킨다. 링크 검사는 Linux와 macOS의 `cargo doc`이 한다.
#![cfg_attr(not(unix), allow(rustdoc::broken_intra_doc_links))]

pub mod app;
pub mod attach;
pub mod behavior;
pub mod choose;
pub mod cli;
pub mod commands;
pub mod complete;
pub mod dirs;
pub mod executors;
pub mod filter;
pub mod guest;
pub mod host;
pub mod layout;
pub mod mcp;
pub mod output;
pub mod parse;
pub mod progress;
pub mod tui;

/// 명령을 실행한다. `mcp`는 [`mcp::serve`]로, `completion`은 [`complete::registration`]으로, 나머지는 [`app::run`]으로 간다.
/// 자동완성 요청(`COMPLETE=<셸> sbx -- ...`)은 여기까지 오지 않는다(`main`이 먼저 받는다).
pub async fn run(cli: cli::Cli) -> app::AppResult<u8> {
    if cli.format == cli::Format::Json
        && let Some(why) = output::refusal(&cli.command)
    {
        return Err(why.into());
    }
    if let Some(code) = &cli.jq {
        if cli.format != cli::Format::Json {
            return Err("--jq needs --format json".into());
        }
        output::set_jq(output::Jq::compile(code)?);
    }
    match cli.command {
        cli::Command::Mcp { .. } => mcp::serve(&cli).await,
        // 화면 루프는 막는 호출이다. 런타임은 그동안 SIGINT 처리기만 돌린다.
        cli::Command::Tui => tokio::task::block_in_place(|| tui::run(&cli)),
        cli::Command::Completion { shell } => {
            complete::registration(shell, &mut std::io::stdout())?;
            Ok(0)
        }
        _ => app::run(cli).await,
    }
}
