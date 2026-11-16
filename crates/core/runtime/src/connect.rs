//! 샌드박스 잇기(`--connect`, 호스트 쪽). VM을 소유한 프로세스가 연결마다 대상을 찾아 그 게스트에 잇는다.
//! 호스트에서는 아무것도 듣지 않고, 대상은 플래그도 네트워크도 필요 없다.
//!
//! 게스트 쪽은 투명 TCP 그대로다(`sbx-guest`의 `transparent`): 게스트 DNS가 이름을 묻고(`Lookup`), 호스트가
//! 잇기 목록으로도 답하면([`crate::egress::lookup`]) 가짜 주소의 그 포트에서 듣고, 받은 연결을 `Connect { host: 이름, port }`로 넘긴다.
//! 프록시(`HTTP_PROXY`, `--network allow`일 때만 있다)로 온 `CONNECT 이름:포트`도 같은 `Connect`다.
//!
//! **찾기** ([`find`]): 기록된 대상([`ConnectRule::target`])으로 같은 data_dir에서 찾는다. 정확한 ID, 그다음
//! 이름이다. ID 앞부분으로는 찾지 않는다: 이름이 사라진 뒤 그 이름으로 시작하는 ID의 다른 샌드박스에 닿지 않게
//! 한다. 만들 때 부르는 쪽이 앞부분을 전체 ID로 바꿔 기록한다. 다른 data_dir의 샌드박스는 보이지 않는다.
//!
//! **잇기** ([`Peers::open`], 연결마다, 이 차례로). 걸리면 그 연결만 거부하고 이유를 기록한다.
//! 1. 대상이 없으면 거부.
//! 2. 대상이 자기 자신이면 거부(만들 때 이름으로도 막는다, [`sbx_model::check_connect`]).
//! 3. 대상이 [`scope`](sbx_model::SandboxSpec::connect_scope)의 라벨을 모두 갖지 않으면 거부.
//! 4. 대상이 실행 중이 아니면 거부(기록과 소유 잠금으로 본다. 소유 프로세스가 죽었으면 멈춘 것이다).
//! 5. 포트가 대상 안의 나가는 연결 프록시 포트면 거부: 대상의 허용 목록과 전송량을 빌려 쓰지 못하게 한다.
//! 6. 대상의 제어 소켓으로 `Forward { port }`를 보낸다(`-p`와 같은 길). 대상 안에서 아무도 듣지 않으면 거부.
//!
//! 연결마다 새로 찾으므로 대상이 다시 시작하면(제어 소켓이 바뀐다) 다음 연결은 새 대상에 간다. 대상이 멈췄으면
//! 기록만 읽고 곧바로 거부한다. 대상이 멈춘 순간 이어져 있던 연결은 끊긴다.
//!
//! 바이트와 연결 수는 잇는 쪽(이 샌드박스)의 것으로 센다(`--egress-limit`, 열린 연결 상한, [`crate::egress`]).
//! 대상의 상한에는 세지 않는다. 대상 쪽에서 보면 `-p`처럼 들어오는 연결이다.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use sbx_executor::{ChannelSpec, Executor, GuestConn};
use sbx_model::{ConnectRule, SandboxId, SandboxSpec, State};

use crate::client::Guest;
use crate::error::RuntimeError;
use crate::records::{self, Paths, StateRecord};

/// 찾은 대상.
#[derive(Debug, Clone)]
pub(crate) struct Found {
    pub id: SandboxId,
    pub spec: SandboxSpec,
    pub state: StateRecord,
}

/// 기록된 대상으로 찾는다. 규칙은 모듈 문서에 있다. 없으면 `None`.
pub(crate) fn find(
    data_dir: &std::path::Path,
    target: &str,
) -> Result<Option<Found>, RuntimeError> {
    let read = |id: SandboxId| -> Result<Option<Found>, RuntimeError> {
        Ok(
            records::read(&Paths::new(data_dir, &id))?.map(|(spec, state)| Found {
                id,
                spec,
                state,
            }),
        )
    };
    if let Ok(id) = SandboxId::new(target)
        && let Some(f) = read(id)?
    {
        return Ok(Some(f));
    }
    if sbx_model::check_sandbox_name(target).is_ok()
        && let Some(id) = records::name_owner(data_dir, target)?
    {
        return read(id);
    }
    Ok(None)
}

/// 대상이 `scope`의 라벨을 모두 가졌는지. 아니면 거부 이유.
pub(crate) fn in_scope(
    scope: &BTreeMap<String, String>,
    target: &str,
    spec: &SandboxSpec,
) -> Result<(), String> {
    match scope.iter().find(|(k, v)| spec.labels.get(*k) != Some(*v)) {
        None => Ok(()),
        Some((k, v)) => Err(format!(
            "sandbox {target} is outside the sandboxes this one may connect to (it lacks the label {k}={v})"
        )),
    }
}

