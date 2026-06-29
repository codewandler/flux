//! JavaScript pre-tool hooks evaluated in an embedded engine (QuickJS via `rquickjs`) — the
//! `hooks` module of `flux-plugin` (folded in from the former `flux-hooks` crate).
//!
//! A hook file exports `preToolUse(ctx)` where `ctx = { tool, input }`. It returns:
//! - nothing / `null` → continue unchanged;
//! - `{ deny: "reason" }` → block the tool call;
//! - `{ input: <new input> }` → replace the tool's input and continue.
//!
//! Hooks run in declaration order (system hooks first, by convention). The engine is sync and
//! dependency-light; the [`PreToolHook`] seam in `flux-runtime` is engine-agnostic, so a
//! `deno_core` backend (async/npm/TypeScript) can be swapped in later without touching the runtime.

use std::path::PathBuf;

use serde_json::{json, Value};

use flux_runtime::{HookOutcome, PreToolHook};

/// A set of JS hook scripts, run in order before each tool call.
pub struct JsHookEngine {
    scripts: Vec<(String, String)>, // (name, source)
}

impl JsHookEngine {
    /// Build from in-memory `(name, source)` pairs (used in tests / programmatic config).
    pub fn from_sources(scripts: Vec<(String, String)>) -> Self {
        Self { scripts }
    }

    /// Load `*.js` hook files from each directory (sorted by filename for deterministic order).
    pub fn load(dirs: &[PathBuf]) -> Self {
        let mut scripts = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|e| e == "js").unwrap_or(false))
                .collect();
            files.sort();
            for path in files {
                if let Ok(src) = std::fs::read_to_string(&path) {
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    scripts.push((name, src));
                }
            }
        }
        Self { scripts }
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }
}

/// What a single hook script decided.
enum PreResult {
    Continue,
    Deny(String),
    Modify(Value),
}

/// Evaluate one hook script's `preToolUse(ctx)` against `(tool, input)`.
fn eval_pre(src: &str, tool: &str, input: &Value) -> Result<PreResult, String> {
    let ctx_json = json!({ "tool": tool, "input": input }).to_string();

    let rt = rquickjs::Runtime::new().map_err(|e| e.to_string())?;
    // Kill a runaway hook (`while(true){}`, a huge allocation loop, …): interrupt evaluation once a
    // wall-clock deadline passes. The interrupt surfaces as an eval error → the hook fails closed
    // (denied by the caller) rather than hanging the agent.
    const HOOK_TIMEOUT_MS: u64 = 1000;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(HOOK_TIMEOUT_MS);
    rt.set_interrupt_handler(Some(Box::new(move || {
        std::time::Instant::now() >= deadline
    })));
    let context = rquickjs::Context::full(&rt).map_err(|e| e.to_string())?;

    let out: String = context
        .with(|ctx| -> rquickjs::Result<String> {
            // Define the hook's functions.
            ctx.eval::<(), _>(src.as_bytes())?;
            // Hand the context object in as a JSON string global.
            ctx.globals().set("__flux_ctx", ctx_json.clone())?;
            // Call preToolUse and serialize whatever it returns.
            ctx.eval::<String, _>(
                r#"(function () {
                if (typeof preToolUse !== 'function') return 'null';
                var r = preToolUse(JSON.parse(__flux_ctx));
                return JSON.stringify(r === undefined ? null : r);
            })()"#,
            )
        })
        .map_err(|e| e.to_string())?;

    let value: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    if value.is_null() {
        return Ok(PreResult::Continue);
    }
    if let Some(reason) = value.get("deny").and_then(|v| v.as_str()) {
        return Ok(PreResult::Deny(reason.to_string()));
    }
    if let Some(new_input) = value.get("input") {
        return Ok(PreResult::Modify(new_input.clone()));
    }
    Ok(PreResult::Continue)
}

impl PreToolHook for JsHookEngine {
    fn pre_tool(&self, tool: &str, input: &Value) -> HookOutcome {
        let mut current = input.clone();
        let mut modified = false;
        for (_name, src) in &self.scripts {
            match eval_pre(src, tool, &current) {
                Ok(PreResult::Continue) => {}
                Ok(PreResult::Deny(reason)) => return HookOutcome::Deny(reason),
                Ok(PreResult::Modify(v)) => {
                    current = v;
                    modified = true;
                }
                // A failing hook must not silently allow nor break the tool: deny with the error.
                Err(e) => return HookOutcome::Deny(format!("hook error: {e}")),
            }
        }
        if modified {
            HookOutcome::Modify(current)
        } else {
            HookOutcome::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine(src: &str) -> JsHookEngine {
        JsHookEngine::from_sources(vec![("test.js".to_string(), src.to_string())])
    }

    #[test]
    fn runaway_hook_is_interrupted_not_hung() {
        // An infinite loop must be interrupted by the deadline and fail closed (deny), not hang.
        let e = engine("function preToolUse(ctx){ while(true){} }");
        match e.pre_tool("bash", &json!({})) {
            HookOutcome::Deny(r) => assert!(r.contains("hook error"), "got: {r}"),
            _ => panic!("expected a deny on hook timeout"),
        }
    }

    #[test]
    fn hook_can_deny() {
        let e = engine(
            "function preToolUse(ctx){ if (ctx.tool === 'bash') return {deny: 'no shell'}; }",
        );
        match e.pre_tool("bash", &json!({"command": "rm -rf /"})) {
            HookOutcome::Deny(r) => assert_eq!(r, "no shell"),
            _ => panic!("expected deny"),
        }
        // other tools pass through
        assert!(matches!(
            e.pre_tool("read", &json!({})),
            HookOutcome::Continue
        ));
    }

    #[test]
    fn hook_can_modify_input() {
        let e = engine(
            "function preToolUse(ctx){ if (ctx.tool === 'write') { ctx.input.path = 'SAFE/' + ctx.input.path; return {input: ctx.input}; } }",
        );
        match e.pre_tool("write", &json!({"path": "a.txt", "content": "x"})) {
            HookOutcome::Modify(v) => {
                assert_eq!(v["path"], "SAFE/a.txt");
                assert_eq!(v["content"], "x");
            }
            _ => panic!("expected modify"),
        }
    }

    #[test]
    fn no_hook_function_is_continue() {
        let e = engine("var x = 1;");
        assert!(matches!(
            e.pre_tool("read", &json!({})),
            HookOutcome::Continue
        ));
    }

    #[test]
    fn hooks_chain_in_order() {
        let e = JsHookEngine::from_sources(vec![
            (
                "01.js".into(),
                "function preToolUse(c){ c.input.n = (c.input.n||0)+1; return {input:c.input}; }"
                    .into(),
            ),
            (
                "02.js".into(),
                "function preToolUse(c){ c.input.n = (c.input.n||0)+10; return {input:c.input}; }"
                    .into(),
            ),
        ]);
        match e.pre_tool("x", &json!({})) {
            HookOutcome::Modify(v) => assert_eq!(v["n"], 11),
            _ => panic!("expected modify"),
        }
    }
}
