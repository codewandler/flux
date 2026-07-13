//! A2A (Agent-to-Agent) protocol support for `flux-server`.
//!
//! Adds three routes to the router:
//!   `GET  /.well-known/agent-card.json` — agent discovery card (A2A spec)
//!   `GET  /.well-known/agent.json`      — legacy discovery alias (same card)
//!   `POST /a2a`                         — JSON-RPC 2.0 dispatcher
//!
//! Supported methods (current A2A spec):
//! - `message/send`   — run one flux turn; `configuration.blocking: true` returns the finished
//!   `Task` synchronously, otherwise (the spec default) a `submitted` task returns immediately
//!   and the turn runs in the background (A-54)
//! - `message/stream` — run one flux turn, stream `TaskStatusUpdate` events as Server-Sent Events
//! - `tasks/get`         — poll a live or retained task to its current state (A-54)
//! - `tasks/cancel`      — fire a live task's cancellation out-of-band (A-55)
//! - `tasks/resubscribe` — re-attach an SSE stream to a live or retained task (A-56)
//! - `tasks/pushNotificationConfig/{set,get,list,delete}` — per-task webhooks (A-57)
//!
//! **The task model (A-53 design):** task id = the flux session id; a `Task` is a *projection*
//! over the session's own turn-lifecycle events (no second store), realm-scoped like every A2A
//! lookup; an in-process [`TaskRegistry`] holds the live runs' cancellation/broadcast handles and
//! the sweep keep-list. See `docs/designs/a2a-stateful-task-model.md`.
//!
//! The wire shapes come from the shared [`flux_a2a`] types, so client and server agree on one
//! definition. **Stateful A2A mode (A-48): one session per `contextId`** — a request whose
//! `contextId` matches a live A2A session continues that session (the engine's conversation
//! projection provides multi-turn memory), exactly the promise the earlier stateless-mode comment
//! made ("needs no client change"): the id was already echoed and recorded as the session's
//! correlation id. A request without a `contextId` mints a fresh session per task, as before.
//! The agent card is exempt from bearer-token auth so external agents can discover flux without a key.
//!
//! Session retention (C-18): every session minted here is tagged `agent_id = "a2a"` in its D-02
//! context envelope, and each mint first sweeps A2A-tagged sessions whose last activity is older
//! than the configured TTL (`[server] a2a_session_ttl_secs`, default 1h, `0` = never) — see
//! [`create_a2a_session`]. Only A2A-tagged sessions are ever eligible.
//!
//! Minting happens *after* the single-turn `turn_gate` is acquired, never before (C-29): a
//! session minted while still queued behind another in-flight turn would sit with a frozen
//! `updated_at` until its own `run_turn` starts, so a concurrent request's mint-time sweep could
//! prune it out from under the queue — no error results (there is no FK from `events` back to
//! `streams`), but its already-tagged registry row is gone, so every future append to its stream
//! becomes an orphaned, unenumerable event row and its spend drops out of the usage rollups.

use std::collections::HashMap;
use std::convert::Infallible;
use std::pin::Pin;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_a2a::{error, server};
use flux_a2a::{AgentCard, Message, Task, TaskState, TaskStatus};
use flux_events::EventKind;
use flux_flow::AgentSink;

use std::sync::Arc;

use super::Collect;
use crate::{A2aTtl, AgentResolver, CardInfo, Shared, TurnGate};

// ── A2A session lifecycle (C-18) ─────────────────────────────────────────────

/// The `agent_id` tag stamped on every session minted by the A2A surface (the D-02 context
/// envelope — the same convention as flux-orchestrate's `subagent:<role>` streams). TTL pruning
/// is scoped to exactly this tag, so a CLI/TUI session (empty context) is never eligible.
pub(crate) const A2A_AGENT_ID: &str = "a2a";

/// Resolve the session for one A2A task: **reuse the live session whose correlation id equals the
/// request's `contextId`** (A-48 stateful mode) or create a new one tagged `agent_id = "a2a"` with
/// that `contextId` (if any) as the correlation id. This is the find-or-mint half only — call it
/// through [`mint_and_register`], which couples it to the TTL sweep and the live-task registration
/// under one registry lock hold (the C-29 protection for the async era).
fn find_or_mint_session(
    engine: &Shared,
    context_id: Option<&str>,
    realm: Option<&str>,
) -> flux_core::Result<String> {
    if let Some(cid) = context_id {
        // `contextId` is a grouping key, NOT a security boundary (A2A spec) — in principal mode
        // (`realm` set) continuity is keyed within the caller's realm, so the same `contextId`
        // presented by two tenants yields two isolated sessions. The realm-scoped lookup matches
        // `account =` (never NULL), so pre-D-69 untagged sessions are structurally unreachable.
        let existing = match realm {
            Some(r) => engine
                .events
                .find_correlated_in_realm(cid, A2A_AGENT_ID, r)?,
            None => engine.events.find_correlated(cid, A2A_AGENT_ID)?,
        };
        if let Some(existing) = existing {
            return Ok(existing);
        }
    }
    let ctx = flux_events::EventContext {
        account: realm.map(str::to_string),
        agent_id: Some(A2A_AGENT_ID.to_string()),
        correlation_id: context_id.map(str::to_string),
        ..Default::default()
    };
    engine
        .events
        .create_session_with_context(&engine.model, &ctx)
}

/// Sweep A2A-tagged sessions whose last activity is more than `ttl_secs` old, as of `now_ms` —
/// except the streams named in `keep` (the in-process live tasks, A-54). `ttl_secs == 0` disables
/// pruning entirely (the documented `[server] a2a_session_ttl_secs = 0`). Non-fatal by design: a
/// failed sweep logs and returns 0 — it must never block the task that triggered it. Returns the
/// number of sessions pruned.
///
/// The sweep runs lazily per mint rather than on a background timer in `serve_on`, because:
/// (a) it then covers *every* mount of [`crate::router`] — the standalone server and the `a2a`
/// channel (which serves the router itself) — with no per-caller wiring; (b) growth only happens
/// when tasks arrive, so sweeping at mint time bounds the registry exactly where it can grow (an
/// idle server accretes nothing, so nothing goes stale while idle); and (c) it needs no
/// background-task lifecycle/shutdown handling. The pass is one indexed query over `streams`
/// plus a whole-stream delete per expired session — negligible next to the model turn it precedes.
pub(crate) fn prune_expired_a2a_sessions_at(
    events: &flux_events::EventStore,
    ttl_secs: u64,
    now_ms: i64,
    keep: &[String],
) -> usize {
    if ttl_secs == 0 {
        return 0;
    }
    let ttl_ms = i64::try_from(ttl_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    match events.prune_inactive_excluding(A2A_AGENT_ID, now_ms.saturating_sub(ttl_ms), keep) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("(a2a session sweep failed: {e})");
            0
        }
    }
}

/// Milliseconds since the Unix epoch — the store's own clock convention (`streams.updated_at`).
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ── Live-task registry (A-54/A-55/A-56/A-57) ─────────────────────────────────

/// One in-process live A2A task: the handles `tasks/cancel` and `tasks/resubscribe` need while a
/// run is queued or in flight. Terminal tasks have no entry — they are served purely from the
/// event-log projection ([`project_stored_task`]), so a restart still answers `tasks/get` for
/// finished work; only *live* cancel/resubscribe require the run to be in-process.
struct LiveTask {
    state: TaskState,
    /// The realm the task was minted in (D-69). Every registry lookup is realm-checked; a
    /// mismatch answers exactly like an unknown id (`-32001`), never a distinguishable "exists
    /// but forbidden".
    realm: Option<String>,
    context_id: String,
    /// Fired by `tasks/cancel` (A-55) or the owning SSE stream's disconnect drop-guard; observed
    /// between plan rounds by `run_turn_cancellable`.
    cancel: CancellationToken,
    /// Live status/artifact update frames as bare JSON-RPC `result` values — each SSE surface
    /// wraps them in its own envelope (a resubscriber's request id differs from the sender's).
    /// Send errors (no subscriber) are normal and ignored.
    updates: tokio::sync::broadcast::Sender<Value>,
}

/// The in-process registry of live A2A tasks, plus per-task push-notification configs (A-57) and
/// the webhook delivery client. One per router (like the turn gate), shared across the mount's
/// agents: keys are `(scope, task_id)` — scope is `""` on the single-agent mount and the
/// `agent_id` on a multi-agent mount — so two agents' identical `s_<n>` ids never collide.
#[derive(Default)]
pub struct TaskRegistry {
    live: std::sync::Mutex<HashMap<(String, String), LiveTask>>,
    /// Push configs live beside (not inside) the live map so a config set while running is still
    /// readable after the task finishes. In-process only, like the live map: a restart drops
    /// them, and delivery only happens for in-process runs anyway.
    push: std::sync::Mutex<HashMap<(String, String), Vec<Value>>>,
    /// One pooled HTTP client for webhook delivery (A-57).
    http: reqwest::Client,
}

