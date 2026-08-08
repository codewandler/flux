//! Causal resource-usage receipts (C-575): one append-only ledger of small immutable spans.
//!
//! Design: `docs/designs/resource-accounting.md`. The outcome this module exists for is that a
//! produced result can answer *which measured resources were used to produce it, where, and what
//! they realistically cost* — from one measurement source, rather than from token events and wall
//! time living in unrelated surfaces.
//!
//! ### The shape
//!
//! A [`ResourceRoot`] is one request/result. Every [`ResourceSpan`] recorded under it carries a
//! stable id and an **explicit parent link**, so a run lands as one causal tree ([`span_tree`]).
//! Time-window coincidence is deliberately not a relation here: several workers and a coordinator
//! run concurrently, so "it happened while the story was open" attributes nothing.
//!
//! ### Three properties that are structural rather than conventional
//!
//! * **Typed absence.** A [`Measurement`] is either [`MeasuredValue::Observed`] or
//!   [`MeasuredValue::Absent`] with a reason. There is no numeric zero standing in for "nobody
//!   metered this", because a zero reads downstream as *this cost nothing* and is unrecoverable
//!   once persisted. [`ResourceSpan::measure`] enforces it: a backend that structurally cannot own
//!   a family ([`SpanBackend::absence_for`]) has an offered number replaced by the typed absence,
//!   at the builder, before the store can ever see it.
//! * **Redaction is the caller's job, applied by construction.** [`ResourceSpan::new`] and
//!   [`ResourceSpan::with_phase`] take the live turn's scrubber and run it on the way in — the same
//!   discipline [`MemoryNote::new`](crate::MemoryNote::new) applies, and for the same reason:
//!   `flux-events` owns no scrubber and must not grow one, so the ledger only ever sees scrubbed
//!   text. Labels are then bounded to [`MAX_LABEL_LEN`], because an unbounded label is a payload
//!   smuggling channel regardless of redaction.
//! * **Append-only and idempotent.** A receipt id is *derived* from (root, span) rather than minted
//!   per append ([`ResourceRoot::receipt_id`]), so an at-least-once event pipeline replaying a span
//!   returns the receipt already recorded instead of appending a second, contradictory row. A
//!   correction is a new receipt naming the one it corrects ([`ResourceSpan::correcting`]); the
//!   original stays in the log exactly as measured.
//!
//! ### What is an identity and what is a label
//!
//! The split is load-bearing and is drawn once, here:
//!
//! * **Identities** — root ids, span ids, parent links, receipt ids, and every [`CausalBinding`]
//!   field — are *host-minted* and are never truncated or rewritten. Truncating an identity does
//!   not bound anything useful; it silently merges two spans or mis-attributes a board reference,
//!   which is worse than the size it saves. This matches how the crate already treats stream ids
//!   (`memory:<scope-key>` is likewise unbounded).
//! * **Labels** — the operation, phase and loop-binding descriptions, and a charge's currency and
//!   rate version — are free text near model-adjacent material. Those are scrubbed and bounded.
//!
//! Receipts carry counts, timings, ids and bounded labels. They never carry a prompt, an answer,
//! reasoning, tool arguments or results, command output, file content, a secret-bearing URL or a
//! network payload — there is no field on [`ResourceReceipt`] that could hold one.
//!
//! ### Why [`EventKind::Custom`](crate::EventKind::Custom) rather than a new variant
//!
//! Same reasoning as A-107 memory: [`EventKind`](crate::EventKind) is deliberately closed and not
//! `#[non_exhaustive]`, so a new variant is a breaking change for every downstream `match` — for a
//! fact none of flux's closed projections (conversation, cost, turns, evidence) needs to
//! understand. Receipts ride `Custom` under the reserved [`RESOURCE_NAME_PREFIX`] namespace on
//! their own `resource:<root-id>` stream, and [`EventStore::resource_receipts`] folds them the same
//! skip-an-undecodable-row way [`memory_entries`](crate::memory_entries) does.
//!
//! [`EventStore::resource_receipts`]: crate::EventStore::resource_receipts

use serde::{Deserialize, Serialize};

use flux_core::Usage;

/// The version stamped on every receipt this build writes.
///
/// A receipt is durable accounting evidence read back years later, so the schema it was written
/// under travels with it rather than being inferred from the reader's own build.
pub const RECEIPT_SCHEMA_VERSION: u32 = 1;

/// The maximum length, in characters, of any label a receipt persists.
///
/// Bounding is separate from redaction and does not replace it: redaction removes what is known to
/// be secret, bounding removes the *channel* — an unbounded operation label would let a caller
/// smuggle an arbitrary payload into the accounting ledger, which is exactly what a receipt must
/// never carry. An over-long label is truncated to this many characters **including** a trailing
/// `…`, so a reader can tell a bounded label from a short one.
pub const MAX_LABEL_LEN: usize = 120;

/// `EventKind::Custom` name for "a resource span was recorded". Payload: a [`ResourceReceipt`].
pub const RESOURCE_SPAN_RECORDED: &str = "resource.span_recorded";

/// The reserved `Custom` name prefix C-575 folds. An embedder writing its own app facts must stay
/// out of it, exactly as it must stay out of [`MEMORY_NAME_PREFIX`](crate::memory::MEMORY_NAME_PREFIX).
pub const RESOURCE_NAME_PREFIX: &str = "resource.";

// --- measurement catalogue ---------------------------------------------------------------------

/// The family a [`Dimension`] belongs to — the design's measurement table, one variant per row.
///
/// The family is what decides whether a backend can honestly meter a dimension at all
/// ([`SpanBackend::absence_for`]), so it is a property of the catalogue rather than a display
/// grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeasurementFamily {
    /// Provider/model calls and their token tiers. Honest source: the provider response, or a
    /// foreign harness's own usage record.
    Model,
    /// Host-owned runtime counters: wall time, loop iterations, dispatches, reports, retries.
    Runtime,
    /// Process accounting for a child flux itself owns: CPU, peak RSS, output bytes.
    Process,
    /// Timings and byte counts from the guarded transport flux egresses through.
    Network,
    /// Bytes a guarded tool or backend actually measured reading, writing or producing.
    Filesystem,
    /// Concurrency occupancy and queue time from the host's own census/semaphore.
    Capacity,
    /// Targeted checks, reviews and gate commands run by the host's process runner.
    Validation,
}

/// The unit a [`Dimension`] is counted in. Derived from the dimension — no constructor lets a
/// caller state a unit that disagrees with what it is measuring — and persisted anyway, so a reader
/// that does not know a newer dimension can still say "40 milliseconds".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// A plain occurrence count.
    Count,
    /// Provider-counted tokens.
    Tokens,
    /// Milliseconds of elapsed or consumed time.
    Milliseconds,
    /// Bytes.
    Bytes,
    /// Occupied-slot milliseconds (one slot held for one millisecond).
    SlotMilliseconds,
}

