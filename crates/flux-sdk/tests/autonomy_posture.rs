//! C-463 — an embedder names one posture, and gets its approver, its confinement and its budget.
//!
//! `secure_defaults.rs` pins the same coherence from the *inference* side: `auto_approve(true)`
//! carries confinement and a ceiling even though the embedder never mentioned either. This file
//! pins the side C-463 adds — that the coupling now has a **name**, so the choice can be stated
//! rather than inferred, and so an embedder reading their own source can see which posture they are
//! in without reconstructing it from three settings.
//!
//! ⚠ These tests deliberately go through the documented builder chain with no `with_sandbox` and no
//! `resource_limits`, for the reason C-444 recorded: documented is not defaulted.

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::approval::{ApprovalChoice, Approver, IntentSet};
use flux_sdk::sandbox::SandboxMode;
use flux_sdk::{ApprovalStance, AutonomyPosture, Client, Sandbox};
use std::sync::Arc;

/// Keep this integration binary deterministic when its parent is itself a confined Flux process:
/// the release gate runs under `FLUX_SANDBOX=require`, and these tests are about what a posture
/// resolves from an *unset* ambient environment. The mutex also keeps the parallel tests from
/// racing while the process-wide environment is cleared and restored.
struct CleanSandboxEnv {
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl CleanSandboxEnv {
    fn enter() -> Self {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        const KEYS: &[&str] = &[
            "FLUX_SANDBOX",
            "FLUX_SANDBOX_NET",
            "FLUX_SANDBOX_WRITABLE",
            "FLUX_SANDBOXED",
            "FLUX_BWRAP_BIN",
            "FLUX_SANDBOX_EXEC_BIN",
        ];
        let lock = LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let saved = KEYS
            .iter()
            .map(|&key| (key, std::env::var_os(key)))
            .collect();
        for key in KEYS {
            std::env::remove_var(key);
        }
        Self { _lock: lock, saved }
    }
}

impl Drop for CleanSandboxEnv {
    fn drop(&mut self) {
        for (key, value) in &self.saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct StubProvider;

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// A stand-in for an embedder's own approval channel — the thing a `supervised` SDK client *is*.
struct HostChannel;

#[async_trait]
impl Approver for HostChannel {
    async fn request(
        &self,
        _tool: &str,
        _subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        ApprovalChoice::Allow
    }
}

/// A unique temp workspace root for one test.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "flux-c463-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn client_sandbox(client: &Client) -> Sandbox {
    client
        .engine()
        .executor
        .context()
        .system()
        .sandbox()
        .clone()
}

/// **The C-463 SDK acceptance.** Naming a posture selects all three at once — and for the postures
/// that never prompt, the confinement and the ceiling arrive without the embedder mentioning either.
///
/// At the merge base this could not be written: there was no value to name. Approval, confinement
/// and ceilings were three independent builder calls, which is exactly why `auto_approve(true)`
/// alone was reachable.
#[test]
fn naming_a_posture_selects_approver_confinement_and_budget_together() {
    for posture in [
        AutonomyPosture::BoundedAutonomy,
        AutonomyPosture::Exploratory,
    ] {
        let _env = CleanSandboxEnv::enter();
        let dir = temp_root(posture.name());
        let client = Client::builder()
            .model("mock")
            .posture(posture)
            .build(Box::new(StubProvider), &dir)
            .unwrap_or_else(|e| panic!("build {posture}: {e}"));

        assert_eq!(posture.approval(), ApprovalStance::None);
        assert_eq!(
            client_sandbox(&client).settings().mode,
            SandboxMode::Require,
            "{posture}: a posture that never prompts must carry its confinement — an embedder \
             naming it stated one choice, not one of three"
        );
        assert!(
            !client.resource_limits().is_unbounded(),
            "{posture}: a posture that never prompts must carry its ceiling"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// The two autonomous postures are genuinely different choices, not one preset with two names:
/// `exploratory` keeps egress open because research and security hardening are network jobs.
#[test]
fn exploratory_keeps_egress_open_and_bounded_autonomy_closes_it() {
    let _env = CleanSandboxEnv::enter();

    let bounded_dir = temp_root("egress-bounded");
    let bounded = Client::builder()
        .model("mock")
        .posture(AutonomyPosture::BoundedAutonomy)
        .build(Box::new(StubProvider), &bounded_dir)
        .expect("build bounded-autonomy client");
    assert!(!client_sandbox(&bounded).settings().network);

    let exploratory_dir = temp_root("egress-exploratory");
    let exploratory = Client::builder()
        .model("mock")
        .posture(AutonomyPosture::Exploratory)
        .build(Box::new(StubProvider), &exploratory_dir)
        .expect("build exploratory client");
    assert!(
        client_sandbox(&exploratory).settings().network,
        "exploratory relies on wide-but-bounded grants; cutting egress would make it a posture \
         nobody selects twice"
    );

    std::fs::remove_dir_all(&bounded_dir).ok();
    std::fs::remove_dir_all(&exploratory_dir).ok();
}

/// ⚠ **No flag day.** The older spelling resolves to a named posture, and to the same envelope.
#[test]
fn auto_approve_and_the_named_posture_build_the_same_envelope() {
    let _env = CleanSandboxEnv::enter();

    let flagged_dir = temp_root("flagged");
    let flagged = Client::builder()
        .model("mock")
        .auto_approve(true)
        .build(Box::new(StubProvider), &flagged_dir)
        .expect("build with auto_approve");

    let named_dir = temp_root("named");
    let named = Client::builder()
        .model("mock")
        .posture(AutonomyPosture::BoundedAutonomy)
        .build(Box::new(StubProvider), &named_dir)
        .expect("build with the named posture");

    assert_eq!(
        client_sandbox(&flagged).settings().mode,
        client_sandbox(&named).settings().mode
    );
    assert_eq!(
        client_sandbox(&flagged).settings().network,
        client_sandbox(&named).settings().network
    );
    assert_eq!(
        flagged.resource_limits().max_concurrent_tool_calls(),
        named.resource_limits().max_concurrent_tool_calls()
    );
    assert_eq!(
        flagged.resource_limits().max_live_agents(),
        named.resource_limits().max_live_agents()
    );

    std::fs::remove_dir_all(&flagged_dir).ok();
    std::fs::remove_dir_all(&named_dir).ok();
}

/// A library has no approval UI, so `supervised` is only meaningful with an injected channel.
/// Refused rather than substituted: silently resolving a stated posture to a different one is the
/// accident the named postures exist to prevent, and it would be the worst possible instance of it —
/// an embedder who believes a human is in the loop.
#[test]
fn the_supervised_posture_needs_an_injected_channel_and_says_so() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("supervised-no-channel");
    let error = match Client::builder()
        .model("mock")
        .posture(AutonomyPosture::Supervised)
        .build(Box::new(StubProvider), &dir)
    {
        Ok(_) => panic!("a supervised client with no approval channel must be refused"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("no approval UI"), "{error}");
    assert!(
        error.contains("bounded-autonomy"),
        "the refusal must name the postures that do not need a channel: {error}"
    );

    let ok_dir = temp_root("supervised-with-channel");
    let client = Client::builder()
        .model("mock")
        .posture(AutonomyPosture::Supervised)
        .approver(Arc::new(HostChannel))
        .build(Box::new(StubProvider), &ok_dir)
        .expect("a supervised client with a channel builds");
    assert_eq!(
        client_sandbox(&client).settings().mode,
        SandboxMode::Off,
        "an embedder who stated `supervised` and supplied a human channel keeps the ambient \
         sandbox resolution — the raise is what a posture with no human in it carries"
    );

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&ok_dir).ok();
}

/// The escape hatch survives naming a posture: an explicit decision is still authoritative, so the
/// posture supplies defaults for what the embedder did not state and never overrides what they did.
#[test]
fn an_explicit_decision_still_wins_over_the_named_posture() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("explicit-wins");
    let client = Client::builder()
        .model("mock")
        .posture(AutonomyPosture::BoundedAutonomy)
        .with_sandbox(Sandbox::resolve(flux_sdk::SandboxSettings::off()))
        .resource_limits(flux_sdk::ResourceLimits::new().with_max_live_agents(3))
        .build(Box::new(StubProvider), &dir)
        .expect("build");

    assert_eq!(client_sandbox(&client).settings().mode, SandboxMode::Off);
    assert_eq!(client.resource_limits().max_live_agents(), Some(3));
    assert_eq!(
        client.resource_limits().max_concurrent_tool_calls(),
        None,
        "an explicit host ceiling must not be silently combined with the posture's preset"
    );

    std::fs::remove_dir_all(&dir).ok();
}