/// What [`mint_and_register`] hands the run that was just registered.
struct RegisteredTask {
    session_id: String,
    context_id: String,
    cancel: CancellationToken,
}

/// The outcome of [`TaskRegistry::request_cancel`].
enum CancelHit {
    /// The live run's token was fired; carries the task's `contextId`.
    Cancelled(String),
    /// The live task is already cancel-requested — a terminal state per spec (`-32002`).
    AlreadyCanceled,
    /// No live, realm-matching entry — fall through to the stored projection.
    NotLive,
}

/// Why [`mint_and_register`] declined.
enum MintError {
    /// The resolved session already has a live in-process task. One session runs one task at a
    /// time (task-id = session id); the caller reports it and the client polls `tasks/get`.
    AlreadyRunning(String),
    Store(flux_core::Error),
}

impl TaskRegistry {
    /// Realm-checked lookup: `Some` only when the task is live **and** minted in the caller's
    /// realm; a cross-realm hit is `None`, indistinguishable from an unknown id.
    fn snapshot(&self, scope: &str, id: &str, realm: Option<&str>) -> Option<(TaskState, String)> {
        let live = self.live.lock().unwrap();
        let t = live.get(&(scope.to_string(), id.to_string()))?;
        (t.realm.as_deref() == realm).then(|| (t.state, t.context_id.clone()))
    }

    /// Realm-checked subscribe (A-56): the current state + a receiver for the live frames.
    /// Subscribing and snapshotting under one lock hold means no frame can fall between the
    /// snapshot and the subscription.
    fn subscribe(
        &self,
        scope: &str,
        id: &str,
        realm: Option<&str>,
    ) -> Option<(TaskState, String, tokio::sync::broadcast::Receiver<Value>)> {
        let live = self.live.lock().unwrap();
        let t = live.get(&(scope.to_string(), id.to_string()))?;
        (t.realm.as_deref() == realm)
            .then(|| (t.state, t.context_id.clone(), t.updates.subscribe()))
    }

    /// Advance a live task's state (`submitted → working`, or `→ canceled` on a cancel request).
    fn set_state(&self, scope: &str, id: &str, state: TaskState) {
        if let Some(t) = self
            .live
            .lock()
            .unwrap()
            .get_mut(&(scope.to_string(), id.to_string()))
        {
            t.state = state;
        }
    }

    /// Fire a live task's cancellation (A-55), realm-checked.
    fn request_cancel(&self, scope: &str, id: &str, realm: Option<&str>) -> CancelHit {
        let mut live = self.live.lock().unwrap();
        let Some(t) = live.get_mut(&(scope.to_string(), id.to_string())) else {
            return CancelHit::NotLive;
        };
        if t.realm.as_deref() != realm {
            return CancelHit::NotLive; // cross-realm == unknown, constant
        }
        if t.state == TaskState::Canceled {
            // Already cancel-requested (the run is still draining to its durable terminal
            // state) — `canceled` is a terminal state per spec, so a repeat is not cancelable.
            return CancelHit::AlreadyCanceled;
        }
        t.cancel.cancel();
        t.state = TaskState::Canceled;
        CancelHit::Cancelled(t.context_id.clone())
    }

    /// Remove a finished run's entry. Call *before* releasing the turn gate, so a follow-up send
    /// on the same context (queued at the gate) never collides with a completed run's entry.
    fn finish(&self, scope: &str, id: &str) {
        self.live
            .lock()
            .unwrap()
            .remove(&(scope.to_string(), id.to_string()));
    }

    /// Publish one update frame to a live task's subscribers (send errors — no subscriber — are
    /// normal), and, for status *transitions* (never per-token deltas — see [`deliver_push`]'s
    /// caller contract), the caller also fans it out to push webhooks.
    fn broadcast(&self, scope: &str, id: &str, frame: Value) {
        if let Some(t) = self
            .live
            .lock()
            .unwrap()
            .get(&(scope.to_string(), id.to_string()))
        {
            let _ = t.updates.send(frame);
        }
    }
}

/// Sweep + find-or-mint + register, atomically with respect to the registry (A-54).
///
/// The lock choreography is the point. Pre-A-54 the C-29 rule was "mint only under the turn
/// gate", which worked because every sweep was a mint-time sweep and every mint ran its turn
/// back-to-back under one gate hold. Non-blocking sends break that: the task id must be answered
/// *now*, so the mint cannot wait behind an in-flight turn — and a session minted (or merely
/// still queued/running) outside the gate is exposed to a concurrent request's sweep again. The
/// registry is the new protection: the keep-list snapshot, the sweep (which excludes it), the
/// mint, and the registration all happen under ONE registry lock hold, so no concurrent sweep
/// can ever observe a live task's session without its keep-list entry. Blocking and streaming
/// sends register here too (they are just as swept-at while running — pre-A-54 no sweep could
/// run mid-turn, now a non-blocking mint's sweep can), which is also what makes them cancelable
/// and resubscribable (A-55/A-56).
///
/// The DB work under the lock is the same few indexed statements the mint always ran; contention
/// is per-request, not per-token.
#[allow(clippy::too_many_arguments)]
fn mint_and_register(
    registry: &TaskRegistry,
    scope: &str,
    engine: &Shared,
    ttl: A2aTtl,
    requested_context: Option<&str>,
    realm: Option<&str>,
    initial: TaskState,
    cancel: CancellationToken,
) -> Result<RegisteredTask, MintError> {
    let mut live = registry.live.lock().unwrap();
    let keep: Vec<String> = live.keys().map(|(_, id)| id.clone()).collect();
    let pruned = prune_expired_a2a_sessions_at(&engine.events, ttl.0, now_ms(), &keep);
    if pruned > 0 {
        eprintln!(
            "(a2a: pruned {pruned} expired session(s) past the {}s TTL)",
            ttl.0
        );
    }
    let session_id =
        find_or_mint_session(engine, requested_context, realm).map_err(MintError::Store)?;
    let key = (scope.to_string(), session_id.clone());
    if live.contains_key(&key) {
        return Err(MintError::AlreadyRunning(session_id));
    }
    let context_id = requested_context
        .map(str::to_string)
        .unwrap_or_else(|| session_id.clone());
    let (updates, _) = tokio::sync::broadcast::channel(256);
    live.insert(
        key,
        LiveTask {
            state: initial,
            realm: realm.map(str::to_string),
            context_id: context_id.clone(),
            cancel: cancel.clone(),
            updates,
        },
    );
    Ok(RegisteredTask {
        session_id,
        context_id,
        cancel,
    })
}

// ── Agent Card ────────────────────────────────────────────────────────────────

/// `GET /.well-known/agent-card.json` (and the `…/agent.json` alias) — A2A discovery.
///
/// The card's `name`/`description`/`skills` come from the served agent's [`CardInfo`] (the built-in
/// coding agent by default, or a program-declared agent when mounted by the `a2a` channel). The `url`
/// field points to the `/a2a` JSON-RPC endpoint: in principal mode it derives from the configured
/// external base ONLY — the card tells clients where to send bearer tokens, and this route is
/// (deliberately, per spec) public, so deriving from the request's `Host` header would let a
/// Host-poisoned request phish tokens toward an attacker host. The open/shared-secret modes keep
/// the pre-D-69 `Host`/`X-Forwarded-Proto` derivation.
///
/// Whenever auth is enabled the card declares its scheme (`securitySchemes` + `security`): the A2A
/// spec has clients authenticate "using one of the schemes declared in the card", which is only
/// satisfiable if servers actually declare one.
pub async fn agent_card(
    State(card): State<Arc<CardInfo>>,
    State(auth): State<Arc<crate::ServerAuth>>,
    headers: HeaderMap,
) -> Json<AgentCard> {
    // Single-agent mount: the `/a2a` endpoint sits at the server root (no path prefix).
    Json(build_agent_card(&card, &auth, &headers, ""))
}

