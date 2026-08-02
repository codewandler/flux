//! C-453: **the envelope's approval stage, answered over the network.**
//!
//! Until this landed, a served agent could not be asked. Every approver in the tree was local — the
//! CLI's terminal prompt, the TUI's in-process channel, the headless sub-agent policy — so
//! `flux app run --serve` ran under `AllowApprover` (approve everything) or `DenyApprover` (approve
//! nothing). Both of those are legitimate postures for the right job; what was missing is that an
//! operator who wanted the posture *with a human in it* could not select it on a served agent at
//! all.
//!
//! These tests drive the whole path, not a component of it: a real `FlowEngine`, the real `write`
//! builtin running through the real safety envelope onto the server's own workspace, the real
//! production router built through `router_with_approvals_in`, and decisions that travel **only**
//! over HTTP. Nothing here calls `ApprovalQueue::decide` directly, and "was it approved" is always
//! answered by looking at the server's disk rather than at a return value — if the routes did not
//! work, every test in this file would fail.
//!
//! The four properties, in the order they are most likely to be got wrong:
//!
//! 1. silence denies ([`an_effect_nobody_answers_is_refused`]);
//! 2. an approval is bound to the effect it was granted for
//!    ([`an_approval_cannot_be_delivered_against_a_different_effect`]);
//! 3. a decision is single-use ([`a_replayed_decision_is_refused`]);
//! 4. only an authenticated caller may answer ([`answering_an_approval_requires_authentication`]).

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};

use flux_flow::engine::FlowEngine;
use flux_runtime::{ApprovalQueue, RemoteApprover};
use flux_server::{ApprovalGate, CardInfo, ServerAuth};

use support::{get_raw, pinned_env, post_json_auth, post_raw_auth, scratch_dir, ProseProvider};

/// A served agent running the remote-approval posture: the engine's executor parks every
/// approval-needing effect on `queue`, and the router serves that same queue.
struct ServedAgent {
    engine: Arc<FlowEngine>,
    router: Router,
    queue: Arc<ApprovalQueue>,
    /// The agent's workspace — where an approved `write` actually lands.
    workspace: PathBuf,
}

impl ServedAgent {
    /// Did the effect land? Read the server's own disk, not a mock's counter.
    fn wrote(&self, name: &str) -> bool {
        self.workspace.join(name).exists()
    }
}

/// Assemble one. `timeout` is how long a parked effect waits before the approver denies it; `token`
/// is the server's shared secret (`None` → the loopback-only open mode).
fn served_agent(timeout: Duration, token: Option<String>) -> ServedAgent {
    let workspace = scratch_dir("remote-approval");
    let system = Arc::new(flux_system::System::new(
        flux_system::Workspace::new(&workspace).unwrap(),
    ));

    let queue = Arc::new(ApprovalQueue::new(timeout));

    // The real builtins, so the effect under test is a real guarded filesystem write and not a
    // stand-in. `PermissionManager::new()` pre-allows nothing, so `write` reaches the approval
    // stage — which is the stage under test. Authorization still runs ahead of it, untouched.
    let mut registry = flux_runtime::ToolRegistry::new();
    flux_tools::try_register_builtins(&mut registry).unwrap();
    // The agent loop's own control ops, which `FlowEngine::assemble` validates its loop against.
    flux_tools::try_register_reflect(&mut registry).unwrap();
    let executor = flux_runtime::Executor::new(
        registry,
        flux_runtime::PermissionManager::new(),
        Arc::new(RemoteApprover::new(Arc::clone(&queue))),
        flux_runtime::ToolContext::new(system),
    );

    let events = Arc::new(flux_events::EventStore::in_memory().unwrap());
    let flow = flux_flow::state::FlowStore::in_memory_with_events(events.clone()).unwrap();
    let engine = Arc::new(
        FlowEngine::assemble(
            Arc::new(ProseProvider),
            executor,
            events,
            flow,
            "claude-sonnet-4-6".into(),
            "test".into(),
            1024,
            5,
            Vec::new(),
            0,
            Vec::new(),
            workspace.clone(),
        )
        .unwrap(),
    );

    let router = flux_server::router_with_approvals_in(
        Arc::clone(&engine),
        ServerAuth::from_token(token),
        CardInfo::flux_coding(),
        "127.0.0.1:0".parse().unwrap(),
        &pinned_env(),
        ApprovalGate::serving(Arc::clone(&queue)),
    )
    .expect("loopback router builds");

    ServedAgent {
        engine,
        router,
        queue,
        workspace,
    }
}