/// One measurable dimension. The catalogue is [`Dimension::CATALOGUE`]; every entry has a stable
/// wire name ([`Dimension::as_str`]), a family and a unit.
///
/// The wire name is a storage identifier baked into receipts already on disk — changing one
/// silently orphans every measurement written under it, so treat these strings the way
/// [`MemoryScope`](crate::MemoryScope)'s scope keys are treated.
///
/// **Several token tiers are subsets of others**, exactly as [`Usage`] documents:
/// `cache_creation_1h_input_tokens` ⊂ `cache_creation_input_tokens`, and `reasoning_tokens` /
/// `audio_output_tokens` ⊂ `output_tokens`, `audio_input_tokens` ⊂ `input_tokens`. They are
/// recorded separately so a cost model can price them apart; a rollup that adds every model
/// dimension together double-counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dimension {
    /// Provider calls made by this span.
    ModelCalls,
    /// Fresh (non-cached) input tokens.
    InputTokens,
    /// Input tokens served from the provider's prompt cache.
    CacheReadInputTokens,
    /// Input tokens written into the provider's prompt cache.
    CacheCreationInputTokens,
    /// The subset of cache-creation tokens written at the one-hour TTL (C-135).
    CacheCreation1hInputTokens,
    /// Generated output tokens.
    OutputTokens,
    /// The reasoning/"thinking" subset of the output tokens.
    ReasoningTokens,
    /// The audio subset of the fresh input tokens.
    AudioInputTokens,
    /// The audio subset of the output tokens.
    AudioOutputTokens,

    /// Elapsed wall time this span was open.
    WallTime,
    /// Agent-loop iterations executed.
    LoopIterations,
    /// Tool dispatches through the executor.
    ToolDispatches,
    /// Reports/handoffs produced.
    Reports,
    /// Retries and rework attempts.
    Retries,

    /// User CPU time of an owned child process.
    ProcessUserCpuTime,
    /// System CPU time of an owned child process.
    ProcessSystemCpuTime,
    /// Peak resident set size of an owned child process, when the OS reports one.
    ProcessPeakRss,
    /// Bytes an owned child process wrote to its captured output.
    ProcessOutputBytes,

    /// Requests issued through the guarded transport.
    NetworkRequests,
    /// DNS resolution time.
    NetworkDnsTime,
    /// TCP connect time.
    NetworkConnectTime,
    /// TLS handshake time.
    NetworkTlsTime,
    /// Time to first response byte.
    NetworkTimeToFirstByte,
    /// Body transfer time after the first byte.
    NetworkTransferTime,
    /// Bytes received.
    NetworkBytesIn,
    /// Bytes sent.
    NetworkBytesOut,

    /// Bytes read by a guarded tool.
    FileBytesRead,
    /// Bytes written by a guarded tool.
    FileBytesWritten,
    /// Bytes of durable artifact produced.
    ArtifactBytes,
    /// Bytes of diff produced.
    DiffBytes,

    /// Occupied-slot time against a concurrency limit.
    CapacityOccupancy,
    /// Time spent queued waiting for a slot.
    CapacityQueueTime,

    /// Targeted/review/gate commands run.
    ValidationCommands,
    /// Wall time spent in those commands.
    ValidationWallTime,
    /// CPU time consumed by those commands.
    ValidationCpuTime,
    /// Bytes of output those commands produced.
    ValidationOutputBytes,
}

impl Dimension {
    /// Every dimension flux can record today, in family order.
    ///
    /// The catalogue is the enumeration consumers iterate (a coverage report, a rollup, this
    /// crate's own conformance tests); a variant missing from it is invisible to all of them, so
    /// adding a variant means adding its row here.
    pub const CATALOGUE: [Dimension; 36] = [
        Dimension::ModelCalls,
        Dimension::InputTokens,
        Dimension::CacheReadInputTokens,
        Dimension::CacheCreationInputTokens,
        Dimension::CacheCreation1hInputTokens,
        Dimension::OutputTokens,
        Dimension::ReasoningTokens,
        Dimension::AudioInputTokens,
        Dimension::AudioOutputTokens,
        Dimension::WallTime,
        Dimension::LoopIterations,
        Dimension::ToolDispatches,
        Dimension::Reports,
        Dimension::Retries,
        Dimension::ProcessUserCpuTime,
        Dimension::ProcessSystemCpuTime,
        Dimension::ProcessPeakRss,
        Dimension::ProcessOutputBytes,
        Dimension::NetworkRequests,
        Dimension::NetworkDnsTime,
        Dimension::NetworkConnectTime,
        Dimension::NetworkTlsTime,
        Dimension::NetworkTimeToFirstByte,
        Dimension::NetworkTransferTime,
        Dimension::NetworkBytesIn,
        Dimension::NetworkBytesOut,
        Dimension::FileBytesRead,
        Dimension::FileBytesWritten,
        Dimension::ArtifactBytes,
        Dimension::DiffBytes,
        Dimension::CapacityOccupancy,
        Dimension::CapacityQueueTime,
        Dimension::ValidationCommands,
        Dimension::ValidationWallTime,
        Dimension::ValidationCpuTime,
        Dimension::ValidationOutputBytes,
    ];

    /// The model-call tiers, in the order [`ResourceSpan::measure_model_call`] records them.
    ///
    /// Kept next to that method rather than derived from a `family()` filter so the two cannot
    /// drift: every entry here is one field of [`Usage`], and a model call states all of them.
    const MODEL_TIERS: [Dimension; 8] = [
        Dimension::InputTokens,
        Dimension::CacheReadInputTokens,
        Dimension::CacheCreationInputTokens,
        Dimension::CacheCreation1hInputTokens,
        Dimension::OutputTokens,
        Dimension::ReasoningTokens,
        Dimension::AudioInputTokens,
        Dimension::AudioOutputTokens,
    ];

