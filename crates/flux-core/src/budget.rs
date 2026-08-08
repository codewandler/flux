//! C-542 — one budget vocabulary for wall time, model calls and token spend.
//!
//! Two words carry the whole distinction: a **target** is a declared intent and warns when it is
//! crossed; a **limit** is a hard ceiling and stops execution at a safe boundary. [`BudgetEnvelope`]
//! carries both for one [`BudgetScope`], [`BudgetUsageEvent`] reports measured spend with run /
//! session / turn / segment attribution, and [`BudgetLedger`] is the only thing that adds them up.
//!
//! **No surface recalculates totals.** The enforcing scope owns a ledger and publishes
//! [`BudgetProjection`] — a serializable snapshot of spent versus declared plus the breaches already
//! observed. CLI and TUI render that projection; they never re-derive it from raw usage. C-571's
//! durable Fleet reservation/settlement ledger consumes the same events and projection.
//!
//! **Charging is exactly once.** A ledger charges an event whose scope is its own or a descendant's,
//! keyed by [`BudgetUsageEvent::event_id`]; a repeated id, a pre-summed child total
//! ([`BudgetUsageEvent::rollup`]) and an ancestor's event are each ignored with a distinct
//! [`BudgetCharge`] reason rather than silently doubling a total.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::stream::Usage;

/// The enforceable dimensions of the first budget contract.
///
/// Every dimension here is one Flux can measure honestly from its own clock, its own call counter or
/// provider-reported usage. Dimensions that need a boundary Flux does not own (CPU, RSS, disk) are
/// deliberately absent rather than reported as zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    /// Elapsed wall time in milliseconds — a host-clock gauge, not an additive counter.
    WallTime,
    /// Provider calls made.
    ModelCalls,
    /// Prompt tokens sent (fresh input plus both cache tiers).
    InputTokens,
    /// Tokens generated.
    OutputTokens,
    /// Billable tokens across input, output and cache — [`Usage::total`].
    TotalTokens,
}

impl BudgetDimension {
    /// Every dimension, in projection order.
    pub const ALL: [BudgetDimension; 5] = [
        BudgetDimension::WallTime,
        BudgetDimension::ModelCalls,
        BudgetDimension::InputTokens,
        BudgetDimension::OutputTokens,
        BudgetDimension::TotalTokens,
    ];

    /// The stable wire/UI name of this dimension.
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetDimension::WallTime => "wall_time",
            BudgetDimension::ModelCalls => "model_calls",
            BudgetDimension::InputTokens => "input_tokens",
            BudgetDimension::OutputTokens => "output_tokens",
            BudgetDimension::TotalTokens => "total_tokens",
        }
    }
}

impl fmt::Display for BudgetDimension {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which nesting level a budget or a measured effect belongs to.
///
/// The order is the containment order of `docs/designs/agent-loop-harnesses.md`: a run contains its
/// sessions, a session its turns, a turn its loop segments / model stages. C-571 adds the Fleet and
/// wave levels above [`BudgetScope::Run`] without changing this vocabulary.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum BudgetScope {
    /// One logical run (the outermost locally enforced scope).
    #[default]
    Run,
    /// One agent/session inside the run.
    Session,
    /// One turn of a session.
    Turn,
    /// One loop segment / model stage inside a turn.
    Segment,
}

impl BudgetScope {
    /// Containment depth — smaller is outer.
    fn depth(self) -> u8 {
        match self {
            BudgetScope::Run => 0,
            BudgetScope::Session => 1,
            BudgetScope::Turn => 2,
            BudgetScope::Segment => 3,
        }
    }

    /// Whether spend measured at `other` is this scope's to charge: its own, or a descendant's.
    pub fn charges(self, other: BudgetScope) -> bool {
        self.depth() <= other.depth()
    }

    /// The stable wire/UI name of this scope.
    pub fn as_str(&self) -> &'static str {
        match self {
            BudgetScope::Run => "run",
            BudgetScope::Session => "session",
            BudgetScope::Turn => "turn",
            BudgetScope::Segment => "segment",
        }
    }
}

impl fmt::Display for BudgetScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Declared ceilings for one side of an envelope. `None` means "not declared", which is never
/// enforced and never rendered as zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
}

