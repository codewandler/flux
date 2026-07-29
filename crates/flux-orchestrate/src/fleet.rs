//! Outbound A2A dispatch — delegating work to a **remote** flux agent (A-116).
//!
//! [`LocalSpawner`](crate::LocalSpawner) runs a sub-agent in-process. A fleet worker is instead a
//! separate flux agent reached over A2A, which is what lets the fleet survive a coordinator restart
//! and lets each worker own its own repo checkout. Two halves live here, because they answer
//! different questions:
//!
//! - [`A2aSpawner`] is the **blocking delegate** case: it implements [`Spawner`], so the existing
//!   `task` op drives it verbatim — zero new op surface, and every depth / cap-scope bound the
//!   `task` op already applies still applies.
//! - [`FleetDispatchTool`] / [`FleetStatusTool`] / [`FleetCancelTool`] are the fire-and-**track**
//!   case, which [`Spawner`]'s fire-and-await signature cannot express: `fleet.dispatch` returns a
//!   `task_id` immediately, and `fleet.status` / `fleet.cancel` act on it later.
//!
//! **Workers must be served by `flux serve` / flux-server.** The stateful task surface
//! (`message/send` non-blocking, `tasks/get`, `tasks/cancel`) lives in `flux-server`;
//! `flux_a2a::server::is_unsupported_a2a_method` still classifies those methods as unsupported in
//! the reduced *embeddable* dispatch, so an embedded A2A agent cannot back a fleet worker.
//!
//! ## Egress
//!
//! The worker endpoint on the `fleet.*` ops is caller-supplied, so every one of them resolves it
//! through [`flux_system::net::guard_url_scoped`] before a request is made — an unguarded,
//! model-named URL is an SSRF hole. `permission_subjects` reports the worker's **origin**, never
//! `*`, so a `network.fetch` grant scopes to the exact worker.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use flux_a2a::{A2aClient, A2aError, Message, Part, SendOutcome, Task};
use flux_core::{Error, Result};
use flux_runtime::{SpawnOutcome, SpawnRequest, Spawner, Tool, ToolContext, ToolResult};
use flux_spec::{tool_input_schema, AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::net::{guard_url_scoped, PrivateNetAllow};

use crate::parse_params;

/// How often [`A2aSpawner`] polls a worker that answered a blocking send with a running task.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// How many polls [`A2aSpawner`] will make before giving up on a worker that never terminates.
const DEFAULT_MAX_POLLS: usize = 7200; // 1h at the default interval

/// Map an A2A transport/protocol failure onto the crate's error style.
fn a2a_err(context: &str, e: A2aError) -> Error {
    Error::Other(format!("{context}: {e}"))
}

/// The worker's guarded **origin** (`scheme://host[:port]`) — the identity a `network.fetch` grant
/// is scoped to, and what the ops report as their permission subject. `None` for an endpoint that
/// does not parse, which deliberately yields *no* subjects: an op that cannot name its target must
/// be forced to approval rather than matching a broad grant.
///
/// Parsed by hand rather than through the `url` crate: `permission_subjects` runs synchronously on
/// the gating path for every invocation, so it must not depend on DNS or pull a dependency into
/// this crate. The authoritative parse still happens on the execution path, inside
/// [`guard_url_scoped`] — this only has to *name* the target, and it names strictly less than the
/// full URL (no path, no query, no userinfo).
fn worker_origin(endpoint: &str) -> Option<String> {
    let (scheme, rest) = endpoint.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    // The authority ends at the first path/query/fragment delimiter.
    let authority = rest.split(['/', '?', '#']).next()?;
    // Userinfo is a credential, never part of the grant subject — drop it at the LAST `@` so a
    // host that embeds one cannot smuggle a different origin past the subject.
    let host_port = authority
        .rsplit_once('@')
        .map(|(_, hp)| hp)
        .unwrap_or(authority);
    if host_port.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{}", host_port.to_ascii_lowercase()))
}

/// Build the one user message that carries a delegation to a worker. The task text is the message
/// body; the role rides as an A2A `data` part, which `flux_a2a::server::extract_input` renders into
/// the worker's turn input alongside the text (A-51) rather than dropping it.
fn delegation_message(role: &str, task: &str, context_id: Option<String>) -> Message {
    Message::user(
        vec![
            Part::data(serde_json::json!({ "role": role })),
            Part::text(task),
        ],
        context_id,
    )
}

/// Turn a finished remote [`Task`] into the [`SpawnOutcome`] contract.
///
/// `model` names the transport and worker rather than a model id: what model the worker ran is its
/// own business and is not reported over A2A. `usage` is `None` for the same reason — the worker
/// bills its own provider, and inventing a zero would understate rather than omit. `session_id`
/// carries the **remote task id**, which is the real correlation handle for a follow-up
/// `tasks/get`. `tool_calls` is `0` because a worker's tool activity is not observable over A2A.
fn outcome_from_task(endpoint: &str, task: &Task) -> SpawnOutcome {
    SpawnOutcome {
        text: task.final_text(),
        model: format!("a2a:{endpoint}"),
        usage: None,
        session_id: task.id.clone(),
        tool_calls: 0,
    }
}

// ── A2aSpawner ──────────────────────────────────────────────────────────────

/// A [`Spawner`] that delegates to a remote flux agent over A2A instead of running a sub-agent
/// in-process. Drop-in for [`LocalSpawner`](crate::LocalSpawner): the `task` op reaches it through
/// the same `ToolContext::spawner` seam and needs no change.
///
/// ## Cancellation
///
/// `spawn` maps onto `message/send` with `blocking = true` and races it against the caller's
/// [`CancellationToken`]. Once the worker's task id is known, cancelling the token fires
/// [`A2aClient::cancel_task`], so the **remote** run stops rather than this client merely detaching
/// from it.
///
/// One window is inherently narrower than that: the task id is minted by the worker, so a
/// cancellation that lands before the send returns can only drop the in-flight request. A worker
/// that answers a blocking send only when the run is already finished has nothing left to cancel in
/// that window anyway; a worker that answers early with a running task — flux-server's own
/// behaviour under load — hands the id over and enters the cancellable poll below.
pub struct A2aSpawner {
    /// The worker endpoint as configured. Also the `a2a:<endpoint>` stamp on [`SpawnOutcome`].
    endpoint: String,
    client: A2aClient,
    poll_interval: Duration,
    max_polls: usize,
}

impl A2aSpawner {
    /// Bind a spawner to one worker endpoint, with an optional bearer token for a worker behind
    /// `flux serve`'s required auth. The endpoint is operator-configured (not model-supplied), but
    /// it is still guarded here so a misconfigured coordinator cannot be pointed at a
    /// private-network address without the grant.
    pub fn new(
        endpoint: &str,
        private_net: &PrivateNetAllow,
        token: Option<String>,
    ) -> Result<Self> {
        guard_url_scoped(endpoint, private_net)?;
        let client = A2aClient::new(endpoint)
            .map_err(|e| Error::Config(format!("a2a worker: {e}")))?
            .with_token(token);
        Ok(Self {
            endpoint: endpoint.to_string(),
            client,
            poll_interval: DEFAULT_POLL_INTERVAL,
            max_polls: DEFAULT_MAX_POLLS,
        })
    }

    /// Override the completion-poll cadence used when a worker answers a blocking send with a task
    /// that is still running.
    pub fn with_poll(mut self, interval: Duration, max_polls: usize) -> Self {
        self.poll_interval = interval;
        self.max_polls = max_polls;
        self
    }
}

#[async_trait]
impl Spawner for A2aSpawner {
    async fn spawn(
        &self,
        request: SpawnRequest,
        cancel: &CancellationToken,
    ) -> Result<SpawnOutcome> {
        let role = request.role.clone();
        let message = delegation_message(&role, &request.task, request.parent_session.clone());

        // Race the blocking send against the token. `biased` checks cancellation first, so a token
        // already fired at entry never dispatches remote work at all.
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return Err(Error::Other(format!(
                    "remote worker '{role}' was cancelled before dispatch"
                )));
            }
            sent = self.client.send(message, true) => {
                sent.map_err(|e| a2a_err(&format!("dispatch to remote worker '{role}'"), e))?
            }
        };

        let task = match outcome {
            // A worker that answers with a bare message ran the whole turn synchronously and has
            // no task to track or cancel.
            SendOutcome::Message(m) => {
                return Ok(SpawnOutcome {
                    text: m.text(),
                    model: format!("a2a:{}", self.endpoint),
                    usage: None,
                    session_id: m.message_id,
                    tool_calls: 0,
                })
            }
            SendOutcome::Task(t) => t,
        };
        if task.status.state.is_terminal() {
            return Ok(outcome_from_task(&self.endpoint, &task));
        }

        // Still running: the id is now known, so cancellation can reach the WORKER.
        let awaited = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                // Best-effort: a worker that finished in the meantime answers `TaskNotCancelable`,
                // which is a benign outcome for an opportunistic cancel, not a failure to report
                // over the cancellation the caller actually asked for.
                if let Err(e) = self.client.cancel_task(&task.id).await {
                    return Err(Error::Other(format!(
                        "remote worker '{role}' was cancelled; task {} may still be running \
                         (tasks/cancel failed: {e})",
                        task.id
                    )));
                }
                return Err(Error::Other(format!(
                    "remote worker '{role}' was cancelled (task {} cancelled on the worker)",
                    task.id
                )));
            }
            polled = self.client.await_task(&task.id, self.poll_interval, self.max_polls) => {
                polled.map_err(|e| a2a_err(&format!("awaiting remote worker '{role}'"), e))?
            }
        };
        if !awaited.status.state.is_terminal() {
            return Err(Error::Other(format!(
                "remote worker '{role}' did not finish task {} within {} polls",
                awaited.id, self.max_polls
            )));
        }
        Ok(outcome_from_task(&self.endpoint, &awaited))
    }
}

