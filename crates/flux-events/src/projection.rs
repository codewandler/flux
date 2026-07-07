//! Read models derived by folding the event log.
//!
//! These are pure functions over `&[StoredEvent]` (so they're trivially testable). The
//! [`EventStore`](crate::EventStore) wraps each one with a load step for ergonomic call
//! sites. The conversation projection is the headline: the "conversations view" we mainly
//! used the session store for is now *derived* from the log rather than stored directly.

use std::collections::BTreeMap;

use flux_core::{is_subscription, CostSource, Message, Money, PricingTable, Usage};
use flux_lang::ast::RunEvent;

use crate::kind::{EventKind, StoredEvent};

/// Rebuild the conversation by replaying message-kind events in stream order. A
/// [`EventKind::Compacted`] snapshot resets the fold (the superseded messages stay on
/// disk but no longer surface) — this is the append-only equivalent of the old
/// destructive `rewrite_messages`. Reproduces `SessionStore::load_messages`.
pub fn conversation(events: &[StoredEvent]) -> Vec<Message> {
    let mut out = Vec::new();
    for e in events {
        match &e.kind {
            EventKind::Message(m) => out.push(m.clone()),
            EventKind::Compacted { messages } => {
                out.clear();
                out.extend(messages.iter().cloned());
            }
            // lifecycle / run / turn events don't touch the conversation
            _ => {}
        }
    }
    out
}

/// The flow run-trace for a stream, in order. Reproduces `FlowStore::events`.
pub fn run_trace(events: &[StoredEvent]) -> Vec<RunEvent> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Run(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

/// One planning attempt within a turn (the old `plan_attempts` row). Also the WRITE shape
/// [`EventStore::record_plan_attempt`](crate::EventStore::record_plan_attempt) takes — one struct,
/// no field drift between what is recorded and what the fold reads back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlanAttempt {
    pub step: u32,
    pub outcome: String,
    pub error: Option<String>,
    /// SHA-256 of the accepted plan AST's canonical JSON (the loop guard's identity) — `None` for
    /// non-plan outcomes and pre-C-14 logs.
    pub fingerprint: Option<String>,
    /// The human-auditable rendered plan graph (`render_pretty`, capped) — the durable "a turn is a
    /// readable graph" record. `None` for non-plan outcomes and pre-C-14 logs.
    pub plan_text: Option<String>,
    /// The multi-pass loop phase (A-14) that produced this attempt — `"orient"`, `"gather"`, or
    /// `"execute"`. `None` for pre-A-14 logs and attempts recorded outside a phase-aware call.
    pub phase: Option<String>,
    /// The accepted plan's canonical parseable Flux-Lang text (`flux_lang::format::format`, L-38)
    /// — round-trips via `parse`. `None` for non-accepted outcomes, pre-L-38 logs, and oversized
    /// plans (never truncated: a present `plan_source` always parses).
    pub plan_source: Option<String>,
}

/// A turn's telemetry, folded from its `TurnStarted` / `PlanAttempted` / `TurnEnded`
/// events (the old `turn_log` row plus its `plan_attempts`). `ended_at_ms` is `None` and
/// `outcome` stays `"pending"` for a turn that never closed.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    pub turn_id: i64,
    pub user_input: String,
    pub model: String,
    pub outcome: String,
    pub iterations: u32,
    pub answer: Option<String>,
    pub plan_attempts: Vec<PlanAttempt>,
    pub started_at_ms: i64,
    pub ended_at_ms: Option<i64>,
    /// The turn's accumulated token usage, when recorded (`None` for older logs / no provider usage).
    pub usage: Option<Usage>,
    /// How many provider calls the turn made (its `CallUsage` events) — 0 for pre-C-06 logs (C-15).
    pub calls: u64,
    /// Field-wise sum of this turn's per-call usage records, all tiers — the attribution-grade
    /// per-turn total (`usage` above stays the turn's replace-style back-compat total) (C-15).
    pub call_usage: Usage,
}

/// Fold turn telemetry, keyed (and ordered) by `turn_id` = the `TurnStarted`'s `global_seq`.
/// Reproduces what the `turn_log` + `plan_attempts` tables were for.
pub fn turns(events: &[StoredEvent]) -> Vec<TurnSummary> {
    let mut by_turn: BTreeMap<i64, TurnSummary> = BTreeMap::new();
    for e in events {
        match &e.kind {
            EventKind::TurnStarted { user_input, model } => {
                by_turn.insert(
                    e.global_seq,
                    TurnSummary {
                        turn_id: e.global_seq,
                        user_input: user_input.clone(),
                        model: model.clone(),
                        outcome: "pending".to_string(),
                        iterations: 0,
                        answer: None,
                        plan_attempts: Vec::new(),
                        started_at_ms: e.ts_ms,
                        ended_at_ms: None,
                        usage: None,
                        calls: 0,
                        call_usage: Usage::default(),
                    },
                );
            }
            EventKind::PlanAttempted {
                step,
                outcome,
                error,
                fingerprint,
                plan_text,
                phase,
                plan_source,
            } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.plan_attempts.push(PlanAttempt {
                        step: *step,
                        outcome: outcome.clone(),
                        error: error.clone(),
                        fingerprint: fingerprint.clone(),
                        plan_text: plan_text.clone(),
                        phase: phase.clone(),
                        plan_source: plan_source.clone(),
                    });
                }
            }
            EventKind::TurnEnded {
                outcome,
                iterations,
                answer,
                usage,
            } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.outcome = outcome.clone();
                    t.iterations = *iterations;
                    t.answer = Some(answer.clone());
                    t.ended_at_ms = Some(e.ts_ms);
                    t.usage = usage.clone();
                }
            }
            EventKind::CallUsage { usage, .. } => {
                if let Some(t) = e.turn_id.and_then(|tid| by_turn.get_mut(&tid)) {
                    t.calls += 1;
                    sum_usage(&mut t.call_usage, usage);
                }
            }
            _ => {}
        }
    }
    by_turn.into_values().collect()
}

/// The durable evidence trail: every persisted observation, in stream order (C-14). This is the
/// offline/programmatic read of what `/evidence` shows live from the in-memory log — plan the
/// `tool_call` markers, `turn.iteration` rounds, `groups.active` (+ signals), skill activations,
/// and flow-emitted `observe(…)` records land here via the engine's per-turn watermark flush.
pub fn observations(events: &[StoredEvent]) -> Vec<flux_evidence::Observation> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::Observation(o) => Some(o.clone()),
            _ => None,
        })
        .collect()
}

/// One paired (user instruction, accepted plan) row — the D-53 corpus-mining projection over the
/// L-38 `plan_source` field: flux-native, zero-LLM-cost NL→Flux-Lang training data mined straight
/// from real usage. `id` is the owning `PlanAttempted` event's stable id (a ULID unless the writer
/// supplied one) — stable, unique, and traceable back to the exact log row.
#[derive(Debug, Clone, PartialEq)]
pub struct CorpusRow {
    /// The `PlanAttempted` event's stable id — this row's identity.
    pub id: String,
    /// The natural-language instruction that produced `source` — the enclosing turn's
    /// `TurnStarted.user_input`. A `PlanAttempted` is always recorded scoped to the turn it was
    /// attempted within (C-14's `turn_id`), so that turn's `user_input` *is* "the most recent user
    /// turn before it in the same session" — no separate conversation walk is needed.
    pub nl_goal: String,
    /// The accepted plan's canonical Flux-Lang text (`PlanAttempted.plan_source`, L-38).
    pub source: String,
    /// The owning session/stream id (e.g. `"s_42"`).
    pub session: String,
    /// The owning turn id (`TurnStarted`'s `global_seq`).
    pub turn: i64,
}

/// Why a candidate accepted-plan row was skipped rather than exported (D-53) — "precision over
/// recall": every skip is counted so an operator sees how much of the log didn't qualify, instead
/// of a silent undercount.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CorpusSkipCounts {
    /// `PlanAttempted.plan_source` was `None` — a pre-L-38 log, or the plan was too large to
    /// record (per [`EventKind::PlanAttempted`]'s doc comment, `plan_source` is never truncated:
    /// present means parseable, so `None` always means "not recorded", not "recorded empty").
    pub no_plan_source: u64,
    /// The plan's `turn_id` was absent, or didn't resolve to a `TurnStarted` event in this stream
    /// — pairing would be a guess, so the row is dropped rather than paired ambiguously.
    pub ambiguous_pairing: u64,
}

impl CorpusSkipCounts {
    /// Fold another stream's skip counts into this one (raw sums — used to aggregate across
    /// streams, mirroring [`EfficiencySummary::merge`]).
    pub fn merge(&mut self, other: &Self) {
        self.no_plan_source += other.no_plan_source;
        self.ambiguous_pairing += other.ambiguous_pairing;
    }

