//! End-to-end host tests: build an [`App`] from a pure-op program (no provider, no model) and prove
//! the trigger → journey → execution path runs, that the orchestration ops are functional, and that
//! the bundled `examples/hello.flux` stays valid.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use flux_app::App;
use flux_core::{Chunk, StopReason};
use flux_lang::program::{Module, Program};
use flux_provider::{ChunkStream, Provider, Request};
use flux_runtime::{Tool, ToolContext, ToolResult};
use serde_json::json;

/// Parse a program source string, panicking with context if it isn't a program.
fn program(src: &str) -> Program {
    match Module::parse_str(src).expect("parse program") {
        Module::Program(p) => p,
        Module::Flow(_) => panic!("expected a program, got a bare flow"),
    }
}

#[derive(Default)]
struct OwnedJourneyTrace {
    searches: Vec<serde_json::Value>,
    requests: Vec<Request>,
}

struct RecordingSearch(Arc<Mutex<OwnedJourneyTrace>>);

#[async_trait]
impl Tool for RecordingSearch {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "search",
            "search the tutorial handbook",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "source": {"type": "string"}
                },
                "required": ["query"]
            }),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        self.0.lock().unwrap().searches.push(params);
        Ok(ToolResult::ok(
            "Offline edits synchronize automatically when a device reconnects.",
        ))
    }
}

struct RecordingProvider(Arc<Mutex<OwnedJourneyTrace>>);

#[async_trait]
impl Provider for RecordingProvider {
    fn name(&self) -> &str {
        "capture"
    }

    async fn stream(&self, req: Request) -> flux_core::Result<ChunkStream> {
        self.0.lock().unwrap().requests.push(req);
        Ok(Box::pin(futures::stream::iter([
            Ok(Chunk::TextDelta(
                "Offline edits sync after the device reconnects.".into(),
            )),
            Ok(Chunk::Done {
                stop_reason: Some(StopReason::EndTurn),
            }),
        ])))
    }
}

#[tokio::test]
async fn owned_journey_inherits_model_persona_datasource_and_capabilities() {
    let src = r#"permissions
  allow [search, "ai.reason", send]
  deny [write, bash]

agent guide
  model "owned-model"
  tools [search]
  datasources [handbook]
  allow [search, "ai.reason", send]
  description "Answer only from the Northstar handbook."

channel cli

datasource handbook
  kind "markdown"
  path "./docs"

trigger questions
  on "user_input"
  run answer-question

journey answer-question
  agent guide
  flow
    $hits = search({"query": "{text}"})
    $answer = ai.reason({"ask": "Question: {text}\nHandbook results: {hits}"})
    send({"channel": "cli", "message": "{answer}"})
    return ""
"#;
    let trace = Arc::new(Mutex::new(OwnedJourneyTrace::default()));
    let provider: Arc<dyn Provider> = Arc::new(RecordingProvider(trace.clone()));
    let search: Arc<dyn Tool> = Arc::new(RecordingSearch(trace.clone()));
    let app = App::try_with_tools(
        program(src),
        Some(provider),
        "host-model",
        false,
        vec![search],
    )
    .expect("valid app");

    app.deliver(
        "user_input",
        json!({"text": "What happens to offline edits?"}),
    )
    .await
    .expect("journey");

    let trace = trace.lock().unwrap();
    assert_eq!(
        trace.searches.len(),
        1,
        "retrieval is structurally mandatory"
    );
    assert_eq!(trace.searches[0]["source"], "handbook");
    assert_eq!(trace.requests.len(), 1, "one authored cognition boundary");
    assert_eq!(trace.requests[0].model, "owned-model");
    let system = trace.requests[0]
        .system_text()
        .expect("cognition system prompt");
    assert!(system.contains("Answer only from the Northstar handbook."));
    assert!(system.contains("careful reasoning engine"));
    assert_eq!(
        app.bus().sent()[0].message,
        "Offline edits sync after the device reconnects."
    );
}

