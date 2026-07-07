//! RFC 7662 token introspection — the shipped [`RequestAuthenticator`] impl — plus
//! [`CachedAuthenticator`], the TTL/capacity-bounded caching decorator that wraps *any*
//! authenticator. Behind the `introspect` cargo feature. Design:
//! `docs/designs/request-auth-seam.md`.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flux_policy::{Caller, CallerKind, Principal, Scope, Trust, TrustKind, TrustLevel};
use serde_json::Value;

use crate::request::{AuthContext, AuthError, RequestAuthenticator};

/// Hard cap on the introspection response body. The introspection call happens *before* any
/// authorization, so an unbounded read here would be the cheapest OOM in the process.
const MAX_RESPONSE_BYTES: usize = 256 * 1024;

/// The reserved group prefix only the authenticator may write (`account:<id>` mirror group).
const ACCOUNT_PREFIX: &str = "account:";

/// `token_type` / `token_use` / `typ` values that identify a **non-access** token
/// (case-insensitive). RFC 7662 happily introspects refresh tokens as `active:true`; a leaked
/// refresh token — useless at the token endpoint without the client secret — must not become a
/// working bearer credential here. Unknown values pass: IdPs report access tokens as `Bearer`,
/// `access_token`, `at+jwt`, and worse.
/// Whether an introspected `token_type`/`token_use`/`typ` names a NON-access token (refresh, id,
/// logout) that must be rejected. Normalizes separators and case first, since IdPs spell these
/// inconsistently (`"Refresh Token"`, `"refresh-token"`, `"REFRESH_TOKEN"` all mean the same
/// thing), then matches by family prefix rather than an exact allowlist — an exact set silently
/// lets a differently-spelled refresh token through, the exact credential this guard rejects.
/// Access tokens (`Bearer`, `access_token`, `at+jwt`, and worse) do not start with these families.
fn is_non_access_token_type(raw: &str) -> bool {
    let norm = raw.to_ascii_lowercase().replace([' ', '-'], "_");
    norm.starts_with("refresh") || norm.starts_with("logout") || norm == "id" || norm == "id_token"
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for [`Introspector`]. `Debug` is manual and redacts the client secret.
#[derive(Clone)]
pub struct IntrospectionConfig {
    /// The RFC 7662 introspection endpoint. Deployment config, never model/request-reachable.
    pub endpoint: String,
    /// `(client_id, client_secret)` → `client_secret_basic`; `None` → bare POST (brokers that
    /// gate the endpoint at the network layer).
    pub client: Option<(String, String)>,
    /// Default `false`: https required. Explicit opt-in for trusted-network / cluster-internal
    /// endpoints only.
    pub allow_http: bool,
    /// Claim carrying the tenancy key, resolved literal-key first then dot-path
    /// (→ [`AuthContext::account`]).
    pub account_claim: Option<String>,
    /// Map a missing/empty account to `Unauthorized` — for deployments where tenancy is
    /// mandatory.
    pub require_account: bool,
    /// Claim carrying roles (JSON array of strings or one space-separated string) →
    /// `caller.groups`, verbatim except the reserved `account:` prefix.
    pub roles_claim: Option<String>,
    /// How far the deployment trusts this IdP. Clamped to ≤ [`TrustLevel::Verified`]: a
    /// network-presented token can never mint `Privileged`/`System`.
    pub trust_level: TrustLevel,
    /// Total request timeout for the introspection call.
    pub timeout: Duration,
}

impl IntrospectionConfig {
    /// A config with the design defaults: no client auth, https required, no account/roles
    /// claims, `Verified` trust, 10 s timeout.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            client: None,
            allow_http: false,
            account_claim: None,
            require_account: false,
            roles_claim: None,
            trust_level: TrustLevel::Verified,
            timeout: Duration::from_secs(10),
        }
    }
}

impl fmt::Debug for IntrospectionConfig {
    /// Manual: the client secret must never reach logs — `Debug` output is exactly where a
    /// config struct leaks.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IntrospectionConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "client",
                &self
                    .client
                    .as_ref()
                    .map(|(id, _)| format!("{id} <secret redacted>")),
            )
            .field("allow_http", &self.allow_http)
            .field("account_claim", &self.account_claim)
            .field("require_account", &self.require_account)
            .field("roles_claim", &self.roles_claim)
            .field("trust_level", &self.trust_level)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Construction-time configuration error (bad endpoint scheme, unbuildable HTTP client).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigError(String);

impl ConfigError {
    fn new(msg: impl Into<String>) -> Self {
        Self(msg.into())
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid introspection config: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// Introspector
// ---------------------------------------------------------------------------

/// The RFC 7662 introspection authenticator: POSTs the token to the configured endpoint and
/// projects the returned claims into an [`AuthContext`] — the one claims→identity projection
/// point. Cache-free by design; wrap it in [`CachedAuthenticator`] for TTL caching.
pub struct Introspector {
    config: IntrospectionConfig,
    http: reqwest::Client,
}

impl fmt::Debug for Introspector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Introspector")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Introspector {
    /// Build an introspector, validating the endpoint scheme (https unless `allow_http`) and
    /// constructing an HTTP client that **never follows redirects** — a 307/308 would replay the
    /// POST (and the caller's live bearer token) wherever `Location` points.
    pub fn new(config: IntrospectionConfig) -> Result<Self, ConfigError> {
        let lower = config.endpoint.to_ascii_lowercase();
        if lower.starts_with("http://") {
            if !config.allow_http {
                return Err(ConfigError::new(
                    "endpoint must be https (set allow_http for trusted-network http)",
                ));
            }
        } else if !lower.starts_with("https://") {
            return Err(ConfigError::new("endpoint must be an http(s) URL"));
        }
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.timeout)
            .build()
            .map_err(|e| ConfigError::new(format!("failed to build HTTP client: {e}")))?;
        Ok(Self { config, http })
    }

