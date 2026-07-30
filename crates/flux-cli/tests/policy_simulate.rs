//! Gate-level acceptance for C-131's `flux policy simulate <proposed.toml>`: replay a proposed
//! authorization policy against the recorded op history and report, diff-style, what it would have
//! newly blocked and newly allowed relative to the active policy.
//!
//! Every test drives the real binary under an isolated HOME + CWD + store, the way the other
//! `flux-cli` gate tests do. The store is seeded in-process (an [`EventStore`] writing the same
//! `events.db` the binary reads) rather than by recording a live run, so the fixture pins the exact
//! `tool_call` shapes the simulator must handle — including the ones that are **not** re-evaluable
//! and must come back as `indeterminate` rather than being silently bucketed as allowed or blocked.
//!
//! The binary runs with every provider credential removed from its environment: `flux policy
//! simulate` is a pure read that constructs no provider, so it must succeed with none available.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use flux_events::{EventStore, StoredEvent};
use flux_evidence::{Observation, Phase};
use serde_json::{json, Value};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

/// A temp dir that removes itself on drop, matching `saved_flows.rs` — a failing assertion (a panic)
/// must not leak the fixture.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("flux-policy-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An isolated workspace: a CWD for `.flux/config.toml`, a HOME with no credentials, and an event
/// store the seeder and the binary both address.
struct Fixture {
    _tmp: TempDir,
    work: PathBuf,
    home: PathBuf,
    store: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let tmp = TempDir::new(tag);
        let work = tmp.path().join("work");
        let home = tmp.path().join("home");
        let store = tmp.path().join("store");
        for dir in [&work, &home, &store] {
            std::fs::create_dir_all(dir).expect("create fixture dir");
        }
        Self {
            _tmp: tmp,
            work,
            home,
            store,
        }
    }

    /// The active policy: what `.flux/config.toml` in the CWD contributes right now.
    fn active_config(&self, toml: &str) {
        std::fs::create_dir_all(self.work.join(".flux")).expect("create .flux");
        std::fs::write(self.work.join(".flux/config.toml"), toml).expect("write config");
    }

    /// A proposal document on disk; returns its path.
    fn proposal(&self, name: &str, toml: &str) -> PathBuf {
        let path = self.work.join(name);
        std::fs::write(&path, toml).expect("write proposal");
        path
    }

    fn simulate(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_flux"));
        cmd.arg("policy")
            .arg("simulate")
            .args(args)
            .current_dir(&self.work)
            .env("HOME", &self.home)
            .env("FLUX_STORE_DIR", &self.store)
            .env("NO_COLOR", "1")
            // C-266: state the posture rather than inherit the host's. `policy simulate` is a pure
            // read — no provider, no spawned process, nothing to confine — so unconfined is the
            // honest declaration, and it also keeps the test hermetic against an operator shell
            // that exports `FLUX_SANDBOX=require`.
            .env("FLUX_SANDBOX", "off")
            .stdin(Stdio::null());
        // A pure read constructs no provider, so it must not need — or quietly pick up — a
        // credential. Removing them all is the behavioral proof of "constructs no providers":
        // every provider flux can build needs one of these (or an interactive login under HOME,
        // which is empty here), so a build attempt would fail the command.
        for key in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "GEMINI_API_KEY",
            "GROQ_API_KEY",
            "XAI_API_KEY",
            "DEEPSEEK_API_KEY",
            "MISTRAL_API_KEY",
        ] {
            cmd.env_remove(key);
        }
        cmd.output().expect("spawn flux policy simulate")
    }

    /// Run and require success, returning stdout.
    fn simulate_ok(&self, args: &[&str]) -> String {
        let out = self.simulate(args);
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        assert!(
            out.status.success(),
            "`flux policy simulate {}` exited {:?}\nstdout: {stdout}\nstderr: {stderr}",
            args.join(" "),
            out.status.code()
        );
        stdout
    }

    fn simulate_json(&self, args: &[&str]) -> Value {
        let stdout = self.simulate_ok(args);
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("--json must emit one JSON report ({e}):\n{stdout}"))
    }

    /// Every event in every stream, so "the simulation wrote nothing" is asserted against the log's
    /// actual contents rather than a file mtime.
    fn snapshot(&self) -> Vec<(String, Vec<StoredEvent>)> {
        let events = EventStore::open(self.store.join("events.db")).expect("reopen store");
        events
            .all_streams()
            .expect("all streams")
            .into_iter()
            .map(|stream| {
                let loaded = events.load_stream(&stream, None).expect("load stream");
                (stream, loaded)
            })
            .collect()
    }
}

