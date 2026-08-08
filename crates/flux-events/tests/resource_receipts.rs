//! C-575 — immutable causal resource-usage receipts.
//!
//! One fixture run (a model call, guarded network work, an owned child process, a tool dispatch and
//! a nested sub-agent) must land as ONE causal span tree in the ledger: stable ids, explicit parent
//! links, typed absence for every dimension nobody could honestly meter, append-only and idempotent
//! across an event retry, corrections appended rather than edited, and no prompt/answer/argument/
//! output content anywhere in the persisted payload.
//!
//! Hermetic: an in-memory store, fixed timestamps, no network, no child process.

use flux_core::Usage;
use flux_events::receipt::{
    span_tree, Absence, CausalBinding, ChargeBasis, ClockPrecision, Coverage, Dimension, Freshness,
    MeasuredValue, Measurement, MeasurementFamily, MeasurementSource, MoneyCharge, PriceCoverage,
    ResourceRoot, ResourceSpan, SpanBackend, SpanTiming, MAX_LABEL_LEN, RECEIPT_SCHEMA_VERSION,
};
use flux_events::EventStore;

/// Redaction is the caller's job everywhere in `flux-events`; the tests pass the same shape of
/// scrubber the live turn's `Redactor::redact` has.
fn scrub(raw: &str) -> String {
    raw.replace("swordfish-42", "<scrubbed>")
}

fn at(start_ms: i64, end_ms: i64) -> SpanTiming {
    SpanTiming::new(start_ms, end_ms, ClockPrecision::Milliseconds)
}

fn fixture_binding() -> CausalBinding {
    CausalBinding {
        agent_id: Some("coding".to_string()),
        session: Some("s_1".to_string()),
        worker: Some("wave-745/flux/C-575".to_string()),
        wave: Some("wave-745".to_string()),
        repository: Some("flux".to_string()),
        board_ref: Some("flux/C-575".to_string()),
        assignment_revision: Some("rev-3".to_string()),
    }
}

