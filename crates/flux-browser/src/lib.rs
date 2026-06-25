//! `flux-browser` — guarded web access for the agent.
//!
//! v1 ships a [`WebFetchTool`] that fetches a URL over HTTP(S) with an egress guard: it rejects
//! non-HTTP schemes and (unless explicitly allowed) loopback/private/link-local addresses, the
//! basic protection against SSRF and metadata-endpoint access. Full CDP browser automation
//! (navigate/screenshot/DOM via chromiumoxide) is the planned upgrade behind the same tool
//! surface; it requires a Chrome binary at runtime, so it's deferred to keep the build verifiable.

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_runtime::{Tool, ToolContext, ToolResult};
use flux_spec::ToolSpec;

const MAX_BYTES: usize = 256 * 1024;

/// Reject URLs that aren't safe to fetch. Delegates to the shared egress guard in `flux-system`
/// (the single SSRF policy: scheme check + host→IP resolution against private/loopback/link-local
/// ranges). Re-exported here so callers and tests of `flux-browser` keep a stable entry point.
pub fn guard_url(raw: &str, allow_private: bool) -> Result<url::Url> {
    flux_system::net::guard_url(raw, allow_private)
}

/// A tool that fetches a URL's body (guarded, size-capped).
pub struct WebFetchTool {
    http: reqwest::Client,
    allow_private: bool,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self {
            http: reqwest::Client::new(),
            allow_private: false,
        }
    }
}

impl WebFetchTool {
    /// Allow fetching private/loopback addresses (e.g. for local development). Off by default.
    pub fn allow_private(mut self, yes: bool) -> Self {
        self.allow_private = yes;
        self
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "web_fetch",
            "Fetch the contents of an HTTP(S) URL. Loopback/private addresses are blocked.",
            json!({
                "type": "object",
                "properties": {"url": {"type": "string"}},
                "required": ["url"]
            }),
        )
        .with_effects(vec![flux_spec::Effect::Network])
        .with_access(vec![flux_spec::AccessKind::Network])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let raw = params
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Other("web_fetch: `url` required".into()))?;
        let url = guard_url(raw, self.allow_private)?;
        let resp = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| Error::Http(e.to_string()))?;
        let status = resp.status();
        let bytes = resp.bytes().await.map_err(|e| Error::Http(e.to_string()))?;
        let mut body = String::from_utf8_lossy(&bytes).into_owned();
        if body.len() > MAX_BYTES {
            // Cut on a char boundary — `String::truncate` panics off one (the cut can land inside a
            // multibyte codepoint of an arbitrary response body).
            let mut end = MAX_BYTES;
            while end > 0 && !body.is_char_boundary(end) {
                end -= 1;
            }
            body.truncate(end);
            body.push_str("\n…[truncated]");
        }
        Ok(ToolResult {
            content: format!("[{status}]\n{body}"),
            view: None,
            is_error: !status.is_success(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_allows_public_https() {
        assert!(guard_url("https://example.com/path", false).is_ok());
        assert!(guard_url("http://93.184.216.34/", false).is_ok());
    }

    #[test]
    fn guard_blocks_local_and_private() {
        assert!(guard_url("http://localhost:8080", false).is_err());
        assert!(guard_url("http://127.0.0.1/", false).is_err());
        assert!(guard_url("http://10.0.0.5/", false).is_err());
        assert!(guard_url("http://192.168.1.1/", false).is_err());
        assert!(guard_url("http://169.254.169.254/latest/meta-data/", false).is_err());
        // cloud metadata
    }

    #[test]
    fn guard_blocks_non_http_schemes() {
        assert!(guard_url("file:///etc/passwd", false).is_err());
        assert!(guard_url("ftp://example.com", false).is_err());
    }

    #[test]
    fn allow_private_opt_in() {
        assert!(guard_url("http://127.0.0.1/", true).is_ok());
    }
}
