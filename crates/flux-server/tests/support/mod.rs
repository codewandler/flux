//! Shared scaffolding for the `flux-server` integration suite (C-41).
//!
//! This is a plain module (`tests/support/mod.rs`, not its own test binary) included by each
//! integration test file via `mod support;`. It duplicates the small engine-building helper the
//! inline unit tests in `src/lib.rs`/`src/a2a.rs` use (`test_engine`) because this suite lives in a
//! separate integration-test crate that can only see `flux-server`'s **public** API — there is no
//! production code change here, only test fixtures built the same way the crate's own tests are.
//!
//! Not every helper is used by every test binary that includes this module (each `tests/*.rs` file
//! compiles separately) — `dead_code` is expected and silenced here rather than per-binary.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt; // for `oneshot`

use flux_flow::engine::FlowEngine;

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, collision-free scratch directory (mirrors the `flux-xxx-test-{pid}-{n}` convention
/// used by the other crates' test suites, e.g. `flux-runtime`/`flux-system`).
pub fn scratch_dir(label: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("flux-server-it-{label}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn declares_intent(req: &flux_provider::Request) -> bool {
    req.tools.iter().any(|tool| tool.name == "declare_intent")
}

fn intent_chunks() -> Vec<flux_core::Chunk> {
    vec![
        flux_core::Chunk::Block(flux_core::ContentBlock::ToolUse {
            id: "intent".into(),
            name: "declare_intent".into(),
            input: json!({
                "intent": "answer the current message",
                "capability_families": [],
            }),
        }),
        flux_core::Chunk::Done {
            stop_reason: Some(flux_core::StopReason::ToolUse),
        },
    ]
}

/// A provider that declares a typed intent, then answers with a one-word prose turn. Mirrors
/// `flux_server`'s own inline `ProseProvider` fixtures.
pub struct ProseProvider;

#[async_trait::async_trait]
impl flux_provider::Provider for ProseProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(
        &self,
        req: flux_provider::Request,
    ) -> flux_core::Result<flux_provider::ChunkStream> {
        let chunks = if declares_intent(&req) {
            intent_chunks()
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

/// A provider whose prose turn streams two separate text deltas before ending — lets SSE-framing
/// tests observe more than one `working` update ahead of the final `completed` frame.
pub struct MultiDeltaProvider;

#[async_trait::async_trait]
impl flux_provider::Provider for MultiDeltaProvider {
    fn name(&self) -> &str {
        "mock"
    }
    async fn stream(
        &self,
        req: flux_provider::Request,
    ) -> flux_core::Result<flux_provider::ChunkStream> {
        let chunks = if declares_intent(&req) {
            intent_chunks()
        } else {
            vec![
                flux_core::Chunk::TextDelta("hello ".into()),
                flux_core::Chunk::TextDelta("world".into()),
                flux_core::Chunk::Done {
                    stop_reason: Some(flux_core::StopReason::EndTurn),
                },
            ]
        };
        Ok(Box::pin(futures::stream::iter(chunks.into_iter().map(Ok))))
    }
}

/// Assemble a real `FlowEngine` over `provider`: real executor/tool-registry/safety-envelope
/// wiring and an in-memory event store, no network. Mirrors `flux_server`'s private
/// `tests::test_engine` helper (duplicated here since it isn't part of the crate's public API).
pub fn test_engine(provider: Arc<dyn flux_provider::Provider>) -> Arc<FlowEngine> {
    let dir = scratch_dir("engine");
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
        provider,
        executor,
        events,
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
    Arc::new(engine)
}

/// The [`flux_server::DiscoveryEnv`] every router in this suite is built against: **no home at
/// all**, so the router's config read (the A2A session TTL and the C-189 resource limits) consults
/// the managed and project layers only and never the operator's `~/.flux/config.toml` (C-392).
///
/// Without this pin an operator who sets `[server] requests_per_minute` or `a2a_session_ttl_secs`
/// in their own home changes what this suite asserts against — a verdict that depends on the
/// machine rather than the fixture, whose failure looks exactly like a real regression.
pub fn pinned_env() -> flux_server::DiscoveryEnv {
    flux_server::DiscoveryEnv::empty()
}

/// Build the real production router (`flux_server::router_in`) over a fresh prose-completing engine
/// — the default fixture most tests need. Callers who need a different provider or an explicit TTL
/// build the engine/router directly with [`test_engine`] and `flux_server::router_in`, passing
/// [`pinned_env`].
pub fn app(token: Option<String>) -> Router {
    let engine = test_engine(Arc::new(ProseProvider));
    flux_server::router_in(
        engine,
        flux_server::ServerAuth::from_token(token),
        flux_server::CardInfo::flux_coding(),
        // A loopback bind so the C-190 construction-time refusal admits the fixture (unauthenticated
        // fixtures use `token = None` → `ServerAuth::Open`, which is loopback-only by construction).
        "127.0.0.1:0".parse().unwrap(),
        &pinned_env(),
    )
    .expect("loopback router builds")
}

/// `POST path` with a JSON body and no `Authorization` header; returns `(status, parsed JSON
/// body)`. Panics if the response body isn't valid JSON — use [`post_raw`] when the point of the
/// test IS a non-JSON/invalid response, or [`post_json_auth`] to present a bearer token.
pub async fn post_json(app: Router, path: &str, body: Value) -> (StatusCode, Value) {
    post_json_auth(app, path, body, None).await
}

/// As [`post_json`], presenting an `Authorization: <auth>` header when `auth` is `Some` (pass the
/// full header value, e.g. `"Bearer s3cr3t"` — same convention as [`get_raw`]).
pub async fn post_json_auth(
    app: Router,
    path: &str,
    body: Value,
    auth: Option<&str>,
) -> (StatusCode, Value) {
    let (status, raw) = post_raw_auth(app, path, "application/json", &body.to_string(), auth).await;
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("response body was not valid JSON: {e}\nbody: {raw:?}"))
    };
    (status, body)
}

/// `POST path` with a raw (possibly malformed/non-JSON) body and no `Authorization` header.
/// Returns `(status, raw body text)`.
pub async fn post_raw(
    app: Router,
    path: &str,
    content_type: &str,
    body: &str,
) -> (StatusCode, String) {
    post_raw_auth(app, path, content_type, body, None).await
}

/// As [`post_raw`], presenting an `Authorization: <auth>` header when `auth` is `Some` (pass the
/// full header value, e.g. `"Bearer s3cr3t"` — same convention as [`get_raw`]).
pub async fn post_raw_auth(
    app: Router,
    path: &str,
    content_type: &str,
    body: &str,
    auth: Option<&str>,
) -> (StatusCode, String) {
    let mut rb = HttpRequest::post(path).header("content-type", content_type);
    if let Some(a) = auth {
        rb = rb.header("authorization", a);
    }
    let res = rb.body(Body::from(body.to_string())).unwrap();
    let res = app.oneshot(res).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// `GET path`, optionally with an `Authorization` header. Returns `(status, raw body text)`.
pub async fn get_raw(app: Router, path: &str, auth: Option<&str>) -> (StatusCode, String) {
    let mut rb = HttpRequest::get(path);
    if let Some(a) = auth {
        rb = rb.header("authorization", a);
    }
    let res = app.oneshot(rb.body(Body::empty()).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Parse a raw SSE byte stream into the ordered list of its `data:` JSON payloads. Every frame in
/// this suite carries a single-line `data:` field (no `event:` line — the A2A frames are plain
/// JSON-RPC responses per spec), so a blank-line-delimited split plus a `data:` prefix strip is
/// enough; a stray `:`-comment (SSE keep-alive ping) has no `data:` line and is skipped.
pub fn parse_sse_json(raw: &str) -> Vec<Value> {
    raw.split("\n\n")
        .filter_map(|frame| {
            let data: String = frame
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim_start)
                .collect::<Vec<_>>()
                .join("\n");
            if data.is_empty() {
                None
            } else {
                Some(serde_json::from_str(&data).unwrap_or_else(|e| {
                    panic!("SSE frame data was not valid JSON: {e}\nframe: {frame:?}")
                }))
            }
        })
        .collect()
}
