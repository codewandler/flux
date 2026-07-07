//! A2A (Agent-to-Agent) protocol support for `flux-server`.
//!
//! Adds three routes to the router:
//!   `GET  /.well-known/agent-card.json` — agent discovery card (A2A spec)
//!   `GET  /.well-known/agent.json`      — legacy discovery alias (same card)
//!   `POST /a2a`                         — JSON-RPC 2.0 dispatcher
//!
//! Supported methods (current A2A spec):
//! - `message/send`   — run one flux turn, return the resulting `Task` synchronously
//! - `message/stream` — run one flux turn, stream `TaskStatusUpdate` events as Server-Sent Events
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

use std::convert::Infallible;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_a2a::{error, server};
use flux_a2a::{AgentCard, Message, Task, TaskState, TaskStatus};
use flux_flow::AgentSink;

use std::sync::Arc;

use super::Collect;
use crate::{A2aTtl, AgentResolver, CardInfo, Shared, TurnGate};

// ── A2A session lifecycle (C-18) ─────────────────────────────────────────────

/// The `agent_id` tag stamped on every session minted by the A2A surface (the D-02 context
/// envelope — the same convention as flux-orchestrate's `subagent:<role>` streams). TTL pruning
/// is scoped to exactly this tag, so a CLI/TUI session (empty context) is never eligible.
pub(crate) const A2A_AGENT_ID: &str = "a2a";