// ── fleet.dispatch / fleet.status / fleet.cancel ────────────────────────────

/// Resolve a caller-supplied worker endpoint into a client, guarding egress first.
fn worker_client(endpoint: &str, private_net: &PrivateNetAllow) -> Result<A2aClient> {
    guard_url_scoped(endpoint, private_net)?;
    A2aClient::new(endpoint).map_err(|e| Error::Other(format!("fleet worker endpoint: {e}")))
}

/// The subject list every `fleet.*` op reports: the worker's origin, or nothing when the endpoint
/// is unparseable. Never `*`.
fn endpoint_subjects(endpoint: Option<&str>) -> Vec<String> {
    endpoint
        .and_then(worker_origin)
        .map(|origin| vec![origin])
        .unwrap_or_default()
}

/// Arguments for `fleet.dispatch`.
#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DispatchInput {
    /// A2A endpoint of the worker to dispatch to (e.g. `https://worker-1.internal:8787`)
    worker: String,
    /// What the worker should do
    task: String,
    /// Worker role/persona name
    #[serde(default)]
    role: Option<String>,
    /// Existing conversation id to continue on the worker
    #[serde(default)]
    context_id: Option<String>,
}

/// Arguments for `fleet.status` and `fleet.cancel` — both act on a task the worker already owns.
#[derive(Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TaskRefInput {
    /// A2A endpoint of the worker that owns the task
    worker: String,
    /// Task id returned by `fleet.dispatch`
    task_id: String,
}