/// The whole point: token events and wall time no longer live in unrelated surfaces — one fixed run
/// produces one root/span tree with stable ids and parent links.
#[test]
fn a_fixed_run_emits_one_causal_span_tree_with_stable_ids_and_parent_links() {
    let store = EventStore::in_memory().unwrap();
    let root = ResourceRoot::new("req-1");

    let turn = ResourceSpan::new(
        "turn",
        SpanBackend::InProcess,
        "agent.turn",
        at(100, 900),
        scrub,
    )
    .bind(fixture_binding())
    .with_phase("execute", scrub)
    .measure(Measurement::observed(
        Dimension::WallTime,
        800,
        MeasurementSource::HostClock,
    ))
    .measure(Measurement::observed(
        Dimension::LoopIterations,
        2,
        MeasurementSource::HostCounter,
    ));
    let turn = store.record_resource_span(&root, turn).unwrap();
    assert_eq!(turn.schema_version, RECEIPT_SCHEMA_VERSION);
    assert_eq!(turn.root_id, "req-1");
    assert_eq!(turn.parent_span_id, None);

    let call = ResourceSpan::new(
        "turn/call-1",
        SpanBackend::Remote,
        "model.call",
        at(120, 400),
        scrub,
    )
    .under("turn")
    .bind(fixture_binding())
    .measure_model_call(&Usage {
        input_tokens: 1_000,
        output_tokens: 120,
        cache_read_input_tokens: 4_000,
        cache_creation_input_tokens: 200,
        cache_creation_1h_input_tokens: 50,
        reasoning_tokens: 30,
        audio_input_tokens: 10,
        audio_output_tokens: 5,
        reported_cost_usd: Some(0.0042),
    })
    .charge(MoneyCharge::reported(0.0042, "USD"));
    store.record_resource_span(&root, call).unwrap();

    let net = ResourceSpan::new(
        "turn/fetch-1",
        SpanBackend::InProcess,
        "web.fetch",
        at(420, 520),
        scrub,
    )
    .under("turn")
    .measure(Measurement::observed(
        Dimension::NetworkRequests,
        1,
        MeasurementSource::GuardedTransport,
    ))
    .measure(Measurement::observed(
        Dimension::NetworkTimeToFirstByte,
        40,
        MeasurementSource::GuardedTransport,
    ))
    .measure(Measurement::observed(
        Dimension::NetworkBytesIn,
        2_048,
        MeasurementSource::GuardedTransport,
    ));
    store.record_resource_span(&root, net).unwrap();

    let child = ResourceSpan::new(
        "turn/proc-1",
        SpanBackend::OwnedChild,
        "system.run",
        at(530, 700),
        scrub,
    )
    .under("turn")
    .measure(Measurement::observed(
        Dimension::ProcessUserCpuTime,
        90,
        MeasurementSource::OsAccounting,
    ))
    .measure(Measurement::observed(
        Dimension::ProcessPeakRss,
        16_384,
        MeasurementSource::OsAccounting,
    ));
    store.record_resource_span(&root, child).unwrap();

    let tool = ResourceSpan::new(
        "turn/tool-1",
        SpanBackend::InProcess,
        "tool.read",
        at(710, 730),
        scrub,
    )
    .under("turn")
    .measure(Measurement::observed(
        Dimension::ToolDispatches,
        1,
        MeasurementSource::HostCounter,
    ))
    .measure(Measurement::observed(
        Dimension::FileBytesRead,
        512,
        MeasurementSource::GuardedTool,
    ));
    store.record_resource_span(&root, tool).unwrap();

    // A nested sub-agent hangs off the tool span that spawned it — causal, not time-window.
    let sub = ResourceSpan::new(
        "turn/tool-1/sub",
        SpanBackend::InProcess,
        "agent.sub_turn",
        at(715, 728),
        scrub,
    )
    .under("turn/tool-1")
    .measure_model_call(&Usage {
        input_tokens: 300,
        output_tokens: 40,
        ..Default::default()
    });
    store.record_resource_span(&root, sub).unwrap();

    let receipts = store.resource_receipts(&root).unwrap();
    assert_eq!(receipts.len(), 6, "one receipt per recorded span");

    let tree = span_tree(&receipts);
    assert_eq!(tree.len(), 1, "one fixed run is ONE root: {tree:#?}");
    let root_node = &tree[0];
    assert_eq!(root_node.receipt.span_id, "turn");
    assert_eq!(
        root_node
            .children
            .iter()
            .map(|c| c.receipt.span_id.as_str())
            .collect::<Vec<_>>(),
        vec!["turn/call-1", "turn/fetch-1", "turn/proc-1", "turn/tool-1"],
        "children keep their explicit parent link, ordered by start"
    );
    let tool_node = root_node
        .children
        .iter()
        .find(|c| c.receipt.span_id == "turn/tool-1")
        .expect("the tool span is in the tree");
    assert_eq!(tool_node.children.len(), 1, "the sub-agent nests under it");
    assert_eq!(tool_node.children[0].receipt.span_id, "turn/tool-1/sub");

    // Ids are stable: the receipt id is derived from root + span, not minted per append.
    assert_eq!(root_node.receipt.receipt_id, root.receipt_id("turn"));

    // The model span carries every Usage tier the canonical `CallUsage` record carries.
    let call = receipts
        .iter()
        .find(|r| r.span_id == "turn/call-1")
        .expect("the model call span");
    for dimension in Dimension::CATALOGUE
        .iter()
        .filter(|d| d.family() == MeasurementFamily::Model)
    {
        assert!(
            call.measurement(*dimension).is_some(),
            "a model call must state {dimension:?} — a missing tier is not the same as zero"
        );
    }
    assert_eq!(
        call.measurement(Dimension::CacheReadInputTokens)
            .map(|m| m.value.clone()),
        Some(MeasuredValue::Observed(4_000))
    );
    assert_eq!(call.charges.len(), 1);
    assert_eq!(call.charges[0].basis, ChargeBasis::ProviderReported);
    assert_eq!(call.charges[0].coverage, PriceCoverage::Complete);
}