/// Resolve the session for one A2A task: sweep expired A2A sessions first (the lazy TTL pass),
/// then **reuse the live session whose correlation id equals the request's `contextId`** (A-48
/// stateful mode) or create a new one tagged `agent_id = "a2a"` with that `contextId` (if any) as
/// the correlation id. Sweep-before-lookup means an expired conversation is never resumed: its
/// session is gone, so the same `contextId` mints a fresh session that becomes the new
/// continuation target.
///
/// Callers MUST hold the `turn_gate` before calling this (C-29): minting ahead of the gate would
/// let a session sit queued — alive but idle, its `updated_at` frozen at mint — where a *different*
/// concurrent request's mint-time sweep (this same lazy pass) could prune it before it ever gets
/// its turn. Minting inside the gate closes that window: a session's lifetime never has a
/// queued-but-unswept-safe gap, because mint and run happen back to back under one gate hold.
///
/// The sweep runs lazily per request rather than on a background timer in `serve_on`, because:
/// (a) it then covers *every* mount of [`crate::router`] — the standalone server and the `a2a`
/// channel (which serves the router itself) — with no per-caller wiring; (b) growth only happens
/// when tasks arrive, so sweeping at mint time bounds the registry exactly where it can grow (an
/// idle server accretes nothing, so nothing goes stale while idle); and (c) it needs no
/// background-task lifecycle/shutdown handling. The pass is one indexed query over `streams`
/// plus a whole-stream delete per expired session — negligible next to the model turn it precedes.
fn create_a2a_session(
    engine: &Shared,
    ttl: A2aTtl,
    context_id: Option<&str>,
    realm: Option<&str>,
) -> flux_core::Result<String> {
    let pruned = prune_expired_a2a_sessions_at(&engine.events, ttl.0, now_ms());
    if pruned > 0 {
        eprintln!(
            "(a2a: pruned {pruned} expired session(s) past the {}s TTL)",
            ttl.0
        );
    }
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

/// Sweep A2A-tagged sessions whose last activity is more than `ttl_secs` old, as of `now_ms`.
/// `ttl_secs == 0` disables pruning entirely (the documented `[server] a2a_session_ttl_secs = 0`).
/// Non-fatal by design: a failed sweep logs and returns 0 — it must never block the task that
/// triggered it. Returns the number of sessions pruned.
pub(crate) fn prune_expired_a2a_sessions_at(
    events: &flux_events::EventStore,
    ttl_secs: u64,
    now_ms: i64,
) -> usize {
    if ttl_secs == 0 {
        return 0;
    }
    let ttl_ms = i64::try_from(ttl_secs)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    match events.prune_inactive(A2A_AGENT_ID, now_ms.saturating_sub(ttl_ms)) {
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
    );
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
    let engine = resolved.engine;
    match req.method.as_str() {
        "message/send" => send(engine, auth, turn_gate, a2a_ttl, ctx, req.id, req.params)
            .await
            .into_response(),
        "message/stream" => {
            match subscribe(
                engine,
                auth,
                turn_gate,
                a2a_ttl,
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

/// `POST /a2a` — JSON-RPC 2.0 endpoint.
///
/// - `message/send`   → [`send`] (synchronous, returns a `Task`)
/// - `message/stream` → [`subscribe`] (SSE stream of `TaskStatusUpdate`s)
pub async fn a2a_handler(
    State(engine): State<Shared>,
    State(auth): State<Arc<crate::ServerAuth>>,
    State(turn_gate): State<TurnGate>,
    State(a2a_ttl): State<A2aTtl>,
    ctx: Option<axum::Extension<flux_auth::request::AuthContext>>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    if req.jsonrpc != "2.0" {
        return rpc_err(req.id, -32600, "jsonrpc must be \"2.0\"").into_response();
    }
    let ctx = ctx.map(|e| e.0);
    match req.method.as_str() {
        "message/send" => send(engine, auth, turn_gate, a2a_ttl, ctx, req.id, req.params)
            .await
            .into_response(),
        "message/stream" => {
            match subscribe(
                engine,
                auth,
                turn_gate,
                a2a_ttl,
                ctx,
                req.id.clone(),
                req.params,
            )
            .await
            {
                Ok(sse) => sse.into_response(),
                // Format pre-SSE errors as JSON-RPC so the `id` is not silently dropped; `subscribe`
                // carries the A2A-specific code (e.g. `-32005` for an unusable content type).
                Err((code, msg)) => rpc_err(req.id, code, msg).into_response(),
            }
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

// ── message/send ──────────────────────────────────────────────────────────────

async fn send(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
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
    let requested_context = server::extract_context_id(&params);
    // Acquire the gate BEFORE minting (C-29) — see `create_a2a_session`'s doc for why. The
    // identity swap + realm derivation happen through `enter_turn` under the same gate hold:
    // the realm used for continuity keying is obtainable only from the function that also sets
    // the executor identity (D-69 coupling).
    let _turn = turn_gate.lock().await;
    let realm = match crate::enter_turn(&auth, &engine, ctx.as_ref(), &_turn) {
        Ok(r) => r,
        // Constant text: unreachable behind `require_auth`, fail-closed if a mount forgets it.
        Err(_) => return rpc_err(id, -32603, crate::UNAUTHORIZED_BODY),
    };
    let session_id =
        match create_a2a_session(&engine, ttl, requested_context.as_deref(), realm.as_deref()) {
            Ok(s) => s,
            Err(e) => return rpc_err(id, -32603, format!("Session error: {e}")),
        };
    let context_id = requested_context.unwrap_or_else(|| session_id.clone());
    let mut sink = Collect::default();
    match engine.run_turn(&session_id, &input, &mut sink).await {
        Ok(()) => {
            // A-52: return the conversation so far as `Task.history`, capped to the client's
            // `historyLength` when set. Read before `session_id` is moved into the `Task`.
            let history = a2a_history(&engine, &session_id, server::history_length(&params));
            let status = TaskStatus::new(
                TaskState::Completed,
                Some(Message::agent_text(sink.text)),
                Some(server::now_rfc3339()),
            );
            let mut task = Task::new(session_id, Some(context_id), status);
            task.history = history;
            match serde_json::to_value(&task) {
                Ok(v) => rpc_json(id, v),
                Err(e) => rpc_err(id, -32603, format!("encode error: {e}")),
            }
        }
        Err(e) => rpc_err(id, -32603, format!("Agent error: {e}")),
    }
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

// ── message/stream ────────────────────────────────────────────────────────────

async fn subscribe(
    engine: Shared,
    auth: Arc<crate::ServerAuth>,
    turn_gate: TurnGate,
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
        let session_id =
            match create_a2a_session(
                &engine_clone,
                ttl,
                requested_context.as_deref(),
                realm.as_deref(),
            ) {
                Ok(s) => s,
                Err(e) => {
                    // The SSE response is already established by the time minting can fail here, so
                    // report it as a JSON-RPC error frame inside the stream (mirroring `rpc_err`)
                    // rather than a pre-SSE HTTP error.
                    let _ = tx.send(Event::default().data(
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": format!("Session error: {e}") },
                    })
                    .to_string(),
                ));
                    return;
                }
            };
        let context_id = requested_context.unwrap_or_else(|| session_id.clone());
        let task_id = session_id.clone();

        // Initial "working" update so the caller knows the task started.
        let _ = tx.send(status_frame(
            &id,
            &task_id,
            &context_id,
            TaskState::Working,
            None,
            false,
        ));
        let mut sink = StreamSink {
            tx: tx.clone(),
            id: id.clone(),
            task_id: task_id.clone(),
            context_id: context_id.clone(),
            cancel: cancel_task.clone(),
        };
        let result = engine_clone
            .run_turn_cancellable(&session_id, &input, &mut sink, &cancel_task)
            .await;
        // If the client disconnected mid-stream, skip the final event — nobody is listening.
        if !cancel_task.is_cancelled() {
            // The final event carries no message on success — the deltas already streamed are
            // authoritative; on failure it carries the error text.
            let (state, message) = match result {
                Ok(()) => (TaskState::Completed, None),
                Err(e) => (TaskState::Failed, Some(Message::agent_text(e.to_string()))),
            };
            let _ = tx.send(status_frame(
                &id,
                &task_id,
                &context_id,
                state,
                message,
                true,
            ));
        }
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
struct StreamSink {
    tx: tokio::sync::mpsc::UnboundedSender<Event>,
    /// The originating JSON-RPC request id, echoed in every frame.
    id: Option<Value>,
    task_id: String,
    context_id: String,
    /// Cancelled when the SSE receiver is dropped (client disconnect); checked between plan rounds
    /// by `run_turn_cancellable`.
    cancel: CancellationToken,
}

impl AgentSink for StreamSink {
    fn text_delta(&mut self, t: &str) {
        // Send only the delta in working events; sending the full accumulated text on every token
        // would be O(N²) in response length.
        let frame = status_frame(
            &self.id,
            &self.task_id,
            &self.context_id,
            TaskState::Working,
            Some(Message::agent_text(t)),
            false,
        );
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

    /// A provider that answers every call with a one-word prose turn — the agent loop exits on
    /// prose, so a handler-driven test turn completes fast and deterministically. (Mirrors
    /// `crate::tests::ProseProvider`; duplicated locally so this module stays self-contained.)
    struct ProseProvider;
    #[async_trait::async_trait]
    impl flux_provider::Provider for ProseProvider {
        fn name(&self) -> &str {
            "mock"
        }
        async fn stream(
            &self,
            _req: flux_provider::Request,
        ) -> flux_core::Result<flux_provider::ChunkStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(flux_core::Chunk::TextDelta("ok".into())),
                Ok(flux_core::Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                }),
            ])))
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
            flux_runtime::PermissionManager::from_rules(
                &["plan".into(), "run_plan".into(), "observe".into()],
                &[],
            ),
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

        // Fire request X through the real `send` handler. It must queue behind the held gate
        // before its turn can run.
        let engine_x = engine.clone();
        let gate_x = turn_gate.clone();
        let x_task = tokio::spawn(async move {
            send(
                engine_x,
                Arc::new(crate::ServerAuth::Open),
                gate_x,
                ttl,
                None,
                Some(json!(1)),
                Some(send_params("hi")),
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
        let _session_y = create_a2a_session(&engine, ttl, None, None).unwrap();

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
