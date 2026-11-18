//! 빌드 오류.

use std::fmt;
use std::io;

/// 빌드 오류. 단계에서 난 것은 그 단계의 줄과 명령을 함께 보인다.
#[derive(Debug)]
pub enum BuildError {
    /// 단계 하나가 할 수 없는 일(없는 원본, 범위 밖 기능, 잘못된 값).
    Step {
        /// Dockerfile의 줄(1부터).
        line: usize,
        /// 풀린 명령.
        text: String,
        /// 까닭(영어 문장).
        message: String,
    },
    /// `RUN`이 0이 아닌 코드로 끝났다.
    RunFailed {
        /// 줄.
        line: usize,
        /// 풀린 명령.
        text: String,
        /// 종료 코드.
        code: i32,
        /// 출력의 끝부분(사람에게 보인다).
        output: String,
        /// VM이 거부한 연결 대상(`--allow`로 열 수 있다).
        blocked: Vec<String>,
    },
    /// 바탕 이미지에 대한 것(받기 실패, 받지 않는 config).
    Base {
        /// 참조.
        reference: String,
        /// 까닭.
        message: String,
    },
    /// 저장소나 VM 쪽 실패. 바이너리가 만든 문장이다.
    Backend(String),
    /// 파일 입출력.
    Io(io::Error),
}

impl BuildError {
    /// 단계 오류.
    pub fn step(step: &sbx_model::build::Step, message: impl Into<String>) -> Self {
        Self::Step {
            line: step.line,
            text: step.text.clone(),
            message: message.into(),
        }
    }

    /// 바깥(저장소, VM) 오류.
    pub fn backend(e: impl fmt::Display) -> Self {
        Self::Backend(e.to_string())
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::Step {
                line,
                text,
                message,
            } => write!(f, "line {line}: {text}: {message}"),
            BuildError::RunFailed {
                line, text, code, ..
            } => write!(
                f,
                "line {line}: {text}: the command returned a non-zero code: {code}"
            ),
            BuildError::Base { reference, message } => write!(f, "{reference}: {message}"),
            BuildError::Backend(m) => f.write_str(m),
            BuildError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BuildError {}

impl From<io::Error> for BuildError {
    fn from(e: io::Error) -> Self {
        BuildError::Io(e)
    }
}

impl From<sbx_rootfs::RootfsError> for BuildError {
    fn from(e: sbx_rootfs::RootfsError) -> Self {
        BuildError::Backend(e.to_string())
    }
}