#[tokio::test]
async fn app_capability_ceiling_is_absolute_under_auto_approve() {
    let src = r#"permissions
  allow [send]

trigger t
  on "startup"
  run forbidden

journey forbidden
  flow
    return now()
"#;
    let app = App::with_options(program(src), None, "mock", true);
    let err = app
        .deliver("startup", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("now"), "unexpected error: {err}");
}

#[tokio::test]
async fn deny_only_policy_preserves_legacy_grants_until_auto_approved() {
    let src = r#"permissions
  deny [bash]

trigger t
  on "startup"
  run clock

journey clock
  flow
    return now()
"#;
    let denied = App::with_options(program(src), None, "mock", false)
        .deliver("startup", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(denied.contains("now"), "unexpected error: {denied}");

    let approved = App::with_options(program(src), None, "mock", true)
        .deliver("startup", json!({}))
        .await
        .expect("now remains inside the deny-only ceiling");
    assert!(!approved[0].result.is_empty());
}

#[tokio::test]
async fn host_permission_rules_apply_inside_but_never_widen_source_ceiling() {
    let app = App::try_with_events_and_permissions(
        program(
            r#"permissions
  allow [now]

trigger t
  on "startup"
  run clock

journey clock
  flow
    return now()
"#,
        ),
        None,
        "mock",
        true,
        Vec::new(),
        None,
        flux_secret::Redactor::new(),
        Arc::new(flux_events::EventStore::in_memory().unwrap()),
        flux_app::HostPermissionRules {
            allow: vec!["now".into()],
            deny: vec!["now".into()],
        },
    )
    .expect("valid app");
    let denied = app
        .deliver("startup", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(denied.contains("now"), "local deny did not win: {denied}");

    let widened = App::try_with_events_and_permissions(
        program(
            r#"permissions
  allow [send]

journey clock
  flow
    return now()
"#,
        ),
        None,
        "mock",
        false,
        Vec::new(),
        None,
        flux_secret::Redactor::new(),
        Arc::new(flux_events::EventStore::in_memory().unwrap()),
        flux_app::HostPermissionRules {
            allow: vec!["now".into()],
            deny: Vec::new(),
        },
    )
    .err()
    .expect("local allow must not widen app source")
    .to_string();
    assert!(
        widened.contains("now") && widened.contains("ceiling"),
        "{widened}"
    );
}

#[test]
fn fallible_app_construction_rejects_invalid_ownership_and_capabilities() {
    let unknown_owner = r#"journey answer
  agent missing
  flow
    return ""
"#;
    let err = App::try_new(program(unknown_owner), None, "mock")
        .err()
        .expect("invalid app")
        .to_string();
    assert!(err.contains("answer") && err.contains("missing"), "{err}");

    let outside_ceiling = r#"permissions
  allow [send]

journey answer
  flow
    return now()
"#;
    let err = App::try_new(program(outside_ceiling), None, "mock")
        .err()
        .expect("invalid app")
        .to_string();
    assert!(err.contains("answer") && err.contains("now"), "{err}");
}

#[test]
fn startup_validation_covers_tools_datasources_nested_calls_and_composites() {
    for (label, src, needles) in [
        (
            "datasource",
            "agent guide\n  tools []\n  datasources [missing]\n",
            &["guide", "missing"][..],
        ),
        (
            "tool",
            "agent guide\n  tools [not_registered]\n",
            &["guide", "not_registered"][..],
        ),
        (
            "permission",
            "permissions\n  allow [not_registered]\n",
            &["permissions", "not_registered"][..],
        ),
        (
            "nested call",
            "permissions\n  allow [send]\n\njourney nested\n  flow\n    when true\n      now()\n",
            &["nested", "now"][..],
        ),
        (
            "composite",
            "permissions\n  allow [wrapper]\n\nop wrapper()\n  now()\n\njourney answer\n  flow\n    return wrapper()\n",
            &["wrapper", "now"][..],
        ),
    ] {
        let err = App::try_new(program(src), None, "mock")
            .err()
            .unwrap_or_else(|| panic!("{label} should fail validation"))
            .to_string();
        for needle in needles {
            assert!(err.contains(needle), "{label} error omitted `{needle}`: {err}");
        }
    }
}

/// A hermetic program: a startup trigger runs a journey that `send`s on the cli channel and returns a
/// literal — entirely pure ops, no model.
const HELLO: &str = "\
channel cli

trigger t
  on \"startup\"
  run greet

journey greet
  flow
    send({ \"channel\": \"cli\", \"message\": \"Hello from flux-app!\" })
    return \"Hello from flux-app!\"
";

#[tokio::test]
async fn startup_trigger_runs_journey_and_records_send() {
    let app = App::new(program(HELLO), None, "test-model");

    let runs = app.deliver("startup", json!({})).await.unwrap();

    // The trigger matched, the journey ran, and its `return` is the journey result.
    assert_eq!(runs.len(), 1, "exactly the one matched journey ran");
    assert_eq!(runs[0].journey, "greet");
    assert_eq!(runs[0].result, "Hello from flux-app!");
    assert_eq!(runs[0].steps, 1, "the one `send` op dispatched");

    // The `send` op recorded the channel message (the test-observable proof the op ran).
    let sent = app.bus().sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].channel, "cli");
    assert_eq!(sent[0].message, "Hello from flux-app!");
    assert!(!sent[0].expects_reply);
}

#[tokio::test]
async fn unmatched_event_runs_nothing() {
    let app = App::new(program(HELLO), None, "test-model");
    let runs = app
        .deliver("user_input", json!({ "text": "hi" }))
        .await
        .unwrap();
    assert!(
        runs.is_empty(),
        "no trigger binds `user_input` in this program"
    );
    assert!(app.bus().sent().is_empty());
}

/// The event payload is seeded into the journey's session: a top-level field binds to its own symbol,
/// so `fmt("...{text}...")` and `$reply` resolve.
const ECHO: &str = "\
channel cli

trigger t
  on \"user_input\"
  run echo

journey echo
  flow
    $reply = fmt(\"you said: {text}\")
    send(\"cli\", $reply)
    return $reply
";

#[tokio::test]
async fn user_input_payload_is_seeded_and_echoed() {
    let app = App::new(program(ECHO), None, "test-model");

    let runs = app
        .deliver("user_input", json!({ "text": "ping" }))
        .await
        .unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].result, "you said: ping",
        "the {{text}} payload field reached the flow"
    );
    let sent = app.bus().sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].message, "you said: ping",
        "send received the $reply var, positionally mapped"
    );
}

