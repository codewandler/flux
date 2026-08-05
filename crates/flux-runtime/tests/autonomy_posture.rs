//! C-463: the autonomy posture is one named choice, and the envelope does not vary with it.
//!
//! Two things are proved here, and they are the whole story:
//!
//! 1. **Coherence.** A posture that stops asking a human carries its confinement *and* its ceiling
//!    with it. Approval, isolation and budget are read off one value, so a caller cannot set the
//!    first and forget the other two — the bug C-444 found from the SDK side, given a name here.
//! 2. **Invariance.** Authorization, guarded IO and evidence are identical under every posture.
//!    Approval is the only stage of *authorization → approval → guarded IO* with a human in it, so
//!    varying that stage is choosing a posture; if either of the other two moved, that would be a
//!    bug rather than a choice. This second suite is what makes the first safe to ship.

use std::sync::Arc;

use async_trait::async_trait;
use flux_core::Result;
use flux_policy::{
    Action, AuthorizationPolicy, Grant, ResourceRef, SubjectKind, SubjectRef, TrustLevel,
};
use flux_runtime::{
    ApprovalChoice, ApprovalStance, Approver, AutonomyPosture, Executor, PermissionManager, Tool,
    ToolContext, ToolRegistry, ToolResult,
};
use flux_spec::{AccessKind, Effect, IntentSet, ToolSpec};
use flux_system::sandbox::SandboxMode;
use flux_system::{System, Workspace};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// 1. Coherence — one named choice, not three flags
// ---------------------------------------------------------------------------

/// The exact bug this story prevents: a posture that sets approval without setting isolation.
///
/// Read as a whole with [`a_posture_that_stops_asking_carries_a_ceiling`]: between them they say
/// that "nobody is asked" and "nothing confines it" cannot be selected apart, because there is one
/// value to select and it answers all three questions.
#[test]
fn a_posture_that_stops_asking_carries_its_confinement() {
    for posture in AutonomyPosture::ALL {
        if posture.approval() != ApprovalStance::None {
            continue;
        }
        let floor = posture.sandbox_floor();
        assert_eq!(
            floor.mode,
            SandboxMode::Require,
            "{posture}: a posture with no human in the approval stage must carry a fail-closed \
             confinement floor — the constraint budget moves from human latency to isolation, so \
             the isolation has to be part of the same choice"
        );
    }
}

/// The other half of the same claim, split out so a regression names which half broke.
#[test]
fn a_posture_that_stops_asking_carries_a_ceiling() {
    for posture in AutonomyPosture::ALL {
        if posture.approval() != ApprovalStance::None {
            continue;
        }
        assert!(
            !posture.budget().is_unbounded(),
            "{posture}: unattended *and* unbounded is the configuration C-444 found; a posture \
             that never prompts must state a ceiling"
        );
    }
}

/// The starting set is four argued postures, not an extensible preset scheme. If a fifth is ever
/// added it should be because someone argued for it here.
#[test]
fn the_starting_set_is_four_named_postures() {
    let names: Vec<&str> = AutonomyPosture::ALL.iter().map(|p| p.name()).collect();
    assert_eq!(
        names,
        vec!["supervised", "bounded-autonomy", "exploratory", "refusing"]
    );
    for posture in AutonomyPosture::ALL {
        assert_eq!(
            posture.name().parse::<AutonomyPosture>().unwrap(),
            posture,
            "every posture must round-trip through its own name"
        );
    }
    assert!(
        "unsafe".parse::<AutonomyPosture>().is_err(),
        "an unknown posture name is refused, never guessed"
    );
}

/// ⚠ Acceptance: nothing presents an autonomous posture as degraded. Each one states what it
/// *relies on*, and each one states what it does **not** protect against — the honest version of
/// the same sentence, and the reason the first is not marketing.
#[test]
fn every_posture_states_what_it_relies_on_and_what_it_does_not_protect_against() {
    // Words that frame a legitimate choice as a defect. A posture description may not use them.
    const DEGRADING: [&str; 6] = [
        "unsafe",
        "insecure",
        "dangerous",
        "safety off",
        "disabled safety",
        "no safety",
    ];
    for posture in AutonomyPosture::ALL {
        let relies_on = posture.relies_on();
        let gap = posture.does_not_protect_against();
        assert!(
            !relies_on.is_empty(),
            "{posture}: must say what constrains it"
        );
        assert!(
            !gap.is_empty(),
            "{posture}: must name the constraint the operator is now leaning on"
        );
        let prose = format!("{relies_on} {gap} {}", posture.announcement()).to_ascii_lowercase();
        for word in DEGRADING {
            assert!(
                !prose.contains(word),
                "{posture}: describes a legitimate posture as {word:?}"
            );
        }
    }
}

/// `--yes` / `auto_approve(true)` is not a fourth thing: it is a spelling of one named posture.
/// No flag day — the mapping is what keeps the existing flag meaningful.
#[test]
fn auto_approval_maps_onto_bounded_autonomy() {
    assert_eq!(
        AutonomyPosture::for_auto_approval(),
        AutonomyPosture::BoundedAutonomy
    );
    let posture = AutonomyPosture::for_auto_approval();
    assert_eq!(posture.approval(), ApprovalStance::None);
    assert_eq!(posture.sandbox_floor().mode, SandboxMode::Require);
    assert!(
        !posture.sandbox_floor().network,
        "today's unattended CLI profile closes the sandbox network (C-410); the named posture is \
         that profile, not a new one"
    );
}

