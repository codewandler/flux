//! The shared dry-run/runtime validation layer (D-88).
//!
//! Historically `--dry-run` trusted only an op's generated JSON schema while the runtime handler
//! enforced more (flex-trimmed strings, non-empty arrays, positive ids, enum values, conditional
//! targets), so a green dry-run could fail immediately at runtime. This module is the single
//! preflight both paths run: [`schema_preflight`] checks an input against the op's declared
//! `input_schema`, and [`crate::PluginBuilder::preflight`] adds per-op custom rules for constraints
//! a schema cannot express. [`crate::Plugin`] dispatch runs the combined preflight before every
//! handler call and answers the auto-registered `plugin.validate` internal op with the same
//! verdict, so dry-run and runtime can never disagree.
//!
//! The structural checks are **flex-aware**: they accept exactly the coercions the handlers'
//! `flex_str`/`flex_i64`-style extraction accepts (numbers where strings are declared, numeric
//! strings where integers are declared), so the preflight never rejects an input the runtime
//! would have served.

use serde_json::Value;

/// The outcome of a preflight check. `problems` are hard failures — dispatch refuses the input.
/// `warnings` are advisory (GL-008): the input still executes, but part of it is likely inert —
/// e.g. a field an *open* schema does not declare. A schema that sets
/// `additionalProperties: false` upgrades unknown fields from warning to problem.
#[derive(Debug, Default)]
pub struct PreflightReport {
    /// Hard failures: the input will not execute.
    pub problems: Vec<String>,
    /// Advisories: the input executes, but something in it is likely ignored.
    pub warnings: Vec<String>,
}

/// Validate `input` against an op's declared JSON `input_schema`. Checks: required fields present
/// and (for strings) non-blank, unknown top-level fields (a warning on open schemas, a problem
/// under `additionalProperties: false`), flex-aware type conformance, `enum` membership, numeric
/// `minimum`/`exclusiveMinimum`/`maximum`, string `minLength`, array `minItems`/`maxItems` and
/// per-element `items` conformance, and nested object schemas (including `$defs`/`definitions`
/// references).
pub fn schema_preflight(schema: &Value, input: &Value) -> PreflightReport {
    let mut report = PreflightReport::default();
    let Some(obj) = input.as_object() else {
        report
            .problems
            .push(format!("input must be a JSON object, got {input}"));
        return report;
    };
    let root = schema;
    let schema = resolve_ref(root, schema);
    let properties = schema.get("properties").and_then(|v| v.as_object());

    // Unknown top-level fields (GL-008): a hard problem when the schema forbids extras, an
    // advisory warning otherwise — handlers may read undeclared aliases, so an open schema
    // cannot justify rejection.
    if let Some(props) = properties {
        let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
        for key in obj.keys() {
            if !props.contains_key(key) {
                if closed {
                    report
                        .problems
                        .push(format!("unknown field `{key}` (not in the op schema)"));
                } else {
                    report.warnings.push(format!(
                        "unknown field `{key}` (not in the op schema; the handler may ignore it)"
                    ));
                }
            }
        }
    }

    // Required fields: absent and null are both missing; a blank string is missing to the
    // handlers' flex extraction, so it is rejected here too (GL-030).
    if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
        for req in required.iter().filter_map(|v| v.as_str()) {
            match obj.get(req) {
                None | Some(Value::Null) => {
                    report
                        .problems
                        .push(format!("missing required field `{req}`"));
                }
                Some(Value::String(s)) if s.trim().is_empty() => {
                    report
                        .problems
                        .push(format!("required field `{req}` is blank"));
                }
                Some(_) => {}
            }
        }
    }

    if let Some(props) = properties {
        for (key, field_schema) in props {
            match obj.get(key) {
                None | Some(Value::Null) => {}
                Some(value) => check_value(root, field_schema, value, key, &mut report.problems),
            }
        }
    }
    report
}