/// Build the A2A discovery card for one agent. `a2a_path_prefix` is the mount prefix before
/// `/a2a` — `""` for the single-agent surface, `"/<agent_id>"` for a resolver-keyed multi-agent
/// mount (D-63) — so the advertised `url` is always the endpoint clients should actually POST to.
///
/// The `url` derivation: in principal mode it comes from the configured external base ONLY — the
/// card tells clients where to send bearer tokens, and this route is (deliberately, per spec)
/// public, so deriving from the request `Host` header would let a Host-poisoned request phish
/// tokens toward an attacker host. Open/shared-secret modes keep the `Host`/`X-Forwarded-Proto`
/// derivation. Whenever auth is enabled the card declares its scheme (`securitySchemes` +
/// `security`): the A2A spec has clients authenticate "using one of the schemes declared in the
/// card", satisfiable only if the server declares one.
pub(crate) fn build_agent_card(
    card: &CardInfo,
    auth: &crate::ServerAuth,
    headers: &HeaderMap,
    a2a_path_prefix: &str,
) -> AgentCard {
    let a2a_path = format!("{a2a_path_prefix}/a2a");
    // Prefer the configured external base (both Principal and, when set, SharedSecret) over the
    // request `Host` header: the public card tells clients where to send bearer tokens, so a
    // Host-poisoned fetch would otherwise phish the credential to an attacker host. Host derivation
    // remains only when no base is configured (a loopback/dev bind).
    let url = match auth.card_external_url() {
        Some(base) => format!("{}{a2a_path}", base.trim_end_matches('/')),
        None => {
            let host = headers
                .get("host")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("localhost");
            let forwarded_proto = headers
                .get("x-forwarded-proto")
                .and_then(|h| h.to_str().ok());
            server::card_url(forwarded_proto, host, &a2a_path)
        }
    };

    let mut out = server::agent_card(
        &card.name,
        &card.description,
        Some(url),
        env!("CARGO_PKG_VERSION"),
        &card.skills,
        true,
    )
    // A-57: this surface implements `tasks/pushNotificationConfig/*` + webhook delivery.
    .with_push_notifications(true);
    // Optional discovery metadata (A-49): emitted only when the served agent's `CardInfo` carries
    // it, so a card that sets none stays byte-stable.
    if let Some(provider) = &card.provider {
        out = out.with_provider(provider.clone());
    }
    if let Some(doc) = &card.documentation_url {
        out = out.with_documentation_url(doc.clone());
    }
    if let Some(icon) = &card.icon_url {
        out = out.with_icon_url(icon.clone());
    }
    if !matches!(auth, crate::ServerAuth::Open) {
        out = out
            .with_security_schemes(std::collections::BTreeMap::from([(
                "bearer".to_string(),
                json!({ "type": "http", "scheme": "bearer" }),
            )]))
            .with_security(vec![std::collections::BTreeMap::from([(
                "bearer".to_string(),
                Vec::new(),
            )])]);
    }
    out
}

// ── Multi-agent mount (D-63) ────────────────────────────────────────────────────

/// `GET /:agent_id/.well-known/agent-card.json` — discovery for one agent of a resolver-keyed
/// multi-agent mount. Resolves the agent (public route: no `AuthContext` available), then builds
/// its card advertising `<base>/<agent_id>/a2a` so a client reads the endpoint it must actually
/// POST to. An unknown agent is a constant 404 (§13.1).
pub async fn agent_card_multi(
    State(resolver): State<Arc<dyn AgentResolver>>,
    State(auth): State<Arc<crate::ServerAuth>>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(resolved) = resolver.resolve(&agent_id, None).await else {
        return crate::realm_not_found();
    };
    let prefix = format!("/{agent_id}");
    Json(build_agent_card(&resolved.card, &auth, &headers, &prefix)).into_response()
}

/// `POST /:agent_id/a2a` — the JSON-RPC dispatcher for one agent of a resolver-keyed mount.
/// Auth has already run ([`crate::require_auth`]), so the resolver sees the authenticated
/// principal; the resolved engine is pinned for the request (and for a streaming turn's whole
/// lifetime, since `send`/`subscribe` own their engine clone). An unknown agent → constant 404.
#[allow(clippy::too_many_arguments)]
pub async fn a2a_handler_multi(
    State(resolver): State<Arc<dyn AgentResolver>>,
    State(auth): State<Arc<crate::ServerAuth>>,
    State(turn_gate): State<TurnGate>,
    State(tasks): State<Arc<TaskRegistry>>,
    State(a2a_ttl): State<A2aTtl>,
    Path(agent_id): Path<String>,
    ctx: Option<axum::Extension<flux_auth::request::AuthContext>>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if req.jsonrpc != "2.0" {
        return rpc_err(req.id, -32600, "jsonrpc must be \"2.0\"").into_response();
    }
    let ctx = ctx.map(|e| e.0);
    let Some(resolved) = resolver.resolve(&agent_id, ctx.as_ref()).await else {
        return crate::realm_not_found();
    };
    // The agent id scopes every registry key, so two agents' identical session ids never collide.
    dispatch_rpc(
        resolved.engine,
        auth,
        turn_gate,
        tasks,
        agent_id,
        a2a_ttl,
        ctx,
        req,
    )
    .await
}

/// Dispatch one parsed JSON-RPC request against `engine` under `scope`. Shared by the
/// single-agent and multi-agent handlers so the A2A method surface cannot drift between mounts.
#[allow(clippy::too_many_arguments)]
async fn dispatch_rpc(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
    registry: Arc<TaskRegistry>,
    scope: String,
    a2a_ttl: A2aTtl,
    ctx: Option<flux_auth::request::AuthContext>,
    req: JsonRpcRequest,
) -> Response {
    match req.method.as_str() {
        "message/send" => send(
            engine, auth, turn_gate, registry, scope, a2a_ttl, ctx, req.id, req.params,
        )
        .await
        .into_response(),
        "message/stream" => {
            match subscribe(
                engine,
                auth,
                turn_gate,
                registry,
                scope,
                a2a_ttl,
                ctx,
                req.id.clone(),
                req.params,
            )
            .await
            {
                Ok(sse) => sse.into_response(),
                // Format pre-SSE errors as JSON-RPC so the `id` is not silently dropped;
                // `subscribe` carries the A2A-specific code (e.g. `-32005`).
                Err((code, msg)) => rpc_err(req.id, code, msg).into_response(),
            }
        }
        "tasks/get" => tasks_get(engine, registry, scope, auth, ctx, req.id, req.params)
            .await
            .into_response(),
        "tasks/cancel" => tasks_cancel(engine, registry, scope, auth, ctx, req.id, req.params)
            .await
            .into_response(),
        "tasks/resubscribe" => {
            match tasks_resubscribe(
                engine,
                registry,
                scope,
                auth,
                ctx,
                req.id.clone(),
                req.params,
            )
            .await
            {
                Ok(sse) => sse.into_response(),
                Err((code, msg)) => rpc_err(req.id, code, msg).into_response(),
            }
        }
        "tasks/pushNotificationConfig/set"
        | "tasks/pushNotificationConfig/get"
        | "tasks/pushNotificationConfig/list"
        | "tasks/pushNotificationConfig/delete" => {
            let method = req.method.clone();
            push_config(
                engine, registry, scope, auth, ctx, &method, req.id, req.params,
            )
            .await
            .into_response()
        }
        m if server::is_unsupported_a2a_method(m) => rpc_err(
            req.id,
            error::UNSUPPORTED_OPERATION,
            format!("Unsupported operation: {m}"),
        )
        .into_response(),
        m => rpc_err(req.id, -32601, format!("Method not found: {m}")).into_response(),
    }
}

// ── JSON-RPC 2.0 helpers ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

fn rpc_json(id: Option<Value>, result: Value) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn rpc_err(id: Option<Value>, code: i32, msg: impl Into<String>) -> Json<Value> {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg.into() } }))
}

// ── A2A message helpers ───────────────────────────────────────────────────────
//
// Text/contextId extraction, the agent card, the RFC-3339 stamp, and the status-update shaping are
// the reusable A2A protocol logic — they live in `flux_a2a::server` and are shared with other A2A
// surfaces. This module keeps only the flux-server-specific axum
// routes, the engine wiring, and the SSE streaming control-flow.