impl BudgetLimits {
    /// The declared ceiling for one dimension, if any.
    pub fn get(&self, dimension: BudgetDimension) -> Option<u64> {
        match dimension {
            BudgetDimension::WallTime => self.wall_time_ms,
            BudgetDimension::ModelCalls => self.model_calls,
            BudgetDimension::InputTokens => self.input_tokens,
            BudgetDimension::OutputTokens => self.output_tokens,
            BudgetDimension::TotalTokens => self.total_tokens,
        }
    }

    /// Declare (or clear) one dimension.
    pub fn set(&mut self, dimension: BudgetDimension, value: Option<u64>) {
        match dimension {
            BudgetDimension::WallTime => self.wall_time_ms = value,
            BudgetDimension::ModelCalls => self.model_calls = value,
            BudgetDimension::InputTokens => self.input_tokens = value,
            BudgetDimension::OutputTokens => self.output_tokens = value,
            BudgetDimension::TotalTokens => self.total_tokens = value,
        }
    }

    /// Whether nothing at all is declared.
    pub fn is_empty(&self) -> bool {
        BudgetDimension::ALL
            .iter()
            .all(|dimension| self.get(*dimension).is_none())
    }

    /// A hard ceiling on total tokens — the shape A-10's `[limits] turn_token_budget` declares.
    pub fn with_total_tokens(tokens: u64) -> Self {
        Self {
            total_tokens: Some(tokens),
            ..Self::default()
        }
    }
}

/// A soft target beside a hard limit for one scope.
///
/// Crossing `target` warns once per dimension and changes nothing else; crossing `limit` stops at
/// the next safe boundary with a typed [`BudgetBreach`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetEnvelope {
    pub scope: BudgetScope,
    #[serde(default)]
    pub target: BudgetLimits,
    #[serde(default)]
    pub limit: BudgetLimits,
}

impl BudgetEnvelope {
    /// An envelope for `scope` with nothing declared — enforcement off, projection still available.
    pub fn none(scope: BudgetScope) -> Self {
        Self {
            scope,
            target: BudgetLimits::default(),
            limit: BudgetLimits::default(),
        }
    }

    /// Whether this envelope declares nothing at all.
    pub fn is_empty(&self) -> bool {
        self.target.is_empty() && self.limit.is_empty()
    }

    /// The declared figure a surface shows for one dimension: the hard limit when there is one,
    /// otherwise the target.
    pub fn declared(&self, dimension: BudgetDimension) -> Option<u64> {
        self.limit
            .get(dimension)
            .or_else(|| self.target.get(dimension))
    }
}

/// Measured spend. Additive counters add; wall time is elapsed, so it takes the maximum.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSpend {
    /// Elapsed wall time in milliseconds. **Not additive:** two overlapping scopes' windows do not
    /// make the run twice as long, so [`Self::fold`] keeps the longer one.
    #[serde(default)]
    pub wall_time_ms: u64,
    #[serde(default)]
    pub model_calls: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

impl BudgetSpend {
    /// One provider call's token spend. `input_tokens` is the call's prompt occupancy
    /// ([`Usage::context_tokens`]) and `total_tokens` its billable total ([`Usage::total`]), so the
    /// subset fields (reasoning, audio, 1h cache writes) are never double-counted.
    pub fn for_call(usage: &Usage) -> Self {
        Self {
            wall_time_ms: 0,
            model_calls: 1,
            input_tokens: usage.context_tokens(),
            output_tokens: usage.output_tokens,
            total_tokens: usage.total(),
        }
    }

    /// Elapsed wall time only.
    pub fn for_elapsed(wall_time_ms: u64) -> Self {
        Self {
            wall_time_ms,
            ..Self::default()
        }
    }

    /// Spend in one dimension.
    pub fn get(&self, dimension: BudgetDimension) -> u64 {
        match dimension {
            BudgetDimension::WallTime => self.wall_time_ms,
            BudgetDimension::ModelCalls => self.model_calls,
            BudgetDimension::InputTokens => self.input_tokens,
            BudgetDimension::OutputTokens => self.output_tokens,
            BudgetDimension::TotalTokens => self.total_tokens,
        }
    }