/// One recorded dispatch, in the exact shape `Executor::dispatch_outcome` writes.
fn call(tool: &str, subjects: &[&str]) -> Observation {
    Observation::new(
        "tool_call",
        Phase::Turn,
        json!({
            "tool": tool,
            "subjects": subjects,
            "caller": "tester",
            // As the recorder writes it. Subject matching discriminates on principal kind, so a
            // record without this is genuinely undecidable against a `user`-subject grant — see
            // `a_record_without_a_principal_kind_cannot_decide_a_floor_withdrawal`.
            "caller_kind": "user",
        }),
    )
}

/// Seed one session with the recorded dispatches the report has to tell apart; returns its id.
fn seed(fixture: &Fixture) -> String {
    let events = EventStore::open(fixture.store.join("events.db")).expect("open seeded store");
    let sid = events.create_session("mock").expect("create session");
    let turn = events
        .begin_turn(&sid, "touch a few things", "mock")
        .expect("begin turn");
    let record = |obs: Observation| {
        events
            .record_observation(&sid, turn, &obs)
            .expect("record observation");
    };

    // Re-evaluable: `read` declares filesystem access, `bash` process access, and `command.invoke`
    // its own named-operation contract — all three derived from the op declaration plus these
    // recorded subjects, with no invocation params involved.
    record(call("read", &["src/main.rs"]));
    record(call("bash", &["ls"]));
    record(call("command.invoke", &["command:deploy"]));
    // Not re-evaluable: an op this build has no authority contract for (a plugin op, a datasource
    // op, or one that has since been removed).
    record(call("acme.deploy", &["cluster-a"]));
    // Not re-evaluable: a dispatch record missing the caller the evaluation reads.
    record(Observation::new(
        "tool_call",
        Phase::Turn,
        json!({ "tool": "read", "subjects": ["legacy.txt"] }),
    ));

    sid
}

/// The active policy used by the diff tests: the built-in local floor plus a project grant that
/// ungates `process.exec` (which the floor otherwise sends to approval).
const ACTIVE_UNGATES_EXEC: &str = r#"
[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "process" }]
actions = ["process.exec"]
"#;

