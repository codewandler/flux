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

use crate::config::WebhookSettings;
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
        // Secrets are already host-resolved before these settings deserialize, so `token` is a plain value.
        //
        // **An empty token is not a token, and it is worse than none.**
        //
        // `token ""` — or `token secret "K"` where `K` is exported empty, because
        // `flux_app::resolve_secrets` resolves through `std::env::var`, which does not filter an empty
        // value — would otherwise arrive here as `Some("")`. It then satisfies the `is_none()` guard
        // below, so the public bind is permitted, and the bearer check compares the *presented* token
        // (which is `""` when the request carries no `Authorization` header at all) against the
        // expected one; two empty byte strings are equal, so every anonymous request authenticates. On
        // a host that auto-approves tools that is an open remote-trigger surface presented as an
        // authenticated one.
        //
        // Refused rather than normalised to `None`, and refused on **loopback** too: normalising is
        // silent, and would ship an operator a channel they believe is authenticated, one `addr` edit
        // away from being public. [`authorized`] refuses an empty expected token as well, so neither
        // half depends on the other being right — but this is the half that runs before a port is
        // bound, which is the only half that can prevent the exposure rather than survive it.
        let token = match s.token.as_deref() {
            Some(token) if token.trim().is_empty() => anyhow::bail!(
                "channel `{}`: `token` is set but empty, which would authenticate every request — \
                 including one carrying no `Authorization` header at all. Give it a value, or remove \
                 it (a loopback bind needs none). A `secret \"KEY\"` reference resolves to an empty \
                 string when `KEY` is exported empty.",
                decl.name
            ),
            other => other.map(str::to_string),
        };
        // The host auto-approves tools (no interactive approver), so an open non-loopback listener is a
        // remote-trigger surface — require a bearer token there, mirroring flux-server.
        if !addr.ip().is_loopback() && token.is_none() {
            anyhow::bail!(
                "channel `{}`: refusing to bind non-loopback {addr} without a `token` \
                 (set `token secret \"KEY\"`)",
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
    if !authorized(state.token.as_deref(), &headers) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
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

/// Whether a request may be delivered, given the channel's expected bearer token.
///
/// Two rules, and the second is the one that is easy to get wrong:
///
/// - **No expected token** → nothing to check. Only reachable on a loopback bind; a non-loopback one
///   without a token is refused at load.
/// - **An empty expected token authenticates nothing.** A request with no `Authorization` header
///   presents `""`, and a constant-time compare of two empty byte strings is `true` — so an empty
///   expected token would admit every anonymous caller while the channel reads, everywhere it is
///   printed or logged, as "token-protected". [`WebhookChannel::from_decl`] already refuses one
///   before a port is bound; this is the same rule stated where the comparison happens, so a future
///   path that reaches the handler without that constructor cannot reopen the hole.
///
/// Extracted rather than inlined precisely so the two halves are independently testable: once the
/// constructor makes `Some("")` unreachable, a test routed through it can only ever cover one of them.
fn authorized(expected: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    if expected.is_empty() {
        return false;
    }
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    constant_time_eq(presented.as_bytes(), expected.as_bytes())
}

/// Length-aware constant-time comparison (mirrors flux-server; avoids leaking the token via timing).
/// Shared with the `connector` adapter, which authenticates the same bearer the same way.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use flux_app::JourneyRun;
    use tower::ServiceExt; // for `oneshot`

    struct Nothing;

    #[async_trait]
    impl Deliverer for Nothing {
        async fn deliver(&self, _label: &str, _payload: Value) -> anyhow::Result<Vec<JourneyRun>> {
            Ok(Vec::new())
        }
    }

    /// A channel built **without** [`WebhookChannel::from_decl`], so the request path can be exercised
    /// with a token the constructor refuses. That is the whole point of testing the two halves apart:
    /// once the constructor makes `Some("")` unreachable, a channel routed through `from_decl` can
    /// never reach the comparison with one, and the comparison would go untested forever.
    fn channel(token: Option<&str>) -> WebhookChannel {
        WebhookChannel {
            name: "hook".to_string(),
            addr: SocketAddr::from_str("127.0.0.1:0").expect("a loopback addr"),
            path: "/hook".to_string(),
            is_async: false,
            token: token.map(str::to_string),
        }
    }

    async fn post(token: Option<&str>, bearer: Option<&str>) -> StatusCode {
        let mut req = Request::post("/hook").header("content-type", "application/json");
        if let Some(bearer) = bearer {
            req = req.header(axum::http::header::AUTHORIZATION, bearer);
        }
        channel(token)
            .router(Arc::new(Nothing))
            .oneshot(req.body(Body::from("{}")).expect("a request"))
            .await
            .expect("the router answers")
            .status()
    }

    /// **A request carrying no `Authorization` header is rejected by a channel whose token is empty.**
    ///
    /// The failure this pins is an identity, not a typo: the presented token is `""` when the header
    /// is absent, so a bare `constant_time_eq(b"", b"")` is `true` — equal lengths, an empty loop.
    /// An empty expected token would admit every anonymous caller while the channel reads, everywhere
    /// it is printed or logged, as "token-protected", on a host that auto-approves tools.
    ///
    /// [`WebhookChannel::from_decl`] refuses an empty token before a port is bound, and that is the
    /// half that prevents the exposure. This is the same rule stated where the comparison happens, so
    /// a future path reaching the handler without that constructor cannot reopen the hole.
    #[tokio::test]
    async fn a_request_with_no_authorization_header_is_rejected_by_an_empty_token_channel() {
        assert_eq!(
            post(Some(""), None).await,
            StatusCode::UNAUTHORIZED,
            "an empty expected token must never authorize an anonymous request"
        );
        assert_eq!(
            post(Some(""), Some("Bearer ")).await,
            StatusCode::UNAUTHORIZED,
            "nor an empty bearer spelled out"
        );

        // The rules either side of it, so the fix cannot have been "refuse everything".
        assert_eq!(
            post(None, None).await,
            StatusCode::OK,
            "no expected token is a loopback channel with nothing to check"
        );
        assert_eq!(post(Some("t0ken"), None).await, StatusCode::UNAUTHORIZED);
        assert_eq!(
            post(Some("t0ken"), Some("Bearer nope")).await,
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            post(Some("t0ken"), Some("Bearer t0ken")).await,
            StatusCode::OK
        );
    }
}
