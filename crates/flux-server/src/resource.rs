//! Principal-aware admission and provider-budget accounting for daemon work.
//!
//! This is deliberately in-process. A replica owns only the work it can actually cancel, while a
//! reverse proxy (or another shared control plane) must enforce deployment-wide limits across
//! replicas. State is cardinality-bounded and stale idle buckets are swept on admission.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

use flux_auth::request::AuthContext;

use crate::{ServerAuth, ServerLimits};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BudgetKey(String);

impl BudgetKey {
    fn resolve(auth: &ServerAuth, ctx: Option<&AuthContext>) -> Result<Self, Box<Response>> {
        match auth {
            ServerAuth::Open => Ok(Self("open-loopback".into())),
            // Never retain, hash, or log the bearer value. Shared-secret mode is one configured
            // authentication realm, so one constant bucket is the intended tenancy boundary.
            ServerAuth::SharedSecret { .. } => Ok(Self("shared-secret-realm".into())),
            ServerAuth::Principal(_) => ctx
                .map(|ctx| {
                    // Bound attacker-controlled IdP claim bytes in resident state. A collision can
                    // only merge two callers into a stricter shared bucket; it cannot grant work.
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    ctx.caller.principal.id.hash(&mut hasher);
                    Self(format!("principal:{:016x}", hasher.finish()))
                })
                .ok_or_else(|| Box::new(crate::unauthorized())),
        }
    }
}

#[derive(Debug)]
struct Bucket {
    rate_started: Instant,
    requests: u32,
    budget_started: Instant,
    provider_calls: u64,
    provider_spend_usd: f64,
    inflight: usize,
    last_seen: Instant,
}

impl Bucket {
    fn new(now: Instant) -> Self {
        Self {
            rate_started: now,
            requests: 0,
            budget_started: now,
            provider_calls: 0,
            provider_spend_usd: 0.0,
            inflight: 0,
            last_seen: now,
        }
    }

    fn roll_windows(&mut self, now: Instant, limits: &ServerLimits) {
        if now.duration_since(self.rate_started) >= limits.request_rate_window {
            self.rate_started = now;
            self.requests = 0;
        }
        if now.duration_since(self.budget_started) >= limits.provider_budget_window {
            self.budget_started = now;
            self.provider_calls = 0;
            self.provider_spend_usd = 0.0;
        }
        self.last_seen = now;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LimitKind {
    RequestRate,
    Concurrency,
    ProviderCalls,
    ProviderSpend,
    StateCapacity,
}

impl LimitKind {
    fn wire_name(self) -> &'static str {
        match self {
            Self::RequestRate => "request_rate",
            Self::Concurrency => "concurrency",
            Self::ProviderCalls => "provider_calls",
            Self::ProviderSpend => "provider_spend",
            Self::StateCapacity => "state_capacity",
        }
    }
}

#[derive(Debug)]
struct Rejection {
    kind: LimitKind,
    retry_after: Duration,
}

struct GovernorInner {
    limits: ServerLimits,
    buckets: Mutex<HashMap<BudgetKey, Bucket>>,
}

/// One in-process resource governor shared by every REST, webhook, and A2A work route.
#[derive(Clone)]
pub(crate) struct ResourceGovernor(Arc<GovernorInner>);

/// Request-rate and live-work admission for one authenticated channel binding. A webhook token or
/// signature authenticates the deployment realm rather than an end-user principal, so all callers
/// share one stricter bucket and no credential material is retained or hashed.
#[derive(Clone)]
pub struct SharedIngressGovernor(ResourceGovernor);

impl SharedIngressGovernor {
    pub fn new(limits: ServerLimits) -> Self {
        Self(ResourceGovernor::new(limits))
    }

    pub fn admit_request(&self) -> Result<(), Box<Response>> {
        self.0
            .admit_request_key(BudgetKey("channel-shared-realm".into()))
            .map_err(|rejection| Box::new(rejection_response(rejection)))
    }

