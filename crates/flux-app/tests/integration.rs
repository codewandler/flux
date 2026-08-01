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
///
/// L-123: `send` is written in the canonical named-object form. It used to read
/// `send("cli", $reply)` — the **deprecated** 2+-positional form that `map_args_to_input` keeps
/// alive only "so a legacy stored plan still executes" (`flux-lang/src/runtime.rs`), and that
/// `analyze_flow` has always rejected for new plans. It survived here purely because the journey
/// path was the one authored-flow door with no analyzer pass; `flux flow run` rejects the same
/// line today via `lower` (`flux-cli/src/flow_cmd.rs`). Both shipped journey examples
/// (`crates/flux-app/examples/hello.flux`, `examples/channels-app.flux`) already use this form.
const ECHO: &str = "\
channel cli

trigger t
  on \"user_input\"
  run echo

journey echo
  flow
    $reply = fmt(\"you said: {text}\")
    send({ \"channel\": \"cli\", \"message\": $reply })
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
        "send received the $reply var under its named `message` parameter"
    );
}

/// A journey whose body names an operation no catalog entry resolves.
const UNKNOWN_OP: &str = "\
channel cli

trigger t
  on \"startup\"
  run bad

journey bad
  flow
    send({ \"channel\": \"cli\", \"message\": \"before\" })
    no_such_op()
    return \"done\"
";

/// L-123: a journey body is authored Flux-Lang this engine did not produce, so it gets the same
/// static gate `flux flow run` and `fork --edit` do. Before the gate a journey ran ungated: the
/// statements ahead of the bad one dispatched their side effects, and only then did the interpreter
/// halt on the unresolvable op.
#[tokio::test]
async fn a_journey_naming_an_unregistered_op_is_refused_before_anything_runs() {
    let app = App::new(program(UNKNOWN_OP), None, "test-model");

    let err = app
        .deliver("startup", json!({}))
        .await
        .expect_err("a journey naming an unregistered op must be refused, not executed");

    let msg = err.to_string();
    assert!(
        msg.contains("no_such_op"),
        "the refusal names the offending op: {msg}"
    );
    assert!(
        app.bus().sent().is_empty(),
        "the refusal lands BEFORE the first statement — the preceding `send` never dispatched"
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

// ---------------------------------------------------------------------------
// A-129 — bounding the delivery concurrency A-112 introduced.
//
// A-112 made the supervisor loop dequeue-and-spawn, which removed the only backpressure in the
// system: the `mpsc` capacity bounded submissions only because the loop was slow to drain it. These
// tests pin the replacement — a slot bound applied *before* the spawn, at which a delivery WAITS
// (it is never dropped and never rejected), plus the load snapshot that tells a waiting delivery
// apart from a slow one.
// ---------------------------------------------------------------------------

/// Counts how many journeys sit inside the op simultaneously and holds every one of them there
/// until the test opens the gate. `peak` is the highest simultaneous occupancy ever observed, so
/// the bound is asserted by construction rather than by elapsed time.
///
/// The gate is a `watch` rather than a `Notify`: it is level-triggered, so a journey that arrives
/// *after* the test opened it passes straight through instead of missing the edge and hanging.
struct Census {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    open: tokio::sync::watch::Sender<bool>,
}

impl Census {
    fn new() -> Self {
        Self {
            active: std::sync::atomic::AtomicUsize::new(0),
            peak: std::sync::atomic::AtomicUsize::new(0),
            open: tokio::sync::watch::channel(false).0,
        }
    }

    fn active(&self) -> usize {
        self.active.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn peak(&self) -> usize {
        self.peak.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn release(&self) {
        let _ = self.open.send(true);
    }
}

struct CensusOp(Arc<Census>);

#[async_trait]
impl Tool for CensusOp {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "census",
            "record simultaneous occupancy, then block until the test releases the gate",
            json!({ "type": "object", "properties": { "at": { "type": "string" } } }),
        )
    }

    async fn execute(
        &self,
        _ctx: &ToolContext,
        _params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        use std::sync::atomic::Ordering::SeqCst;
        let now = self.0.active.fetch_add(1, SeqCst) + 1;
        self.0.peak.fetch_max(now, SeqCst);
        let mut open = self.0.open.subscribe();
        loop {
            if *open.borrow_and_update() {
                break;
            }
            if open.changed().await.is_err() {
                break;
            }
        }
        self.0.active.fetch_sub(1, SeqCst);
        Ok(ToolResult::ok("counted"))
    }
}

const CENSUS: &str = r#"permissions
  allow [census]

channel cli

trigger t
  on "work"
  run work

journey work
  flow
    census({ "at": "bound" })
"#;

/// A submission wave comfortably wider than the *default* bound (64), so the storm test below needs
/// no configuration API and therefore still compiles against the pre-A-129 tree.
const STORM: usize = 80;

/// Yield until simultaneous occupancy of the op has held one value long enough that every other
/// task has plainly had its chance, then report it.
///
/// Quiescence rather than a positive edge, because the storm test's job is to *discover* the ceiling
/// rather than assert a known one. On the single-threaded test runtime each yield hands control to
/// the spawned journeys, so a few thousand of them give every one of `STORM` tasks thousands of
/// chances to reach the op; and once a bound exists the settled state is genuinely stable, because
/// the journeys it holds back cannot enter while the gate is shut. Zero never counts as settled —
/// the steady state is necessarily non-empty, so treating it as "not yet" removes the only way this
/// could return early.
async fn settled_occupancy(census: &Census) -> usize {
    const STEADY_YIELDS: usize = 5_000;
    let wait = async {
        let (mut last, mut stable) = (usize::MAX, 0);
        loop {
            let now = census.active();
            if now > 0 && now == last {
                stable += 1;
                if stable >= STEADY_YIELDS {
                    return now;
                }
            } else {
                last = now;
                stable = 0;
            }
            tokio::task::yield_now().await;
        }
    };
    tokio::time::timeout(DEADLOCK_BACKSTOP, wait)
        .await
        .expect("occupancy never settled: no journey ever reached the census op")
}

/// The failing-first proof for A-129: a storm wider than the default bound must not put a journey
/// into the op for every event at once.
///
/// This is the one test here that names no admission API — it runs on the default bound and asserts
/// only `occupancy < STORM`. That is deliberate on both counts. It compiles against A-112's tree, so
/// the defect is reproducible as a *behavioural* failure rather than a missing symbol; and being
/// bound-agnostic it states the defect itself — a dequeue-and-spawn loop applies no backpressure
/// whatsoever — rather than the particular ceiling chosen to fix it, so it keeps its meaning if the
/// default ever moves.
#[tokio::test]
async fn a_delivery_storm_does_not_spawn_a_journey_for_every_event_at_once() {
    let census = Arc::new(Census::new());
    let app = Arc::new(
        App::try_with_tools(
            program(CENSUS),
            None,
            "test-model",
            false,
            vec![Arc::new(CensusOp(census.clone())) as Arc<dyn Tool>],
        )
        .expect("valid app"),
    );

    let wave: Vec<_> = (0..STORM)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move { app.deliver("work", json!({})).await })
        })
        .collect();

    let occupancy = settled_occupancy(&census).await;
    assert!(
        occupancy < STORM,
        "all {STORM} deliveries were running at once: the supervisor's dequeue-and-spawn loop \
         applied no backpressure, so a webhook storm spawns a journey per event"
    );

    // Bounded, not dropped: the deliveries the bound held back still run once slots free up, and
    // every submitter gets its own runs back.
    census.release();
    for handle in wave {
        tokio::time::timeout(DEADLOCK_BACKSTOP, handle)
            .await
            .expect("every held delivery still completed once slots freed up")
            .expect("delivery task")
            .expect("delivery");
    }
    assert_eq!(
        census.peak(),
        occupancy,
        "the ceiling held for the whole storm, not just while it was saturated"
    );
    assert_eq!(census.active(), 0, "the storm drained completely");
}

/// Spin until `condition` holds of the App's admission snapshot. Every state these tests wait for is
/// a *stable* one — the journeys are blocked on a gate only the test opens — so this converges and
/// then stops changing; the backstop only turns a broken bound into a readable failure.
async fn await_load(
    app: &App,
    label: &str,
    condition: impl Fn(flux_app::DeliveryLoad) -> bool,
) -> flux_app::DeliveryLoad {
    let wait = async {
        loop {
            let load = app.delivery_load();
            if condition(load) {
                return load;
            }
            tokio::task::yield_now().await;
        }
    };
    match tokio::time::timeout(DEADLOCK_BACKSTOP, wait).await {
        Ok(load) => load,
        Err(_) => panic!(
            "delivery load never reached {label}; last was {:?}",
            app.delivery_load()
        ),
    }
}

/// The bound under test, and a submission wave several times wider than it.
const BOUND: usize = 4;
const SUBMITTED: usize = 12;

#[tokio::test]
async fn deliveries_beyond_the_bound_wait_rather_than_running() {
    let census = Arc::new(Census::new());
    let app = Arc::new(
        App::try_with_tools(
            program(CENSUS),
            None,
            "test-model",
            false,
            vec![Arc::new(CensusOp(census.clone())) as Arc<dyn Tool>],
        )
        .expect("valid app")
        .with_max_inflight_deliveries(BOUND),
    );

    let wave: Vec<_> = (0..SUBMITTED)
        .map(|_| {
            let app = app.clone();
            tokio::spawn(async move { app.deliver("work", json!({})).await })
        })
        .collect();

    // The steady state is fully determined: every admitted journey is parked in `census`, and the
    // rest cannot start. Waiting for it is a positive edge, not a sleep.
    let load = await_load(&app, "saturation", |load| {
        load.in_flight == BOUND && load.waiting == SUBMITTED - BOUND
    })
    .await;
    assert_eq!(load.limit, BOUND);
    await_load(&app, "every admitted journey inside the op", |_| {
        census.active() == BOUND
    })
    .await;
    assert_eq!(
        census.active(),
        BOUND,
        "exactly the bound ran; the other {} are held, not lost",
        SUBMITTED - BOUND
    );

    census.release();
    for handle in wave {
        tokio::time::timeout(DEADLOCK_BACKSTOP, handle)
            .await
            .expect("every submitted delivery completed once the gate opened")
            .expect("delivery task")
            .expect("delivery");
    }

    assert_eq!(
        census.peak(),
        BOUND,
        "concurrency never exceeded the bound across the whole wave"
    );
    assert_eq!(
        app.bus().sent().len(),
        0,
        "the census program sends nothing; all {SUBMITTED} completions came back through deliver"
    );
    let drained = await_load(&app, "drained", |load| {
        load.in_flight == 0 && load.waiting == 0
    })
    .await;
    assert!(!drained.is_backpressured());
}

#[tokio::test]
async fn a_delivery_waiting_on_the_bound_is_distinguishable_from_a_slow_one() {
    let census = Arc::new(Census::new());
    let app = Arc::new(
        App::try_with_tools(
            program(CENSUS),
            None,
            "test-model",
            false,
            vec![Arc::new(CensusOp(census.clone())) as Arc<dyn Tool>],
        )
        .expect("valid app")
        .with_max_inflight_deliveries(1),
    );

    // One delivery, admitted, running slowly: busy but not backpressured.
    let slow = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("work", json!({})).await }
    });
    let load = await_load(&app, "one slow delivery", |load| load.in_flight == 1).await;
    assert_eq!(
        (load.in_flight, load.waiting, load.limit),
        (1, 0, 1),
        "a slow delivery is in flight with nothing queued behind it"
    );
    assert!(
        !load.is_backpressured(),
        "slow work is not backpressure: {load:?}"
    );

    // A second delivery, held by the bound: same latency to its caller, different cause.
    let held = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("work", json!({})).await }
    });
    let load = await_load(&app, "one delivery held by the bound", |load| {
        load.waiting == 1
    })
    .await;
    assert_eq!(
        (load.in_flight, load.waiting, load.limit),
        (1, 1, 1),
        "the second delivery is waiting on the bound, not running"
    );
    assert!(
        load.is_backpressured(),
        "a delivery held by the bound reports backpressure: {load:?}"
    );

    census.release();
    for handle in [slow, held] {
        tokio::time::timeout(DEADLOCK_BACKSTOP, handle)
            .await
            .expect("both deliveries completed")
            .expect("delivery task")
            .expect("delivery");
    }
    assert_eq!(
        census.peak(),
        1,
        "a bound of one really did serialize the two deliveries"
    );
}