/// A journey that `emit`s a second event whose trigger runs another journey — proving the bus cascade
/// inside one `deliver`.
const CASCADE: &str = "\
channel cli

trigger a
  on \"startup\"
  run first

trigger b
  on \"followup\"
  run second

journey first
  flow
    emit({ \"event\": \"followup\" })

journey second
  flow
    send({ \"channel\": \"cli\", \"message\": \"cascaded!\" })
";

#[tokio::test]
async fn emit_cascades_to_a_second_trigger_within_one_deliver() {
    let app = App::new(program(CASCADE), None, "test-model");

    let runs = app.deliver("startup", json!({})).await.unwrap();

    // Both the initial journey and the emit-triggered one ran.
    let names: Vec<&str> = runs.iter().map(|r| r.journey.as_str()).collect();
    assert!(
        names.contains(&"first"),
        "the startup journey ran: {names:?}"
    );
    assert!(
        names.contains(&"second"),
        "the emit cascaded into the followup journey: {names:?}"
    );
    let sent = app.bus().sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message, "cascaded!");
}

/// A parent journey that `spawn`s a child journey to completion and returns its result — proving the
/// `spawn` op re-enters the engine and runs a real journey through the same execution path.
const SPAWN: &str = "\
channel cli

trigger t
  on \"startup\"
  run parent

journey parent
  flow
    $out = spawn({ \"run\": \"child\" })
    return $out

journey child
  flow
    send({ \"channel\": \"cli\", \"message\": \"child ran\" })
    return \"child-result\"
";

#[tokio::test]
async fn spawn_runs_a_named_journey_and_returns_its_result() {
    let app = App::new(program(SPAWN), None, "test-model");

    let runs = app.deliver("startup", json!({})).await.unwrap();

    assert_eq!(runs.len(), 1, "only the parent matches startup");
    assert_eq!(runs[0].journey, "parent");
    assert_eq!(
        runs[0].result, "child-result",
        "parent returned the child's result via spawn"
    );
    // The child genuinely executed (its `send` was recorded).
    let sent = app.bus().sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message, "child ran");
}

/// A journey that `ask`s the cli channel and only completes once the reply arrives: the follow-up
/// `send` and the `return` reference the reply, so completion is observable (A-11 reply-parking).
const INTERVIEW: &str = "\
channel cli

trigger t
  on \"user_input\"
  run interview

journey interview
  flow
    $answer = ask({ \"channel\": \"cli\", \"message\": \"favourite colour?\" })
    send({ \"channel\": \"cli\", \"message\": fmt(\"you chose: {answer}\") })
    return $answer
";

#[tokio::test]
async fn ask_suspends_the_journey_until_a_reply() {
    let app = App::new(program(INTERVIEW), None, "test-model");

    let runs = app
        .deliver("user_input", json!({ "text": "start" }))
        .await
        .unwrap();

    // The journey ran up to the ask and PARKED: the question went out (expects_reply), but the
    // post-ask send did not run and the run carries no result yet.
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].journey, "interview");
    assert_eq!(
        runs[0].result, "",
        "a parked journey has no result yet, got: {:?}",
        runs[0].result
    );
    let sent = app.bus().sent();
    assert_eq!(sent.len(), 1, "only the question was sent: {sent:?}");
    assert_eq!(sent[0].message, "favourite colour?");
    assert!(sent[0].expects_reply);
}

#[tokio::test]
async fn delivered_reply_resumes_with_the_reply_text_bound() {
    let app = App::new(program(INTERVIEW), None, "test-model");
    app.deliver("user_input", json!({ "text": "start" }))
        .await
        .unwrap();

    // The next message on the asked (cli) channel is the reply: it resumes the parked journey with
    // the reply text bound as the ask's result — it does NOT start a second interview.
    let runs = app
        .deliver("user_input", json!({ "text": "blue" }))
        .await
        .unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].journey, "interview");
    assert_eq!(
        runs[0].result, "blue",
        "the flow's `return $answer` is the reply text"
    );
    let sent = app.bus().sent();
    assert_eq!(sent.len(), 2, "question + the post-reply send: {sent:?}");
    assert_eq!(
        sent[1].message, "you chose: blue",
        "the resumed body saw the bound reply"
    );
    // The park was consumed: only one question was ever asked.
    assert_eq!(sent.iter().filter(|m| m.expects_reply).count(), 1);
}

#[tokio::test]
async fn unrelated_message_does_not_resume_the_parked_journey() {
    // Same interview journey plus an unrelated trigger/journey on another event label.
    let src = format!(
        "{INTERVIEW}
trigger u
  on \"ping\"
  run other

journey other
  flow
    return \"pong\"
"
    );
    let app = App::new(program(&src), None, "test-model");
    app.deliver("user_input", json!({ "text": "start" }))
        .await
        .unwrap();

    // An event for a different label routes normally and leaves the park alone.
    let runs = app.deliver("ping", json!({})).await.unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].journey, "other");
    assert_eq!(runs[0].result, "pong");
    let sent = app.bus().sent();
    assert_eq!(
        sent.len(),
        1,
        "the parked journey did not advance on the unrelated event: {sent:?}"
    );

    // The park is still pending: the next cli-channel message resumes it.
    let runs = app
        .deliver("user_input", json!({ "text": "green" }))
        .await
        .unwrap();
    assert_eq!(runs[0].result, "green");
    assert_eq!(app.bus().sent()[1].message, "you chose: green");
}

#[tokio::test]
async fn registry_carries_the_orchestration_ops_and_builtins() {
    let app = App::new(program(HELLO), None, "test-model");
    let names = app.registry().names();
    for op in ["emit", "send", "ask", "spawn", "read", "bash"] {
        assert!(
            names.iter().any(|n| n == op),
            "registry is missing `{op}`: {names:?}"
        );
    }
}

#[test]
fn bundled_example_parses_as_a_program() {
    let src = include_str!("../examples/hello.flux");
    let p = program(src);
    assert!(p.triggers.iter().any(|t| t.on == "startup"));
    assert!(p.flow_named("greet").is_some());
    assert!(p.flow_named("echo").is_some());
}

/// The agent-driven support example exercises every typed declaration that actually runs — agent (with
/// its `search` tool + docs) + slack channel (with `secret` references) + a markdown datasource + an
/// agent-bound trigger — and secrets stay as unresolved markers until the host resolves them. The
/// trigger is agent-bound (no `run`), so a Slack mention wakes the model, not a fixed journey.
#[test]
fn support_bot_example_covers_the_module_surface() {
    let p = program(include_str!("../examples/support-bot.flux"));
    assert_eq!(p.agents[0].tools, vec!["search"]);
    assert_eq!(p.agents[0].datasources, vec!["docs"]);
    assert_eq!(p.channels[0].kind, "slack");
    assert_eq!(
        p.channels[0].settings["bot_token"],
        json!({ "$secret": "SLACK_BOT_TOKEN" }),
        "secrets are references, never inline plaintext"
    );
    assert_eq!(p.datasources[0].kind, "markdown");
    assert_eq!(p.datasources[0].path.as_deref(), Some("./docs"));
    assert_eq!(p.triggers[0].agent.as_deref(), Some("assistant"));
    assert!(
        p.triggers[0].run.is_empty(),
        "the trigger is agent-bound — no journey to run"
    );
}

// ---------------------------------------------------------------------------
// A-112 — per-delivery bus isolation.
//
// Deliveries used to be serialized by the single delivery supervisor: one root ran to completion,
// cascade and all, before the next was dequeued. The tests below pin the concurrent contract —
// each delivery sees only its own cascade, and a blocked delivery does not hold the queue.
// ---------------------------------------------------------------------------

/// A rendezvous a journey can block on, so a test can prove one delivery is genuinely *in flight*
/// while another one runs — deterministically, with no wall-clock sleeps. `hold` parks the calling
/// journey until some other journey calls `release`.
#[derive(Default)]
struct Gate {
    entered: tokio::sync::Notify,
    open: tokio::sync::Notify,
}

struct HoldOp(Arc<Gate>);

#[async_trait]
impl Tool for HoldOp {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "hold",
            "block this journey until another journey releases the gate",
            json!({ "type": "object", "properties": { "at": { "type": "string" } } }),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        self.0.entered.notify_one();
        self.0.open.notified().await;
        Ok(ToolResult::ok("held"))
    }
}

struct ReleaseOp(Arc<Gate>);

#[async_trait]
impl Tool for ReleaseOp {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "release",
            "release a journey blocked on the gate",
            json!({ "type": "object", "properties": { "at": { "type": "string" } } }),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        self.0.open.notify_one();
        Ok(ToolResult::ok("released"))
    }
}

/// A generous deadlock backstop. Nothing in these tests waits on the clock for its *assertion* —
/// the timeout only turns "serialized forever" into a readable failure instead of a hung test.
const DEADLOCK_BACKSTOP: std::time::Duration = std::time::Duration::from_secs(30);

fn gate_ops(gate: &Arc<Gate>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(HoldOp(gate.clone())) as Arc<dyn Tool>,
        Arc::new(ReleaseOp(gate.clone())) as Arc<dyn Tool>,
    ]
}

/// Two labels, two independent cascade trees, one blocking rendezvous forcing them to overlap.
const CONCURRENT_CASCADES: &str = r#"permissions
  allow [emit, send, hold, release]

channel cli

trigger t_alpha
  on "alpha"
  run alpha

trigger t_alpha_cascade
  on "alpha_cascade"
  run alpha-followup

trigger t_beta
  on "beta"
  run beta

trigger t_beta_cascade
  on "beta_cascade"
  run beta-followup

journey alpha
  flow
    emit({ "event": "alpha_cascade" })
    hold({ "at": "gate" })

journey alpha-followup
  flow
    send({ "channel": "cli", "message": "alpha cascade ran" })

journey beta
  flow
    emit({ "event": "beta_cascade" })
    release({ "at": "gate" })

journey beta-followup
  flow
    send({ "channel": "cli", "message": "beta cascade ran" })
"#;

#[tokio::test]
async fn concurrent_deliveries_each_collect_only_their_own_cascade() {
    let gate = Arc::new(Gate::default());
    let app = Arc::new(
        App::try_with_tools(
            program(CONCURRENT_CASCADES),
            None,
            "test-model",
            false,
            gate_ops(&gate),
        )
        .expect("valid app"),
    );

    let alpha = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("alpha", json!({})).await }
    });
    // `alpha` has emitted its cascade event and is now parked mid-delivery.
    tokio::time::timeout(DEADLOCK_BACKSTOP, gate.entered.notified())
        .await
        .expect("the alpha delivery reached its blocking op");

    let beta = tokio::time::timeout(DEADLOCK_BACKSTOP, app.deliver("beta", json!({})))
        .await
        .expect("a second delivery must not queue behind an in-flight one")
        .expect("beta delivery");
    let alpha = tokio::time::timeout(DEADLOCK_BACKSTOP, alpha)
        .await
        .expect("the alpha delivery completed once released")
        .expect("alpha task")
        .expect("alpha delivery");

    let alpha_names: Vec<&str> = alpha.iter().map(|r| r.journey.as_str()).collect();
    let beta_names: Vec<&str> = beta.iter().map(|r| r.journey.as_str()).collect();
    assert_eq!(
        alpha_names,
        vec!["alpha", "alpha-followup"],
        "alpha collected exactly its own root and cascade"
    );
    assert_eq!(
        beta_names,
        vec!["beta", "beta-followup"],
        "beta collected exactly its own root and cascade"
    );
    assert!(
        alpha_names.iter().all(|name| !beta_names.contains(name)),
        "no journey run appears in both results: {alpha_names:?} / {beta_names:?}"
    );

    // Each cascade event was processed exactly once overall — no broadcast double-processing.
    let messages: Vec<String> = app.bus().sent().into_iter().map(|s| s.message).collect();
    assert_eq!(
        messages
            .iter()
            .filter(|m| *m == "alpha cascade ran")
            .count(),
        1,
        "alpha's cascade ran once: {messages:?}"
    );
    assert_eq!(
        messages.iter().filter(|m| *m == "beta cascade ran").count(),
        1,
        "beta's cascade ran once: {messages:?}"
    );
}

