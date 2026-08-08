//! C-686: the attach client (`flux_a2a::attach`) driven against a **real served agent** over a
//! **real socket**.
//!
//! The rest of this suite drives the router in-process with `tower::ServiceExt::oneshot`, which is
//! right for asserting handler behaviour but proves nothing about a client: `A2aClient` is a
//! reqwest client that discovers a card, negotiates SSE, holds a bearer credential and follows a
//! task across three JSON-RPC methods. So this file binds `axum::serve` on loopback and points the
//! real client at it. What is under test is the *pair* — every assertion here would still pass on a
//! hand-rolled fixture server, and that is exactly why the fixture is the production
//! `flux_server::router_in`, i.e. what `flux app run --serve` mounts.
//!
//! It lives in `flux-server`'s suite rather than `flux-a2a`'s because this is the only crate that
//! can stand the served agent up: `flux-a2a` is L1 and cannot see an engine, and `flux-cli` has no
//! library target for an integration test to link against.

mod support;

use std::sync::Arc;
use std::time::Duration;

use axum::Router;

use flux_a2a::attach::{ApprovalReach, AttachEvent, AttachedA2aAgent, Availability};
use flux_server::{ApprovalGate, CardInfo, ServerAuth};

use support::{pinned_env, test_engine, MultiDeltaProvider, ProseProvider};

/// Serve `router` on a fresh loopback port and return its base URL. The server task is detached and
/// dies with the test process; nothing here binds a fixed port, so the file is parallel-safe.
async fn serve(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}")
}

/// The default fixture: an unauthenticated loopback agent that streams two text deltas.
async fn served_streaming_agent() -> String {
    let engine = test_engine(Arc::new(MultiDeltaProvider));
    let router = flux_server::router_in(
        engine,
        ServerAuth::from_token(None),
        CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &pinned_env(),
    )
    .expect("loopback router builds");
    serve(router).await
}

/// Drain one attached turn into the events it produced.
async fn run_turn(agent: &AttachedA2aAgent, text: &str) -> Vec<AttachEvent> {
    let mut events = Vec::new();
    agent.send(text, &mut |event| events.push(event)).await;
    events
}