    /// Fold another measurement into this accumulator: counters add (saturating), elapsed wall time
    /// takes the maximum.
    pub fn fold(&mut self, other: &BudgetSpend) {
        self.wall_time_ms = self.wall_time_ms.max(other.wall_time_ms);
        self.model_calls = self.model_calls.saturating_add(other.model_calls);
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.total_tokens = self.total_tokens.saturating_add(other.total_tokens);
    }
}

/// Who a measured effect belongs to. The ledger keeps the attribution of the events it charged so a
/// projection (and C-571's durable ledger) can say *where* the spend happened.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetAttribution {
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<i64>,
    /// The loop segment / model stage that spent it (`intent`, `explore`, a named stage, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment: Option<String>,
}

/// One measured effect, reported to every scope that must charge it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsageEvent {
    /// Identity of the measured effect. The dedupe key: the same effect reported twice (a retried
    /// reporting path, a replayed event log) is charged once.
    pub event_id: String,
    /// The innermost scope that owns this spend.
    pub scope: BudgetScope,
    pub attribution: BudgetAttribution,
    pub spend: BudgetSpend,
    /// `true` when this event re-reports spend already charged by finer-grained events (a turn-end
    /// total beside per-call events). A ledger never charges it.
    #[serde(default)]
    pub rollup: bool,
}

impl BudgetUsageEvent {
    /// One provider call's spend, charged to `scope`.
    pub fn for_call(
        event_id: impl Into<String>,
        scope: BudgetScope,
        attribution: BudgetAttribution,
        usage: &Usage,
    ) -> Self {
        Self {
            event_id: event_id.into(),
            scope,
            attribution,
            spend: BudgetSpend::for_call(usage),
            rollup: false,
        }
    }
}

/// Why a ledger did or did not charge an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetCharge {
    /// Charged once.
    Charged,
    /// The same `event_id` was already charged.
    DuplicateIgnored,
    /// A summary of spend already charged through finer-grained events.
    RollupIgnored,
    /// Measured at an ancestor scope; not this scope's spend to charge.
    OutOfScopeIgnored,
}

/// A crossed line, naming exactly what was crossed where.
///
/// This is the typed stop result the loop returns when a hard limit is hit, and the typed warning
/// when a target is crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetBreach {
    pub scope: BudgetScope,
    pub dimension: BudgetDimension,
    pub spent: u64,
    /// The crossed figure — the target for a warning, the hard limit for a stop.
    pub limit: u64,
}

impl fmt::Display for BudgetBreach {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} budget {}: {} of {}",
            self.scope, self.dimension, self.spent, self.limit
        )
    }
}

/// What recording one event did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetOutcome {
    pub charge: BudgetCharge,
    /// The first crossing of a declared target for a dimension — `None` on every later event, so a
    /// surface warns once instead of once per call.
    pub warning: Option<BudgetBreach>,
    /// A crossed hard limit. Execution stops at the next safe boundary; the effect that produced
    /// this event has already finished and is reported normally.
    pub exhausted: Option<BudgetBreach>,
}

/// A serializable snapshot of spent versus declared for one scope — the single source CLI, TUI and
/// C-571's Fleet ledger project.
///
/// [`Default`] is the honest empty snapshot: nothing spent and nothing declared, so
/// [`Self::is_declared`] is `false` and [`Self::declared`] is `None` for every dimension. It is not a
/// zero ceiling.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetProjection {
    pub scope: BudgetScope,
    pub spent: BudgetSpend,
    #[serde(default)]
    pub target: BudgetLimits,
    #[serde(default)]
    pub limit: BudgetLimits,
    /// Targets crossed so far, one entry per dimension.
    #[serde(default)]
    pub warnings: Vec<BudgetBreach>,
    /// The hard limit that has stopped (or will stop) this scope, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted: Option<BudgetBreach>,
    /// The attribution of the most recently charged event, when anything has been charged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<BudgetAttribution>,
}

impl BudgetProjection {
    /// The declared figure to render beside `spent`: the hard limit when there is one, else the
    /// target. `None` means nothing was declared for that dimension — render nothing, not zero.
    pub fn declared(&self, dimension: BudgetDimension) -> Option<u64> {
        self.limit
            .get(dimension)
            .or_else(|| self.target.get(dimension))
    }