/// 만들 때의 확인: 대상이 모두 있고 범위 안이다. 규칙은 모듈 문서에 있다(잇기의 1과 3).
pub(crate) fn check_targets(
    data_dir: &std::path::Path,
    spec: &SandboxSpec,
) -> Result<(), RuntimeError> {
    for r in &spec.connect {
        let found = find(data_dir, &r.target)?.ok_or_else(|| {
            RuntimeError::Invalid(format!("--connect {r}: no sandbox {}", r.target))
        })?;
        in_scope(&spec.connect_scope, &r.target, &found.spec)
            .map_err(|e| RuntimeError::Invalid(format!("--connect {r}: {e}")))?;
    }
    Ok(())
}

/// 이어진 대상.
pub(crate) struct Peer {
    /// 대상 ID.
    pub id: SandboxId,
    /// 대상 게스트의 `127.0.0.1:port`로 이어진 연결(`Forward`가 받아들여진 뒤).
    pub conn: Box<dyn GuestConn>,
}

/// 거부: 이유와(찾았으면) 대상 ID.
pub(crate) type Refusal = (String, Option<SandboxId>);

/// 한 샌드박스의 잇기. 소유 프로세스가 시작할 때 만든다.
pub(crate) struct Peers {
    /// `--connect` 목록.
    pub rules: Vec<ConnectRule>,
    /// 대상을 찾는 data_dir(이 샌드박스의 것).
    pub data_dir: PathBuf,
    /// 대상 게스트에 연결할 실행기. 연결은 규약(제어 소켓과 vsock 방식)만으로 되므로 대상을 띄운 실행기가 아니어도 된다.
    pub executor: Arc<dyn Executor>,
    /// 이 샌드박스.
    pub me: SandboxId,
    /// 대상이 가져야 하는 라벨.
    pub scope: BTreeMap<String, String>,
}

impl Peers {
    /// 이름에 맞는 규칙의 대상(포트는 보지 않는다).
    pub fn target_of(&self, name: &str) -> Option<&str> {
        self.rules
            .iter()
            .find(|r| r.matches_name(name))
            .map(|r| r.target.as_str())
    }

    /// `host:port`에 맞는 규칙.
    pub fn rule(&self, host: &str, port: u16) -> Option<&ConnectRule> {
        self.rules
            .iter()
            .find(|r| r.port == port && r.matches_name(host))
    }

    /// 대상을 찾아 잇는다. 차례는 모듈 문서에 있다.
    pub fn open(&self, rule: &ConnectRule) -> Result<Peer, Refusal> {
        let target = &rule.target;
        let found = find(&self.data_dir, target)
            .map_err(|e| (format!("cannot read sandbox {target}: {e}"), None))?
            .ok_or_else(|| (format!("no sandbox {target}"), None))?;
        let id = Some(found.id.clone());
        let refuse = |why: String| (why, id.clone());
        if found.id == self.me {
            return Err(refuse("a sandbox cannot connect to itself".into()));
        }
        in_scope(&self.scope, target, &found.spec).map_err(refuse)?;
        let control = match (found.state.state, &found.state.control) {
            (State::Running, Some(c)) => c.clone(),
            (s, _) => {
                return Err(refuse(format!(
                    "sandbox {target} is not running ({})",
                    s.as_str()
                )));
            }
        };
        if found.state.egress_port == Some(rule.port) {
            return Err(refuse(format!(
                "port {} of sandbox {target} is its outgoing-connection proxy",
                rule.port
            )));
        }
        let guest = Guest::new(
            self.executor.clone(),
            ChannelSpec {
                host_endpoint: control,
                guest_port: sbx_guest_proto::GUEST_PORT,
            },
        );
        match guest.forward(rule.port) {
            Ok(conn) => Ok(Peer { id: found.id, conn }),
            Err(e) => Err(refuse(format!("sandbox {target}: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(name: &str, target: &str, port: u16) -> ConnectRule {
        ConnectRule {
            name: name.into(),
            target: target.into(),
            port,
        }
    }

    #[test]
    fn names_answer_with_their_ports_only() {
        let p = Peers {
            rules: vec![
                rule("db", "db", 5432),
                rule("db", "db", 80),
                rule("cache", "r1", 6379),
            ],
            data_dir: std::env::temp_dir().join("sbx-connect-unused"),
            executor: Arc::new(sbx_executor_fake::FakeExecutor::basic()),
            me: SandboxId::new("me").unwrap(),
            scope: BTreeMap::new(),
        };
        let ports = |n: &str| crate::egress::lookup(&[], &p.rules, n).ports;
        assert_eq!(ports("DB."), vec![80, 5432]);
        assert_eq!(ports("cache"), vec![6379]);
        assert!(ports("other").is_empty());
        assert_eq!(p.target_of("cache"), Some("r1"));
        assert_eq!(p.target_of("other"), None);
        assert_eq!(p.rule("db", 5432).map(|r| r.port), Some(5432));
        assert!(p.rule("db", 6379).is_none());
        assert!(p.rule("cache.example", 6379).is_none());
    }
}
