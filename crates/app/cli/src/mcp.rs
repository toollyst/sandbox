//! `sbx mcp`: 샌드박스를 AI 에이전트에게 [Model Context Protocol](https://modelcontextprotocol.io) 도구로 내보낸다.
//!
//! - 전송: 표준 입출력, 한 줄에 JSON-RPC 2.0 메시지 하나(MCP stdio). 진단(이미지 받기 등)은 표준 오류로 간다.
//! - **정책이 없다.** 도구는 CLI 명령과 같은 코어를 같은 규칙으로 부른다. 인자는 CLI 인자로 바꿔 clap으로
//!   해석하므로 검증, 기본값, 이미지 받기(`--pull=missing`)가 `sbx run`과 같다.
//!   - 네트워크는 CLI처럼 기본으로 없고, 도구 인자 `allow`(`host:port` 목록)로 연다.
//!   - 호스트 폴더 마운트(`-v`, `--mount`)는 내보내지 않는다. 에이전트가 호스트의 어떤 파일을 보게 할지는
//!     서버를 띄우는 쪽이 정할 일이다. 파일은 `write_file`과 `read_file`로 옮긴다.
//!   - 인자는 언제나 `--이름=값`으로 넘기고, `-`로 시작하는 이미지 이름은 거부한다. 그래서 도구 인자가
//!     다른 플래그(예: `-v`)로 읽히지 않는다.
//! - 도구는 MCP로 만든 샌드박스만 본다(라벨 [`OWNER_LABEL`]). CLI로 만든 샌드박스는 호스트 폴더를 붙였을 수
//!   있으므로 `exec`, 파일 도구, `list`, `remove`에 없는 것과 같다. `sbx mcp --all-sandboxes`로 넓힌다.
//! - 도구([`tools`]): `run`(새 샌드박스에서 명령 하나, 끝나면 지운다), `create`(오래 사는 샌드박스, 이 서버가
//!   끝나도 남는다), `exec`, `write_file`, `read_file`, `list`, `remove`.
//! - 시간 제한: `run`은 넘으면 VM을 끄고 지운다. `exec`은 넘으면 그 프로세스에 SIGKILL을 보낸다.
//!   기본 [`DEFAULT_TIMEOUT_SECS`], 최대 [`MAX_TIMEOUT_SECS`].
//! - 출력은 스트림마다 마지막 [`OUTPUT_LIMIT`] 바이트를 담는다(오류는 보통 끝에 있다). 앞을 버렸으면 `truncated`다.
//! - 취소: `notifications/cancelled`를 받으면 그 호출은 시간이 넘은 것처럼 끝낸다(`run`은 VM을 끄고 지우고,
//!   `exec`은 프로세스를 죽인다). 규약대로 답은 보내지 않는다. 표준 입력이 닫히면(클라이언트가 사라짐)
//!   진행 중인 호출을 모두 취소하고 정리한 뒤 끝난다.
//! - 도구 호출은 동시에 처리한다(호출마다 작업 하나).
//! - 요청이 `_meta.progressToken`을 주면 이미지를 받는 동안 `notifications/progress`를 보낸다(`Progress`).

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use base64::Engine as _;
use clap::Parser;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

use sbx_model::ExecSpec;
use sbx_runtime::{IoMode, Output, Process};

use crate::app::{AppResult, Ctx};
use crate::cli::{Cli, Command, RunArgs};

/// 스트림 하나에서 담는 출력의 최대 바이트.
pub const OUTPUT_LIMIT: usize = 256 << 10;
/// `read_file`로 읽는 파일의 최대 바이트.
pub const FILE_LIMIT: u64 = 8 << 20;
/// 시간 제한 기본값(초).
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// 시간 제한 최댓값(초).
pub const MAX_TIMEOUT_SECS: u64 = 3600;
/// `create`에 명령을 주지 않았을 때의 기본 프로세스. 샌드박스가 계속 살아 있게 한다.
pub const KEEP_ALIVE: &[&str] = &["sleep", "infinity"];
/// 답하는 규약 버전: 클라이언트가 이 중 하나를 원하면 그대로, 아니면 첫 번째.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
/// 시간이 넘은 `exec`에 SIGKILL을 보낸 뒤 남은 출력을 기다리는 시간.
const KILL_GRACE: Duration = Duration::from_secs(2);
const SIGKILL: i32 = 9;

type Stdout = Rc<Mutex<tokio::io::Stdout>>;

/// MCP로 만든 샌드박스에 붙이는 라벨과 값. 도구는 이 라벨이 있는 샌드박스만 본다
/// (`sbx mcp --all-sandboxes`로 넓힐 수 있다).
pub const OWNER_LABEL: &str = "io.github.toollyst.sandbox.owner";
/// [`OWNER_LABEL`]의 값.
pub const OWNER_MCP: &str = "mcp";

/// 서버 하나의 설정.
struct Server {
    ctx: Ctx,
    /// `--all-sandboxes`: 같은 data_dir의 다른 샌드박스(CLI로 만든 것)도 다룬다.
    all_sandboxes: bool,
    /// 에이전트가 쓸 수 있는 자원의 상한.
    limits: Limits,
    /// 만드는 중인(아직 목록에 없는) 샌드박스 수. 동시에 들어온 호출이 상한을 함께 넘지 않게.
    reserved: std::cell::Cell<u32>,
}

/// `sbx mcp --max-sandboxes`, `--max-cpus`, `--max-memory`.
#[derive(Debug, Clone, Copy)]
struct Limits {
    sandboxes: u32,
    cpus: u8,
    memory_mib: u32,
}

/// 만드는 동안 쥐는 자리. 놓으면 [`Server::reserved`]에서 뺀다.
struct Seat<'a>(&'a std::cell::Cell<u32>);