/// The agent text an attached turn produced, concatenated the way a surface appends it.
fn text_of(events: &[AttachEvent]) -> String {
    events
        .iter()
        .filter_map(|e| match e {
            AttachEvent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

/// Acceptance 1: a served agent's streamed turn arrives as ordered text on the attach seam, and the
/// attachment learns the task it is now following.
#[tokio::test]
async fn a_served_agents_streamed_turn_reaches_the_attach_seam() {
    let base = served_streaming_agent().await;
    let agent = AttachedA2aAgent::connect(&base, None, None)
        .await
        .expect("the attach client connects to a served agent");

    assert!(
        agent.support().streaming.is_available(),
        "the served card declares streaming: {:?}",
        agent.support().streaming
    );

    let events = run_turn(&agent, "hello").await;
    assert_eq!(
        text_of(&events),
        "hello world",
        "the two streamed deltas must arrive in order and exactly once: {events:?}"
    );
    assert!(
        events.last() == Some(&AttachEvent::Ended),
        "a turn always ends with Ended: {events:?}"
    );
    assert!(
        agent.task_id().is_some(),
        "the attachment must learn the remote task it is following — cancel and reattach address it"
    );
}

/// Acceptance 4: reattaching replays the remote's own history, and it is the *remote's* record —
/// the client wrote nothing locally to build it from.
#[tokio::test]
async fn reattaching_replays_the_remotes_own_history() {
    let base = served_streaming_agent().await;
    let context = "ctx-attach-history".to_string();

    // First attachment: one turn, then drop the client entirely — the "detach".
    let task = {
        let agent = AttachedA2aAgent::connect(&base, None, Some(context.clone()))
            .await
            .unwrap();
        run_turn(&agent, "roll the deployment").await;
        let history = agent.history().await.expect("history after a turn");
        assert!(
            history
                .iter()
                .any(|t| t.from_user && t.text.contains("roll the deployment")),
            "the remote's own history carries the operator's turn: {history:?}"
        );
        assert!(
            history
                .iter()
                .any(|t| !t.from_user && t.text.contains("hello world")),
            "…and the agent's answer: {history:?}"
        );
        agent.task_id().expect("a task id")
    };

    // Second attachment on the same context: the remote session continued, and the earlier turns
    // are readable from it. This is the whole of "reattaching replays enough history to make the
    // pane truthful about what happened while detached".
    let agent = AttachedA2aAgent::connect(&base, None, Some(context))
        .await
        .unwrap();
    run_turn(&agent, "and now?").await;
    assert_eq!(
        agent.task_id().as_deref(),
        Some(task.as_str()),
        "the same context id must continue the same remote session, not mint a second one"
    );
    let history = agent.history().await.expect("history on reattach");
    assert!(
        history
            .iter()
            .any(|t| t.text.contains("roll the deployment")),
        "the turn from before the detach must still be in the replay: {history:?}"
    );
    assert!(
        history.iter().any(|t| t.text.contains("and now?")),
        "…alongside the new one: {history:?}"
    );
}

/// Acceptance 2: cancellation is delivered through `tasks/cancel`, and the attachment reports the
/// capability rather than discovering it by failing.
#[tokio::test]
async fn cancel_is_probed_up_front_and_delivered_to_the_live_task() {
    let base = served_streaming_agent().await;
    let agent = AttachedA2aAgent::connect(&base, None, None).await.unwrap();
    assert!(
        agent.support().cancel.is_available(),
        "the served stateful surface implements tasks/cancel: {:?}",
        agent.support().cancel
    );
    // Nothing is running yet, and the client must say exactly that rather than firing at a task it
    // does not have.
    assert_eq!(
        agent.cancel().await,
        flux_a2a::attach::CancelOutcome::Idle,
        "with no live task there is nothing to cancel"
    );

    run_turn(&agent, "hello").await;
    // The turn is over, so the server answers `-32002 TaskNotCancelable`. That is a benign outcome
    // for an opportunistic cancel and must be distinguishable from "this agent cannot cancel".
    assert_eq!(
        agent.cancel().await,
        flux_a2a::attach::CancelOutcome::AlreadyTerminal,
        "a finished task reports as already terminal, not as unsupported"
    );
}

/// Acceptance 2: the same cancel against an agent that does not implement `tasks/cancel` is
/// reported as unsupported, so the surface can disable the control instead of pretending.
#[tokio::test]
async fn an_agent_without_the_task_surface_reports_cancel_as_unsupported() {
    // `flux_a2a::server::dispatch` is the reduced *embeddable* dispatch: it implements
    // `message/send` and classifies `tasks/cancel` as `-32004 UnsupportedOperation`. Serving it
    // here is not a mock of a broken agent — it is the other dispatch flux ships.
    struct Embeddable;
    #[async_trait::async_trait]
    impl flux_a2a::server::A2aTurn for Embeddable {
        async fn run(&self, input: &str) -> Result<String, String> {
            Ok(format!("echo: {input}"))
        }
    }
    let router = Router::new()
        .route(
            "/a2a",
            axum::routing::post(|body: axum::Json<serde_json::Value>| async move {
                axum::Json(flux_a2a::server::dispatch(&Embeddable, None, &body.0).await)
            }),
        )
        .route(
            "/.well-known/agent-card.json",
            axum::routing::get(|| async {
                axum::Json(flux_a2a::server::agent_card(
                    "embeddable",
                    "the reduced dispatch",
                    None,
                    "1.0.0",
                    &[],
                    false,
                ))
            }),
        );
    let base = serve(router).await;

    let agent = AttachedA2aAgent::connect(&base, None, None).await.unwrap();
    let why = match &agent.support().cancel {
        Availability::Unavailable(why) => why.clone(),
        Availability::Available => panic!("the embeddable dispatch does not implement cancel"),
    };
    assert!(
        why.contains("does not implement tasks/cancel"),
        "the reason must name the missing method: {why}"
    );
    assert!(
        why.contains("it does not stop the remote turn"),
        "…and the consequence for the operator: {why}"
    );
    // The card declares no streaming, so the client must fall back rather than hang on a non-SSE
    // response — and must say it fell back.
    assert!(!agent.support().streaming.is_available());
    let events = run_turn(&agent, "ping").await;
    assert_eq!(text_of(&events), "echo: ping", "{events:?}");
}

/// Acceptance 3: a served agent with no remote-approval posture is reported as *never raising*
/// approvals, not as "no approvals pending". Those are different facts and only one has a human in
/// the loop.
#[tokio::test]
async fn a_headless_served_agent_reports_that_approvals_are_never_raised() {
    let base = served_streaming_agent().await;
    let agent = AttachedA2aAgent::connect(&base, None, None).await.unwrap();
    match &agent.support().approvals {
        ApprovalReach::NotRaised(why) => assert!(
            why.contains("remote-approval posture"),
            "the server's own words must survive to the operator: {why}"
        ),
        other => panic!("expected NotRaised, got {other:?}"),
    }
    // And the client refuses to poll a queue that does not exist, rather than reporting an empty one.
    let error = agent
        .pending_approvals()
        .await
        .expect_err("there is no queue to read");
    assert!(error.contains("never raised"), "{error}");
}

/// Acceptance 3: a served agent running the remote-approval posture is answerable, an effect it
/// parks is readable, and a decision is bound to that effect by its fingerprint.
#[tokio::test]
async fn a_parked_effect_is_readable_and_answerable_by_fingerprint() {
    let queue = Arc::new(flux_runtime::ApprovalQueue::new(Duration::from_secs(30)));
    let engine = test_engine(Arc::new(ProseProvider));
    let router = flux_server::router_with_approvals_in(
        engine,
        ServerAuth::from_token(None),
        CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &pinned_env(),
        ApprovalGate::serving(queue.clone()),
    )
    .expect("a loopback remote-approval router builds");
    let base = serve(router).await;

    let agent = AttachedA2aAgent::connect(&base, None, None).await.unwrap();
    match &agent.support().approvals {
        ApprovalReach::Answerable { caveat } => assert!(
            caveat.contains("C-687"),
            "the shared-operator-token limit must be stated wherever approvals are offered: \
             {caveat}"
        ),
        other => panic!("expected Answerable, got {other:?}"),
    }

    // Park a real effect on the very queue the router serves, exactly as `RemoteApprover` does.
    let approver = flux_runtime::RemoteApprover::new(queue.clone());
    let parked = tokio::spawn(async move {
        use flux_runtime::Approver;
        approver
            .request(
                "bash",
                &["rm -rf /srv/tmp".to_string()],
                // `flux-spec` is not a direct dependency of this crate; the SDK's `approval` module
                // re-exports the very same type, which is also how an out-of-tree approver sees it.
                &flux_sdk::approval::IntentSet::default(),
            )
            .await
    });
    let pending = loop {
        let pending = agent
            .pending_approvals()
            .await
            .expect("the queue is readable");
        if let Some(first) = pending.into_iter().next() {
            break first;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert_eq!(pending.tool, "bash");
    assert_eq!(pending.subjects, ["rm -rf /srv/tmp"]);

    // A decision aimed at the right id but carrying the wrong fingerprint approves nothing.
    let mismatch = agent
        .decide_approval(&pending.id, "not-the-effect", true, None)
        .await
        .expect_err("a mismatched fingerprint is refused");
    assert!(mismatch.contains("409"), "{mismatch}");

    agent
        .decide_approval(
            &pending.id,
            &pending.fingerprint,
            false,
            Some("not on this host"),
        )
        .await
        .expect("the echoed fingerprint is accepted");
    match parked.await.unwrap() {
        flux_runtime::ApprovalChoice::DenyWithReason(why) => assert_eq!(why, "not on this host"),
        other => panic!(
            "the decision the operator gave must be the one the runtime receives, got {other:?}"
        ),
    }
}

/// A gated agent's credential travels as a bearer header on every call the attachment makes —
/// including the card fetch and the `/approvals` probe, not only the JSON-RPC turn.
#[tokio::test]
async fn the_bearer_credential_reaches_every_call_the_attachment_makes() {
    let engine = test_engine(Arc::new(MultiDeltaProvider));
    let router = flux_server::router_in(
        engine,
        ServerAuth::from_token(Some("s3cr3t".to_string())),
        CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &pinned_env(),
    )
    .expect("an authenticated router builds");
    let base = serve(router).await;

    let unauthenticated = AttachedA2aAgent::connect(&base, None, None).await.unwrap();
    let events = run_turn(&unauthenticated, "hello").await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, AttachEvent::Notice { error: true, .. })),
        "an unauthenticated attach must fail loudly, not render an empty turn: {events:?}"
    );

    let agent = AttachedA2aAgent::connect(&base, Some("s3cr3t".to_string()), None)
        .await
        .unwrap();
    assert_eq!(text_of(&run_turn(&agent, "hello").await), "hello world");
    assert!(
        agent.support().cancel.is_available(),
        "the credential must reach the cancel probe too: {:?}",
        agent.support().cancel
    );
}
