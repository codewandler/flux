//! `websearch` — a flux integration plugin: web search via Tavily (when `TAVILY_API_KEY` is set) with a
//! DuckDuckGo Instant-Answer fallback. Results are returned and contributed as `web.result` datasource
//! records so the agent can `search`/`get` them later. `websearch.provider.list` reports the two backends
//! and which one is active.
//!
//! Flux folds both backends into this one plugin (Tavily primary, DuckDuckGo fallback) rather than the
//! fluxplane aggregator + per-provider-plugin split; `websearch.search` takes an optional `providers`
//! filter to pin a backend.

use host_kit::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

// ===========================================================================
// Schema-only op input structs (D-36)
// ===========================================================================
// Each op's `input_schema` is derived from the structs below via schemars
// (`host_kit::read_op_typed::<T>`), instead of a hand-written `json!({...})`
// object, so the schema the model sees cannot drift from a separately-maintained
// literal. The structs are schema-only: handlers keep their existing Value
// extraction (the D-34 schema-only precedent).

/// `websearch.search`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct SearchInput {
    /// Single search query (convenience field).
    query: Option<String>,
    /// Multiple search queries executed in order.
    queries: Option<Vec<String>>,
    /// Maximum results per query (alias: `max`).
    max_results: Option<i64>,
    /// Alias for `max_results`.
    max: Option<i64>,
    /// Alias for `max_results` (datasource-search convention).
    limit: Option<i64>,
    /// Optional backend filter: "tavily" and/or "duckduckgo" (alias "ddg").
    providers: Option<Vec<String>>,
}

/// `websearch.provider.list`.
#[derive(Deserialize, JsonSchema)]
#[allow(dead_code)]
struct ProviderListInput {}

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("websearch", "0.1.0")
        .capabilities(Caps {
            http: true,
            http_hosts: vec!["api.tavily.com".into(), "api.duckduckgo.com".into()],
            secrets: vec!["TAVILY_API_KEY".into()],
            ..Default::default()
        })
        .auth(AuthMethod {
            purpose: "tavily_api_key".into(),
            env: vec!["TAVILY_API_KEY".into()],
            description: "Tavily API key (optional; falls back to DuckDuckGo)".into(),
            ..Default::default()
        })
        .datasource(Declaration {
            name: "websearch.results".into(),
            entity: "web.result".into(),
            description: Some("Web search results.".into()),
            capabilities: vec!["search".into(), "get".into()],
            entity_schema: None,
        })
        .operation(
            read_op_typed::<SearchInput>(
                "websearch.search",
                "Search the web (Tavily if configured, else DuckDuckGo). Returns ranked results.",
            ),
            search,
        )
        .operation(
            read_op_typed::<ProviderListInput>(
                "websearch.provider.list",
                "List the web-search backends and which one is active (Tavily when configured, else DuckDuckGo).",
            ),
            provider_list,
        )
}

const DEFAULT_MAX: u64 = 10;
const MAX_RESULTS: u64 = 20;
const MAX_QUERIES: usize = 5;
const MAX_QUERY_LENGTH: usize = 500;

fn search(input: Value, host: &mut Host) -> Result<Value, String> {
    let queries = normalize_queries(&input)?;
    let max = normalize_max(&input);

    // Optional backend selection (default: both, Tavily preferred — primary/fallback).
    let requested = providers_filter(&input);
    let allow_tavily = requested.is_empty() || requested.iter().any(|p| p == "tavily");
    let allow_ddg =
        requested.is_empty() || requested.iter().any(|p| p == "duckduckgo" || p == "ddg");
    if !allow_tavily && !allow_ddg {
        return Err(format!(
            "websearch.search: no known provider in `providers` (have: tavily, duckduckgo); got {requested:?}"
        ));
    }

    let mut all_results = Vec::new();
    for query in &queries {
        let mut results = if allow_tavily {
            match host.secret("tavily_api_key") {
                Ok(key) => tavily(host, &key, query, max)?,
                Err(_) if allow_ddg => duckduckgo(host, query, max)?,
                Err(_) => {
                    return Err(
                        "websearch.search: provider `tavily` requested but TAVILY_API_KEY is not configured"
                            .into(),
                    )
                }
            }
        } else {
            duckduckgo(host, query, max)?
        };
        all_results.append(&mut results);
    }

    // Contribute the results as records so they're searchable knowledge afterwards.
    let records: Vec<Record> = all_results
        .iter()
        .filter_map(|r| {
            let url = r.get("url").and_then(|v| v.as_str())?;
            Some(Record::new(
                Source::new("websearch"),
                "web.result",
                url,
                r.get("title").and_then(|v| v.as_str()).unwrap_or(url),
                r.get("content").and_then(|v| v.as_str()).unwrap_or(""),
            ))
        })
        .collect();
    if !records.is_empty() {
        let _ = host.contribute(&records);
    }

    Ok(json!({ "results": all_results }))
}