/// `fleet.dispatch` — hand a task to a remote worker and return its `task_id` without waiting.
///
/// This is the half [`Spawner`] cannot express: `spawn` is fire-and-await, so a coordinator that
/// wants to run ten workers concurrently and reconcile them later needs a non-blocking send.
pub struct FleetDispatchTool {
    private_net: PrivateNetAllow,
    token: Option<String>,
}

impl FleetDispatchTool {
    /// Build the op with the operator's private-network grant and an optional worker bearer token.
    pub fn new(private_net: PrivateNetAllow, token: Option<String>) -> Self {
        Self { private_net, token }
    }
}

#[async_trait]
impl Tool for FleetDispatchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fleet.dispatch".into(),
            description:
                "Dispatch a task to a remote flux worker over A2A without waiting for it. \
                          Returns the worker's task id; poll it with fleet.status and stop it with \
                          fleet.cancel."
                    .into(),
            input_schema: tool_input_schema::<DispatchInput>(),
            output_schema: None,
            // Network is the carrier; Process is the consequence — the worker runs arbitrary agent
            // work of its own choosing on the other end, exactly as the local `task` op declares.
            effects: vec![Effect::Network, Effect::Process],
            risk: Risk::Medium,
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Network, AccessKind::Provider],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let endpoint = serde_json::from_value::<DispatchInput>(params.clone())
            .ok()
            .map(|args| args.worker);
        endpoint_subjects(endpoint.as_deref())
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: DispatchInput = parse_params(params, "fleet.dispatch")?;
        let client = worker_client(&args.worker, &self.private_net)?.with_token(self.token.clone());
        let role = args.role.as_deref().unwrap_or("worker");
        let message = delegation_message(role, &args.task, args.context_id);
        match client.send(message, false).await {
            Ok(SendOutcome::Task(t)) => Ok(ToolResult::ok(
                serde_json::json!({
                    "task_id": t.id,
                    "context_id": t.context_id,
                    "state": t.status.state,
                })
                .to_string(),
            )),
            // A worker that answered synchronously left nothing to track; say so rather than
            // inventing a task id the caller would then poll forever.
            Ok(SendOutcome::Message(m)) => Ok(ToolResult::ok(
                serde_json::json!({ "task_id": Value::Null, "answer": m.text() }).to_string(),
            )),
            Err(e) => Ok(ToolResult::error(format!("fleet.dispatch: {e}"))),
        }
    }
}