#[tokio::test]
async fn the_bound_does_not_serialize_unrelated_channels_while_slots_remain() {
    // A-112's Acceptance, re-proved with the bound installed and only one spare slot: the sweep
    // occupies one, the intake takes the other and overtakes it. A bound that had reintroduced
    // head-of-line blocking would deadlock here, because it is the intake that frees the sweep.
    let gate = Arc::new(Gate::default());
    let app = Arc::new(
        App::try_with_tools(
            program(SWEEP_AND_INTAKE),
            None,
            "test-model",
            false,
            gate_ops(&gate),
        )
        .expect("valid app")
        .with_max_inflight_deliveries(2),
    );

    let sweep = tokio::spawn({
        let app = app.clone();
        async move { app.deliver("sweep", json!({})).await }
    });
    tokio::time::timeout(DEADLOCK_BACKSTOP, gate.entered.notified())
        .await
        .expect("the sweep delivery reached its blocking op");

    tokio::time::timeout(DEADLOCK_BACKSTOP, app.deliver("intake", json!({})))
        .await
        .expect("intake must run beside the sweep, not behind it")
        .expect("intake delivery");
    tokio::time::timeout(DEADLOCK_BACKSTOP, sweep)
        .await
        .expect("the sweep completed once the intake released it")
        .expect("sweep task")
        .expect("sweep delivery");

    assert_eq!(
        app.bus()
            .sent()
            .into_iter()
            .map(|s| s.message)
            .collect::<Vec<_>>(),
        vec!["intake finished", "sweep finished"],
        "the bound left unrelated channels parallel"
    );
}