    /// One introspection round-trip → the raw claims object. All transport-level trouble is
    /// [`AuthError::Unavailable`] (payload log-only); token verdicts happen in `map_claims`.
    async fn introspect(&self, bearer: &str) -> Result<serde_json::Map<String, Value>, AuthError> {
        // Form-serialized, never string concatenation: a token containing `&`/`=`/`%` must
        // arrive as exactly one `token` field.
        let mut req = self
            .http
            .post(&self.config.endpoint)
            .form(&[("token", bearer), ("token_type_hint", "access_token")]);
        if let Some((id, secret)) = &self.config.client {
            req = req.basic_auth(id, Some(secret));
        }
        let mut resp = req
            .send()
            .await
            .map_err(|e| AuthError::Unavailable(format!("introspection request failed: {e}")))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(AuthError::Unavailable(format!(
                "introspection endpoint redirected ({status}); redirects are refused"
            )));
        }
        if !status.is_success() {
            return Err(AuthError::Unavailable(format!(
                "introspection endpoint returned {status}"
            )));
        }
        // Content-Length when present, and a capped incremental read either way (chunked
        // responses carry no length header).
        if let Some(len) = resp.content_length() {
            if len > MAX_RESPONSE_BYTES as u64 {
                return Err(AuthError::Unavailable(format!(
                    "introspection response too large ({len} bytes)"
                )));
            }
        }
        let mut body: Vec<u8> = Vec::new();
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AuthError::Unavailable(format!("introspection read failed: {e}")))?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(AuthError::Unavailable(
                    "introspection response exceeded size cap".to_string(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Object(map)) => Ok(map),
            Ok(_) => Err(AuthError::Unavailable(
                "introspection response is not a JSON object".to_string(),
            )),
            Err(e) => Err(AuthError::Unavailable(format!(
                "introspection response is not valid JSON: {e}"
            ))),
        }
    }

    /// Claims → identity projection per the design. Returns the context plus the token's `exp`
    /// (seconds since epoch) so a cache can bound its TTL.
    fn map_claims(
        &self,
        obj: &serde_json::Map<String, Value>,
    ) -> Result<(AuthContext, Option<u64>), AuthError> {
        // Only a boolean `true` activates; strings, numbers, or absence fail closed.
        if obj.get("active") != Some(&Value::Bool(true)) {
            return Err(AuthError::Unauthorized);
        }
        // Reject non-access tokens (refresh, id): introspection reports them `active:true`.
        for key in ["token_type", "token_use", "typ"] {
            if let Some(v) = obj.get(key).and_then(Value::as_str) {
                if is_non_access_token_type(v) {
                    return Err(AuthError::Unauthorized);
                }
            }
        }

        let account = match &self.config.account_claim {
            Some(claim) => resolve_claim(obj, claim).and_then(claim_to_string),
            None => None,
        };
        if account.is_none() && self.config.require_account {
            return Err(AuthError::Unauthorized);
        }

        let sub = str_claim(obj, "sub");
        let username = str_claim(obj, "username");
        let client_id = str_claim(obj, "client_id");

        // Principal chain sub → username → client_id; final fallback: the account value,
        // namespaced so the shared service principal is audit-distinguishable and can never
        // collide with a real subject in policy.
        let (id, source) = match sub.or(username).or(client_id) {
            Some(id) => (id.to_string(), "introspect"),
            None => match &account {
                Some(acct) => (
                    format!("{ACCOUNT_PREFIX}{acct}"),
                    "introspect:account-fallback",
                ),
                None => return Err(AuthError::Unauthorized),
            },
        };
        let name = username
            .or_else(|| str_claim(obj, "name"))
            .map(str::to_string)
            .unwrap_or_else(|| id.clone());
        // Client-credentials-shaped ⇒ Agent: real IdPs set `sub` on service tokens too, so
        // keying on "has client_id" alone would leave User grants silently matching machines.
        let kind = if client_id.is_some() && (sub == client_id || username.is_none()) {
            CallerKind::Agent
        } else {
            CallerKind::User
        };

        // Roles pass through verbatim — no normalization, that's app policy — except empties and
        // the reserved `account:` prefix: a tenant-managed role named `account:victim-org` must
        // not mint cross-tenant group grants.
        let mut groups = match &self.config.roles_claim {
            Some(claim) => resolve_claim(obj, claim)
                .map(roles_from)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        // The mirror group: this is the *sole* writer of the `account:` prefix.
        if let Some(acct) = &account {
            groups.push(format!("{ACCOUNT_PREFIX}{acct}"));
        }

        let scopes: Vec<Scope> = str_claim(obj, "scope")
            .map(|s| s.split_whitespace().map(Scope::from).collect())
            .unwrap_or_default();
        let trust = Trust {
            kind: TrustKind::Invocation,
            // Trust ceiling: a network-presented token yields at most Verified, whatever the
            // deployment configured.
            level: self.config.trust_level.min(TrustLevel::Verified),
            scopes,
        };
        let caller = Caller {
            principal: Principal { id, name, kind },
            groups,
            source: source.to_string(),
        };
        let exp = obj.get("exp").and_then(Value::as_u64);
        Ok((
            AuthContext {
                account,
                caller,
                trust,
            },
            exp,
        ))
    }
}

#[async_trait::async_trait]
impl RequestAuthenticator for Introspector {
    async fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthError> {
        self.authenticate_with_expiry(bearer)
            .await
            .map(|(ctx, _)| ctx)
    }

    async fn authenticate_with_expiry(
        &self,
        bearer: &str,
    ) -> Result<(AuthContext, Option<u64>), AuthError> {
        let claims = self.introspect(bearer).await?;
        self.map_claims(&claims)
    }
}