/// Append-only and idempotent: a retried event does not append a second receipt, and it cannot
/// restate the measurements of the one already recorded.
#[test]
fn a_retried_receipt_is_idempotent_and_never_rewrites_the_original() {
    let store = EventStore::in_memory().unwrap();
    let root = ResourceRoot::new("req-retry");

    let span = || {
        ResourceSpan::new("call", SpanBackend::Remote, "model.call", at(0, 10), scrub).measure(
            Measurement::observed(
                Dimension::OutputTokens,
                100,
                MeasurementSource::ProviderReported,
            ),
        )
    };
    let first = store.record_resource_span(&root, span()).unwrap();

    // The retry carries a DIFFERENT figure: an at-least-once event pipeline must not let it land.
    let retry = ResourceSpan::new("call", SpanBackend::Remote, "model.call", at(0, 10), scrub)
        .measure(Measurement::observed(
            Dimension::OutputTokens,
            999_999,
            MeasurementSource::ProviderReported,
        ));
    let second = store.record_resource_span(&root, retry).unwrap();

    assert_eq!(
        second, first,
        "a retry returns the receipt already recorded"
    );
    let receipts = store.resource_receipts(&root).unwrap();
    assert_eq!(receipts.len(), 1, "no duplicate row: {receipts:#?}");
    assert_eq!(
        receipts[0]
            .measurement(Dimension::OutputTokens)
            .unwrap()
            .value,
        MeasuredValue::Observed(100)
    );
    assert_eq!(
        store.resource_history(&root).unwrap().len(),
        1,
        "the log itself holds exactly one event"
    );
}

/// Unsupported / not-reported / not-attributable is typed per dimension. A backend that owns no
/// meter reports absence — never a numeric zero that later reads as "this cost nothing".
#[test]
fn an_unmetered_dimension_is_typed_absent_rather_than_zero() {
    let store = EventStore::in_memory().unwrap();
    let root = ResourceRoot::new("req-absence");

    // An in-process library owns no process accounting, even if a caller offers a number.
    let in_process = ResourceSpan::new(
        "lib",
        SpanBackend::InProcess,
        "cognition.consult",
        at(0, 5),
        scrub,
    )
    .measure(Measurement::observed(
        Dimension::ProcessUserCpuTime,
        0,
        MeasurementSource::OsAccounting,
    ))
    .measure(Measurement::observed(
        Dimension::ProcessPeakRss,
        0,
        MeasurementSource::OsAccounting,
    ));
    let in_process = store.record_resource_span(&root, in_process).unwrap();
    assert_eq!(
        in_process
            .measurement(Dimension::ProcessUserCpuTime)
            .unwrap()
            .value,
        MeasuredValue::Absent(Absence::Unsupported),
        "an in-process span cannot honestly meter child-process CPU"
    );
    assert_eq!(
        in_process
            .measurement(Dimension::ProcessPeakRss)
            .unwrap()
            .value,
        MeasuredValue::Absent(Absence::Unsupported)
    );

    // A foreign harness reports only what its conformance contract measures.
    let foreign = ResourceSpan::new(
        "foreign",
        SpanBackend::Foreign,
        "harness.claude",
        at(0, 5),
        scrub,
    )
    .measure(Measurement::observed(
        Dimension::NetworkBytesOut,
        0,
        MeasurementSource::GuardedTransport,
    ))
    .measure(Measurement::observed(
        Dimension::ProcessUserCpuTime,
        0,
        MeasurementSource::OsAccounting,
    ))
    .measure(Measurement::absent(
        Dimension::ReasoningTokens,
        Absence::NotReported,
    ));
    let foreign = store.record_resource_span(&root, foreign).unwrap();
    assert_eq!(
        foreign
            .measurement(Dimension::NetworkBytesOut)
            .unwrap()
            .value,
        MeasuredValue::Absent(Absence::NotReported),
        "a foreign backend's traffic never crossed our guarded transport"
    );
    assert_eq!(
        foreign
            .measurement(Dimension::ProcessUserCpuTime)
            .unwrap()
            .value,
        MeasuredValue::Absent(Absence::NotReported)
    );
    assert_eq!(
        foreign
            .measurement(Dimension::ReasoningTokens)
            .unwrap()
            .value,
        MeasuredValue::Absent(Absence::NotReported)
    );

    // A cancelled span keeps what it measured and types the rest — partial, not complete.
    let cancelled = ResourceSpan::new(
        "cancelled",
        SpanBackend::OwnedChild,
        "system.run",
        at(0, 3),
        scrub,
    )
    .with_coverage(Coverage::Partial)
    .measure(Measurement::observed(
        Dimension::WallTime,
        3,
        MeasurementSource::HostClock,
    ))
    .measure(Measurement::absent(
        Dimension::ProcessUserCpuTime,
        Absence::NotAttributable,
    ));
    let cancelled = store.record_resource_span(&root, cancelled).unwrap();
    assert_eq!(cancelled.coverage, Coverage::Partial);
    assert_eq!(
        cancelled
            .measurement(Dimension::ProcessUserCpuTime)
            .unwrap()
            .value,
        MeasuredValue::Absent(Absence::NotAttributable)
    );
    assert_eq!(cancelled.freshness, Freshness::Live);
}

