//! C-444 — the documented happy path cannot reach auto-approval with no confinement and no ceiling.
//!
//! The Pi comparison found two settings an embedder falls out of without noticing, both *documented*
//! and neither *defaulted*:
//!
//! * **F2** — `auto_approve(true)` did not imply confinement; the embedder had to set it.
//! * **F4** — the runtime-use ceilings were unbounded by default and per agent, so a delegated tree
//!   multiplied its concurrent tool count without bound.
//!
//! What these tests pin is **not** that autonomy is discouraged. Running without per-effect approval
//! is a valid posture (C-463) — research, security hardening and long exploration are cases where
//! prompting per effect is actively the wrong design. What they pin is that *choosing it carries its
//! confinement and its ceiling with it*: the three settings compose into one coherent posture instead
//! of being independent knobs an embedder can set one of and miss the others.
//!
//! Every assertion here goes through the **documented** builder chain from the crate root — no
//! `with_sandbox`, no `resource_limits` — because "documented is not defaulted" is the whole finding.

use async_trait::async_trait;
use flux_core::Result;
use flux_provider::{ChunkStream, Provider, Request};
use flux_sdk::approval::{ApprovalChoice, Approver, IntentSet};
use flux_sdk::sandbox::SandboxMode;
use flux_sdk::{Client, ResourceLimits, Sandbox, SandboxSettings};
use std::sync::Arc;

/// Keep this integration binary deterministic when its parent is itself a confined Flux process.
///
/// The automatic release gate runs under `FLUX_SANDBOX=require` and `FLUX_SANDBOXED=1`. These tests
/// exercise the SDK's *unset ambient posture* defaults, so inheriting that outer process policy
/// changes the subject under test. The mutex also keeps the six parallel tests from racing while
/// the process-wide environment is cleared and restored.
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

/// The smallest custom policy that proves an opaque approver can remove every human decision.
struct AlwaysAllow;

#[async_trait]
impl Approver for AlwaysAllow {
    async fn request(
        &self,
        _tool: &str,
        _subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        ApprovalChoice::Allow
    }
}

#[async_trait]
impl Provider for StubProvider {
    fn name(&self) -> &str {
        "unused"
    }
    async fn stream(&self, _req: Request) -> Result<ChunkStream> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

/// A unique temp workspace root for one test.
fn temp_root(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "flux-c444-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// The sandbox posture a built [`Client`] actually enforces on its spawns, read off the guarded
/// `System` the engine's executor dispatches through — the thing that binds, not a builder field.
fn client_posture(client: &Client) -> Sandbox {
    client
        .engine()
        .executor
        .context()
        .system()
        .sandbox()
        .clone()
}

// ---------------------------------------------------------------------------
// F2 + F4 — the autonomous posture carries its confinement and its ceiling
// ---------------------------------------------------------------------------

/// **The C-444 acceptance test.** An SDK agent constructed the documented way with
/// `auto_approve(true)` is confined *and* has a resource ceiling.
///
/// At the merge base both halves fail: `Envelope::resolve_sandbox` resolved the ambient environment
/// (unset ⇒ `SandboxMode::Off`) regardless of the approval posture, and `Envelope`'s
/// `resource_limits` started at `ResourceLimits::new()` — unbounded.
#[test]
fn an_auto_approved_client_is_confined_and_bounded() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("confined-and-bounded");
    // The exact chain the crate-root doc example shows, plus a model: no `with_sandbox`, no
    // `resource_limits`. This is the "documented happy path" the story names.
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");

    let posture = client_posture(&client);
    assert_eq!(
        posture.settings().mode,
        SandboxMode::Require,
        "auto-approval did not carry its confinement: an embedder following the documented happy \
         path reached a blanket-allow approver with the OS-sandbox posture left at `{:?}`. \
         Auto-approval is a valid posture (C-463), but it must bring its confinement with it — the \
         CLI raises the same form to fail-closed `require` (C-262 / C-410).",
        posture.settings().mode
    );
    assert!(
        !client.resource_limits().is_unbounded(),
        "auto-approval did not carry its ceiling: the runtime's use ceilings are unbounded, so an \
         unattended embedder has no bound on simultaneously executing tool calls or retained bytes."
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The autonomous posture also closes the sandbox network by default, matching the CLI's unattended
/// profile — when the prompt is gone, destination scope is part of what is left constraining the run.
#[test]
fn the_autonomous_posture_closes_the_sandbox_network() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("network-closed");
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");

    assert!(
        !client_posture(&client).settings().network,
        "the autonomous posture must default its sandbox network CLOSED, as the CLI's unattended \
         profile does — an embedder may reopen it explicitly"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// An injected approver is opaque to the SDK: it may prompt a human, but it may also be the
/// three-line blanket allow above. Silence about confinement and ceilings must therefore resolve
/// conservatively. An embedder that really has an outer boundary can still state both overrides.
#[test]
fn an_opaque_approver_cannot_claim_supervision_by_default() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("opaque-approver");
    let client = Client::builder()
        .model("mock")
        .approver(Arc::new(AlwaysAllow))
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");

    let posture = client_posture(&client);
    assert_eq!(
        posture.settings().mode,
        SandboxMode::Require,
        "an opaque custom approver escaped the confinement floor"
    );
    assert!(
        !client.resource_limits().is_unbounded(),
        "an opaque custom approver escaped the delegated-tree resource ceiling"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The escape hatch, and the reason it is not a hole: an embedder who genuinely wants no confinement
/// must be able to say so, and that call is **visible in their code**. An explicit `with_sandbox`
/// wins over the implied raise — a pinned posture is a decision, not an omission.
#[test]
fn an_explicit_sandbox_decision_still_wins() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("explicit-off");
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        // Explicit, visible, and in the embedder's own source: no OS confinement, because isolation
        // is being provided some other way (an outer container, a VM, a disposable host).
        .with_sandbox(Sandbox::resolve(SandboxSettings::off()))
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");

    assert_eq!(
        client_posture(&client).settings().mode,
        SandboxMode::Off,
        "an explicitly pinned sandbox must win over the auto-approval raise — otherwise an embedder \
         who has provided isolation another way cannot say so"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// C-471: an explicit SDK ceiling is host policy, so it wins over the autonomous preset just as an
/// explicit sandbox decision does. File config reaches this same builder input through
/// `ResourceLimits::from_config`; only silence selects the posture preset.
#[test]
fn explicit_resource_limits_win_over_the_autonomous_preset() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("explicit-resource-limits");
    let client = Client::builder()
        .model("mock")
        .auto_approve(true)
        .resource_limits(ResourceLimits::new().with_max_live_agents(3))
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");

    assert_eq!(client.resource_limits().max_live_agents(), Some(3));
    assert_eq!(
        client.resource_limits().max_concurrent_tool_calls(),
        None,
        "an explicit host value must not be silently combined with the autonomous preset"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The raise is scoped to the autonomous posture. A supervised client — no `auto_approve`, so the
/// headless default deny applies — keeps the pre-C-444 ambient resolution, because there *is* an
/// approval boundary to fall back on.
#[test]
fn a_supervised_client_is_unchanged() {
    let _env = CleanSandboxEnv::enter();
    let dir = temp_root("supervised");
    let client = Client::builder()
        .model("mock")
        .build(Box::new(StubProvider), &dir)
        .expect("build Client");

    assert_eq!(
        client_posture(&client).settings().mode,
        SandboxMode::Off,
        "a client with no `auto_approve` has a human-owned approval boundary (the default deny), so \
         it must keep resolving the ambient posture rather than being raised"
    );

    std::fs::remove_dir_all(&dir).ok();
}