/// Resolve a claim **literal-key first** on the top-level object, dot-path walk only on miss —
/// namespaced OIDC claim names (`"https://idp.example/account"`) contain dots and must not be
/// silently split into a missing nested path.
fn resolve_claim<'a>(obj: &'a serde_json::Map<String, Value>, path: &str) -> Option<&'a Value> {
    if let Some(v) = obj.get(path) {
        return Some(v);
    }
    let mut parts = path.split('.');
    let mut cur = obj.get(parts.next()?)?;
    for part in parts {
        cur = cur.as_object()?.get(part)?;
    }
    Some(cur)
}

/// Stringify an account claim value: non-empty strings verbatim, numbers via their JSON display
/// (`42` → `"42"`). Anything else (bool/null/array/object, empty string) is "no account".
fn claim_to_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Roles claim → group values: a JSON array of strings or one space-separated string. Drops
/// empties and anything carrying the reserved `account:` prefix.
fn roles_from(v: &Value) -> Vec<String> {
    let raw: Vec<String> = match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        Value::String(s) => s.split_whitespace().map(str::to_string).collect(),
        _ => Vec::new(),
    };
    raw.into_iter()
        .filter(|r| !r.is_empty() && !r.starts_with(ACCOUNT_PREFIX))
        .collect()
}

/// A non-empty top-level string claim.
fn str_claim<'a>(obj: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Caching decorator
// ---------------------------------------------------------------------------

/// Default cap on a positive cache entry's lifetime (further bounded by the token's `exp`).
pub const DEFAULT_POSITIVE_TTL: Duration = Duration::from_secs(60);
/// Default lifetime of a negative (`Unauthorized`) entry.
pub const DEFAULT_NEGATIVE_TTL: Duration = Duration::from_secs(5);
/// Default capacity of the positive store.
pub const DEFAULT_POSITIVE_CAPACITY: usize = 1024;
/// Default capacity of the negative store.
pub const DEFAULT_NEGATIVE_CAPACITY: usize = 1024;

/// A caching decorator over any [`RequestAuthenticator`] (the introspector stays cache-free and
/// hermetically testable).
///
/// - **Keys are SHA-256(token)** — raw bearers never sit in a map, and hashing normalizes lookup
///   timing across token prefixes.
/// - **Segregated positive/negative stores**: negative inserts (`Unauthorized` only) can never
///   evict positive entries, so a garbage-token flood cannot churn out legitimate sessions and
///   hand the IdP the full load precisely under attack. The negative cache absorbs *repeated
///   identical* bad tokens; it does not protect the IdP from a unique-garbage flood — that is
///   upstream rate limiting.
/// - Positive TTL = `exp.saturating_sub(now).min(positive_ttl)`; `ttl == 0` (expired-but-active,
///   clock skew, millisecond-unit `exp`) → the result is returned but **never cached**.
/// - [`AuthError::Unavailable`] is never cached in either direction.
/// - No single-flight in v1: N concurrent first sights of one token may introspect N times.
pub struct CachedAuthenticator<A: RequestAuthenticator> {
    inner: A,
    positive_ttl: Duration,
    negative_ttl: Duration,
    state: Mutex<CacheState>,
}

struct CacheState {
    positive: Store<AuthContext>,
    negative: Store<()>,
}

impl<A: RequestAuthenticator> CachedAuthenticator<A> {
    /// Wrap `inner` with the design defaults (60 s / 5 s TTLs, 1024-entry stores).
    pub fn new(inner: A) -> Self {
        Self::with_limits(
            inner,
            DEFAULT_POSITIVE_TTL,
            DEFAULT_NEGATIVE_TTL,
            DEFAULT_POSITIVE_CAPACITY,
            DEFAULT_NEGATIVE_CAPACITY,
        )
    }