    /// The stable wire name written into every persisted measurement.
    pub fn as_str(&self) -> &'static str {
        match self {
            Dimension::ModelCalls => "model.calls",
            Dimension::InputTokens => "model.input_tokens",
            Dimension::CacheReadInputTokens => "model.cache_read_input_tokens",
            Dimension::CacheCreationInputTokens => "model.cache_creation_input_tokens",
            Dimension::CacheCreation1hInputTokens => "model.cache_creation_1h_input_tokens",
            Dimension::OutputTokens => "model.output_tokens",
            Dimension::ReasoningTokens => "model.reasoning_tokens",
            Dimension::AudioInputTokens => "model.audio_input_tokens",
            Dimension::AudioOutputTokens => "model.audio_output_tokens",
            Dimension::WallTime => "runtime.wall_time_ms",
            Dimension::LoopIterations => "runtime.loop_iterations",
            Dimension::ToolDispatches => "runtime.tool_dispatches",
            Dimension::Reports => "runtime.reports",
            Dimension::Retries => "runtime.retries",
            Dimension::ProcessUserCpuTime => "process.user_cpu_time_ms",
            Dimension::ProcessSystemCpuTime => "process.system_cpu_time_ms",
            Dimension::ProcessPeakRss => "process.peak_rss_bytes",
            Dimension::ProcessOutputBytes => "process.output_bytes",
            Dimension::NetworkRequests => "network.requests",
            Dimension::NetworkDnsTime => "network.dns_time_ms",
            Dimension::NetworkConnectTime => "network.connect_time_ms",
            Dimension::NetworkTlsTime => "network.tls_time_ms",
            Dimension::NetworkTimeToFirstByte => "network.time_to_first_byte_ms",
            Dimension::NetworkTransferTime => "network.transfer_time_ms",
            Dimension::NetworkBytesIn => "network.bytes_in",
            Dimension::NetworkBytesOut => "network.bytes_out",
            Dimension::FileBytesRead => "filesystem.bytes_read",
            Dimension::FileBytesWritten => "filesystem.bytes_written",
            Dimension::ArtifactBytes => "filesystem.artifact_bytes",
            Dimension::DiffBytes => "filesystem.diff_bytes",
            Dimension::CapacityOccupancy => "capacity.occupancy_slot_ms",
            Dimension::CapacityQueueTime => "capacity.queue_time_ms",
            Dimension::ValidationCommands => "validation.commands",
            Dimension::ValidationWallTime => "validation.wall_time_ms",
            Dimension::ValidationCpuTime => "validation.cpu_time_ms",
            Dimension::ValidationOutputBytes => "validation.output_bytes",
        }
    }

    /// The dimension a persisted wire name names, or `None` for one this build does not know.
    pub fn from_wire(wire: &str) -> Option<Dimension> {
        Dimension::CATALOGUE
            .into_iter()
            .find(|dimension| dimension.as_str() == wire)
    }

    /// Which family this dimension belongs to.
    pub fn family(&self) -> MeasurementFamily {
        match self {
            Dimension::ModelCalls
            | Dimension::InputTokens
            | Dimension::CacheReadInputTokens
            | Dimension::CacheCreationInputTokens
            | Dimension::CacheCreation1hInputTokens
            | Dimension::OutputTokens
            | Dimension::ReasoningTokens
            | Dimension::AudioInputTokens
            | Dimension::AudioOutputTokens => MeasurementFamily::Model,
            Dimension::WallTime
            | Dimension::LoopIterations
            | Dimension::ToolDispatches
            | Dimension::Reports
            | Dimension::Retries => MeasurementFamily::Runtime,
            Dimension::ProcessUserCpuTime
            | Dimension::ProcessSystemCpuTime
            | Dimension::ProcessPeakRss
            | Dimension::ProcessOutputBytes => MeasurementFamily::Process,
            Dimension::NetworkRequests
            | Dimension::NetworkDnsTime
            | Dimension::NetworkConnectTime
            | Dimension::NetworkTlsTime
            | Dimension::NetworkTimeToFirstByte
            | Dimension::NetworkTransferTime
            | Dimension::NetworkBytesIn
            | Dimension::NetworkBytesOut => MeasurementFamily::Network,
            Dimension::FileBytesRead
            | Dimension::FileBytesWritten
            | Dimension::ArtifactBytes
            | Dimension::DiffBytes => MeasurementFamily::Filesystem,
            Dimension::CapacityOccupancy | Dimension::CapacityQueueTime => {
                MeasurementFamily::Capacity
            }
            Dimension::ValidationCommands
            | Dimension::ValidationWallTime
            | Dimension::ValidationCpuTime
            | Dimension::ValidationOutputBytes => MeasurementFamily::Validation,
        }
    }

    /// The unit this dimension is counted in.
    pub fn unit(&self) -> Unit {
        match self {
            Dimension::ModelCalls
            | Dimension::LoopIterations
            | Dimension::ToolDispatches
            | Dimension::Reports
            | Dimension::Retries
            | Dimension::NetworkRequests
            | Dimension::ValidationCommands => Unit::Count,
            Dimension::InputTokens
            | Dimension::CacheReadInputTokens
            | Dimension::CacheCreationInputTokens
            | Dimension::CacheCreation1hInputTokens
            | Dimension::OutputTokens
            | Dimension::ReasoningTokens
            | Dimension::AudioInputTokens
            | Dimension::AudioOutputTokens => Unit::Tokens,
            Dimension::WallTime
            | Dimension::ProcessUserCpuTime
            | Dimension::ProcessSystemCpuTime
            | Dimension::NetworkDnsTime
            | Dimension::NetworkConnectTime
            | Dimension::NetworkTlsTime
            | Dimension::NetworkTimeToFirstByte
            | Dimension::NetworkTransferTime
            | Dimension::CapacityQueueTime
            | Dimension::ValidationWallTime
            | Dimension::ValidationCpuTime => Unit::Milliseconds,
            Dimension::ProcessPeakRss
            | Dimension::ProcessOutputBytes
            | Dimension::NetworkBytesIn
            | Dimension::NetworkBytesOut
            | Dimension::FileBytesRead
            | Dimension::FileBytesWritten
            | Dimension::ArtifactBytes
            | Dimension::DiffBytes
            | Dimension::ValidationOutputBytes => Unit::Bytes,
            Dimension::CapacityOccupancy => Unit::SlotMilliseconds,
        }
    }
}

// Hand-written so the persisted name has exactly one definition — `as_str`. A `#[serde(rename)]`
// per variant would be a second copy of 36 strings, free to drift from the first.
impl Serialize for Dimension {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Dimension {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = String::deserialize(deserializer)?;
        Dimension::from_wire(&wire)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown resource dimension {wire:?}")))
    }
}

/// Why a dimension carries no number.
///
/// The distinction is the whole point of the type: all three read very differently in a bill, and
/// none of them is `0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Absence {
    /// This backend structurally cannot have this dimension — an in-process library owns no child
    /// process, so its CPU time is not a number that was missed, it is a number that cannot exist.
    Unsupported,
    /// The resource was plausibly used, and nobody told us how much. A foreign harness's own CPU
    /// and traffic are real; they simply never crossed a meter of ours.
    NotReported,
    /// A real, metered quantity that cannot honestly be attributed to *this* span — shared or
    /// truncated work, e.g. a cancelled child whose consumption belongs to no single caller.
    NotAttributable,
}

/// A measured number, or a typed reason there is none. Never a zero standing in for absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "snake_case")]
pub enum MeasuredValue {
    /// A real measurement, in the dimension's [`Unit`].
    Observed(u64),
    /// No measurement, and why.
    Absent(Absence),
}