/// `fleet.status` — read a dispatched task's current state from its worker.
pub struct FleetStatusTool {
    private_net: PrivateNetAllow,
    token: Option<String>,
}

impl FleetStatusTool {
    /// Build the op with the operator's private-network grant and an optional worker bearer token.
    pub fn new(private_net: PrivateNetAllow, token: Option<String>) -> Self {
        Self { private_net, token }
    }
}

#[async_trait]
impl Tool for FleetStatusTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fleet.status".into(),
            description: "Read the current state of a task previously dispatched to a remote flux \
                          worker with fleet.dispatch."
                .into(),
            input_schema: tool_input_schema::<TaskRefInput>(),
            output_schema: None,
            // A guarded fetch: `Read` + `Network` observes remote state and changes nothing.
            effects: vec![Effect::Read, Effect::Network],
            risk: Risk::Low,
            // Deliberately NOT `Idempotent`: that word licenses the op cache to serve a stored
            // result *instead of executing*, and the entire point of a status poll is to observe
            // the change since the last one.
            idempotency: Idempotency::NonIdempotent,
            access: vec![AccessKind::Network],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let endpoint = serde_json::from_value::<TaskRefInput>(params.clone())
            .ok()
            .map(|args| args.worker);
        endpoint_subjects(endpoint.as_deref())
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: TaskRefInput = parse_params(params, "fleet.status")?;
        let client = worker_client(&args.worker, &self.private_net)?.with_token(self.token.clone());
        match client.get_task(&args.task_id).await {
            Ok(t) => Ok(ToolResult::ok(
                serde_json::json!({
                    "task_id": t.id,
                    "state": t.status.state,
                    "terminal": t.status.state.is_terminal(),
                    "text": t.final_text(),
                })
                .to_string(),
            )),
            Err(e) => Ok(ToolResult::error(format!("fleet.status: {e}"))),
        }
    }
}