    /// Total rows skipped, across every reason.
    pub fn total(&self) -> u64 {
        self.no_plan_source + self.ambiguous_pairing
    }
}

/// Pair every accepted plan's canonical `plan_source` (L-38) with the `user_input` of the turn it
/// was attempted within, for one stream (D-53). `stream` stamps the row's `session` field (the fold
/// itself is over `events`, which carry no stream id of their own outside [`StoredEvent::stream`]).
///
/// Skips (counted, never guessed at): a `PlanAttempted` with `plan_source: None` (pre-L-38 log, or
/// an oversized plan); a `PlanAttempted` with no `turn_id`, or one whose `turn_id` never matches a
/// `TurnStarted` in this stream (both would require guessing the originating instruction).
pub fn corpus_rows(stream: &str, events: &[StoredEvent]) -> (Vec<CorpusRow>, CorpusSkipCounts) {
    let mut user_input_by_turn: std::collections::HashMap<i64, String> =
        std::collections::HashMap::new();
    let mut rows = Vec::new();
    let mut skips = CorpusSkipCounts::default();
    for e in events {
        match &e.kind {
            EventKind::TurnStarted { user_input, .. } => {
                user_input_by_turn.insert(e.global_seq, user_input.clone());
            }
            EventKind::PlanAttempted { plan_source, .. } => {
                let Some(source) = plan_source.clone() else {
                    skips.no_plan_source += 1;
                    continue;
                };
                let nl_goal = e
                    .turn_id
                    .and_then(|tid| user_input_by_turn.get(&tid).cloned());
                let Some(nl_goal) = nl_goal else {
                    skips.ambiguous_pairing += 1;
                    continue;
                };
                rows.push(CorpusRow {
                    id: e.id.clone(),
                    nl_goal,
                    source,
                    session: stream.to_string(),
                    turn: e
                        .turn_id
                        .expect("nl_goal resolved only when turn_id is Some"),
                });
            }
            _ => {}
        }
    }
    (rows, skips)
}

/// Turn-efficiency counters folded from a stream's turn telemetry (C-15) — the Improve pillar's
/// measurability rollup: how many model calls and loop iterations a turn takes, and how much of
/// the prompt side is served from cache. Raw sums, so summaries from many streams merge by
/// addition; the `avg_*`/share accessors derive the report figures.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EfficiencySummary {
    /// Completed turns folded in (turns that never ended are skipped — their counters would skew).
    pub turns: u64,
    /// Provider calls across those turns (`CallUsage` events).
    pub calls: u64,
    /// Loop iterations across those turns (`TurnEnded.iterations`).
    pub iterations: u64,
    /// Prompt tokens served from cache across those turns.
    pub cache_read_tokens: u64,
    /// Prompt tokens NOT served from cache (fresh input + cache writes).
    pub uncached_input_tokens: u64,
    /// Generated tokens across those turns.
    pub output_tokens: u64,
    /// Orient-phase planner rounds (`PlanAttempt.phase == "orient"`) — every phased turn (A-14)
    /// opens with exactly one, so this doubles as the "does this log carry phase data at all"
    /// signal ([`has_phase_rounds`](Self::has_phase_rounds)). 0 for pre-A-14 logs.
    pub orient_rounds: u64,
    /// Gather-phase planner rounds (`phase == "gather"`, every outcome) — the provider rounds the
    /// phased loop spent collecting context, the I-03 gather-over-use watch. Counts attempts, not
    /// accepted plans: a compile-repair round in the gather pass is still a paid round.
    pub gather_rounds: u64,
    /// Execute-phase attempts that accepted a FURTHER plan (`phase == "execute"` +
    /// `outcome == "accepted"`) — the loop revised and ran again after execution, the I-03
    /// revision-churn watch. Deliberately narrower than `gather_rounds`: the execute pass's
    /// terminal chat/complete round is not a revision, and repair rounds already show in
    /// calls/turn. Phase-less accepted plans (pre-A-14 / ejected loops) are not counted.
    pub revise_rounds: u64,
    /// Accepted plans across those turns, all phases (`outcome == "accepted"`) — the I-03
    /// plans-per-turn tiny-plan-dribble watch. Works on pre-A-14 logs too (no phase needed).
    pub accepted_plans: u64,
}

impl EfficiencySummary {
    /// Fold another summary in (raw sums — used to aggregate across streams).
    pub fn merge(&mut self, other: &Self) {
        self.turns += other.turns;
        self.calls += other.calls;
        self.iterations += other.iterations;
        self.cache_read_tokens += other.cache_read_tokens;
        self.uncached_input_tokens += other.uncached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.orient_rounds += other.orient_rounds;
        self.gather_rounds += other.gather_rounds;
        self.revise_rounds += other.revise_rounds;
        self.accepted_plans += other.accepted_plans;
    }

    pub fn avg_calls_per_turn(&self) -> f64 {
        ratio(self.calls, self.turns)
    }

    pub fn avg_iterations_per_turn(&self) -> f64 {
        ratio(self.iterations, self.turns)
    }

    /// Share of the prompt side served from cache: `cache_read / (cache_read + uncached_input)`.
    pub fn cache_read_share(&self) -> f64 {
        let prompt = self.cache_read_tokens + self.uncached_input_tokens;
        ratio(self.cache_read_tokens, prompt)
    }

    pub fn uncached_input_per_turn(&self) -> f64 {
        ratio(self.uncached_input_tokens, self.turns)
    }

    pub fn output_per_turn(&self) -> f64 {
        ratio(self.output_tokens, self.turns)
    }

    /// Whether any folded turn recorded phase-stamped plan attempts (A-14). `false` means the log
    /// predates the phased loop — gather/revise figures are then *unrecorded*, not zero, and a
    /// report should omit them rather than print a misleading `0.0`.
    pub fn has_phase_rounds(&self) -> bool {
        self.orient_rounds + self.gather_rounds + self.revise_rounds > 0
    }

    pub fn avg_gather_rounds_per_turn(&self) -> f64 {
        ratio(self.gather_rounds, self.turns)
    }

    pub fn avg_revise_rounds_per_turn(&self) -> f64 {
        ratio(self.revise_rounds, self.turns)
    }

    pub fn avg_plans_per_turn(&self) -> f64 {
        ratio(self.accepted_plans, self.turns)
    }
}

fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

/// Fold a stream's completed turns into an [`EfficiencySummary`] (C-15). `None` when the stream
/// has no completed turn — a section with nothing to report renders nothing.
pub fn efficiency_summary(events: &[StoredEvent]) -> Option<EfficiencySummary> {
    let mut s = EfficiencySummary::default();
    for t in turns(events) {
        if t.ended_at_ms.is_none() {
            continue;
        }
        s.turns += 1;
        s.calls += t.calls;
        s.iterations += u64::from(t.iterations);
        s.cache_read_tokens += t.call_usage.cache_read_input_tokens;
        s.uncached_input_tokens +=
            t.call_usage.input_tokens + t.call_usage.cache_creation_input_tokens;
        s.output_tokens += t.call_usage.output_tokens;
        for a in &t.plan_attempts {
            if a.outcome == "accepted" {
                s.accepted_plans += 1;
            }
            match a.phase.as_deref() {
                Some("orient") => s.orient_rounds += 1,
                Some("gather") => s.gather_rounds += 1,
                Some("execute") if a.outcome == "accepted" => s.revise_rounds += 1,
                _ => {}
            }
        }
    }
    (s.turns > 0).then_some(s)
}

/// One model's rolled-up token spend + cost — a row of [`cost_summary`].
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCost {
    /// The model id as it was recorded on the event (bare id or `provider/model`, whichever the
    /// caller stamped — see [`EventKind::CallUsage`]).
    pub model: String,
    /// Field-wise sum across every call attributed to this model, ALL tiers included —
    /// `reasoning_tokens` too, unlike [`Usage::total`]/[`Usage::accumulate`], since [`cost_summary`]
    /// exists to price spend and [`flux_core::pricing::cost`] bills reasoning as its own tier.
    pub usage: Usage,
    /// The number of calls (or turns, for the old-log fallback) folded into `usage`.
    pub calls: u64,
    /// The priced cost of `usage` under `model`, when the model is known to the pricing table.
    /// `None` for a model the table has no rates for (never a panic).
    pub cost: Option<Money>,
}