impl Drop for Seat<'_> {
    fn drop(&mut self) {
        self.0.set(self.0.get().saturating_sub(1));
    }
}

impl std::ops::Deref for Server {
    type Target = Ctx;
    fn deref(&self) -> &Ctx {
        &self.ctx
    }
}

impl Server {
    fn mine(&self, info: &sbx_runtime::SandboxInfo) -> bool {
        self.all_sandboxes
            || info.spec.labels.get(OWNER_LABEL).map(String::as_str) == Some(OWNER_MCP)
    }

    /// 만들 샌드박스가 상한 안인지 보고 자리를 잡는다. vCPU와 메모리는 호출 하나의 상한, 개수는 MCP로 만든
    /// 샌드박스(지우기 전까지, 만드는 중인 것 포함)의 상한이다.
    async fn admit(&self, spec: &RunArgs) -> AppResult<Seat<'_>> {
        let l = self.limits;
        let cpus: u8 = spec
            .cpus
            .as_deref()
            .map_or(Ok(1), str::parse)
            .map_err(|_| "invalid `cpus`")?;
        if cpus > l.cpus {
            return Err(format!(
                "`cpus` {cpus} is above this server's limit of {} (sbx mcp --max-cpus)",
                l.cpus
            )
            .into());
        }
        let memory = match spec.memory.as_deref() {
            Some(m) => crate::parse::memory_mib(m)?,
            None => crate::behavior::DEFAULT_MEMORY_MIB,
        };
        if memory > l.memory_mib {
            return Err(format!(
                "`memory` {memory} MiB is above this server's limit of {} MiB (sbx mcp --max-memory)",
                l.memory_mib
            )
            .into());
        }
        // 기다리기(목록 읽기) 전에 자리를 잡는다. 호출은 동시에 돌므로, 읽은 뒤에 잡으면 동시에 온 호출들이 모두
        // 같은 빈자리를 보고 통과한다. 실패하면 자리는 놓인다(Drop).
        self.reserved.set(self.reserved.get() + 1);
        let seat = Seat(&self.reserved);
        let rt = self.runtime()?;
        let existing = rt
            .list()
            .await?
            .iter()
            .filter(|s| s.spec.labels.get(OWNER_LABEL).map(String::as_str) == Some(OWNER_MCP))
            .count() as u32;
        // 자기 자리를 뺀, 이미 있거나 만드는 중인 것.
        let taken = existing + self.reserved.get() - 1;
        if taken >= l.sandboxes {
            return Err(format!(
                "this server already has {taken} sandboxes, its limit (sbx mcp --max-sandboxes {}); remove one first",
                l.sandboxes
            )
            .into());
        }
        Ok(seat)
    }

    /// `query`(이름, ID, ID 앞부분)로 찾는다. 이 서버가 다룰 수 없는 샌드박스는 없는 것과 같다.
    async fn find(
        &self,
        rt: &sbx_runtime::Runtime,
        query: &str,
    ) -> AppResult<sbx_runtime::Sandbox> {
        let sandbox = rt.find(query).await?;
        if !self.mine(sandbox.info()) {
            return Err(format!(
                "no sandbox {query:?} created through MCP (sandboxes made with the sbx CLI are not \
                 available here unless the server runs with --all-sandboxes)"
            )
            .into());
        }
        Ok(sandbox)
    }
}

/// 표준 입출력으로 MCP 서버를 돌린다. 표준 입력이 닫히면 끝난다.
///
/// 도구 호출은 한 스레드([`tokio::task::LocalSet`])에서 동시에 돈다. CLI의 코어 호출은 `Send`가 아닌 오류를
/// 쓰므로 여러 스레드로 옮길 수 없다. 오래 걸리는 일(VM 실행, 기다리기)은 어차피 기다림이다.
pub async fn serve(cli: &Cli) -> AppResult<u8> {
    let Command::Mcp {
        all_sandboxes,
        max_sandboxes,
        max_cpus,
        max_memory,
    } = cli.command
    else {
        return Err("internal: not the mcp command".into());
    };
    let ctx = Rc::new(Server {
        ctx: Ctx::new(cli)?,
        all_sandboxes,
        limits: Limits {
            sandboxes: max_sandboxes,
            cpus: max_cpus,
            memory_mib: max_memory,
        },
        reserved: Default::default(),
    });
    tokio::task::LocalSet::new().run_until(serve_on(ctx)).await
}