/// Normalize queries from `query` and/or `queries`, trim, deduplicate, and validate.
fn normalize_queries(input: &Value) -> Result<Vec<String>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let mut push = |q: &str| {
        let q = q.trim();
        if q.is_empty() || seen.contains(q) {
            return;
        }
        seen.insert(q.to_string());
        out.push(q.to_string());
    };

    if let Some(q) = input.get("query").and_then(|v| v.as_str()) {
        push(q);
    }
    if let Some(arr) = input.get("queries").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(q) = v.as_str() {
                push(q);
            }
        }
    }

    if out.is_empty() {
        return Err("websearch.search: at least one query is required".into());
    }
    if out.len() > MAX_QUERIES {
        return Err(format!(
            "websearch.search: at most {MAX_QUERIES} queries are allowed"
        ));
    }
    for q in &out {
        if q.len() > MAX_QUERY_LENGTH {
            return Err(format!(
                "websearch.search: query exceeds {MAX_QUERY_LENGTH} characters"
            ));
        }
    }
    Ok(out)
}

/// Resolve `max_results` → `max` → `limit`, defaulting to 10 and capping at 20.
fn normalize_max(input: &Value) -> u64 {
    let pick = input
        .get("max_results")
        .and_then(|v| v.as_u64())
        .or_else(|| input.get("max").and_then(|v| v.as_u64()))
        .or_else(|| input.get("limit").and_then(|v| v.as_u64()))
        .unwrap_or(DEFAULT_MAX);
    if pick == 0 || pick > MAX_RESULTS {
        MAX_RESULTS
    } else {
        pick
    }
}

/// Normalized, lowercased provider names from the `providers` input (empty = no filter).
fn providers_filter(input: &Value) -> Vec<String> {
    input
        .get("providers")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// List the search backends folded into this plugin and which one is active. Tavily is active (primary)
/// when `TAVILY_API_KEY` resolves; otherwise the keyless DuckDuckGo fallback is active.
fn provider_list(_input: Value, host: &mut Host) -> Result<Value, String> {
    let tavily_available = host.secret("tavily_api_key").is_ok();
    Ok(json!({
        "providers": [
            {
                "name": "tavily",
                "description": "Tavily Search API (ranked results; requires TAVILY_API_KEY).",
                "auth_required": true,
                "available": tavily_available,
                "active": tavily_available
            },
            {
                "name": "duckduckgo",
                "aliases": ["ddg"],
                "description": "DuckDuckGo Instant Answer API (no key; fallback).",
                "auth_required": false,
                "available": true,
                "active": !tavily_available
            }
        ],
        "count": 2
    }))
}

/// Tavily: POST /search with the API key in the body (not a bearer header).
fn tavily(host: &mut Host, key: &str, query: &str, max: u64) -> Result<Vec<Value>, String> {
    let body = json!({
        "api_key": key,
        "query": query,
        "max_results": max,
        "search_depth": "basic"
    });
    let resp = host.send_json("POST", "https://api.tavily.com/search", None, &body)?;
    let results = resp
        .get("results")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|r| {
            json!({
                "title": r.get("title").and_then(|v| v.as_str()).unwrap_or(""),
                "url": r.get("url").and_then(|v| v.as_str()).unwrap_or(""),
                "content": r.get("content").and_then(|v| v.as_str()).unwrap_or("")
            })
        })
        .collect();
    Ok(results)
}

/// DuckDuckGo Instant Answer API (no key): GET /?q=…&format=json. Best-effort, limited recall.
fn duckduckgo(host: &mut Host, query: &str, max: u64) -> Result<Vec<Value>, String> {
    let url = format!(
        "https://api.duckduckgo.com/?q={}&format=json&no_html=1",
        urlencode(query)
    );
    let resp = host.get_json(&url, None)?;
    let mut out = Vec::new();
    if let Some(abstract_text) = resp.get("AbstractText").and_then(|v| v.as_str()) {
        if !abstract_text.is_empty() {
            out.push(json!({
                "title": resp.get("Heading").and_then(|v| v.as_str()).unwrap_or(query),
                "url": resp.get("AbstractURL").and_then(|v| v.as_str()).unwrap_or(""),
                "content": abstract_text
            }));
        }
    }
    for topic in resp
        .get("RelatedTopics")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[])
    {
        if out.len() as u64 >= max {
            break;
        }
        if let (Some(text), Some(url)) = (
            topic.get("Text").and_then(|v| v.as_str()),
            topic.get("FirstURL").and_then(|v| v.as_str()),
        ) {
            out.push(json!({ "title": text, "url": url, "content": text }));
        }
    }
    Ok(out)
}