/// Field-wise-sum one call's usage into a running total. Every tier is summed here — including
/// `reasoning_tokens` and (C-34) `reported_cost_usd` — because this is a cost rollup, not a
/// context-window occupancy figure: each call's reasoning tokens are a real (if usually
/// zero-priced) cost line, and folding them keeps [`ModelCost::usage`] a faithful total across
/// every tier `pricing::cost` can charge for. This is deliberately NOT [`Usage::accumulate`] (which
/// replaces the input/cache side per call — correct for a live turn's context-window occupancy,
/// wrong for summing many independent calls' spend). Note: the summed `reported_cost_usd` on the
/// returned `Usage` is informational only — [`RowAcc::record_call`] prices each call individually
/// and never re-derives a row's [`ModelCost::cost`] from this aggregate (see its doc comment for
/// why that would silently under-report a mixed tabled/reported row).
fn sum_usage(acc: &mut Usage, call: &Usage) {
    acc.input_tokens += call.input_tokens;
    acc.output_tokens += call.output_tokens;
    acc.cache_creation_input_tokens += call.cache_creation_input_tokens;
    acc.cache_read_input_tokens += call.cache_read_input_tokens;
    acc.reasoning_tokens += call.reasoning_tokens;
    if let Some(c) = call.reported_cost_usd {
        *acc.reported_cost_usd.get_or_insert(0.0) += c;
    }
}

/// Per-model running fold for [`cost_summary`]/[`EventStore::cost_summary_all`](crate::EventStore::cost_summary_all):
/// raw usage + call count, plus enough to price the row **honestly** when its calls are a mix of
/// provider-reported and table-estimated cost (C-34). Pricing the row's already-summed [`Usage`] in
/// one shot (checking `reported_cost_usd` once on the aggregate) would let a couple of reported
/// calls short-circuit the table for the rest of a *tabled* model's calls and silently
/// under-report — so each call is priced individually via [`RowAcc::record_call`] and the row's
/// cost is the SUM of those individual prices.
#[derive(Debug, Clone)]
pub(crate) struct RowAcc {
    pub usage: Usage,
    pub calls: u64,
    /// USD summed from calls that priced (reported or table) — `0.0` when none did.
    priced_usd: f64,
    /// How many of `calls` contributed to `priced_usd`. `0` means the whole row is unpriced
    /// ([`ModelCost::cost`] is `None`) — an untabled model with no reported calls, unchanged from
    /// before C-34.
    priced_calls: u64,
    /// `true` iff every call that contributed to `priced_usd` was provider-reported, not
    /// table-estimated — the AND across every folded call/row.
    all_reported: bool,
}

impl Default for RowAcc {
    fn default() -> Self {
        RowAcc {
            usage: Usage::default(),
            calls: 0,
            priced_usd: 0.0,
            priced_calls: 0,
            all_reported: true,
        }
    }
}

impl RowAcc {
    /// Fold one provider call's usage into this row, pricing it individually via `pricing`+`model`
    /// (reported cost short-circuits inside [`PricingTable::cost`] itself — this just reads back
    /// which source won via [`Money::source`]).
    fn record_call(&mut self, usage: &Usage, model: &str, pricing: &PricingTable) {
        self.calls += 1;
        sum_usage(&mut self.usage, usage);
        if let Some(money) = pricing.cost(usage, model) {
            self.priced_usd += money.usd;
            self.priced_calls += 1;
            if money.source != CostSource::Reported {
                self.all_reported = false;
            }
        }
    }

    /// Fold an already-priced [`ModelCost`] row (e.g. one stream's [`cost_summary`] output) into
    /// this one — used by the cross-stream re-merge in `EventStore::cost_summary_all`, which only
    /// has each stream's already-priced total, not its individual calls.
    pub(crate) fn record_priced_row(&mut self, row: &ModelCost) {
        sum_usage(&mut self.usage, &row.usage);
        self.calls += row.calls;
        if let Some(money) = &row.cost {
            self.priced_usd += money.usd;
            // The row itself doesn't say how many of its calls were priced — only whether the row
            // AS A WHOLE priced. `.max(1)` marks the row priced without needing that granularity;
            // it never reaches 0 when `row.cost` is `Some`, which is all `to_cost` checks.
            self.priced_calls += row.calls.max(1);
            if money.source != CostSource::Reported {
                self.all_reported = false;
            }
        }
    }

    /// Merge another row's totals into this one (same-key folds across legacy key variants).
    fn merge(&mut self, other: &RowAcc) {
        sum_usage(&mut self.usage, &other.usage);
        self.calls += other.calls;
        self.priced_usd += other.priced_usd;
        self.priced_calls += other.priced_calls;
        self.all_reported = self.all_reported && other.all_reported;
    }

    /// The row's final priced cost — `None` when nothing in it could be priced (an untabled model
    /// with no reported calls, exactly today's `$?` case).
    fn to_cost(&self, model: &str) -> Option<Money> {
        if self.priced_calls == 0 {
            return None;
        }
        Some(Money {
            usd: self.priced_usd,
            subscription: is_subscription(model),
            source: if self.all_reported {
                CostSource::Reported
            } else {
                CostSource::Estimated
            },
        })
    }
}

/// Build the final [`ModelCost`] row from a folded [`RowAcc`] — shared by [`cost_summary`] and
/// `EventStore::cost_summary_all`'s cross-stream re-merge.
pub(crate) fn finalize_row(model: String, acc: RowAcc) -> ModelCost {
    let cost = acc.to_cost(&model);
    ModelCost {
        model,
        usage: acc.usage,
        calls: acc.calls,
        cost,
    }
}

/// Roll up token spend + cost by model, folding a stream's events. Prefers the per-call
/// [`EventKind::CallUsage`] attribution (C-06); a stream with none recorded (an older log, written
/// before per-call attribution existed) falls back to summing each turn's [`EventKind::TurnEnded`]
/// total, attributed to that turn's [`EventKind::TurnStarted`] model — coarser (turn-level, not
/// call-level), but every old log still rolls up rather than reporting nothing. The two sources are
/// never mixed within one stream: a stream that recorded even one `CallUsage` uses ONLY those (the
/// turn totals they came from would double-count the same spend). Rows are sorted by model id for a
/// stable, diffable report. Each call (or, in the fallback, each turn total) is priced
/// INDIVIDUALLY — see [`RowAcc`] — so a mix of provider-reported and table-estimated calls for the
/// same model sums correctly instead of one source silently eclipsing the other (C-34).
pub fn cost_summary(events: &[StoredEvent], pricing: &PricingTable) -> Vec<ModelCost> {
    let mut per_model: BTreeMap<String, RowAcc> = BTreeMap::new();
    let mut any_call_usage = false;

    for e in events {
        if let EventKind::CallUsage { model, usage } = &e.kind {
            any_call_usage = true;
            per_model
                .entry(model.clone())
                .or_default()
                .record_call(usage, model, pricing);
        }
    }

    if !any_call_usage {
        // Fallback: attribute each turn's total to the model active when that turn STARTED (the
        // `turns()` join point) — the best attribution an old log (no `CallUsage`) can give. Each
        // turn's total is still priced as ONE call (single-total pricing, same as before C-34) —
        // there is no per-call granularity to recover from an old log.
        for t in turns(events) {
            if let Some(usage) = &t.usage {
                per_model
                    .entry(t.model.clone())
                    .or_default()
                    .record_call(usage, &t.model, pricing);
            }
        }
    }

    merge_legacy_keys(per_model)
        .into_iter()
        .map(|(model, acc)| finalize_row(model, acc))
        .collect()
}

