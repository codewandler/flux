//! The pure operation contracts: the typed [`OpSpec`] (which lowers to a [`flux_spec::ToolSpec`]),
//! the [`OpSignature`] the compiler and analyzer reason over, and the abstract [`OpCatalog`] the
//! analyzer validates against.
//!
//! None of this depends on a concrete tool registry. The runtime adapter that presents the real
//! `flux_runtime::ToolRegistry` as an [`OpCatalog`] lives in the engine crate (`flux-flow`'s
//! `registry` module) — keeping the language core free of any dependency on actual tools/ops.

use serde::{Deserialize, Serialize};

use flux_spec::{Effect, Idempotency, Risk, ToolSpec};

use crate::ast::{FlowEffect, TypeRef};
use crate::program::CompositeOpDecl;

/// A single named input parameter of an [`OpSpec`]: a `name`, its [`TypeRef`], and whether it may be
/// omitted. Naming the param here — rather than leaving `inputs` positional — is what lets
/// [`OpSpec::lower`] project a faithful JSON Schema whose `properties`/`required` the planner catalog
/// and [`schema_params`] read back to recover the op's parameter *set* (required vs optional).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// When true, the param is omitted from the schema's `required` array (it still appears in
    /// `properties`). Parameter order is non-load-bearing — calls name their args via a single
    /// object — so `optional` only controls `required` membership.
    #[serde(default)]
    pub optional: bool,
}

/// The typed specification of a Flux-Lang operation. Carries richer language metadata than a
/// [`ToolSpec`] (typed, *named* I/O, semantic effects) and lowers onto one via [`OpSpec::lower`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpSpec {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub inputs: Vec<Param>,
    pub output: TypeRef,
    #[serde(default)]
    pub effects: Vec<FlowEffect>,
    pub risk: Risk,
    pub idempotency: Idempotency,
}

impl OpSpec {
    /// Lower to a host [`ToolSpec`] so the op can be registered and dispatched through the existing
    /// envelope. Semantic effects collapse to their host-resource [`Effect`]s (deduped); the typed,
    /// named [`inputs`](Self::inputs) project to a real JSON Schema object via [`Self::input_schema`].
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
            input_schema: self.input_schema(),
            output_schema: None,
            effects,
            risk: self.risk,
            idempotency: self.idempotency,
            access: Vec::new(),
            group: None,
        }
    }

    /// Project the named, typed [`inputs`](Self::inputs) onto a JSON Schema object: every param
    /// becomes a `properties` entry (its [`TypeRef`] via [`type_ref_to_schema`]), and every
    /// non-`optional` param is listed in the `required` array. [`schema_params`] reads the schema
    /// back to recover the required/optional parameter **sets** — membership is load-bearing, order
    /// is not: calls name their args via a single object (see `map_args_to_input`), and the declared
    /// order the `required` array happens to preserve is used only for stable display.
    pub fn input_schema(&self) -> serde_json::Value {
        let mut properties = serde_json::Map::new();
        let mut required: Vec<serde_json::Value> = Vec::new();
        for p in &self.inputs {
            properties.insert(p.name.clone(), type_ref_to_schema(&p.ty));
            if !p.optional {
                required.push(serde_json::Value::String(p.name.clone()));
            }
        }
        serde_json::json!({
            "type": "object",
            "properties": serde_json::Value::Object(properties),
            "required": serde_json::Value::Array(required),
        })
    }

    /// Derive this op's [`OpSignature`] directly from the typed spec — the D-138 counterpart of
    /// [`Self::lower`] that does NOT erase the semantic tier. [`Self::lower`]'s [`ToolSpec`] can only
    /// carry the host-resource effects [`FlowEffect::lower`] projects (`Money` vanishes entirely,
    /// `Delete`/`SendExternal` collapse into `Write`/`Network`), because a `ToolSpec` has no room for
    /// a semantic-effect field. This method still calls [`Self::lower`] for every host-facing field
    /// (name, schema, host effects, risk, idempotency), but additionally copies the ORIGINAL,
    /// undegraded [`effects`](Self::effects) onto [`OpSignature::semantic_effects`] — so a consumer
    /// reading the signature (the SDK catalog, a downstream visual editor, `annotate_effects`) can
    /// see `Money`/`Delete`/`SendExternal` even though no host `Effect` distinguishes them.
    pub fn to_signature(&self) -> OpSignature {
        let mut sig = OpSignature::from_spec(&self.lower());
        let mut semantic: Vec<FlowEffect> = Vec::new();
        for e in &self.effects {
            if !semantic.contains(e) {
                semantic.push(*e);
            }
        }
        sig.semantic_effects = semantic;
        sig
    }
}