/// Minimal percent-encoding for a query string (alnum + `-_.~` pass; everything else is `%XX`).
fn urlencode(s: &str) -> String {
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

fn main() {
    manifest_builder().serve();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tavily_path_returns_results_and_contributes_records() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("tavily_api_key", "k")
            .with_http(
                "api.tavily.com",
                json!({ "results": [
                    { "title": "Warm transfer", "url": "https://x/y", "content": "how warm transfer works" }
                ]}),
            );
        let out = plugin
            .call(
                "websearch.search",
                json!({ "query": "warm transfer" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["results"][0]["url"], "https://x/y");
        // result was contributed as a record
        assert_eq!(host.contributed.borrow().len(), 1);
        assert_eq!(host.contributed.borrow()[0].entity, "web.result");
    }

    #[test]
    fn falls_back_to_duckduckgo_without_a_key() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_http(
            "duckduckgo.com",
            json!({ "Heading": "Rust", "AbstractURL": "https://r/", "AbstractText": "a language", "RelatedTopics": [] }),
        );
        let out = plugin
            .call(
                "websearch.search",
                json!({ "query": "rust lang" }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["results"][0]["title"], "Rust");
    }

    #[test]
    fn providers_filter_forces_duckduckgo_even_with_a_tavily_key() {
        // Tavily is configured, but `providers: ["duckduckgo"]` pins the keyless backend.
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("tavily_api_key", "k")
            .with_http(
                "duckduckgo.com",
                json!({ "Heading": "Pinned", "AbstractURL": "https://d/", "AbstractText": "via ddg", "RelatedTopics": [] }),
            );
        let out = plugin
            .call(
                "websearch.search",
                json!({ "query": "anything", "providers": ["duckduckgo"] }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["results"][0]["title"], "Pinned");
    }

    #[test]
    fn provider_list_marks_tavily_active_when_key_present() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_secret("tavily_api_key", "k");
        let out = plugin
            .call("websearch.provider.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["count"], 2);
        assert_eq!(out["providers"][0]["name"], "tavily");
        assert_eq!(out["providers"][0]["active"], true);
        assert_eq!(out["providers"][1]["active"], false);
    }

    #[test]
    fn provider_list_marks_duckduckgo_active_without_a_key() {
        let plugin = manifest_builder().build();
        let mut host = MockHost::default();
        let out = plugin
            .call("websearch.provider.list", json!({}), &mut host)
            .unwrap();
        assert_eq!(out["providers"][0]["available"], false); // tavily: no key
        assert_eq!(out["providers"][1]["name"], "duckduckgo");
        assert_eq!(out["providers"][1]["active"], true); // ddg fallback active
    }

    // -- ported gaps (D-36) --

    #[test]
    fn limit_alias_caps_max_results() {
        // DuckDuckGo client-side truncation means `limit` controls returned count.
        let plugin = manifest_builder().build();
        let mut host = MockHost::default().with_http(
            "duckduckgo.com",
            json!({
                "Heading": "Top",
                "AbstractURL": "https://a/",
                "AbstractText": "abstract",
                "RelatedTopics": [
                    { "Text": "one", "FirstURL": "https://1/" },
                    { "Text": "two", "FirstURL": "https://2/" },
                    { "Text": "three", "FirstURL": "https://3/" }
                ]
            }),
        );
        let out = plugin
            .call(
                "websearch.search",
                json!({ "query": "widgets", "limit": 2 }),
                &mut host,
            )
            .unwrap();
        assert_eq!(out["results"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn queries_array_runs_multiple_queries() {
        // Two queries sequential through Tavily, each returning a distinct result.
        let plugin = manifest_builder().build();
        let mut host = MockHost::default()
            .with_secret("tavily_api_key", "k")
            .with_http_seq(
                "api.tavily.com",
                json!({ "results": [{ "title": "A", "url": "https://a/", "content": "a" }] }),
            )
            .with_http_seq(
                "api.tavily.com",
                json!({ "results": [{ "title": "B", "url": "https://b/", "content": "b" }] }),
            );
        let out = plugin
            .call(
                "websearch.search",
                json!({ "queries": ["foo", "bar"], "providers": ["tavily"] }),
                &mut host,
            )
            .unwrap();
        let urls: Vec<String> = out["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["url"].as_str().map(String::from))
            .collect();
        assert!(urls.contains(&"https://a/".to_string()));
        assert!(urls.contains(&"https://b/".to_string()));
    }
}

// ===========================================================================
// Schema contract
// ===========================================================================
#[cfg(test)]
mod schema_contract {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Kind {
        Str,
        Int,
        ArrayStr,
    }
    #[derive(Clone)]
    struct Prop {
        name: &'static str,
        kind: Kind,
    }
    struct OpContract {
        props: Vec<Prop>,
        required: Vec<&'static str>,
    }
    fn p(name: &'static str, kind: Kind) -> Prop {
        Prop { name, kind }
    }
    fn c(props: Vec<Prop>, required: Vec<&'static str>) -> OpContract {
        OpContract { props, required }
    }

    fn contracts() -> Vec<(&'static str, OpContract)> {
        vec![
            (
                "websearch.search",
                c(
                    vec![
                        p("query", Kind::Str),
                        p("queries", Kind::ArrayStr),
                        p("max_results", Kind::Int),
                        p("max", Kind::Int),
                        p("limit", Kind::Int),
                        p("providers", Kind::ArrayStr),
                    ],
                    vec![],
                ),
            ),
            ("websearch.provider.list", c(vec![], vec![])),
        ]
    }

    fn resolve<'a>(node: &'a Value, defs: &'a Value) -> &'a Value {
        if let Some(obj) = node.as_object() {
            if let Some(r) = obj.get("$ref").and_then(|v| v.as_str()) {
                if let Some(name) = r.strip_prefix("#/definitions/") {
                    return defs.get(name).unwrap_or(node);
                }
            }
            if let Some(any) = obj.get("anyOf").and_then(|v| v.as_array()) {
                for m in any {
                    if m.get("type").and_then(|v| v.as_str()) != Some("null") {
                        return resolve(m, defs);
                    }
                }
            }
        }
        node
    }

    fn kind_of(node: &Value) -> Kind {
        let t = node.get("type");
        if let Some(arr) = t.and_then(|v| v.as_array()) {
            let first = arr
                .iter()
                .find(|v| v.as_str() != Some("null"))
                .and_then(|v| v.as_str())
                .unwrap_or("null");
            return base_kind(first, node);
        }
        base_kind(t.and_then(|v| v.as_str()).unwrap_or(""), node)
    }

    fn base_kind(t: &str, node: &Value) -> Kind {
        match t {
            "integer" => Kind::Int,
            "string" => Kind::Str,
            "array" => {
                let items = node.get("items").cloned().unwrap_or(Value::Null);
                if items.get("type").and_then(|v| v.as_str()) == Some("string") {
                    Kind::ArrayStr
                } else {
                    panic!("unsupported array item type: {items}")
                }
            }
            other => panic!("unsupported property type: {other} ({node})"),
        }
    }

    fn assert_contract(op_name: &str, schema: &Value, contract: &OpContract) {
        assert_eq!(schema["type"], "object", "{op_name}: root type");
        let defs = schema.get("definitions").cloned().unwrap_or(json!({}));
        let props_obj = schema.get("properties").and_then(|v| v.as_object());
        let mut got: BTreeMap<&str, Kind> = BTreeMap::new();
        if let Some(props) = props_obj {
            for (k, v) in props {
                got.insert(k.as_str(), kind_of(resolve(v, &defs)));
            }
        }
        let want: BTreeMap<&str, Kind> = contract
            .props
            .iter()
            .map(|Prop { name, kind }| (*name, kind.clone()))
            .collect();
        assert_eq!(got.len(), want.len(), "{op_name}: property count");
        for Prop { name, kind } in &contract.props {
            let got_kind = got
                .get(*name)
                .unwrap_or_else(|| panic!("{op_name}: missing property `{name}`"));
            assert_eq!(got_kind, kind, "{op_name}: property `{name}` kind");
        }
        let req: Vec<&str> = schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let mut req_set: Vec<&str> = req.clone();
        req_set.sort();
        let mut want_req: Vec<&str> = contract.required.clone();
        want_req.sort();
        assert_eq!(req_set, want_req, "{op_name}: required set");
    }

    #[test]
    fn derived_schemas_match_contract() {
        let ops = contracts();
        let manifest = manifest_builder().build().manifest();
        let by_name: BTreeMap<&str, &OperationSpec> = manifest
            .operations
            .iter()
            .map(|o| (o.name.as_str(), o))
            .collect();
        assert_eq!(by_name.len(), ops.len(), "op count changed");
        for (name, contract) in &ops {
            let spec = by_name
                .get(*name)
                .unwrap_or_else(|| panic!("missing op {name}"));
            assert_contract(name, &spec.input_schema, contract);
        }
    }
}