/// Build an SSE frame: a JSON-RPC response whose `result` is a `TaskStatusUpdateEvent`. The SSE
/// event name is left at the default so the frame is a plain `data:` JSON-RPC response per spec.
fn status_frame(
    id: &Option<Value>,
    task_id: &str,
    context_id: &str,
    state: TaskState,
    message: Option<Message>,
    is_final: bool,
) -> Event {
    let result = server::status_update_value(task_id, context_id, state, message, is_final);
    Event::default().data(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

// ── Main dispatcher ───────────────────────────────────────────────────────────

/// `POST /a2a` — JSON-RPC 2.0 endpoint (the single-agent mount; scope `""`).
///
/// - `message/send`   → [`send`] (blocking or non-blocking per `configuration.blocking`)
/// - `message/stream` → [`subscribe`] (SSE stream of `TaskStatusUpdate`s)
/// - `tasks/get` / `tasks/cancel` / `tasks/resubscribe` → the stateful task surface (A-54/55/56)
/// - `tasks/pushNotificationConfig/*` → per-task webhooks (A-57)
pub async fn a2a_handler(
    State(engine): State<Shared>,
    State(auth): State<Arc<crate::ServerAuth>>,
    State(turn_gate): State<TurnGate>,
    State(tasks): State<Arc<TaskRegistry>>,
    State(a2a_ttl): State<A2aTtl>,
    ctx: Option<axum::Extension<flux_auth::request::AuthContext>>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if req.jsonrpc != "2.0" {
        return rpc_err(req.id, -32600, "jsonrpc must be \"2.0\"").into_response();
    }
    let ctx = ctx.map(|e| e.0);
    dispatch_rpc(
        engine,
        auth,
        turn_gate,
        tasks,
        String::new(),
        a2a_ttl,
        ctx,
        req,
    )
    .await
}

// ── message/send ──────────────────────────────────────────────────────────────

/// `message/send`, branched on the client's `configuration.blocking` (A-54). The A2A spec default
/// is **non-blocking**: absent/`false` returns a `submitted` task immediately and runs the turn in
/// the background; `blocking: true` keeps the synchronous run-to-completion behavior.
#[allow(clippy::too_many_arguments)]
async fn send(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
    registry: Arc<TaskRegistry>,
    scope: String,
    ttl: A2aTtl,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Option<Value>,
) -> Json<Value> {
    let params = match params {
        Some(p) => p,
        None => return rpc_err(id, -32602, "Missing params"),
    };
    let input = match server::extract_input(&params) {
        Ok(t) => t,
        Err(code) => return rpc_err(id, code, "Message has no usable text or data part"),
    };
    if server::blocking_requested(&params) {
        send_blocking(
            engine, auth, turn_gate, registry, scope, ttl, ctx, id, params, input,
        )
        .await
    } else {
        send_nonblocking(
            engine, auth, turn_gate, registry, scope, ttl, ctx, id, params, input,
        )
        .await
    }
}

/// Report a [`MintError`] as a JSON-RPC error response.
fn mint_err(id: Option<Value>, e: MintError) -> Json<Value> {
    match e {
        MintError::AlreadyRunning(sid) => rpc_err(
            id,
            -32603,
            format!("a task is already running in this context (task {sid}); poll tasks/get"),
        ),
        MintError::Store(e) => rpc_err(id, -32603, format!("Session error: {e}")),
    }
}

/// The blocking fast path — today's synchronous behavior, preserved: run the turn to completion
/// under the gate and return the finished `Task` (A-52 history, answer in `status.message`). New
/// in A-54: the run is registered while in flight (so it is sweep-protected, cancelable, and
/// resubscribable like any other live task) and runs on the cancellable path.
#[allow(clippy::too_many_arguments)]
async fn send_blocking(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
    registry: Arc<TaskRegistry>,
    scope: String,
    ttl: A2aTtl,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Value,
    input: String,
) -> Json<Value> {
    let requested_context = server::extract_context_id(&params);
    // Acquire the gate BEFORE minting (C-29) — the identity swap + realm derivation happen
    // through `enter_turn` under the same gate hold: the realm used for continuity keying is
    // obtainable only from the function that also sets the executor identity (D-69 coupling).
    let _turn = turn_gate.lock().await;
    let realm = match crate::enter_turn(&auth, &engine, ctx.as_ref(), &_turn) {
        Ok(r) => r,
        // Constant text: unreachable behind `require_auth`, fail-closed if a mount forgets it.
        Err(_) => return rpc_err(id, -32603, crate::UNAUTHORIZED_BODY),
    };
    let task = match mint_and_register(
        &registry,
        &scope,
        &engine,
        ttl,
        requested_context.as_deref(),
        realm.as_deref(),
        TaskState::Working,
        CancellationToken::new(),
    ) {
        Ok(t) => t,
        Err(e) => return mint_err(id, e),
    };
    let mut sink = Collect::default();
    let result = engine
        .run_turn_cancellable(&task.session_id, &input, &mut sink, &task.cancel)
        .await;
    // Publish the terminal transition for any resubscriber/webhook, then release the entry
    // BEFORE the gate drops (a queued follow-up on this context must not collide with it).
    if let Err(e) = result {
        publish_transition(
            &registry,
            &scope,
            &task.session_id,
            &task.context_id,
            TaskState::Failed,
            Some(Message::agent_text(e.to_string())),
            true,
        );
        registry.finish(&scope, &task.session_id);
        return rpc_err(id, -32603, format!("Agent error: {e}"));
    }
    // A cancel that landed mid-run (A-55) surfaces as a canceled task, not a completed one.
    let (state, message) = if task.cancel.is_cancelled() {
        (TaskState::Canceled, None)
    } else {
        (TaskState::Completed, Some(Message::agent_text(sink.text)))
    };
    publish_transition(
        &registry,
        &scope,
        &task.session_id,
        &task.context_id,
        state,
        None,
        true,
    );
    registry.finish(&scope, &task.session_id);
    // A-52: return the conversation so far as `Task.history`, capped to the client's
    // `historyLength` when set.
    let history = a2a_history(&engine, &task.session_id, server::history_length(&params));
    let status = TaskStatus::new(state, message, Some(server::now_rfc3339()));
    let mut out = Task::new(task.session_id, Some(task.context_id), status);
    out.history = history;
    match serde_json::to_value(&out) {
        Ok(v) => rpc_json(id, v),
        Err(e) => rpc_err(id, -32603, format!("encode error: {e}")),
    }
}

/// The non-blocking path (A-54): answer `submitted` + the task id immediately and drive the turn
/// on a background task. The client advances via `tasks/get` (poll) or `tasks/resubscribe`
/// (stream). Realm comes from [`crate::caller_realm`] — the D-69 identity swap is deferred to the
/// background task's gate-held [`crate::enter_turn`], so no turn ever runs under the service
/// identity.
#[allow(clippy::too_many_arguments)]
async fn send_nonblocking(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
    registry: Arc<TaskRegistry>,
    scope: String,
    ttl: A2aTtl,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Value,
    input: String,
) -> Json<Value> {
    let requested_context = server::extract_context_id(&params);
    let Ok(realm) = crate::caller_realm(&auth, ctx.as_ref()) else {
        return rpc_err(id, -32603, crate::UNAUTHORIZED_BODY);
    };
    let task = match mint_and_register(
        &registry,
        &scope,
        &engine,
        ttl,
        requested_context.as_deref(),
        realm.as_deref(),
        TaskState::Submitted,
        CancellationToken::new(),
    ) {
        Ok(t) => t,
        Err(e) => return mint_err(id, e),
    };
    let status = TaskStatus::new(TaskState::Submitted, None, Some(server::now_rfc3339()));
    let submitted = Task::new(
        task.session_id.clone(),
        Some(task.context_id.clone()),
        status,
    );
    let response = match serde_json::to_value(&submitted) {
        Ok(v) => rpc_json(id, v),
        Err(e) => {
            registry.finish(&scope, &task.session_id);
            return rpc_err(id, -32603, format!("encode error: {e}"));
        }
    };
    tokio::spawn(run_background(
        engine, auth, turn_gate, registry, scope, task, input, ctx,
    ));
    response
}

/// Drive one non-blocking send to its terminal state: queue on the single-turn gate, swap the
/// executor identity ([`crate::enter_turn`] — the D-69 swap deferred from mint), advance
/// `submitted → working`, run the cancellable turn, publish the terminal transition, and release
/// the registry entry (before the gate, so a queued follow-up never collides with it).
#[allow(clippy::too_many_arguments)]
async fn run_background(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
    registry: Arc<TaskRegistry>,
    scope: String,
    task: RegisteredTask,
    input: String,
    ctx: Option<flux_auth::request::AuthContext>,
) {
    let _turn = turn_gate.lock().await;
    // Fail-closed belt+braces: `caller_realm` already vetted the context at mint.
    if crate::enter_turn(&auth, &engine, ctx.as_ref(), &_turn).is_err() {
        publish_transition(
            &registry,
            &scope,
            &task.session_id,
            &task.context_id,
            TaskState::Failed,
            Some(Message::agent_text(crate::UNAUTHORIZED_BODY)),
            true,
        );
        registry.finish(&scope, &task.session_id);
        return;
    }
    // A cancel can land while the task is still queued (A-55): keep the registry state
    // `canceled` — the engine's first cancellation check ends the run immediately either way.
    if !task.cancel.is_cancelled() {
        registry.set_state(&scope, &task.session_id, TaskState::Working);
        publish_transition(
            &registry,
            &scope,
            &task.session_id,
            &task.context_id,
            TaskState::Working,
            None,
            false,
        );
    }
    let mut sink = BroadcastSink {
        registry: registry.clone(),
        scope: scope.clone(),
        task_id: task.session_id.clone(),
        context_id: task.context_id.clone(),
    };
    let result = engine
        .run_turn_cancellable(&task.session_id, &input, &mut sink, &task.cancel)
        .await;
    // The final frame carries no message on success — the deltas already broadcast are
    // authoritative, and the answer is durable in the task projection (`tasks/get`).
    let (state, message) = if task.cancel.is_cancelled() {
        (TaskState::Canceled, None)
    } else {
        match result {
            Ok(()) => (TaskState::Completed, None),
            Err(e) => (TaskState::Failed, Some(Message::agent_text(e.to_string()))),
        }
    };
    publish_transition(
        &registry,
        &scope,
        &task.session_id,
        &task.context_id,
        state,
        message,
        true,
    );
    registry.finish(&scope, &task.session_id);
}

/// Streams a background run's text deltas as `working` frames to any live subscriber (A-56).
/// Unlike [`StreamSink`], a zero-subscriber send is NOT a disconnect — the task's owner is the
/// registry, not a socket — so nothing cancels; unobserved frames are simply dropped. Deltas
/// broadcast directly (never through [`publish_transition`]), so they are never pushed to
/// webhooks — a POST per token would hammer the receiver.
struct BroadcastSink {
    registry: Arc<TaskRegistry>,
    scope: String,
    task_id: String,
    context_id: String,
}

impl AgentSink for BroadcastSink {
    fn text_delta(&mut self, t: &str) {
        let frame = server::status_update_value(
            &self.task_id,
            &self.context_id,
            TaskState::Working,
            Some(Message::agent_text(t)),
            false,
        );
        self.registry.broadcast(&self.scope, &self.task_id, frame);
    }
}

/// Publish a status **transition** frame: broadcast to resubscribers (A-56) and fan out to any
/// registered push webhooks (A-57). Per-token deltas never come through here (see
/// [`BroadcastSink`]).
fn publish_transition(
    registry: &Arc<TaskRegistry>,
    scope: &str,
    task_id: &str,
    context_id: &str,
    state: TaskState,
    message: Option<Message>,
    is_final: bool,
) {
    let frame = server::status_update_value(task_id, context_id, state, message, is_final);
    registry.broadcast(scope, task_id, frame.clone());
    deliver_push(registry, scope, task_id, &frame);
}

/// Build the A2A `Task.history` for a completed turn from the engine's conversation projection
/// (A-52): the session's user/agent messages as A2A [`Message`]s, capped to the most-recent
/// `limit` when the client set `configuration.historyLength`. System messages (the agent's own
/// prompt) and text-less turns (a pure tool-call round) are omitted — history is the
/// caller-visible conversation, not flux's internals. A projection read failure degrades to empty
/// history rather than failing the (already-successful) turn.
fn a2a_history(engine: &Shared, session_id: &str, limit: Option<usize>) -> Vec<Message> {
    let mut msgs: Vec<Message> = match engine.events.conversation(session_id) {
        Ok(convo) => convo.into_iter().filter_map(to_a2a_message).collect(),
        Err(e) => {
            eprintln!("(a2a: history projection failed for {session_id}: {e})");
            Vec::new()
        }
    };
    if let Some(n) = limit {
        if msgs.len() > n {
            msgs.drain(0..msgs.len() - n); // keep the most-recent `n`
        }
    }
    msgs
}

/// Convert one projected conversation message to an A2A [`Message`], or `None` to drop it from
/// history (a system message, or a message with no text — e.g. a pure tool-call turn).
fn to_a2a_message(m: flux_core::Message) -> Option<Message> {
    let text = m.text();
    if text.is_empty() {
        return None;
    }
    match m.role {
        flux_core::Role::User => Some(Message::user_text(text, None)),
        flux_core::Role::Assistant => Some(Message::agent_text(text)),
        flux_core::Role::System => None,
    }
}

// ── Task projection (A-54) ────────────────────────────────────────────────────

/// Project the current [`Task`] for `task_id`, realm-scoped: a live in-process task answers from
/// the registry; otherwise the retained event log is folded ([`project_stored_task`]). `Err`
/// carries the JSON-RPC error code: `-32001` for unknown/cross-realm/non-A2A ids (one constant
/// answer — existence is never distinguishable), `-32603` for a store read failure.
fn project_task(
    engine: &Shared,
    registry: &TaskRegistry,
    scope: &str,
    task_id: &str,
    realm: Option<&str>,
    history_limit: Option<usize>,
) -> Result<Task, i32> {
    if let Some((state, context_id)) = registry.snapshot(scope, task_id, realm) {
        let status = TaskStatus::new(state, None, Some(server::now_rfc3339()));
        let mut task = Task::new(task_id.to_string(), Some(context_id), status);
        task.history = a2a_history(engine, task_id, history_limit);
        return Ok(task);
    }
    project_stored_task(engine, task_id, realm, history_limit)
}

/// Fold a retained A2A session's events into its terminal (or last-known) [`Task`] — the
/// "task-as-projection" half of the A-53 design: no second store, reconstructable for as long as
/// the stream is retained, realm-scoped like every A2A lookup.
///
/// The state folds over the engine's own turn-lifecycle events:
/// - no `turn_started` at all → `submitted` (minted, never ran — e.g. the process died before a
///   queued run started);
/// - a `turn_started` newer than the last `turn_ended` → `working` — with no in-process registry
///   entry this means "in flight on another replica" (shared-store deployments) or "crashed
///   mid-turn"; the optimistic answer keeps cross-replica polling truthful, and a crashed task's
///   session ages out via the TTL sweep rather than reporting a false `failed`;
/// - otherwise the last `turn_ended.outcome` decides: `cancelled` → canceled, `error` → failed,
///   anything else (`ok`, `max_iter`) → completed — with the recorded answer as the status
///   message and the event's own timestamp.
fn project_stored_task(
    engine: &Shared,
    task_id: &str,
    realm: Option<&str>,
    history_limit: Option<usize>,
) -> Result<Task, i32> {
    let Ok(info) = engine.events.info(task_id) else {
        return Err(error::TASK_NOT_FOUND);
    };
    // Only A2A-minted sessions are addressable tasks: session ids are guessable (`s_<n>`), so a
    // CLI/TUI session must not be readable through the task surface.
    if info.context.agent_id.as_deref() != Some(A2A_AGENT_ID) {
        return Err(error::TASK_NOT_FOUND);
    }
    // Realm scoping (D-69): cross-realm is the same constant answer as unknown. The open/
    // shared-secret modes (`realm == None`) are single-tenant — every A2A session is theirs.
    if let Some(realm) = realm {
        if info.context.account.as_deref() != Some(realm) {
            return Err(error::TASK_NOT_FOUND);
        }
    }
    let context_id = info
        .context
        .correlation_id
        .clone()
        .unwrap_or_else(|| task_id.to_string());
    let started = engine
        .events
        .load_by_kind(task_id, "turn_started")
        .map_err(|_| -32603)?;
    let ended = engine
        .events
        .load_by_kind(task_id, "turn_ended")
        .map_err(|_| -32603)?;
    let last_start = started.last().map(|e| e.stream_seq).unwrap_or(-1);
    let (state, message, ts_ms) = match ended.last() {
        Some(end) if end.stream_seq > last_start => {
            let EventKind::TurnEnded {
                outcome, answer, ..
            } = &end.kind
            else {
                return Err(-32603);
            };
            let state = match outcome.as_str() {
                "cancelled" => TaskState::Canceled,
                "error" => TaskState::Failed,
                _ => TaskState::Completed,
            };
            let message = (!answer.is_empty()).then(|| Message::agent_text(answer.clone()));
            (state, message, end.ts_ms)
        }
        _ if last_start >= 0 => (TaskState::Working, None, info.updated_at_ms),
        _ => (TaskState::Submitted, None, info.updated_at_ms),
    };
    let status = TaskStatus::new(state, message, Some(server::rfc3339_ms(ts_ms)));
    let mut task = Task::new(task_id.to_string(), Some(context_id), status);
    task.history = a2a_history(engine, task_id, history_limit);
    Ok(task)
}

// ── tasks/get · tasks/cancel · tasks/resubscribe (A-54/A-55/A-56) ────────────

/// Validate a `tasks/*` request: the task id from `params.id` plus the caller's realm (no
/// identity swap — these operations never run a turn).
fn task_request(
    auth: &crate::ServerAuth,
    ctx: &Option<flux_auth::request::AuthContext>,
    params: &Option<Value>,
) -> Result<(String, Option<String>), (i32, String)> {
    let Some(params) = params else {
        return Err((-32602, "Missing params".to_string()));
    };
    let Some(task_id) = server::extract_task_id(params) else {
        return Err((-32602, "Missing task id".to_string()));
    };
    let Ok(realm) = crate::caller_realm(auth, ctx.as_ref()) else {
        return Err((-32603, crate::UNAUTHORIZED_BODY.to_string()));
    };
    Ok((task_id, realm))
}

/// `tasks/get` (A-54): resolve a task id to its current [`Task`] within the caller's realm.
async fn tasks_get(
    engine: Shared,
    registry: Arc<TaskRegistry>,
    scope: String,
    auth: Arc<crate::ServerAuth>,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Option<Value>,
) -> Json<Value> {
    let (task_id, realm) = match task_request(&auth, &ctx, &params) {
        Ok(x) => x,
        Err((code, msg)) => return rpc_err(id, code, msg),
    };
    let limit = params.as_ref().and_then(server::history_length);
    match project_task(
        &engine,
        &registry,
        &scope,
        &task_id,
        realm.as_deref(),
        limit,
    ) {
        Ok(task) => match serde_json::to_value(&task) {
            Ok(v) => rpc_json(id, v),
            Err(e) => rpc_err(id, -32603, format!("encode error: {e}")),
        },
        Err(error::TASK_NOT_FOUND) => rpc_err(id, error::TASK_NOT_FOUND, "Task not found"),
        Err(code) => rpc_err(id, code, "task state unavailable"),
    }
}

/// `tasks/cancel` (A-55): fire a live task's `CancellationToken` from an out-of-band request —
/// the same token an SSE disconnect fires, generalized. The run observes it between plan rounds
/// (`run_turn_cancellable`) and records the durable `cancelled` turn event; the response reflects
/// the requested state immediately. A task with no live run answers `-32002 TaskNotCancelable`
/// (terminal, or running on another replica — only in-process runs are cancelable); an
/// unknown/cross-realm id answers `-32001`.
async fn tasks_cancel(
    engine: Shared,
    registry: Arc<TaskRegistry>,
    scope: String,
    auth: Arc<crate::ServerAuth>,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Option<Value>,
) -> Json<Value> {
    let (task_id, realm) = match task_request(&auth, &ctx, &params) {
        Ok(x) => x,
        Err((code, msg)) => return rpc_err(id, code, msg),
    };
    match registry.request_cancel(&scope, &task_id, realm.as_deref()) {
        CancelHit::Cancelled(context_id) => {
            let status = TaskStatus::new(TaskState::Canceled, None, Some(server::now_rfc3339()));
            let mut task = Task::new(task_id.clone(), Some(context_id), status);
            task.history = a2a_history(&engine, &task_id, None);
            return match serde_json::to_value(&task) {
                Ok(v) => rpc_json(id, v),
                Err(e) => rpc_err(id, -32603, format!("encode error: {e}")),
            };
        }
        CancelHit::AlreadyCanceled => {
            return rpc_err(
                id,
                error::TASK_NOT_CANCELABLE,
                "task is already in a terminal state",
            );
        }
        CancelHit::NotLive => {}
    }
    match project_task(&engine, &registry, &scope, &task_id, realm.as_deref(), None) {
        Ok(t) if t.status.state.is_terminal() => rpc_err(
            id,
            error::TASK_NOT_CANCELABLE,
            "task is already in a terminal state",
        ),
        Ok(_) => rpc_err(
            id,
            error::TASK_NOT_CANCELABLE,
            "task has no live run on this instance",
        ),
        Err(error::TASK_NOT_FOUND) => rpc_err(id, error::TASK_NOT_FOUND, "Task not found"),
        Err(code) => rpc_err(id, code, "task state unavailable"),
    }
}

/// The boxed SSE type `tasks/resubscribe` returns — its two arms (live follow vs. terminal
/// replay) build different stream shapes. axum 0.8's `keep_alive` wraps the stream in
/// `KeepAliveStream`, so the wrapper is part of the named type now.
type BoxedSse = Sse<KeepAliveStream<Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>>>;

/// `tasks/resubscribe` (A-56): re-attach an SSE stream to a task. A **live** task yields a
/// snapshot frame of its current state and then follows the run's broadcast to the terminal
/// frame — the same framing as `message/stream`, wrapped in THIS request's JSON-RPC id. A
/// **retained terminal** task yields its final state as one frame and closes. Unknown/cross-realm
/// → `-32001` before the SSE is established.
///
/// A resubscriber is an observer, not the owner: dropping this stream cancels nothing (unlike
/// `message/stream`, whose disconnect drop-guard cancels the turn it owns).
async fn tasks_resubscribe(
    engine: Shared,
    registry: Arc<TaskRegistry>,
    scope: String,
    auth: Arc<crate::ServerAuth>,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Option<Value>,
) -> Result<BoxedSse, (i32, String)> {
    let (task_id, realm) = task_request(&auth, &ctx, &params)?;
    if let Some((state, context_id, mut rx)) =
        registry.subscribe(&scope, &task_id, realm.as_deref())
    {
        // Subscribed-then-snapshotted under one registry lock: no frame falls in between. A
        // repeated `working` frame (snapshot + a broadcast transition) is harmless per spec.
        let snapshot = server::status_update_value(&task_id, &context_id, state, None, false);
        let req_id = id.clone();
        let stream = async_stream::stream! {
            yield Ok(rpc_frame(&req_id, snapshot));
            loop {
                match rx.recv().await {
                    Ok(frame) => {
                        let is_final = frame
                            .get("final")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        yield Ok(rpc_frame(&req_id, frame));
                        if is_final {
                            break;
                        }
                    }
                    // Lagged: a slow consumer missed some deltas — status transitions are rare
                    // and re-derivable via tasks/get, so skip forward rather than erroring.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        return Ok(Sse::new(Box::pin(stream) as _).keep_alive(KeepAlive::default()));
    }
    match project_stored_task(&engine, &task_id, realm.as_deref(), None) {
        Ok(task) => {
            let frame = server::status_update_value(
                &task_id,
                task.context_id.as_deref().unwrap_or(&task_id),
                task.status.state,
                task.status.message.clone(),
                true,
            );
            let req_id = id.clone();
            let stream = async_stream::stream! {
                yield Ok(rpc_frame(&req_id, frame));
            };
            Ok(Sse::new(Box::pin(stream) as _).keep_alive(KeepAlive::default()))
        }
        Err(error::TASK_NOT_FOUND) => Err((error::TASK_NOT_FOUND, "Task not found".to_string())),
        Err(code) => Err((code, "task state unavailable".to_string())),
    }
}

/// Wrap a broadcast `result` value in a JSON-RPC SSE frame carrying `req_id` — each subscriber
/// wraps the shared frames in its OWN request id.
fn rpc_frame(req_id: &Option<Value>, result: Value) -> Event {
    Event::default().data(json!({ "jsonrpc": "2.0", "id": req_id, "result": result }).to_string())
}

// ── Push notifications (A-57) ─────────────────────────────────────────────────

/// `tasks/pushNotificationConfig/{set,get,list,delete}` (A-57): a per-task webhook registration,
/// realm-scoped like every task operation. Configs are held in-process beside the live-task map —
/// delivery only happens for in-process runs, so durability beyond the process buys nothing.
///
/// `set` params: `{ taskId, pushNotificationConfig: { url, token?, id? } }` (the spec shape; a
/// plain `id` is accepted for the task id too). The others take `{ id, pushNotificationConfigId? }`.
#[allow(clippy::too_many_arguments)]
async fn push_config(
    engine: Shared,
    registry: Arc<TaskRegistry>,
    scope: String,
    auth: Arc<crate::ServerAuth>,
    ctx: Option<flux_auth::request::AuthContext>,
    method: &str,
    id: Option<Value>,
    params: Option<Value>,
) -> Json<Value> {
    let Some(params) = params else {
        return rpc_err(id, -32602, "Missing params");
    };
    // `set` addresses the task as `taskId` (its params ARE a TaskPushNotificationConfig);
    // get/list/delete use `id`.
    let task_id = params
        .get("taskId")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| server::extract_task_id(&params));
    let Some(task_id) = task_id else {
        return rpc_err(id, -32602, "Missing task id");
    };
    let Ok(realm) = crate::caller_realm(&auth, ctx.as_ref()) else {
        return rpc_err(id, -32603, crate::UNAUTHORIZED_BODY);
    };
    // The task must resolve within the caller's realm before any config surface is touched.
    if let Err(code) = project_task(&engine, &registry, &scope, &task_id, realm.as_deref(), None) {
        let msg = if code == error::TASK_NOT_FOUND {
            "Task not found"
        } else {
            "task state unavailable"
        };
        return rpc_err(id, code, msg);
    }
    let key = (scope.clone(), task_id.clone());
    match method {
        "tasks/pushNotificationConfig/set" => {
            let Some(cfg) = params.get("pushNotificationConfig") else {
                return rpc_err(id, -32602, "Missing pushNotificationConfig");
            };
            let Some(url) = cfg.get("url").and_then(Value::as_str) else {
                return rpc_err(id, -32602, "pushNotificationConfig.url is required");
            };
            if !push_url_allowed(url) {
                // -32003: this server does not push to that destination (scheme/host policy).
                return rpc_err(
                    id,
                    error::PUSH_NOTIFICATION_NOT_SUPPORTED,
                    "push URL not supported: only public http(s) endpoints are allowed",
                );
            }
            // The config id defaults to its URL (the spec lets servers key configs that way).
            let mut cfg = cfg.clone();
            if cfg.get("id").is_none() {
                cfg["id"] = Value::String(url.to_string());
            }
            let cfg_id = cfg.get("id").cloned();
            let mut push = registry.push.lock().unwrap();
            let entry = push.entry(key).or_default();
            entry.retain(|c| c.get("id") != cfg_id.as_ref());
            entry.push(cfg.clone());
            rpc_json(
                id,
                json!({ "taskId": task_id, "pushNotificationConfig": cfg }),
            )
        }
        "tasks/pushNotificationConfig/get" => {
            let wanted = params.get("pushNotificationConfigId");
            let push = registry.push.lock().unwrap();
            let found = push.get(&key).and_then(|cfgs| match wanted {
                Some(w) => cfgs.iter().find(|c| c.get("id") == Some(w)).cloned(),
                None => cfgs.first().cloned(),
            });
            match found {
                Some(cfg) => rpc_json(
                    id,
                    json!({ "taskId": task_id, "pushNotificationConfig": cfg }),
                ),
                None => rpc_err(
                    id,
                    error::TASK_NOT_FOUND,
                    "no push-notification config for this task",
                ),
            }
        }
        "tasks/pushNotificationConfig/list" => {
            let push = registry.push.lock().unwrap();
            let list: Vec<Value> = push
                .get(&key)
                .map(|cfgs| {
                    cfgs.iter()
                        .map(|c| json!({ "taskId": task_id, "pushNotificationConfig": c }))
                        .collect()
                })
                .unwrap_or_default();
            rpc_json(id, Value::Array(list))
        }
        "tasks/pushNotificationConfig/delete" => {
            let wanted = params.get("pushNotificationConfigId");
            let mut push = registry.push.lock().unwrap();
            if let Some(cfgs) = push.get_mut(&key) {
                match wanted {
                    Some(w) => cfgs.retain(|c| c.get("id") != Some(w)),
                    None => cfgs.clear(),
                }
                if cfgs.is_empty() {
                    push.remove(&key);
                }
            }
            rpc_json(id, Value::Null)
        }
        _ => rpc_err(id, -32601, format!("Method not found: {method}")),
    }
}

/// The push-destination policy (documented SSRF posture): only `http`/`https`, and never a
/// loopback, private, link-local, or unspecified **literal** address (nor `localhost`) — a
/// webhook must be a public endpoint. Resolution-time tricks (DNS rebinding) are out of scope:
/// deployments that need stronger egress guarantees should enforce them at the network layer.
/// `FLUX_A2A_PUSH_ALLOW_LOCAL=1` lifts the host policy for local development and tests.
fn push_url_allowed(url: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(u.scheme(), "http" | "https") {
        return false;
    }
    if std::env::var("FLUX_A2A_PUSH_ALLOW_LOCAL").is_ok_and(|v| v == "1") {
        return true;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return false;
    }
    if let Ok(ip) = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
    {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                !(v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified())
            }
            std::net::IpAddr::V6(v6) => !(v6.is_loopback() || v6.is_unspecified()),
        };
    }
    true
}

/// Best-effort webhook delivery of one transition frame (A-57): one POST per registered config,
/// fire-and-forget on a spawned task, 10s timeout, failures logged, **no retry** (the documented
/// policy — the durable task projection is the source of truth; push is a hint to poll). A
/// config's `token` rides along as `X-A2A-Notification-Token` so receivers can authenticate the
/// caller.
fn deliver_push(registry: &Arc<TaskRegistry>, scope: &str, task_id: &str, frame: &Value) {
    let configs: Vec<Value> = registry
        .push
        .lock()
        .unwrap()
        .get(&(scope.to_string(), task_id.to_string()))
        .cloned()
        .unwrap_or_default();
    for cfg in configs {
        let Some(url) = cfg.get("url").and_then(Value::as_str).map(str::to_string) else {
            continue;
        };
        let token = cfg.get("token").and_then(Value::as_str).map(str::to_string);
        let client = registry.http.clone();
        let frame = frame.clone();
        tokio::spawn(async move {
            let mut req = client
                .post(&url)
                .timeout(std::time::Duration::from_secs(10))
                .json(&frame);
            if let Some(t) = token {
                req = req.header("X-A2A-Notification-Token", t);
            }
            match req.send().await {
                Ok(resp) if !resp.status().is_success() => {
                    eprintln!("(a2a push: {url} answered {})", resp.status());
                }
                Err(e) => eprintln!("(a2a push: delivery to {url} failed: {e})"),
                _ => {}
            }
        });
    }
}

// ── message/stream ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn subscribe(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
    registry: Arc<TaskRegistry>,
    scope: String,
    ttl: A2aTtl,
    ctx: Option<flux_auth::request::AuthContext>,
    id: Option<Value>,
    params: Option<Value>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (i32, String)> {
    let params = params.ok_or((-32602, "Missing params".to_string()))?;
    let input = match server::extract_input(&params) {
        Ok(t) => t,
        Err(code) => return Err((code, "Message has no usable text or data part".to_string())),
    };
    let requested_context = server::extract_context_id(&params);
    // Fail closed before the SSE response is established (unreachable behind `require_auth`).
    if matches!(auth.as_ref(), crate::ServerAuth::Principal(_)) && ctx.is_none() {
        return Err((-32603, crate::UNAUTHORIZED_BODY.to_string()));
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    // `drop_guard` cancels `cancel` when the SSE stream is dropped (client disconnect), which
    // propagates through `cancel_task` into `run_turn_cancellable`.
    let cancel = CancellationToken::new();
    let cancel_task = cancel.clone();
    let drop_guard = cancel.drop_guard();

    let engine_clone = engine.clone();
    tokio::spawn(async move {
        // Acquire the gate BEFORE minting (C-29) — see `create_a2a_session`'s doc for why. This
        // also delays the session id (and so `task_id`/`context_id`) until it's this task's turn,
        // which is why the mint and the initial "working" frame both live inside the gate below
        // rather than before `tokio::spawn`.
        let _turn = turn_gate.lock().await;
        // Identity swap + realm under the gate (pre-checked above; error frame is belt+braces).
        let realm = match crate::enter_turn(&auth, &engine_clone, ctx.as_ref(), &_turn) {
            Ok(r) => r,
            Err(_) => {
                let _ = tx.send(
                    Event::default().data(
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32603, "message": crate::UNAUTHORIZED_BODY },
                        })
                        .to_string(),
                    ),
                );
                return;
            }
        };
        // Mint + register under the gate (A-54: registration makes the streaming run
        // sweep-protected, cancelable by `tasks/cancel`, and observable by `tasks/resubscribe`).
        let task = match mint_and_register(
            &registry,
            &scope,
            &engine_clone,
            ttl,
            requested_context.as_deref(),
            realm.as_deref(),
            TaskState::Working,
            cancel_task.clone(),
        ) {
            Ok(t) => t,
            Err(e) => {
                // The SSE response is already established by the time minting can fail here, so
                // report it as a JSON-RPC error frame inside the stream (mirroring `rpc_err`)
                // rather than a pre-SSE HTTP error.
                let msg = match e {
                    MintError::AlreadyRunning(sid) => format!(
                        "a task is already running in this context (task {sid}); poll tasks/get"
                    ),
                    MintError::Store(e) => format!("Session error: {e}"),
                };
                let _ = tx.send(
                    Event::default().data(
                        json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32603, "message": msg },
                        })
                        .to_string(),
                    ),
                );
                return;
            }
        };
        let session_id = task.session_id.clone();
        let context_id = task.context_id.clone();
        let task_id = session_id.clone();

        // Initial "working" update so the caller knows the task started (the transition also
        // reaches resubscribers/webhooks).
        let _ = tx.send(status_frame(
            &id,
            &task_id,
            &context_id,
            TaskState::Working,
            None,
            false,
        ));
        publish_transition(
            &registry,
            &scope,
            &task_id,
            &context_id,
            TaskState::Working,
            None,
            false,
        );
        let mut sink = StreamSink {
            tx: tx.clone(),
            id: id.clone(),
            task_id: task_id.clone(),
            context_id: context_id.clone(),
            cancel: cancel_task.clone(),
            registry: registry.clone(),
            scope: scope.clone(),
        };
        let result = engine_clone
            .run_turn_cancellable(&session_id, &input, &mut sink, &cancel_task)
            .await;
        // The terminal state: a disconnect-cancelled run is `canceled`, otherwise the run's own
        // outcome. The final event carries no message on success — the deltas already streamed
        // are authoritative; on failure it carries the error text.
        let (state, message) = if cancel_task.is_cancelled() {
            (TaskState::Canceled, None)
        } else {
            match result {
                Ok(()) => (TaskState::Completed, None),
                Err(e) => (TaskState::Failed, Some(Message::agent_text(e.to_string()))),
            }
        };
        // If the client disconnected mid-stream, skip its final event — nobody is listening —
        // but the transition still reaches resubscribers and webhooks.
        if !cancel_task.is_cancelled() {
            let _ = tx.send(status_frame(
                &id,
                &task_id,
                &context_id,
                state,
                message.clone(),
                true,
            ));
        }
        publish_transition(
            &registry,
            &scope,
            &task_id,
            &context_id,
            state,
            message,
            true,
        );
        // Release the entry before the gate drops (a queued follow-up must not collide with it).
        registry.finish(&scope, &task_id);
        // `tx` (and sink.tx clone) drop here → channel closes → stream ends.
    });

    let stream = async_stream::stream! {
        // Keep the drop guard alive for the stream's lifetime: when axum drops the SSE response
        // (TCP disconnect), `_guard` fires and cancels the in-flight turn.
        let _guard = drop_guard;
        while let Some(ev) = rx.recv().await {
            yield Ok(ev);
        }
    };
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