/// Project a [`TypeRef`] onto a JSON Schema fragment. A `Named` type renders as a `$ref` into
/// `#/$defs/<name>` — forward-compatible with the registered-type definitions (the prelude) a later
/// phase adds; an as-yet-unresolved `$ref` is still a stable, valid schema node. `Any` is the
/// unconstrained schema (`{}`), matching "the top type."
fn type_ref_to_schema(ty: &TypeRef) -> serde_json::Value {
    match ty {
        TypeRef::Any => serde_json::json!({}),
        TypeRef::Bool => serde_json::json!({ "type": "boolean" }),
        TypeRef::Number => serde_json::json!({ "type": "number" }),
        TypeRef::String => serde_json::json!({ "type": "string" }),
        TypeRef::List(inner) => serde_json::json!({
            "type": "array",
            "items": type_ref_to_schema(inner),
        }),
        TypeRef::Named(name) => serde_json::json!({ "$ref": format!("#/$defs/{name}") }),
    }
}

/// The input parameter names of a tool's JSON-Schema, as `(required, optional)`. Treated as
/// **sets** — membership is load-bearing ("which params must be present / which may be omitted"),
/// order is not. `required` follows the schema's `required` array order; `optional` is the
/// `properties` keys not in `required`, sorted for stable display. Neither order is used for
/// positional binding — calls name their args via a single object (see `map_args_to_input`);
/// `param_signature` renders the same names for the planner catalog.
pub fn schema_params(schema: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let required: Vec<String> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let mut optional = Vec::new();
    if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
        for k in props.keys() {
            if !required.contains(k) {
                optional.push(k.clone());
            }
        }
        optional.sort();
    }
    (required, optional)
}

