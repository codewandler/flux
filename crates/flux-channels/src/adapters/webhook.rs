//! The **webhook** adapter (`kind = "webhook" | "http"`): an axum server per channel. A `POST` to its
//! path delivers the JSON body under the channel name and replies with the triggered journeys' results.

use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use flux_lang::program::ChannelDecl;

use crate::config::{resolve_secret, WebhookSettings};
use crate::{Channel, Deliverer};

pub struct WebhookChannel {
    name: String,
    addr: SocketAddr,
    path: String,
    is_async: bool,
    token: Option<String>,
}

impl WebhookChannel {
    pub fn from_decl(decl: &ChannelDecl) -> anyhow::Result<Self> {
        let s: WebhookSettings = serde_json::from_value(decl.settings.clone())
            .map_err(|e| anyhow::anyhow!("channel `{}` settings: {e}", decl.name))?;
        let addr = SocketAddr::from_str(&s.addr)
            .map_err(|e| anyhow::anyhow!("channel `{}`: bad addr `{}`: {e}", decl.name, s.addr))?;
        let token = match s.token {
            Some(t) => Some(resolve_secret(&t)?),
            None => None,
        };
        // The host auto-approves tools (no interactive approver), so an open non-loopback listener is a
        // remote-trigger surface — require a bearer token there, mirroring flux-server.
        if !addr.ip().is_loopback() && token.is_none() {
            anyhow::bail!(
                "channel `{}`: refusing to bind non-loopback {addr} without a `token` \
                 (set `token = \"secret:env/KEY\"`)",
                decl.name
            );
        }
        // axum route paths must start with `/`; normalize a bare path so a typo isn't a runtime panic.
        let path = if s.path.starts_with('/') {
            s.path
        } else {
            format!("/{}", s.path)
        };
        Ok(Self {
            name: decl.name.clone(),
            addr,
            path,
            is_async: s.is_async,
            token,
        })
    }

    /// Build the axum router for this channel over `d` (exposed for hermetic tests).
    pub fn router(&self, d: Arc<dyn Deliverer>) -> Router {
        let state = Arc::new(HookState {
            name: self.name.clone(),
            deliverer: d,
            is_async: self.is_async,
            token: self.token.clone(),
        });
        Router::new()
            .route(&self.path, post(handle))
            .with_state(state)
    }
}

struct HookState {
    name: String,
    deliverer: Arc<dyn Deliverer>,
    is_async: bool,
    token: Option<String>,
}

async fn handle(
    State(state): State<Arc<HookState>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Some(expected) = &state.token {
        let presented = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .unwrap_or("");
        if !constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
        }
    }

    if state.is_async {
        let d = state.deliverer.clone();
        let label = state.name.clone();
        tokio::spawn(async move {
            if let Err(e) = d.deliver(&label, body).await {
                eprintln!("webhook `{label}`: async delivery failed: {e}");
            }
        });
        return StatusCode::ACCEPTED.into_response();
    }

    match state.deliverer.deliver(&state.name, body).await {
        Ok(runs) => {
            let out: Vec<Value> = runs
                .into_iter()
                .map(|r| json!({ "journey": r.journey, "result": r.result, "steps": r.steps }))
                .collect();
            Json(json!({ "runs": out })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Length-aware constant-time comparison (mirrors flux-server; avoids leaking the token via timing).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[async_trait]
impl Channel for WebhookChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, d: Arc<dyn Deliverer>, cancel: CancellationToken) -> anyhow::Result<()> {
        let listener = tokio::net::TcpListener::bind(self.addr)
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: bind {}: {e}", self.name, self.addr))?;
        axum::serve(listener, self.router(d))
            .with_graceful_shutdown(async move { cancel.cancelled().await })
            .await
            .map_err(|e| anyhow::anyhow!("channel `{}`: serve: {e}", self.name))
    }
}