/// Streams text deltas back as SSE `working` status updates. Each delta is an incremental
/// status-update message; the final `completed` event (sent by the spawner) carries no message.
/// Deltas also broadcast to the task's registry entry (A-56), so a resubscriber of a streaming
/// task observes the same frames as the owning stream.
struct StreamSink {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
    /// The originating JSON-RPC request id, echoed in every frame.
    id: Option<Value>,
    task_id: String,
    context_id: String,
    /// Cancelled when the SSE receiver is dropped (client disconnect); checked between plan rounds
    /// by `run_turn_cancellable`.
    cancel: CancellationToken,
    registry: Arc<TaskRegistry>,
    scope: String,
}

impl AgentSink for StreamSink {
    fn text_delta(&mut self, t: &str) {
        // Send only the delta in working events; sending the full accumulated text on every token
        // would be O(N²) in response length.
        let result = server::status_update_value(
            &self.task_id,
            &self.context_id,
            TaskState::Working,
            Some(Message::agent_text(t)),
            false,
        );
        // Broadcast to resubscribers (zero subscribers is normal — never a disconnect signal).
        self.registry
            .broadcast(&self.scope, &self.task_id, result.clone());
        let frame = Event::default()
            .data(json!({ "jsonrpc": "2.0", "id": self.id, "result": result }).to_string());
        if self.tx.send(frame).is_err() {
            // Receiver gone — client disconnected; stop doing work as soon as possible.
            self.cancel.cancel();
        }
    }
}

