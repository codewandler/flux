//! An **in-process Brave Talk / JaaS token service** — the HTTP half of D-206, bound on loopback.
//!
//! It answers the three requests the 2026-07-30 spike observed, and it is **strict about the things
//! the handshake is actually for**: the `PUT` refuses without the CSRF token *and* the cookie the
//! `OPTIONS` handed out, and the conference-request refuses without the exact JWT as a Bearer. A
//! double that accepted anything would let a backend that forgot the CSRF header pass its tests and
//! fail against the vendor.
//!
//! Every request is recorded, so a test asserts the *wire*, not just the outcome.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{options, post};
use axum::Router;
use base64::Engine;

/// The CSRF token the double hands out and then insists on.
pub const CSRF_TOKEN: &str = "csrf-token-from-the-options-preflight";
/// The cookie the double sets on the preflight and then insists on.
pub const CSRF_COOKIE: &str = "_gorilla_csrf=gorilla-csrf-cookie-value";
/// The JaaS tenant every token this double mints is `sub`-scoped to.
pub const TENANT: &str = "vpaas-magic-cookie-testtenant";
/// The MUC service the double's conference-request answers with — matching the XMPP double, so the
/// two can be driven end to end in one test.
pub const MUC_SERVICE: &str = "conference.example.org";

/// One recorded request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub query: String,
    pub csrf: Option<String>,
    pub cookie: Option<String>,
    pub authorization: Option<String>,
    pub body: String,
}

struct DoubleState {
    /// How long each minted token is valid for.
    ttl: Duration,
    /// How many conference-requests answer `ready: false` before the first `ready: true`.
    not_ready_first: AtomicUsize,
    minted: Mutex<Vec<String>>,
    seen: Mutex<Vec<Seen>>,
}

/// A running token service.
pub struct JaasDouble {
    addr: SocketAddr,
    state: Arc<DoubleState>,
}

impl JaasDouble {
    /// Bind on loopback and start serving, minting tokens valid for `ttl`.
    pub async fn start(ttl: Duration) -> Self {
        Self::start_with(ttl, 0).await
    }

    /// As [`JaasDouble::start`], but the first `not_ready` conference-requests answer
    /// `{"ready": false}` — the state Jitsi reports while focus is still allocating.
    pub async fn start_with(ttl: Duration, not_ready: usize) -> Self {
        let state = Arc::new(DoubleState {
            ttl,
            not_ready_first: AtomicUsize::new(not_ready),
            minted: Mutex::new(Vec::new()),
            seen: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/api/v1/rooms/{room}", options(preflight).put(mint))
            .route("/{tenant}/conference-request/v1", post(conference))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, state }
    }

    /// The `http://` base a backend is configured with — it serves both the token service and the
    /// conference-request path.
    pub fn base(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Every JWT this double has minted, in order.
    pub fn minted(&self) -> Vec<String> {
        self.state.minted.lock().unwrap().clone()
    }

    /// Every request it has answered, in order.
    pub fn seen(&self) -> Vec<Seen> {
        self.state.seen.lock().unwrap().clone()
    }

    /// The recorded requests for one method, in order.
    pub fn seen_method(&self, method: &str) -> Vec<Seen> {
        self.seen()
            .into_iter()
            .filter(|s| s.method == method)
            .collect()
    }
}

fn record(state: &DoubleState, method: &str, path: &str, query: &str, h: &HeaderMap, body: &Bytes) {
    let get = |name: &str| {
        h.get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    state.seen.lock().unwrap().push(Seen {
        method: method.to_string(),
        path: path.to_string(),
        query: query.to_string(),
        csrf: get("x-csrf-token"),
        cookie: get("cookie"),
        authorization: get("authorization"),
        body: String::from_utf8_lossy(body).to_string(),
    });
}

/// `OPTIONS /api/v1/rooms/<room>` — hands out the CSRF token and its cookie.
async fn preflight(
    State(state): State<Arc<DoubleState>>,
    Path(room): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(
        &state,
        "OPTIONS",
        &format!("/api/v1/rooms/{room}"),
        "",
        &headers,
        &body,
    );
    let mut out = HeaderMap::new();
    out.insert("x-csrf-token", CSRF_TOKEN.parse().unwrap());
    out.insert(
        "set-cookie",
        format!("{CSRF_COOKIE}; Path=/; HttpOnly; SameSite=Lax")
            .parse()
            .unwrap(),
    );
    (StatusCode::NO_CONTENT, out).into_response()
}

/// `PUT /api/v1/rooms/<room>` — joins an existing room and answers with the guest JWT. Refuses
/// without both halves of the CSRF handshake, which is the whole reason the preflight exists.
async fn mint(
    State(state): State<Arc<DoubleState>>,
    Path(room): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    record(
        &state,
        "PUT",
        &format!("/api/v1/rooms/{room}"),
        "",
        &headers,
        &body,
    );
    let csrf_ok = headers
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v == CSRF_TOKEN);
    let cookie_ok = headers
        .get("cookie")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains(CSRF_COOKIE));
    if !csrf_ok || !cookie_ok {
        return (StatusCode::FORBIDDEN, "CSRF check failed").into_response();
    }

    let jwt = guest_jwt(&room, state.ttl);
    state.minted.lock().unwrap().push(jwt.clone());
    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::json!({ "jwt": jwt }).to_string(),
    )
        .into_response()
}

