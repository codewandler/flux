//! Crate-private "rebuild a variant engine" seam (D-174).
//!
//! A [`crate::Client`] assembles its [`FlowEngine`] once, at [`ClientBuilder::build`](crate::ClientBuilder::build).
//! But the Deterministic Agent Lab's Test Kit (`check()`/`inject_at`, and later Tune's `what_if`)
//! needs a SECOND, throwaway engine — same tool catalog, same permissions, same authorization floor
//! — with exactly one thing changed (a model, a system prompt, a provider), or pointed at an
//! entirely different pair of stores (a fixture's temp work-dir copy) — without perturbing the live
//! client's single-active-turn state or its own stores.
//!
//! [`AgentSpec`] and [`ExecutionEnvironment`] are both cheap [`Clone`]s (mostly `Arc` bumps — see
//! their own docs), so a variant engine is built by cloning both, applying overrides, and calling
//! [`AgentSpec::assemble_in`] again: the exact constructor `ClientBuilder::build` itself uses.
//! Building a second [`flux_runtime::Executor`] this way is safe — every executor's bookkeeping
//! state (op cache, plan-scope stack) is freshly constructed inside `into_executor`, never shared —
//! the only things two variant engines share are the real side-effecting resources their cloned
//! `ExecutionEnvironment` still points at (the guarded `System`, the approver).

use std::sync::Arc;

use flux_agent::AgentSpec;
use flux_core::Result;
use flux_events::EventStore;
use flux_flow::engine::FlowEngine;
use flux_flow::state::FlowStore;
use flux_provider::Provider;
use flux_runtime::ExecutionEnvironment;

/// The pieces [`ClientBuilder::build`](crate::ClientBuilder::build) assembled the live engine from,
/// retained so a variant engine can be built later without re-running the whole builder (registry
/// composition, permission overlay, sub-agent wiring, … already happened once, at `build`).
///
/// Crate-private: no consumer constructs or reads this directly. The `test-kit` feature's
/// `Scenario`/`Outcome` machinery (and, later, D-176's `Session::what_if`) reach it through
/// [`crate::Client`]/[`crate::Session`], which each carry an `Arc<EngineAssembly>`.
// The default (no `test-kit`) build has no caller yet — `crate::test` is the only consumer today,
// feature-gated in `lib.rs`. D-176's always-on `Session::what_if()` becomes the un-gated caller;
// until then this compiles-and-is-tested-but-unreachable-by-default, like any shared foundation
// landed ahead of its second consumer.
#[allow(dead_code)]
pub(crate) struct EngineAssembly {
    pub(crate) spec: AgentSpec,
    pub(crate) environment: ExecutionEnvironment,
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) events: Arc<EventStore>,
    pub(crate) user_interaction: Option<Arc<dyn flux_runtime::UserInteraction>>,
}

/// What a variant engine may override relative to the retained assembly. `None` fields reuse the
/// original. `#[non_exhaustive]` + [`Default`]: every new knob lands here (new `variant*` methods
/// are not the growth seam), so a caller builds one with `VariantOverrides { model: Some(..),
/// ..Default::default() }`.
#[derive(Default)]
#[non_exhaustive]
#[allow(dead_code)]
pub(crate) struct VariantOverrides {
    pub(crate) model: Option<String>,
    pub(crate) system_prompt: Option<String>,
    pub(crate) provider: Option<Arc<dyn Provider>>,
    /// D-177: re-authorize the variant under a DIFFERENT permission rule set — the "would the
    /// tightened policy have blocked this?" knob. Replaces the spec's own `allow`/`deny` wholesale
    /// (not merged: a policy-mode question is about the rules AS GIVEN, and silently unioning the
    /// original's allows would make a stricter policy untestable).
    pub(crate) permissions: Option<flux_agent::Permissions>,
}

#[allow(dead_code)]
impl EngineAssembly {
    /// Re-assemble a throwaway [`FlowEngine`] over the SAME events log the live client is built on
    /// (so it shares turn/session history — a fresh session minted here is visible to the live
    /// client too) but a fresh in-memory flow store, so it can never perturb the live client's
    /// suspended-flow state or its `FlowStore`-scoped cassette install. This is what
    /// `Scenario::record` uses: a variant engine (typically with a recording provider tee) runs one
    /// turn on a brand-new session of the client's own events, which is then exported into a fixture.
    pub(crate) fn variant(&self, overrides: VariantOverrides) -> Result<FlowEngine> {
        let flow = FlowStore::in_memory_with_events(self.events.clone())?;
        self.variant_with_stores(overrides, self.events.clone(), flow)
    }

    /// Same as [`variant`](Self::variant), but over caller-supplied stores — e.g. a `Scenario`'s
    /// temp work-dir copy of a fixture, so the variant engine's turn is recorded into (and its
    /// dispatches gated by) that copy instead of the live client's own store. Used by
    /// `Scenario::check`/`inject_at`, which must never touch the live client's stores at all.
    pub(crate) fn variant_with_stores(
        &self,
        overrides: VariantOverrides,
        events: Arc<EventStore>,
        flow: FlowStore,
    ) -> Result<FlowEngine> {
        let mut spec = self.spec.clone();
        if let Some(model) = overrides.model {
            spec.model = model;
        }
        if let Some(system_prompt) = overrides.system_prompt {
            spec.system_prompt = system_prompt;
        }
        if let Some(permissions) = overrides.permissions {
            spec.permissions = permissions;
        }
        let provider = overrides.provider.unwrap_or_else(|| self.provider.clone());
        let engine = spec.assemble_in(provider, self.environment.clone(), events, flow)?;
        Ok(match &self.user_interaction {
            Some(interaction) => engine.with_user_interaction(interaction.clone()),
            None => engine,
        })
    }
}