    /// Wrap `inner` with explicit TTLs and per-store capacities. A zero TTL or capacity disables
    /// that store.
    pub fn with_limits(
        inner: A,
        positive_ttl: Duration,
        negative_ttl: Duration,
        positive_capacity: usize,
        negative_capacity: usize,
    ) -> Self {
        Self {
            inner,
            positive_ttl,
            negative_ttl,
            state: Mutex::new(CacheState {
                positive: Store::new(positive_capacity),
                negative: Store::new(negative_capacity),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        // A poisoned lock only means another thread panicked mid-insert; the map itself is
        // always structurally valid, so keep serving rather than propagating the panic.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<A: RequestAuthenticator> fmt::Debug for CachedAuthenticator<A> {
    /// Manual: exposes counts and limits only — neither tokens nor their hashes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.lock();
        f.debug_struct("CachedAuthenticator")
            .field("positive_entries", &state.positive.map.len())
            .field("negative_entries", &state.negative.map.len())
            .field("positive_ttl", &self.positive_ttl)
            .field("negative_ttl", &self.negative_ttl)
            .field("positive_capacity", &state.positive.capacity)
            .field("negative_capacity", &state.negative.capacity)
            .finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl<A: RequestAuthenticator> RequestAuthenticator for CachedAuthenticator<A> {
    async fn authenticate(&self, bearer: &str) -> Result<AuthContext, AuthError> {
        let key = token_key(bearer);
        let now = Instant::now();
        {
            let mut state = self.lock();
            if let Some(ctx) = state.positive.get(&key, now) {
                return Ok(ctx);
            }
            if state.negative.get(&key, now).is_some() {
                return Err(AuthError::Unauthorized);
            }
        }
        match self.inner.authenticate_with_expiry(bearer).await {
            Ok((ctx, exp)) => {
                // Saturating: wrapping subtraction on a stale `exp` would cache an expired token
                // for the full window.
                let ttl_secs = match exp {
                    Some(exp) => exp
                        .saturating_sub(now_secs())
                        .min(self.positive_ttl.as_secs()),
                    None => self.positive_ttl.as_secs(),
                };
                if ttl_secs > 0 {
                    if let Some(deadline) =
                        Instant::now().checked_add(Duration::from_secs(ttl_secs))
                    {
                        self.lock().positive.insert(key, deadline, ctx.clone());
                    }
                }
                Ok(ctx)
            }
            Err(AuthError::Unauthorized) => {
                if !self.negative_ttl.is_zero() {
                    if let Some(deadline) = Instant::now().checked_add(self.negative_ttl) {
                        self.lock().negative.insert(key, deadline, ());
                    }
                }
                Err(AuthError::Unauthorized)
            }
            // Backend trouble is transient — caching it would turn a blip into an outage window.
            Err(e @ AuthError::Unavailable(_)) => Err(e),
        }
    }
}

/// SHA-256 of the token — the only form a bearer ever takes inside the cache.
fn token_key(token: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hasher.finalize().into()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One capacity-bounded TTL store: HashMap + insertion-order VecDeque, evict-oldest at capacity.
/// Expired entries are dropped on lookup; their queue slots go stale and are either skipped
/// during eviction or compacted when the queue outgrows twice the capacity.
struct Store<V> {
    capacity: usize,
    map: HashMap<[u8; 32], (Instant, V)>,
    order: VecDeque<[u8; 32]>,
}

impl<V: Clone> Store<V> {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&mut self, key: &[u8; 32], now: Instant) -> Option<V> {
        match self.map.get(key) {
            Some((deadline, _)) if *deadline <= now => {
                self.map.remove(key);
                None
            }
            Some((_, v)) => Some(v.clone()),
            None => None,
        }
    }

    fn insert(&mut self, key: [u8; 32], deadline: Instant, value: V) {
        if self.capacity == 0 {
            return;
        }
        // A refresh of an existing key replaces in place — no second queue slot.
        if let std::collections::hash_map::Entry::Occupied(mut e) = self.map.entry(key) {
            e.insert((deadline, value));
            return;
        }
        // Compact stale queue slots (expiry-removed entries) before they accumulate.
        if self.order.len() > self.capacity.saturating_mul(2) {
            self.order.retain(|k| self.map.contains_key(k));
        }
        while self.map.len() >= self.capacity {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
        self.map.insert(key, (deadline, value));
        self.order.push_back(key);
    }
}

// ---------------------------------------------------------------------------
// Tests — hermetic: a real local axum mock introspection endpoint per case.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use flux_policy::{
        evaluate, Action, AuthorizationPolicy, Decision, Grant, Request, ResourceKind, ResourceRef,
        SubjectKind, SubjectRef,
    };
    use serde_json::json;

    use super::*;
    use crate::{IdentityProvider, OidcIdentity};

    // -- mock IdP -----------------------------------------------------------

    struct MockResponse {
        status: u16,
        location: Option<String>,
        body: Vec<u8>,
        delay_ms: u64,
    }

    impl MockResponse {
        fn json(v: serde_json::Value) -> Self {
            Self {
                status: 200,
                location: None,
                body: v.to_string().into_bytes(),
                delay_ms: 0,
            }
        }

        fn raw(status: u16, body: &str) -> Self {
            Self {
                status,
                location: None,
                body: body.as_bytes().to_vec(),
                delay_ms: 0,
            }
        }

        fn redirect(location: &str) -> Self {
            Self {
                status: 307,
                location: Some(location.to_string()),
                body: Vec::new(),
                delay_ms: 0,
            }
        }

        fn delayed(mut self, ms: u64) -> Self {
            self.delay_ms = ms;
            self
        }
    }

    type Behavior = Box<dyn Fn(&str) -> MockResponse + Send + Sync>;

    struct MockState {
        behavior: Behavior,
        hits: AtomicUsize,
        forms: Mutex<Vec<Vec<(String, String)>>>,
        auth_headers: Mutex<Vec<Option<String>>>,
    }

    struct Mock {
        endpoint: String,
        state: Arc<MockState>,
    }

    impl Mock {
        fn hits(&self) -> usize {
            self.state.hits.load(Ordering::SeqCst)
        }
    }

    async fn mock_handler(
        axum::extract::State(st): axum::extract::State<Arc<MockState>>,
        headers: axum::http::HeaderMap,
        axum::extract::Form(pairs): axum::extract::Form<Vec<(String, String)>>,
    ) -> axum::response::Response {
        st.hits.fetch_add(1, Ordering::SeqCst);
        st.auth_headers.lock().unwrap().push(
            headers
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        );
        st.forms.lock().unwrap().push(pairs.clone());
        let token = pairs
            .iter()
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        let resp = (st.behavior)(token);
        if resp.delay_ms > 0 {
            tokio::time::sleep(Duration::from_millis(resp.delay_ms)).await;
        }
        let mut builder = axum::http::Response::builder().status(resp.status);
        if let Some(loc) = &resp.location {
            builder = builder.header(axum::http::header::LOCATION, loc);
        }
        builder
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(axum::body::Body::from(resp.body))
            .unwrap()
    }

    async fn spawn_mock(behavior: impl Fn(&str) -> MockResponse + Send + Sync + 'static) -> Mock {
        let state = Arc::new(MockState {
            behavior: Box::new(behavior),
            hits: AtomicUsize::new(0),
            forms: Mutex::new(Vec::new()),
            auth_headers: Mutex::new(Vec::new()),
        });
        let app = axum::Router::new()
            .route("/introspect", axum::routing::post(mock_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/introspect", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Mock { endpoint, state }
    }

    /// The tests' base config: http allowed (the mock is loopback http), generous timeout.
    fn test_config(endpoint: &str) -> IntrospectionConfig {
        let mut c = IntrospectionConfig::new(endpoint);
        c.allow_http = true;
        c.timeout = Duration::from_secs(5);
        c
    }

    fn introspector(mock: &Mock) -> Introspector {
        Introspector::new(test_config(&mock.endpoint)).unwrap()
    }

    fn unavailable(err: AuthError) -> bool {
        matches!(err, AuthError::Unavailable(_))
    }

    // -- contract: claims → identity mapping ---------------------------------

    #[tokio::test]
    async fn maps_active_token_to_full_context() {
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({
                "active": true,
                "sub": "u-1",
                "username": "alice",
                "scope": "workspace:read workspace:write",
                "roles": ["team-eng", "ops"],
            }))
        })
        .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.roles_claim = Some("roles".to_string());
        let auth = Introspector::new(cfg).unwrap();
        let ctx = auth.authenticate("tok-1").await.unwrap();
        assert_eq!(ctx.caller.principal.id, "u-1");
        assert_eq!(ctx.caller.principal.name, "alice");
        assert_eq!(ctx.caller.principal.kind, CallerKind::User);
        assert_eq!(ctx.caller.source, "introspect");
        assert_eq!(ctx.caller.groups, vec!["team-eng", "ops"]);
        assert_eq!(ctx.account, None);
        assert_eq!(ctx.trust.kind, TrustKind::Invocation);
        assert_eq!(ctx.trust.level, TrustLevel::Verified);
        assert_eq!(
            ctx.trust.scopes,
            vec![
                Scope::from("workspace:read"),
                Scope::from("workspace:write")
            ]
        );
    }

    #[tokio::test]
    async fn roles_accept_one_space_separated_string() {
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({"active": true, "sub": "u", "roles": " team-eng  ops "}))
        })
        .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.roles_claim = Some("roles".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.caller.groups, vec!["team-eng", "ops"]);
    }