#[tokio::test]
async fn the_default_bound_is_documented_and_wider_than_flux_s_own_fan_out() {
    // A const block: both sides are compile-time constants, so this is a static guarantee rather
    // than a runtime check. The message takes no format args because a formatted panic is not const.
    const {
        assert!(
            flux_app::DEFAULT_MAX_INFLIGHT_DELIVERIES > WAVE_WIDTH,
            "the default bound must not engage on flux's own widest fan-out (A-112's delivery wave)"
        )
    };
    assert_eq!(
        flux_app::MAX_INFLIGHT_DELIVERIES_ENV,
        "FLUX_MAX_INFLIGHT_DELIVERIES"
    );

    // An App that has never delivered still reports the bound it would start with.
    if std::env::var_os(flux_app::MAX_INFLIGHT_DELIVERIES_ENV).is_none() {
        let app = App::new(program(CENSUS), None, "test-model");
        assert_eq!(
            app.delivery_load(),
            flux_app::DeliveryLoad {
                in_flight: 0,
                waiting: 0,
                limit: flux_app::DEFAULT_MAX_INFLIGHT_DELIVERIES,
            }
        );
    }
}

// ---------------------------------------------------------------------------
// C-415 — the journey half of the room identity gap
// ---------------------------------------------------------------------------