/// The supervised posture *is* the surface's human channel. A surface with none cannot offer it,
/// and must be told so rather than silently getting an allow-all or a deny-all under a name that
/// promises a human.
#[test]
fn supervised_needs_the_surfaces_human_channel_and_the_others_do_not() {
    assert!(
        AutonomyPosture::Supervised.approver(None).is_none(),
        "supervised without a channel is not resolvable — it must not degrade silently"
    );
    let human: Arc<dyn Approver> = Arc::new(ScriptedApprover(ApprovalChoice::Allow));
    assert!(AutonomyPosture::Supervised.approver(Some(human)).is_some());
    for posture in [
        AutonomyPosture::BoundedAutonomy,
        AutonomyPosture::Exploratory,
        AutonomyPosture::Refusing,
    ] {
        assert!(
            posture.approver(None).is_some(),
            "{posture} is fully determined by the posture; it needs no channel"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Invariance — the envelope does not move
// ---------------------------------------------------------------------------

/// ⚠ The most important test in this change.
///
/// Authorization is pure and default-deny, and it runs *before* approval. An auto-approving posture
/// answers the approval gate; it does not widen the grant set. If this ever passes for only three
/// postures, autonomy has stopped being a posture and become a bypass.
#[tokio::test]
async fn authorization_denies_the_same_op_under_every_posture() {
    for posture in AutonomyPosture::ALL {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool));
        // A permissive allow-rule, so the permission layer is not what refuses.
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["save".to_string()], &[]),
            posture
                .approver(Some(Arc::new(ScriptedApprover(ApprovalChoice::Allow))))
                .expect("every posture resolves given a human channel"),
            test_ctx(),
        )
        .with_policy(read_only_policy());

        let result = executor.dispatch("save", json!({})).await;
        assert!(
            result.is_error && result.content.contains("denied by policy"),
            "{posture}: authorization must refuse an ungranted op regardless of posture — got {:?}",
            result.content
        );
    }
}

/// Guarded IO is the only path to the outside world, under every posture. A workspace escape is
/// refused by the guarded `System` itself, which no posture can reach or reconfigure — the posture
/// type exposes an approval stance, a confinement floor and a budget, and deliberately nothing that
/// selects or bypasses a substrate.
#[tokio::test]
async fn guarded_io_refuses_a_workspace_escape_under_every_posture() {
    for posture in AutonomyPosture::ALL {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadTool));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["file_read".to_string()], &[]),
            posture
                .approver(Some(Arc::new(ScriptedApprover(ApprovalChoice::Allow))))
                .expect("every posture resolves given a human channel"),
            test_ctx(),
        );

        let result = executor
            .dispatch("file_read", json!({"path": "../../../etc/passwd"}))
            .await;
        assert!(
            result.is_error,
            "{posture}: guarded IO must refuse a workspace escape — got {:?}",
            result.content
        );
    }
}

/// Evidence is recorded under every posture. The autonomous postures are the ones that need it
/// most: when the prompt is gone, the audit trail is the only account of what happened.
#[tokio::test]
async fn evidence_records_the_call_under_every_posture() {
    for posture in AutonomyPosture::ALL {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(ReadTool));
        let executor = Executor::new(
            registry,
            PermissionManager::from_rules(&["file_read".to_string()], &[]),
            posture
                .approver(Some(Arc::new(ScriptedApprover(ApprovalChoice::Allow))))
                .expect("every posture resolves given a human channel"),
            test_ctx(),
        );

        let _ = executor
            .dispatch("file_read", json!({"path": "missing.txt"}))
            .await;
        let log = executor.evidence();
        assert!(
            !log.all().is_empty(),
            "{posture}: the dispatch left no evidence at all"
        );
        assert!(
            log.all().iter().any(|o| o.kind == "tool_call"),
            "{posture}: every posture must record the call it made — kinds seen: {:?}",
            log.all().iter().map(|o| &o.kind).collect::<Vec<_>>()
        );
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn test_ctx() -> ToolContext {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("flux-posture-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
}

/// Grants reads only — a write is outside the grant set, so default-deny refuses it.
fn read_only_policy() -> AuthorizationPolicy {
    AuthorizationPolicy {
        grants: vec![Grant {
            subjects: vec![SubjectRef {
                kind: SubjectKind::User,
                id: "*".into(),
            }],
            resources: vec![ResourceRef::path("*")],
            actions: vec![Action::from("workspace.read")],
            required_trust: TrustLevel::Untrusted,
            required_scopes: Vec::new(),
            requires_approval: false,
        }],
    }
}

struct ScriptedApprover(ApprovalChoice);

#[async_trait]
impl Approver for ScriptedApprover {
    async fn request(
        &self,
        _tool: &str,
        _subjects: &[String],
        _intents: &IntentSet,
    ) -> ApprovalChoice {
        self.0.clone()
    }
}

/// A write-shaped tool: outside a read-only grant set, so authorization refuses it.
struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only("save", "save", json!({"type": "object"}))
            .with_effects(vec![Effect::Write, Effect::Filesystem])
            .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["out.txt".into()]
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        Ok(ToolResult::ok("saved"))
    }
}

/// A read that goes through the guarded `System`, so a workspace escape is refused there.
struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only("file_read", "read a file", json!({"type": "object"}))
            .with_access(vec![AccessKind::Filesystem])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("path")
            .and_then(Value::as_str)
            .map(|path| vec![path.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default();
        Ok(ToolResult::ok(ctx.system().read_file(path).await?))
    }
}