/// A correction is a new receipt naming the one it corrects. The original stays in the log exactly
/// as recorded — durable history stays auditable.
#[test]
fn a_correction_is_appended_and_leaves_the_original_untouched() {
    let store = EventStore::in_memory().unwrap();
    let root = ResourceRoot::new("req-correct");

    let original = store
        .record_resource_span(
            &root,
            ResourceSpan::new("call", SpanBackend::Remote, "model.call", at(0, 10), scrub).measure(
                Measurement::observed(
                    Dimension::OutputTokens,
                    100,
                    MeasurementSource::ProviderReported,
                ),
            ),
        )
        .unwrap();

    let correction = store
        .record_resource_span(
            &root,
            ResourceSpan::new(
                "call#c1",
                SpanBackend::Remote,
                "model.call",
                at(0, 10),
                scrub,
            )
            .correcting(&original.receipt_id)
            .with_freshness(Freshness::Backfilled)
            .measure(Measurement::observed(
                Dimension::OutputTokens,
                140,
                MeasurementSource::ProviderReported,
            )),
        )
        .unwrap();

    assert_eq!(
        correction.correction_of.as_deref(),
        Some(original.receipt_id.as_str())
    );
    assert_eq!(correction.freshness, Freshness::Backfilled);

    let receipts = store.resource_receipts(&root).unwrap();
    assert_eq!(
        receipts.len(),
        2,
        "the correction is an append, not an edit"
    );
    let kept = receipts
        .iter()
        .find(|r| r.receipt_id == original.receipt_id)
        .expect("the original receipt is still there");
    assert_eq!(
        kept.measurement(Dimension::OutputTokens).unwrap().value,
        MeasuredValue::Observed(100),
        "a correction must not rewrite the measured original"
    );
}

