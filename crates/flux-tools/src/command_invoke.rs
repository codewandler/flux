//! `command.invoke` (D-187, absorbs C-93): let the agent invoke a discovered command file or
//! skill mid-turn, but ONLY when three independently-enforced, fail-closed gates all pass:
//!
//! 1. **permitted** — the caller's policy grants the `command.invoke` operation for this exact
//!    target (`AuthorityRequirement::operation`, checked by `Executor::dispatch` before
//!    `execute` ever runs — this module does not implement that gate itself).
//! 2. **accessible** — the named target is discovered in this session: re-running the same
//!    guarded discovery (`flux_runtime::metadata::discover_commands` /
//!    `discover_skills`) the engine already used to detect the `agent_triggerable` evidence
//!    signal (`flux_runtime::detect_signals`), so the two can never disagree about what is
//!    discoverable.
//! 3. **agent-triggerable** — the discovered target's own frontmatter opts in
//!    (`agent-triggerable: true`, default `false`); a human-only target is refused even when
//!    accessible and permitted.
//!
//! Any missing gate is refused with a clean, recoverable [`ToolResult`] error — never executed,
//! never a partial invocation. Invoking a **command** expands `$ARGUMENTS`/`$1..$9` (via the
//! existing `expand_command_arguments`) and returns the substituted body as prompt-text for the
//! model's current turn; it does not execute the body or start a nested turn. Invoking a
//! **skill** returns its body — equivalent to reading it. This is deliberately the narrower
//! capability, not a reproduction of a slash command's human-side REPL/TUI effects.

use std::path::Path;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_policy::{Action, ResourceKind, ResourceRef};
use flux_runtime::{AuthorityRequirement, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};

/// The evidence-gated group `command.invoke` belongs to (surfaced only when
/// [`flux_runtime::detect_signals`] finds at least one agent-triggerable command or skill).
pub const GROUP: &str = "agent_invoke";

/// Register the `command.invoke` op.
pub fn try_register_command_invoke(registry: &mut ToolRegistry) -> Result<()> {
    registry.try_register_all_from(
        "flux-tools agent-invocation pack (D-187)",
        vec![std::sync::Arc::new(CommandInvokeTool) as std::sync::Arc<dyn Tool>],
    )
}

/// Compatibility wrapper for pre-fallible pack installers.
///
/// # Deprecated
///
/// Production assembly should call [`try_register_command_invoke`].
pub fn register_command_invoke(registry: &mut ToolRegistry) {
    try_register_command_invoke(registry).expect("flux-tools command.invoke registration failed");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
enum InvokeKind {
    Command,
    Skill,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommandInvokeInput {
    /// Which discovered target kind to invoke: "command" (a `.flux/commands`/`.claude/commands`
    /// file) or "skill" (a discovered skill).
    kind: InvokeKind,
    /// The target's discovery-time identity: a command's filename stem, or a skill's `name`.
    name: String,
    /// Raw argument text substituted into a command's `$ARGUMENTS`/`$1..$9`. Ignored for skills.
    #[serde(default)]
    arguments: String,
}

struct CommandInvokeTool;

#[async_trait]
impl Tool for CommandInvokeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "command.invoke".into(),
            description: "Invoke a discovered command file or skill that has explicitly opted \
                          into agent invocation. Three independent gates must ALL pass: your \
                          policy permits this exact target, the target is discovered in this \
                          session, and its frontmatter declares `agent-triggerable: true` \
                          (default false — most commands/skills are human-only and this call \
                          refuses them). Invoking kind=\"command\" expands $ARGUMENTS/$1..$9 and \
                          returns the substituted body as prompt text for you to use — it does \
                          not execute anything or start a nested turn. Invoking kind=\"skill\" \
                          returns the skill body, equivalent to reading it. An unknown, \
                          human-only, or policy-denied target is refused with a clear error, \
                          never executed."
                .into(),
            input_schema: flux_spec::tool_input_schema::<CommandInvokeInput>(),
            output_schema: None,
            effects: vec![Effect::Read, Effect::Filesystem],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: vec![AccessKind::Filesystem],
            group: Some(GROUP.into()),
        }
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("command");
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        vec![format!("{kind}:{name}")]
    }

    /// The **permitted** gate (1 of 3): a named-operation authority requirement per subject, so a
    /// policy grant must name `command.invoke` (or a matching wildcard) for this exact
    /// `kind:name` target — a bare workspace-read grant does not imply permission to invoke.
    fn authority_requirements(
        &self,
        _params: &Value,
        subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        Ok(subjects
            .iter()
            .map(|subject| {
                AuthorityRequirement::new(
                    Action::from("command.invoke"),
                    ResourceRef::named(ResourceKind::Operation, subject.clone()),
                )
            })
            .collect())
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: CommandInvokeInput = crate::parse_params(params, "command.invoke")?;
        let root = ctx.system.workspace().root();
        match args.kind {
            InvokeKind::Command => invoke_command(root, &args.name, &args.arguments),
            InvokeKind::Skill => invoke_skill(root, &args.name),
        }
    }
}

