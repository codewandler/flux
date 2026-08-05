//! `skill.load` — on-demand skill body loading for D-188's opt-in model-invoked progressive
//! disclosure. A thin delegator over the [`flux_runtime::SkillLoader`] capability the flow engine
//! installs on [`ToolContext`] whenever a session's model-invoked skill catalog is non-empty; this
//! op holds no catalog state of its own. Registered unconditionally in
//! [`try_register_builtins`](crate::try_register_builtins) — like `observe`/`evidence`, existence in
//! the registry is not exposure: the engine's per-turn surfacing narrows `skill.load` back out of
//! the advertised catalog whenever the opt-in catalog is empty, which is the default.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use flux_core::{Error, Result};
use flux_evidence::{Observation, Phase};
use flux_runtime::{OperationPlacement, SkillLoader, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{tool_input_schema, Idempotency, Risk, ToolSpec};

/// Register `skill.load` (D-188). Always registered — like `observe`/`evidence` — because
/// per-turn *surfacing*, not registry presence, is what stays off by default; see the module docs.
pub(crate) fn try_register_skill_load(registry: &mut ToolRegistry) -> Result<()> {
    registry.try_register_all_from_with_placement(
        "flux-tools skill-load pack",
        vec![Arc::new(SkillLoadOp) as Arc<dyn Tool>],
        OperationPlacement::LocalControlPlane,
    )
}

/// `skill.load(name) -> body` — pull one model-invocable skill's full body into context. Loading is
/// idempotent and, per D-188's design decision, makes the skill behave like an explicitly
/// `--skill`-activated one for the rest of the session (its body is re-injected on every later
/// turn, same as a manually enabled skill).
struct SkillLoadOp;

/// Arguments for the `skill.load` op.
#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SkillLoadInput {
    /// the exact `name` of a skill from the surfaced `<available-skills>` catalog
    name: String,
}

#[async_trait]
impl Tool for SkillLoadOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "skill.load".into(),
            description: "Pull one skill's full body into context by exact `name`, from the \
                          `<available-skills>` catalog surfaced in the system prompt. Only \
                          available when that opt-in catalog is non-empty. The skill stays active \
                          for the rest of this session once loaded — you don't need to reload it \
                          on later turns."
                .into(),
            input_schema: tool_input_schema::<SkillLoadInput>(),
            output_schema: None,
            // No host IO of its own: an in-memory catalog lookup plus an evidence-log append.
            effects: Vec::new(),
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
            access: Vec::new(),
            group: None,
        }
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let args: SkillLoadInput = crate::parse_params(params, "skill.load")?;
        let loader: &dyn SkillLoader = ctx.skill_loader.as_deref().ok_or_else(|| {
            Error::Other(
                "skill.load: no model-invoked skill catalog is active in this context".into(),
            )
        })?;
        let session_id = ctx.session_id().unwrap_or_default();
        let outcome = loader.load_skill(&session_id, &args.name).await?;
        // The same observation kind manual `--skill` activation emits (`base_system_with_skills`),
        // so an audit trail can't tell the two activation paths apart after the fact — by design,
        // they end up meaning the same thing.
        ctx.evidence.lock().unwrap().record(Observation::new(
            "skill.activated",
            Phase::Turn,
            serde_json::json!({ "skill": outcome.name }),
        ));
        Ok(ToolResult::ok(outcome.body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct MockLoader {
        body: String,
    }

    #[async_trait]
    impl SkillLoader for MockLoader {
        async fn load_skill(
            &self,
            _session_id: &str,
            name: &str,
        ) -> Result<flux_runtime::SkillLoadOutcome> {
            if name == "missing" {
                return Err(Error::Other("skill.load: unknown skill".into()));
            }
            Ok(flux_runtime::SkillLoadOutcome {
                name: name.to_string(),
                body: self.body.clone(),
            })
        }
    }

    fn ctx_with_loader(loader: Option<Arc<dyn SkillLoader>>) -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-skill-load-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = flux_system::System::new(flux_system::Workspace::new(&dir).unwrap())
            .with_worktree_base(crate::test_worktrees::pinned_worktree_base());
        let mut ctx = ToolContext::new(Arc::new(system));
        ctx.skill_loader = loader;
        ctx
    }

    #[tokio::test]
    async fn returns_the_body_and_records_skill_activated() {
        let ctx = ctx_with_loader(Some(Arc::new(MockLoader {
            body: "full instructions".into(),
        })));
        let out = SkillLoadOp
            .execute(&ctx, serde_json::json!({ "name": "pkg" }))
            .await
            .unwrap();
        assert_eq!(out.content, "full instructions");
        let evidence = ctx.evidence.lock().unwrap();
        let recorded = evidence.all();
        assert!(
            recorded
                .iter()
                .any(|o| o.kind == "skill.activated" && o.data["skill"] == "pkg"),
            "expected a skill.activated observation for `pkg`: {recorded:?}"
        );
    }

    #[tokio::test]
    async fn errors_clearly_without_an_installed_loader() {
        let ctx = ctx_with_loader(None);
        let error = SkillLoadOp
            .execute(&ctx, serde_json::json!({ "name": "pkg" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no model-invoked skill catalog"));
    }

    #[tokio::test]
    async fn propagates_an_unknown_skill_error() {
        let ctx = ctx_with_loader(Some(Arc::new(MockLoader {
            body: String::new(),
        })));
        let error = SkillLoadOp
            .execute(&ctx, serde_json::json!({ "name": "missing" }))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown skill"));
    }
}
