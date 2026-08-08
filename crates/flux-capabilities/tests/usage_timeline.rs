//! The shared cross-harness usage timeline (C-519).
//!
//! One extraction feeds `flux usage` and the observatory: every harness the discovery contract
//! knows about resolves through [`harness_usage`], and nothing but usage metadata is ever read.

use std::fs;
use std::path::{Path, PathBuf};

use codewandler_flux_capabilities::harness::{
    flux_usage, harness_usage, HarnessEnv, HarnessKind, HarnessLocation, NoProgress, UsageScan,
    UsageWindow,
};
use codewandler_flux_capabilities::usage_observatory::{
    totals, ProviderAttribution, ResourceLink, UsageFilter, UsageRange,
};
use flux_core::{PricingTable, Usage};
use flux_events::receipt::{
    Absence, CausalBinding, ClockPrecision, Dimension, Measurement, MeasurementSource,
    ResourceRoot, ResourceSpan, SpanBackend, SpanTiming,
};
use flux_events::EventStore;

/// The sentinel every fixture puts in every field a transcript reader would want: prompts, answers,
/// tool arguments and message bodies. A timeline that never asks for them can never carry them.
const SENTINEL: &str = "DO_NOT_LOAD_THIS_TRANSCRIPT";

fn temp_root(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "flux-timeline-{name}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

/// A home directory holding real state for all four harnesses, wired through the discovery
/// contract's own environment overrides.
fn fixture_env(root: &Path) -> HarnessEnv {
    write_flux(&root.join("flux"));
    write_codex(&root.join("codex"));
    write_claude(&root.join("claude"));
    write_opencode(&root.join("opencode"));
    HarnessEnv::empty()
        .with("HOME", root)
        .with("FLUX_HOME", root.join("flux"))
        .with("CODEX_HOME", root.join("codex"))
        .with("CLAUDE_CONFIG_DIR", root.join("claude"))
        .with("OPENCODE_DATA_DIR", root.join("opencode"))
}

fn write_flux(home: &Path) {
    fs::create_dir_all(home).unwrap();
    let store = EventStore::open(home.join("events.db")).unwrap();
    record_turn(&store);
}

/// One usage-bearing flux turn, whose prompt and answer are the sentinel. Returns its session id.
fn record_turn(store: &EventStore) -> String {
    let session = store.create_session("gpt-5.5").unwrap();
    let turn = store.begin_turn(&session, SENTINEL, "gpt-5.5").unwrap();
    store
        .record_call_usage(
            &session,
            turn,
            "gpt-5.5",
            Usage {
                input_tokens: 120,
                output_tokens: 30,
                ..Default::default()
            },
        )
        .unwrap();
    store
        .end_turn(&session, turn, "ok", 1, SENTINEL, None)
        .unwrap();
    session
}

fn write_codex(home: &Path) {
    let day = home.join("sessions").join("2026").join("07").join("08");
    fs::create_dir_all(&day).unwrap();
    let lines = [
        // No interpolation in this line, so it is a plain literal with real braces rather than a
        // `format!` with doubled ones — clippy's `useless_format` refuses the latter under -D warnings.
        r#"{"timestamp":"2026-07-08T12:00:00Z","type":"turn_context","payload":{"model":"gpt-5.5","cwd":"/w"}}"#
            .to_string(),
        format!(
            r#"{{"timestamp":"2026-07-08T12:00:01Z","type":"event_msg","payload":{{"type":"user_message","message":"{SENTINEL}"}}}}"#
        ),
        r#"{"timestamp":"2026-07-08T12:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":12,"reasoning_output_tokens":3}}}}"#
            .to_string(),
        format!(
            r#"{{"timestamp":"2026-07-08T12:00:03Z","type":"event_msg","payload":{{"type":"agent_message","message":"{SENTINEL}"}}}}"#
        ),
    ];
    fs::write(day.join("rollout.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn write_claude(home: &Path) {
    let project = home.join("projects").join("p");
    fs::create_dir_all(&project).unwrap();
    let lines = [
        format!(
            r#"{{"type":"user","timestamp":"2026-07-08T12:00:00Z","sessionId":"cc","cwd":"/w","message":{{"role":"user","content":"{SENTINEL}"}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","timestamp":"2026-07-08T12:00:01Z","sessionId":"cc","message":{{"id":"msg_1","model":"claude-opus-4-8","content":[{{"type":"text","text":"{SENTINEL}"}},{{"type":"tool_use","name":"bash","input":{{"command":"{SENTINEL}"}}}}],"usage":{{"input_tokens":10,"cache_read_input_tokens":5,"output_tokens":3}}}}}}"#
        ),
    ];
    fs::write(project.join("cc.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn write_opencode(home: &Path) {
    fs::create_dir_all(home).unwrap();
    let conn = rusqlite::Connection::open(home.join("opencode.db")).unwrap();
    conn.execute(
        "create table message (id text primary key, data text not null)",
        [],
    )
    .unwrap();
    conn.execute(
        "insert into message (id, data) values (?1, ?2)",
        (
            "m1",
            format!(
                r#"{{"role":"assistant","providerID":"openrouter","modelID":"z-ai/glm","text":"{SENTINEL}","time":{{"created":1783512000,"completed":1783512001}},"tokens":{{"input":7,"output":2,"reasoning":1,"cache":{{"read":5,"write":3}}}},"cost":0.0042}}"#
            ),
        ),
    )
    .unwrap();
    conn.execute(
        "insert into message (id, data) values (?1, ?2)",
        ("m2", format!(r#"{{"role":"user","text":"{SENTINEL}"}}"#)),
    )
    .unwrap();
    drop(conn);
}

fn scan_all(env: &HarnessEnv, window: UsageWindow) -> Vec<(HarnessKind, UsageScan)> {
    HarnessKind::ALL
        .into_iter()
        .map(|kind| {
            let HarnessLocation::Found(path) = kind.locate(env) else {
                panic!("{} state must be discoverable", kind.id());
            };
            let scan = harness_usage(
                kind,
                &path,
                &PricingTable::builtin(),
                window,
                &mut NoProgress,
            )
            .unwrap_or_else(|e| panic!("{} scan failed: {e}", kind.id()));
            (kind, scan)
        })
        .collect()
}

#[test]
fn shared_timeline_covers_every_discovered_harness() {
    let root = temp_root("every-harness");
    let env = fixture_env(&root);

    let scans = scan_all(&env, UsageWindow::UNBOUNDED);
    for (kind, scan) in &scans {
        assert!(
            !scan.facts.is_empty(),
            "{} yielded no usage-bearing record",
            kind.id()
        );
        for fact in &scan.facts {
            assert_eq!(fact.harness, *kind, "a record kept its source harness");
            assert!(
                !fact.session_id.is_empty(),
                "{} named its session",
                kind.id()
            );
            assert!(
                !fact.raw_model.is_empty(),
                "{} kept its raw model",
                kind.id()
            );
            assert!(fact.calls >= 1, "{} counted its call", kind.id());
            assert!(
                fact.event_ms().is_some(),
                "{} placed its record in time",
                kind.id()
            );
            assert!(
                fact.usage.total() > 0 || fact.usage.reported_cost_usd.is_some(),
                "{} record carries usage",
                kind.id()
            );
            // Absence stays explicit: no harness here proves a billing provider, and none of them
            // may be handed one derived from the model string.
            assert_eq!(fact.provider, ProviderAttribution::Unknown);
        }
        assert!(
            !scan.sessions.is_empty(),
            "{} yielded no session metadata",
            kind.id()
        );
    }

    // Foreign harnesses are partial records by construction: token history proves no CPU, network,
    // or byte ownership, so none of them acquires a causal resource receipt.
    for (kind, scan) in &scans {
        if *kind == HarnessKind::Flux {
            continue;
        }
        assert!(
            scan.facts.iter().all(|fact| fact.receipt.is_none()),
            "{} must not be assigned native resource ownership",
            kind.id()
        );
    }

    // One timeline: the four harnesses fold into a single series without losing any of them.
    let facts = scans
        .iter()
        .flat_map(|(_, scan)| scan.facts.clone())
        .collect::<Vec<_>>();
    let range = UsageRange::new(0, i64::MAX / 4).unwrap();
    let combined = totals(&facts, range, &UsageFilter::default());
    assert_eq!(combined.calls as usize, facts.len());
    assert!(combined.usage.total() > 0);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn usage_timeline_reads_metadata_only() {
    let root = temp_root("metadata-only");
    let env = fixture_env(&root);

    // Every fixture carries the sentinel in its prompt, its assistant text and its tool arguments.
    let corpus = format!("{:?}", scan_all(&env, UsageWindow::UNBOUNDED));
    assert!(
        !corpus.contains(SENTINEL),
        "the timeline carried transcript content out of acquisition"
    );

    // The same holds for a bounded range read — narrowing the window must not widen what is read.
    let bounded = format!(
        "{:?}",
        scan_all(
            &env,
            UsageWindow::from(UsageRange::trailing(i64::MAX / 4, 1))
        )
    );
    assert!(!bounded.contains(SENTINEL));

    let _ = fs::remove_dir_all(root);
}

/// A native record's physical bill comes from C-575's ledger and stays with the session that
/// ledger names. Linking a receipt to another session's call would attribute measured CPU to work
/// that never did it — the same invention the foreign-harness guard already refuses, one identity
/// further down.
#[test]
fn a_flux_record_links_only_the_receipt_recorded_for_its_own_session() {
    let root = temp_root("receipt-link");
    let store = EventStore::open(root.join("events.db")).unwrap();
    let session = record_turn(&store);
    let other_session = record_turn(&store);

    // A real recorded receipt: an owned child whose CPU flux metered, the model tier the call
    // reported, and a network byte count nobody reported.
    let causal_root = ResourceRoot::new("req-519");
    let span = ResourceSpan::new(
        "turn/proc-1",
        SpanBackend::OwnedChild,
        "system.run",
        SpanTiming::new(100, 900, ClockPrecision::Milliseconds),
        |raw: &str| raw.to_string(),
    )
    .bind(CausalBinding {
        session: Some(session.clone()),
        board_ref: Some("flux/C-519".to_string()),
        ..Default::default()
    })
    .measure(Measurement::observed(
        Dimension::ProcessUserCpuTime,
        90,
        MeasurementSource::OsAccounting,
    ))
    .measure(Measurement::observed(
        Dimension::InputTokens,
        120,
        MeasurementSource::ProviderReported,
    ))
    .measure(Measurement::absent(
        Dimension::NetworkBytesIn,
        Absence::NotReported,
    ));
    let recorded = store.record_resource_span(&causal_root, span).unwrap();

    // The link is copied from the receipt as the ledger holds it, and covers only the physical
    // dimensions actually observed: not a model tier, and not the byte count nobody measured.
    let link = ResourceLink::from_receipt(&store.resource_receipts(&causal_root).unwrap()[0]);
    assert_eq!(link.receipt_id, recorded.receipt_id);
    assert_eq!(link.root_id, "req-519");
    assert_eq!(link.session.as_deref(), Some(session.as_str()));
    assert_eq!(link.board_ref.as_deref(), Some("flux/C-519"));
    assert_eq!(link.physical_dimensions, ["process.user_cpu_time_ms"]);
    assert!(link.covers_physical_resources());

    let scan = flux_usage(
        &store,
        &PricingTable::builtin(),
        UsageWindow::UNBOUNDED,
        &mut NoProgress,
    )
    .unwrap();
    let fact = |id: &str| {
        scan.facts
            .iter()
            .find(|fact| fact.session_id == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} produced no usage record"))
    };
    assert_eq!(
        fact(&session).receipt,
        None,
        "acquisition links no receipt on its own"
    );
    assert_eq!(
        fact(&session).with_receipt(link.clone()).receipt,
        Some(link.clone())
    );
    assert_eq!(
        fact(&other_session).with_receipt(link).receipt,
        None,
        "a measured bill is not transferable between sessions"
    );

    let _ = fs::remove_dir_all(root);
}