async fn serve_on(ctx: Rc<Server>) -> AppResult<u8> {
    let out: Stdout = Rc::new(Mutex::new(tokio::io::stdout()));
    let mut input = BufReader::new(tokio::io::stdin());
    let mut calls = Vec::new();
    // 진행 중인 호출의 취소 신호 (요청 id의 JSON → 신호).
    let pending: Rc<std::cell::RefCell<std::collections::HashMap<String, Cancel>>> = Rc::default();
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if input.read_until(b'\n', &mut raw).await? == 0 {
            break;
        }
        // 잘못된 UTF-8이나 JSON은 그 줄만 오류로 답한다. 서버는 계속 돈다.
        let line = match std::str::from_utf8(&raw) {
            Ok(l) => l.trim(),
            Err(e) => {
                send(
                    &out,
                    error(Value::Null, -32700, &format!("parse error: {e}")),
                )
                .await;
                continue;
            }
        };
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                send(
                    &out,
                    error(Value::Null, -32700, &format!("parse error: {e}")),
                )
                .await;
                continue;
            }
        };
        if msg.is_array() {
            send(
                &out,
                error(Value::Null, -32600, "JSON-RPC batches are not supported"),
            )
            .await;
            continue;
        }
        let method = msg.get("method").and_then(Value::as_str);
        // 알림(id 없음)과 응답(method 없음)에는 답하지 않는다. 이 서버는 요청을 보내지 않는다.
        let Some(id) = msg.get("id").cloned() else {
            if method == Some("notifications/cancelled")
                && let Some(req) = msg.pointer("/params/requestId")
                && let Some(c) = pending.borrow().get(&req.to_string())
            {
                c.cancel();
            }
            continue;
        };
        let Some(method) = method else { continue };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "initialize" => send(&out, result(id, initialize(&params))).await,
            "ping" => send(&out, result(id, json!({}))).await,
            "tools/list" => send(&out, result(id, json!({ "tools": tools() }))).await,
            "tools/call" => {
                let (ctx, out, pending) = (ctx.clone(), out.clone(), pending.clone());
                let cancel = Cancel::new();
                let key = id.to_string();
                pending.borrow_mut().insert(key.clone(), cancel.clone());
                calls.push(tokio::task::spawn_local(async move {
                    let progress = Progress::start(&out, &params);
                    let r = call(&ctx, &params, &progress, &cancel).await;
                    progress.finish().await;
                    pending.borrow_mut().remove(&key);
                    if !cancel.is_cancelled() {
                        send(&out, r.into_response(id)).await;
                    }
                }));
                calls.retain(|c| !c.is_finished());
            }
            other => {
                send(
                    &out,
                    error(id, -32601, &format!("method not found: {other}")),
                )
                .await
            }
        }
    }
    // 클라이언트가 사라졌다. 답을 받을 쪽이 없으므로 모두 취소하고 정리를 기다린다.
    for c in pending.borrow().values() {
        c.cancel();
    }
    for c in calls {
        let _ = c.await;
    }
    Ok(0)
}

/// 호출 하나의 취소 신호.
#[derive(Clone)]
struct Cancel(Rc<tokio::sync::watch::Sender<bool>>);

impl Cancel {
    fn new() -> Self {
        Self(Rc::new(tokio::sync::watch::channel(false).0))
    }

    fn cancel(&self) {
        self.0.send_replace(true);
    }

    fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }

    /// 취소될 때 끝난다.
    async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        let _ = rx.wait_for(|c| *c).await;
    }
}

/// 명령이 어떻게 끝났는지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    Done,
    TimedOut,
    Cancelled,
}

/// 끝나거나, 시간이 넘거나, 취소될 때까지 출력을 담는다.
async fn wait_for(p: &mut Process, c: &mut Captured, timeout: Duration, cancel: &Cancel) -> Ended {
    tokio::select! {
        () = capture(p, c) => Ended::Done,
        () = tokio::time::sleep(timeout) => Ended::TimedOut,
        () = cancel.cancelled() => Ended::Cancelled,
    }
}

/// 도구 호출 하나의 진행 알림(`notifications/progress`). 요청이 `_meta.progressToken`을 주었을 때만 보낸다.
///
/// 지금은 이미지 받기만 알린다(`progress`는 받은 바이트, `total`은 받을 바이트). 큰 이미지를 처음 받는 동안
/// 클라이언트가 호출을 끊지 않게 한다. 규약대로 `progress`는 보낼 때마다 커진다.
struct Progress {
    tx: Option<tokio::sync::mpsc::UnboundedSender<Value>>,
    writer: Option<tokio::task::JoinHandle<()>>,
    /// 마지막으로 보낸 때와 값.
    last: std::sync::Mutex<Option<(std::time::Instant, u64)>>,
}

/// 진행 알림의 최소 간격.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

impl Progress {
    fn start(out: &Stdout, params: &Value) -> Self {
        let Some(token) = params
            .pointer("/_meta/progressToken")
            .filter(|t| t.is_string() || t.is_number())
            .cloned()
        else {
            return Self {
                tx: None,
                writer: None,
                last: Default::default(),
            };
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let out = out.clone();
        let writer = tokio::task::spawn_local(async move {
            while let Some(mut p) = rx.recv().await {
                p["progressToken"] = token.clone();
                send(
                    &out,
                    json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": p }),
                )
                .await;
            }
        });
        Self {
            tx: Some(tx),
            writer: Some(writer),
            last: Default::default(),
        }
    }

    fn pull(&self, label: &str, p: sbx_manage::PullProgress) {
        let Some(tx) = &self.tx else { return };
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();
        if !progress_due(*last, now, p) {
            return;
        }
        *last = Some((now, p.bytes_done));
        let _ = tx.send(json!({
            "progress": p.bytes_done,
            "total": p.bytes,
            "message": crate::progress::render_pull(label, p),
        }));
    }

    /// 남은 알림을 다 보낸다. 결과보다 먼저 나가야 한다.
    async fn finish(mut self) {
        self.tx = None;
        if let Some(w) = self.writer.take() {
            let _ = w.await;
        }
    }
}

/// 진행 알림을 지금 보낼지. 처음은 보내고, 그 뒤로는 값이 커졌고 간격이 지났거나 다 받았을 때만.
fn progress_due(
    last: Option<(std::time::Instant, u64)>,
    now: std::time::Instant,
    p: sbx_manage::PullProgress,
) -> bool {
    match last {
        None => true,
        Some((at, sent)) => {
            p.bytes_done > sent
                && (p.bytes_done == p.bytes || now.duration_since(at) >= PROGRESS_EVERY)
        }
    }
}

async fn send(out: &Stdout, msg: Value) {
    let mut line = msg.to_string();
    line.push('\n');
    let mut w = out.lock().await;
    // 클라이언트가 떠났으면 보낼 곳이 없다. 표준 입력도 곧 닫힌다.
    let _ = w.write_all(line.as_bytes()).await;
    let _ = w.flush().await;
}

fn result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == asked)
        .unwrap_or(&PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "sbx", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Each sandbox is a microVM with its own Linux kernel, running an OCI image \
            (Docker Hub names like alpine or python:3.12-slim work). There is no network unless you pass \
            `allow` (host:port entries). Use `run` for one-off commands; use `create`, then `exec`, \
            `write_file` and `read_file` for a longer session, and `remove` when done.",
    })
}