/// Where a number came from — the design's "honest source" column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementSource {
    /// The provider's own usage response for the call.
    ProviderReported,
    /// A foreign harness's own usage record, read back by flux.
    HarnessRecord,
    /// The host's monotonic clock.
    HostClock,
    /// A host-owned counter (iterations, dispatches, retries).
    HostCounter,
    /// OS/container accounting for a process flux owns.
    OsAccounting,
    /// The instrumented guarded transport all flux egress crosses.
    GuardedTransport,
    /// A guarded tool or backend that measured the bytes it actually moved.
    GuardedTool,
    /// The host's own concurrency census/semaphore.
    HostCensus,
}

/// One dimension of one span: what was measured, in which unit, from which source.
///
/// There is no public field-assignment path — [`Measurement::observed`] and [`Measurement::absent`]
/// are the only constructors — so `unit` always agrees with `dimension`, and an absent measurement
/// can never claim a source that measured nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Measurement {
    /// What is being measured.
    pub dimension: Dimension,
    /// The number, or the typed reason there is none.
    pub value: MeasuredValue,
    /// The dimension's unit, persisted so the record is self-describing.
    pub unit: Unit,
    /// Where the number came from. `None` exactly when `value` is [`MeasuredValue::Absent`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<MeasurementSource>,
}

impl Measurement {
    /// A real measurement of `dimension` from `source`.
    pub fn observed(dimension: Dimension, value: u64, source: MeasurementSource) -> Self {
        Self {
            dimension,
            value: MeasuredValue::Observed(value),
            unit: dimension.unit(),
            source: Some(source),
        }
    }

    /// A typed absence for `dimension` — the honest record when nobody metered it.
    pub fn absent(dimension: Dimension, absence: Absence) -> Self {
        Self {
            dimension,
            value: MeasuredValue::Absent(absence),
            unit: dimension.unit(),
            source: None,
        }
    }

    /// This measurement restated as a typed absence, dropping the source along with the number.
    ///
    /// Used when a backend cannot honestly own the dimension: keeping the claimed source next to a
    /// discarded number would leave the receipt asserting that OS accounting reported something it
    /// never saw.
    fn into_absence(self, absence: Absence) -> Self {
        Measurement::absent(self.dimension, absence)
    }
}

// --- money -------------------------------------------------------------------------------------

/// On what authority an amount is claimed. The truth-carrier of a [`MoneyCharge`]: a reader trusts
/// this field, not which constructor produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChargeBasis {
    /// The provider's own figure for this call — the only basis that is a *bill*.
    ProviderReported,
    /// Derived from a versioned pricing table.
    PricingTable,
    /// What this work would have cost outside a subscription.
    SubscriptionEquivalent,
    /// An operator-supplied rate for a physical resource (CPU, storage, egress).
    OperatorRate,
}

impl ChargeBasis {
    /// `true` for the one basis that is a provider's own billed figure rather than an estimate.
    pub fn is_provider_reported(&self) -> bool {
        matches!(self, ChargeBasis::ProviderReported)
    }
}

/// How much of the span's usage an amount actually prices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceCoverage {
    /// Every measured dimension this charge claims to cover is priced.
    Complete,
    /// Some of it is priced and some is not — the amount is a floor, not a total.
    Partial,
    /// Nothing is priced. Present so "we know the usage and there is no price" is expressible
    /// without inventing a `0.0`.
    Unpriced,
}

/// A monetary claim about a span, kept strictly separate from what was physically measured.
///
/// Tokens stay visible even when no price exists; CPU, network, storage and process time become
/// money only when an operator supplies a versioned rate or a backend reports a real charge. An
/// unpriced dimension carries **no charge at all** rather than a `$0` one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoneyCharge {
    /// The amount, in `currency`.
    pub amount: f64,
    /// ISO-4217-style currency code, bounded like any other label.
    pub currency: String,
    /// On what authority the amount is claimed.
    pub basis: ChargeBasis,
    /// The rate table/version an estimate was derived from, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_version: Option<String>,
    /// When that rate took effect, unix milliseconds — so a later repricing is visibly a different
    /// rate rather than a rewrite of this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at_ms: Option<i64>,
    /// How much of the span's usage this amount prices.
    pub coverage: PriceCoverage,
    /// Receipts this amount was derived from, when it aggregates others.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_receipt_ids: Vec<String>,
}

impl MoneyCharge {
    /// The provider's own figure for this call: [`ChargeBasis::ProviderReported`], complete.
    ///
    /// This is the only constructor that mints a billed basis, and it takes no basis argument
    /// precisely so it cannot mint anything else.
    pub fn reported(amount: f64, currency: &str) -> Self {
        Self {
            amount,
            currency: bounded_label(currency),
            basis: ChargeBasis::ProviderReported,
            rate_version: None,
            effective_at_ms: None,
            coverage: PriceCoverage::Complete,
            source_receipt_ids: Vec::new(),
        }
    }

    /// A computed amount on a stated `basis` — a pricing table, a subscription equivalent, an
    /// operator rate.
    ///
    /// Defaults to [`PriceCoverage::Complete`] for what it claims to cover; narrow it with
    /// [`with_coverage`](Self::with_coverage) when it prices only part of the span. Passing
    /// [`ChargeBasis::ProviderReported`] here produces exactly what [`reported`](Self::reported)
    /// would — the basis field is what a reader trusts, so no lie is created either way, but
    /// `reported` is the call that says what it means.
    pub fn estimated(amount: f64, currency: &str, basis: ChargeBasis) -> Self {
        Self {
            basis,
            ..Self::reported(amount, currency)
        }
    }

    /// Name the rate table/version this amount came from.
    pub fn with_rate_version(mut self, version: &str) -> Self {
        self.rate_version = Some(bounded_label(version));
        self
    }

    /// When the rate took effect, unix milliseconds.
    pub fn effective_at(mut self, effective_at_ms: i64) -> Self {
        self.effective_at_ms = Some(effective_at_ms);
        self
    }

    /// State how much of the span's usage this amount prices.
    pub fn with_coverage(mut self, coverage: PriceCoverage) -> Self {
        self.coverage = coverage;
        self
    }

    /// Name the receipts this amount was derived from.
    pub fn from_receipts(mut self, receipt_ids: Vec<String>) -> Self {
        self.source_receipt_ids = receipt_ids;
        self
    }
}

// --- span identity, timing and binding -----------------------------------------------------------

/// What kind of thing did the work, and therefore what it can honestly meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanBackend {
    /// Work done inside this process — a library call, a tool, the agent loop itself.
    InProcess,
    /// A child process flux spawned and owns, so its OS accounting is flux's to read.
    OwnedChild,
    /// A remote service flux called: a provider endpoint, an execution host.
    Remote,
    /// A foreign harness whose usage flux reads back from its own records.
    Foreign,
}