    pub fn admit_work(&self) -> Result<IngressPermit, Box<Response>> {
        self.0
            .admit_work_key(BudgetKey("channel-shared-realm".into()))
            .map(|permit| IngressPermit { _permit: permit })
            .map_err(|rejection| Box::new(rejection_response(rejection)))
    }
}

/// RAII ownership of one channel-delivery slot. Provider budgets remain an App/runtime concern:
/// one delivery may fan out into several journey turns and has no single durable turn id that can
/// be attributed to this HTTP request without guessing.
pub struct IngressPermit {
    _permit: WorkPermit,
}

impl ResourceGovernor {
    pub(crate) fn new(limits: ServerLimits) -> Self {
        Self(Arc::new(GovernorInner {
            limits,
            buckets: Mutex::new(HashMap::new()),
        }))
    }

    /// Count one authenticated request before its handler runs. Public health/discovery routes do
    /// not use this gate; every protected REST/A2A route does, including read-only routes.
    pub(crate) fn admit_request(
        &self,
        auth: &ServerAuth,
        ctx: Option<&AuthContext>,
    ) -> Result<(), Box<Response>> {
        let key = BudgetKey::resolve(auth, ctx)?;
        self.admit_request_key(key)
            .map_err(|rejection| self.reject(auth, rejection))
    }

    /// Reserve one live-work slot after request-rate admission. Provider call/spend totals are
    /// completed-usage circuit breakers: they reject new work after the threshold is observed,
    /// while already-admitted turns remain bounded by the concurrency limit.
    pub(crate) fn admit_work(
        &self,
        auth: &ServerAuth,
        ctx: Option<&AuthContext>,
    ) -> Result<WorkPermit, Box<Response>> {
        let key = BudgetKey::resolve(auth, ctx)?;
        self.admit_work_key(key)
            .map_err(|rejection| self.reject(auth, rejection))
    }

    fn reject(&self, auth: &ServerAuth, rejection: Rejection) -> Box<Response> {
        let bucket = match auth {
            ServerAuth::Open => "open-loopback",
            ServerAuth::SharedSecret { .. } => "shared-secret-realm",
            ServerAuth::Principal(_) => "principal",
        };
        // Metrics-shaped, secret-free operational signal. The response carries the same
        // dimension in a header; neither path contains a token or shared-secret value.
        eprintln!(
            "flux_server_limit_rejections_total{{limit=\"{}\",bucket=\"{}\"}} += 1",
            rejection.kind.wire_name(),
            bucket
        );
        Box::new(rejection_response(rejection))
    }

    fn admit_request_key(&self, key: BudgetKey) -> Result<(), Rejection> {
        let now = Instant::now();
        let limits = self.0.limits;
        let mut buckets = self.0.buckets.lock().unwrap();
        sweep(&mut buckets, now, &limits);
        ensure_capacity(&buckets, &key, &limits)?;
        let bucket = buckets.entry(key).or_insert_with(|| Bucket::new(now));
        bucket.roll_windows(now, &limits);
        if bucket.requests >= limits.requests_per_window {
            return Err(Rejection {
                kind: LimitKind::RequestRate,
                retry_after: limits
                    .request_rate_window
                    .saturating_sub(now.duration_since(bucket.rate_started)),
            });
        }
        bucket.requests += 1;
        Ok(())
    }

    fn admit_work_key(&self, key: BudgetKey) -> Result<WorkPermit, Rejection> {
        let now = Instant::now();
        let limits = self.0.limits;
        let mut buckets = self.0.buckets.lock().unwrap();
        sweep(&mut buckets, now, &limits);
        ensure_capacity(&buckets, &key, &limits)?;
        let bucket = buckets
            .entry(key.clone())
            .or_insert_with(|| Bucket::new(now));
        bucket.roll_windows(now, &limits);

        if bucket.provider_calls >= limits.provider_calls_per_window {
            return Err(Rejection {
                kind: LimitKind::ProviderCalls,
                retry_after: limits
                    .provider_budget_window
                    .saturating_sub(now.duration_since(bucket.budget_started)),
            });
        }
        if bucket.provider_spend_usd >= limits.provider_spend_usd_per_window {
            return Err(Rejection {
                kind: LimitKind::ProviderSpend,
                retry_after: limits
                    .provider_budget_window
                    .saturating_sub(now.duration_since(bucket.budget_started)),
            });
        }
        if bucket.inflight >= limits.max_inflight_per_key {
            return Err(Rejection {
                kind: LimitKind::Concurrency,
                retry_after: Duration::from_secs(1),
            });
        }
        bucket.inflight += 1;
        drop(buckets);
        Ok(WorkPermit {
            governor: self.clone(),
            key: Some(key),
            tracked: None,
        })
    }

