//! detached 샌드박스의 소유 프로세스. 데몬 없이 샌드박스마다 하나씩 뜬다.
//!
//! 실행 코어에서 VM은 그것을 시작한 프로세스가 소유하고, 그 프로세스가 끝나면 VM도 끝난다.
//! 띄운 프로세스가 끝나도 샌드박스를 남기려면 대신 시작해 줄 프로세스가 필요하다. 그것이 shim이다.
//!
//! - 띄우기: [`start_detached`] (규칙은 [`launch`] 모듈 문서).
//! - shim 프로세스: [`main`] (규칙은 [`serve`] 모듈 문서). 명령줄은 [`ShimConfig`].
//! - 기본 프로세스 출력: [`log`] 형식으로 샌드박스 로그 디렉터리의 [`MAIN_LOG`]에 쓴다. [`LogReader`]로 읽는다.
//!
//! 다른 프로세스는 shim을 몰라도 된다. exec, cp, stop은 실행 코어가 게스트 연결로 하고,
//! stop이 게스트를 끄면 기본 프로세스가 끝나 shim도 끝난다.
//!
//! **층**: `lib` → `launch`, `serve` → `config` → `log`.
//!
//! - 의존: `sandbox-model`, `sandbox-executor`, `sandbox-runtime`, `executor-process`(shim이 실행기를 고른다)

pub mod config;
pub mod launch;
pub mod log;
pub mod serve;

pub use config::ShimConfig;
pub use launch::{SHIM_LOG, start_detached};
pub use log::{LogReader, LogWriter, MAIN_LOG, Stream};
pub use serve::{READY, main};