/// C-131 acceptance 1 + 4: `--json` carries the newly-blocked / newly-allowed / unchanged counts and
/// the per-op detail behind them, over a seeded event store.
#[test]
fn simulate_diffs_a_proposed_policy_against_the_recorded_op_history() {
    let fixture = Fixture::new("diff");
    let sid = seed(&fixture);
    fixture.active_config(ACTIVE_UNGATES_EXEC);

    // The proposal: drop the `process.exec` grant, add one for `command.invoke` (default-deny
    // without it), and leave everything the floor already covers untouched.
    let proposed = fixture.proposal(
        "proposed.toml",
        r#"
[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "operation" }]
actions = ["command.invoke"]
"#,
    );

    let report = fixture.simulate_json(&[proposed.to_str().unwrap(), "--sessions", "5", "--json"]);

    assert_eq!(report["ops"], json!(5), "{report:#}");
    assert_eq!(report["counts"]["newly_blocked"], json!(1), "{report:#}");
    assert_eq!(report["counts"]["newly_allowed"], json!(1), "{report:#}");
    assert_eq!(report["counts"]["unchanged"], json!(1), "{report:#}");
    assert_eq!(report["counts"]["indeterminate"], json!(2), "{report:#}");

    // Per-op detail: which op, in which session, for whom, and how its decision moved.
    let blocked = &report["newly_blocked"][0];
    assert_eq!(blocked["op"], json!("bash"), "{report:#}");
    assert_eq!(blocked["session"], json!(sid), "{report:#}");
    assert_eq!(blocked["subjects"], json!(["ls"]), "{report:#}");
    assert_eq!(blocked["caller"], json!("tester"), "{report:#}");
    assert_eq!(blocked["active"], json!("allow"), "{report:#}");
    assert_eq!(
        blocked["proposed"],
        json!("approval_required"),
        "the built-in floor approval-gates process.exec: {report:#}"
    );
    assert_eq!(
        blocked["requirements"][0]["action"],
        json!("process.exec"),
        "{report:#}"
    );

    let allowed = &report["newly_allowed"][0];
    assert_eq!(allowed["op"], json!("command.invoke"), "{report:#}");
    assert_eq!(allowed["active"], json!("deny"), "{report:#}");
    assert_eq!(allowed["proposed"], json!("allow"), "{report:#}");

    assert_eq!(report["unchanged"][0]["op"], json!("read"), "{report:#}");

    // The default (non-`--json`) rendering names the same buckets and the same ops.
    let human = fixture.simulate_ok(&[proposed.to_str().unwrap()]);
    for expected in [
        "newly blocked",
        "newly allowed",
        "unchanged",
        "indeterminate",
        "bash",
        "command.invoke",
        "acme.deploy",
    ] {
        assert!(
            human.contains(expected),
            "the human report omits {expected:?}:\n{human}"
        );
    }
}

/// C-131 acceptance 3: a record the log cannot re-evaluate is reported as `indeterminate` with a
/// reason, and never appears in a decided bucket.
#[test]
fn unre_evaluable_records_are_indeterminate_and_never_silently_bucketed() {
    let fixture = Fixture::new("indeterminate");
    seed(&fixture);
    fixture.active_config(ACTIVE_UNGATES_EXEC);
    let proposed = fixture.proposal("proposed.toml", "");

    let report = fixture.simulate_json(&[proposed.to_str().unwrap(), "--json"]);

    let indeterminate = report["indeterminate"].as_array().expect("array");
    assert_eq!(indeterminate.len(), 2, "{report:#}");
    let ops: Vec<&str> = indeterminate
        .iter()
        .map(|i| i["op"].as_str().unwrap_or("<none>"))
        .collect();
    assert!(
        ops.contains(&"acme.deploy"),
        "an op with no authority contract in this build must be indeterminate: {report:#}"
    );
    for entry in indeterminate {
        assert!(
            !entry["reason"].as_str().unwrap_or_default().is_empty(),
            "every indeterminate op must carry a why: {entry}"
        );
    }
    // The count is not a summary of a bucket the ops were also folded into.
    let decided: usize = ["newly_blocked", "newly_allowed", "unchanged"]
        .iter()
        .map(|bucket| report[*bucket].as_array().expect("array").len())
        .sum();
    assert_eq!(
        decided + indeterminate.len(),
        report["ops"].as_u64().unwrap() as usize,
        "every recorded op lands in exactly one bucket: {report:#}"
    );
    for bucket in ["newly_blocked", "newly_allowed", "unchanged"] {
        let names: Vec<&str> = report[bucket]
            .as_array()
            .expect("array")
            .iter()
            .map(|o| o["op"].as_str().unwrap_or_default())
            .collect();
        assert!(
            !names.contains(&"acme.deploy"),
            "an op with no recoverable authority contract was classified as {bucket}: {report:#}"
        );
    }
}

