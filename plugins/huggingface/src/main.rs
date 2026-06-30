//! `huggingface` — a flux integration plugin for the Hugging Face Hub catalog (models, datasets,
//! spaces) and the Hugging Face inference router (OpenAI-compatible chat/embed).
//!
//! Hub catalog ops (`model.search/get`, `dataset.search/get`, `space.search`, `whoami`, `test`)
//! talk to the **hub endpoint** (`huggingface.hub`, default `https://huggingface.co`); auth is
//! optional on public Hub reads (injected when the token is present, skipped otherwise).
//!
//! Inference ops (`chat`, `embed`) talk to the **router endpoint** (`huggingface.router`, default
//! `https://router.huggingface.co`); auth is always required there.

use host_kit::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Manifest.
// ---------------------------------------------------------------------------

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("huggingface", "0.1.0")
        .capabilities(Caps {
            http: true,
            secrets: vec!["HF_TOKEN".into(), "HUGGING_FACE_HUB_TOKEN".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "api_token".into(),
            env: vec!["HF_TOKEN".into(), "HUGGING_FACE_HUB_TOKEN".into()],
            description: "Hugging Face user access token. Optional for public Hub reads; required for inference, whoami, and private/gated repos.".into(),
            ..Default::default()
        })
        .endpoint(EndpointSpec {
            name: "huggingface.hub".into(),
            env: vec!["HF_HUB_URL".into()],
            description: "Hugging Face Hub base URL (default https://huggingface.co)".into(),
        })
        .endpoint(EndpointSpec {
            name: "huggingface.router".into(),
            env: vec!["HF_ROUTER_URL".into()],
            description: "Hugging Face inference router base URL (default https://router.huggingface.co)".into(),
        })
        .datasource(ds("huggingface.models", "huggingface.model", "Hugging Face Hub models."))
        .datasource(ds("huggingface.datasets", "huggingface.dataset", "Hugging Face Hub datasets."))
        .datasource(ds("huggingface.spaces", "huggingface.space", "Hugging Face Hub spaces."))
        // ---- reachability / auth test ----
        .operation(
            read_op(
                "huggingface.test",
                "Test reachability of the Hub (anonymous) and, when a token is present, validate it via whoami.",
                so(json!({}), json!([])),
            ),
            op_test,
        )
        // ---- model ops ----
        .operation(
            read_op(
                "huggingface.model.search",
                "Search the Hugging Face Hub for models (GET /api/models).",
                so(
                    json!({
                        "search":    {"type": "string"},
                        "author":    {"type": "string"},
                        "filter":    {"type": "string"},
                        "sort":      {"type": "string"},
                        "direction": {"type": "string"},
                        "limit":     {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_model_search,
        )
        .operation(
            read_op(
                "huggingface.model.get",
                "Get metadata for a single Hugging Face model repo (GET /api/models/{repo_id}).",
                so(json!({"repo_id": {"type": "string"}}), json!(["repo_id"])),
            ),
            op_model_get,
        )
        // ---- dataset ops ----
        .operation(
            read_op(
                "huggingface.dataset.search",
                "Search the Hugging Face Hub for datasets (GET /api/datasets).",
                so(
                    json!({
                        "search":    {"type": "string"},
                        "author":    {"type": "string"},
                        "filter":    {"type": "string"},
                        "sort":      {"type": "string"},
                        "direction": {"type": "string"},
                        "limit":     {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_dataset_search,
        )
        .operation(
            read_op(
                "huggingface.dataset.get",
                "Get metadata for a single Hugging Face dataset repo (GET /api/datasets/{repo_id}).",
                so(json!({"repo_id": {"type": "string"}}), json!(["repo_id"])),
            ),
            op_dataset_get,
        )
        // ---- space ops ----
        .operation(
            read_op(
                "huggingface.space.search",
                "Search the Hugging Face Hub for spaces (GET /api/spaces).",
                so(
                    json!({
                        "search":    {"type": "string"},
                        "author":    {"type": "string"},
                        "filter":    {"type": "string"},
                        "sort":      {"type": "string"},
                        "direction": {"type": "string"},
                        "limit":     {"type": "integer"}
                    }),
                    json!([]),
                ),
            ),
            op_space_search,
        )
        // ---- identity ----
        .operation(
            read_op(
                "huggingface.whoami",
                "Show the identity associated with the stored Hugging Face token (GET /api/whoami-v2).",
                so(json!({}), json!([])),
            ),
            op_whoami,
        )
        // ---- inference ----
        .operation(
            write_op(
                "huggingface.chat",
                "Run a chat completion via the Hugging Face inference router (OpenAI-compatible POST /v1/chat/completions, non-streaming).",
                so(
                    json!({
                        "model":       {"type": "string"},
                        "messages":    {"type": "array"},
                        "max_tokens":  {"type": "integer"},
                        "temperature": {"type": "number"},
                        "top_p":       {"type": "number"},
                        "stop":        {"type": "array"}
                    }),
                    json!(["model", "messages"]),
                ),
            ),
            op_chat,
        )
        .operation(
            write_op(
                "huggingface.embed",
                "Create embeddings via the Hugging Face inference router (OpenAI-compatible POST /v1/embeddings).",
                so(
                    json!({
                        "model": {"type": "string"},
                        "input": {"type": "array"}
                    }),
                    json!(["model", "input"]),
                ),
            ),
            op_embed,
        )
}

// ---------------------------------------------------------------------------
// Shared schema helpers.
// ---------------------------------------------------------------------------

fn ds(name: &str, entity: &str, desc: &str) -> Declaration {
    Declaration {
        name: name.into(),
        entity: entity.into(),
        description: Some(desc.into()),
        capabilities: vec!["search".into(), "get".into(), "index".into()],
        entity_schema: None,
    }
}

fn so(props: Value, required: Value) -> Value {
    json!({ "type": "object", "properties": props, "required": required })
}

// ---------------------------------------------------------------------------
// HTTP helpers.
// ---------------------------------------------------------------------------

/// Resolve the Hub base URL (falling back to huggingface.co).
fn hub_base(host: &mut Host) -> String {
    host.endpoint("huggingface.hub")
        .unwrap_or_else(|_| "https://huggingface.co".into())
        .trim_end_matches('/')
        .to_string()
}

/// Resolve the router base URL (falling back to router.huggingface.co).
fn router_base(host: &mut Host) -> String {
    host.endpoint("huggingface.router")
        .unwrap_or_else(|_| "https://router.huggingface.co".into())
        .trim_end_matches('/')
        .to_string()
}

/// True when a token is available for the `api_token` purpose.
fn has_token(host: &mut Host) -> bool {
    host.secret("api_token").is_ok()
}

/// Auth purpose for Hub reads: inject only when a token is present.
fn hub_auth(host: &mut Host) -> Option<&'static str> {
    if has_token(host) {
        Some("api_token")
    } else {
        None
    }
}

/// GET `{base}{path}` with optional auth injection.
fn hf_get(host: &mut Host, base: &str, path: &str, auth: Option<&str>) -> Result<Value, String> {
    let url = format!("{base}{path}");
    host.get_json(&url, auth)
}

/// POST JSON to `{base}{path}` with required auth injection.
fn hf_post(host: &mut Host, base: &str, path: &str, body: &Value) -> Result<Value, String> {
    let url = format!("{base}{path}");
    host.send_json("POST", &url, Some("api_token"), body)
}

/// Percent-encode a single URL path segment (not the slash separator).
fn penc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Encode each segment of `org/name` preserving the `/` separator.
fn repo_path(repo_id: &str) -> String {
    repo_id
        .trim_matches('/')
        .split('/')
        .map(penc)
        .collect::<Vec<_>>()
        .join("/")
}

/// Build a query string from the common catalog search params, skipping empties.
fn search_qs(input: &Value) -> String {
    let mut pairs: Vec<String> = Vec::new();
    for key in ["search", "author", "filter", "sort", "direction"] {
        if let Some(Value::String(s)) = input.get(key) {
            let s = s.trim();
            if !s.is_empty() {
                pairs.push(format!("{key}={}", penc(s)));
            }
        }
    }
    if let Some(limit) = input.get("limit").and_then(|v| v.as_i64()) {
        if limit > 0 {
            pairs.push(format!("limit={limit}"));
        }
    }
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

// ---------------------------------------------------------------------------
// Contribution helpers.
// ---------------------------------------------------------------------------

fn contribute_items(host: &mut Host, items: &Value, entity: &str) {
    let arr = match items.as_array() {
        Some(a) => a,
        None => return,
    };
    let records: Vec<Record> = arr
        .iter()
        .filter_map(|item| {
            let id = item.get("id").and_then(|v| v.as_str())?;
            let title = item
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or(id)
                .to_string();
            // Build a short body from the most useful fields.
            let mut parts: Vec<String> = vec![format!("id: {id}")];
            for key in ["pipeline_tag", "library_name", "author", "sdk"] {
                if let Some(Value::String(s)) = item.get(key) {
                    if !s.is_empty() {
                        parts.push(format!("{key}: {s}"));
                    }
                }
            }
            Some(Record::new(
                Source::new("huggingface"),
                entity,
                id,
                title,
                parts.join("; "),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }
}

// ---------------------------------------------------------------------------
// Op handlers.
// ---------------------------------------------------------------------------

fn op_test(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = hub_base(host);
    // Anonymous reachability check.
    if let Err(e) = hf_get(host, &base, "/api/models?limit=1", None) {
        return Ok(json!({
            "status": "unreachable",
            "error": e,
            "hint": "Hugging Face Hub is unreachable — check network and the huggingface.hub endpoint (env: HF_HUB_URL)."
        }));
    }
    // Token validity check when a token is present.
    if has_token(host) {
        match hf_get(host, &base, "/api/whoami-v2", Some("api_token")) {
            Ok(who) => {
                let user = who
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Ok(json!({
                    "status": "ok",
                    "reachable": true,
                    "authenticated": true,
                    "user": user
                }));
            }
            Err(e) => {
                return Ok(json!({
                    "status": "auth_error",
                    "reachable": true,
                    "authenticated": false,
                    "error": e,
                    "hint": "Hub is reachable but the token failed validation — mint a new token at https://huggingface.co/settings/tokens."
                }));
            }
        }
    }
    Ok(json!({
        "status": "ok",
        "reachable": true,
        "authenticated": false,
        "hint": "Hub is reachable; no token stored — public reads work. Set HF_TOKEN for inference and private repos."
    }))
}

fn op_model_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = hub_base(host);
    let auth = hub_auth(host);
    let qs = search_qs(&input);
    let items = hf_get(host, &base, &format!("/api/models{qs}"), auth)?;
    contribute_items(host, &items, "huggingface.model");
    Ok(items)
}

fn op_model_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let repo_id = req_repo_id(&input)?;
    let base = hub_base(host);
    let auth = hub_auth(host);
    hf_get(
        host,
        &base,
        &format!("/api/models/{}", repo_path(&repo_id)),
        auth,
    )
}

fn op_dataset_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = hub_base(host);
    let auth = hub_auth(host);
    let qs = search_qs(&input);
    let items = hf_get(host, &base, &format!("/api/datasets{qs}"), auth)?;
    contribute_items(host, &items, "huggingface.dataset");
    Ok(items)
}

fn op_dataset_get(input: Value, host: &mut Host) -> Result<Value, String> {
    let repo_id = req_repo_id(&input)?;
    let base = hub_base(host);
    let auth = hub_auth(host);
    hf_get(
        host,
        &base,
        &format!("/api/datasets/{}", repo_path(&repo_id)),
        auth,
    )
}

fn op_space_search(input: Value, host: &mut Host) -> Result<Value, String> {
    let base = hub_base(host);
    let auth = hub_auth(host);
    let qs = search_qs(&input);
    let items = hf_get(host, &base, &format!("/api/spaces{qs}"), auth)?;
    contribute_items(host, &items, "huggingface.space");
    Ok(items)
}

fn op_whoami(_input: Value, host: &mut Host) -> Result<Value, String> {
    let base = hub_base(host);
    hf_get(host, &base, "/api/whoami-v2", Some("api_token"))
}

fn op_chat(input: Value, host: &mut Host) -> Result<Value, String> {
    let model = req_str(&input, "model")?;
    let messages = input
        .get("messages")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`messages` (non-empty array) required")?
        .clone();
    let base = router_base(host);
    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": false
    });
    if let Some(n) = input.get("max_tokens").and_then(|v| v.as_i64()) {
        if n > 0 {
            body["max_tokens"] = json!(n);
        }
    }
    if let Some(v) = input.get("temperature") {
        if !v.is_null() {
            body["temperature"] = v.clone();
        }
    }
    if let Some(v) = input.get("top_p") {
        if !v.is_null() {
            body["top_p"] = v.clone();
        }
    }
    if let Some(arr) = input.get("stop").and_then(|v| v.as_array()) {
        if !arr.is_empty() {
            body["stop"] = Value::Array(arr.clone());
        }
    }
    hf_post(host, &base, "/v1/chat/completions", &body)
}

fn op_embed(input: Value, host: &mut Host) -> Result<Value, String> {
    let model = req_str(&input, "model")?;
    let inp = input
        .get("input")
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or("`input` (non-empty array) required")?
        .clone();
    let base = router_base(host);
    let body = json!({ "model": model, "input": inp });
    hf_post(host, &base, "/v1/embeddings", &body)
}

// ---------------------------------------------------------------------------
// Input helpers.
// ---------------------------------------------------------------------------

fn req_repo_id(input: &Value) -> Result<String, String> {
    match input.get("repo_id").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err("`repo_id` (string) required".into()),
    }
}

fn req_str(input: &Value, key: &str) -> Result<String, String> {
    match input.get(key).and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
        _ => Err(format!("`{key}` (string) required")),
    }
}

// ---------------------------------------------------------------------------
// Entry point.
// ---------------------------------------------------------------------------

fn main() {
    manifest_builder().serve();
}

// ---------------------------------------------------------------------------
// Tests — one MockHost test per op (via plugin.call, mirroring gitlab pattern).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin() -> Plugin {
        manifest_builder().build()
    }

    fn run(op: &str, input: Value, host: &mut MockHost) -> Value {
        plugin().call(op, input, host).unwrap()
    }

    fn run_err(op: &str, input: Value, host: &mut MockHost) -> String {
        plugin().call(op, input, host).unwrap_err()
    }

    fn hub_host() -> MockHost {
        MockHost::default().with_endpoint("huggingface.hub", "https://huggingface.co")
    }

    fn router_host() -> MockHost {
        MockHost::default()
            .with_endpoint("huggingface.router", "https://router.huggingface.co")
            .with_secret("api_token", "hf_tok")
    }

    // ---- huggingface.test ----

    #[test]
    fn test_reachable_no_token() {
        let mut host = hub_host().with_http("/api/models?limit=1", json!([]));
        // No secret registered → has_token returns false.
        let result = run("huggingface.test", json!({}), &mut host);
        assert_eq!(result["status"], "ok");
        assert_eq!(result["reachable"], true);
        assert_eq!(result["authenticated"], false);
    }

    #[test]
    fn test_reachable_with_valid_token() {
        let mut host = hub_host()
            .with_secret("api_token", "hf_tok")
            .with_http("/api/models?limit=1", json!([]))
            .with_http("/api/whoami-v2", json!({"name": "alice", "type": "user"}));
        let result = run("huggingface.test", json!({}), &mut host);
        assert_eq!(result["status"], "ok");
        assert_eq!(result["authenticated"], true);
        assert_eq!(result["user"], "alice");
    }

    #[test]
    fn test_hub_unreachable() {
        // No http mock → get_json will fail.
        let mut host = hub_host();
        let result = run("huggingface.test", json!({}), &mut host);
        assert_eq!(result["status"], "unreachable");
    }

    // ---- huggingface.model.search ----

    #[test]
    fn model_search_returns_list_and_contributes() {
        let models = json!([
            {"id": "meta-llama/Llama-3.1-8B-Instruct", "pipeline_tag": "text-generation", "downloads": 100, "likes": 50, "tags": []},
            {"id": "google/gemma-2b", "pipeline_tag": "text-generation", "downloads": 80, "likes": 30, "tags": []}
        ]);
        let mut host = hub_host().with_http("/api/models", models.clone());
        let result = run(
            "huggingface.model.search",
            json!({"search": "llama"}),
            &mut host,
        );
        assert_eq!(result, models);
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 2);
        assert_eq!(contributed[0].entity, "huggingface.model");
        assert_eq!(contributed[0].id, "meta-llama/Llama-3.1-8B-Instruct");
    }

    // ---- huggingface.model.get ----

    #[test]
    fn model_get_fetches_by_repo_id() {
        let model =
            json!({"id": "meta-llama/Llama-3.1-8B-Instruct", "pipeline_tag": "text-generation"});
        let mut host = hub_host().with_http("/api/models/meta-llama/", model.clone());
        let result = run(
            "huggingface.model.get",
            json!({"repo_id": "meta-llama/Llama-3.1-8B-Instruct"}),
            &mut host,
        );
        assert_eq!(result["id"], "meta-llama/Llama-3.1-8B-Instruct");
    }

    #[test]
    fn model_get_requires_repo_id() {
        let mut host = hub_host();
        let err = run_err("huggingface.model.get", json!({}), &mut host);
        assert!(
            err.contains("repo_id"),
            "error should mention repo_id: {err}"
        );
    }

    // ---- huggingface.dataset.search ----

    #[test]
    fn dataset_search_returns_list_and_contributes() {
        let datasets = json!([
            {"id": "rajpurkar/squad", "downloads": 200, "tags": []},
            {"id": "glue", "downloads": 300, "tags": []}
        ]);
        let mut host = hub_host().with_http("/api/datasets", datasets.clone());
        let result = run(
            "huggingface.dataset.search",
            json!({"search": "squad"}),
            &mut host,
        );
        assert_eq!(result, datasets);
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 2);
        assert_eq!(contributed[0].entity, "huggingface.dataset");
        assert_eq!(contributed[0].id, "rajpurkar/squad");
    }

    // ---- huggingface.dataset.get ----

    #[test]
    fn dataset_get_fetches_by_repo_id() {
        let ds = json!({"id": "rajpurkar/squad", "downloads": 200});
        let mut host = hub_host().with_http("/api/datasets/rajpurkar/", ds.clone());
        let result = run(
            "huggingface.dataset.get",
            json!({"repo_id": "rajpurkar/squad"}),
            &mut host,
        );
        assert_eq!(result["id"], "rajpurkar/squad");
    }

    // ---- huggingface.space.search ----

    #[test]
    fn space_search_returns_list_and_contributes() {
        let spaces = json!([
            {"id": "openai/whisper", "sdk": "gradio", "likes": 10, "tags": []},
            {"id": "stabilityai/stable-diffusion", "sdk": "gradio", "likes": 50, "tags": []}
        ]);
        let mut host = hub_host().with_http("/api/spaces", spaces.clone());
        let result = run(
            "huggingface.space.search",
            json!({"search": "whisper"}),
            &mut host,
        );
        assert_eq!(result, spaces);
        let contributed = host.contributed.borrow();
        assert_eq!(contributed.len(), 2);
        assert_eq!(contributed[0].entity, "huggingface.space");
        assert_eq!(contributed[0].id, "openai/whisper");
    }

    // ---- huggingface.whoami ----

    #[test]
    fn whoami_returns_identity() {
        let who = json!({"name": "alice", "fullname": "Alice Test", "type": "user", "orgs": []});
        let mut host = hub_host()
            .with_secret("api_token", "hf_tok")
            .with_http("/api/whoami-v2", who.clone());
        let result = run("huggingface.whoami", json!({}), &mut host);
        assert_eq!(result["name"], "alice");
    }

    // ---- huggingface.chat ----

    #[test]
    fn chat_posts_to_completions_endpoint() {
        let resp = json!({
            "id": "chatcmpl-abc",
            "choices": [{"message": {"role": "assistant", "content": "Hello!"}, "finish_reason": "stop"}],
            "model": "meta-llama/Llama-3.1-8B-Instruct"
        });
        let mut host = router_host().with_http("/v1/chat/completions", resp.clone());
        let result = run(
            "huggingface.chat",
            json!({
                "model": "meta-llama/Llama-3.1-8B-Instruct",
                "messages": [{"role": "user", "content": "Hello!"}]
            }),
            &mut host,
        );
        assert_eq!(result["id"], "chatcmpl-abc");
        assert_eq!(result["choices"][0]["message"]["content"], "Hello!");
    }

    #[test]
    fn chat_requires_model() {
        let mut host = router_host();
        let err = run_err(
            "huggingface.chat",
            json!({"messages": [{"role": "user", "content": "hi"}]}),
            &mut host,
        );
        assert!(err.contains("model"), "error should mention model: {err}");
    }

    #[test]
    fn chat_requires_messages() {
        let mut host = router_host();
        let err = run_err(
            "huggingface.chat",
            json!({"model": "meta-llama/Llama-3.1-8B-Instruct", "messages": []}),
            &mut host,
        );
        assert!(
            err.contains("messages"),
            "error should mention messages: {err}"
        );
    }

    // ---- huggingface.embed ----

    #[test]
    fn embed_posts_to_embeddings_endpoint() {
        let resp = json!({
            "object": "list",
            "data": [{"object": "embedding", "embedding": [0.1, 0.2, 0.3], "index": 0}],
            "model": "intfloat/multilingual-e5-large"
        });
        let mut host = router_host().with_http("/v1/embeddings", resp.clone());
        let result = run(
            "huggingface.embed",
            json!({
                "model": "intfloat/multilingual-e5-large",
                "input": ["hello world"]
            }),
            &mut host,
        );
        assert_eq!(result["data"][0]["embedding"][0], 0.1);
    }

    #[test]
    fn embed_requires_model() {
        let mut host = router_host();
        let err = run_err("huggingface.embed", json!({"input": ["hello"]}), &mut host);
        assert!(err.contains("model"), "error should mention model: {err}");
    }

    #[test]
    fn embed_requires_input() {
        let mut host = router_host();
        let err = run_err(
            "huggingface.embed",
            json!({"model": "intfloat/multilingual-e5-large", "input": []}),
            &mut host,
        );
        assert!(err.contains("input"), "error should mention input: {err}");
    }
}