/// Receipts carry counts, timings, ids and bounded labels — never prompt, answer, tool argument,
/// command output, file content or a secret-bearing URL.
#[test]
fn labels_are_scrubbed_and_bounded_before_persistence() {
    let store = EventStore::in_memory().unwrap();
    let root = ResourceRoot::new("req-labels");

    let long = format!(
        "web.fetch https://api.example.com/?opaque=swordfish-42&q={}",
        "x".repeat(400)
    );
    let span = ResourceSpan::new("fetch", SpanBackend::InProcess, &long, at(0, 1), scrub)
        .with_phase(&format!("execute-{}", "y".repeat(400)), scrub);
    let stored = store.record_resource_span(&root, span).unwrap();

    assert!(
        !stored.operation.contains("swordfish-42"),
        "the redactor runs before the store ever sees the label: {}",
        stored.operation
    );
    assert!(
        stored.operation.chars().count() <= MAX_LABEL_LEN,
        "an unbounded label is a payload smuggling channel: {}",
        stored.operation.chars().count()
    );
    assert!(stored.phase.unwrap().chars().count() <= MAX_LABEL_LEN);

    // The persisted payload is the ledger's whole surface: nothing in it can hold content.
    let history = store.resource_history(&root).unwrap();
    let json = serde_json::to_string(&history[0].kind).unwrap();
    assert!(!json.contains("swordfish-42"), "{json}");
    assert!(
        !json.contains(&"x".repeat(200)),
        "no smuggled payload: {json}"
    );
}

/// The first catalogue covers every family the design names, and every dimension states its unit.
#[test]
fn the_measurement_catalogue_covers_every_family_with_explicit_units() {
    use MeasurementFamily::*;
    for family in [
        Model, Runtime, Process, Network, Filesystem, Capacity, Validation,
    ] {
        assert!(
            Dimension::CATALOGUE.iter().any(|d| d.family() == family),
            "no dimension covers {family:?}"
        );
    }
    for dimension in Dimension::CATALOGUE {
        assert!(
            !dimension.as_str().is_empty(),
            "{dimension:?} needs a stable wire name"
        );
    }
    let mut names: Vec<&str> = Dimension::CATALOGUE.iter().map(|d| d.as_str()).collect();
    names.sort_unstable();
    let total = names.len();
    names.dedup();
    assert_eq!(total, names.len(), "two dimensions share one wire name");
}

/// Money stays separate from physical measurement, and an estimate never claims to be a bill.
#[test]
fn estimated_and_reported_money_keep_their_basis_and_coverage() {
    let store = EventStore::in_memory().unwrap();
    let root = ResourceRoot::new("req-money");

    let span = ResourceSpan::new("call", SpanBackend::Remote, "model.call", at(0, 10), scrub)
        .measure(Measurement::observed(
            Dimension::OutputTokens,
            100,
            MeasurementSource::ProviderReported,
        ))
        .charge(MoneyCharge::reported(0.0042, "USD"))
        .charge(
            MoneyCharge::estimated(0.0051, "USD", ChargeBasis::PricingTable)
                .with_rate_version("table-2026-07")
                .with_coverage(PriceCoverage::Partial),
        );
    let stored = store.record_resource_span(&root, span).unwrap();

    assert_eq!(stored.charges.len(), 2);
    assert_eq!(stored.charges[0].basis, ChargeBasis::ProviderReported);
    assert_eq!(stored.charges[0].coverage, PriceCoverage::Complete);
    assert_eq!(stored.charges[1].basis, ChargeBasis::PricingTable);
    assert_eq!(stored.charges[1].coverage, PriceCoverage::Partial);
    assert_eq!(
        stored.charges[1].rate_version.as_deref(),
        Some("table-2026-07")
    );

    // An unpriced dimension has no charge at all — it never becomes $0.
    let unpriced = ResourceSpan::new(
        "cpu",
        SpanBackend::OwnedChild,
        "system.run",
        at(0, 10),
        scrub,
    )
    .measure(Measurement::observed(
        Dimension::ProcessUserCpuTime,
        250,
        MeasurementSource::OsAccounting,
    ));
    let unpriced = store.record_resource_span(&root, unpriced).unwrap();
    assert!(
        unpriced.charges.is_empty(),
        "local CPU seconds are not silently priced"
    );
}
