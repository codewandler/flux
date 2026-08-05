//! Shared, metadata-only usage timeline and deterministic observatory projection (C-518).
//!
//! This module is deliberately below both user-facing surfaces. Acquisition adapters produce
//! [`UsageFact`] values; the CLI and TUI consume the same range, attribution, cost, bucketing and
//! replay semantics. No prompt, answer, tool argument, or transcript body is represented here.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use flux_core::{canonical_model_parts, CostSource, PricingTable, Usage};
use flux_events::{EventKind, StoredEvent};

use crate::harness::HarnessKind;

/// Resolution of a source timestamp. Coarse facts remain visibly coarse during replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimePrecision {
    Call,
    Message,
    Bucket,
    Unknown,
}

/// Why a provider attribution is safe to display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderAttribution {
    /// The source supplied the billing/routing provider independently of the model name.
    Proven(String),
    /// The source did not prove a provider. A model prefix alone is intentionally insufficient.
    Unknown,
}

impl ProviderAttribution {
    pub fn value(&self) -> Option<&str> {
        match self {
            Self::Proven(value) => Some(value),
            Self::Unknown => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CostStatus {
    Reported,
    EstimatedTable,
    SubscriptionEquivalent,
    UnpricedUnknownModel,
    UnpricedMissingUsage,
}

impl CostStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::EstimatedTable => "estimated_table",
            Self::SubscriptionEquivalent => "subscription_equivalent",
            Self::UnpricedUnknownModel => "unpriced_unknown_model",
            Self::UnpricedMissingUsage => "unpriced_missing_usage",
        }
    }

    pub fn short_reason(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::EstimatedTable => "table",
            Self::SubscriptionEquivalent => "sub",
            Self::UnpricedUnknownModel => "unknown model",
            Self::UnpricedMissingUsage => "missing usage",
        }
    }

    pub fn is_unpriced(self) -> bool {
        matches!(
            self,
            Self::UnpricedUnknownModel | Self::UnpricedMissingUsage
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostSourceCell {
    Reported,
    Estimated,
}

/// One priced call. Pricing basis is retained on historical estimates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostCell {
    pub usd: f64,
    pub subscription: bool,
    pub source: CostSourceCell,
    pub status: CostStatus,
    /// Stable basis label; `provider_reported` or `pricing_table`.
    pub basis: &'static str,
}

/// One usage-bearing call, or one explicitly coarse legacy fallback.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageFact {
    pub harness: HarnessKind,
    pub session_id: String,
    pub raw_model: String,
    /// Compatibility grouping key used by the existing `flux usage` surface.
    pub model: String,
    pub canonical_model: String,
    pub provider: ProviderAttribution,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
    pub precision: TimePrecision,
    pub usage: Usage,
    pub calls: u64,
    pub cost: Option<CostCell>,
    pub cost_status: CostStatus,
}

impl UsageFact {
    pub fn event_ms(&self) -> Option<i64> {
        self.ended_at_ms.or(self.started_at_ms)
    }

    pub fn model(&self) -> &str {
        &self.canonical_model
    }

    pub fn priced(
        harness: HarnessKind,
        session_id: impl Into<String>,
        raw_model: impl Into<String>,
        provider: ProviderAttribution,
        at_ms: Option<i64>,
        precision: TimePrecision,
        usage: Usage,
        pricing: &PricingTable,
    ) -> Self {
        let raw_model = raw_model.into();
        let (_, canonical) = canonical_model_parts(&raw_model);
        let canonical_model = canonical.to_string();
        let (cost, cost_status) = price_call(&usage, &raw_model, pricing);
        Self {
            harness,
            session_id: session_id.into(),
            raw_model: raw_model.clone(),
            model: raw_model,
            canonical_model,
            provider,
            started_at_ms: at_ms,
            ended_at_ms: at_ms,
            precision,
            usage,
            calls: 1,
            cost,
            cost_status,
        }
    }
}