/// Run one guarded effect through the served agent's own executor — the same envelope an inbound
/// A2A turn dispatches through.
fn write_effect(
    engine: &Arc<FlowEngine>,
    path: &'static str,
) -> tokio::task::JoinHandle<flux_runtime::ToolResult> {
    let engine = Arc::clone(engine);
    tokio::spawn(async move {
        engine
            .executor
            .dispatch("write", json!({ "path": path, "content": "landed\n" }))
            .await
    })
}

/// `GET /approvals` over HTTP, parsed. Panics on a non-200 so a posture or auth regression is never
/// silently read as "nothing pending".
async fn list_approvals(router: &Router, auth: Option<&str>) -> Vec<Value> {
    let (status, raw) = get_raw(router.clone(), "/approvals", auth).await;
    assert_eq!(status, StatusCode::OK, "GET /approvals: {raw}");
    let body: Value = serde_json::from_str(&raw).unwrap();
    body["approvals"].as_array().cloned().unwrap_or_default()
}

/// The `(id, fingerprint)` of the parked request naming `subject` — the two values a decision has
/// to carry. Polls, because the effect parks asynchronously.
async fn await_parked(router: &Router, auth: Option<&str>, subject: &str) -> (String, String) {
    for _ in 0..500 {
        if let Some(request) = list_approvals(router, auth).await.into_iter().find(|r| {
            r["subjects"]
                .as_array()
                .is_some_and(|s| s.iter().any(|v| v == subject))
        }) {
            return (
                request["id"].as_str().unwrap().to_string(),
                request["fingerprint"].as_str().unwrap().to_string(),
            );
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("no approval naming `{subject}` ever appeared on /approvals");
}

/// `POST /approvals/{id}` over HTTP. Raw-bodied on purpose: a refusal from the auth layer or from
/// axum's own extractor is not JSON, and a helper that panicked on those would hide exactly the
/// paths this file is here to pin.
async fn decide(
    router: &Router,
    auth: Option<&str>,
    id: &str,
    fingerprint: &str,
    decision: &str,
) -> (StatusCode, String) {
    post_raw_auth(
        router.clone(),
        &format!("/approvals/{id}"),
        "application/json",
        &json!({ "fingerprint": fingerprint, "decision": decision }).to_string(),
        auth,
    )
    .await
}

/// ⚠ **The story's headline.** One specific effect on a served agent, approved by one decision that
/// travelled over the network — and nothing approved that was not decided.
#[tokio::test]
async fn a_served_effect_is_approved_by_one_remote_decision() {
    let agent = served_agent(Duration::from_secs(30), None);

    let running = write_effect(&agent.engine, "report.txt");
    let (id, fingerprint) = await_parked(&agent.router, None, "report.txt").await;

    // Parked, not racing ahead: the effect has not touched the disk yet.
    assert!(
        !agent.wrote("report.txt"),
        "the effect landed before anyone approved it"
    );

    let (status, body) = decide(&agent.router, None, &id, &fingerprint, "allow").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let result = running.await.unwrap();
    assert!(
        !result.is_error,
        "approved effect errored: {}",
        result.content
    );
    assert!(
        agent.wrote("report.txt"),
        "the effect was approved over the network but never landed"
    );

    // The approval covered the one effect it was granted for; nothing is left standing.
    assert!(list_approvals(&agent.router, None).await.is_empty());
}

/// ⚠ **Fails closed.** Nobody answers; the effect is refused, not allowed. An approval channel that
/// allowed on silence would be worse than having no approval stage at all, because it would *look*
/// like a control.
#[tokio::test]
async fn an_effect_nobody_answers_is_refused() {
    let agent = served_agent(Duration::from_millis(150), None);

    let result = write_effect(&agent.engine, "report.txt").await.unwrap();

    assert!(
        result.is_error,
        "an unanswered effect must be refused, got: {}",
        result.content
    );
    assert!(
        result.content.contains("denied"),
        "expected a denial, got: {}",
        result.content
    );
    assert!(
        !agent.wrote("report.txt"),
        "⚠ an effect nobody approved was executed"
    );
    assert!(
        list_approvals(&agent.router, None).await.is_empty(),
        "a denied request must not stay listed as still answerable"
    );
}

/// ⚠ **Confused deputy.** A `yes` displayed for one effect must not be deliverable against another.
/// Both are parked at once, which is exactly the substitution an attacker — or a buggy client
/// holding a stale sheet — would attempt.
#[tokio::test]
async fn an_approval_cannot_be_delivered_against_a_different_effect() {
    let agent = served_agent(Duration::from_secs(30), None);

    let benign = write_effect(&agent.engine, "notes.txt");
    let sensitive = write_effect(&agent.engine, "credentials.txt");
    let (benign_id, benign_fingerprint) = await_parked(&agent.router, None, "notes.txt").await;
    let (sensitive_id, sensitive_fingerprint) =
        await_parked(&agent.router, None, "credentials.txt").await;
    assert_ne!(
        benign_fingerprint, sensitive_fingerprint,
        "two different effects must not share a fingerprint"
    );

    // The substitution: the fingerprint the human was shown, aimed at the other request's id.
    let (status, body) = decide(
        &agent.router,
        None,
        &sensitive_id,
        &benign_fingerprint,
        "allow",
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an approval granted for one effect was accepted for another: {body}"
    );

    // And aimed the other way, for symmetry.
    let (status, _) = decide(
        &agent.router,
        None,
        &benign_id,
        &sensitive_fingerprint,
        "allow",
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Refusing a decision is not an implicit answer in either direction — both still await one.
    assert_eq!(list_approvals(&agent.router, None).await.len(), 2);

    // Each honest decision still lands against its own effect.
    assert_eq!(
        decide(
            &agent.router,
            None,
            &sensitive_id,
            &sensitive_fingerprint,
            "deny"
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        decide(
            &agent.router,
            None,
            &benign_id,
            &benign_fingerprint,
            "allow"
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(!benign.await.unwrap().is_error);
    assert!(sensitive.await.unwrap().is_error);
    assert!(agent.wrote("notes.txt"));
    assert!(
        !agent.wrote("credentials.txt"),
        "⚠ the effect the human never approved landed anyway"
    );
}

/// ⚠ **Single use.** A captured decision, replayed, finds nothing to apply itself to.
#[tokio::test]
async fn a_replayed_decision_is_refused() {
    let agent = served_agent(Duration::from_secs(30), None);

    let first = write_effect(&agent.engine, "report.txt");
    let (id, fingerprint) = await_parked(&agent.router, None, "report.txt").await;
    assert_eq!(
        decide(&agent.router, None, &id, &fingerprint, "allow")
            .await
            .0,
        StatusCode::OK
    );
    assert!(!first.await.unwrap().is_error);

    // The identical decision, byte for byte.
    let (status, body) = decide(&agent.router, None, &id, &fingerprint, "allow").await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a decision was replayable: {body}"
    );

    // ...and replaying it while an *identical* effect is parked does not approve that one either:
    // the fresh request carries its own id, which the captured decision does not name.
    let second = write_effect(&agent.engine, "report.txt");
    let (fresh_id, fresh_fingerprint) = await_parked(&agent.router, None, "report.txt").await;
    assert_ne!(fresh_id, id, "each request gets its own id");
    assert_eq!(
        fresh_fingerprint, fingerprint,
        "the same effect fingerprints the same — the id is what makes the decision single-use"
    );
    assert_eq!(
        decide(&agent.router, None, &id, &fingerprint, "allow")
            .await
            .0,
        StatusCode::NOT_FOUND,
        "⚠ a replayed decision approved a later effect"
    );
    assert_eq!(
        decide(&agent.router, None, &fresh_id, &fresh_fingerprint, "deny")
            .await
            .0,
        StatusCode::OK
    );
    assert!(second.await.unwrap().is_error);
}

/// A decision word that is neither `allow` nor `deny` is refused — and crucially does **not** fall
/// through to allow. The effect stays parked and is denied when it times out.
#[tokio::test]
async fn an_unrecognised_decision_is_not_an_approval() {
    let agent = served_agent(Duration::from_millis(600), None);

    let running = write_effect(&agent.engine, "report.txt");
    let (id, fingerprint) = await_parked(&agent.router, None, "report.txt").await;

    for word in ["yes", "true", "Allow", "", "ALLOW", "approve"] {
        let (status, body) = decide(&agent.router, None, &id, &fingerprint, word).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "decision {word:?} was not refused: {body}"
        );
    }
    // A body that omits the fingerprint entirely is a malformed decision, not an approval — the
    // binding is not optional, so axum's extractor rejects it before any handler runs.
    let (status, _) = post_raw_auth(
        agent.router.clone(),
        &format!("/approvals/{id}"),
        "application/json",
        &json!({ "decision": "allow" }).to_string(),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    assert!(running.await.unwrap().is_error);
    assert!(!agent.wrote("report.txt"));
}

/// ⚠ Who may answer an approval is exactly who may authenticate. An unauthenticated caller must not
/// be able to see what the agent is about to do, let alone approve it.
#[tokio::test]
async fn answering_an_approval_requires_authentication() {
    let agent = served_agent(Duration::from_secs(30), Some("s3cr3t".into()));
    let auth = Some("Bearer s3cr3t");

    let running = write_effect(&agent.engine, "report.txt");
    let (id, fingerprint) = await_parked(&agent.router, auth, "report.txt").await;

    let (status, _) = get_raw(agent.router.clone(), "/approvals", None).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "an anonymous caller could see what the agent is about to do"
    );
    let (status, _) = decide(&agent.router, None, &id, &fingerprint, "allow").await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "⚠ an anonymous caller approved an effect"
    );
    let (status, _) = decide(
        &agent.router,
        Some("Bearer wrong"),
        &id,
        &fingerprint,
        "allow",
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(!agent.wrote("report.txt"));

    // The authenticated operator still can.
    assert_eq!(
        decide(&agent.router, auth, &id, &fingerprint, "allow")
            .await
            .0,
        StatusCode::OK
    );
    assert!(!running.await.unwrap().is_error);
    assert!(agent.wrote("report.txt"));
}

/// A denial reason reaches the model (C-113), rather than being flattened into a bare refusal by
/// the trip over the network.
#[tokio::test]
async fn a_denial_reason_survives_the_network() {
    let agent = served_agent(Duration::from_secs(30), None);

    let running = write_effect(&agent.engine, "report.txt");
    let (id, fingerprint) = await_parked(&agent.router, None, "report.txt").await;
    let (status, _) = post_json_auth(
        agent.router.clone(),
        &format!("/approvals/{id}"),
        json!({
            "fingerprint": fingerprint,
            "decision": "deny",
            "reason": "write it under notes/ instead",
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let result = running.await.unwrap();
    assert!(result.is_error);
    assert!(
        result.content.contains("write it under notes/ instead"),
        "the operator's reason did not reach the model: {}",
        result.content
    );
    assert!(!agent.wrote("report.txt"));
}

/// ⚠ A server running some *other* posture says so, rather than serving an empty list. "Nothing is
/// waiting" and "nobody is ever asked" must not look identical to a client — an operator pointing
/// an approval UI at the wrong server would otherwise see a permanently quiet queue and conclude
/// their agent had nothing to approve.
#[tokio::test]
async fn a_server_without_the_posture_does_not_pretend_to_have_one() {
    // `support::app` is the ordinary served agent: a headless approver, no queue.
    let router = support::app(None);

    let (status, body) = get_raw(router.clone(), "/approvals", None).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED, "{body}");
    assert!(
        body.contains("remote-approval posture"),
        "the refusal should name the posture: {body}"
    );

    let (status, _) = post_json_auth(
        router,
        "/approvals/ap_whatever_0",
        json!({ "fingerprint": "{}", "decision": "allow" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
}

/// The listing carries what a human needs in order to decide — and every field in it is part of
/// what the decision binds to, so the sheet and the binding cannot drift apart.
#[tokio::test]
async fn the_listing_describes_the_effect_it_binds() {
    let agent = served_agent(Duration::from_secs(30), None);
    let running = write_effect(&agent.engine, "report.txt");
    await_parked(&agent.router, None, "report.txt").await;

    let listed = list_approvals(&agent.router, None).await;
    assert_eq!(listed.len(), 1);
    let request = &listed[0];
    assert_eq!(request["tool"], "write");
    assert_eq!(request["subjects"], json!(["report.txt"]));
    assert!(request["id"].is_string());
    assert!(request["fingerprint"].is_string());
    assert_eq!(
        request["mutating"], true,
        "the runtime's own risk signal must reach the human, not be dropped in transit"
    );
    assert!(
        request["intents"]["intents"].is_array(),
        "the exact intent targets must be shown and bound, not collapsed to risk booleans"
    );
    assert!(
        request["plan"].is_null(),
        "one effect is not a plan approval"
    );
    assert!(request["waiting_secs"].is_number());

    // The advertised timeout is the real one, so a client can show how long the operator has.
    let (_, raw) = get_raw(agent.router.clone(), "/approvals", None).await;
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["timeout_secs"], 30);
    assert_eq!(agent.queue.timeout(), Duration::from_secs(30));

    let (id, fingerprint) = await_parked(&agent.router, None, "report.txt").await;
    decide(&agent.router, None, &id, &fingerprint, "deny").await;
    assert!(running.await.unwrap().is_error);
}
