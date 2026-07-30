//! Message-shaped extraction from another harness's local state (C-214), exercised as a library.
//!
//! The scan layer (C-213) answers *where* a harness keeps its state; this is the layer that answers
//! *what was said*. The fixtures below are cut down from the shapes the three external harnesses
//! actually write — a claude-code project transcript, a codex rollout, an opencode database — so a
//! silent shape drift shows up as an assertion rather than as an empty index.
//!
//! What is pinned, and why:
//! - **The text survives.** Every harness stores at least some messages as an array of typed
//!   content blocks, and a naive `as_str()` on those yields `""` rather than failing. Each fixture
//!   therefore carries a multi-part message whose text is asserted whole.
//! - **The body budget bites.** A message record carries full text where a usage record carries
//!   integers, so the inherited file/count caps are necessary and not sufficient: a total
//!   extracted-bytes ceiling, a per-body ceiling, and a message-count ceiling are all asserted to
//!   degrade by skipping *and counting*.
//! - **Malformed input never aborts a scan** — one bad line, one bad row, one unreadable file.
//! - **`index` is stable across re-scans**, because C-215 builds record ids on it.

use std::fs;
use std::path::{Path, PathBuf};

use codewandler_flux_capabilities::harness::{
    claude_messages, codex_messages, opencode_messages, HarnessKind, HarnessMessage, MessageRole,
    ScanBudget,
};