/// Recover a [`TypeRef`] from one JSON-Schema property — the inverse of [`type_ref_to_schema`], used
/// to populate an [`OpSignature`]'s `param_types`. Unknown/untyped shapes become [`TypeRef::Any`].
fn schema_prop_type(prop: &serde_json::Value) -> TypeRef {
    if let Some(r) = prop.get("$ref").and_then(|v| v.as_str()) {
        let name = r.rsplit('/').next().unwrap_or(r);
        return TypeRef::Named(name.to_string());
    }
    match prop.get("type").and_then(|v| v.as_str()) {
        Some("string") => TypeRef::String,
        Some("number") | Some("integer") => TypeRef::Number,
        Some("boolean") => TypeRef::Bool,
        Some("array") => {
            let item = prop
                .get("items")
                .map(schema_prop_type)
                .unwrap_or(TypeRef::Any);
            TypeRef::List(Box::new(item))
        }
        _ => TypeRef::Any,
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
    /// Required input parameters — a membership set (declared order kept only for stable display).
    /// Calls bind arguments by NAME via a single object argument; the analyzer rejects two or more
    /// positional args (the deprecated positional form), so nothing binds "to these first."
    #[serde(default)]
    pub required_params: Vec<String>,
    /// Optional input parameters (may be omitted from the call's named-object argument).
    #[serde(default)]
    pub optional_params: Vec<String>,
    /// The declared type of each named param (parsed from the op's input schema), for the analyzer's
    /// argument type-checking. Empty when the schema is untyped (a param absent here is `Any`).
    #[serde(default)]
    pub param_types: std::collections::BTreeMap<String, TypeRef>,
    /// The op's declared SEMANTIC effects (`Money`/`Delete`/`SendExternal`/…), carried alongside the
    /// lowered host [`effects`](Self::effects) instead of being erased by [`OpSpec::lower`] (D-138).
    /// `OpSignature::from_spec` cannot recover these from a bare [`ToolSpec`] — a `ToolSpec` has no
    /// semantic-effect field, by design (it stays free of any `flux-lang` dependency) — so this is
    /// empty unless a caller populates it from a richer source: [`OpSpec::to_signature`] (an
    /// `OpSpec`'s own declared `effects`) or the engine's tool-registry adapter, which folds in a
    /// `flux_runtime::Tool`'s declared semantic-effect tags (e.g. a plugin manifest's
    /// `OperationSpec::semantic_effects`). `annotate_effects` (`crate::analyze`) folds these into a
    /// call's `EffectAnnotation` without requiring an authored `effect:` tag on the call site.
    /// Deduped, first-seen order; additive — existing callers that never set this field keep getting
    /// an empty list, same as before this field existed.
    #[serde(default)]
    pub semantic_effects: Vec<FlowEffect>,
}

impl OpSignature {
    /// Derive an op signature from a registered tool spec.
    pub fn from_spec(spec: &ToolSpec) -> Self {
        let (required_params, optional_params) = schema_params(&spec.input_schema);
        let mut param_types = std::collections::BTreeMap::new();
        if let Some(props) = spec
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
        {
            for (name, prop) in props {
                param_types.insert(name.clone(), schema_prop_type(prop));
            }
        }
        Self {
            name: spec.name.clone(),
            description: spec.description.clone(),
            effects: spec.effects.clone(),
            risk: spec.risk,
            idempotency: spec.idempotency,
            required_params,
            optional_params,
            param_types,
            // A bare `ToolSpec` carries no semantic-effect tier (see the field doc); a caller that
            // has a richer source (an `OpSpec`, or a `flux_runtime::Tool`'s declared semantic-effect
            // tags) sets `semantic_effects` afterward. See `OpSpec::to_signature` for the OpSpec case.
            semantic_effects: Vec::new(),
        }
    }

    /// A compact parameter signature for the planner catalog, e.g. `{path, content}` or `path`
    /// (empty when the op takes no declared params). Multi-param ops are shown with braces to signal
    /// the named-object call form; a sole required param is shown bare (the single-value sugar).
    pub fn param_signature(&self) -> String {
        let req = &self.required_params;
        let opt = &self.optional_params;
        if req.len() == 1 && opt.is_empty() {
            return req[0].clone();
        }
        if req.is_empty() && opt.is_empty() {
            return String::new();
        }
        let mut all: Vec<String> = req.to_vec();
        all.extend(opt.iter().cloned());
        format!("{{{}}}", all.join(", "))
    }
}

/// The abstract operation catalog the analyzer validates against. Decouples analysis from the
/// concrete tool registry: anything that can resolve an op name to its [`OpSignature`] is a catalog,
/// so the language core needs no dependency on `flux-runtime`/`flux-tools`.
///
/// Resolution must NOT be advertised-filtered — a pre-authored flow may name any registered op, even
/// one whose evidence group is currently hidden. The engine's registry adapter implements this via
/// its unfiltered lookup.
pub trait OpCatalog {
    /// Resolve an op name to its signature, if registered.
    fn lookup(&self, name: &str) -> Option<OpSignature>;

    /// Return the JSON-Schema `format` marker for one named input parameter, when the catalog can
    /// expose it. Most analyzer checks only need [`OpSignature`], but literal string parameters with
    /// domain-specific syntax (for example `format: "flux-expr"`) need the original schema marker.
    fn param_format(&self, _op: &str, _param: &str) -> Option<String> {
        None
    }

    /// Resolve an op name to a Flux-Lang composite definition, if one is installed. The default
    /// keeps existing catalogs tool-only.
    fn composite(&self, _name: &str) -> Option<CompositeOpDecl> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn kb_search() -> OpSpec {
        OpSpec {
            name: "kb.search".into(),
            description: "search the knowledge base".into(),
            inputs: vec![
                Param {
                    name: "query".into(),
                    ty: TypeRef::String,
                    optional: false,
                },
                Param {
                    name: "limit".into(),
                    ty: TypeRef::Number,
                    optional: true,
                },
            ],
            output: TypeRef::Named("List".into()),
            effects: vec![
                FlowEffect::Read,
                FlowEffect::Network,
                FlowEffect::SendExternal,
            ],
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
        }
    }

    #[test]
    fn opspec_lowers_preserving_name_risk_and_host_effects() {
        let tool = kb_search().lower();

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

    /// D-138: unlike `lower()`'s `ToolSpec` (which collapses `Network` and `SendExternal` into the
    /// same host `Effect::Network`, indistinguishable from each other), `to_signature()` preserves
    /// the ORIGINAL semantic effects on `OpSignature::semantic_effects` — so `SendExternal` stays
    /// visible to a catalog consumer even though no host effect distinguishes it from `Network`.
    #[test]
    fn opspec_to_signature_preserves_semantic_effects_lower_erases() {
        let sig = kb_search().to_signature();

        assert_eq!(sig.name, "kb.search");
        // The host-facing fields still match a plain `from_spec(&lower())`.
        assert!(sig.effects.contains(&Effect::Read));
        // But the semantic tier survives, distinctly:
        assert!(sig.semantic_effects.contains(&FlowEffect::Read));
        assert!(sig.semantic_effects.contains(&FlowEffect::Network));
        assert!(sig.semantic_effects.contains(&FlowEffect::SendExternal));
    }

    #[test]
    fn opspec_lowers_typed_inputs_to_a_named_json_schema() {
        let tool = kb_search().lower();
        let schema = &tool.input_schema;

        // No longer the `{"type":"object"}` placeholder — a real object schema with named props.
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["query"], json!({ "type": "string" }));
        assert_eq!(schema["properties"]["limit"], json!({ "type": "number" }));
        // Only the non-optional param is required.
        assert_eq!(schema["required"], json!(["query"]));

        // Round-trip: the lowered schema reads back to the declared params — `query` required,
        // `limit` optional. Membership is what matters; calls bind args by name.
        let (required, optional) = schema_params(schema);
        assert_eq!(required, vec!["query"]);
        assert_eq!(optional, vec!["limit"]);

        // And the planner-catalog signature renders names, not a generic object.
        let sig = OpSignature::from_spec(&tool);
        assert_eq!(sig.param_signature(), "{query, limit}");
    }

    #[test]
    fn required_param_order_is_preserved_through_lowering() {
        // The `required` array preserves declaration order through lowering (stable display in the
        // planner catalog). Binding itself is by name — the canonical call form is a single
        // named-map object; the analyzer rejects the 2+-positional form.
        let spec = OpSpec {
            name: "edit".into(),
            description: "edit a file".into(),
            inputs: vec![
                Param {
                    name: "path".into(),
                    ty: TypeRef::String,
                    optional: false,
                },
                Param {
                    name: "old".into(),
                    ty: TypeRef::String,
                    optional: false,
                },
                Param {
                    name: "new".into(),
                    ty: TypeRef::String,
                    optional: false,
                },
            ],
            output: TypeRef::Any,
            effects: Vec::new(),
            risk: Risk::Low,
            idempotency: Idempotency::Idempotent,
        };
        let (required, optional) = schema_params(&spec.lower().input_schema);
        assert_eq!(required, vec!["path", "old", "new"]);
        assert!(optional.is_empty());
    }

    #[test]
    fn optional_params_are_a_sorted_set_not_an_order() {
        // `x-param-order` is gone; optional params are the `properties` keys not in `required`,
        // sorted for stable display. Order is non-binding (calls name args via an object).
        let schema = json!({
            "type": "object",
            "properties": {
                "args": { "type": "array" },
                "manifest_path": { "type": "string" },
                "package": { "type": "string" },
                "filter": { "type": "string" }
            }
        });
        let (required, optional) = schema_params(&schema);
        assert!(required.is_empty());
        assert_eq!(optional, vec!["args", "filter", "manifest_path", "package"]);
    }

    #[test]
    fn required_is_a_set_membership_check() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" },
                "limit": { "type": "integer" }
            },
            "required": ["path", "content"]
        });
        let (required, optional) = schema_params(&schema);
        assert_eq!(required, vec!["path", "content"]);
        assert_eq!(optional, vec!["limit"]);
    }

    #[test]
    fn type_ref_to_schema_projects_each_variant() {
        assert_eq!(type_ref_to_schema(&TypeRef::Any), json!({}));
        assert_eq!(
            type_ref_to_schema(&TypeRef::Bool),
            json!({ "type": "boolean" })
        );
        assert_eq!(
            type_ref_to_schema(&TypeRef::Number),
            json!({ "type": "number" })
        );
        assert_eq!(
            type_ref_to_schema(&TypeRef::String),
            json!({ "type": "string" })
        );
        assert_eq!(
            type_ref_to_schema(&TypeRef::List(Box::new(TypeRef::String))),
            json!({ "type": "array", "items": { "type": "string" } })
        );
        assert_eq!(
            type_ref_to_schema(&TypeRef::Named("Claim".into())),
            json!({ "$ref": "#/$defs/Claim" })
        );
    }
}