/// The precision the timestamps on a span were taken at — a millisecond clock and a second-
/// granularity one produce very different confidence in a short span's duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockPrecision {
    /// Whole seconds.
    Seconds,
    /// Milliseconds.
    Milliseconds,
    /// Microseconds.
    Microseconds,
}

/// When a span ran, and how precisely that is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanTiming {
    /// Start, unix milliseconds.
    pub start_ms: i64,
    /// End, unix milliseconds.
    pub end_ms: i64,
    /// The precision the clock behind those stamps offers.
    pub precision: ClockPrecision,
}

impl SpanTiming {
    /// A span that ran from `start_ms` to `end_ms` on a clock of `precision`.
    pub fn new(start_ms: i64, end_ms: i64, precision: ClockPrecision) -> Self {
        Self {
            start_ms,
            end_ms,
            precision,
        }
    }

    /// Elapsed milliseconds, floored at zero — a clock that went backwards is not negative work.
    pub fn duration_ms(&self) -> i64 {
        (self.end_ms - self.start_ms).max(0)
    }
}

/// Who the work was done for. Every field is a host-minted identity, never model-authored and
/// never truncated — see this module's header on the identity/label split.
///
/// A Fleet admission binds `board_ref` and `assignment_revision` alongside `worker` and `wave`, so
/// the writer, its nested tasks, the reviewer, the rework pass and its targeted checks all stay
/// causally attached to the same story rather than to whatever else was running at the time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausalBinding {
    /// The agent role that ran the work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The session (event stream) it ran in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The Fleet worker seat.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// The Fleet wave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wave: Option<String>,
    /// The repository the work was done in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// The board item this work was admitted against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_ref: Option<String>,
    /// The assignment revision that admitted it — so changing the board later does not rewrite an
    /// old bill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignment_revision: Option<String>,
}

/// How complete a span's own measurement is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Every dimension this span owns was measured.
    Complete,
    /// Some were measured and some were not — a cancelled span keeps what it got.
    Partial,
    /// Nothing was measured; the span records only that the work happened.
    Unmetered,
}

/// How current a receipt's numbers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    /// Recorded as the work happened.
    Live,
    /// Recorded later, from a source that became available after the fact — a provider's settled
    /// usage record, a harness log read on the next run.
    Backfilled,
}

/// One request/result: the causal root every span under it inherits.
///
/// The root owns its own event stream (`resource:<root-id>`), which is what makes a per-root read
/// a stream scan rather than a store-wide filter, and what keeps two concurrent workers' receipts
/// from interleaving into one another's ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRoot {
    root_id: String,
}

impl ResourceRoot {
    /// The prefix every resource-receipt stream carries.
    pub const STREAM_PREFIX: &'static str = "resource:";

    /// The prefix every derived receipt id carries, so a receipt id can never be mistaken for — or
    /// collide with — a store-minted ULID in the log's `UNIQUE(id)` space.
    const RECEIPT_ID_PREFIX: &'static str = "receipt:";

    /// The root for request/result `root_id`.
    pub fn new(root_id: impl Into<String>) -> Self {
        Self {
            root_id: root_id.into(),
        }
    }

    /// The request/result id this root names.
    pub fn root_id(&self) -> &str {
        &self.root_id
    }

    /// The event stream this root's receipts live on.
    pub fn stream(&self) -> String {
        format!("{}{}", Self::STREAM_PREFIX, self.root_id)
    }

    /// The receipt id for `span_id` under this root — **derived, not minted**.
    ///
    /// This is what makes an at-least-once pipeline safe: the id is a pure function of (root,
    /// span), so replaying a span produces the same id, and the store's caller-id idempotency
    /// returns the receipt already recorded instead of appending a contradictory second row.
    ///
    /// The encoding doubles `#` inside the root id before joining on a single `#`, which makes it
    /// injective over (root, span). That matters more than it looks: event ids are `UNIQUE(id)`
    /// **store-wide**, so a naive join would let two different roots derive one id, and the loser's
    /// span would silently return the winner's receipt — a provenance failure that still resolves,
    /// which is the worst kind.
    pub fn receipt_id(&self, span_id: &str) -> String {
        format!(
            "{}{}#{}",
            Self::RECEIPT_ID_PREFIX,
            self.root_id.replace('#', "##"),
            span_id
        )
    }
}

impl SpanBackend {
    /// The typed absence this backend must record for `family`, or `None` when it can honestly
    /// meter it.
    ///
    /// This is the table behind "an in-process or foreign backend never emits zero CPU/RSS/network
    /// merely because it lacks an honest meter". It is consulted by [`ResourceSpan::measure`], so
    /// the rule holds at the builder rather than depending on every call site to remember it — and
    /// it discards an offered number whatever its value, not just a zero, because a backend that
    /// cannot own a dimension has no more standing to report `90` than to report `0`.
    ///
    /// The reason differs by backend, and the difference is the point: an in-process library's
    /// child-process CPU is [`Absence::Unsupported`] (there is no such process, so there is no
    /// number to miss), while a remote or foreign backend's is [`Absence::NotReported`] (the work
    /// really did burn CPU somewhere; nobody told us how much).
    pub fn absence_for(&self, family: MeasurementFamily) -> Option<Absence> {
        match (self, family) {
            // Nothing this process spawned, so no process accounting exists to attribute.
            (SpanBackend::InProcess, MeasurementFamily::Process) => Some(Absence::Unsupported),
            // A child flux owns is the one backend whose OS accounting is genuinely flux's to read,
            // and every other family is measured on this side of it.
            (SpanBackend::OwnedChild, _) => None,
            // The remote side really consumed CPU; its accounting is simply not ours.
            (SpanBackend::Remote, MeasurementFamily::Process) => Some(Absence::NotReported),
            // A foreign harness reports only what its conformance contract measures: its own model
            // usage, and the wall time we observed from outside it. Its traffic never crossed our
            // guarded transport and its files never crossed our guarded tools.
            (
                SpanBackend::Foreign,
                MeasurementFamily::Process
                | MeasurementFamily::Network
                | MeasurementFamily::Filesystem
                | MeasurementFamily::Capacity
                | MeasurementFamily::Validation,
            ) => Some(Absence::NotReported),
            _ => None,
        }
    }

    /// The honest source for a model-call tier recorded against this backend.
    fn model_usage_source(&self) -> MeasurementSource {
        match self {
            SpanBackend::Foreign => MeasurementSource::HarnessRecord,
            _ => MeasurementSource::ProviderReported,
        }
    }
}

/// The write shape: one span of work, its causal position, and what was measured of it.
///
/// Every field is private with exactly one constructor, mirroring
/// [`MemoryNote`](crate::MemoryNote): it makes "scrub and bound before the store sees it" and
/// "typed absence for what this backend cannot meter" properties of the type rather than
/// conventions a future call site can forget.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSpan {
    span_id: String,
    parent_span_id: Option<String>,
    backend: SpanBackend,
    operation: String,
    phase: Option<String>,
    loop_binding: Option<String>,
    timing: SpanTiming,
    binding: CausalBinding,
    measurements: Vec<Measurement>,
    charges: Vec<MoneyCharge>,
    coverage: Coverage,
    freshness: Freshness,
    correction_of: Option<String>,
}