/// 도구 목록(이름, 설명, 입력 JSON Schema).
pub fn tools() -> Value {
    let image = json!({ "type": "string", "description": "OCI image, e.g. alpine, python:3.12-slim, ghcr.io/org/app:tag. Pulled on first use." });
    let command = json!({ "type": "array", "items": { "type": "string" }, "description": "argv, e.g. [\"python\", \"-c\", \"print(1)\"]. Use [\"sh\", \"-c\", \"...\"] for shell syntax." });
    let env = json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": "Environment variables." });
    let workdir =
        json!({ "type": "string", "description": "Working directory inside the sandbox." });
    let allow = json!({ "type": "array", "items": { "type": "string" }, "description": "Outbound network allow list, host:port each (e.g. pypi.org:443, *.github.com:443). Without it the sandbox has no network." });
    let stdin =
        json!({ "type": "string", "description": "Text written to the command's standard input." });
    let timeout = json!({ "type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_SECS, "description": format!("Seconds before the command is killed (default {DEFAULT_TIMEOUT_SECS}).") });
    let cpus = json!({ "type": "integer", "minimum": 1, "maximum": 255, "description": "vCPUs (default 1)." });
    let memory =
        json!({ "type": "string", "description": "Memory, e.g. 512m or 2g (default 512m)." });
    let id = json!({ "type": "string", "description": "Sandbox ID (or a unique prefix), or the name given to create." });
    let name = json!({ "type": "string", "description": "Optional unique name (letters, digits, _ . -) to use in place of the ID." });
    let path = json!({ "type": "string", "description": "Absolute path inside the sandbox." });
    let encoding = json!({ "type": "string", "enum": ["utf-8", "base64"], "description": "Encoding of content (default utf-8)." });
    let output = json!({ "type": "object", "properties": {
        "exit_code": { "type": ["integer", "null"] }, "stdout": { "type": "string" }, "stderr": { "type": "string" },
        "timed_out": { "type": "boolean" }, "truncated": { "type": "boolean" } } });
    json!([
        {
            "name": "run",
            "title": "Run a command in a new sandbox",
            "description": "Start a fresh microVM from an image, run one command, return its exit code and output, then delete the sandbox.",
            "inputSchema": { "type": "object", "required": ["image"], "properties": {
                "image": image, "command": command, "env": env, "workdir": workdir, "allow": allow,
                "stdin": stdin, "timeout_secs": timeout, "cpus": cpus, "memory": memory } },
            "outputSchema": output,
        },
        {
            "name": "create",
            "title": "Create a long-lived sandbox",
            "description": "Start a microVM that keeps running (by default its main process is `sleep infinity`) for exec, write_file and read_file. It stays until remove, even after this server exits.",
            "inputSchema": { "type": "object", "required": ["image"], "properties": {
                "image": image, "name": name, "command": command, "env": env, "workdir": workdir, "allow": allow,
                "cpus": cpus, "memory": memory } },
        },
        {
            "name": "exec",
            "title": "Run a command in a sandbox",
            "description": "Run a command in a running sandbox and return its exit code and output. The call returns when the command's output is closed, so to leave a server running in the background, redirect its output, e.g. [\"sh\", \"-c\", \"server >/tmp/server.log 2>&1 &\"].",
            "inputSchema": { "type": "object", "required": ["id", "command"], "properties": {
                "id": id, "command": command, "env": env, "workdir": workdir, "stdin": stdin, "timeout_secs": timeout } },
            "outputSchema": output,
        },
        {
            "name": "write_file",
            "title": "Write a file into a sandbox",
            "description": "Create or replace a file in a running sandbox. Missing parent directories are created.",
            "inputSchema": { "type": "object", "required": ["id", "path", "content"], "properties": {
                "id": id, "path": path, "content": { "type": "string" }, "encoding": encoding } },
        },
        {
            "name": "read_file",
            "title": "Read a file from a sandbox",
            "description": format!("Read a file (up to {} MiB) from a running sandbox. Text comes back as utf-8, anything else as base64.", FILE_LIMIT >> 20),
            "inputSchema": { "type": "object", "required": ["id", "path"], "properties": { "id": id, "path": path } },
        },
        {
            "name": "list",
            "title": "List sandboxes",
            "description": "List all sandboxes with their name, image, command and state.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "remove",
            "title": "Remove a sandbox",
            "description": "Stop a sandbox immediately if it is running, and delete it.",
            "inputSchema": { "type": "object", "required": ["id"], "properties": { "id": id } },
        },
    ])
}

/// 도구 호출의 결과.
enum Called {
    /// 도구가 돌았다(명령의 0 아닌 종료 코드도 여기다). 실패면 `isError`.
    Tool(AppResult<Value>),
    /// 없는 도구. JSON-RPC 오류 -32602로 답한다(MCP 규약).
    Unknown(String),
}

impl Called {
    fn into_response(self, id: Value) -> Value {
        match self {
            Called::Unknown(name) => error(id, -32602, &format!("unknown tool {name:?}")),
            Called::Tool(Ok(v)) => result(
                id,
                json!({
                    "content": [{ "type": "text", "text": text(&v) }],
                    "structuredContent": v,
                    "isError": false,
                }),
            ),
            Called::Tool(Err(e)) => result(
                id,
                json!({ "content": [{ "type": "text", "text": e.to_string() }], "isError": true }),
            ),
        }
    }
}

/// 도구 하나를 부른다.
async fn call(ctx: &Server, params: &Value, progress: &Progress, cancel: &Cancel) -> Called {
    let args = Args(params.get("arguments").unwrap_or(&Value::Null));
    Called::Tool(match params["name"].as_str().unwrap_or_default() {
        "run" => run(ctx, &args, progress, cancel).await,
        "create" => create(ctx, &args, progress).await,
        "exec" => exec(ctx, &args, cancel).await,
        "write_file" => write_file(ctx, &args).await,
        "read_file" => read_file(ctx, &args).await,
        "list" => list(ctx).await,
        "remove" => remove(ctx, &args).await,
        other => return Called::Unknown(other.to_owned()),
    })
}

/// 사람(과 모델)이 읽는 결과. 명령 결과는 종료 코드와 두 출력을 나눠 쓰고, 나머지는 JSON.
fn text(v: &Value) -> String {
    if v.get("stdout").is_none() {
        return serde_json::to_string_pretty(v).unwrap_or_default();
    }
    let mut s = match (v["timed_out"].as_bool(), v["exit_code"].as_i64()) {
        (Some(true), _) => "timed out (killed)".to_owned(),
        (_, Some(c)) => format!("exit code {c}"),
        _ => "exit code unknown".to_owned(),
    };
    if v["truncated"].as_bool() == Some(true) {
        s.push_str(&format!(
            " (only the last {OUTPUT_LIMIT} bytes of each stream are kept)"
        ));
    }
    for k in ["stdout", "stderr"] {
        let o = v[k].as_str().unwrap_or_default();
        if !o.is_empty() {
            s.push_str(&format!("\n--- {k} ---\n{o}"));
        }
    }
    s
}

/// 도구 인자. 형식이 틀리면 무엇이 틀렸는지 알린다.
struct Args<'a>(&'a Value);

impl Args<'_> {
    fn get(&self, k: &str) -> Option<&Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }

    fn str(&self, k: &str) -> AppResult<Option<String>> {
        match self.get(k) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(format!("`{k}` must be a string").into()),
        }
    }

    fn required(&self, k: &str) -> AppResult<String> {
        self.str(k)?
            .ok_or_else(|| format!("`{k}` is required").into())
    }

    fn strings(&self, k: &str) -> AppResult<Vec<String>> {
        match self.get(k) {
            None => Ok(Vec::new()),
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| format!("`{k}` must be a list of strings").into())
                })
                .collect(),
            Some(_) => Err(format!("`{k}` must be a list of strings").into()),
        }
    }

    fn env(&self) -> AppResult<Vec<(String, String)>> {
        match self.get("env") {
            None => Ok(Vec::new()),
            Some(Value::Object(m)) => m
                .iter()
                .map(|(k, v)| {
                    let v = v.as_str().ok_or("`env` values must be strings")?;
                    if k.is_empty() || k.contains('=') {
                        return Err(format!("invalid environment variable name {k:?}").into());
                    }
                    Ok((k.clone(), v.to_owned()))
                })
                .collect(),
            Some(_) => Err("`env` must be an object of strings".into()),
        }
    }

    fn timeout(&self) -> AppResult<Duration> {
        let secs = match self.get("timeout_secs") {
            None => DEFAULT_TIMEOUT_SECS,
            Some(v) => v
                .as_u64()
                .filter(|s| (1..=MAX_TIMEOUT_SECS).contains(s))
                .ok_or(format!("`timeout_secs` must be 1..={MAX_TIMEOUT_SECS}"))?,
        };
        Ok(Duration::from_secs(secs))
    }
}