/// Resolve a local `$ref` (`#/$defs/Name` or `#/definitions/Name`) against the root schema;
/// non-refs and unresolvable refs pass through unchanged.
fn resolve_ref<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    let Some(r) = schema.get("$ref").and_then(|v| v.as_str()) else {
        return schema;
    };
    for (prefix, section) in [("#/$defs/", "$defs"), ("#/definitions/", "definitions")] {
        if let Some(name) = r.strip_prefix(prefix) {
            if let Some(resolved) = root.get(section).and_then(|d| d.get(name)) {
                return resolved;
            }
        }
    }
    schema
}

/// The declared `type`(s) of a subschema, ignoring `"null"` (an absent optional).
fn declared_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(|v| v.as_str())
            .filter(|t| *t != "null")
            .collect(),
        _ => Vec::new(),
    }
}

/// A value's integer reading under the handlers' flex extraction (integer, or numeric string).
fn flex_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

/// A value's float reading under flex extraction (number, or numeric string).
fn flex_float(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// A value's string reading under flex extraction (string, or number rendered as one).
fn flex_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Whether `value` conforms to one declared `type` under flex coercion.
fn matches_type(value: &Value, ty: &str) -> bool {
    match ty {
        "string" => flex_string(value).is_some(),
        "integer" => flex_int(value).is_some(),
        "number" => flex_float(value).is_some(),
        // A non-bool "boolean" is silently ignored by `as_bool()` extraction — reject it so the
        // caller learns the value would not take effect (the GL-033 silent-ignore trap).
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => true,
    }
}

/// Validate one present, non-null `value` at `path` against its (possibly `$ref`) subschema.
fn check_value(
    root: &Value,
    schema: &Value,
    value: &Value,
    path: &str,
    problems: &mut Vec<String>,
) {
    let schema = resolve_ref(root, schema);

    // Union shapes (schemars emits `anyOf`/`oneOf` for Option<T> and enums-with-data): the value
    // passes if any branch accepts it. On failure, a single non-null branch (the Option<T> case)
    // reports its own specific problems instead of a generic mismatch.
    for key in ["anyOf", "oneOf"] {
        if let Some(branches) = schema.get(key).and_then(|v| v.as_array()) {
            let non_null: Vec<&Value> = branches
                .iter()
                .map(|b| resolve_ref(root, b))
                .filter(|b| declared_types(b) != ["null"])
                .collect();
            let passes = value.is_null()
                || non_null.iter().any(|branch| {
                    let mut sub = Vec::new();
                    check_value(root, branch, value, path, &mut sub);
                    sub.is_empty()
                });
            if !passes {
                if let [only] = non_null.as_slice() {
                    check_value(root, only, value, path, problems);
                } else {
                    problems.push(format!("`{path}`: matches none of the allowed shapes"));
                }
            }
            return;
        }
    }

    // Enum membership, under the same string coercion the handlers apply (GL-011/GL-022).
    if let Some(allowed) = schema.get("enum").and_then(|v| v.as_array()) {
        let matched = allowed.iter().any(|a| match (a, value) {
            (Value::String(a), _) => flex_string(value).as_deref() == Some(a.as_str()),
            _ => a == value,
        });
        if !matched {
            let set: Vec<String> = allowed.iter().map(|a| a.to_string()).collect();
            problems.push(format!(
                "`{path}`: must be one of [{}], got {value}",
                set.join(", ")
            ));
        }
        return;
    }

    let types = declared_types(schema);
    if !types.is_empty() && !types.iter().any(|t| matches_type(value, t)) {
        problems.push(format!(
            "`{path}`: expected {}, got {value}",
            types.join(" or ")
        ));
        return;
    }

    // Numeric bounds (schemars `range(...)`) — positive ids/iids land here (GL-024).
    if let Some(v) =
        flex_float(value).filter(|_| types.contains(&"integer") || types.contains(&"number"))
    {
        if let Some(min) = schema.get("minimum").and_then(|m| m.as_f64()) {
            if v < min {
                problems.push(format!("`{path}`: must be >= {min}, got {value}"));
            }
        }
        if let Some(min) = schema.get("exclusiveMinimum").and_then(|m| m.as_f64()) {
            if v <= min {
                problems.push(format!("`{path}`: must be > {min}, got {value}"));
            }
        }
        if let Some(max) = schema.get("maximum").and_then(|m| m.as_f64()) {
            if v > max {
                problems.push(format!("`{path}`: must be <= {max}, got {value}"));
            }
        }
    }

    // String length (schemars `length(...)`).
    if let (Some(s), Some(min)) = (
        value.as_str(),
        schema.get("minLength").and_then(|m| m.as_u64()),
    ) {
        if (s.chars().count() as u64) < min {
            problems.push(format!("`{path}`: must be at least {min} character(s)"));
        }
    }

    if let Some(arr) = value.as_array() {
        if let Some(min) = schema.get("minItems").and_then(|m| m.as_u64()) {
            if (arr.len() as u64) < min {
                problems.push(format!(
                    "`{path}`: must have at least {min} item(s), got {}",
                    arr.len()
                ));
            }
        }
        if let Some(max) = schema.get("maxItems").and_then(|m| m.as_u64()) {
            if (arr.len() as u64) > max {
                problems.push(format!(
                    "`{path}`: must have at most {max} item(s), got {}",
                    arr.len()
                ));
            }
        }
        // Per-element conformance for typed payloads (GL-012).
        if let Some(items) = schema.get("items") {
            for (i, elem) in arr.iter().enumerate() {
                check_value(root, items, elem, &format!("{path}[{i}]"), problems);
            }
        }
    }

    // Nested object schemas (typed payload elements): recurse with the same rules.
    if let Some(obj) = value.as_object() {
        if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
            if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
                for key in obj.keys() {
                    if !props.contains_key(key) {
                        problems.push(format!("`{path}`: unknown field `{key}`"));
                    }
                }
            }
        }
        if let Some(required) = schema.get("required").and_then(|v| v.as_array()) {
            for req in required.iter().filter_map(|v| v.as_str()) {
                match obj.get(req) {
                    None | Some(Value::Null) => {
                        problems.push(format!("`{path}`: missing required field `{req}`"));
                    }
                    Some(Value::String(s)) if s.trim().is_empty() => {
                        problems.push(format!("`{path}`: required field `{req}` is blank"));
                    }
                    Some(_) => {}
                }
            }
        }
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            for (key, field_schema) in props {
                match obj.get(key) {
                    None | Some(Value::Null) => {}
                    Some(v) => {
                        check_value(root, field_schema, v, &format!("{path}.{key}"), problems)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn problems(schema: Value, input: Value) -> Vec<String> {
        schema_preflight(&schema, &input).problems
    }

    #[test]
    fn accepts_a_conforming_input() {
        let schema = json!({
            "type": "object",
            "properties": {
                "project": {"type": "string"},
                "limit": {"type": ["integer", "null"]},
            },
            "required": ["project"],
        });
        assert!(problems(schema, json!({"project": "group/app", "limit": 5})).is_empty());
    }

    #[test]
    fn flex_coercions_match_the_handlers() {
        let schema = json!({
            "type": "object",
            "properties": {
                "project": {"type": "string"},
                "limit": {"type": ["integer", "null"]},
            },
            "required": ["project"],
        });
        // Numbers where strings are declared, numeric strings where integers are declared —
        // exactly what flex_str/flex_i64 accept at runtime.
        assert!(problems(schema.clone(), json!({"project": 42, "limit": "7"})).is_empty());
        // A non-numeric string where an integer is declared is a type problem.
        let p = problems(schema, json!({"project": "x", "limit": "many"}));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("`limit`"), "{p:?}");
    }

    #[test]
    fn required_blank_and_missing_are_rejected() {
        let schema = json!({
            "type": "object",
            "properties": {"title": {"type": "string"}},
            "required": ["title"],
        });
        assert!(problems(schema.clone(), json!({}))[0].contains("missing required field `title`"));
        assert!(problems(schema.clone(), json!({"title": null}))[0].contains("missing"));
        // GL-030: whitespace-only passes a naive schema check but fails flex extraction.
        assert!(problems(schema, json!({"title": "   "}))[0].contains("blank"));
    }

    #[test]
    fn unknown_fields_rejected_only_when_schema_forbids_them() {
        let closed = json!({
            "type": "object",
            "properties": {"a": {"type": "string"}},
            "additionalProperties": false,
        });
        let p = problems(closed, json!({"a": "x", "typo": 1}));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("unknown field `typo`"), "{p:?}");
        // An open schema (no additionalProperties: false) keeps accepting extras — but warns
        // (GL-008): the handler may read an undeclared alias, or may silently ignore the key.
        let open = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        let report = schema_preflight(&open, &json!({"a": "x", "extra": 1}));
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("`extra`"),
            "{:?}",
            report.warnings
        );
    }

    #[test]
    fn enum_membership_is_enforced_with_string_coercion() {
        let schema = json!({
            "type": "object",
            "properties": {"state": {"type": "string", "enum": ["opened", "closed", "all"]}},
        });
        assert!(problems(schema.clone(), json!({"state": "opened"})).is_empty());
        // Trimmed like flex_str.
        assert!(problems(schema.clone(), json!({"state": " opened "})).is_empty());
        let p = problems(schema, json!({"state": "weird"}));
        assert!(p[0].contains("must be one of"), "{p:?}");
    }

    #[test]
    fn refs_into_defs_resolve() {
        let schema = json!({
            "type": "object",
            "properties": {"visibility": {"$ref": "#/$defs/Visibility"}},
            "$defs": {"Visibility": {"type": "string", "enum": ["private", "internal", "public"]}},
        });
        assert!(problems(schema.clone(), json!({"visibility": "public"})).is_empty());
        assert!(!problems(schema, json!({"visibility": "hidden"})).is_empty());
    }

    #[test]
    fn numeric_minimum_rejects_non_positive_ids() {
        let schema = json!({
            "type": "object",
            "properties": {"iid": {"type": ["integer", "null"], "minimum": 1}},
        });
        assert!(problems(schema.clone(), json!({"iid": 12})).is_empty());
        assert!(problems(schema.clone(), json!({"iid": 0}))[0].contains(">= 1"));
        assert!(problems(schema, json!({"iid": -3}))[0].contains(">= 1"));
    }

    #[test]
    fn arrays_enforce_min_items_and_element_schemas() {
        let schema = json!({
            "type": "object",
            "properties": {
                "actions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "action": {"type": "string", "enum": ["create", "update", "delete"]},
                            "file_path": {"type": "string"},
                        },
                        "required": ["action", "file_path"],
                    },
                },
            },
        });
        assert!(problems(schema.clone(), json!({"actions": []}))[0].contains("at least 1 item"));
        let p = problems(
            schema.clone(),
            json!({"actions": [{"action": "explode", "file_path": "x"}]}),
        );
        assert!(p[0].contains("`actions[0].action`"), "{p:?}");
        let p = problems(schema.clone(), json!({"actions": [{"action": "create"}]}));
        assert!(p[0].contains("missing required field `file_path`"), "{p:?}");
        assert!(problems(
            schema,
            json!({"actions": [{"action": "create", "file_path": "src/a.rs"}]})
        )
        .is_empty());
    }

    #[test]
    fn option_anyof_unions_accept_either_branch() {
        let schema = json!({
            "type": "object",
            "properties": {
                "kind": {"anyOf": [{"$ref": "#/$defs/Kind"}, {"type": "null"}]},
            },
            "$defs": {"Kind": {"type": "string", "enum": ["env_var", "file"]}},
        });
        assert!(problems(schema.clone(), json!({})).is_empty());
        assert!(problems(schema.clone(), json!({"kind": "file"})).is_empty());
        assert!(!problems(schema, json!({"kind": "secret"})).is_empty());
    }

    #[test]
    fn booleans_must_be_real_booleans() {
        // `as_bool()` extraction silently ignores a string "true" — reject it so the caller
        // learns the value would not take effect.
        let schema = json!({
            "type": "object",
            "properties": {"squash": {"type": ["boolean", "null"]}},
        });
        assert!(problems(schema.clone(), json!({"squash": true})).is_empty());
        assert!(!problems(schema, json!({"squash": "true"})).is_empty());
    }

    #[test]
    fn empty_schema_accepts_anything() {
        assert!(problems(json!({}), json!({"whatever": [1, 2, 3]})).is_empty());
    }

    #[test]
    fn non_object_input_is_rejected() {
        assert!(!problems(json!({"type": "object"}), json!([1, 2])).is_empty());
    }
}