impl ResourceSpan {
    /// A span identified by `span_id` within its root, run by `backend`, describing `operation`.
    ///
    /// `redact` is the **live turn's [`flux_secret::Redactor::redact`]** — the same scrubber
    /// `flux-flow`'s evidence flush applies (C-22/C-164), seeded with every credential the run
    /// materialized. It is passed in rather than reached for because redaction is a caller
    /// responsibility everywhere in this crate; the operation label is scrubbed through it and then
    /// bounded to [`MAX_LABEL_LEN`] before anything is stored.
    ///
    /// `span_id` is an identity, not a label: it is host-minted, is neither scrubbed nor truncated,
    /// and must match whatever a child passes to [`under`](Self::under).
    ///
    /// [`flux_secret::Redactor::redact`]: https://docs.rs/codewandler-flux-secret
    pub fn new(
        span_id: impl Into<String>,
        backend: SpanBackend,
        operation: &str,
        timing: SpanTiming,
        redact: impl Fn(&str) -> String,
    ) -> Self {
        Self {
            span_id: span_id.into(),
            parent_span_id: None,
            backend,
            operation: bounded_label(&redact(operation)),
            phase: None,
            loop_binding: None,
            timing,
            binding: CausalBinding::default(),
            measurements: Vec::new(),
            charges: Vec::new(),
            coverage: Coverage::Complete,
            freshness: Freshness::Live,
            correction_of: None,
        }
    }

    /// Attach this span to its causal parent — the span that *caused* the work, which is not
    /// necessarily the one running at the same time.
    pub fn under(mut self, parent_span_id: impl Into<String>) -> Self {
        self.parent_span_id = Some(parent_span_id.into());
        self
    }

    /// Bind this span to the agent/session/worker/wave/repository and board item it was admitted
    /// under.
    pub fn bind(mut self, binding: CausalBinding) -> Self {
        self.binding = binding;
        self
    }

    /// The agent-loop phase this span ran in (`"orient"`, `"gather"`, `"execute"`), scrubbed and
    /// bounded like any other label.
    pub fn with_phase(mut self, phase: &str, redact: impl Fn(&str) -> String) -> Self {
        self.phase = Some(bounded_label(&redact(phase)));
        self
    }

    /// The authored loop binding this span ran under, scrubbed and bounded like any other label.
    pub fn with_loop_binding(
        mut self,
        loop_binding: &str,
        redact: impl Fn(&str) -> String,
    ) -> Self {
        self.loop_binding = Some(bounded_label(&redact(loop_binding)));
        self
    }

    /// State that this span measured only part of what it owns — a cancelled or truncated run.
    pub fn with_coverage(mut self, coverage: Coverage) -> Self {
        self.coverage = coverage;
        self
    }

    /// State that these numbers were recorded after the fact rather than live.
    pub fn with_freshness(mut self, freshness: Freshness) -> Self {
        self.freshness = freshness;
        self
    }

    /// Mark this span as a correction of an already-recorded receipt.
    ///
    /// A correction is an **append** naming its original, never an edit of it: the measured
    /// original stays in the log exactly as recorded, so durable history stays auditable and a
    /// later repricing can never make a past bill claim it was billed at today's rate.
    pub fn correcting(mut self, receipt_id: &str) -> Self {
        self.correction_of = Some(receipt_id.to_string());
        self
    }

    /// Record one measurement, replacing any earlier one for the same dimension.
    ///
    /// A number offered for a family this backend cannot honestly meter is replaced here by the
    /// typed absence [`SpanBackend::absence_for`] names — the value never reaches the ledger. An
    /// explicitly stated [`Measurement::absent`] is kept verbatim: the caller knows which of
    /// `unsupported`/`not_reported`/`not_attributable` applies better than a per-family table can.
    pub fn measure(mut self, measurement: Measurement) -> Self {
        let measurement = match (
            &measurement.value,
            self.backend.absence_for(measurement.dimension.family()),
        ) {
            (MeasuredValue::Observed(_), Some(absence)) => measurement.into_absence(absence),
            _ => measurement,
        };
        match self
            .measurements
            .iter_mut()
            .find(|existing| existing.dimension == measurement.dimension)
        {
            Some(slot) => *slot = measurement,
            None => self.measurements.push(measurement),
        }
        self
    }

    /// Record one provider call: the call itself plus **every** token tier [`Usage`] carries.
    ///
    /// All tiers are stated, because a tier missing from a receipt is not the same fact as a tier
    /// reported at zero, and only the record itself can tell them apart. `Usage` is the provider's
    /// (or a foreign harness's) own numeric record, so what it carries is what was reported —
    /// including its zeros. A backend that genuinely does *not* report a tier states that with
    /// [`Measurement::absent`] and [`Absence::NotReported`] after this call, which overwrites the
    /// tier for that dimension.
    ///
    /// Deliberately records **no** [`MoneyCharge`], even when `usage.reported_cost_usd` is set:
    /// money is a separate claim with its own basis and coverage, and minting one implicitly here
    /// would double it against the caller's own [`charge`](Self::charge).
    pub fn measure_model_call(mut self, usage: &Usage) -> Self {
        let source = self.backend.model_usage_source();
        self = self.measure(Measurement::observed(
            Dimension::ModelCalls,
            1,
            MeasurementSource::HostCounter,
        ));
        for dimension in Dimension::MODEL_TIERS {
            let value = match dimension {
                Dimension::InputTokens => usage.input_tokens,
                Dimension::CacheReadInputTokens => usage.cache_read_input_tokens,
                Dimension::CacheCreationInputTokens => usage.cache_creation_input_tokens,
                Dimension::CacheCreation1hInputTokens => usage.cache_creation_1h_input_tokens,
                Dimension::OutputTokens => usage.output_tokens,
                Dimension::ReasoningTokens => usage.reasoning_tokens,
                Dimension::AudioInputTokens => usage.audio_input_tokens,
                Dimension::AudioOutputTokens => usage.audio_output_tokens,
                // `MODEL_TIERS` is closed and every entry is handled above; a new tier that reaches
                // here has no `Usage` field to read and must be added to both.
                other => unreachable!("model tier {other:?} has no Usage field"),
            };
            self = self.measure(Measurement::observed(dimension, value, source));
        }
        self
    }

    /// Attach a monetary claim to this span.
    pub fn charge(mut self, charge: MoneyCharge) -> Self {
        self.charges.push(charge);
        self
    }