/// 도구 인자를 `sbx create`의 인자로 바꿔 CLI와 같은 규칙으로 해석한다.
fn run_args(a: &Args, stdin: bool, default_command: &[&str]) -> AppResult<RunArgs> {
    let image = a.required("image")?;
    if image.starts_with('-') || image.trim().is_empty() {
        return Err(format!("invalid image {image:?}").into());
    }
    let mut argv: Vec<String> = vec![
        "sbx".into(),
        "create".into(),
        format!("--label={OWNER_LABEL}={OWNER_MCP}"),
    ];
    let allow = a.strings("allow")?;
    if !allow.is_empty() {
        argv.push("--network=allow".into());
        argv.extend(allow.iter().map(|r| format!("--allow={r}")));
    }
    argv.extend(a.env()?.iter().map(|(k, v)| format!("--env={k}={v}")));
    if let Some(w) = a.str("workdir")? {
        argv.push(format!("--workdir={w}"));
    }
    match a.get("cpus") {
        None => {}
        Some(v) => match v.as_f64() {
            Some(n) if n >= 1.0 && n.fract() == 0.0 && n <= f64::from(u8::MAX) => {
                argv.push(format!("--cpus={}", n as u8))
            }
            _ => {
                return Err(format!("`cpus` must be a whole number from 1 to 255, not {v}").into());
            }
        },
    }
    if let Some(m) = a.str("memory")? {
        argv.push(format!("--memory={m}"));
    }
    if let Some(n) = a.str("name")? {
        argv.push(format!("--name={n}"));
    }
    if stdin {
        argv.push("--interactive".into());
    }
    argv.push(image);
    let command = a.strings("command")?;
    if command.is_empty() {
        argv.extend(default_command.iter().map(|s| s.to_string()));
    } else {
        argv.extend(command);
    }
    match Cli::try_parse_from(&argv)
        .map_err(|e| arg_error(&e.render().to_string()))?
        .command
    {
        Command::Create(r) => Ok(r),
        _ => Err("internal: not a create command".into()),
    }
}

