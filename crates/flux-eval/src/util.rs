//! Small shared helpers for the ops: input coercion and JSON result building.
//!
//! Every op result is stored as a JSON **string**, so a `$var` reaches a consumer op as a string.
//! [`coerce_json`] parses that back; [`arg`] reads a named field tolerant of the lone-object-passthrough
//! arg mapping (where the op's whole input *is* the payload object).

use flux_core::{Error, Result};
use flux_runtime::ToolResult;
use serde_json::Value;

/// Coerce a value to JSON: parse a JSON-encoded string (how a `$var` arrives), else use as-is.
pub fn coerce_json(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| v.clone()),
        other => other.clone(),
    }
}

/// Read named field `key` from `params`, coerced to JSON. Falls back to treating `params` itself as
/// the payload (the lone-object-passthrough case, where there is no wrapper key).
pub fn arg(params: &Value, key: &str) -> Value {
    match params.get(key) {
        Some(v) => coerce_json(v),
        None => coerce_json(params),
    }
}

/// A string field (not coerced).
pub fn str_field<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(|v| v.as_str())
}

/// Serialize a value to a compact JSON string.
pub fn to_json_string(v: &Value) -> Result<String> {
    serde_json::to_string(v).map_err(|e| Error::Other(e.to_string()))
}

/// An OK [`ToolResult`] whose canonical content is `value` as JSON and whose model-facing view is `view`.
pub fn json_result(value: &Value, view: impl Into<String>) -> Result<ToolResult> {
    Ok(ToolResult::ok_view(to_json_string(value)?, view.into()))
}