/// The **accessible** gate (2 of 3): re-run the same command discovery
/// [`flux_runtime::detect_signals`] uses, then the **agent-triggerable** gate (3 of 3) on the
/// match. Unknown name or an untriggerable target both degrade to a clean [`ToolResult::error`],
/// never a hard `Err` — this is a normal, recoverable refusal, not a system fault.
fn invoke_command(root: &Path, name: &str, arguments: &str) -> Result<ToolResult> {
    let discovery = flux_runtime::metadata::discover_commands(root)
        .map_err(|e| Error::Other(format!("command.invoke: discover commands: {e}")))?;
    let Some(command) = discovery.commands.iter().find(|c| c.name == name) else {
        return Ok(ToolResult::error(format!(
            "command.invoke: no command named `{name}` is discovered in this session"
        )));
    };
    if !command.agent_triggerable {
        return Ok(ToolResult::error(format!(
            "command.invoke: command `{name}` is not agent-triggerable (human-only)"
        )));
    }
    let body = flux_runtime::metadata::expand_command_arguments(&command.body, arguments);
    Ok(ToolResult::ok(body))
}

/// Skill counterpart of [`invoke_command`]: same accessible + agent-triggerable gates, returning
/// the skill body verbatim (no argument substitution — skills don't declare `$ARGUMENTS`).
fn invoke_skill(root: &Path, name: &str) -> Result<ToolResult> {
    let skills = flux_runtime::metadata::discover_skills(root, &[])
        .map_err(|e| Error::Other(format!("command.invoke: discover skills: {e}")))?;
    let Some(skill) = skills.iter().find(|s| s.name == name) else {
        return Ok(ToolResult::error(format!(
            "command.invoke: no skill named `{name}` is discovered in this session"
        )));
    };
    if !skill.agent_triggerable {
        return Ok(ToolResult::error(format!(
            "command.invoke: skill `{name}` is not agent-triggerable (human-only)"
        )));
    }
    Ok(ToolResult::ok(skill.body.text().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_policy::{
        Action, AuthorizationPolicy, Grant, ResourceKind, ResourceRef, SubjectKind, SubjectRef,
        TrustLevel,
    };
    use flux_runtime::{AllowApprover, Executor, PermissionManager, ToolContext, ToolRegistry};
    use flux_system::{System, Workspace};
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "flux-command-invoke-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write_command(root: &Path, name: &str, agent_triggerable: bool) {
        std::fs::create_dir_all(root.join(".flux/commands")).unwrap();
        let flag = if agent_triggerable {
            "agent-triggerable: true\n"
        } else {
            ""
        };
        std::fs::write(
            root.join(format!(".flux/commands/{name}.md")),
            format!("---\ndescription: d\n{flag}---\nhello $1"),
        )
        .unwrap();
    }

    fn ctx_for(root: &Path) -> ToolContext {
        ToolContext::new(Arc::new(System::new(Workspace::new(root).unwrap())))
    }

    fn grant_command_invoke() -> AuthorizationPolicy {
        AuthorizationPolicy {
            grants: vec![Grant {
                subjects: vec![SubjectRef {
                    kind: SubjectKind::User,
                    id: "*".into(),
                }],
                resources: vec![ResourceRef::any(ResourceKind::Operation)],
                actions: vec![Action::from("command.invoke")],
                required_trust: TrustLevel::Untrusted,
                required_scopes: Vec::new(),
                requires_approval: false,
            }],
        }
    }

    fn no_grants() -> AuthorizationPolicy {
        AuthorizationPolicy { grants: Vec::new() }
    }

    fn executor(root: &Path, policy: AuthorizationPolicy) -> Executor {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CommandInvokeTool));
        Executor::new(
            registry,
            PermissionManager::from_rules(&["command.invoke".into()], &[]),
            Arc::new(AllowApprover),
            ctx_for(root),
        )
        .with_policy(policy)
    }

    /// Gate matrix (a): permitted + accessible + agent-triggerable → runs, returns the
    /// substituted body.
    #[tokio::test]
    async fn triggerable_permitted_accessible_command_runs() {
        let root = temp_dir("a-command");
        write_command(&root, "greet", true);
        let ex = executor(&root, grant_command_invoke());
        let result = ex
            .dispatch(
                "command.invoke",
                json!({"kind": "command", "name": "greet", "arguments": "world"}),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.content, "hello world");
        std::fs::remove_dir_all(root).ok();
    }

    /// Gate matrix (b): accessible + permitted, but NOT agent-triggerable → refused, never
    /// executed (recoverable error, not a hard failure).
    #[tokio::test]
    async fn human_only_target_is_refused() {
        let root = temp_dir("b-human-only");
        write_command(&root, "deploy", false);
        let ex = executor(&root, grant_command_invoke());
        let result = ex
            .dispatch(
                "command.invoke",
                json!({"kind": "command", "name": "deploy", "arguments": ""}),
            )
            .await;
        assert!(result.is_error, "{}", result.content);
        assert!(
            result.content.contains("not agent-triggerable"),
            "{}",
            result.content
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// Gate matrix (c): permitted + agent-triggerable, but NOT accessible (no such command is
    /// discovered in this session) → refused.
    #[tokio::test]
    async fn inaccessible_target_is_refused() {
        let root = temp_dir("c-inaccessible");
        // No command files at all — "ghost" cannot be discovered.
        let ex = executor(&root, grant_command_invoke());
        let result = ex
            .dispatch(
                "command.invoke",
                json!({"kind": "command", "name": "ghost", "arguments": ""}),
            )
            .await;
        assert!(result.is_error, "{}", result.content);
        assert!(
            result.content.contains("no command named"),
            "{}",
            result.content
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// Gate matrix (d): accessible + agent-triggerable, but the policy grants no `command.invoke`
    /// authority → denied by the dispatcher before `execute` ever runs.
    #[tokio::test]
    async fn policy_denied_target_is_refused() {
        let root = temp_dir("d-policy-denied");
        write_command(&root, "greet", true);
        let ex = executor(&root, no_grants());
        let outcome = ex
            .dispatch_outcome(
                "command.invoke",
                json!({"kind": "command", "name": "greet", "arguments": "world"}),
            )
            .await;
        assert!(outcome.denied, "{}", outcome.result.content);
        assert!(
            outcome.result.content.contains("denied by policy")
                || outcome.result.content.contains("policy"),
            "{}",
            outcome.result.content
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// A skill target runs the same three gates, returning its body (equivalent to reading it) —
    /// no `$ARGUMENTS` substitution.
    #[tokio::test]
    async fn triggerable_permitted_accessible_skill_runs() {
        let root = temp_dir("skill-a");
        std::fs::create_dir_all(root.join(".flux/skills")).unwrap();
        std::fs::write(
            root.join(".flux/skills/reviewer.md"),
            "---\nname: reviewer\ndescription: d\nagent-triggerable: true\n---\nReview checklist.",
        )
        .unwrap();
        let ex = executor(&root, grant_command_invoke());
        let result = ex
            .dispatch(
                "command.invoke",
                json!({"kind": "skill", "name": "reviewer"}),
            )
            .await;
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.content, "Review checklist.");
        std::fs::remove_dir_all(root).ok();
    }

    /// D-187: caller identity is frozen for the turn — dispatching `command.invoke` neither
    /// requires nor mutates a `TurnIdentity`; a lexically scoped identity is unaffected by the
    /// call and does not leak into a later dispatch outside that scope.
    #[tokio::test]
    async fn dispatch_does_not_touch_frozen_turn_identity() {
        use flux_policy::{Caller, CallerKind, Principal, Trust, TrustKind};

        let root = temp_dir("identity");
        write_command(&root, "greet", true);
        let ex = executor(&root, grant_command_invoke());

        let caller = Caller {
            principal: Principal {
                id: "alice".into(),
                name: "alice".into(),
                kind: CallerKind::User,
            },
            groups: Vec::new(),
            source: "test".into(),
        };
        let trust = Trust {
            kind: TrustKind::Invocation,
            level: TrustLevel::Verified,
            scopes: Vec::new(),
        };
        let identity = flux_runtime::TurnIdentity::new(caller, trust);
        let result = flux_runtime::scope_runtime_turn(
            flux_runtime::RuntimeTurnContext::new().with_identity(identity.clone()),
            ex.dispatch(
                "command.invoke",
                json!({"kind": "command", "name": "greet", "arguments": "world"}),
            ),
        )
        .await;
        assert!(!result.is_error, "{}", result.content);

        // Outside the scope, the executor's default (unset) identity is unaffected — the call did
        // not leave a mutable trace on the executor itself.
        let default_context: Value = serde_json::from_str(&ex.approval_context()).unwrap();
        assert_ne!(
            default_context["caller"]["principal"]["id"], "alice",
            "alice's lexically scoped identity must not stick to the executor after dispatch"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// Evidence-gated surfacing: `try_register_command_invoke` always registers the tool, but its
    /// `group` tag is what a group-aware surfacing layer keys on — asserted directly rather than
    /// standing up a full `FlowEngine`, mirroring `spec_group_tag_is_honored_without_a_manifest_tools_list`.
    #[test]
    fn spec_carries_the_agent_invoke_group_tag() {
        assert_eq!(CommandInvokeTool.spec().group.as_deref(), Some(GROUP));
    }
}