/// clap 오류에서 모델이 고칠 수 있는 첫 줄만 남긴다(`Usage:`와 CLI 도움말은 도구 인자와 맞지 않는다).
/// `'--memory <MEMORY>'` 같은 플래그는 도구 인자 이름(`memory`)으로 바꾼다.
fn arg_error(rendered: &str) -> String {
    let line = rendered
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(rendered)
        .trim();
    let line = line.strip_prefix("error: ").unwrap_or(line);
    let mut out = String::new();
    let mut rest = line;
    while let Some(i) = rest.find("'--") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 3..];
        let name_end = after.find(['\'', ' ', '=']).unwrap_or(after.len());
        out.push('`');
        out.push_str(&after[..name_end].replace('-', "_"));
        out.push('`');
        // 닫는 따옴표까지 건너뛴다(`<MEMORY>` 같은 자리 표시).
        let close = after.find('\'').map_or(after.len(), |j| j + 1);
        rest = &after[close..];
    }
    out.push_str(rest);
    out
}

#[derive(Default)]
struct Captured {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

/// 끝날 때까지 출력을 담는다. 시간 제한으로 중간에 멈춰도 그때까지 담은 것은 남는다.
async fn capture(p: &mut Process, c: &mut Captured) {
    while let Some(o) = p.next().await {
        let (buf, data) = match o {
            Output::Stdout(d) => (&mut c.stdout, d),
            Output::Stderr(d) => (&mut c.stderr, d),
        };
        buf.extend_from_slice(&data);
        if buf.len() > OUTPUT_LIMIT {
            let drop = buf.len() - OUTPUT_LIMIT;
            buf.drain(..drop);
            c.truncated = true;
        }
    }
}

fn output(exit_code: Option<i32>, c: Captured, timed_out: bool) -> Value {
    json!({
        "exit_code": exit_code,
        "stdout": String::from_utf8_lossy(&c.stdout),
        "stderr": String::from_utf8_lossy(&c.stderr),
        "timed_out": timed_out,
        "truncated": c.truncated,
    })
}

fn feed(p: &Process, stdin: Option<&str>) {
    if let Some(s) = stdin {
        p.write_stdin(s.as_bytes());
        p.close_stdin();
    }
}

async fn run(ctx: &Server, a: &Args<'_>, progress: &Progress, cancel: &Cancel) -> AppResult<Value> {
    let stdin = a.str("stdin")?;
    let timeout = a.timeout()?;
    let spec = run_args(a, stdin.is_some(), &[])?;
    let seat = ctx.admit(&spec).await?;
    let report = |p| progress.pull(spec.image(), p);
    let id = tokio::select! {
        id = ctx.create_with(&spec, &report) => id?,
        () = cancel.cancelled() => return Err("cancelled".into()),
    };
    drop(seat);
    let rt = ctx.launcher_for(&id).await?;
    let r: AppResult<Value> = async {
        // `?`가 만드는 값(Send가 아닌 오류를 담는다)이 다음 await까지 남지 않게 문장을 나눈다.
        let sandbox = rt.get(&id).await?;
        let mut p = sandbox
            .start(IoMode {
                stdin: stdin.is_some(),
                tty: None,
            })
            .await?;
        feed(&p, stdin.as_deref());
        let mut c = Captured::default();
        let done = wait_for(&mut p, &mut c, timeout, cancel).await == Ended::Done;
        if !done {
            let sandbox = rt.get(&id).await?;
            sandbox.stop(None, Duration::ZERO).await?;
        }
        Ok(output(p.exit_code().filter(|_| done), c, !done))
    }
    .await;
    let r = r.map_err(|e| e.to_string());
    // 끝나면(실패해도) 지운다. `sbx run --rm`과 같다.
    match rt.get(&id).await {
        Ok(s) => {
            if let Err(e) = s.delete().await {
                eprintln!("sbx mcp: {id}: cannot remove: {e}");
            }
        }
        Err(e) => eprintln!("sbx mcp: {id}: cannot remove: {e}"),
    }
    Ok(r?)
}

async fn create(ctx: &Server, a: &Args<'_>, progress: &Progress) -> AppResult<Value> {
    let spec = run_args(a, false, KEEP_ALIVE)?;
    let seat = ctx.admit(&spec).await?;
    let id = ctx
        .create_with(&spec, &|p| progress.pull(spec.image(), p))
        .await?;
    drop(seat);
    // 오류(`Box<dyn Error>`)는 Send가 아니라 정리를 기다리는 동안 쥐고 있을 수 없다. 글로 바꿔 둔다.
    let failed = ctx
        .start_detached(&id, false)
        .await
        .err()
        .map(|e| e.to_string());
    if let Some(e) = failed {
        let rt = ctx.runtime()?;
        if let Ok(s) = rt.get(&id).await {
            let _ = s.delete().await;
        }
        return Err(e.into());
    }
    Ok(json!({ "id": id }))
}

async fn exec(ctx: &Server, a: &Args<'_>, cancel: &Cancel) -> AppResult<Value> {
    let id = a.required("id")?;
    let command = a.strings("command")?;
    if command.is_empty() {
        return Err("`command` must not be empty".into());
    }
    let stdin = a.str("stdin")?;
    let timeout = a.timeout()?;
    let spec = ExecSpec {
        args: command,
        env: a.env()?,
        cwd: a.str("workdir")?,
        user: None,
        tty: false,
    };
    let rt = ctx.runtime()?;
    let sandbox = ctx.find(&rt, &id).await?;
    let mut p = sandbox
        .exec_with(
            spec,
            IoMode {
                stdin: stdin.is_some(),
                tty: None,
            },
        )
        .await?;
    feed(&p, stdin.as_deref());
    let mut c = Captured::default();
    let done = wait_for(&mut p, &mut c, timeout, cancel).await == Ended::Done;
    if !done {
        p.signal(SIGKILL);
        let _ = tokio::time::timeout(KILL_GRACE, capture(&mut p, &mut c)).await;
    }
    Ok(output(p.exit_code().filter(|_| done), c, !done))
}

/// 호스트 쪽 임시 디렉터리. 지우는 것은 부른 쪽이다.
///
/// 새로 만든 것만 쓴다(`create_dir`, 이미 있으면 다른 이름). 공용 `/tmp`에서 다른 사용자가 미리 만들어 둔
/// 디렉터리나 링크를 받아들이지 않는다. Unix에서는 0700이다.
fn scratch() -> AppResult<PathBuf> {
    static N: AtomicU64 = AtomicU64::new(0);
    let salt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    for _ in 0..100 {
        let dir = std::env::temp_dir().join(format!(
            "sbx-mcp-{}-{salt:08x}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let b = {
            #[allow(unused_mut)]
            let mut b = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
            b
        };
        match b.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err("could not create a temporary directory".into())
}

async fn write_file(ctx: &Server, a: &Args<'_>) -> AppResult<Value> {
    let id = a.required("id")?;
    let path = a.required("path")?;
    let content = a.required("content")?;
    let bytes = match a.str("encoding")?.as_deref() {
        None | Some("utf-8") => content.into_bytes(),
        Some("base64") => base64::engine::general_purpose::STANDARD
            .decode(content)
            .map_err(|e| format!("`content` is not valid base64: {e}"))?,
        Some(other) => return Err(format!("unknown encoding {other:?}").into()),
    };
    let name = std::path::Path::new(&path)
        .file_name()
        .ok_or_else(|| format!("{path:?} does not name a file"))?
        .to_owned();
    let dir = scratch()?;
    let host = dir.join(name);
    let r: AppResult<()> = async {
        std::fs::write(&host, &bytes)?;
        let rt = ctx.runtime()?;
        let sandbox = ctx.find(&rt, &id).await?;
        let first = sandbox.copy_in(&host, &path).await;
        let Err(e) = first else { return Ok(()) };
        // 부모 디렉터리가 없을 수 있다. 에이전트는 보통 만들어 주기를 기대하므로 `mkdir -p`로 만들고 한 번 더
        // 해 본다(이미지에 mkdir이 있어야 한다). 그래도 안 되면 처음 오류를 알린다.
        let first = e.to_string();
        let parent = match std::path::Path::new(&path).parent() {
            Some(p) if p.as_os_str().len() > 1 => p.to_string_lossy().into_owned(),
            _ => return Err(first.into()),
        };
        let mkdir = ExecSpec {
            args: vec!["mkdir".into(), "-p".into(), parent],
            env: Vec::new(),
            cwd: None,
            user: None,
            tty: false,
        };
        let made = match sandbox.exec_with(mkdir, IoMode::default()).await {
            Ok(p) => p.wait().await.is_ok_and(|c| c == 0),
            Err(_) => false,
        };
        if !made {
            return Err(first.into());
        }
        sandbox.copy_in(&host, &path).await?;
        Ok(())
    }
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    r?;
    Ok(json!({ "path": path, "bytes": bytes.len() }))
}

async fn read_file(ctx: &Server, a: &Args<'_>) -> AppResult<Value> {
    let id = a.required("id")?;
    let path = a.required("path")?;
    let rt = ctx.runtime()?;
    let sandbox = ctx.find(&rt, &id).await?;
    // 호스트에 꺼내지 않고 메모리로 받는다. 게스트가 만든 링크가 호스트 파일을 가리켜도 따라가지 않는다.
    let bytes = sandbox.read_file(&path, FILE_LIMIT).await?;
    Ok(match String::from_utf8(bytes) {
        Ok(s) => json!({ "path": path, "encoding": "utf-8", "content": s }),
        Err(e) => json!({
            "path": path,
            "encoding": "base64",
            "content": base64::engine::general_purpose::STANDARD.encode(e.as_bytes()),
        }),
    })
}

async fn list(ctx: &Server) -> AppResult<Value> {
    let rt = ctx.runtime()?;
    let sandboxes = rt.list().await?;
    let all: Vec<Value> = sandboxes
        .iter()
        .filter(|s| ctx.mine(s))
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.spec.name,
                "image": ctx.image_name(&s.spec.image),
                "command": s.spec.process.args,
                "state": s.state,
                "exit_code": s.exit_code,
                "error": s.error,
            })
        })
        .collect();
    Ok(json!({ "sandboxes": all }))
}