    fn release(&self, key: &BudgetKey) {
        let mut buckets = self.0.buckets.lock().unwrap();
        if let Some(bucket) = buckets.get_mut(key) {
            bucket.inflight = bucket.inflight.saturating_sub(1);
            bucket.last_seen = Instant::now();
        }
    }

    fn record_provider_delta(&self, key: &BudgetKey, calls: u64, spend_usd: f64) {
        let now = Instant::now();
        let mut buckets = self.0.buckets.lock().unwrap();
        let bucket = buckets
            .entry(key.clone())
            .or_insert_with(|| Bucket::new(now));
        bucket.roll_windows(now, &self.0.limits);
        bucket.provider_calls = bucket.provider_calls.saturating_add(calls);
        bucket.provider_spend_usd += spend_usd.max(0.0);
    }

    #[cfg(test)]
    fn bucket_count(&self) -> usize {
        self.0.buckets.lock().unwrap().len()
    }

    #[cfg(test)]
    fn provider_calls(&self, key: &BudgetKey) -> u64 {
        self.0
            .buckets
            .lock()
            .unwrap()
            .get(key)
            .map(|bucket| bucket.provider_calls)
            .unwrap_or(0)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ProviderTotals {
    calls: u64,
    spend_usd: f64,
}

struct TrackedTurn {
    events: Arc<flux_events::EventStore>,
    session_id: String,
    turn_id: i64,
}

fn provider_totals_for_turn(
    events: &flux_events::EventStore,
    session_id: &str,
    turn_id: i64,
) -> ProviderTotals {
    let pricing = flux_credentials::load_pricing_table();
    match events.load_turn(session_id, turn_id) {
        Ok(events) => flux_events::cost_summary(&events, &pricing)
            .into_iter()
            .fold(ProviderTotals::default(), |mut acc, row| {
                acc.calls = acc.calls.saturating_add(row.calls);
                acc.spend_usd += row.cost.map(|cost| cost.usd).unwrap_or(0.0);
                acc
            }),
        Err(error) => {
            eprintln!("flux_server_budget_accounting_errors_total += 1 ({error})");
            ProviderTotals::default()
        }
    }
}

/// RAII ownership of one live-work slot. Background and SSE tasks own this permit for their full
/// lifetime. Completed provider call/spend deltas are folded from durable call-usage events on
/// drop, including failure and cancellation paths. Each permit is bound to the exact durable turn
/// id while the engine gate is held, so overlapping principals cannot claim one another's calls.
pub(crate) struct WorkPermit {
    governor: ResourceGovernor,
    key: Option<BudgetKey>,
    tracked: Option<TrackedTurn>,
}

impl WorkPermit {
    pub(crate) fn track_turn(
        &mut self,
        events: Arc<flux_events::EventStore>,
        session_id: &str,
        turn_id: i64,
    ) {
        if self.key.is_none() || self.tracked.is_some() {
            return;
        }
        self.tracked = Some(TrackedTurn {
            events,
            session_id: session_id.to_string(),
            turn_id,
        });
    }
}

impl Drop for WorkPermit {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        if let Some(tracked) = self.tracked.take() {
            let totals =
                provider_totals_for_turn(&tracked.events, &tracked.session_id, tracked.turn_id);
            self.governor
                .record_provider_delta(&key, totals.calls, totals.spend_usd);
        }
        self.governor.release(&key);
    }
}

fn rejection_response(rejection: Rejection) -> Response {
    // HTTP Retry-After is integral seconds. Round up so the advertised delay never lands just
    // before the fixed window actually resets (Duration::as_secs truncates).
    let retry_after = rejection
        .retry_after
        .as_secs()
        .saturating_add(u64::from(rejection.retry_after.subsec_nanos() != 0))
        .max(1);
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        [("x-flux-limit", rejection.kind.wire_name())],
        "resource limit exceeded",
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry_after.to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

fn ensure_capacity(
    buckets: &HashMap<BudgetKey, Bucket>,
    key: &BudgetKey,
    limits: &ServerLimits,
) -> Result<(), Rejection> {
    if !buckets.contains_key(key) && buckets.len() >= limits.max_resource_keys {
        return Err(Rejection {
            kind: LimitKind::StateCapacity,
            retry_after: limits.request_rate_window,
        });
    }
    Ok(())
}

fn sweep(buckets: &mut HashMap<BudgetKey, Bucket>, now: Instant, limits: &ServerLimits) {
    let stale_after = limits
        .provider_budget_window
        .max(limits.request_rate_window)
        .saturating_mul(2);
    buckets.retain(|_, bucket| {
        bucket.inflight > 0 || now.duration_since(bucket.last_seen) < stale_after
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_policy::{Caller, CallerKind, Principal, Trust, TrustKind, TrustLevel};

    fn limits() -> ServerLimits {
        ServerLimits {
            requests_per_window: 2,
            request_rate_window: Duration::from_secs(60),
            max_inflight_per_key: 1,
            provider_calls_per_window: 2,
            provider_spend_usd_per_window: 1.0,
            provider_budget_window: Duration::from_secs(60),
            max_resource_keys: 2,
            ..ServerLimits::default()
        }
    }

    #[test]
    fn concurrency_and_request_rate_are_independent_gates() {
        let governor = ResourceGovernor::new(limits());
        let key = BudgetKey("principal:alice".into());
        governor.admit_request_key(key.clone()).unwrap();
        governor.admit_request_key(key.clone()).unwrap();
        let rate = governor.admit_request_key(key.clone()).unwrap_err();
        assert_eq!(rate.kind, LimitKind::RequestRate);

        let first = governor.admit_work_key(key.clone()).unwrap();
        let concurrent = governor.admit_work_key(key.clone()).err().unwrap();
        assert_eq!(concurrent.kind, LimitKind::Concurrency);
        drop(first);
        governor.admit_work_key(key).unwrap();
    }

    #[test]
    fn completed_provider_usage_breakers_reject_new_work() {
        let governor = ResourceGovernor::new(limits());
        let calls = BudgetKey("principal:calls".into());
        governor.record_provider_delta(&calls, 2, 0.0);
        assert_eq!(
            governor.admit_work_key(calls).err().unwrap().kind,
            LimitKind::ProviderCalls
        );

        let spend = BudgetKey("principal:spend".into());
        governor.record_provider_delta(&spend, 0, 1.0);
        assert_eq!(
            governor.admit_work_key(spend).err().unwrap().kind,
            LimitKind::ProviderSpend
        );
    }

    #[test]
    fn bucket_state_is_cardinality_bounded() {
        let governor = ResourceGovernor::new(limits());
        for id in 0..2 {
            let permit = governor
                .admit_work_key(BudgetKey(format!("principal:{id}")))
                .unwrap();
            drop(permit);
        }
        let full = governor
            .admit_work_key(BudgetKey("principal:overflow".into()))
            .err()
            .unwrap();
        assert_eq!(full.kind, LimitKind::StateCapacity);
        assert_eq!(governor.bucket_count(), 2);
    }

    #[test]
    fn principal_identity_is_the_key_even_inside_one_account_realm() {
        let auth = ServerAuth::Principal(crate::PrincipalAuth::new(
            Arc::new(UnusedAuthenticator),
            "https://example.test",
        ));
        let context = |id: &str| AuthContext {
            account: Some("shared-account".into()),
            caller: Caller {
                principal: Principal {
                    id: id.into(),
                    name: id.into(),
                    kind: CallerKind::User,
                },
                groups: Vec::new(),
                source: "test".into(),
            },
            trust: Trust {
                kind: TrustKind::Invocation,
                level: TrustLevel::Verified,
                scopes: Vec::new(),
            },
        };
        assert_ne!(
            BudgetKey::resolve(&auth, Some(&context("alice"))).unwrap(),
            BudgetKey::resolve(&auth, Some(&context("bob"))).unwrap()
        );
    }

    #[test]
    fn permit_drop_charges_durable_provider_calls_before_the_next_admission() {
        let limits = ServerLimits {
            requests_per_window: 100,
            provider_calls_per_window: 1,
            provider_spend_usd_per_window: 100.0,
            ..limits()
        };
        let governor = ResourceGovernor::new(limits);
        let key = BudgetKey("principal:alice".into());
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let mut permit = governor.admit_work_key(key.clone()).unwrap();
        let turn = events.begin_turn(&sid, "hi", "claude-sonnet-4-6").unwrap();
        permit.track_turn(events.clone(), &sid, turn);
        events
            .record_call_usage(
                &sid,
                turn,
                "claude-sonnet-4-6",
                flux_core::Usage {
                    input_tokens: 1_000,
                    output_tokens: 10,
                    ..Default::default()
                },
            )
            .unwrap();
        drop(permit);

        assert_eq!(
            governor.admit_work_key(key).err().unwrap().kind,
            LimitKind::ProviderCalls
        );
    }

    #[test]
    fn overlapping_same_session_principals_charge_their_exact_turn_when_dropped_in_reverse() {
        let limits = ServerLimits {
            max_inflight_per_key: 2,
            provider_calls_per_window: 100,
            provider_spend_usd_per_window: 100.0,
            ..limits()
        };
        let governor = ResourceGovernor::new(limits);
        let alice = BudgetKey("principal:alice".into());
        let bob = BudgetKey("principal:bob".into());
        let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
        let sid = events.create_session("claude-sonnet-4-6").unwrap();
        let mut first = governor.admit_work_key(alice.clone()).unwrap();
        let mut second = governor.admit_work_key(bob.clone()).unwrap();
        let first_turn = events
            .begin_turn(&sid, "first", "claude-sonnet-4-6")
            .unwrap();
        first.track_turn(events.clone(), &sid, first_turn);
        events
            .record_call_usage(
                &sid,
                first_turn,
                "claude-sonnet-4-6",
                flux_core::Usage::default(),
            )
            .unwrap();
        let second_turn = events
            .begin_turn(&sid, "second", "claude-sonnet-4-6")
            .unwrap();
        second.track_turn(events.clone(), &sid, second_turn);
        events
            .record_call_usage(
                &sid,
                second_turn,
                "claude-sonnet-4-6",
                flux_core::Usage::default(),
            )
            .unwrap();
        drop(second);
        assert_eq!(governor.provider_calls(&bob), 1);
        assert_eq!(governor.provider_calls(&alice), 0);

        drop(first);
        assert_eq!(governor.provider_calls(&alice), 1);
        assert_eq!(governor.provider_calls(&bob), 1);
    }

    #[test]
    fn completed_usage_breaker_bounds_overshoot_by_inflight_work() {
        let limits = ServerLimits {
            max_inflight_per_key: 2,
            provider_calls_per_window: 1,
            ..limits()
        };
        let governor = ResourceGovernor::new(limits);
        let key = BudgetKey("principal:alice".into());

        // Two turns may be admitted before either has completed usage to charge. The concurrency
        // gate bounds that retrospective circuit-breaker overshoot.
        let first = governor.admit_work_key(key.clone()).unwrap();
        let second = governor.admit_work_key(key.clone()).unwrap();
        let concurrency = governor.admit_work_key(key.clone()).err().unwrap();
        assert_eq!(concurrency.kind, LimitKind::Concurrency);

        governor.record_provider_delta(&key, 1, 0.0);
        drop(first);
        let provider_calls = governor.admit_work_key(key).err().unwrap();
        assert_eq!(provider_calls.kind, LimitKind::ProviderCalls);
        drop(second);
    }

    #[test]
    fn retry_after_rounds_fractional_seconds_up() {
        let response = rejection_response(Rejection {
            kind: LimitKind::RequestRate,
            retry_after: Duration::from_millis(1_001),
        });
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
    }

    struct UnusedAuthenticator;

    #[async_trait::async_trait]
    impl flux_auth::request::RequestAuthenticator for UnusedAuthenticator {
        async fn authenticate(
            &self,
            _bearer: &str,
        ) -> Result<AuthContext, flux_auth::request::AuthError> {
            unreachable!("key-resolution test never authenticates")
        }
    }
}