/// `fleet.cancel` — stop a dispatched task on its worker.
pub struct FleetCancelTool {
    private_net: PrivateNetAllow,
    token: Option<String>,
}

impl FleetCancelTool {
    /// Build the op with the operator's private-network grant and an optional worker bearer token.
    pub fn new(private_net: PrivateNetAllow, token: Option<String>) -> Self {
        Self { private_net, token }
    }
}

#[async_trait]
impl Tool for FleetCancelTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fleet.cancel".into(),
            description: "Stop a task previously dispatched to a remote flux worker. The worker \
                          aborts the run; already-finished tasks report that they were not \
                          cancelable."
                .into(),
            input_schema: tool_input_schema::<TaskRefInput>(),
            output_schema: None,
            // `Write` because it mutates state the caller does not own — a running worker turn.
            effects: vec![Effect::Write, Effect::Network],
            risk: Risk::Medium,
            // Repeating a cancel is safe by construction: the second attempt answers
            // `TaskNotCancelable` rather than doing anything further. That is the stated condition
            // `Conditional` exists for — it is NOT `Idempotent`, which would let the op cache skip
            // the call entirely.
            idempotency: Idempotency::Conditional,
            access: vec![AccessKind::Network],
            group: None,
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let endpoint = serde_json::from_value::<TaskRefInput>(params.clone())
            .ok()
            .map(|args| args.worker);
        endpoint_subjects(endpoint.as_deref())
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: TaskRefInput = parse_params(params, "fleet.cancel")?;
        let client = worker_client(&args.worker, &self.private_net)?.with_token(self.token.clone());
        match client.cancel_task(&args.task_id).await {
            Ok(t) => Ok(ToolResult::ok(
                serde_json::json!({ "task_id": t.id, "state": t.status.state }).to_string(),
            )),
            Err(e) => Ok(ToolResult::error(format!("fleet.cancel: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use flux_spec::metadata_violations;
    use flux_system::{System, Workspace};
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ── Stub worker ─────────────────────────────────────────────────────────

    /// Every `(method, params)` a stub worker was asked for, in order.
    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    /// Read one HTTP request (headers + `Content-Length` body) off `sock`.
    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = match sock.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(end) = text.find("\r\n\r\n") {
                let len = text[..end]
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if buf.len() >= end + 4 + len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    /// A loopback A2A worker: routes each JSON-RPC request through `respond(method, params)` and
    /// records what it was asked for. tokio + std only — no new dependency and no network beyond
    /// 127.0.0.1, per the offline-first testing rule.
    async fn worker_stub(
        respond: impl Fn(&str, &Value) -> Value + Send + Sync + 'static,
    ) -> (String, Seen) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let raw = read_request(&mut sock).await;
                let body = raw.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
                let req: Value = serde_json::from_str(body).unwrap_or(Value::Null);
                let method = req
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let params = req.get("params").cloned().unwrap_or(Value::Null);
                recorder
                    .lock()
                    .unwrap()
                    .push((method.clone(), params.clone()));
                let payload =
                    json!({ "jsonrpc": "2.0", "id": 1, "result": respond(&method, &params) })
                        .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                     Connection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    /// A `Task` value in `state`, carrying `text` as its final status message.
    fn task_json(id: &str, state: &str, text: &str) -> Value {
        json!({
            "kind": "task",
            "id": id,
            "status": {
                "state": state,
                "message": {
                    "kind": "message",
                    "messageId": "m_1",
                    "role": "agent",
                    "parts": [{ "kind": "text", "text": text }],
                },
            },
        })
    }

    fn temp_system() -> Arc<System> {
        let dir = std::env::temp_dir().join(format!("flux-fleet-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Arc::new(System::new(Workspace::new(&dir).unwrap()))
    }

    // ── A2aSpawner ──────────────────────────────────────────────────────────

    /// A-116 Acceptance 2, the named failing-first test: cancelling the token must cancel the
    /// **remote** task, not merely detach this client from it. A worker left running after the
    /// coordinator believed it had stopped it is the exact gap `cancel_task` closes.
    ///
    /// Deterministic without a sleep: the stub fires the caller's token at the moment it serves the
    /// first `tasks/get`, which is precisely when the spawner is parked in its cancellable poll.
    #[tokio::test]
    async fn cancelling_the_token_cancels_the_remote_task() {
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let (base, seen) = worker_stub(move |method, _params| match method {
            // Answer the blocking send with a task that is still running, so the id is known and
            // the spawner enters the poll — flux-server's own behaviour when a run outlives the
            // blocking send.
            "message/send" => task_json("t_1", "working", ""),
            "tasks/get" => {
                trigger.cancel();
                task_json("t_1", "working", "")
            }
            "tasks/cancel" => task_json("t_1", "canceled", ""),
            _ => Value::Null,
        })
        .await;

        let spawner = A2aSpawner::new(&base, &PrivateNetAllow::Any, None)
            .unwrap()
            .with_poll(Duration::from_millis(5), 50);
        let err = spawner
            .spawn(SpawnRequest::new("worker", "index the repo"), &cancel)
            .await
            .expect_err("a cancelled spawn reports the cancellation");
        assert!(
            err.to_string().contains("cancelled"),
            "expected a cancellation error, got: {err}"
        );

        let calls = seen.lock().unwrap();
        let cancelled = calls
            .iter()
            .find(|(m, _)| m == "tasks/cancel")
            .unwrap_or_else(|| panic!("no tasks/cancel reached the worker; saw {calls:?}"));
        assert_eq!(cancelled.1.get("id").and_then(Value::as_str), Some("t_1"));
    }

    /// A token already fired before `spawn` must not dispatch remote work at all — the `biased`
    /// select arm. Otherwise a cancelled turn still starts a worker it will never collect.
    #[tokio::test]
    async fn a_pre_cancelled_token_dispatches_nothing() {
        let (base, seen) =
            worker_stub(|_m, _p| task_json("t_x", "completed", "should never run")).await;
        let cancel = CancellationToken::new();
        cancel.cancel();

        let spawner = A2aSpawner::new(&base, &PrivateNetAllow::Any, None).unwrap();
        let err = spawner
            .spawn(SpawnRequest::new("worker", "do it"), &cancel)
            .await
            .expect_err("a pre-cancelled spawn reports the cancellation");
        assert!(err.to_string().contains("before dispatch"), "got: {err}");
        assert!(
            seen.lock().unwrap().is_empty(),
            "a pre-cancelled spawn must reach the worker zero times"
        );
    }

    /// A worker that finishes inside the blocking send needs no poll and no cancel.
    #[tokio::test]
    async fn a_terminal_blocking_send_returns_the_workers_text() {
        let (base, seen) =
            worker_stub(|_m, _p| task_json("t_2", "completed", "extracted 12 symbols")).await;

        let spawner = A2aSpawner::new(&base, &PrivateNetAllow::Any, None).unwrap();
        let outcome = spawner
            .spawn(
                SpawnRequest::new("scout", "look around"),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.text, "extracted 12 symbols");
        // The remote task id is the correlation handle a follow-up `tasks/get` needs.
        assert_eq!(outcome.session_id, "t_2");
        assert_eq!(outcome.model, format!("a2a:{base}"));
        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 1, "one blocking send, no poll: {calls:?}");
        assert_eq!(calls[0].0, "message/send");
    }

    /// A-116 Acceptance 3: the existing `task` op drives a remote worker **verbatim** — the op is
    /// unchanged and reaches `A2aSpawner` through the same `ToolContext::spawner` seam
    /// `LocalSpawner` uses.
    #[tokio::test]
    async fn the_task_op_drives_the_remote_spawner_verbatim() {
        let (base, seen) =
            worker_stub(|_m, _p| task_json("t_3", "completed", "remote answer")).await;
        let spawner = Arc::new(A2aSpawner::new(&base, &PrivateNetAllow::Any, None).unwrap());
        let ctx = ToolContext::new(temp_system()).with_spawner(spawner);

        let out = crate::TaskTool
            .execute(&ctx, json!({ "role": "worker", "task": "extract Alice" }))
            .await
            .unwrap();
        assert!(!out.is_error, "task op errored: {}", out.content);
        assert_eq!(out.content, "remote answer");
        // The role reached the worker as an A2A `data` part rather than being dropped.
        let calls = seen.lock().unwrap();
        let sent = &calls[0].1;
        let parts = sent["message"]["parts"].as_array().unwrap();
        assert!(
            parts.iter().any(|p| p["data"]["role"] == "worker"),
            "role did not cross the wire: {parts:?}"
        );
    }

    // ── fleet.* op metadata (Acceptance 5) ──────────────────────────────────

    fn fleet_specs() -> Vec<ToolSpec> {
        vec![
            FleetDispatchTool::new(PrivateNetAllow::None, None).spec(),
            FleetStatusTool::new(PrivateNetAllow::None, None).spec(),
            FleetCancelTool::new(PrivateNetAllow::None, None).spec(),
        ]
    }

    /// The `fleet.*` ops are not in `try_register_builtins`, so `flux-tools`' registry-wide gate
    /// does not cover them. Hold them to the same I1/I2/I3 encoding here.
    #[test]
    fn fleet_op_metadata_is_coherent() {
        let mut violations = Vec::new();
        for spec in fleet_specs() {
            violations.extend(metadata_violations(&spec, &[]));
        }
        assert!(violations.is_empty(), "{}", violations.join("\n  "));
    }

    /// A status poll must never be `Idempotent`: that word licenses the op cache to serve a stored
    /// result *instead of executing*, which would make `fleet.status` unable to observe the state
    /// change it exists to observe.
    #[test]
    fn fleet_status_is_never_cache_replayable() {
        let spec = FleetStatusTool::new(PrivateNetAllow::None, None).spec();
        assert_ne!(spec.idempotency, Idempotency::Idempotent);
    }

    /// A-116 Acceptance 5: the dispatch target's guarded origin is the subject — never `*`, and
    /// never empty just to dodge the gate when the endpoint IS nameable.
    #[test]
    fn permission_subjects_are_the_workers_origin() {
        let dispatch = FleetDispatchTool::new(PrivateNetAllow::None, None);
        assert_eq!(
            dispatch.permission_subjects(&json!({
                "worker": "https://worker-1.internal:8787/a2a",
                "task": "build",
            })),
            vec!["https://worker-1.internal:8787".to_string()],
        );
        // The default port is not invented into the origin.
        assert_eq!(
            dispatch.permission_subjects(&json!({ "worker": "https://w.example", "task": "x" })),
            vec!["https://w.example".to_string()],
        );

        for tool in [
            &FleetStatusTool::new(PrivateNetAllow::None, None) as &dyn Tool,
            &FleetCancelTool::new(PrivateNetAllow::None, None) as &dyn Tool,
        ] {
            assert_eq!(
                tool.permission_subjects(&json!({
                    "worker": "http://10.0.0.4:8787",
                    "task_id": "t_9",
                })),
                vec!["http://10.0.0.4:8787".to_string()],
            );
        }
    }

    /// An endpoint the op cannot name yields NO subjects, which forces approval. The failure mode
    /// this guards is the opposite one: a `*` or a silently-empty subject list on a nameable target
    /// would match a broad path grant.
    #[test]
    fn an_unnameable_worker_yields_no_subjects_never_a_wildcard() {
        let dispatch = FleetDispatchTool::new(PrivateNetAllow::None, None);
        for params in [
            json!({ "worker": "not a url", "task": "x" }),
            json!({ "worker": "file:///etc/passwd", "task": "x" }),
            json!({ "task": "x" }),
            json!({}),
        ] {
            let subjects = dispatch.permission_subjects(&params);
            assert!(
                subjects.is_empty(),
                "expected no subjects for {params}, got {subjects:?}"
            );
            assert!(!subjects.iter().any(|s| s == "*"));
        }
    }

    /// Every `fleet.*` op takes its endpoint from the caller, so each must resolve it through the
    /// egress guard. Without this a model-named `worker` reaches loopback and link-local addresses.
    #[tokio::test]
    async fn a_private_worker_is_refused_without_the_grant() {
        let ctx = ToolContext::new(temp_system());
        let status = FleetStatusTool::new(PrivateNetAllow::None, None);
        let err = status
            .execute(
                &ctx,
                json!({ "worker": "http://169.254.169.254", "task_id": "t" }),
            )
            .await
            .expect_err("link-local egress is refused without a private-net grant");
        assert!(err.to_string().contains("169.254.169.254"), "got: {err}");

        let dispatch = FleetDispatchTool::new(PrivateNetAllow::None, None);
        assert!(dispatch
            .execute(&ctx, json!({ "worker": "http://127.0.0.1:9", "task": "x" }))
            .await
            .is_err());
    }

    // ── fleet.dispatch / status / cancel behaviour (Acceptance 4) ───────────

    /// `fleet.dispatch` is the fire-and-**track** half: a NON-blocking send that hands back a
    /// `task_id` instead of waiting, which `Spawner`'s fire-and-await signature cannot express.
    #[tokio::test]
    async fn fleet_dispatch_sends_non_blocking_and_returns_the_task_id() {
        let (base, seen) = worker_stub(|_m, _p| task_json("t_7", "submitted", "")).await;
        let ctx = ToolContext::new(temp_system());
        let out = FleetDispatchTool::new(PrivateNetAllow::Any, None)
            .execute(
                &ctx,
                json!({ "worker": base, "task": "sweep", "role": "sweeper" }),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let body: Value = serde_json::from_str(&out.content).unwrap();
        assert_eq!(body["task_id"], "t_7");

        let calls = seen.lock().unwrap();
        assert_eq!(calls[0].0, "message/send");
        assert_eq!(
            calls[0].1["configuration"]["blocking"],
            json!(false),
            "fleet.dispatch must not block: {:?}",
            calls[0].1
        );
    }

    /// `fleet.status` wraps `tasks/get`; `fleet.cancel` wraps the new `cancel_task`.
    #[tokio::test]
    async fn fleet_status_and_cancel_wrap_their_rpcs() {
        let (base, seen) = worker_stub(|method, _p| match method {
            "tasks/get" => task_json("t_7", "working", "half done"),
            "tasks/cancel" => task_json("t_7", "canceled", ""),
            _ => Value::Null,
        })
        .await;
        let ctx = ToolContext::new(temp_system());

        let status = FleetStatusTool::new(PrivateNetAllow::Any, None)
            .execute(&ctx, json!({ "worker": base.clone(), "task_id": "t_7" }))
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&status.content).unwrap();
        assert_eq!(body["state"], "working");
        assert_eq!(body["terminal"], json!(false));

        let cancelled = FleetCancelTool::new(PrivateNetAllow::Any, None)
            .execute(&ctx, json!({ "worker": base, "task_id": "t_7" }))
            .await
            .unwrap();
        let body: Value = serde_json::from_str(&cancelled.content).unwrap();
        assert_eq!(body["state"], "canceled");

        let calls = seen.lock().unwrap();
        assert_eq!(calls[0].0, "tasks/get");
        assert_eq!(calls[1].0, "tasks/cancel");
        assert_eq!(calls[1].1["id"], "t_7");
    }
}