    #[tokio::test]
    async fn account_claim_resolves_literal_key_first() {
        // A namespaced OIDC claim name contains dots and must resolve as one literal key; and
        // when a literal key shadows a nested path, the literal wins.
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({
                "active": true,
                "sub": "u",
                "https://idp.example/account": "acme",
                "info.accountId": "literal-wins",
                "info": {"accountId": "nested"},
            }))
        })
        .await;

        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("https://idp.example/account".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.account.as_deref(), Some("acme"));
        assert!(
            ctx.caller.groups.contains(&"account:acme".to_string()),
            "mirror group"
        );

        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("info.accountId".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.account.as_deref(), Some("literal-wins"));
    }

    #[tokio::test]
    async fn account_claim_walks_dot_path_on_literal_miss() {
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({
                "active": true,
                "sub": "u",
                "info": {"accountId": "nested"},
            }))
        })
        .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("info.accountId".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.account.as_deref(), Some("nested"));
        assert_eq!(ctx.caller.groups, vec!["account:nested"]);
    }

    #[tokio::test]
    async fn numeric_account_uses_json_display() {
        let mock =
            spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u", "acct": 42})))
                .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("acct".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.account.as_deref(), Some("42"));
        assert_eq!(ctx.caller.groups, vec!["account:42"]);
    }

    #[tokio::test]
    async fn require_account_fails_closed_on_missing_or_empty() {
        let mock = spawn_mock(|t| {
            if t == "empty" {
                MockResponse::json(json!({"active": true, "sub": "u", "acct": ""}))
            } else {
                MockResponse::json(json!({"active": true, "sub": "u"}))
            }
        })
        .await;

        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("acct".to_string());
        cfg.require_account = true;
        let auth = Introspector::new(cfg).unwrap();
        assert_eq!(
            auth.authenticate("missing").await.unwrap_err(),
            AuthError::Unauthorized
        );
        assert_eq!(
            auth.authenticate("empty").await.unwrap_err(),
            AuthError::Unauthorized
        );

        // Without require_account the same token authenticates with account = None.
        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("acct".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("missing")
            .await
            .unwrap();
        assert_eq!(ctx.account, None);
        assert!(
            ctx.caller.groups.is_empty(),
            "no mirror group without an account"
        );
    }

    #[tokio::test]
    async fn reserved_account_prefix_is_stripped_from_roles() {
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({
                "active": true,
                "sub": "u",
                "acct": "acme",
                "roles": ["admin", "account:victim-org", ""],
            }))
        })
        .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("acct".to_string());
        cfg.roles_claim = Some("roles".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        // The forged cross-tenant group is gone; the only `account:` group is the mirror the
        // authenticator itself wrote.
        assert_eq!(ctx.caller.groups, vec!["admin", "account:acme"]);
    }

    #[tokio::test]
    async fn principal_chain_falls_back_username_then_client_id() {
        let mock = spawn_mock(|t| match t {
            "user" => MockResponse::json(json!({"active": true, "username": "bob"})),
            _ => MockResponse::json(json!({"active": true, "client_id": "svc-1"})),
        })
        .await;
        let auth = introspector(&mock);

        let ctx = auth.authenticate("user").await.unwrap();
        assert_eq!(ctx.caller.principal.id, "bob");
        assert_eq!(ctx.caller.principal.name, "bob");
        assert_eq!(ctx.caller.principal.kind, CallerKind::User);

        let ctx = auth.authenticate("svc").await.unwrap();
        assert_eq!(ctx.caller.principal.id, "svc-1");
        assert_eq!(
            ctx.caller.principal.kind,
            CallerKind::Agent,
            "client_id-only is agent-shaped"
        );
    }

    #[tokio::test]
    async fn account_fallback_principal_is_namespaced() {
        let mock =
            spawn_mock(|_| MockResponse::json(json!({"active": true, "acct": "acme"}))).await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.account_claim = Some("acct".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.caller.principal.id, "account:acme");
        assert_eq!(ctx.caller.principal.name, "account:acme");
        assert_eq!(ctx.caller.source, "introspect:account-fallback");
        assert_eq!(ctx.account.as_deref(), Some("acme"));
        assert_eq!(ctx.caller.groups, vec!["account:acme"]);
    }

    #[tokio::test]
    async fn missing_principal_is_unauthorized() {
        let mock = spawn_mock(|_| MockResponse::json(json!({"active": true}))).await;
        let err = introspector(&mock).authenticate("t").await.unwrap_err();
        assert_eq!(err, AuthError::Unauthorized);
    }

    #[tokio::test]
    async fn caller_kind_heuristic_covers_both_branches() {
        let mock = spawn_mock(|t| match t {
            // sub == client_id → Agent even with a username present.
            "cc" => MockResponse::json(
                json!({"active": true, "sub": "svc", "client_id": "svc", "username": "ci-bot"}),
            ),
            // client_id present, username absent → Agent even when sub differs.
            "svc-sub" => {
                MockResponse::json(json!({"active": true, "sub": "u-9", "client_id": "app-1"}))
            }
            // username present, sub != client_id → User.
            "user" => MockResponse::json(
                json!({"active": true, "sub": "u-1", "client_id": "app-1", "username": "alice"}),
            ),
            // no client_id at all → User.
            _ => MockResponse::json(json!({"active": true, "sub": "u-2"})),
        })
        .await;
        let auth = introspector(&mock);
        let kind = |ctx: AuthContext| ctx.caller.principal.kind;
        assert_eq!(
            kind(auth.authenticate("cc").await.unwrap()),
            CallerKind::Agent
        );
        assert_eq!(
            kind(auth.authenticate("svc-sub").await.unwrap()),
            CallerKind::Agent
        );
        assert_eq!(
            kind(auth.authenticate("user").await.unwrap()),
            CallerKind::User
        );
        assert_eq!(
            kind(auth.authenticate("plain").await.unwrap()),
            CallerKind::User
        );
    }

    #[tokio::test]
    async fn trust_level_is_clamped_to_verified() {
        let mock = spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"}))).await;

        // Privileged is not derivable from a network-presented token.
        let mut cfg = test_config(&mock.endpoint);
        cfg.trust_level = TrustLevel::Privileged;
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.trust.level, TrustLevel::Verified);

        // The clamp is a ceiling, not a floor: Untrusted stays Untrusted.
        let mut cfg = test_config(&mock.endpoint);
        cfg.trust_level = TrustLevel::Untrusted;
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        assert_eq!(ctx.trust.level, TrustLevel::Untrusted);
    }

    #[tokio::test]
    async fn inactive_or_non_boolean_active_is_unauthorized() {
        let mock = spawn_mock(|t| match t {
            "false" => MockResponse::json(json!({"active": false, "sub": "u"})),
            "string" => MockResponse::json(json!({"active": "true", "sub": "u"})),
            _ => MockResponse::json(json!({"sub": "u"})),
        })
        .await;
        let auth = introspector(&mock);
        for t in ["false", "string", "missing"] {
            assert_eq!(
                auth.authenticate(t).await.unwrap_err(),
                AuthError::Unauthorized,
                "{t}"
            );
        }
    }

    #[tokio::test]
    async fn non_access_token_types_are_unauthorized() {
        // The token value doubles as the reported type, exercising the case-insensitive check.
        let mock = spawn_mock(|t| match t {
            "use-refresh" => {
                MockResponse::json(json!({"active": true, "sub": "u", "token_use": "refresh"}))
            }
            "typ-id" => MockResponse::json(json!({"active": true, "sub": "u", "typ": "id_token"})),
            other => MockResponse::json(json!({"active": true, "sub": "u", "token_type": other})),
        })
        .await;
        let auth = introspector(&mock);
        for t in [
            "refresh_token",
            "refresh",
            "id_token",
            "Refresh",
            // Separator/casing variants that an exact allowlist would have let through:
            "Refresh Token",
            "refresh-token",
            "REFRESH_TOKEN",
            "logout_token",
            "use-refresh",
            "typ-id",
        ] {
            assert_eq!(
                auth.authenticate(t).await.unwrap_err(),
                AuthError::Unauthorized,
                "{t}"
            );
        }
        for t in ["Bearer", "access_token", "access token", "at+jwt"] {
            assert!(
                auth.authenticate(t).await.is_ok(),
                "{t} must pass as an access token"
            );
        }
    }

    // -- contract: transport failures ----------------------------------------

    #[tokio::test]
    async fn http_500_is_unavailable() {
        let mock = spawn_mock(|_| MockResponse::raw(500, "{}")).await;
        assert!(unavailable(
            introspector(&mock).authenticate("t").await.unwrap_err()
        ));
    }

    #[tokio::test]
    async fn timeout_is_unavailable() {
        let mock =
            spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"})).delayed(2_000))
                .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.timeout = Duration::from_millis(250);
        let err = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap_err();
        assert!(unavailable(err));
    }

    #[tokio::test]
    async fn malformed_json_is_unavailable() {
        let mock = spawn_mock(|t| match t {
            "array" => MockResponse::raw(200, "[1,2,3]"),
            _ => MockResponse::raw(200, "{not json"),
        })
        .await;
        let auth = introspector(&mock);
        assert!(unavailable(auth.authenticate("junk").await.unwrap_err()));
        assert!(unavailable(auth.authenticate("array").await.unwrap_err()));
    }

    #[tokio::test]
    async fn over_cap_body_is_unavailable() {
        // axum sets Content-Length for a fixed body, so this exercises the header check.
        let mock = spawn_mock(|_| MockResponse::raw(200, &"a".repeat(300 * 1024))).await;
        assert!(unavailable(
            introspector(&mock).authenticate("t").await.unwrap_err()
        ));
    }

    #[tokio::test]
    async fn chunked_over_cap_body_is_unavailable() {
        // A raw chunked response carries no Content-Length — this exercises the limited read.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/introspect", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let head =
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ntransfer-encoding: chunked\r\n\r\n";
            if sock.write_all(head).await.is_err() {
                return;
            }
            let chunk = format!("400\r\n{}\r\n", "a".repeat(0x400));
            for _ in 0..400 {
                if sock.write_all(chunk.as_bytes()).await.is_err() {
                    return; // introspector hit its cap and hung up — expected
                }
            }
            let _ = sock.write_all(b"0\r\n\r\n").await;
        });
        let auth = Introspector::new(test_config(&endpoint)).unwrap();
        assert!(unavailable(auth.authenticate("t").await.unwrap_err()));
    }

    #[tokio::test]
    async fn redirects_are_refused_not_followed() {
        // A 307 preserves the POST body — following it would forward the caller's live bearer
        // token to wherever Location points. The second listener must see nothing.
        let target = spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"}))).await;
        let loc = target.endpoint.clone();
        let mock = spawn_mock(move |_| MockResponse::redirect(&loc)).await;
        let err = introspector(&mock).authenticate("t").await.unwrap_err();
        assert!(unavailable(err));
        assert_eq!(mock.hits(), 1);
        assert_eq!(target.hits(), 0, "redirect target must never be contacted");
    }

    #[tokio::test]
    async fn token_with_form_metacharacters_arrives_as_one_field() {
        let token = "a&token=evil=1%25 x";
        let mock = spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"}))).await;
        introspector(&mock).authenticate(token).await.unwrap();
        let forms = mock.state.forms.lock().unwrap();
        assert_eq!(forms.len(), 1);
        let token_fields: Vec<&str> = forms[0]
            .iter()
            .filter(|(k, _)| k == "token")
            .map(|(_, v)| v.as_str())
            .collect();
        assert_eq!(
            token_fields,
            vec![token],
            "exactly one token field, byte-exact"
        );
        assert!(
            forms[0].contains(&("token_type_hint".to_string(), "access_token".to_string())),
            "token_type_hint present"
        );
    }

    #[tokio::test]
    async fn client_secret_basic_is_sent_when_configured() {
        let mock = spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"}))).await;

        let mut cfg = test_config(&mock.endpoint);
        cfg.client = Some(("cid".to_string(), "s3cr3t".to_string()));
        Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();
        introspector(&mock).authenticate("t2").await.unwrap();

        let headers = mock.state.auth_headers.lock().unwrap();
        assert_eq!(headers[0].as_deref(), Some("Basic Y2lkOnMzY3IzdA=="));
        assert_eq!(headers[1], None, "client: None sends a bare POST");
    }

    #[test]
    fn https_is_required_unless_allow_http() {
        let err = Introspector::new(IntrospectionConfig::new("http://idp.local/introspect"));
        assert!(
            err.is_err(),
            "http endpoint without allow_http must fail construction"
        );

        let mut cfg = IntrospectionConfig::new("http://idp.local/introspect");
        cfg.allow_http = true;
        assert!(Introspector::new(cfg).is_ok());

        assert!(
            Introspector::new(IntrospectionConfig::new("https://idp.example/introspect")).is_ok()
        );

        let mut cfg = IntrospectionConfig::new("ftp://idp.local/introspect");
        cfg.allow_http = true;
        assert!(
            Introspector::new(cfg).is_err(),
            "non-http(s) scheme is always rejected"
        );
    }

    // -- cache ----------------------------------------------------------------

    #[tokio::test]
    async fn cache_hit_within_ttl_performs_no_network_call() {
        // No exp claim → positive_ttl bounds the entry.
        let mock = spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"}))).await;
        let cached = CachedAuthenticator::new(introspector(&mock));
        cached.authenticate("tok").await.unwrap();
        let ctx = cached.authenticate("tok").await.unwrap();
        assert_eq!(ctx.caller.principal.id, "u");
        assert_eq!(mock.hits(), 1, "second call must be served from cache");
    }

    #[tokio::test]
    async fn expired_exp_returns_result_but_never_caches() {
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({"active": true, "sub": "u", "exp": now_secs() - 100}))
        })
        .await;
        let cached = CachedAuthenticator::new(introspector(&mock));
        for expected_hits in 1..=3 {
            cached.authenticate("tok").await.unwrap();
            assert_eq!(
                mock.hits(),
                expected_hits,
                "ttl==0 entries are never inserted"
            );
        }
    }

    #[tokio::test]
    async fn millisecond_unit_exp_is_capped_not_panicking() {
        // An IdP emitting exp in milliseconds looks far-future; the positive TTL caps it.
        let mock = spawn_mock(|_| {
            MockResponse::json(
                json!({"active": true, "sub": "u", "exp": (now_secs() + 3600) * 1000}),
            )
        })
        .await;
        let cached = CachedAuthenticator::new(introspector(&mock));
        cached.authenticate("tok").await.unwrap();
        cached.authenticate("tok").await.unwrap();
        assert_eq!(mock.hits(), 1);
    }

    #[tokio::test]
    async fn negative_cache_absorbs_repeated_identical_bad_token() {
        let mock = spawn_mock(|_| MockResponse::json(json!({"active": false}))).await;
        let cached = CachedAuthenticator::new(introspector(&mock));
        for _ in 0..3 {
            assert_eq!(
                cached.authenticate("bad").await.unwrap_err(),
                AuthError::Unauthorized
            );
        }
        assert_eq!(
            mock.hits(),
            1,
            "one network call within the negative window"
        );
    }

    #[tokio::test]
    async fn negative_flood_cannot_evict_positive_entries() {
        let mock = spawn_mock(|t| {
            if t == "good" {
                MockResponse::json(json!({"active": true, "sub": "u"}))
            } else {
                MockResponse::json(json!({"active": false}))
            }
        })
        .await;
        let cached = CachedAuthenticator::new(introspector(&mock));
        cached.authenticate("good").await.unwrap();
        for i in 0..2_000 {
            let _ = cached.authenticate(&format!("garbage-{i}")).await;
        }
        assert_eq!(mock.hits(), 2_001);
        cached.authenticate("good").await.unwrap();
        assert_eq!(
            mock.hits(),
            2_001,
            "positive entry survived the negative flood"
        );
    }

    #[tokio::test]
    async fn unavailable_is_never_cached() {
        let mock = spawn_mock(|_| MockResponse::raw(500, "{}")).await;
        let cached = CachedAuthenticator::new(introspector(&mock));
        assert!(unavailable(cached.authenticate("tok").await.unwrap_err()));
        assert!(unavailable(cached.authenticate("tok").await.unwrap_err()));
        assert_eq!(
            mock.hits(),
            2,
            "backend trouble must be retried, not remembered"
        );
    }

    #[tokio::test]
    async fn positive_store_evicts_oldest_at_capacity() {
        let mock = spawn_mock(|t| {
            let sub = t.to_string();
            MockResponse::json(json!({"active": true, "sub": sub}))
        })
        .await;
        let cached = CachedAuthenticator::with_limits(
            introspector(&mock),
            DEFAULT_POSITIVE_TTL,
            DEFAULT_NEGATIVE_TTL,
            2,
            2,
        );
        cached.authenticate("t1").await.unwrap(); // hits 1
        cached.authenticate("t2").await.unwrap(); // hits 2
        cached.authenticate("t3").await.unwrap(); // hits 3, evicts t1
        assert_eq!(mock.hits(), 3);
        cached.authenticate("t3").await.unwrap(); // cached
        assert_eq!(mock.hits(), 3);
        cached.authenticate("t1").await.unwrap(); // evicted → network, evicts t2 (store: t3, t1)
        assert_eq!(mock.hits(), 4);
        cached.authenticate("t2").await.unwrap(); // evicted → network, evicts t3 (store: t1, t2)
        assert_eq!(mock.hits(), 5);
        cached.authenticate("t1").await.unwrap(); // still cached
        assert_eq!(mock.hits(), 5);
    }

    // -- leak / redaction -------------------------------------------------------

    #[tokio::test]
    async fn debug_output_carries_no_secret_and_no_token() {
        let mut cfg = IntrospectionConfig::new("https://idp.example/introspect");
        cfg.client = Some(("cid".to_string(), "s3cr3t".to_string()));
        let cfg_dbg = format!("{cfg:?}");
        assert!(cfg_dbg.contains("cid"), "client id is fine to log");
        assert!(
            !cfg_dbg.contains("s3cr3t"),
            "secret must be redacted: {cfg_dbg}"
        );

        let auth = Introspector::new(cfg).unwrap();
        assert!(!format!("{auth:?}").contains("s3cr3t"));

        let mock = spawn_mock(|_| MockResponse::json(json!({"active": true, "sub": "u"}))).await;
        let token = "super-secret-bearer-token";
        let cached = CachedAuthenticator::new(introspector(&mock));
        cached.authenticate(token).await.unwrap();
        let cache_dbg = format!("{cached:?}");
        assert!(
            !cache_dbg.contains(token),
            "no raw token in cache Debug: {cache_dbg}"
        );
        assert!(
            cache_dbg.contains("positive_entries: 1"),
            "counts only: {cache_dbg}"
        );
    }

    // -- shared projection --------------------------------------------------------

    #[tokio::test]
    async fn introspected_identity_evaluates_like_oidc_identity() {
        // The same claims, reached via introspection or via OidcIdentity::from_claims, must be
        // indistinguishable to flux_policy::evaluate — one projection, one authorization SoT.
        let mock = spawn_mock(|_| {
            MockResponse::json(json!({
                "active": true,
                "sub": "u-1",
                "username": "Alice",
                "roles": ["team-eng"],
                "scope": "workspace:write",
            }))
        })
        .await;
        let mut cfg = test_config(&mock.endpoint);
        cfg.roles_claim = Some("roles".to_string());
        let ctx = Introspector::new(cfg)
            .unwrap()
            .authenticate("t")
            .await
            .unwrap();

        let (oidc_caller, oidc_trust) = OidcIdentity::from_claims(
            "u-1",
            "Alice",
            vec!["team-eng".to_string()],
            TrustLevel::Verified,
            vec!["workspace:write".to_string()],
        )
        .resolve();

        let policy = AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::Group,
                    id: "team-eng".to_string(),
                }],
                resources: vec![ResourceRef::any(ResourceKind::Operation)],
                actions: vec![Action::from("workspace.read")],
                required_trust: TrustLevel::Verified,
                required_scopes: vec![Scope::from("workspace:write")],
                requires_approval: false,
            }],
        };
        let resource = ResourceRef::any(ResourceKind::Operation);
        for (action, expected) in [
            ("workspace.read", Decision::Allow),
            ("process.exec", Decision::Deny),
        ] {
            let action = Action::from(action);
            let via_introspect = evaluate(
                &policy,
                &Request {
                    caller: &ctx.caller,
                    trust: &ctx.trust,
                    action: &action,
                    resource: &resource,
                },
            );
            let via_oidc = evaluate(
                &policy,
                &Request {
                    caller: &oidc_caller,
                    trust: &oidc_trust,
                    action: &action,
                    resource: &resource,
                },
            );
            assert_eq!(
                via_introspect.decision, expected,
                "{action:?} via introspection"
            );
            assert_eq!(via_oidc.decision, expected, "{action:?} via oidc");
        }
    }
}