    /// Seal this span into the receipt that gets persisted, stamping the schema version, the root
    /// it belongs to and its derived receipt id.
    pub(crate) fn into_receipt(self, root: &ResourceRoot) -> ResourceReceipt {
        ResourceReceipt {
            receipt_id: root.receipt_id(&self.span_id),
            schema_version: RECEIPT_SCHEMA_VERSION,
            root_id: root.root_id().to_string(),
            span_id: self.span_id,
            parent_span_id: self.parent_span_id,
            backend: self.backend,
            operation: self.operation,
            phase: self.phase,
            loop_binding: self.loop_binding,
            timing: self.timing,
            binding: self.binding,
            measurements: self.measurements,
            charges: self.charges,
            coverage: self.coverage,
            freshness: self.freshness,
            correction_of: self.correction_of,
        }
    }
}

/// One immutable, append-only resource receipt — the ledger's row and the payload of a
/// `resource.span_recorded` event.
///
/// Read back through [`EventStore::resource_receipts`](crate::EventStore::resource_receipts). What
/// it holds is counts, timings, ids and bounded labels; there is deliberately no field on it that
/// a prompt, an answer, a tool argument, command output or a network payload could occupy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceReceipt {
    /// Derived from (root, span) — see [`ResourceRoot::receipt_id`]. Also the log event's id, so
    /// there is one identity rather than two to reconcile.
    pub receipt_id: String,
    /// The schema this receipt was written under.
    pub schema_version: u32,
    /// The request/result this span belongs to.
    pub root_id: String,
    /// This span's id within the root.
    pub span_id: String,
    /// The span that caused this work; `None` for the root span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    /// What kind of thing did the work.
    pub backend: SpanBackend,
    /// A bounded, scrubbed label for the operation (`"model.call"`, `"web.fetch"`, `"tool.read"`).
    pub operation: String,
    /// The agent-loop phase, when the span ran inside one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    /// The authored loop binding the span ran under, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_binding: Option<String>,
    /// When it ran, and how precisely that is known.
    pub timing: SpanTiming,
    /// Who the work was done for.
    #[serde(default)]
    pub binding: CausalBinding,
    /// What was measured, one entry per dimension.
    pub measurements: Vec<Measurement>,
    /// Monetary claims about this span, each with its own basis and coverage.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub charges: Vec<MoneyCharge>,
    /// How complete this span's own measurement is.
    pub coverage: Coverage,
    /// How current its numbers are.
    pub freshness: Freshness,
    /// The receipt this one corrects, when it is a correction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correction_of: Option<String>,
}

impl ResourceReceipt {
    /// This receipt's entry for `dimension`, or `None` when the span said nothing about it.
    ///
    /// `None` and `Some(Absent(..))` are different answers: the first is a dimension the span never
    /// addressed, the second is one it addressed by saying nobody metered it.
    pub fn measurement(&self, dimension: Dimension) -> Option<&Measurement> {
        self.measurements
            .iter()
            .find(|measurement| measurement.dimension == dimension)
    }

    /// The measured value for `dimension`, when there is a number.
    pub fn observed(&self, dimension: Dimension) -> Option<u64> {
        match self.measurement(dimension)?.value {
            MeasuredValue::Observed(value) => Some(value),
            MeasuredValue::Absent(_) => None,
        }
    }
}

/// One node of a causal span tree: a receipt and the receipts it caused.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanNode<'a> {
    /// This node's receipt.
    pub receipt: &'a ResourceReceipt,
    /// Spans that named this one as their parent, ordered by start time.
    pub children: Vec<SpanNode<'a>>,
}

/// Assemble `receipts` into causal trees by their explicit parent links, roots first, children
/// ordered by start time.
///
/// Every receipt appears exactly once, and never more than once — which is the property a rollup
/// depends on to avoid double-counting. Two cases that would otherwise lose or duplicate a row are
/// handled deliberately: a span whose named parent is **not in the slice** becomes a root of its
/// own (an orphan is a partial read, not a receipt to drop, and dropping it would understate the
/// bill), and a parent cycle — only reachable from corrupt or hand-written links — is broken by
/// promoting its first member to a root rather than recursing forever.
pub fn span_tree(receipts: &[ResourceReceipt]) -> Vec<SpanNode<'_>> {
    let mut position: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (index, receipt) in receipts.iter().enumerate() {
        position.entry(receipt.span_id.as_str()).or_insert(index);
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); receipts.len()];
    let mut roots: Vec<usize> = Vec::new();
    for (index, receipt) in receipts.iter().enumerate() {
        match receipt
            .parent_span_id
            .as_deref()
            .and_then(|parent| position.get(parent).copied())
        {
            Some(parent) if parent != index => children[parent].push(index),
            _ => roots.push(index),
        }
    }

    let by_start = |receipts: &[ResourceReceipt], indices: &mut Vec<usize>| {
        indices.sort_by(|a, b| {
            receipts[*a]
                .timing
                .start_ms
                .cmp(&receipts[*b].timing.start_ms)
                .then_with(|| receipts[*a].span_id.cmp(&receipts[*b].span_id))
        });
    };
    for list in &mut children {
        by_start(receipts, list);
    }
    by_start(receipts, &mut roots);

    let mut visited = vec![false; receipts.len()];
    let mut tree: Vec<SpanNode<'_>> = roots
        .into_iter()
        .map(|index| build_node(index, receipts, &children, &mut visited))
        .collect();
    // Anything still unvisited sits in a parent cycle: promote it rather than lose it.
    for index in 0..receipts.len() {
        if !visited[index] {
            tree.push(build_node(index, receipts, &children, &mut visited));
        }
    }
    tree
}

fn build_node<'a>(
    index: usize,
    receipts: &'a [ResourceReceipt],
    children: &[Vec<usize>],
    visited: &mut Vec<bool>,
) -> SpanNode<'a> {
    visited[index] = true;
    let mut kids = Vec::new();
    for child in &children[index] {
        if !visited[*child] {
            kids.push(build_node(*child, receipts, children, visited));
        }
    }
    SpanNode {
        receipt: &receipts[index],
        children: kids,
    }
}

