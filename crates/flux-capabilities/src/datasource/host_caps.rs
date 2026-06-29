//! [`DatasourceHostCaps`] — the L5 bridge that lets a plugin contribute and query datasource records.
//!
//! Wraps flux-plugin's [`SystemHostCaps`] (process / secret / http / endpoint) and additionally services
//! the `datasource.*` host capabilities against a [`DatasourceBackend`] (the D-07 index): a plugin emits
//! records (`datasource.records`) that become searchable knowledge, and can query the index
//! (`datasource.search` / `datasource.get`). Non-datasource commands delegate to the inner caps. This is
//! where D-08 integration plugins' contributed records reach the D-07 knowledge layer. Layering: this is
//! L5 (it owns the index) wrapping L4 `SystemHostCaps`; flux-plugin defines only the trait + protocol.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_datasource::{GetInput, Record, SearchInput};
use flux_plugin::{HostCapabilities, SystemHostCaps};

use super::DatasourceBackend;

/// Host capabilities = the guarded `SystemHostCaps` **plus** the `datasource.*` commands backed by a
/// shared [`DatasourceBackend`].
pub struct DatasourceHostCaps {
    inner: SystemHostCaps,
    backend: Arc<dyn DatasourceBackend>,
}

impl DatasourceHostCaps {
    /// Wrap `inner` (the system-backed caps for a plugin) so its `datasource.*` calls hit `backend`.
    pub fn new(inner: SystemHostCaps, backend: Arc<dyn DatasourceBackend>) -> Self {
        Self { inner, backend }
    }
}

#[async_trait]
impl HostCapabilities for DatasourceHostCaps {
    async fn handle(&self, command: &str, payload: &Value) -> std::result::Result<Value, String> {
        match command {
            "datasource.records" => {
                let records: Vec<Record> =
                    serde_json::from_value(payload.get("records").cloned().unwrap_or(Value::Null))
                        .map_err(|e| format!("datasource.records: bad `records`: {e}"))?;
                let indexed = records.len();
                self.backend.upsert(&records).map_err(|e| e.to_string())?;
                Ok(json!({ "indexed": indexed }))
            }
            "datasource.search" => {
                let input: SearchInput = serde_json::from_value(payload.clone())
                    .map_err(|e| format!("datasource.search: bad input: {e}"))?;
                let hits = self.backend.search(&input).map_err(|e| e.to_string())?;
                serde_json::to_value(hits).map_err(|e| e.to_string())
            }
            "datasource.get" => {
                let input: GetInput = serde_json::from_value(payload.clone())
                    .map_err(|e| format!("datasource.get: bad input: {e}"))?;
                let rec = self.backend.get(&input).map_err(|e| e.to_string())?;
                serde_json::to_value(rec).map_err(|e| e.to_string())
            }
            // Everything else (process/secret/http/endpoint) is the inner system caps' job.
            other => self.inner.handle(other, payload).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasource::MemoryBackend;
    use flux_system::{System, Workspace};

    #[tokio::test]
    async fn plugin_contributed_records_become_searchable() {
        let dir = std::env::temp_dir().join(format!("flux-dshostcaps-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let system = Arc::new(System::new(Workspace::new(&dir).unwrap()));
        let backend: Arc<dyn DatasourceBackend> = Arc::new(MemoryBackend::new());
        let caps = DatasourceHostCaps::new(SystemHostCaps::new(system), backend.clone());

        // A plugin contributes a record via the host capability …
        let contributed = caps
            .handle(
                "datasource.records",
                &json!({ "records": [{
                    "entity": "gitlab.merge_request",
                    "id": "42",
                    "source": { "plugin": "gitlab" },
                    "title": "Fix the warm transfer bug",
                    "body": "MR !42 fixes the warm transfer announcement timing."
                }]}),
            )
            .await
            .unwrap();
        assert_eq!(contributed["indexed"], 1);
        assert_eq!(backend.len(), 1);

        // … and it is then searchable through the same bridge.
        let hits = caps
            .handle("datasource.search", &json!({ "query": "warm transfer" }))
            .await
            .unwrap();
        let arr = hits.as_array().expect("search returns an array of matches");
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["record"]["id"], "42");

        // A non-datasource command delegates to the inner caps (and is denied when ungranted).
        assert!(caps
            .handle("http.do", &json!({ "url": "http://example.com" }))
            .await
            .is_err());

        std::fs::remove_dir_all(&dir).ok();
    }
}
