//! The runtime adapter over the existing tool registry: a read-only [`OpRegistry`] view that presents
//! `flux_runtime::ToolRegistry` as a catalog of Flux-Lang operations, plus the [`ThingResolver`] /
//! [`ModelClient`] seams the runtime and compiler use.
//!
//! The pure op contracts ([`OpSpec`], [`OpSignature`], [`OpCatalog`], [`schema_params`]) live in
//! `flux-lang`; this module re-exports them so existing `flux_flow::registry::*` paths keep working.
//! Every Flux-Lang operation is a `flux_runtime::Tool` under the hood, so it executes through
//! `Executor::dispatch` like any other tool — the safety envelope is reused, not bypassed.

use std::collections::HashSet;

use async_trait::async_trait;

use flux_runtime::ToolRegistry;

use crate::ast::{ResolvedThing, ThingRef};
pub use flux_lang::opspec::{schema_params, OpCatalog, OpSignature, OpSpec};

/// A read-only adapter presenting the existing [`ToolRegistry`] as a registry of operations the
/// compiler can target. Existing tools *are* operations — no separate registration is required.
///
/// `advertised` optionally restricts the **catalog** (`op_names`/`signatures` — what the model is
/// shown) to a precomputed allow-set of op names (evidence-gated surfacing; see
/// [`flux_runtime::advertised_op_names`]). `None` means advertise everything. `get` is *never*
/// filtered, so a pre-authored flow that references a hidden-group op still resolves and executes.
pub struct OpRegistry<'a> {
    tools: &'a ToolRegistry,
    advertised: Option<HashSet<String>>,
}

impl<'a> OpRegistry<'a> {
    /// Wrap a tool registry, advertising every op (no gating).
    pub fn new(tools: &'a ToolRegistry) -> Self {
        Self {
            tools,
            advertised: None,
        }
    }

    /// Restrict the advertised catalog to `advertised` (the surfaced op names). Execution/resolution
    /// via [`get`](Self::get) is unaffected.
    pub fn with_advertised(mut self, advertised: HashSet<String>) -> Self {
        self.advertised = Some(advertised);
        self
    }

    fn is_advertised(&self, name: &str) -> bool {
        self.advertised.as_ref().is_none_or(|a| a.contains(name))
    }

    /// The names of every **advertised** operation.
    pub fn op_names(&self) -> Vec<String> {
        self.tools
            .names()
            .into_iter()
            .filter(|n| self.is_advertised(n))
            .collect()
    }

    /// The signature of every **advertised** operation.
    pub fn signatures(&self) -> Vec<OpSignature> {
        self.tools
            .specs()
            .iter()
            .filter(|s| self.is_advertised(&s.name))
            .map(OpSignature::from_spec)
            .collect()
    }

    /// The signature of one operation, if registered. Not filtered by surfacing — resolution must
    /// succeed for any registered op a flow names.
    pub fn get(&self, name: &str) -> Option<OpSignature> {
        self.tools
            .get(name)
            .map(|t| OpSignature::from_spec(&t.spec()))
    }
}

impl OpCatalog for OpRegistry<'_> {
    /// Delegate to the unfiltered [`get`](OpRegistry::get) so analysis accepts any registered op,
    /// advertised or not.
    fn lookup(&self, name: &str) -> Option<OpSignature> {
        self.get(name)
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
    fn with_advertised_filters_catalog_but_not_get() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        // Advertise only `read`: the catalog shrinks to it, but a hidden op still resolves via `get`.
        let allowed: HashSet<String> = ["read".to_string()].into_iter().collect();
        let ops = OpRegistry::new(&reg).with_advertised(allowed);
        assert_eq!(ops.op_names(), vec!["read".to_string()]);
        assert_eq!(ops.signatures().len(), 1);
        // `git_status` is not advertised…
        assert!(!ops.op_names().contains(&"git_status".to_string()));
        // …but still resolves for execution / pre-authored flows.
        assert!(ops.get("git_status").is_some());
    }

    #[test]
    fn signature_carries_param_names_in_required_then_optional_order() {
        let mut reg = ToolRegistry::new();
        flux_tools::register_builtins(&mut reg);
        let ops = OpRegistry::new(&reg);

        let read = ops.get("read").unwrap();
        assert_eq!(read.required_params, vec!["path".to_string()]);
        // `offset`/`limit` are optional; required order is fixed, so `path` always renders first.
        assert!(read.param_signature().starts_with("path["));

        let edit = ops.get("edit").unwrap();
        assert_eq!(
            edit.required_params,
            vec![
                "path".to_string(),
                "old_string".to_string(),
                "new_string".to_string()
            ]
        );
        assert!(edit
            .param_signature()
            .starts_with("path, old_string, new_string"));
    }
}