/// What one op dispatch saw about the principal it ran as.
#[derive(Debug, Clone)]
struct SeenCaller {
    /// The tag the flow passed, so a parent and a spawned child are distinguishable.
    tag: String,
    /// `tool_call.caller` — the id the **safety envelope itself** authorized and audited this
    /// dispatch under. `Executor::dispatch` writes that observation before it calls `execute`, so
    /// reading it back from inside the tool reads the record the gate just made, not a restatement.
    audited: String,
    /// The request-owned [`flux_runtime::TurnIdentity`] frozen for the turn, when one was installed
    /// — `None` means the dispatch fell back to the executor's assembly-time identity.
    turn_principal: Option<String>,
    /// That identity's trust level, rendered.
    turn_trust: Option<String>,
}

/// A read-only op whose whole purpose is to report the identity its own dispatch ran under.
struct IdentityProbe(Arc<Mutex<Vec<SeenCaller>>>);

#[async_trait]
impl Tool for IdentityProbe {
    fn spec(&self) -> flux_spec::ToolSpec {
        flux_spec::ToolSpec::read_only(
            "whoami",
            "report the caller identity this dispatch ran under",
            json!({
                "type": "object",
                "properties": { "tag": { "type": "string" } },
                "required": ["tag"]
            }),
        )
    }