/// Truncate `label` to [`MAX_LABEL_LEN`] characters, marking a cut one with a trailing `…`.
///
/// Character-counted rather than byte-sliced: a byte slice through a multi-byte character panics,
/// and a label is arbitrary text.
fn bounded_label(label: &str) -> String {
    if label.chars().count() <= MAX_LABEL_LEN {
        return label.to_string();
    }
    let mut out: String = label.chars().take(MAX_LABEL_LEN - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(raw: &str) -> String {
        raw.to_string()
    }

    fn at(start_ms: i64, end_ms: i64) -> SpanTiming {
        SpanTiming::new(start_ms, end_ms, ClockPrecision::Milliseconds)
    }

    /// The wire names are storage identifiers: a receipt written today must decode into the same
    /// dimension years from now, and no two dimensions may claim one name.
    #[test]
    fn every_dimension_round_trips_through_its_wire_name() {
        for dimension in Dimension::CATALOGUE {
            let json = serde_json::to_string(&dimension).unwrap();
            assert_eq!(json, format!("\"{}\"", dimension.as_str()));
            assert_eq!(
                serde_json::from_str::<Dimension>(&json).unwrap(),
                dimension,
                "{dimension:?} did not survive its own wire name"
            );
            assert_eq!(Dimension::from_wire(dimension.as_str()), Some(dimension));
        }
        assert_eq!(Dimension::from_wire("model.no_such_tier"), None);
    }

    /// A dimension a newer build introduced must fail loudly on decode rather than silently
    /// becoming some other dimension — a mis-decoded measurement is a wrong bill.
    #[test]
    fn an_unknown_dimension_fails_to_decode_rather_than_guessing() {
        let err = serde_json::from_str::<Dimension>("\"network.quantum_entanglement_ms\"")
            .expect_err("an unknown dimension must not decode");
        assert!(err.to_string().contains("unknown resource dimension"));
    }

    /// The receipt id is a pure function of (root, span) — that is what makes an at-least-once
    /// pipeline idempotent — and it is injective, so two roots can never derive one id in the
    /// log's store-wide `UNIQUE(id)` space.
    #[test]
    fn receipt_ids_are_derived_and_injective() {
        let root = ResourceRoot::new("req-1");
        assert_eq!(root.receipt_id("turn"), root.receipt_id("turn"));
        assert_ne!(root.receipt_id("turn"), root.receipt_id("turn/call-1"));
        assert_ne!(
            root.receipt_id("call"),
            ResourceRoot::new("req-2").receipt_id("call")
        );
        // The pathological pair a naive `root + "#" + span` join would collide.
        assert_ne!(
            ResourceRoot::new("a#b").receipt_id("c"),
            ResourceRoot::new("a").receipt_id("b#c")
        );
    }

    /// The Fleet path the story names: one writer, its review and its rework all hang off the same
    /// admission, so a rework's cost lands on the story that caused it rather than on the clock.
    #[test]
    fn a_fleet_writer_review_and_rework_share_one_admission() {
        let admission = CausalBinding {
            agent_id: Some("coding".to_string()),
            session: Some("s_9".to_string()),
            worker: Some("wave-745/flux/C-575".to_string()),
            wave: Some("wave-745".to_string()),
            repository: Some("flux".to_string()),
            board_ref: Some("flux/C-575".to_string()),
            assignment_revision: Some("rev-3".to_string()),
        };
        let root = ResourceRoot::new("admission-1");

        let write = ResourceSpan::new(
            "write",
            SpanBackend::InProcess,
            "fleet.write",
            at(0, 100),
            plain,
        )
        .bind(admission.clone())
        .into_receipt(&root);
        let review = ResourceSpan::new(
            "write/review",
            SpanBackend::InProcess,
            "fleet.review",
            at(100, 150),
            plain,
        )
        .under("write")
        .bind(admission.clone())
        .into_receipt(&root);
        let rework = ResourceSpan::new(
            "write/review/rework",
            SpanBackend::InProcess,
            "fleet.rework",
            at(150, 220),
            plain,
        )
        .under("write/review")
        .bind(admission.clone())
        .measure(Measurement::observed(
            Dimension::Retries,
            1,
            MeasurementSource::HostCounter,
        ))
        .into_receipt(&root);

        let receipts = vec![write, review, rework];
        let tree = span_tree(&receipts);
        assert_eq!(tree.len(), 1, "the whole admission is one causal tree");
        assert_eq!(tree[0].children.len(), 1);
        assert_eq!(tree[0].children[0].receipt.span_id, "write/review");
        assert_eq!(
            tree[0].children[0].children[0].receipt.span_id,
            "write/review/rework"
        );
        for receipt in &receipts {
            assert_eq!(
                receipt.binding.board_ref.as_deref(),
                Some("flux/C-575"),
                "every span in the admission stays attached to the story"
            );
            assert_eq!(
                receipt.binding.assignment_revision.as_deref(),
                Some("rev-3")
            );
        }
    }

    /// An orphan is a partial read, not a row to drop, and a cycle is corruption we must not hang
    /// on. Both still yield exactly one node per receipt.
    #[test]
    fn the_tree_keeps_every_receipt_through_orphans_and_cycles() {
        let root = ResourceRoot::new("req-odd");
        let orphan = ResourceSpan::new("child", SpanBackend::InProcess, "op", at(10, 20), plain)
            .under("a-parent-that-is-not-here")
            .into_receipt(&root);
        let left = ResourceSpan::new("left", SpanBackend::InProcess, "op", at(0, 5), plain)
            .under("right")
            .into_receipt(&root);
        let right = ResourceSpan::new("right", SpanBackend::InProcess, "op", at(1, 6), plain)
            .under("left")
            .into_receipt(&root);

        let receipts = vec![orphan, left, right];
        let tree = span_tree(&receipts);
        let mut seen: Vec<&str> = Vec::new();
        let mut stack: Vec<&SpanNode<'_>> = tree.iter().collect();
        while let Some(node) = stack.pop() {
            seen.push(node.receipt.span_id.as_str());
            stack.extend(node.children.iter());
        }
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec!["child", "left", "right"],
            "every receipt appears exactly once"
        );
    }

    /// Bounding marks the cut, and counts characters rather than bytes — a byte slice through a
    /// multi-byte character panics, and a label is arbitrary text.
    #[test]
    fn a_bounded_label_is_marked_and_never_splits_a_character() {
        let short = bounded_label("web.fetch");
        assert_eq!(short, "web.fetch");
        let long = bounded_label(&"é".repeat(500));
        assert_eq!(long.chars().count(), MAX_LABEL_LEN);
        assert!(long.ends_with('…'));
    }

    /// A charge's basis is the truth-carrier, and an unpriced dimension is expressible without
    /// inventing a zero.
    #[test]
    fn a_charge_states_its_basis_and_coverage() {
        let reported = MoneyCharge::reported(0.0042, "USD");
        assert!(reported.basis.is_provider_reported());
        assert_eq!(reported.coverage, PriceCoverage::Complete);

        let estimated = MoneyCharge::estimated(1.5, "EUR", ChargeBasis::OperatorRate)
            .with_rate_version("cpu-2026-07")
            .effective_at(1_700_000_000_000)
            .with_coverage(PriceCoverage::Unpriced);
        assert!(!estimated.basis.is_provider_reported());
        assert_eq!(estimated.rate_version.as_deref(), Some("cpu-2026-07"));
        assert_eq!(estimated.effective_at_ms, Some(1_700_000_000_000));
        assert_eq!(estimated.coverage, PriceCoverage::Unpriced);
    }
}