fn scratch(name: &str) -> PathBuf {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("flux-msg-{name}-{}-{n}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn collect(scan: impl FnOnce(&mut dyn FnMut(HarnessMessage))) -> Vec<HarnessMessage> {
    let mut out = Vec::new();
    scan(&mut |m| out.push(m));
    out
}

// ---------------------------------------------------------------------------------------------
// claude-code: `~/.claude/projects/<slug>/<session>.jsonl`
// ---------------------------------------------------------------------------------------------

/// A transcript in the shape claude-code writes: `type` at the top level, the body under `message`,
/// content either a bare string or an array of typed blocks.
const CLAUDE_TRANSCRIPT: &str = concat!(
    r#"{"type":"user","sessionId":"s-1","cwd":"/work/repo","timestamp":"2026-01-02T03:04:05.123Z","#,
    r#""message":{"role":"user","content":"why did we drop the retry wrapper"}}"#,
    "\n",
    r#"{"type":"assistant","sessionId":"s-1","cwd":"/work/repo","timestamp":"2026-01-02T03:04:07.000Z","#,
    r#""message":{"role":"assistant","model":"claude-opus-4","content":["#,
    r#"{"type":"thinking","thinking":"the wrapper double-counted"},"#,
    r#"{"type":"text","text":"Because it retried on 4xx."},"#,
    r#"{"type":"tool_use","name":"Bash","input":{"command":"git log"}}"#,
    r#"]}}"#,
    "\n",
    r#"{"type":"file-history-snapshot","snapshot":{}}"#,
    "\n",
    r#"{"type":"user","sessionId":"s-1","message":{"role":"user","content":"trunc"#,
    "\n",
    r#"{"type":"user","sessionId":"s-1","cwd":"/work/repo","timestamp":"2026-01-02T03:04:09.000Z","#,
    r#""message":{"role":"user","content":[{"type":"tool_result","content":[{"type":"text","text":"abc123 drop retry"}]}]}}"#,
    "\n",
);

#[test]
fn claude_messages_come_back_whole_including_structured_content() {
    let root = scratch("claude");
    let project = root.join("-work-repo");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("s-1.jsonl"), CLAUDE_TRANSCRIPT).unwrap();

    let mut stats = None;
    let msgs = collect(|emit| {
        stats = Some(claude_messages(&root, ScanBudget::for_messages(), emit).unwrap());
    });
    let stats = stats.unwrap();

    assert_eq!(
        msgs.len(),
        3,
        "user, assistant, tool-result user: {msgs:#?}"
    );
    assert!(msgs.iter().all(|m| m.harness == HarnessKind::Claude));
    assert!(msgs.iter().all(|m| m.session_id == "s-1"));
    assert!(msgs
        .iter()
        .all(|m| m.workspace.as_deref() == Some("/work/repo")));

    assert_eq!(msgs[0].role, MessageRole::User);
    assert_eq!(msgs[0].text, "why did we drop the retry wrapper");
    assert_eq!(msgs[0].ts_ms, Some(1_767_323_045_123));

    // The trap: a naive `as_str()` on this content array yields "" and never fails.
    assert_eq!(msgs[1].role, MessageRole::Assistant);
    assert_eq!(
        msgs[1].text, "the wrapper double-counted\nBecause it retried on 4xx.\n[tool_use: Bash]",
        "every block of a structured message contributes, tool calls included"
    );
    assert_eq!(msgs[1].model.as_deref(), Some("claude-opus-4"));

    // A tool result arrives as a `user` record; the normalized role says what it really is.
    assert_eq!(msgs[2].role, MessageRole::Tool);
    assert_eq!(msgs[2].text, "abc123 drop retry");

    // Ordering is dense and per-session, so `index` addresses a message.
    assert_eq!(
        msgs.iter().map(|m| m.index).collect::<Vec<_>>(),
        vec![0, 1, 2]
    );

    // The corrupt line is counted, not swallowed, and it did not abort the file.
    assert_eq!(stats.skipped_malformed, 1, "{stats:?}");
    assert_eq!(stats.emitted, 3);

    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------------------------
// codex: `~/.codex/sessions/<y>/<m>/<d>/rollout-*.jsonl`
// ---------------------------------------------------------------------------------------------

/// A rollout in the shape codex writes: every line `{timestamp, type, payload}`, the conversation
/// carried by `response_item` payloads of type `message` with typed `input_text`/`output_text`.
const CODEX_ROLLOUT: &str = concat!(
    r#"{"timestamp":"2026-01-02T03:04:05.000Z","type":"session_meta","#,
    r#""payload":{"id":"c-9","cwd":"/work/other","timestamp":"2026-01-02T03:04:05.000Z"}}"#,
    "\n",
    r#"{"timestamp":"2026-01-02T03:04:06.000Z","type":"turn_context","payload":{"model":"gpt-5.5"}}"#,
    "\n",
    r#"{"timestamp":"2026-01-02T03:04:07.000Z","type":"response_item","payload":{"type":"message","#,
    r#""role":"user","content":[{"type":"input_text","text":"first half"},{"type":"input_text","text":"second half"}]}}"#,
    "\n",
    r#"{"timestamp":"2026-01-02T03:04:07.500Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_te"#,
    "\n",
    r#"{"timestamp":"2026-01-02T03:04:08.000Z","type":"response_item","payload":{"type":"message","#,
    r#""role":"assistant","content":[{"type":"output_text","text":"answered"}]}}"#,
    "\n",
    r#"{"timestamp":"2026-01-02T03:04:09.000Z","type":"event_msg","payload":{"type":"token_count","info":{}}}"#,
    "\n",
);

#[test]
fn codex_messages_come_back_whole_including_structured_content() {
    let root = scratch("codex");
    let day = root.join("2026").join("01").join("02");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join("rollout-2026-01-02T03-04-05-c-9.jsonl"),
        CODEX_ROLLOUT,
    )
    .unwrap();

    let mut stats = None;
    let msgs = collect(|emit| {
        stats = Some(codex_messages(&root, ScanBudget::for_messages(), emit).unwrap());
    });
    let stats = stats.unwrap();

    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert!(msgs.iter().all(|m| m.harness == HarnessKind::Codex));
    assert!(msgs.iter().all(|m| m.session_id == "c-9"));
    assert!(msgs
        .iter()
        .all(|m| m.workspace.as_deref() == Some("/work/other")));
    assert!(msgs.iter().all(|m| m.model.as_deref() == Some("gpt-5.5")));

    assert_eq!(msgs[0].role, MessageRole::User);
    assert_eq!(
        msgs[0].text, "first half\nsecond half",
        "a multi-part codex message keeps every part"
    );
    assert_eq!(msgs[1].role, MessageRole::Assistant);
    assert_eq!(msgs[1].text, "answered");
    assert_eq!(msgs.iter().map(|m| m.index).collect::<Vec<_>>(), vec![0, 1]);

    assert_eq!(stats.skipped_malformed, 1, "{stats:?}");
    assert_eq!(stats.emitted, 2);

    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------------------------
// opencode: `~/.local/share/opencode/opencode.db`
// ---------------------------------------------------------------------------------------------

/// Build a database in opencode's shape: `message` rows carry role/model/path, and the *text* lives
/// in `part` rows keyed by `message_id`. A reader that only looks at `message.data` — which is what
/// `flux usage` needs and all it needs — comes back with empty bodies.
fn seed_opencode(db: &Path) {
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "create table session (id text primary key, directory text, time_created integer);
         create table message (id text primary key, session_id text, time_created integer,
                               time_updated integer, data text not null);
         create table part (id text primary key, message_id text, session_id text,
                            time_created integer, data text not null);",
    )
    .unwrap();
    conn.execute(
        "insert into session values ('o-1', '/work/opencode', 1767323045)",
        [],
    )
    .unwrap();
    let msg = |id: &str, ts: i64, data: &str| (id.to_string(), ts, data.to_string());
    for (id, ts, data) in [
        msg(
            "m-1",
            1_767_323_045_000,
            r#"{"role":"user","path":{"cwd":"/work/opencode"}}"#,
        ),
        msg(
            "m-2",
            1_767_323_047_000,
            r#"{"role":"assistant","modelID":"claude-sonnet-4","providerID":"anthropic","path":{"cwd":"/work/opencode"}}"#,
        ),
        msg("m-3", 1_767_323_049_000, "{ not json"),
    ] {
        conn.execute(
            "insert into message values (?1, 'o-1', ?2, ?2, ?3)",
            rusqlite::params![id, ts, data],
        )
        .unwrap();
    }
    for (id, mid, data) in [
        (
            "p-1",
            "m-1",
            r#"{"type":"text","text":"why did we drop the retry wrapper"}"#,
        ),
        (
            "p-2",
            "m-2",
            r#"{"type":"reasoning","text":"it double-counted"}"#,
        ),
        (
            "p-3",
            "m-2",
            r#"{"type":"text","text":"Because it retried on 4xx."}"#,
        ),
        ("p-4", "m-2", r#"{"type":"tool","tool":"bash","state":{}}"#),
        ("p-5", "m-2", r#"{"type":"step-finish"}"#),
    ] {
        conn.execute(
            "insert into part values (?1, ?2, 'o-1', 0, ?3)",
            rusqlite::params![id, mid, data],
        )
        .unwrap();
    }
}

#[test]
fn opencode_messages_come_back_whole_including_multi_part_bodies() {
    let root = scratch("opencode");
    let db = root.join("opencode.db");
    seed_opencode(&db);

    let mut stats = None;
    let msgs = collect(|emit| {
        stats = Some(opencode_messages(&db, ScanBudget::for_messages(), emit).unwrap());
    });
    let stats = stats.unwrap();

    assert_eq!(msgs.len(), 2, "{msgs:#?}");
    assert!(msgs.iter().all(|m| m.harness == HarnessKind::Opencode));
    assert!(msgs.iter().all(|m| m.session_id == "o-1"));
    assert!(msgs
        .iter()
        .all(|m| m.workspace.as_deref() == Some("/work/opencode")));

    assert_eq!(msgs[0].role, MessageRole::User);
    assert_eq!(msgs[0].text, "why did we drop the retry wrapper");
    assert_eq!(msgs[0].ts_ms, Some(1_767_323_045_000));

    assert_eq!(msgs[1].role, MessageRole::Assistant);
    assert_eq!(
        msgs[1].text, "it double-counted\nBecause it retried on 4xx.\n[tool_use: bash]",
        "an opencode body is assembled from its `part` rows, in order"
    );
    assert_eq!(msgs[1].model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(msgs.iter().map(|m| m.index).collect::<Vec<_>>(), vec![0, 1]);

    assert_eq!(
        stats.skipped_malformed, 1,
        "the bad row is counted: {stats:?}"
    );

    let _ = fs::remove_dir_all(root);
}

// ---------------------------------------------------------------------------------------------
// The budget — the part the inherited file/count caps do not cover
// ---------------------------------------------------------------------------------------------

#[test]
fn one_enormous_message_is_skipped_and_counted_rather_than_extracted() {
    let root = scratch("huge-body");
    let project = root.join("p");
    fs::create_dir_all(&project).unwrap();
    let huge = "x".repeat(200_000);
    let mut transcript = String::new();
    transcript.push_str(&format!(
        r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"{huge}"}}}}"#
    ));
    transcript.push('\n');
    transcript
        .push_str(r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"small"}}"#);
    transcript.push('\n');
    fs::write(project.join("s.jsonl"), &transcript).unwrap();

    let budget = ScanBudget {
        max_message_bytes: 1024,
        ..ScanBudget::for_messages()
    };
    let mut stats = None;
    let msgs = collect(|emit| stats = Some(claude_messages(&root, budget, emit).unwrap()));
    let stats = stats.unwrap();

    assert_eq!(msgs.len(), 1, "the oversized body never materialized");
    assert_eq!(msgs[0].text, "small");
    assert_eq!(stats.skipped_oversize, 1, "{stats:?}");
    assert!(
        stats.body_bytes < 1024,
        "no body over the cap was ever accounted: {stats:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_line_over_the_line_budget_is_skipped_without_being_read_into_memory() {
    let root = scratch("huge-line");
    let project = root.join("p");
    fs::create_dir_all(&project).unwrap();
    let huge = "x".repeat(100_000);
    let transcript = format!(
        "{}\n{}\n",
        format_args!(
            r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"{huge}"}}}}"#
        ),
        r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"after"}}"#
    );
    fs::write(project.join("s.jsonl"), &transcript).unwrap();

    let budget = ScanBudget {
        max_line_bytes: 4096,
        ..ScanBudget::for_messages()
    };
    let mut stats = None;
    let msgs = collect(|emit| stats = Some(claude_messages(&root, budget, emit).unwrap()));
    let stats = stats.unwrap();

    assert_eq!(msgs.len(), 1, "the rest of the file still scans");
    assert_eq!(msgs[0].text, "after");
    assert_eq!(stats.skipped_oversize, 1, "{stats:?}");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn the_total_extracted_bytes_ceiling_stops_the_scan_and_reports_it() {
    let root = scratch("total-bytes");
    let project = root.join("p");
    fs::create_dir_all(&project).unwrap();
    let body = "y".repeat(100);
    let mut transcript = String::new();
    for _ in 0..50 {
        transcript.push_str(&format!(
            r#"{{"type":"user","sessionId":"s","message":{{"role":"user","content":"{body}"}}}}"#
        ));
        transcript.push('\n');
    }
    fs::write(project.join("s.jsonl"), &transcript).unwrap();

    let budget = ScanBudget {
        max_message_total_bytes: 450,
        ..ScanBudget::for_messages()
    };
    let mut stats = None;
    let msgs = collect(|emit| stats = Some(claude_messages(&root, budget, emit).unwrap()));
    let stats = stats.unwrap();

    assert_eq!(msgs.len(), 4, "100 bytes each, ceiling 450: {stats:?}");
    assert!(stats.body_bytes <= 450, "{stats:?}");
    assert!(
        stats.budget_exhausted,
        "an exhausted budget is reported, not swallowed: {stats:?}"
    );
    assert!(stats.skipped_over_budget > 0, "{stats:?}");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_file_over_the_file_budget_is_skipped_and_counted() {
    let root = scratch("huge-file");
    let project = root.join("p");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("big.jsonl"),
        r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"never read"}}