/// `POST /<tenant>/conference-request/v1?room=<room>` — focus allocation. Refuses without the JWT as
/// a Bearer, and **lowercases the room** in the MUC JID it answers with, exactly as the vendor does.
async fn conference(
    State(state): State<Arc<DoubleState>>,
    Path(tenant): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let room = params.get("room").cloned().unwrap_or_default();
    record(
        &state,
        "POST",
        &format!("/{tenant}/conference-request/v1"),
        &format!("room={room}"),
        &headers,
        &body,
    );
    let minted = state.minted.lock().unwrap().clone();
    let bearer_ok = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|jwt| minted.iter().any(|m| m == jwt));
    if !bearer_ok {
        return (StatusCode::UNAUTHORIZED, "bad bearer").into_response();
    }
    if tenant != TENANT {
        return (StatusCode::NOT_FOUND, "unknown tenant").into_response();
    }

    // Focus may still be allocating. The spike only ever saw `ready: true`; this arm exists so the
    // backend's handling of the documented not-ready state is exercised.
    if state
        .not_ready_first
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            n.checked_sub(1).or(Some(0))
        })
        .is_ok_and(|remaining| remaining > 0)
    {
        return (
            StatusCode::OK,
            [("content-type", "application/json")],
            serde_json::json!({ "ready": false }).to_string(),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [("content-type", "application/json")],
        serde_json::json!({
            "ready": true,
            "focusJid": "focus@auth.8x8.vc",
            "room": format!("{}@{MUC_SERVICE}", room.to_lowercase()),
        })
        .to_string(),
    )
        .into_response()
}

/// A guest JWT shaped like the one the spike observed. The signature is a placeholder — flux never
/// verifies it, and neither does this double.
fn guest_jwt(room: &str, ttl: Duration) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let exp = (SystemTime::now() + ttl)
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = serde_json::json!({
        "aud": "jitsi",
        "iss": "chat",
        "sub": TENANT,
        "room": room,
        "exp": exp,
        "context": { "user": { "moderator": "false" }, "features": { "recording": "false" } },
    });
    format!(
        "{}.{}.{}",
        b64.encode(br#"{"alg":"RS256","typ":"JWT","kid":"vpaas-magic-cookie/abc"}"#),
        b64.encode(serde_json::to_vec(&claims).unwrap()),
        // Distinct per mint, so a test can tell a refreshed token from the one it replaced.
        b64.encode(format!("sig-{}", NEXT.fetch_add(1, Ordering::SeqCst)).as_bytes())
    )
}