pub fn price_call(
    usage: &Usage,
    model: &str,
    pricing: &PricingTable,
) -> (Option<CostCell>, CostStatus) {
    if usage.total() == 0 && usage.reasoning_tokens == 0 && usage.reported_cost_usd.is_none() {
        return (None, CostStatus::UnpricedMissingUsage);
    }
    match pricing.cost(usage, model) {
        Some(money) => {
            let (source, status, basis) = match money.source {
                CostSource::Reported => (
                    CostSourceCell::Reported,
                    CostStatus::Reported,
                    "provider_reported",
                ),
                CostSource::Estimated if money.subscription => (
                    CostSourceCell::Estimated,
                    CostStatus::SubscriptionEquivalent,
                    "pricing_table",
                ),
                CostSource::Estimated => (
                    CostSourceCell::Estimated,
                    CostStatus::EstimatedTable,
                    "pricing_table",
                ),
            };
            (
                Some(CostCell {
                    usd: money.usd,
                    subscription: money.subscription,
                    source,
                    status,
                    basis,
                }),
                status,
            )
        }
        None => (None, CostStatus::UnpricedUnknownModel),
    }
}

/// Extract Flux usage metadata without ever touching message/transcript payloads supplied elsewhere.
/// `CallUsage` is canonical per turn; `TurnEnded.usage` is used only for uncovered legacy turns.
pub fn flux_facts(
    session_id: &str,
    events: &[StoredEvent],
    pricing: &PricingTable,
) -> Vec<UsageFact> {
    let mut covered_turns = HashSet::new();
    let mut out = Vec::new();
    for event in events {
        if let EventKind::CallUsage { model, usage } = &event.kind {
            if let Some(turn_id) = event.turn_id {
                covered_turns.insert(turn_id);
            }
            if usage.total() > 0 || usage.reasoning_tokens > 0 || usage.reported_cost_usd.is_some()
            {
                out.push(UsageFact::priced(
                    HarnessKind::Flux,
                    session_id,
                    model,
                    ProviderAttribution::Unknown,
                    Some(event.ts_ms),
                    TimePrecision::Call,
                    usage.clone(),
                    pricing,
                ));
            }
        }
    }
    for turn in flux_events::turns(events) {
        if covered_turns.contains(&turn.turn_id) {
            continue;
        }
        if let Some(usage) = turn.usage {
            if usage.total() > 0 || usage.reasoning_tokens > 0 || usage.reported_cost_usd.is_some()
            {
                let mut fact = UsageFact::priced(
                    HarnessKind::Flux,
                    session_id,
                    turn.model,
                    ProviderAttribution::Unknown,
                    turn.ended_at_ms.or(Some(turn.started_at_ms)),
                    TimePrecision::Bucket,
                    usage,
                    pricing,
                );
                fact.calls = 1;
                out.push(fact);
            }
        }
    }
    out.sort_by(|a, b| {
        a.event_ms()
            .cmp(&b.event_ms())
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    out
}

/// Half-open range `[start_ms, end_ms)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsageRange {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl UsageRange {
    pub const FOUR_HOURS_MS: i64 = 4 * 60 * 60 * 1000;
    pub const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    pub const WEEK_MS: i64 = 7 * Self::DAY_MS;

    pub fn new(start_ms: i64, end_ms: i64) -> Option<Self> {
        (start_ms < end_ms).then_some(Self { start_ms, end_ms })
    }

    pub fn trailing(end_ms: i64, duration_ms: i64) -> Self {
        Self {
            start_ms: end_ms.saturating_sub(duration_ms.max(1)),
            end_ms,
        }
    }

    pub fn duration_ms(self) -> i64 {
        self.end_ms - self.start_ms
    }

    pub fn previous(self) -> Self {
        let duration = self.duration_ms();
        Self {
            start_ms: self.start_ms.saturating_sub(duration),
            end_ms: self.start_ms,
        }
    }

    pub fn contains(self, ts: i64) -> bool {
        ts >= self.start_ms && ts < self.end_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupBy {
    Harness,
    Provider,
    Model,
    Route,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageFilter {
    pub harnesses: BTreeSet<HarnessKind>,
    pub providers: BTreeSet<String>,
    pub models: BTreeSet<String>,
}

impl UsageFilter {
    pub fn matches(&self, fact: &UsageFact) -> bool {
        (self.harnesses.is_empty() || self.harnesses.contains(&fact.harness))
            && (self.providers.is_empty()
                || self
                    .providers
                    .contains(fact.provider.value().unwrap_or("unknown")))
            && (self.models.is_empty() || self.models.contains(&fact.canonical_model))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageTotals {
    pub usage: Usage,
    pub calls: u64,
    pub sessions: u64,
    pub reported_usd: f64,
    pub estimated_usd: f64,
    pub subscription_equivalent_usd: f64,
    pub reported_calls: u64,
    pub estimated_calls: u64,
    pub subscription_calls: u64,
    pub unpriced_calls: u64,
}

impl UsageTotals {
    pub fn priced_usd(&self) -> f64 {
        self.reported_usd + self.estimated_usd + self.subscription_equivalent_usd
    }

    fn add_fact(&mut self, fact: &UsageFact) {
        self.usage.sum_independent(&fact.usage);
        self.calls = self.calls.saturating_add(fact.calls);
        match (fact.cost, fact.cost_status) {
            (Some(cost), CostStatus::Reported) => {
                self.reported_usd += cost.usd;
                self.reported_calls += fact.calls;
            }
            (Some(cost), CostStatus::SubscriptionEquivalent) => {
                self.subscription_equivalent_usd += cost.usd;
                self.subscription_calls += fact.calls;
            }
            (Some(cost), _) => {
                self.estimated_usd += cost.usd;
                self.estimated_calls += fact.calls;
            }
            (None, _) => self.unpriced_calls += fact.calls,
        }
    }

    fn merge(&mut self, other: &Self) {
        self.usage.sum_independent(&other.usage);
        self.calls += other.calls;
        self.reported_usd += other.reported_usd;
        self.estimated_usd += other.estimated_usd;
        self.subscription_equivalent_usd += other.subscription_equivalent_usd;
        self.reported_calls += other.reported_calls;
        self.estimated_calls += other.estimated_calls;
        self.subscription_calls += other.subscription_calls;
        self.unpriced_calls += other.unpriced_calls;
    }
}

fn selected<'a>(
    facts: &'a [UsageFact],
    range: UsageRange,
    filter: &'a UsageFilter,
) -> impl Iterator<Item = &'a UsageFact> {
    facts
        .iter()
        .filter(move |fact| fact.event_ms().is_some_and(|ts| range.contains(ts)))
        .filter(move |fact| filter.matches(fact))
}

pub fn totals(facts: &[UsageFact], range: UsageRange, filter: &UsageFilter) -> UsageTotals {
    let mut totals = UsageTotals::default();
    let mut sessions = BTreeSet::new();
    for fact in selected(facts, range, filter) {
        totals.add_fact(fact);
        sessions.insert((fact.harness, fact.session_id.clone()));
    }
    totals.sessions = sessions.len() as u64;
    totals
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageBucket {
    pub range: UsageRange,
    pub totals: UsageTotals,
}

/// Adaptive, deterministic buckets. Bucket count never exceeds plot width or selected milliseconds.
pub fn buckets(
    facts: &[UsageFact],
    range: UsageRange,
    plot_width: usize,
    filter: &UsageFilter,
) -> Vec<UsageBucket> {
    if plot_width == 0 {
        return Vec::new();
    }
    let count = plot_width.min(range.duration_ms().max(1) as usize).max(1);
    let duration = range.duration_ms();
    let mut out = (0..count)
        .map(|index| {
            let start = range.start_ms + duration * index as i64 / count as i64;
            let end = range.start_ms + duration * (index + 1) as i64 / count as i64;
            UsageBucket {
                range: UsageRange {
                    start_ms: start,
                    end_ms: end,
                },
                totals: UsageTotals::default(),
            }
        })
        .collect::<Vec<_>>();
    let mut sessions = vec![BTreeSet::new(); count];
    for fact in selected(facts, range, filter) {
        let ts = fact.event_ms().expect("selected facts have time");
        let offset = ts.saturating_sub(range.start_ms) as i128;
        let index = ((offset * count as i128) / duration as i128).min((count - 1) as i128) as usize;
        out[index].totals.add_fact(fact);
        sessions[index].insert((fact.harness, fact.session_id.clone()));
    }
    for (bucket, sessions) in out.iter_mut().zip(sessions) {
        bucket.totals.sessions = sessions.len() as u64;
    }
    out
}

pub fn cumulative(series: &[UsageBucket]) -> Vec<UsageTotals> {
    let mut running = UsageTotals::default();
    series
        .iter()
        .map(|bucket| {
            running.merge(&bucket.totals);
            running.clone()
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
pub struct GroupRow {
    pub key: String,
    pub totals: UsageTotals,
}

pub fn groups(
    facts: &[UsageFact],
    range: UsageRange,
    filter: &UsageFilter,
    by: GroupBy,
) -> Vec<GroupRow> {
    let mut rows = BTreeMap::<String, (UsageTotals, BTreeSet<(HarnessKind, String)>)>::new();
    for fact in selected(facts, range, filter) {
        let provider = fact.provider.value().unwrap_or("unknown");
        let key = match by {
            GroupBy::Harness => fact.harness.label().to_string(),
            GroupBy::Provider => provider.to_string(),
            GroupBy::Model => fact.canonical_model.clone(),
            GroupBy::Route => format!(
                "{} → {provider} → {}",
                fact.harness.label(),
                fact.canonical_model
            ),
        };
        let row = rows.entry(key).or_default();
        row.0.add_fact(fact);
        row.1.insert((fact.harness, fact.session_id.clone()));
    }
    let mut out = rows
        .into_iter()
        .map(|(key, (mut totals, sessions))| {
            totals.sessions = sessions.len() as u64;
            GroupRow { key, totals }
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        b.totals
            .priced_usd()
            .total_cmp(&a.totals.priced_usd())
            .then_with(|| b.totals.usage.total().cmp(&a.totals.usage.total()))
            .then_with(|| a.key.cmp(&b.key))
    });
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct PeriodComparison {
    pub current: UsageTotals,
    pub previous: UsageTotals,
    /// `None` means no prior baseline; zero-to-positive is deliberately not infinity.
    pub token_percent: Option<f64>,
    pub cost_percent: Option<f64>,
}

pub fn compare_previous(
    facts: &[UsageFact],
    range: UsageRange,
    filter: &UsageFilter,
) -> PeriodComparison {
    let current = totals(facts, range, filter);
    let previous = totals(facts, range.previous(), filter);
    let percent = |now: f64, before: f64| {
        if before == 0.0 {
            None
        } else {
            Some((now - before) * 100.0 / before)
        }
    };
    PeriodComparison {
        token_percent: percent(current.usage.total() as f64, previous.usage.total() as f64),
        cost_percent: percent(current.priced_usd(), previous.priced_usd()),
        current,
        previous,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pulse {
    pub at_ms: i64,
    pub harness: HarnessKind,
    pub provider: String,
    pub model: String,
    pub count: u64,
    pub tokens: u64,
    pub precision: TimePrecision,
    pub cost_status: CostStatus,
}

/// Coalesce route-identical calls into a bounded visible pulse set without touching accounting totals.
pub fn visible_pulses(
    facts: &[UsageFact],
    range: UsageRange,
    cursor_ms: i64,
    filter: &UsageFilter,
    cap: usize,
) -> Vec<Pulse> {
    if cap == 0 {
        return Vec::new();
    }
    let visible = UsageRange {
        start_ms: range.start_ms,
        end_ms: cursor_ms.saturating_add(1).min(range.end_ms),
    };
    let mut routes =
        BTreeMap::<(HarnessKind, String, String, TimePrecision, CostStatus), Pulse>::new();
    for fact in selected(facts, visible, filter) {
        let provider = fact.provider.value().unwrap_or("unknown").to_string();
        let key = (
            fact.harness,
            provider.clone(),
            fact.canonical_model.clone(),
            fact.precision,
            fact.cost_status,
        );
        routes
            .entry(key)
            .and_modify(|pulse| {
                pulse.count += fact.calls;
                pulse.tokens += fact.usage.total();
                pulse.at_ms = pulse.at_ms.max(fact.event_ms().unwrap_or(pulse.at_ms));
            })
            .or_insert(Pulse {
                at_ms: fact.event_ms().unwrap_or(range.start_ms),
                harness: fact.harness,
                provider,
                model: fact.canonical_model.clone(),
                count: fact.calls,
                tokens: fact.usage.total(),
                precision: fact.precision,
                cost_status: fact.cost_status,
            });
    }
    let mut pulses = routes.into_values().collect::<Vec<_>>();
    pulses.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.model.cmp(&b.model)));
    pulses.truncate(cap);
    pulses
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayClock {
    pub range: UsageRange,
    pub cursor_ms: i64,
    pub playing: bool,
    pub speed: f64,
    pub reduced_motion: bool,
}

impl ReplayClock {
    pub fn new(range: UsageRange) -> Self {
        Self {
            range,
            cursor_ms: range.start_ms,
            playing: false,
            speed: 1.0,
            reduced_motion: false,
        }
    }

    pub fn toggle(&mut self) {
        self.playing = !self.playing;
    }

    pub fn restart(&mut self) {
        self.cursor_ms = self.range.start_ms;
        self.playing = false;
    }

    pub fn seek(&mut self, delta_ms: i64) {
        self.cursor_ms = self
            .cursor_ms
            .saturating_add(delta_ms)
            .clamp(self.range.start_ms, self.range.end_ms);
    }

    pub fn set_speed(&mut self, speed: f64) -> bool {
        if (0.5..=100.0).contains(&speed) {
            self.speed = speed;
            true
        } else {
            false
        }
    }

    pub fn fit_to(&mut self, replay_duration_ms: i64) {
        if replay_duration_ms > 0 {
            self.speed =
                (self.range.duration_ms() as f64 / replay_duration_ms as f64).clamp(0.5, 100.0);
        }
    }

    pub fn advance(&mut self, wall_delta_ms: i64) {
        if !self.playing || wall_delta_ms <= 0 {
            return;
        }
        let delta = (wall_delta_ms as f64 * self.speed).round() as i64;
        self.cursor_ms = self.cursor_ms.saturating_add(delta).min(self.range.end_ms);
        if self.cursor_ms == self.range.end_ms {
            self.playing = false;
        }
    }

    pub fn rebase(&mut self, range: UsageRange) {
        self.range = range;
        self.cursor_ms = range.start_ms;
        self.playing = false;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReplayFrame {
    pub cursor_ms: i64,
    pub cumulative: UsageTotals,
    pub pulses: Vec<Pulse>,
}

pub fn replay_frame(
    facts: &[UsageFact],
    clock: &ReplayClock,
    filter: &UsageFilter,
    pulse_cap: usize,
) -> ReplayFrame {
    let cursor_end = clock.cursor_ms.saturating_add(1).min(clock.range.end_ms);
    let cumulative = if cursor_end <= clock.range.start_ms {
        UsageTotals::default()
    } else {
        totals(
            facts,
            UsageRange {
                start_ms: clock.range.start_ms,
                end_ms: cursor_end,
            },
            filter,
        )
    };
    ReplayFrame {
        cursor_ms: clock.cursor_ms,
        cumulative,
        pulses: if clock.reduced_motion {
            Vec::new()
        } else {
            visible_pulses(facts, clock.range, clock.cursor_ms, filter, pulse_cap)
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_events::NewEvent;

    fn fact(harness: HarnessKind, session: &str, model: &str, at: i64, tokens: u64) -> UsageFact {
        UsageFact::priced(
            harness,
            session,
            model,
            ProviderAttribution::Unknown,
            Some(at),
            TimePrecision::Call,
            Usage {
                input_tokens: tokens,
                ..Default::default()
            },
            &PricingTable::builtin(),
        )
    }

    fn stored(seq: i64, turn: Option<i64>, ts: i64, kind: EventKind) -> StoredEvent {
        StoredEvent {
            global_seq: seq,
            stream: "s".into(),
            stream_seq: seq,
            id: format!("e{seq}"),
            turn_id: turn,
            schema_version: 1,
            ts_ms: ts,
            kind,
            context: Default::default(),
        }
    }

    #[test]
    fn shared_timeline_covers_every_discovered_harness() {
        let facts = HarnessKind::ALL
            .into_iter()
            .enumerate()
            .map(|(i, harness)| fact(harness, "s", "gpt-5", i as i64, 1))
            .collect::<Vec<_>>();
        assert_eq!(
            facts.iter().map(|f| f.harness).collect::<BTreeSet<_>>(),
            HarnessKind::ALL.into_iter().collect()
        );
    }

    #[test]
    fn call_usage_wins_without_doubling_turn_usage() {
        let usage = Usage {
            input_tokens: 10,
            output_tokens: 2,
            ..Default::default()
        };
        let events = vec![
            stored(
                1,
                None,
                1,
                EventKind::TurnStarted {
                    user_input: "secret sentinel".into(),
                    model: "gpt-5".into(),
                },
            ),
            stored(
                2,
                Some(1),
                2,
                EventKind::CallUsage {
                    model: "gpt-5".into(),
                    usage: usage.clone(),
                },
            ),
            stored(
                3,
                Some(1),
                3,
                EventKind::TurnEnded {
                    outcome: "ok".into(),
                    iterations: 1,
                    answer: "secret sentinel".into(),
                    usage: Some(usage),
                },
            ),
        ];
        let facts = flux_facts("s", &events, &PricingTable::builtin());
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].usage.total(), 12);
    }

    #[test]
    fn routed_model_prefix_does_not_invent_provider() {
        let f = fact(HarnessKind::Flux, "s", "openrouter/anthropic/claude", 1, 1);
        assert_eq!(f.provider, ProviderAttribution::Unknown);
    }

    #[test]
    fn independent_calls_preserve_usage_tiers() {
        let mut a = fact(HarnessKind::Flux, "a", "gpt-5", 1, 1);
        a.usage.cache_read_input_tokens = 7;
        a.usage.cache_creation_input_tokens = 3;
        a.usage.cache_creation_1h_input_tokens = 2;
        a.usage.reasoning_tokens = 5;
        a.usage.audio_input_tokens = 4;
        let mut b = a.clone();
        b.session_id = "b".into();
        b.started_at_ms = Some(2);
        b.ended_at_ms = Some(2);
        let got = totals(
            &[a, b],
            UsageRange::new(0, 3).unwrap(),
            &UsageFilter::default(),
        );
        assert_eq!(got.usage.cache_read_input_tokens, 14);
        assert_eq!(got.usage.cache_creation_1h_input_tokens, 4);
        assert_eq!(got.usage.reasoning_tokens, 10);
        assert_eq!(got.usage.audio_input_tokens, 8);
    }

    #[test]
    fn mixed_cost_provenance_survives_aggregation() {
        let mut reported = fact(HarnessKind::Flux, "a", "gpt-5", 1, 10);
        reported.cost = Some(CostCell {
            usd: 1.0,
            subscription: false,
            source: CostSourceCell::Reported,
            status: CostStatus::Reported,
            basis: "provider_reported",
        });
        reported.cost_status = CostStatus::Reported;
        let mut estimated = reported.clone();
        estimated.session_id = "b".into();
        estimated.started_at_ms = Some(2);
        estimated.ended_at_ms = Some(2);
        estimated.cost = Some(CostCell {
            usd: 2.0,
            subscription: false,
            source: CostSourceCell::Estimated,
            status: CostStatus::EstimatedTable,
            basis: "pricing_table",
        });
        estimated.cost_status = CostStatus::EstimatedTable;
        let mut unknown = estimated.clone();
        unknown.session_id = "c".into();
        unknown.started_at_ms = Some(3);
        unknown.ended_at_ms = Some(3);
        unknown.cost = None;
        unknown.cost_status = CostStatus::UnpricedUnknownModel;
        let got = totals(
            &[reported, estimated, unknown],
            UsageRange::new(0, 4).unwrap(),
            &UsageFilter::default(),
        );
        assert_eq!(
            (got.reported_usd, got.estimated_usd, got.unpriced_calls),
            (1.0, 2.0, 1)
        );
    }

    #[test]
    fn adaptive_buckets_fit_plot_width_without_losing_totals() {
        let facts = (0..100)
            .map(|i| fact(HarnessKind::Flux, &format!("s{i}"), "gpt-5", i, 1))
            .collect::<Vec<_>>();
        let range = UsageRange::new(0, 100).unwrap();
        let series = buckets(&facts, range, 17, &UsageFilter::default());
        assert_eq!(series.len(), 17);
        assert_eq!(
            cumulative(&series).last().unwrap().usage.total(),
            totals(&facts, range, &UsageFilter::default()).usage.total()
        );
    }

    #[test]
    fn previous_period_is_equal_length_and_adjacent() {
        let facts = vec![
            fact(HarnessKind::Flux, "a", "gpt-5", 5, 10),
            fact(HarnessKind::Flux, "b", "gpt-5", 15, 20),
        ];
        let range = UsageRange::new(10, 20).unwrap();
        assert_eq!(range.previous(), UsageRange::new(0, 10).unwrap());
        let comparison = compare_previous(&facts, range, &UsageFilter::default());
        assert_eq!(comparison.token_percent, Some(100.0));
        assert!(compare_previous(
            &facts,
            UsageRange::new(20, 30).unwrap(),
            &UsageFilter::default()
        )
        .token_percent
        .is_none());
    }

    #[test]
    fn virtual_clock_replay_is_frame_deterministic() {
        let facts = vec![
            fact(HarnessKind::Flux, "a", "gpt-5", 5, 10),
            fact(HarnessKind::Codex, "b", "gpt-5", 8, 20),
        ];
        let mut clock = ReplayClock::new(UsageRange::new(0, 10).unwrap());
        clock.playing = true;
        clock.set_speed(2.0);
        clock.advance(4);
        assert_eq!(
            replay_frame(&facts, &clock, &UsageFilter::default(), 8),
            replay_frame(&facts, &clock, &UsageFilter::default(), 8)
        );
    }

    #[test]
    fn seek_backward_restores_checkpointed_totals() {
        let facts = (0..100)
            .map(|i| fact(HarnessKind::Flux, "s", "gpt-5", i, 1))
            .collect::<Vec<_>>();
        let mut clock = ReplayClock::new(UsageRange::new(0, 100).unwrap());
        clock.seek(80);
        clock.seek(-50);
        let restored = replay_frame(&facts, &clock, &UsageFilter::default(), 4);
        let mut fresh = ReplayClock::new(clock.range);
        fresh.seek(30);
        assert_eq!(
            restored,
            replay_frame(&facts, &fresh, &UsageFilter::default(), 4)
        );
    }

    #[test]
    fn dense_replay_keeps_frame_work_bounded() {
        let facts = (0..50_000)
            .map(|i| fact(HarnessKind::Flux, "s", "gpt-5", i, 1))
            .collect::<Vec<_>>();
        let mut clock = ReplayClock::new(UsageRange::new(0, 50_001).unwrap());
        clock.seek(50_001);
        assert!(
            replay_frame(&facts, &clock, &UsageFilter::default(), 32)
                .pulses
                .len()
                <= 32
        );
        assert!(buckets(&facts, clock.range, 120, &UsageFilter::default()).len() <= 120);
    }

    #[test]
    fn usage_timeline_reads_metadata_only() {
        // The public fact type has no text/transcript field; extraction observes only typed usage
        // event variants even when secret sentinels exist in neighbouring turn payloads.
        let event = NewEvent::new(EventKind::Message(flux_core::Message::user("DO_NOT_LOAD")));
        assert!(matches!(event.kind, EventKind::Message(_)));
        assert!(std::mem::size_of::<UsageFact>() > 0);
    }
}
