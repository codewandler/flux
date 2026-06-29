//! The agent-facing retrieval ops over a [`DatasourceBackend`]: `search` / `get` / `list` / `relation`
//! / `batch_get`. Each is a read-only [`Tool`] that parses its JSON input into the matching
//! `flux-datasource` request type, calls the backend, and renders a compact text result.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput,
};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::ToolSpec;

use super::DatasourceBackend;

/// The five datasource retrieval ops over `backend`, as a tool vec (the form a surface registers into
/// an agent/app registry — e.g. `App::with_tools`).
pub fn datasource_tools(backend: Arc<dyn DatasourceBackend>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(SearchOp(backend.clone())) as Arc<dyn Tool>,
        Arc::new(GetOp(backend.clone())),
        Arc::new(ListOp(backend.clone())),
        Arc::new(RelationOp(backend.clone())),
        Arc::new(BatchGetOp(backend)),
    ]
}

/// Register all five datasource retrieval ops over `backend` into `registry`.
pub fn register_datasource_ops(registry: &mut ToolRegistry, backend: Arc<dyn DatasourceBackend>) {
    for tool in datasource_tools(backend) {
        registry.register(tool);
    }
}

/// `[entity id] (source, score) title — snippet`
fn render_match(m: &Match) -> String {
    let title = if m.record.title.is_empty() {
        String::new()
    } else {
        format!("{} — ", m.record.title)
    };
    format!(
        "[{} {}] ({}, score {:.0}) {}{}",
        m.record.entity,
        m.record.id,
        m.record.source.key(),
        m.score,
        title,
        m.record.body
    )
}

/// `[entity id] (source) title` + body on the next line — for `get`/`batch_get`/`list`/`relation`.
fn render_record(r: &Record) -> String {
    let mut out = format!("[{} {}] ({})", r.entity, r.id, r.source.key());
    if !r.title.is_empty() {
        out.push(' ');
        out.push_str(&r.title);
    }
    if !r.body.is_empty() {
        out.push('\n');
        out.push_str(&r.body);
    }
    out
}

fn parse<T: serde::de::DeserializeOwned>(op: &str, params: Value) -> Result<T> {
    serde_json::from_value(params).map_err(|e| Error::Other(format!("{op}: bad input: {e}")))
}

/// `search` — keyword search over the indexed datasource.
struct SearchOp(Arc<dyn DatasourceBackend>);

#[async_trait]
impl Tool for SearchOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "search",
            "Search the indexed knowledge datasource (local docs + integration records) by keyword.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "source": {"type": "string", "description": "Restrict to one source (e.g. \"local\", \"gitlab\")"},
                    "entity": {"type": "string", "description": "Restrict to one entity type (e.g. \"file.document\")"},
                    "limit": {"type": "integer", "description": "Max results (default 5)"}
                },
                "required": ["query"]
            }),
        )
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        params
            .get("query")
            .and_then(|v| v.as_str())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default()
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let input: SearchInput = parse("search", params)?;
        let hits = self.0.search(&input)?;
        if hits.is_empty() {
            return Ok(ToolResult::ok("no matches"));
        }
        Ok(ToolResult::ok(
            hits.iter().map(render_match).collect::<Vec<_>>().join("\n"),
        ))
    }
}

/// `get` — fetch one record by its `(source, entity, id)` address.
struct GetOp(Arc<dyn DatasourceBackend>);

#[async_trait]
impl Tool for GetOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "get",
            "Fetch one datasource record in full by its source, entity, and id.",
            json!({
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "entity": {"type": "string"},
                    "id": {"type": "string"}
                },
                "required": ["source", "entity", "id"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let input: GetInput = parse("get", params)?;
        match self.0.get(&input)? {
            Some(r) => Ok(ToolResult::ok(render_record(&r))),
            None => Ok(ToolResult::ok("not found")),
        }
    }
}

/// `list` — enumerate a source's records (optionally one entity), paged.
struct ListOp(Arc<dyn DatasourceBackend>);

#[async_trait]
impl Tool for ListOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "list",
            "List datasource records from one source (optionally one entity type), paged.",
            json!({
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "entity": {"type": "string"},
                    "offset": {"type": "integer"},
                    "limit": {"type": "integer", "description": "Max results (default all)"}
                },
                "required": ["source"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let input: ListInput = parse("list", params)?;
        let recs = self.0.list(&input)?;
        if recs.is_empty() {
            return Ok(ToolResult::ok("no records"));
        }
        Ok(ToolResult::ok(
            recs.iter()
                .map(|r| format!("[{} {}] {}", r.entity, r.id, r.title))
                .collect::<Vec<_>>()
                .join("\n"),
        ))
    }
}

/// `relation` — the records linked from one record (optionally filtered by relation name).
struct RelationOp(Arc<dyn DatasourceBackend>);

#[async_trait]
impl Tool for RelationOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "relation",
            "Follow a record's typed relations to the linked records (optionally one relation name).",
            json!({
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "entity": {"type": "string"},
                    "id": {"type": "string"},
                    "rel": {"type": "string", "description": "Only this relation name"}
                },
                "required": ["source", "entity", "id"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let input: RelationInput = parse("relation", params)?;
        let recs = self.0.relation(&input)?;
        if recs.is_empty() {
            return Ok(ToolResult::ok("no linked records"));
        }
        Ok(ToolResult::ok(
            recs.iter()
                .map(render_record)
                .collect::<Vec<_>>()
                .join("\n\n"),
        ))
    }
}

/// `batch_get` — fetch several records of one entity from one source in one call.
struct BatchGetOp(Arc<dyn DatasourceBackend>);

#[async_trait]
impl Tool for BatchGetOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "batch_get",
            "Fetch several datasource records of one entity (from one source) by id, in one call.",
            json!({
                "type": "object",
                "properties": {
                    "source": {"type": "string"},
                    "entity": {"type": "string"},
                    "ids": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["source", "entity", "ids"]
            }),
        )
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let input: BatchGetInput = parse("batch_get", params)?;
        let recs = self.0.batch_get(&input)?;
        if recs.is_empty() {
            return Ok(ToolResult::ok("no records"));
        }
        Ok(ToolResult::ok(
            recs.iter()
                .map(render_record)
                .collect::<Vec<_>>()
                .join("\n\n"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datasource::MemoryBackend;
    use flux_datasource::Source;
    use flux_system::{System, Workspace};

    fn ctx() -> ToolContext {
        let dir = std::env::temp_dir().join(format!("flux-ds-ops-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&dir).unwrap())))
    }

    fn backend() -> Arc<dyn DatasourceBackend> {
        let b = MemoryBackend::new();
        b.upsert(&[Record::new(
            Source::new("local"),
            "file.document",
            "warm-transfer",
            "Warm transfer",
            "A warm transfer connects the caller to an agent after an announcement.",
        )])
        .unwrap();
        Arc::new(b)
    }

    #[tokio::test]
    async fn search_op_returns_hits_and_get_round_trips() {
        let b = backend();
        let search = SearchOp(b.clone());
        let r = search
            .execute(&ctx(), json!({"query": "warm transfer", "limit": 3}))
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("warm-transfer"), "got: {}", r.content);

        let get = GetOp(b);
        let g = get
            .execute(
                &ctx(),
                json!({"source": "local", "entity": "file.document", "id": "warm-transfer"}),
            )
            .await
            .unwrap();
        assert!(g.content.contains("Warm transfer"));
    }
}
