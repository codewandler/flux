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

/// A single named input parameter of an [`OpSpec`]: a `name`, its [`TypeRef`], and whether it may be
/// omitted. Naming the param here — rather than leaving `inputs` positional — is what lets
/// [`OpSpec::lower`] project a faithful JSON Schema whose `properties`/`required` the planner catalog
/// and [`schema_params`] read back to recover the op's positional binding order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: TypeRef,
    /// When true, the param is omitted from the schema's `required` array (it still appears in
    /// `properties`). Optional params carry no inter-param order guarantee — JSON object keys are
    /// unordered.
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
    /// non-`optional` param is listed in `required` **in declared order** — the array preserves order,
    /// so [`schema_params`] reads it back to recover the op's positional binding order. Optional params
    /// carry no order guarantee (JSON object keys are unordered), matching hand-written op schemas.
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

/// The input parameter names of a tool's JSON-Schema, as `(required, optional)`. The `required`
/// array fixes the order of mandatory params; the optional ones are whatever remaining `properties`
/// the schema declares (key order is the schema map's order). A flow's positional `Call.args` map
/// onto `required ++ optional` at execution (see `runtime::map_args_to_input`), and the planner
/// catalog renders the same signature so the model emits args in this order.
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
    /// Required input parameters, in declared order (positional call args bind to these first).
    #[serde(default)]
    pub required_params: Vec<String>,
    /// Optional input parameters (bound after the required ones).
    #[serde(default)]
    pub optional_params: Vec<String>,
    /// The declared type of each named param (parsed from the op's input schema), for the analyzer's
    /// argument type-checking. Empty when the schema is untyped (a param absent here is `Any`).
    #[serde(default)]
    pub param_types: std::collections::BTreeMap<String, TypeRef>,
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
        }
    }

    /// A compact parameter signature for the planner catalog, e.g. `path[, offset, limit]` or
    /// `pattern` (empty when the op takes no declared params).
    pub fn param_signature(&self) -> String {
        let mut s = self.required_params.join(", ");
        if !self.optional_params.is_empty() {
            let opt = self.optional_params.join(", ");
            if self.required_params.is_empty() {
                s.push_str(&format!("[{opt}]"));
            } else {
                s.push_str(&format!("[, {opt}]"));
            }
        }
        s
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

        // Round-trip: the lowered schema reads back to the declared params. Required order is
        // load-bearing (positional binding); `query` is required, `limit` optional.
        let (required, optional) = schema_params(schema);
        assert_eq!(required, vec!["query"]);
        assert_eq!(optional, vec!["limit"]);

        // And the planner-catalog signature renders names, not a generic object.
        let sig = OpSignature::from_spec(&tool);
        assert_eq!(sig.param_signature(), "query[, limit]");
    }

    #[test]
    fn required_param_order_is_preserved_through_lowering() {
        // Required params bind positionally, so their order must survive the round-trip exactly
        // (the `required` array preserves order even though object keys are unordered).
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
