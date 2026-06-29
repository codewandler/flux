//! `websearch` — a flux integration plugin: web search via Tavily (when `TAVILY_API_KEY` is set) with a
//! DuckDuckGo Instant-Answer fallback. Results are returned and contributed as `web.result` datasource
//! records so the agent can `search`/`get` them later.

use host_kit::*;
use serde_json::{json, Value};

fn manifest_builder() -> PluginBuilder {
    PluginBuilder::new("websearch", "0.1.0")
        .capabilities(Caps {
            http: true,
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
            read_op(
                "websearch.search",
                "Search the web (Tavily if configured, else DuckDuckGo). Returns ranked results.",
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"},
                        "max_results": {"type": "integer", "description": "default 5"}
                    },
                    "required": ["query"]
                }),
            ),
            search,
        )
}

fn search(input: Value, host: &mut Host) -> Result<Value, String> {
    let query = input
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or("websearch.search: `query` required")?;
    let max = input
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(5);

    let results = match host.secret("tavily_api_key") {
        Ok(key) => tavily(host, &key, query, max)?,
        Err(_) => duckduckgo(host, query, max)?,
    };

    // Contribute the results as records so they're searchable knowledge afterwards.
    let records: Vec<Record> = results
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

    Ok(json!({ "results": results }))
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
}