    /// Whether anything at all is declared, i.e. whether there is a budget to show.
    pub fn is_declared(&self) -> bool {
        !(self.target.is_empty() && self.limit.is_empty())
    }

    /// Fraction of the declared figure already spent (`None` when nothing is declared). Can exceed
    /// `1.0`: a target may be overspent, and the last effect before a hard stop may overshoot.
    pub fn fraction(&self, dimension: BudgetDimension) -> Option<f64> {
        let declared = self.declared(dimension)?;
        if declared == 0 {
            return None;
        }
        Some(self.spent.get(dimension) as f64 / declared as f64)
    }
}

/// The only place budget totals are added up.
///
/// One ledger enforces one [`BudgetEnvelope`]. It is deliberately clock-free: elapsed wall time
/// arrives as a measured [`BudgetSpend::wall_time_ms`] from the host that owns the clock, which keeps
/// this contract pure, testable and identical for a replayed event log.
#[derive(Debug, Clone)]
pub struct BudgetLedger {
    envelope: BudgetEnvelope,
    spent: BudgetSpend,
    charged: BTreeSet<String>,
    warned: BTreeSet<BudgetDimension>,
    warnings: Vec<BudgetBreach>,
    attribution: Option<BudgetAttribution>,
}

impl BudgetLedger {
    /// A ledger enforcing `envelope`.
    pub fn new(envelope: BudgetEnvelope) -> Self {
        Self {
            envelope,
            spent: BudgetSpend::default(),
            charged: BTreeSet::new(),
            warned: BTreeSet::new(),
            warnings: Vec::new(),
            attribution: None,
        }
    }

    /// The enforced envelope.
    pub fn envelope(&self) -> &BudgetEnvelope {
        &self.envelope
    }

    /// Charged spend so far.
    pub fn spent(&self) -> &BudgetSpend {
        &self.spent
    }

    /// Charge one measured effect, unless it is a duplicate, a rollup or an ancestor's spend.
    pub fn record(&mut self, event: &BudgetUsageEvent) -> BudgetOutcome {
        let charge = if event.rollup {
            BudgetCharge::RollupIgnored
        } else if !self.envelope.scope.charges(event.scope) {
            BudgetCharge::OutOfScopeIgnored
        } else if !self.charged.insert(event.event_id.clone()) {
            BudgetCharge::DuplicateIgnored
        } else {
            self.spent.fold(&event.spend);
            self.attribution = Some(event.attribution.clone());
            BudgetCharge::Charged
        };
        if charge != BudgetCharge::Charged {
            return BudgetOutcome {
                charge,
                warning: None,
                exhausted: None,
            };
        }
        BudgetOutcome {
            charge,
            warning: self.take_warning(),
            exhausted: self.exhausted(),
        }
    }

    /// The hard limit this scope has crossed, if any. Called at a safe boundary — before the next
    /// model call or loop round — never in the middle of an effect that is still finishing.
    pub fn exhausted(&self) -> Option<BudgetBreach> {
        BudgetDimension::ALL.iter().find_map(|dimension| {
            let limit = self.envelope.limit.get(*dimension)?;
            let spent = self.spent.get(*dimension);
            (spent >= limit).then_some(BudgetBreach {
                scope: self.envelope.scope,
                dimension: *dimension,
                spent,
                limit,
            })
        })
    }

    /// A snapshot for surfaces and durable events.
    pub fn projection(&self) -> BudgetProjection {
        BudgetProjection {
            scope: self.envelope.scope,
            spent: self.spent,
            target: self.envelope.target,
            limit: self.envelope.limit,
            warnings: self.warnings.clone(),
            exhausted: self.exhausted(),
            attribution: self.attribution.clone(),
        }
    }

    /// The first target crossing not yet reported, remembering it so it is reported once.
    fn take_warning(&mut self) -> Option<BudgetBreach> {
        for dimension in BudgetDimension::ALL {
            let Some(target) = self.envelope.target.get(dimension) else {
                continue;
            };
            let spent = self.spent.get(dimension);
            if spent < target || self.warned.contains(&dimension) {
                continue;
            }
            self.warned.insert(dimension);
            let breach = BudgetBreach {
                scope: self.envelope.scope,
                dimension,
                spent,
                limit: target,
            };
            self.warnings.push(breach);
            return Some(breach);
        }
        None
    }
}