// The text/contextId extraction, the agent card, the RFC-3339 timestamp, and the status-update
// shaping now live in `flux_a2a::server` (shared with other A2A surfaces) and are unit-tested there.

#[cfg(test)]
mod tests {
    use super::*;
    use flux_flow::engine::FlowEngine;

    /// A provider that declares an intent, then answers with one word. (Mirrors
    /// `crate::tests::ProseProvider`; duplicated locally so this module stays self-contained.)
    struct ProseProvider;
    #[async_trait::async_trait]
    impl flux_provider::Provider for ProseProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(
            &self,
            req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            let chunks = if req.tools.iter().any(|tool| tool.name == "declare_intent") {
                vec![
                    flux_core::Chunk::Block(flux_core::ContentBlock::ToolUse {
                        id: "intent".into(),
                        name: "declare_intent".into(),
                        input: serde_json::json!({
                            "intent": "answer the current message",
                            "capability_families": [],
                        }),
                    }),
                    flux_core::Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::ToolUse),
                    },
                ]
            } else {
                vec![
                    flux_core::Chunk::TextDelta("ok".into()),
                    flux_core::Chunk::Done {
                        stop_reason: Some(flux_core::StopReason::EndTurn),
                    },
                ]
            };
            Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
        }
    }

    /// A minimal `FlowEngine` for the test below: real machinery, but the only turn ever run is a
    /// one-word prose completion (`ProseProvider`), so it finishes fast and deterministically.
    fn test_engine() -> (Shared, Arc<flux_events::EventStore>) {
        let dir =
            std::env::temp_dir().join(format!("flux-server-a2a-c29-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(flux_system::System::new(
            flux_system::Workspace::new(&dir).unwrap(),
        ));
        let mut registry = flux_runtime::ToolRegistry::new();
        flux_tools::register_reflect(&mut registry);
        flux_tools::register_evidence(&mut registry);
        let executor = flux_runtime::Executor::new(
            registry,
            flux_runtime::PermissionManager::from_rules(&[], &[]),
            Arc::new(flux_runtime::AllowApprover),
            flux_runtime::ToolContext::new(system),
        );
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone()).unwrap();
        let engine = FlowEngine::assemble(
            Arc::new(ProseProvider),
            executor,
            events.clone(),
            flow,
            "claude-sonnet-4-6".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            dir,
        )
        .unwrap();
        (Arc::new(engine), events)
    }

    /// A minimal `message/send` params payload with the given text.
    fn send_params(text: &str) -> Value {
        json!({ "message": { "parts": [{ "kind": "text", "text": text }] } })
    }

    /// C-29 failing-first test: `send`/`subscribe` used to mint a session (running the lazy TTL
    /// sweep) *before* acquiring the single-turn `turn_gate`, and a session's `updated_at` is
    /// frozen at mint until its own `run_turn` first records something. So a session minted just
    /// ahead of a long turn already holding the gate could sit queued long enough to look expired
    /// to a *different*, concurrent request's mint-time sweep — which prunes it out from under the
    /// queue. The request whose turn it was still runs to completion (there is no FK from `events`
    /// back to `streams`, so nothing errors), but the session's registry row is gone: every future
    /// append becomes an orphaned event row, and its spend drops out of the usage rollups.
    ///
    /// This drives the real `send` handler for the queued request (so it mints exactly where the
    /// production code does — pre-fix: before the gate; post-fix: after it) against the real
    /// `create_a2a_session` mint path with a real wall-clock TTL crossed by a real sleep, and
    /// asserts the queued session survives a concurrent request's mint-time sweep.
    #[tokio::test]
    async fn queued_session_survives_concurrent_sweep_while_gate_held() {
        let (engine, events) = test_engine();
        let ttl = A2aTtl(1); // 1s — small enough to cross for real within the test.
        let turn_gate: TurnGate = Arc::new(tokio::sync::Mutex::new(()));

        // Simulate a long turn already in flight: something else holds the single-turn gate.
        let held = turn_gate.clone().lock_owned().await;

        // Fire request X through the real `send` handler — a BLOCKING send (the path whose C-29
        // protection is the gate-held mint). It must queue behind the held gate before its turn
        // can run.
        let registry = Arc::new(TaskRegistry::default());
        let engine_x = engine.clone();
        let gate_x = turn_gate.clone();
        let registry_x = registry.clone();
        let x_task = tokio::spawn(async move {
            let mut params = send_params("hi");
            params["configuration"] = json!({ "blocking": true });
            send(
                engine_x,
                Arc::new(crate::ServerAuth::Open),
                gate_x,
                registry_x,
                String::new(),
                ttl,
                None,
                Some(json!(1)),
                Some(params),
            )
            .await
        });
        // Give X's task a chance to actually reach (and block on) the gate before time moves past
        // the TTL below.
        tokio::task::yield_now().await;

        // Advance the store clock past the TTL for real.
        tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

        // Request Y arrives while X is still queued: its mint runs the lazy TTL sweep — the exact
        // moment the bug prunes a queued session out from under the queue.
        let _session_y = mint_and_register(
            &registry,
            "",
            &engine,
            ttl,
            None,
            None,
            TaskState::Submitted,
            CancellationToken::new(),
        )
        .map_err(|_| "mint failed")
        .unwrap();

        // Release the gate: X's queued turn can finally run.
        drop(held);
        let response = x_task.await.expect("X's task must not panic").0;
        let session_x = response["result"]["id"]
            .as_str()
            .expect("send returns a Task with an `id`")
            .to_string();

        // X must still be a live, correctly-tagged session — not orphaned by Y's concurrent sweep.
        // (Do NOT assert the request failed — it does not; the bug is silent data loss, not an
        // error.)
        let info = events.info(&session_x).expect(
            "a session queued behind the turn gate must survive a concurrent request's sweep",
        );
        assert_eq!(info.context.agent_id.as_deref(), Some(A2A_AGENT_ID));
    }
}