"#,
    )
    .unwrap();

    let budget = ScanBudget {
        max_file_bytes: 4,
        ..ScanBudget::for_messages()
    };
    let mut stats = None;
    let msgs = collect(|emit| stats = Some(claude_messages(&root, budget, emit).unwrap()));
    let stats = stats.unwrap();

    assert!(msgs.is_empty());
    assert_eq!(
        stats.skipped_unreadable + stats.skipped_oversize,
        1,
        "{stats:?}"
    );

    let _ = fs::remove_dir_all(root);
}

#[test]
fn index_addresses_the_same_message_on_a_re_scan_and_survives_an_append() {
    let root = scratch("stable-index");
    let project = root.join("p");
    fs::create_dir_all(&project).unwrap();
    let base = concat!(
        r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"one"}}"#,
        "\n",
        r#"{"type":"assistant","sessionId":"s","message":{"role":"assistant","content":"two"}}"#,
        "\n"
    );
    fs::write(project.join("s.jsonl"), base).unwrap();

    let addressed = |root: &Path| -> Vec<(String, u32, String)> {
        collect(|emit| {
            claude_messages(root, ScanBudget::for_messages(), emit).unwrap();
        })
        .into_iter()
        .map(|m| (m.session_id, m.index, m.text))
        .collect()
    };

    let first = addressed(&root);
    assert_eq!(first, addressed(&root), "a re-scan must renumber nothing");

    fs::write(
        project.join("s.jsonl"),
        format!(
            "{base}{}\n",
            r#"{"type":"user","sessionId":"s","message":{"role":"user","content":"three"}}"#
        ),
    )
    .unwrap();
    let second = addressed(&root);
    assert_eq!(
        second[..first.len()],
        first[..],
        "appending must not renumber what was already addressed"
    );
    assert_eq!(second[2].1, 2);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn an_unreadable_root_is_the_only_failure_that_propagates() {
    let root = scratch("absent");
    let missing = root.join("nope");
    let mut sink = |_: HarnessMessage| {};
    assert!(claude_messages(&missing, ScanBudget::for_messages(), &mut sink).is_err());
    assert!(codex_messages(&missing, ScanBudget::for_messages(), &mut sink).is_err());
    assert!(opencode_messages(
        &missing.join("opencode.db"),
        ScanBudget::for_messages(),
        &mut sink
    )
    .is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn an_opencode_database_without_the_expected_schema_degrades_to_nothing() {
    let root = scratch("odd-schema");
    let db = root.join("opencode.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute("create table unrelated (id text primary key)", [])
        .unwrap();
    drop(conn);

    let mut stats = None;
    let msgs = collect(|emit| {
        stats = Some(opencode_messages(&db, ScanBudget::for_messages(), emit).unwrap())
    });
    assert!(msgs.is_empty(), "an unexpected schema is not an abort");
    assert_eq!(stats.unwrap().emitted, 0);

    let _ = fs::remove_dir_all(root);
}
