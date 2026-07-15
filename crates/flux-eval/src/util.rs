//! Small shared helpers for the ops: input coercion and JSON result building.
//!
//! Every op result is stored as a JSON **string**, so a `$var` reaches a consumer op as a string.
//! [`coerce_json`] parses that back; [`arg`] reads a named field tolerant of the lone-object-passthrough
//! arg mapping (where the op's whole input *is* the payload object).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use flux_core::{Error, Result};
use flux_runtime::ToolResult;
use serde_json::Value;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A freshly created temp directory under the system temp dir, unique per process *and* per call.
/// The name folds the process id with a global atomic counter so two directories minted in the same
/// process — concurrent eval tasks, or a test that needs several — can't collide. Every call site
/// (`runner`, `ops`, `aggregate`, `git` tests) previously re-derived this, and the process-id-only
/// variants silently lacked the per-call counter; this is the single implementation.
pub fn unique_temp_dir(prefix: &str) -> std::io::Result<PathBuf> {
    let n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("{prefix}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Coerce a value to JSON: parse a JSON-encoded string (how a `$var` arrives), else use as-is.
pub fn coerce_json(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or_else(|_| v.clone()),
        other => other.clone(),
    }
}

/// Deserialize an op's arguments into its typed input struct — the single source of truth paired
/// with the `schemars`-derived `input_schema`. Coerces JSON-encoded-string args first (how a `$var`
/// arrives), so the typed struct sees real JSON. Maps a serde error to the op-error style.
pub fn parse_params<T: serde::de::DeserializeOwned>(params: &Value, op: &str) -> Result<T> {
    serde_json::from_value(coerce_json(params))
        .map_err(|e| Error::Other(format!("{op}: invalid arguments: {e}")))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_temp_dir_is_distinct_per_call_within_one_process() {
        // The bug the process-id-only call sites carried: two dirs minted in the same process
        // shared a name, so a second create landed in (and could clobber) the first. The atomic
        // counter must make every call distinct, and each must exist afterward.
        let a = unique_temp_dir("flux-eval-util-test").unwrap();
        let b = unique_temp_dir("flux-eval-util-test").unwrap();
        assert_ne!(a, b, "same prefix must still yield distinct directories");
        assert!(a.is_dir() && b.is_dir(), "both directories must be created");
        std::fs::remove_dir_all(&a).ok();
        std::fs::remove_dir_all(&b).ok();
    }
}