/// Fold legacy attribution-key variants into their canonical siblings (C-15). New events are
/// stamped canonically at write time (`canonical_model_spec`); older logs carry variants of the
/// same backend (`gpt-5.5` vs `openai/gpt-5.5`; region-prefixed Bedrock ids). The log is
/// append-only and never rewritten — this read-side merge is the migration:
/// - rows with the SAME provider whose model ids canonicalize identically merge (e.g.
///   `aws/us.anthropic.…` + `aws/eu.anthropic.…` → `aws/anthropic.…`);
/// - a BARE key merges into a provider-prefixed row iff exactly ONE prefixed row shares its
///   canonical model id — with two candidate providers the bare row stays separate (they may
///   bill differently; never guess).
///
/// Passthrough note (C-30): keys stamped `openrouter-anthropic/anthropic/<model>` canonicalize
/// stably to `(openrouter-anthropic, anthropic/<model>)`; rows written before the C-30 attribution
/// fix under the dropped-outer form `anthropic/<model>` carry a DIFFERENT provider and deliberately
/// stay separate rows (never guess across providers).
pub(crate) fn merge_legacy_keys(per_model: BTreeMap<String, RowAcc>) -> BTreeMap<String, RowAcc> {
    use flux_core::canonical_model_parts;
    // Pass 1: canonicalize each key in place (same-provider variants collapse here).
    let mut canon: BTreeMap<(Option<String>, String), RowAcc> = BTreeMap::new();
    for (key, acc) in per_model {
        let (provider, model) = canonical_model_parts(&key);
        canon
            .entry((provider.map(str::to_string), model.to_string()))
            .or_default()
            .merge(&acc);
    }
    // Pass 2: fold each bare row into its sole prefixed sibling, when unambiguous.
    let bare_keys: Vec<String> = canon
        .keys()
        .filter(|(p, _)| p.is_none())
        .map(|(_, m)| m.clone())
        .collect();
    for model in bare_keys {
        let providers: Vec<String> = canon
            .keys()
            .filter(|(p, m)| p.is_some() && *m == model)
            .filter_map(|(p, _)| p.clone())
            .collect();
        if let [sole] = providers.as_slice() {
            let acc = canon.remove(&(None, model.clone())).expect("bare row");
            canon
                .entry((Some(sole.clone()), model))
                .or_default()
                .merge(&acc);
        }
    }
    canon
        .into_iter()
        .map(|((provider, model), v)| {
            let key = match provider {
                Some(p) => format!("{p}/{model}"),
                None => model,
            };
            (key, v)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// C-44: run diff — align two run traces, classify plan vs output divergence
// ---------------------------------------------------------------------------

/// One executed top-level statement of a run, with the cassette cells its dispatches recorded —
/// the C-44 diff unit. `stmt` is the interpreter's `stmt_hash16` (content identity, already on
/// `StatementCompleted`/`PlanHalted`); `cells` are the `(op, content)` pairs attributed by trace
/// interleaving (cells land before their statement's completion row).
#[derive(Debug, Clone, PartialEq)]
pub struct StmtRow {
    pub node: u32,
    pub stmt: String,
    pub halted: bool,
    pub cells: Vec<(String, String)>,
}

/// One aligned row of a two-run diff (C-44).
#[derive(Debug, Clone, PartialEq)]
pub enum DiffRow {
    /// Same statement content, same op outputs.
    Same { node: u32, stmt: String },
    /// The PLANS diverge at this position: the statement content differs (or exists on only one
    /// side — the other field is `None`).
    Plan {
        node: u32,
        a_stmt: Option<String>,
        b_stmt: Option<String>,
    },
    /// The same statement hit a DIFFERENT WORLD: identical content, differing recorded output.
    /// `op` is the first differing dispatch; `a`/`b` are its recorded (redacted) contents.
    Output {
        node: u32,
        stmt: String,
        op: String,
        a: String,
        b: String,
    },
}

/// The C-44 two-run diff: `rows` in execution order, `identical` when every row is `Same`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunDiff {
    pub rows: Vec<DiffRow>,
    pub identical: bool,
}

/// Fold a trace into its executed-statement rows: cells accumulate and attach to the next
/// statement boundary (`StatementCompleted` or `PlanHalted` — the interpreter appends the
/// boundary AFTER the statement's dispatches).
pub fn stmt_rows(trace: &[RunEvent]) -> Vec<StmtRow> {
    let mut rows = Vec::new();
    let mut cells: Vec<(String, String)> = Vec::new();
    for ev in trace {
        match ev {
            RunEvent::OpRecorded { op, content, .. } => {
                cells.push((op.clone(), content.clone()));
            }
            RunEvent::StatementCompleted { node, stmt, .. } => rows.push(StmtRow {
                node: node.0,
                stmt: stmt.clone(),
                halted: false,
                cells: std::mem::take(&mut cells),
            }),
            RunEvent::PlanHalted { node, stmt, .. } => rows.push(StmtRow {
                node: node.0,
                stmt: stmt.clone(),
                halted: true,
                cells: std::mem::take(&mut cells),
            }),
            _ => {}
        }
    }
    rows
}

/// Diff two run traces (C-44): positional alignment over the executed-statement sequence — the
/// natural shape for "a run and its fork" (shared prefix, then divergence). Classification:
/// differing `stmt_hash16` → the plan changed (`DiffRow::Plan`); same statement but differing
/// recorded op output → the world changed (`DiffRow::Output`); else `Same`.
pub fn run_diff(a: &[RunEvent], b: &[RunEvent]) -> RunDiff {
    let ra = stmt_rows(a);
    let rb = stmt_rows(b);
    let mut rows = Vec::new();
    let n = ra.len().max(rb.len());
    for i in 0..n {
        match (ra.get(i), rb.get(i)) {
            (Some(x), Some(y)) if x.stmt == y.stmt => {
                let differing = x
                    .cells
                    .iter()
                    .zip(y.cells.iter())
                    .find(|((oa, ca), (ob, cb))| oa != ob || ca != cb);
                match differing {
                    Some(((op, ca), (_, cb))) => rows.push(DiffRow::Output {
                        node: x.node,
                        stmt: x.stmt.clone(),
                        op: op.clone(),
                        a: ca.clone(),
                        b: cb.clone(),
                    }),
                    None if x.cells.len() != y.cells.len() => {
                        // One side dispatched more (e.g. a halt cut the statement short) — an
                        // output divergence with the first unpaired cell.
                        let (longer, shorter_is_a) = if x.cells.len() > y.cells.len() {
                            (&x.cells[y.cells.len()], false)
                        } else {
                            (&y.cells[x.cells.len()], true)
                        };
                        rows.push(DiffRow::Output {
                            node: x.node,
                            stmt: x.stmt.clone(),
                            op: longer.0.clone(),
                            a: if shorter_is_a {
                                String::new()
                            } else {
                                longer.1.clone()
                            },
                            b: if shorter_is_a {
                                longer.1.clone()
                            } else {
                                String::new()
                            },
                        });
                    }
                    None => rows.push(DiffRow::Same {
                        node: x.node,
                        stmt: x.stmt.clone(),
                    }),
                }
            }
            (xa, xb) => rows.push(DiffRow::Plan {
                node: xa.or(xb).map(|r| r.node).unwrap_or_default(),
                a_stmt: xa.map(|r| r.stmt.clone()),
                b_stmt: xb.map(|r| r.stmt.clone()),
            }),
        }
    }
    let identical = rows.iter().all(|r| matches!(r, DiffRow::Same { .. }));
    RunDiff { rows, identical }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::EventContext;
    use crate::kind::EventKind;
    use flux_core::Message;

    /// Build a minimal StoredEvent for projection unit tests.
    fn ev(global_seq: i64, stream_seq: i64, turn_id: Option<i64>, kind: EventKind) -> StoredEvent {
        StoredEvent {
            global_seq,
            stream: "s_1".to_string(),
            stream_seq,
            id: format!("e{global_seq}"),
            turn_id,
            schema_version: 1,
            ts_ms: 1000 + global_seq,
            kind,
            context: EventContext::default(),
        }
    }

    #[test]
    fn conversation_folds_messages_in_order() {
        let events = vec![
            ev(1, 0, None, EventKind::SessionStarted { model: "m".into() }),
            ev(2, 1, None, EventKind::Message(Message::user_text("hi"))),
            ev(
                3,
                2,
                None,
                EventKind::Run(RunEvent::FlowReturned {
                    value: "v_1".into(),
                }),
            ),
            ev(
                4,
                3,
                None,
                EventKind::Message(Message::assistant_text("hello")),
            ),
        ];
        let convo = conversation(&events);
        assert_eq!(convo.len(), 2);
        assert_eq!(convo[0].text(), "hi");
        assert_eq!(convo[1].text(), "hello");
    }

    #[test]
    fn compaction_resets_the_fold_then_continues() {
        let events = vec![
            ev(1, 0, None, EventKind::Message(Message::user_text("a"))),
            ev(2, 1, None, EventKind::Message(Message::user_text("b"))),
            ev(
                3,
                2,
                None,
                EventKind::Compacted {
                    messages: vec![Message::user_text("summary"), Message::user_text("recent")],
                },
            ),
            ev(4, 3, None, EventKind::Message(Message::user_text("more"))),
        ];
        let convo = conversation(&events);
        assert_eq!(
            convo.iter().map(|m| m.text()).collect::<Vec<_>>(),
            vec!["summary", "recent", "more"]
        );
    }

    #[test]
    fn multiple_compactions_keep_only_the_latest_snapshot() {
        let events = vec![
            ev(1, 0, None, EventKind::Message(Message::user_text("a"))),
            ev(
                2,
                1,
                None,
                EventKind::Compacted {
                    messages: vec![Message::user_text("first")],
                },
            ),
            ev(
                3,
                2,
                None,
                EventKind::Compacted {
                    messages: vec![Message::user_text("second")],
                },
            ),
        ];
        let convo = conversation(&events);
        assert_eq!(convo.len(), 1);
        assert_eq!(convo[0].text(), "second");
    }

    #[test]
    fn run_trace_keeps_only_run_events_in_order() {
        let events = vec![
            ev(1, 0, None, EventKind::Message(Message::user_text("hi"))),
            ev(
                2,
                1,
                None,
                EventKind::Run(RunEvent::StepSucceeded {
                    step: "s1".into(),
                    output: "v_1".into(),
                }),
            ),
            ev(
                3,
                2,
                None,
                EventKind::Run(RunEvent::FlowReturned {
                    value: "v_1".into(),
                }),
            ),
        ];
        let trace = run_trace(&events);
        assert_eq!(trace.len(), 2);
        assert!(matches!(trace[0], RunEvent::StepSucceeded { .. }));
        assert!(matches!(trace[1], RunEvent::FlowReturned { .. }));
    }

    #[test]
    fn turns_fold_telemetry_by_turn_id() {
        let events = vec![
            ev(
                10,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "do it".into(),
                    model: "m".into(),
                },
            ),
            ev(
                11,
                1,
                Some(10),
                EventKind::PlanAttempted {
                    step: 0,
                    outcome: "compile_error".into(),
                    error: Some("boom".into()),
                    fingerprint: None,
                    plan_text: None,
                    phase: None,
                    plan_source: None,
                },
            ),
            ev(
                12,
                2,
                Some(10),
                EventKind::PlanAttempted {
                    step: 1,
                    outcome: "accepted".into(),
                    error: None,
                    fingerprint: Some("abc123".into()),
                    plan_text: Some("$x = read(\"a\")".into()),
                    phase: Some("execute".into()),
                    plan_source: Some("flow\n  $x = read(\"a\")".into()),
                },
            ),
            ev(
                13,
                3,
                Some(10),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 2,
                    answer: "done".into(),
                    usage: Some(Usage {
                        input_tokens: 100,
                        output_tokens: 20,
                        ..Default::default()
                    }),
                },
            ),
        ];
        let turns = turns(&events);
        assert_eq!(turns.len(), 1);
        let t = &turns[0];
        assert_eq!(t.turn_id, 10);
        assert_eq!(t.user_input, "do it");
        assert_eq!(t.outcome, "accepted");
        assert_eq!(t.iterations, 2);
        assert_eq!(t.answer.as_deref(), Some("done"));
        assert_eq!(t.plan_attempts.len(), 2);
        assert_eq!(t.plan_attempts[0].outcome, "compile_error");
        assert_eq!(t.plan_attempts[0].error.as_deref(), Some("boom"));
        assert_eq!(
            t.plan_attempts[0].phase, None,
            "pre-A-14 attempts decode with no phase"
        );
        assert_eq!(
            t.plan_attempts[1].phase.as_deref(),
            Some("execute"),
            "phase folds through"
        );
        assert_eq!(
            t.plan_attempts[0].plan_source, None,
            "non-accepted attempts carry no plan_source"
        );
        assert_eq!(
            t.plan_attempts[1].plan_source.as_deref(),
            Some("flow\n  $x = read(\"a\")"),
            "the canonical plan source folds through (L-38)"
        );
        assert!(t.ended_at_ms.is_some());
        assert_eq!(t.usage.as_ref().map(|u| u.total()), Some(120));
    }

    /// L-38 back-compat: a `PlanAttempted` event serialized before `plan_source` existed (the
    /// stored-JSON shape of every pre-L-38 events.db row) decodes with `plan_source: None` — the
    /// same serde-default contract `phase` established (A-14).
    #[test]
    fn pre_l38_plan_attempted_rows_decode_without_plan_source() {
        let old = serde_json::json!({
            "kind": "plan_attempted",
            "data": {
                "step": 1,
                "outcome": "accepted",
                "fingerprint": "abc123",
                "plan_text": "$x = read(\"a\")",
                "phase": "execute",
            },
        });
        let kind: EventKind = serde_json::from_value(old).expect("old row decodes");
        match kind {
            EventKind::PlanAttempted {
                plan_source, phase, ..
            } => {
                assert_eq!(plan_source, None, "missing field defaults to None");
                assert_eq!(phase.as_deref(), Some("execute"));
            }
            other => panic!("decoded the wrong kind: {other:?}"),
        }
    }

    /// D-53's failing-first test: a stream with two accepted plans — one carrying `plan_source`
    /// (L-38), one in the pre-L-38 shape (`plan_source: None`) — pairs and exports exactly the
    /// ONE row with `plan_source`, skipping the other and counting it rather than guessing.
    #[test]
    fn corpus_rows_pairs_accepted_plan_with_its_turn_and_skips_missing_plan_source() {
        let events = vec![
            // Turn 10: a pre-L-38 accepted plan (no plan_source) — must be skipped and counted.
            ev(
                10,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "old-style request".into(),
                    model: "m".into(),
                },
            ),
            ev(
                11,
                1,
                Some(10),
                EventKind::PlanAttempted {
                    step: 0,
                    outcome: "accepted".into(),
                    error: None,
                    fingerprint: Some("fp-old".into()),
                    plan_text: Some("$x = read(\"a\")".into()),
                    phase: None,
                    plan_source: None,
                },
            ),
            // Turn 20: a real L-38 accepted plan — must export, paired with THIS turn's user_input.
            ev(
                20,
                2,
                None,
                EventKind::TurnStarted {
                    user_input: "summarize the README".into(),
                    model: "m".into(),
                },
            ),
            ev(
                21,
                3,
                Some(20),
                EventKind::PlanAttempted {
                    step: 0,
                    outcome: "accepted".into(),
                    error: None,
                    fingerprint: Some("fp-new".into()),
                    plan_text: Some("$x = read(\"README.md\")".into()),
                    phase: Some("execute".into()),
                    plan_source: Some("flow\n  $x = read(\"README.md\")".into()),
                },
            ),
        ];

        let (rows, skips) = corpus_rows("s_1", &events);
        assert_eq!(rows.len(), 1, "exactly one row exports: {rows:?}");
        assert_eq!(skips.no_plan_source, 1, "the pre-L-38 row is counted");
        assert_eq!(skips.ambiguous_pairing, 0);
        assert_eq!(skips.total(), 1);

        let row = &rows[0];
        assert_eq!(row.id, "e21", "the PlanAttempted event's own stable id");
        assert_eq!(row.nl_goal, "summarize the README");
        assert_eq!(row.source, "flow\n  $x = read(\"README.md\")");
        assert_eq!(row.session, "s_1");
        assert_eq!(row.turn, 20);
    }

    /// An accepted plan whose `turn_id` never resolves to a `TurnStarted` in the stream (orphaned —
    /// e.g. a truncated/corrupted log) is dropped as an ambiguous pairing, not guessed at.
    #[test]
    fn corpus_rows_skips_plan_attempted_with_unresolved_turn_id() {
        let events = vec![ev(
            5,
            0,
            Some(999), // no TurnStarted with global_seq 999 in this stream
            EventKind::PlanAttempted {
                step: 0,
                outcome: "accepted".into(),
                error: None,
                fingerprint: None,
                plan_text: None,
                phase: None,
                plan_source: Some("flow\n  $x = read(\"a\")".into()),
            },
        )];
        let (rows, skips) = corpus_rows("s_1", &events);
        assert!(rows.is_empty());
        assert_eq!(skips.ambiguous_pairing, 1);
        assert_eq!(skips.no_plan_source, 0);
    }

    #[test]
    fn unclosed_turn_stays_pending() {
        let events = vec![ev(
            10,
            0,
            None,
            EventKind::TurnStarted {
                user_input: "hi".into(),
                model: "m".into(),
            },
        )];
        let turns = turns(&events);
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].outcome, "pending");
        assert!(turns[0].ended_at_ms.is_none());
    }

    fn usage_with(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Default::default()
        }
    }

    /// C-06 attribution: a turn that switches model mid-flight (`/model`) must attribute each
    /// `CallUsage` to the model that was ACTIVE for that call, not to the turn's `TurnStarted` model
    /// nor to whichever model is active by the time the fold finishes. Fixture: one turn started on
    /// `model-a`, a `CallUsage` under `model-a`, then a `ModelChanged` to `model-b` mid-turn, then a
    /// second `CallUsage` under `model-b` before the turn ends.
    #[test]
    fn usage_attributed_per_model_after_switch() {
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "do the thing".into(),
                    model: "model-a".into(),
                },
            ),
            ev(
                2,
                1,
                Some(1),
                EventKind::CallUsage {
                    model: "model-a".into(),
                    usage: usage_with(100, 10),
                },
            ),
            // Mid-turn model switch (the REPL `/model` command).
            ev(
                3,
                2,
                None,
                EventKind::ModelChanged {
                    model: "model-b".into(),
                },
            ),
            ev(
                4,
                3,
                Some(1),
                EventKind::CallUsage {
                    model: "model-b".into(),
                    usage: usage_with(50, 5),
                },
            ),
            ev(
                5,
                4,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 2,
                    answer: "done".into(),
                    usage: Some(usage_with(150, 15)),
                },
            ),
        ];

        let pricing = PricingTable::builtin();
        let summary = cost_summary(&events, &pricing);
        assert_eq!(summary.len(), 2, "two distinct models: {summary:?}");

        let a = summary.iter().find(|m| m.model == "model-a").unwrap();
        assert_eq!(a.usage.input_tokens, 100);
        assert_eq!(a.usage.output_tokens, 10);
        assert_eq!(a.calls, 1);

        let b = summary.iter().find(|m| m.model == "model-b").unwrap();
        assert_eq!(b.usage.input_tokens, 50);
        assert_eq!(b.usage.output_tokens, 5);
        assert_eq!(b.calls, 1);

        // Neither model's slice is double-counted or merged into the other — the switch didn't
        // smear model-a's tokens onto model-b (or vice versa) despite sharing one `TurnStarted`.
        assert_eq!(a.usage.total() + b.usage.total(), 165);
    }

    /// C-15: the turns fold counts each turn's provider calls and sums their per-call usage —
    /// the per-turn attribution `TurnEnded.usage` (a replace-style total) can't give.
    #[test]
    fn turns_fold_per_turn_call_counts_and_cache_usage() {
        let cached = Usage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 900,
            cache_creation_input_tokens: 50,
            ..Default::default()
        };
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "go".into(),
                    model: "m".into(),
                },
            ),
            ev(
                2,
                1,
                Some(1),
                EventKind::CallUsage {
                    model: "m".into(),
                    usage: cached.clone(),
                },
            ),
            ev(
                3,
                2,
                Some(1),
                EventKind::CallUsage {
                    model: "m".into(),
                    usage: cached.clone(),
                },
            ),
            ev(
                4,
                3,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "ok".into(),
                    iterations: 2,
                    answer: "done".into(),
                    usage: Some(usage_with(150, 15)),
                },
            ),
        ];
        let ts = turns(&events);
        assert_eq!(ts.len(), 1);
        assert_eq!(ts[0].calls, 2, "both provider calls counted");
        assert_eq!(ts[0].call_usage.input_tokens, 200);
        assert_eq!(ts[0].call_usage.cache_read_input_tokens, 1800);
        assert_eq!(ts[0].call_usage.cache_creation_input_tokens, 100);
        assert_eq!(ts[0].call_usage.output_tokens, 20);
    }

    /// C-15: the efficiency rollup reports calls/turn and the cache-read share of the prompt side.
    #[test]
    fn efficiency_summary_reports_calls_per_turn_and_cache_read_share() {
        let cached = Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_input_tokens: 300,
            cache_creation_input_tokens: 100,
            ..Default::default()
        };
        let mut events = Vec::new();
        // Two completed turns: 2 calls and 1 call respectively; plus one PENDING turn that must
        // not skew the averages.
        let mut seq = 0i64;
        let mut push = |turn_id: Option<i64>, kind: EventKind| {
            seq += 1;
            events.push(ev(seq, seq - 1, turn_id, kind));
        };
        push(
            None,
            EventKind::TurnStarted {
                user_input: "a".into(),
                model: "m".into(),
            },
        ); // turn 1
        push(
            Some(1),
            EventKind::CallUsage {
                model: "m".into(),
                usage: cached.clone(),
            },
        );
        push(
            Some(1),
            EventKind::CallUsage {
                model: "m".into(),
                usage: cached.clone(),
            },
        );
        push(
            Some(1),
            EventKind::TurnEnded {
                outcome: "ok".into(),
                iterations: 3,
                answer: "x".into(),
                usage: None,
            },
        );
        push(
            None,
            EventKind::TurnStarted {
                user_input: "b".into(),
                model: "m".into(),
            },
        ); // turn 5
        push(
            Some(5),
            EventKind::CallUsage {
                model: "m".into(),
                usage: cached.clone(),
            },
        );
        push(
            Some(5),
            EventKind::TurnEnded {
                outcome: "ok".into(),
                iterations: 1,
                answer: "y".into(),
                usage: None,
            },
        );
        push(
            None,
            EventKind::TurnStarted {
                user_input: "never ends".into(),
                model: "m".into(),
            },
        );

        let e = efficiency_summary(&events).expect("two completed turns");
        assert_eq!(e.turns, 2, "the pending turn is excluded");
        assert_eq!(e.calls, 3);
        assert!((e.avg_calls_per_turn() - 1.5).abs() < f64::EPSILON);
        assert!((e.avg_iterations_per_turn() - 2.0).abs() < f64::EPSILON);
        // Per call: 300 cached vs 200 uncached (100 input + 100 cache write) → 60% cache-read.
        assert!(
            (e.cache_read_share() - 0.6).abs() < 1e-9,
            "{}",
            e.cache_read_share()
        );
        assert!((e.uncached_input_per_turn() - 300.0).abs() < f64::EPSILON);
        assert!((e.output_per_turn() - 75.0).abs() < f64::EPSILON);
    }

    /// I-03: the efficiency rollup reports the phased loop's rounds — gather rounds count every
    /// gather-phase attempt (a repair round is a paid round), revise rounds only execute-phase
    /// attempts that accepted a further plan (the terminal chat round is not a revision), and
    /// plans/turn counts accepted plans phase-blind so pre-A-14 logs still report it.
    #[test]
    fn efficiency_summary_reports_phase_rounds_and_plans_per_turn() {
        let attempt = |phase: Option<&str>, outcome: &str| EventKind::PlanAttempted {
            step: 0,
            outcome: outcome.into(),
            error: None,
            fingerprint: None,
            plan_text: None,
            phase: phase.map(str::to_string),
            plan_source: None,
        };
        let mut events = Vec::new();
        let mut seq = 0i64;
        let mut push = |turn_id: Option<i64>, kind: EventKind| {
            seq += 1;
            events.push(ev(seq, seq - 1, turn_id, kind));
        };
        // Turn 1 — a phased turn: orient emits a gather plan, one gather round needs a compile
        // repair, the next settles the execution plan, one execute-phase revision, then the
        // terminal chat round.
        push(
            None,
            EventKind::TurnStarted {
                user_input: "a".into(),
                model: "m".into(),
            },
        );
        push(Some(1), attempt(Some("orient"), "accepted"));
        push(Some(1), attempt(Some("gather"), "compile_error"));
        push(Some(1), attempt(Some("gather"), "accepted"));
        push(Some(1), attempt(Some("execute"), "accepted"));
        push(Some(1), attempt(Some("execute"), "chat"));
        push(
            Some(1),
            EventKind::TurnEnded {
                outcome: "ok".into(),
                iterations: 3,
                answer: "x".into(),
                usage: None,
            },
        );
        // Turn 8 — a pre-A-14 log shape: one phase-less accepted plan. Counts toward plans/turn,
        // never toward phase rounds.
        push(
            None,
            EventKind::TurnStarted {
                user_input: "b".into(),
                model: "m".into(),
            },
        );
        push(Some(8), attempt(None, "accepted"));
        push(
            Some(8),
            EventKind::TurnEnded {
                outcome: "ok".into(),
                iterations: 1,
                answer: "y".into(),
                usage: None,
            },
        );

        let e = efficiency_summary(&events).expect("two completed turns");
        assert_eq!(e.turns, 2);
        assert_eq!(e.orient_rounds, 1);
        assert_eq!(e.gather_rounds, 2, "repair + settle rounds both count");
        assert_eq!(e.revise_rounds, 1, "terminal chat round is not a revision");
        assert_eq!(
            e.accepted_plans, 4,
            "orient + gather + execute + phase-less"
        );
        assert!(e.has_phase_rounds());
        assert!((e.avg_gather_rounds_per_turn() - 1.0).abs() < f64::EPSILON);
        assert!((e.avg_revise_rounds_per_turn() - 0.5).abs() < f64::EPSILON);
        assert!((e.avg_plans_per_turn() - 2.0).abs() < f64::EPSILON);

        // A pre-A-14-only log reports no phase rounds at all — the figures are unrecorded, not 0.
        let old_only = efficiency_summary(&events[7..]).expect("the phase-less turn");
        assert!(!old_only.has_phase_rounds());
    }

    /// C-15: legacy attribution-key variants merge on the read side — a bare `gpt-5.5` folds into
    /// its sole prefixed sibling `openai/gpt-5.5`, and two regional Bedrock stamps under one
    /// provider collapse to the region-less id. An AMBIGUOUS bare key (two candidate providers)
    /// stays separate — providers bill differently, never guess.
    #[test]
    fn cost_summary_merges_bare_and_prefixed_model_keys() {
        let call = |seq: i64, model: &str| {
            ev(
                seq,
                seq - 1,
                Some(1),
                EventKind::CallUsage {
                    model: model.into(),
                    usage: usage_with(100, 10),
                },
            )
        };
        let pricing = PricingTable::builtin();

        // Bare + prefixed variant of one backend → ONE row under the canonical key.
        let events = vec![call(1, "gpt-5.5"), call(2, "openai/gpt-5.5")];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].model, "openai/gpt-5.5");
        assert_eq!(rows[0].calls, 2);
        assert_eq!(rows[0].usage.input_tokens, 200);

        // Two regional Bedrock stamps under one provider → the region-less canonical id.
        let events = vec![
            call(1, "aws/us.anthropic.claude-sonnet-4-6"),
            call(2, "aws/eu.anthropic.claude-sonnet-4-6"),
        ];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].model, "aws/anthropic.claude-sonnet-4-6");
        assert_eq!(rows[0].calls, 2);

        // Ambiguous: the bare id has TWO prefixed siblings — it must stay its own row.
        let events = vec![
            call(1, "claude-sonnet-4-6"),
            call(2, "anthropic/claude-sonnet-4-6"),
            call(3, "claude/claude-sonnet-4-6"),
        ];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(rows.len(), 3, "ambiguous bare key never merges: {rows:?}");
    }

    /// C-30: a passthrough key (`openrouter-anthropic/anthropic/…`) survives the merge unchanged
    /// and never folds into a plain `anthropic/…` row — those are different billing providers
    /// (rows written before the C-30 attribution fix stay separate, by design).
    #[test]
    fn merge_keeps_passthrough_provider_rows_separate() {
        let call = |seq: i64, model: &str| {
            ev(
                seq,
                seq - 1,
                Some(1),
                EventKind::CallUsage {
                    model: model.into(),
                    usage: usage_with(100, 10),
                },
            )
        };
        let pricing = PricingTable::builtin();
        let events = vec![
            call(1, "openrouter-anthropic/anthropic/claude-sonnet-4.6"),
            call(2, "anthropic/claude-sonnet-4.6"),
        ];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(
            rows.len(),
            2,
            "passthrough must not fold into anthropic: {rows:?}"
        );
        let keys: Vec<&str> = rows.iter().map(|r| r.model.as_str()).collect();
        assert!(
            keys.contains(&"openrouter-anthropic/anthropic/claude-sonnet-4.6"),
            "{keys:?}"
        );
        assert!(keys.contains(&"anthropic/claude-sonnet-4.6"), "{keys:?}");
    }

    /// `cost_summary` rolls up multiple turns, multiple models, and cache tiers, folding
    /// `CallUsage` events by model and pricing the total via the built-in table.
    #[test]
    fn cost_summary_rolls_up_session() {
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "first".into(),
                    model: "claude-sonnet-4-6".into(),
                },
            ),
            ev(
                2,
                1,
                Some(1),
                EventKind::CallUsage {
                    model: "claude-sonnet-4-6".into(),
                    usage: Usage {
                        input_tokens: 1_000_000,
                        output_tokens: 100_000,
                        cache_creation_input_tokens: 200_000,
                        cache_read_input_tokens: 500_000,
                        reasoning_tokens: 0,
                        ..Default::default()
                    },
                },
            ),
            ev(
                3,
                2,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(1_000_000, 100_000)),
                },
            ),
            // A second turn, same model — its usage must be SUMMED with the first, not replaced.
            ev(
                4,
                3,
                None,
                EventKind::TurnStarted {
                    user_input: "second".into(),
                    model: "claude-sonnet-4-6".into(),
                },
            ),
            ev(
                5,
                4,
                Some(4),
                EventKind::CallUsage {
                    model: "claude-sonnet-4-6".into(),
                    usage: Usage {
                        input_tokens: 500_000,
                        output_tokens: 50_000,
                        ..Default::default()
                    },
                },
            ),
            ev(
                6,
                5,
                Some(4),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(500_000, 50_000)),
                },
            ),
            // A third turn on a different model entirely.
            ev(
                7,
                6,
                None,
                EventKind::TurnStarted {
                    user_input: "third".into(),
                    model: "gpt-5.5".into(),
                },
            ),
            ev(
                8,
                7,
                Some(7),
                EventKind::CallUsage {
                    model: "gpt-5.5".into(),
                    usage: usage_with(1_000_000, 1_000_000),
                },
            ),
            ev(
                9,
                8,
                Some(7),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(1_000_000, 1_000_000)),
                },
            ),
        ];

        let pricing = PricingTable::builtin();
        let summary = cost_summary(&events, &pricing);
        assert_eq!(summary.len(), 2, "two models rolled up: {summary:?}");

        // claude-sonnet-4-6: two calls summed field-wise (1.5M input, 150K output, 200K cache
        // write, 500K cache read) — NOT replaced (would lose the first turn's cache tiers).
        let sonnet = summary
            .iter()
            .find(|m| m.model == "claude-sonnet-4-6")
            .unwrap();
        assert_eq!(sonnet.calls, 2);
        assert_eq!(sonnet.usage.input_tokens, 1_500_000);
        assert_eq!(sonnet.usage.output_tokens, 150_000);
        assert_eq!(sonnet.usage.cache_creation_input_tokens, 200_000);
        assert_eq!(sonnet.usage.cache_read_input_tokens, 500_000);
        // cost = 1.5·3 + 0.15·15 + 0.2·3.75 + 0.5·0.30 = 4.5 + 2.25 + 0.75 + 0.15 = 7.65
        let sonnet_cost = sonnet.cost.expect("claude-sonnet-4-6 is a priced model");
        assert!(
            (sonnet_cost.usd - 7.65).abs() < 1e-9,
            "got {}",
            sonnet_cost.usd
        );
        assert!(
            !sonnet_cost.subscription,
            "bare model id is not a subscription spec"
        );

        let gpt = summary.iter().find(|m| m.model == "gpt-5.5").unwrap();
        assert_eq!(gpt.calls, 1);
        assert_eq!(gpt.usage.input_tokens, 1_000_000);
        assert_eq!(gpt.usage.output_tokens, 1_000_000);
        // cost = 1·5 + 1·30 = 35 (builtin gpt-5.5 rates, corrected in C-20)
        let gpt_cost = gpt.cost.expect("gpt-5.5 is a priced model");
        assert!((gpt_cost.usd - 35.0).abs() < 1e-9, "got {}", gpt_cost.usd);
    }

    /// Old logs recorded before per-call attribution existed carry no `CallUsage` events at all —
    /// only `TurnStarted.model` + `TurnEnded.usage`. `cost_summary` must still roll them up (via the
    /// turn-level fallback), attributing each turn's total to that turn's `TurnStarted` model, rather
    /// than silently reporting zero spend for every session ever recorded before this feature shipped.
    #[test]
    fn cost_summary_falls_back_to_turn_totals_for_old_logs_without_call_usage() {
        let events = vec![
            ev(
                1,
                0,
                None,
                EventKind::TurnStarted {
                    user_input: "hi".into(),
                    model: "claude-opus-4-8".into(),
                },
            ),
            // No CallUsage event — an old log written before C-06.
            ev(
                2,
                1,
                Some(1),
                EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "done".into(),
                    usage: Some(usage_with(1_000_000, 1_000_000)),
                },
            ),
        ];

        let pricing = PricingTable::builtin();
        let summary = cost_summary(&events, &pricing);
        assert_eq!(summary.len(), 1);
        let row = &summary[0];
        assert_eq!(row.model, "claude-opus-4-8");
        assert_eq!(row.usage.input_tokens, 1_000_000);
        assert_eq!(row.usage.output_tokens, 1_000_000);
        assert_eq!(row.calls, 1, "one turn folded via the fallback");
        // opus: 1·5 + 1·25 = 30
        let cost = row.cost.expect("claude-opus-4-8 is a priced model");
        assert!((cost.usd - 30.0).abs() < 1e-9, "got {}", cost.usd);
    }

    /// C-34: `cost_summary` must price each `CallUsage` INDIVIDUALLY — reported cost when the call
    /// reported one, else the table — and SUM the per-call prices. Naively re-pricing the row's
    /// already-summed `Usage` (i.e. checking `usage.reported_cost_usd` once on the aggregate) would
    /// let a couple of reported calls short-circuit the table for the REST of a tabled model's
    /// calls and silently under-report the row.
    #[test]
    fn cost_summary_prices_each_call_reported_or_table() {
        let call = |seq: i64, model: &str, usage: Usage| {
            ev(
                seq,
                seq - 1,
                Some(1),
                EventKind::CallUsage {
                    model: model.into(),
                    usage,
                },
            )
        };
        let pricing = PricingTable::builtin();

        // A TABLED model (claude-sonnet-4-6): one plain (table-priced) call + one reported call.
        // The row's total must be the table price for the first PLUS the reported figure for the
        // second — table math is linear so the table-priced call alone prices at 3+15 = 18.0.
        let events = vec![
            call(1, "claude-sonnet-4-6", usage_with(1_000_000, 1_000_000)),
            call(
                2,
                "claude-sonnet-4-6",
                Usage {
                    input_tokens: 1_000_000,
                    output_tokens: 1_000_000,
                    reported_cost_usd: Some(5.0),
                    ..Default::default()
                },
            ),
        ];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row.calls, 2);
        let cost = row.cost.expect("mixed row must still price");
        assert!(
            (cost.usd - 23.0).abs() < 1e-9,
            "18.0 table + 5.0 reported, got {}",
            cost.usd
        );
        assert_eq!(
            cost.source,
            CostSource::Estimated,
            "a mix with even one table-priced call is not purely reported"
        );

        // An UNTABLED model where EVERY call reported → Some/Reported, the summed reported figures
        // (the exact scenario that used to be permanently `None` / `$?`).
        let events = vec![
            call(
                1,
                "openrouter/some/untabled-model",
                Usage {
                    reported_cost_usd: Some(0.001),
                    ..Default::default()
                },
            ),
            call(
                2,
                "openrouter/some/untabled-model",
                Usage {
                    reported_cost_usd: Some(0.002),
                    ..Default::default()
                },
            ),
        ];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let cost = rows[0]
            .cost
            .expect("an all-reported untabled model must price");
        assert!((cost.usd - 0.003).abs() < 1e-9, "got {}", cost.usd);
        assert_eq!(cost.source, CostSource::Reported);

        // An UNTABLED model with NO reported calls → None, unchanged from before C-34.
        let events = vec![call(
            1,
            "openrouter/some/untabled-model",
            usage_with(100, 10),
        )];
        let rows = cost_summary(&events, &pricing);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert!(
            rows[0].cost.is_none(),
            "untabled + unreported must stay None"
        );
    }

    /// D-55: one flux stream, `Message`/`TurnStarted`/`CallUsage`/`TurnEnded` events, folded once
    /// plain and once with an app's `Custom` rows (an audit trail riding the same log) interleaved
    /// at several points — before the turn, mid-turn, and after it closes. The `conversation`,
    /// `cost_summary`, and `turns` projections must fold to IDENTICAL results either way: a
    /// consumer's app facts must be fully invisible to flux's own read models.
    #[test]
    fn custom_events_interleaved_dont_affect_conversation_cost_or_turns_projections() {
        /// One logical event plus whether it belongs to the turn (stamped with that turn's
        /// `global_seq` once the final position is known — the position shifts depending on how
        /// many `Custom` rows precede it).
        struct Logical {
            kind: EventKind,
            in_turn: bool,
        }

        fn build(items: Vec<Logical>) -> Vec<StoredEvent> {
            let turn_seq = items
                .iter()
                .enumerate()
                .find_map(|(i, e)| matches!(e.kind, EventKind::TurnStarted { .. }).then_some(i))
                .map(|i| (i as i64) + 1);
            items
                .into_iter()
                .enumerate()
                .map(|(i, e)| {
                    let seq = (i as i64) + 1;
                    let turn_id = if e.in_turn { turn_seq } else { None };
                    ev(seq, i as i64, turn_id, e.kind)
                })
                .collect()
        }

        fn scenario(with_custom: bool) -> Vec<StoredEvent> {
            let custom = |n: u32| Logical {
                kind: EventKind::Custom {
                    name: "audit.tool_call".into(),
                    payload: serde_json::json!({"n": n}),
                },
                in_turn: false,
            };
            let mut items = Vec::new();
            if with_custom {
                items.push(custom(0)); // before the turn even starts
            }
            items.push(Logical {
                kind: EventKind::TurnStarted {
                    user_input: "summarize the log".into(),
                    model: "m".into(),
                },
                in_turn: false,
            });
            if with_custom {
                items.push(custom(1)); // mid-turn, before the first message
            }
            items.push(Logical {
                kind: EventKind::Message(Message::user_text("summarize the log")),
                in_turn: false,
            });
            items.push(Logical {
                kind: EventKind::CallUsage {
                    model: "m".into(),
                    usage: usage_with(10, 2),
                },
                in_turn: true,
            });
            if with_custom {
                items.push(custom(2)); // mid-turn, between the call and the answer
            }
            items.push(Logical {
                kind: EventKind::Message(Message::assistant_text("here's the summary")),
                in_turn: false,
            });
            items.push(Logical {
                kind: EventKind::TurnEnded {
                    outcome: "accepted".into(),
                    iterations: 1,
                    answer: "here's the summary".into(),
                    usage: Some(usage_with(10, 2)),
                },
                in_turn: true,
            });
            if with_custom {
                items.push(custom(3)); // after the turn closes
            }
            build(items)
        }

        let plain = scenario(false);
        let with_custom = scenario(true);
        assert_eq!(
            with_custom.len(),
            plain.len() + 4,
            "sanity: the interleaved stream really does carry 4 extra Custom rows"
        );

        assert_eq!(
            conversation(&plain),
            conversation(&with_custom),
            "Custom rows must not perturb the conversation fold"
        );

        let pricing = PricingTable::builtin();
        assert_eq!(
            cost_summary(&plain, &pricing),
            cost_summary(&with_custom, &pricing),
            "Custom rows must not perturb the cost_summary fold"
        );

        // `turns()` keys turn identity off `TurnStarted`'s `global_seq`, which legitimately shifts
        // when Custom rows are appended ahead of it in the same ordered log (that's not a fold bug
        // — it's the same thing a real concurrent app-fact writer would cause). Compare the folded
        // VALUES (what the turn IS), not the position-derived identity/timestamps.
        fn shape(
            t: &TurnSummary,
        ) -> (
            &str,
            &str,
            &str,
            u32,
            Option<&str>,
            u64,
            Usage,
            Option<Usage>,
        ) {
            (
                &t.user_input,
                &t.model,
                &t.outcome,
                t.iterations,
                t.answer.as_deref(),
                t.calls,
                t.call_usage.clone(),
                t.usage.clone(),
            )
        }
        let plain_turns = turns(&plain);
        let with_custom_turns = turns(&with_custom);
        assert_eq!(plain_turns.len(), 1);
        assert_eq!(with_custom_turns.len(), 1);
        assert_eq!(
            shape(&plain_turns[0]),
            shape(&with_custom_turns[0]),
            "Custom rows must not perturb what the turns fold computes"
        );
    }

    /// C-44 (failing-first acceptance): `run_diff` classifies the two divergence kinds exactly —
    /// a differing `stmt_hash16` at a position → one `Plan` row; the same statement with one
    /// differing recorded output → one `Output` row at the right node; identical traces → all
    /// `Same` + `identical`.
    #[test]
    fn run_diff_classifies_plan_vs_output_divergence() {
        use flux_lang::ast::{NodeId, RunEvent, StepId};
        fn cell(op: &str, content: &str) -> RunEvent {
            RunEvent::OpRecorded {
                seq: 0,
                step: StepId(format!("step_{op}_x")),
                op: op.into(),
                input_hash: "h".into(),
                input_hash_redacted: None,
                content: content.into(),
                view: None,
                is_error: false,
                denied: false,
                redacted: false,
                truncated: false,
            }
        }
        fn done(node: u32, stmt: &str) -> RunEvent {
            RunEvent::StatementCompleted {
                plan: "p".into(),
                node: NodeId(node),
                stmt: stmt.into(),
                value: None,
                skipped: false,
            }
        }

        let a = vec![
            cell("read", "alpha"),
            done(0, "s0"),
            cell("write", "W"),
            done(1, "s1"),
        ];

        // Same plan, different world at node 0.
        let b_world = vec![
            cell("read", "BETA"),
            done(0, "s0"),
            cell("write", "W"),
            done(1, "s1"),
        ];
        let d = run_diff(&a, &b_world);
        assert!(!d.identical);
        assert_eq!(
            d.rows
                .iter()
                .filter(|r| matches!(r, DiffRow::Output { node: 0, op, .. } if op == "read"))
                .count(),
            1,
            "exactly one Output row at node 0: {:?}",
            d.rows
        );
        assert!(matches!(d.rows[1], DiffRow::Same { node: 1, .. }));

        // Different plan at position 1.
        let b_plan = vec![
            cell("read", "alpha"),
            done(0, "s0"),
            cell("write", "W"),
            done(1, "sX"),
        ];
        let d = run_diff(&a, &b_plan);
        assert!(matches!(
            &d.rows[1],
            DiffRow::Plan { a_stmt: Some(x), b_stmt: Some(y), .. } if x == "s1" && y == "sX"
        ));

        // Identical traces.
        let d = run_diff(&a, &a);
        assert!(d.identical, "{:?}", d.rows);
    }
}