/// C-131 acceptance 3, the caller-fact leg: the log records the caller's principal id and nothing
/// else about the caller. A proposal whose only grant for an op is gated on trust the log never
/// recorded must report that op `indeterminate` rather than decide it from an invented trust level
/// — while an op the floor already settles regardless of trust stays decided.
#[test]
fn a_trust_gated_grant_makes_only_the_ops_it_could_decide_indeterminate() {
    let fixture = Fixture::new("trust");
    seed(&fixture);
    fixture.active_config(ACTIVE_UNGATES_EXEC);

    // `command.invoke` is default-deny without a grant, and the only grant proposing it is
    // trust-gated: whether it applies depends on a fact the log does not carry.
    let proposed = fixture.proposal(
        "proposed.toml",
        r#"
[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "operation" }]
actions = ["command.invoke"]
required_trust = "privileged"
"#,
    );

    let report = fixture.simulate_json(&[proposed.to_str().unwrap(), "--json"]);

    let indeterminate = report["indeterminate"].as_array().expect("array");
    let entry = indeterminate
        .iter()
        .find(|i| i["op"] == json!("command.invoke"))
        .unwrap_or_else(|| panic!("`command.invoke` must be indeterminate: {report:#}"));
    let reason = entry["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("trust"),
        "the reason must name the missing caller fact, got {reason:?}: {report:#}"
    );
    // `read` is settled by the floor's ungated `workspace.read` grant no matter what trust the
    // caller held, so the trust gate elsewhere must not smear it into indeterminate.
    assert_eq!(report["unchanged"][0]["op"], json!("read"), "{report:#}");
    // `bash` still moves: the proposal drops the config's `process.exec` grant.
    assert_eq!(
        report["newly_blocked"][0]["op"],
        json!("bash"),
        "{report:#}"
    );
}

/// End-to-end guard for the joint bracket, through the real binary.
///
/// A grant gated on a **group** and a **trust level** at once is satisfied by neither fact alone,
/// so probing the omitted caller facts one axis at a time finds no movement and reports the op as
/// confidently `unchanged` — while the proposal in fact converts an approval-gated process exec
/// into a silent allow for a caller wholly consistent with the log. `group X at trust Y` is an
/// ordinary policy shape, and precisely what approval distillation (C-94) would propose, so this
/// must not be a case the simulator gets quietly wrong.
#[test]
fn a_grant_gated_on_two_omitted_facts_at_once_is_never_reported_as_decided() {
    let fixture = Fixture::new("joint");
    seed(&fixture);
    fixture.active_config(ACTIVE_UNGATES_EXEC);

    let proposed = fixture.proposal(
        "proposed.toml",
        r#"
[[policy.grants]]
subjects = [{ kind = "group", id = "ops" }]
resources = [{ kind = "process" }]
actions = ["process.exec"]
required_trust = "privileged"
"#,
    );

    let report = fixture.simulate_json(&[proposed.to_str().unwrap(), "--json"]);

    let indeterminate = report["indeterminate"].as_array().expect("array");
    assert!(
        indeterminate.iter().any(|i| i["op"] == json!("bash")),
        "a grant gated on group AND trust together must not be reported as decided: {report:#}"
    );
    assert!(
        !report["unchanged"]
            .as_array()
            .expect("array")
            .iter()
            .any(|u| u["op"] == json!("bash")),
        "{report:#}"
    );
}

/// C-131 acceptance 2: a pure read. Two full simulations later the log is byte-for-byte the stream
/// it was seeded as, and the command succeeded with no provider credential in its environment.
#[test]
fn simulation_writes_nothing_to_the_event_store() {
    let fixture = Fixture::new("pure-read");
    seed(&fixture);
    fixture.active_config(ACTIVE_UNGATES_EXEC);
    let proposed = fixture.proposal(
        "proposed.toml",
        r#"
[[policy.grants]]
subjects = [{ kind = "user", id = "*" }]
resources = [{ kind = "operation" }]
actions = ["command.invoke"]
"#,
    );
    let before = fixture.snapshot();
    assert!(!before.is_empty(), "the fixture must seed a stream");

    fixture.simulate_ok(&[proposed.to_str().unwrap(), "--json"]);
    fixture.simulate_ok(&[proposed.to_str().unwrap()]);

    assert_eq!(
        fixture.snapshot(),
        before,
        "simulation must not write to the event store"
    );
}