async fn remove(ctx: &Server, a: &Args<'_>) -> AppResult<Value> {
    let wanted = a.required("id")?;
    let rt = ctx.runtime()?;
    let sandbox = ctx.find(&rt, &wanted).await?;
    let id = sandbox.id().clone();
    if sandbox.state() == sbx_model::State::Running {
        sandbox.stop(None, Duration::ZERO).await?;
        let stopped = rt.get(&id).await?;
        stopped.delete().await?;
    } else {
        sandbox.delete().await?;
    }
    Ok(json!({ "removed": id }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: Value, stdin: bool) -> AppResult<RunArgs> {
        run_args(&Args(&v), stdin, KEEP_ALIVE)
    }

    #[test]
    fn progress_is_sent_when_it_grows_and_is_due() {
        let p = |done| sbx_manage::PullProgress {
            layers: 1,
            layers_done: 0,
            bytes: 100,
            bytes_done: done,
        };
        let t0 = std::time::Instant::now();
        let soon = t0 + Duration::from_millis(10);
        let later = t0 + PROGRESS_EVERY;
        assert!(progress_due(None, t0, p(0)));
        assert!(!progress_due(Some((t0, 0)), soon, p(50)), "too soon");
        assert!(progress_due(Some((t0, 0)), later, p(50)));
        assert!(
            progress_due(Some((t0, 50)), soon, p(100)),
            "done is sent at once"
        );
        assert!(!progress_due(Some((t0, 100)), later, p(100)), "must grow");
    }

    #[test]
    fn tool_arguments_become_the_same_create_arguments() {
        let r = parse(
            json!({
                "image": "python:3.12-alpine",
                "command": ["python", "-c", "print(1)"],
                "env": { "A": "1=2" },
                "workdir": "/w",
                "allow": ["pypi.org:443"],
                "cpus": 2,
                "memory": "1g",
            }),
            true,
        )
        .unwrap();
        assert_eq!(r.image(), "python:3.12-alpine");
        assert_eq!(r.command(), ["python", "-c", "print(1)"]);
        assert_eq!(r.env, ["A=1=2"]);
        assert_eq!(r.workdir.as_deref(), Some("/w"));
        assert_eq!(
            (r.network.as_str(), r.allow.as_slice()),
            ("allow", &["pypi.org:443".to_owned()][..])
        );
        assert_eq!(
            (r.cpus.as_deref(), r.memory.as_deref()),
            (Some("2"), Some("1g"))
        );
        assert!(r.interactive && !r.rm && r.volumes.is_empty() && r.mounts.is_empty());
        assert_eq!(
            parse(json!({ "image": "alpine", "name": "w1" }), false)
                .unwrap()
                .name
                .as_deref(),
            Some("w1")
        );
        // 명령이 없으면 기본 프로세스.
        assert_eq!(
            parse(json!({ "image": "alpine" }), false)
                .unwrap()
                .command(),
            KEEP_ALIVE
        );
        // 기본은 네트워크 없음.
        assert_eq!(
            parse(json!({ "image": "alpine" }), false).unwrap().network,
            "none"
        );
    }

    #[test]
    fn arguments_cannot_smuggle_flags() {
        // 이미지 자리에 플래그를 넣을 수 없다.
        assert!(parse(json!({ "image": "-v" }), false).is_err());
        assert!(parse(json!({ "image": "--volume=/:/host" }), false).is_err());
        // 값은 `--이름=값`으로 넘기므로 플래그처럼 생긴 값도 값일 뿐이다.
        let r = parse(
            json!({ "image": "alpine", "workdir": "-v", "env": { "X": "--mount=type=bind" } }),
            false,
        )
        .unwrap();
        assert_eq!(r.workdir.as_deref(), Some("-v"));
        assert!(r.volumes.is_empty() && r.mounts.is_empty());
        // 명령은 이미지 뒤라 모두 명령이다.
        let r = parse(
            json!({ "image": "alpine", "command": ["-v", "/:/x"] }),
            false,
        )
        .unwrap();
        assert_eq!(r.command(), ["-v", "/:/x"]);
        assert!(r.volumes.is_empty());
        assert!(parse(json!({ "image": "alpine", "env": { "A=B": "x" } }), false).is_err());
        assert!(parse(json!({ "image": 3 }), false).is_err());
        assert!(parse(json!({ "image": "alpine", "allow": "x" }), false).is_err());
    }

    #[test]
    fn cpus_must_be_a_whole_number() {
        let cpus = |v: Value| parse(json!({ "image": "alpine", "cpus": v }), false).map(|r| r.cpus);
        assert_eq!(cpus(json!(2.0)).unwrap().as_deref(), Some("2"));
        assert_eq!(cpus(json!(3)).unwrap().as_deref(), Some("3"));
        for bad in [json!(1.5), json!(0), json!(-1), json!(256), json!("2")] {
            assert!(cpus(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn argument_errors_name_tool_arguments_not_cli_flags() {
        let e = arg_error(
            "error: invalid value 'lots' for '--memory <MEMORY>': not a size\n\nUsage: sbx create [OPTIONS]\n",
        );
        assert_eq!(e, "invalid value 'lots' for `memory`: not a size");
        assert_eq!(
            arg_error("error: invalid value 'x' for '--stop-timeout <SECS>': bad\n\nUsage: sbx"),
            "invalid value 'x' for `stop_timeout`: bad"
        );
    }

    #[test]
    fn timeouts_are_bounded() {
        let t = |v: Value| Args(&v).timeout().map(|d| d.as_secs());
        assert_eq!(t(json!({})).unwrap(), DEFAULT_TIMEOUT_SECS);
        assert_eq!(t(json!({ "timeout_secs": 5 })).unwrap(), 5);
        assert!(t(json!({ "timeout_secs": 0 })).is_err());
        assert!(t(json!({ "timeout_secs": MAX_TIMEOUT_SECS + 1 })).is_err());
    }

    #[test]
    fn versions_and_tools_are_well_formed() {
        assert_eq!(
            initialize(&json!({ "protocolVersion": "2025-03-26" }))["protocolVersion"],
            "2025-03-26"
        );
        assert_eq!(
            initialize(&json!({ "protocolVersion": "1999-01-01" }))["protocolVersion"],
            PROTOCOL_VERSIONS[0]
        );
        let tools = tools();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "run",
                "create",
                "exec",
                "write_file",
                "read_file",
                "list",
                "remove"
            ]
        );
        for t in tools.as_array().unwrap() {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
        }
    }

    #[test]
    fn command_results_read_well() {
        let c = Captured {
            stdout: b"hi\n".to_vec(),
            stderr: Vec::new(),
            truncated: false,
        };
        assert_eq!(
            text(&output(Some(3), c, false)),
            "exit code 3\n--- stdout ---\nhi\n"
        );
        assert!(text(&output(None, Captured::default(), true)).starts_with("timed out"));
    }
}