/// A sweep-shaped journey (long-running, blocking) and a short one that must overtake it.
const SWEEP_AND_INTAKE: &str = r#"permissions
  allow [send, hold, release]

channel cli

trigger t_sweep
  on "sweep"
  run sweep

trigger t_intake
  on "intake"
  run intake

journey sweep
  flow
    hold({ "at": "gate" })
    send({ "channel": "cli", "message": "sweep finished" })

journey intake
  flow
    release({ "at": "gate" })
    send({ "channel": "cli", "message": "intake finished" })
"#;

#[tokio::test]
async fn a_long_running_delivery_does_not_delay_the_next_one() {
    let gate = Arc::new(Gate::default());
    let app = Arc::new(
        App::try_with_tools(
            program(SWEEP_AND_INTAKE),
            None,
            "test-model",
            false,
            gate_ops(&gate),
        )
        .expect("valid app"),
    );

    let sweep = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("sweep", json!({})).await }
    });
    tokio::time::timeout(DEADLOCK_BACKSTOP, gate.entered.notified())
        .await
        .expect("the sweep delivery reached its blocking op");

    // The intake delivery is submitted while the sweep is still running, and it is the sweep that
    // depends on the intake — not the other way round. Ordering, not elapsed time, is the assertion.
    tokio::time::timeout(DEADLOCK_BACKSTOP, app.deliver("intake", json!({})))
        .await
        .expect("intake must run while the sweep is still in flight")
        .expect("intake delivery");
    tokio::time::timeout(DEADLOCK_BACKSTOP, sweep)
        .await
        .expect("the sweep completed once the intake released it")
        .expect("sweep task")
        .expect("sweep delivery");

    let messages: Vec<String> = app.bus().sent().into_iter().map(|s| s.message).collect();
    assert_eq!(
        messages,
        vec!["intake finished", "sweep finished"],
        "the second delivery completed while the first was still blocked"
    );
}