    async fn execute(
        &self,
        ctx: &ToolContext,
        params: serde_json::Value,
    ) -> flux_core::Result<ToolResult> {
        let audited = ctx
            .evidence
            .lock()
            .unwrap()
            .by_kind("tool_call")
            .last()
            .and_then(|o| o.data.get("caller").and_then(|c| c.as_str()))
            .unwrap_or("<no tool_call observation>")
            .to_string();
        let identity = ctx.turn_identity();
        self.0.lock().unwrap().push(SeenCaller {
            tag: params["tag"].as_str().unwrap_or_default().to_string(),
            audited,
            turn_principal: identity.as_ref().map(|i| i.caller().principal.id.clone()),
            turn_trust: identity.as_ref().map(|i| format!("{:?}", i.trust().level)),
        });
        Ok(ToolResult::ok("ok"))
    }
}

const ADA: &str = "standup@rooms.example/ada";
const MALLORY: &str = "standup@rooms.example/mallory";

/// Every `journey.identity` attribution the app recorded durably, in order — the operator's
/// after-the-fact view of who caused a journey's effects.
///
/// The stream name is written out rather than read from `flux_app::JOURNEY_AUDIT_STREAM` on
/// purpose: it keeps this whole file compiling against the merge base, so the failing-first run is
/// reproducible. It pins the same thing either way — the value the published const carries is what
/// `record_journey_identity` writes to, so changing one without the other reds this.
fn recorded_attributions(app: &App) -> Vec<serde_json::Value> {
    app.events()
        .observations("journey-audit")
        .expect("the journey audit stream")
        .into_iter()
        .filter(|o| o.kind == "journey.identity")
        .map(|o| o.data)
        .collect()
}

