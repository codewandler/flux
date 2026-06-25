//! The operation registry: the typed [`OpSpec`] (which lowers to a [`flux_spec::ToolSpec`]), a
//! read-only [`OpRegistry`] view over the existing [`flux_runtime::ToolRegistry`], and the
//! [`ThingResolver`] / [`ModelClient`] seams the runtime and compiler will use.
//!
//! Every Flux-Lang operation is a `flux_runtime::Tool` under the hood, so it executes through
//! `Executor::dispatch` like any other tool — the safety envelope is reused, not bypassed.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use flux_runtime::ToolRegistry;
use flux_spec::{Effect, Idempotency, Risk, ToolSpec};

use crate::ast::{FlowEffect, ResolvedThing, ThingRef, TypeRef};

/// The typed specification of a Flux-Lang operation. Carries richer language metadata than a
/// [`ToolSpec`] (typed I/O, semantic effects) and lowers onto one via [`OpSpec::lower`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<TypeRef>,
    pub output: TypeRef,
    #[serde(default)]
    pub effects: Vec<FlowEffect>,
    pub risk: Risk,
    pub idempotency: Idempotency,
}

impl OpSpec {
    /// Lower to a host [`ToolSpec`] so the op can be registered and dispatched through the existing
    /// envelope. Semantic effects collapse to their host-resource [`Effect`]s (deduped); the typed
    /// signature is not yet projected to JSON Schema (a generic object schema is used for now).
    pub fn lower(&self) -> ToolSpec {
        let mut effects: Vec<Effect> = Vec::new();
        for e in &self.effects {
            if let (Some(host), _) = e.lower() {
                if !effects.contains(&host) {
                    effects.push(host);
                }
            }
        }
        ToolSpec {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: serde_json::json!({ "type": "object" }),
            output_schema: None,
            effects,
            risk: self.risk,
            idempotency: self.idempotency,
            access: Vec::new(),
        }
    }
}

/// The compiler/analyzer's view of an available operation, derived from a registered [`ToolSpec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpSignature {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub effects: Vec<Effect>,
    pub risk: Risk,
    pub idempotency: Idempotency,
}

impl OpSignature {
    /// Derive an op signature from a registered tool spec.
    pub fn from_spec(spec: &ToolSpec) -> Self {
        Self {
            name: spec.name.clone(),
            description: spec.description.clone(),
            effects: spec.effects.clone(),
            risk: spec.risk,
            idempotency: spec.idempotency,
        }
    }
}

/// A read-only adapter presenting the existing [`ToolRegistry`] as a registry of operations the
/// compiler can target. Existing tools *are* operations — no separate registration is required.
pub struct OpRegistry<'a> {
    tools: &'a ToolRegistry,
}

impl<'a> OpRegistry<'a> {
    /// Wrap a tool registry.
    pub fn new(tools: &'a ToolRegistry) -> Self {
        Self { tools }
    }

    /// The names of every available operation.
    pub fn op_names(&self) -> Vec<String> {
        self.tools.names()
    }

    /// The signature of every available operation.
    pub fn signatures(&self) -> Vec<OpSignature> {
        self.tools
            .specs()
            .iter()
            .map(OpSignature::from_spec)
            .collect()
    }

    /// The signature of one operation, if registered.
    pub fn get(&self, name: &str) -> Option<OpSignature> {
        self.tools
            .get(name)
            .map(|t| OpSignature::from_spec(&t.spec()))
    }
}

/// Resolves an unresolved [`ThingRef`] to an exact identity. Implementations live outside this crate
/// (they perform IO); the runtime resolves things at execution time before any side effect.
#[async_trait]
pub trait ThingResolver: Send + Sync {
    /// Resolve a thing reference to an exact identity.
    async fn resolve(&self, thing: &ThingRef) -> flux_core::Result<ResolvedThing>;
}

/// The narrow seam a `!model` operation uses to call a provider from inside the execution envelope —
/// a thin trait over `flux_provider::Provider` so `flux-runtime`'s `ToolContext` can carry it without
/// depending on a concrete provider.
#[async_trait]
pub trait ModelClient: Send + Sync {
    /// Complete a prompt, optionally constrained to a JSON schema, returning the raw model output.
    async fn complete(
        &self,
        prompt: &str,
        schema: Option<&serde_json::Value>,
    ) -> flux_core::Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_registry_lists_the_same_ops_as_the_tool_registry() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);

        let ops = OpRegistry::new(&reg);
        let mut from_ops = ops.op_names();
        let mut from_tools = reg.names();
        from_ops.sort();
        from_tools.sort();

        assert_eq!(from_ops, from_tools);
        assert!(!from_ops.is_empty(), "builtins should register some ops");
        assert_eq!(ops.signatures().len(), from_tools.len());
        assert!(ops.get("read").is_some());
    }

    #[test]
    fn opspec_lowers_preserving_name_risk_and_host_effects() {
        let spec = OpSpec {
            name: "kb.search".into(),
            description: "search the knowledge base".into(),
            inputs: vec![TypeRef::String],
            output: TypeRef::Named("List".into()),
            effects: vec![
                FlowEffect::Read,
                FlowEffect::Network,
                FlowEffect::SendExternal,
            ],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
        };
        let tool = spec.lower();

        assert_eq!(tool.name, "kb.search");
        assert_eq!(tool.risk, Risk::Low);
        assert_eq!(tool.idempotency, Idempotency::Idempotent);
        assert!(tool.effects.contains(&Effect::Read));
        // Network appears once even though both Network and SendExternal lower onto it.
        assert_eq!(
            tool.effects
                .iter()
                .filter(|e| **e == Effect::Network)
                .count(),
            1
        );
    }
}