/// One journey whose `emit` re-triggers itself: the per-delivery cascade bound is what stops it.
const SELF_CASCADE: &str = r#"channel cli

trigger t
  on "tick"
  run tick

journey tick
  flow
    emit({ "event": "tick" })
"#;

#[tokio::test]
async fn a_self_feeding_cascade_stays_bounded_within_one_delivery() {
    let app = App::new(program(SELF_CASCADE), None, "test-model");

    let runs = app.deliver("tick", json!({})).await.expect("delivery");

    // `MAX_CASCADE` (256) roots per synchronous delivery — the bound is per delivery, and it holds.
    assert_eq!(
        runs.len(),
        256,
        "the cascade tree stayed bounded inside one delivery"
    );
    assert!(runs.iter().all(|run| run.journey == "tick"));
}

/// Every journey blocks on a shared barrier: the wave only completes if all of the deliveries are
/// genuinely in flight at the same time.
const WAVE: &str = r#"permissions
  allow [send, rendezvous]

channel cli

trigger t
  on "wave"
  run wave

journey wave
  flow
    rendezvous({ "at": "wave" })
    send({ "channel": "cli", "message": "wave done" })
"#;

/// Wider than `MAX_SPAWN_DEPTH` (16): concurrent deliveries must not consume one another's nesting
/// budget, only their own.
const WAVE_WIDTH: usize = 24;

struct RendezvousOp(Arc<tokio::sync::Barrier>);

#[async_trait]
impl Tool for RendezvousOp {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "rendezvous",
            "wait until every journey in the wave has arrived",
            json!({ "type": "object", "properties": { "at": { "type": "string" } } }),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        self.0.wait().await;
        Ok(ToolResult::ok("arrived"))
    }
}

#[tokio::test]
async fn a_wave_of_deliveries_does_not_share_one_nesting_budget() {
    let barrier = Arc::new(tokio::sync::Barrier::new(WAVE_WIDTH));
    let rendezvous: Arc<dyn Tool> = Arc::new(RendezvousOp(barrier));
    let app = Arc::new(
        App::try_with_tools(program(WAVE), None, "test-model", false, vec![rendezvous])
            .expect("valid app"),
    );

    let wave: Vec<_> = (0..WAVE_WIDTH)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move { app.deliver("wave", json!({})).await })
        })
        .collect();
    for handle in wave {
        tokio::time::timeout(DEADLOCK_BACKSTOP, handle)
            .await
            .expect("every delivery in the wave ran concurrently")
            .expect("wave task")
            .expect("wave delivery");
    }

    assert_eq!(app.bus().sent().len(), WAVE_WIDTH);
}