/// C-415 (the journey half of F2 of the 2026-08-01 security-posture review). C-408 gave the *agent*
/// path a request-owned identity; `run_journey` still built its `RuntimeTurnContext` with no
/// `.with_identity(..)`, so a room-triggered **journey** authorized and audited every op as the
/// assembly-time `local` operator at `Privileged`. A room is the most multi-principal surface flux
/// has, and `docs/designs/meeting-rooms.md` says a room event wakes a journey *or* an agent — so
/// closing only the agent half left the identity invariant (`AGENTS.md`) open on the other.
#[tokio::test]
async fn a_room_triggered_journeys_op_authorizes_and_audits_as_the_speaker() {
    const SRC: &str = "\
permissions
  allow [whoami]

trigger t
  on \"standup\"
  run report

journey report
  flow
    $who = whoami({ \"tag\": \"journey\" })
    return \"{who}\"
";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe: Arc<dyn Tool> = Arc::new(IdentityProbe(seen.clone()));
    let app = App::try_with_tools(program(SRC), None, "test-model", false, vec![probe])
        .expect("valid app");

    app.deliver(
        "standup",
        json!({
            "room": "standup@rooms.example",
            "speaker": ADA,
            // Non-unique by construction in a MUC — never the thing an identity is derived from.
            "nick": "ada",
            "text": "what is the status?",
            "name": "standup",
        }),
    )
    .await
    .expect("deliver");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        1,
        "the journey dispatched its op once: {seen:?}"
    );
    assert_eq!(
        seen[0].audited, ADA,
        "the op the journey dispatched must authorize and audit as the SPEAKER, not as the local \
         operator: {seen:?}"
    );
    assert_eq!(seen[0].turn_principal.as_deref(), Some(ADA), "{seen:?}");
    assert_eq!(
        seen[0].turn_trust.as_deref(),
        Some("Untrusted"),
        "a room occupant presented no credential — C-408's decision, reused: {seen:?}"
    );

    // Where the attribution is RECORDED. A journey has no engine turn and therefore no
    // `turn.identity` observation, and its executor's evidence log dies with the run — so the run
    // writes one `journey.identity` observation into the app's durable event store, on the journey
    // run's own stream.
    let recorded = recorded_attributions(&app);
    assert_eq!(
        recorded.len(),
        1,
        "one attribution per journey run: {recorded:?}"
    );
    assert_eq!(recorded[0]["journey"], json!("report"), "{recorded:?}");
    assert_eq!(recorded[0]["caller"], json!(ADA), "{recorded:?}");
    assert_eq!(recorded[0]["source"], json!("room"), "{recorded:?}");
    assert_eq!(
        recorded[0]["attribution"],
        json!("delivery"),
        "{recorded:?}"
    );
    assert_eq!(
        recorded[0]["trust"]["level"],
        json!("untrusted"),
        "{recorded:?}"
    );
}

/// The other half of C-415's contract, and the same pin C-408 put on the agent path: only a
/// delivery that *names* a principal gets a request-owned identity. A schedule tick names nobody, so
/// its journey keeps the executor's immutable assembly-time identity — `local` at `Privileged` —
/// exactly as before. Without this, "derive an identity from the payload" could quietly become
/// "derive one from every payload", and a `startup` trigger would start reporting a principal nobody
/// asserted.
#[tokio::test]
async fn a_journey_for_an_event_that_names_no_principal_keeps_the_assembly_time_identity() {
    const SRC: &str = "\
permissions
  allow [whoami]

trigger tick
  on \"schedule\"
  run sweep

journey sweep
  flow
    $who = whoami({ \"tag\": \"tick\" })
    return \"{who}\"
";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe: Arc<dyn Tool> = Arc::new(IdentityProbe(seen.clone()));
    let app = App::try_with_tools(program(SRC), None, "test-model", false, vec![probe])
        .expect("valid app");

    app.deliver("schedule", json!({ "at": "2026-08-01T09:00:00Z" }))
        .await
        .expect("deliver");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].audited, "local", "{seen:?}");
    assert_eq!(
        seen[0].turn_principal, None,
        "no principal was named, so none is installed: {seen:?}"
    );

    // The record says so out loud rather than by omission: an operator reading this back learns
    // that nobody but the operator was ever named, not merely that a field is missing.
    let recorded = recorded_attributions(&app);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(recorded[0]["caller"], json!("local"), "{recorded:?}");
    assert_eq!(
        recorded[0]["attribution"],
        json!("assembly"),
        "{recorded:?}"
    );
    assert_eq!(
        recorded[0]["trust"]["level"],
        json!("privileged"),
        "{recorded:?}"
    );
}

/// A park is a pause in one logical turn, not the start of a new one — so the continuation
/// authorizes and audits as the principal the run *started* as (C-415).
///
/// Two things would be wrong here and this pins both. Falling back to the executor's assembly-time
/// identity would mean a room stranger's journey finishes as the local operator merely because it
/// asked a question — the exact defect C-415 removes, reintroduced across the suspension. Adopting
/// the *replier's* speaker instead would be an outer-surface swap of a live turn's caller, which the
/// identity invariant forbids outright; so Mallory answering Ada's question does not make the rest
/// of Ada's journey run as Mallory.
#[tokio::test]
async fn a_parked_room_journey_resumes_as_the_speaker_that_started_it() {
    const SRC: &str = "\
channel cli

permissions
  allow [whoami, ask]

trigger t
  on \"standup\"
  run interview

journey interview
  flow
    $answer = ask({ \"channel\": \"cli\", \"message\": \"status?\" })
    $who = whoami({ \"tag\": \"after-reply\" })
    return \"{answer}\"
";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe: Arc<dyn Tool> = Arc::new(IdentityProbe(seen.clone()));
    let app = App::try_with_tools(program(SRC), None, "test-model", false, vec![probe])
        .expect("valid app");

    app.deliver(
        "standup",
        json!({ "room": "standup@rooms.example", "speaker": ADA, "text": "start" }),
    )
    .await
    .expect("deliver");
    assert!(
        seen.lock().unwrap().is_empty(),
        "the journey parked before reaching the probe"
    );

    // A DIFFERENT occupant answers.
    app.deliver(
        "cli",
        json!({ "room": "standup@rooms.example", "speaker": MALLORY, "text": "green" }),
    )
    .await
    .expect("reply");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "the continuation ran: {seen:?}");
    assert_ne!(
        seen[0].audited, "local",
        "asking a question must not hand the continuation back to the operator: {seen:?}"
    );
    assert_ne!(
        seen[0].audited, MALLORY,
        "the replier does not become the running turn's caller: {seen:?}"
    );
    assert_eq!(seen[0].audited, ADA, "{seen:?}");

    let recorded = recorded_attributions(&app);
    let resumed = recorded
        .iter()
        .find(|r| r["attribution"] == json!("resumed"))
        .unwrap_or_else(|| panic!("the resumed segment is attributed too: {recorded:?}"));
    assert_eq!(resumed["caller"], json!(ADA), "{recorded:?}");
}

/// C-415's hard half, and the reason it is a story of its own: `run_journey` is ALSO reached from
/// `run_journey_for_spawn` — with a payload the **model authored**. If a spawn-sourced delivery
/// derived its principal from its payload the way a channel delivery does, a model could name any
/// principal it liked and the record would believe it.
///
/// The rule this pins: a spawn-sourced journey **never derives** an identity from its payload; it
/// inherits the enclosing turn's, which is by construction the turn that spawned it. So the forged
/// `speaker` below is inert, and the child runs as the same untrusted stranger the parent did —
/// never a different principal, never a stronger one.
#[tokio::test]
async fn a_spawned_journey_inherits_the_spawning_turn_and_cannot_be_told_who_it_is() {
    const SRC: &str = "\
permissions
  allow [whoami, spawn]

trigger t
  on \"standup\"
  run parent

journey parent
  flow
    $me = whoami({ \"tag\": \"parent\" })
    $out = spawn({ \"run\": \"child\", \"input\": { \"room\": \"standup@rooms.example\", \"speaker\": \"standup@rooms.example/mallory\" } })
    return \"{out}\"

journey child
  flow
    $who = whoami({ \"tag\": \"child\" })
    return \"{who}\"
";
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe: Arc<dyn Tool> = Arc::new(IdentityProbe(seen.clone()));
    let app = App::try_with_tools(program(SRC), None, "test-model", false, vec![probe])
        .expect("valid app");

    app.deliver(
        "standup",
        json!({
            "room": "standup@rooms.example",
            "speaker": ADA,
            "text": "run the report",
            "name": "standup",
        }),
    )
    .await
    .expect("deliver");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "parent then child: {seen:?}");
    let parent = seen.iter().find(|s| s.tag == "parent").expect("parent op");
    let child = seen.iter().find(|s| s.tag == "child").expect("child op");

    assert_eq!(parent.audited, ADA, "{seen:?}");
    assert_ne!(
        child.audited, MALLORY,
        "the model-authored spawn payload named a principal and it MUST be inert: {seen:?}"
    );
    assert_eq!(
        child.audited, ADA,
        "a spawned journey runs as the turn that spawned it: {seen:?}"
    );
    assert_eq!(
        child.turn_trust.as_deref(),
        Some("Untrusted"),
        "no stronger than the spawning turn: {seen:?}"
    );
    assert_eq!(child.turn_principal, parent.turn_principal, "{seen:?}");

    // The record says the child's principal was inherited, not asserted by its own payload.
    let recorded = recorded_attributions(&app);
    let child_record = recorded
        .iter()
        .find(|r| r["journey"] == json!("child"))
        .unwrap_or_else(|| panic!("the child run is attributed too: {recorded:?}"));
    assert_eq!(child_record["caller"], json!(ADA), "{recorded:?}");
    assert_eq!(
        child_record["attribution"],
        json!("inherited"),
        "a spawn-sourced run never derives from its own payload: {recorded:?}"
    );
}
