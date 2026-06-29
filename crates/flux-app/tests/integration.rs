//! End-to-end host tests: build an [`App`] from a pure-op program (no provider, no model) and prove
//! the trigger → journey → execution path runs, that the orchestration ops are functional, and that
//! the bundled `examples/hello.flux` stays valid.

use flux_app::App;
use flux_lang::program::{Module, Program};
use serde_json::json;

/// Parse a program source string, panicking with context if it isn't a program.
fn program(src: &str) -> Program {
    match Module::parse_str(src).expect("parse program") {
        Module::Program(p) => p,
        Module::Flow(_) => panic!("expected a program, got a bare flow"),
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

/// The full-surface example exercises every typed declaration — agent + slack channel (with `secret`
/// references) + a markdown datasource + an agent-bound trigger + a journey — and secrets stay as
/// unresolved markers until the host resolves them.
#[test]
fn support_bot_example_covers_the_full_module_surface() {
    let p = program(include_str!("../examples/support-bot.flux"));
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
    assert!(p.flow_named("answer").is_some());
}
