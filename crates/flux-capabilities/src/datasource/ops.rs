//! The agent-facing retrieval ops over a [`DatasourceBackend`]: `search` / `get` / `list` / `relation`
//! / `batch_get` / `sources`. Each is a read-only [`Tool`] that parses its JSON input into the
//! matching `flux-datasource` request type, calls the backend, and renders a compact text result.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use flux_core::{Error, Result};
use flux_datasource::{
    BatchGetInput, GetInput, ListInput, Match, Record, RelationInput, SearchInput, SourceSummary,
};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, ToolSpec};

use super::harness_history::{record_is_from, HarnessSelector, HARNESS_SOURCE};
use super::{DatasourceBackend, HarnessHistory};

/// How far `search` over-fetches when a `harness` selector has to be applied after the backend.
///
/// The backend filters natively on `source` and `entity` only, and `harness` is a *within-source*
/// distinction (that is precisely why the design gives it its own field rather than reusing
/// `source:`). So a filtered search asks for more than it needs and truncates. The factor is small
/// because the query is already pinned to the `harness` source, leaving at most one dilution factor
/// per enabled harness rather than per indexed record.
const HARNESS_OVERFETCH: usize = 8;

/// The six datasource retrieval ops over `backend`, as a tool vec (the form a surface registers into
/// an agent/app registry — e.g. `App::with_tools`).
///
/// Harness history is **off**: this is [`datasource_tools_with_history`] under
/// [`HarnessHistory::disabled`], not a parallel code path, so "the default advertises no `harness`
/// field" is true by construction rather than by two declarations being kept in step.
pub fn datasource_tools(backend: Arc<dyn DatasourceBackend>) -> Vec<Arc<dyn Tool>> {
    datasource_tools_with_history(backend, &HarnessHistory::disabled())
}

/// The six datasource retrieval ops, with `search` carrying a `harness` selector for the harnesses
/// `history` enables (C-215).
///
/// When `history` is disabled the returned `search` is identical to [`datasource_tools`]': no
/// `harness` property in its schema, and its permission subjects unchanged. When it is enabled,
/// `search` advertises the selector and every invocation carries per-harness
/// `datasource:harness.<id>` subjects so a policy can allow `flux` and deny the rest.
///
/// Registering these ops does **not** read a transcript. Ingest is the separate, host-called
/// [`ingest_harness_history`](super::ingest_harness_history); this pack only searches whatever is
/// already in the index, which is why `search` keeps its read-only, `Datasource`-access declaration.
pub fn datasource_tools_with_history(
    backend: Arc<dyn DatasourceBackend>,
    history: &HarnessHistory,
) -> Vec<Arc<dyn Tool>> {
    let selector = history.is_enabled().then(|| history.selector());
    vec![
        Arc::new(SearchOp {
            backend: backend.clone(),
            harness: selector,
        }) as Arc<dyn Tool>,
        Arc::new(GetOp(backend.clone())),
        Arc::new(ListOp(backend.clone())),
        Arc::new(RelationOp(backend.clone())),
        Arc::new(BatchGetOp(backend.clone())),
        Arc::new(SourcesOp(backend)),
    ]
}

/// Register all six datasource retrieval ops over `backend` into `registry`.
pub fn register_datasource_ops(registry: &mut ToolRegistry, backend: Arc<dyn DatasourceBackend>) {
    try_register_datasource_ops(registry, backend)
        .expect("flux datasource operation pack registration failed");
}

/// Fallibly register datasource operations with an auditable source label.
pub fn try_register_datasource_ops(
    registry: &mut ToolRegistry,
    backend: Arc<dyn DatasourceBackend>,
) -> Result<()> {
    try_register_datasource_ops_with_history(registry, backend, &HarnessHistory::disabled())
}

/// Fallibly register the datasource pack with harness history enabled per `history` (C-215).
pub fn try_register_datasource_ops_with_history(
    registry: &mut ToolRegistry,
    backend: Arc<dyn DatasourceBackend>,
    history: &HarnessHistory,
) -> Result<()> {
    registry.try_register_all_from(
        "flux-capabilities datasource pack",
        datasource_tools_with_history(backend, history),
    )
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

fn datasource_subjects(params: &Value) -> Vec<String> {
    let source = params
        .get("source")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("*");
    let entity = params
        .get("entity")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("*");
    vec![format!("datasource:{source}/{entity}")]
}

/// `search` — keyword search over the indexed datasource.
///
/// `harness` is `Some` only when the host opted into harness history (C-215); it is what makes the
/// `harness` selector, and its per-harness permission subjects, part of this op's declaration at all.
struct SearchOp {
    backend: Arc<dyn DatasourceBackend>,
    harness: Option<HarnessSelector>,
}

#[async_trait]
impl Tool for SearchOp {
    fn spec(&self) -> ToolSpec {
        let mut properties = json!({
            "query": {"type": "string"},
            "source": {"type": "string", "description": "Restrict to one source (e.g. \"local\", \"gitlab\")"},
            "entity": {"type": "string", "description": "Restrict to one entity type (e.g. \"file.document\")"},
            "limit": {"type": "integer", "description": "Max results (default 5)"}
        });
        let mut description = "Search the indexed knowledge datasource (local docs + integration \
             records) by keyword."
            .to_string();
        if let Some(selector) = &self.harness {
            properties["harness"] = selector.schema_property();
            description.push_str(
                " Also indexes local coding-harness conversation history; pass `harness` to \
                 restrict to one of them.",
            );
        }
        ToolSpec::read_only(
            "search",
            description,
            json!({
                "type": "object",
                "properties": properties,
                "required": ["query"]
            }),
        )
        .with_access(vec![AccessKind::Datasource])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        let Some(selector) = &self.harness else {
            return datasource_subjects(params);
        };
        let mut subjects = datasource_subjects(params);
        subjects.extend(selector.subjects(params));
        subjects
    }

    async fn execute(&self, _ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        // Resolve the selector *before* parsing: `SearchInput` ignores unknown fields, so a typo'd
        // or not-enabled harness would otherwise be dropped into an unfiltered all-harness search.
        let kind = match &self.harness {
            Some(selector) => selector.resolve(&params)?,
            None => None,
        };
        let mut input: SearchInput = parse("search", params)?;
        let limit = input.limit;
        if kind.is_some() {
            // Pin the source natively (free and exact) and over-fetch only to cover the
            // within-source dilution the backend cannot filter on.
            input
                .source
                .get_or_insert_with(|| HARNESS_SOURCE.to_string());
            input.limit = limit.map(|n| n.saturating_mul(HARNESS_OVERFETCH));
        }
        let mut hits = self.backend.search(&input)?;
        if let Some(kind) = kind {
            hits.retain(|hit| record_is_from(&hit.record, kind));
            if let Some(limit) = limit {
                hits.truncate(limit);
            }
        }
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
        .with_access(vec![AccessKind::Datasource])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        datasource_subjects(params)
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
        .with_access(vec![AccessKind::Datasource])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        datasource_subjects(params)
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
        .with_access(vec![AccessKind::Datasource])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        datasource_subjects(params)
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
        .with_access(vec![AccessKind::Datasource])
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        datasource_subjects(params)
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

/// `source (N records; entities: e1, e2)` — for `sources`.
fn render_source(s: &SourceSummary) -> String {
    format!(
        "{} ({} record{}; entities: {})",
        s.source,
        s.count,
        if s.count == 1 { "" } else { "s" },
        s.entities.join(", ")
    )
}

/// `sources` — enumerate the distinct sources in the index: per source, its entity types and record
/// count. Answers "what knowledge do I have?" one call ahead of the other five ops, all of which
/// require a known source key.
struct SourcesOp(Arc<dyn DatasourceBackend>);

#[async_trait]
impl Tool for SourcesOp {
    fn spec(&self) -> ToolSpec {
        ToolSpec::read_only(
            "sources",
            "Enumerate the knowledge datasources in the index: per source, its entity types and \
             record count. Call this before search/get/list/relation/batch_get to learn which \
             source keys exist.",
            json!({
                "type": "object",
                "properties": {}
            }),
        )
        .with_access(vec![AccessKind::Datasource])
    }

    fn permission_subjects(&self, _params: &Value) -> Vec<String> {
        vec!["datasource:*/*".to_string()]
    }

    async fn execute(&self, _ctx: &ToolContext, _params: Value) -> Result<ToolResult> {
        let sources = self.0.sources()?;
        if sources.is_empty() {
            return Ok(ToolResult::ok("no sources"));
        }
        Ok(ToolResult::ok(
            sources
                .iter()
                .map(render_source)
                .collect::<Vec<_>>()
                .join("\n"),
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
        let search = SearchOp {
            backend: b.clone(),
            harness: None,
        };
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

    /// D-114: `sources` reports both the auto-indexed `local` source (the workspace doc walk) and a
    /// program-`datasource`-declared source (ingested under its own name), after ingestion — the
    /// scenario the acceptance criteria names.
    #[tokio::test]
    async fn sources_op_reports_local_and_a_declared_source() {
        let backend = Arc::new(MemoryBackend::new());
        let dyn_backend: Arc<dyn DatasourceBackend> = backend.clone();
        // The auto-index path (`build_doc_index`) ingests under "local".
        crate::datasource::ingest_markdown(
            &*dyn_backend,
            "local",
            &[("README.md".to_string(), "workspace readme".to_string())],
        )
        .unwrap();
        // A program `datasource` declaration ingests under its own name (`build_datasources`).
        crate::datasource::ingest_markdown(
            &*dyn_backend,
            "docs",
            &[(
                "guide.md".to_string(),
                "declared datasource guide".to_string(),
            )],
        )
        .unwrap();

        let sources = SourcesOp(dyn_backend);
        let r = sources.execute(&ctx(), json!({})).await.unwrap();
        assert!(!r.is_error);
        assert!(r.content.contains("local"), "got: {}", r.content);
        assert!(r.content.contains("docs"), "got: {}", r.content);
        assert!(
            r.content.contains("file.document"),
            "reports the entity type: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn sources_op_reports_no_sources_on_an_empty_index() {
        let sources = SourcesOp(Arc::new(MemoryBackend::new()));
        let r = sources.execute(&ctx(), json!({})).await.unwrap();
        assert!(!r.is_error);
        assert_eq!(r.content, "no sources");
    }

    #[test]
    fn sources_is_a_sixth_ungrouped_op() {
        let tools = datasource_tools(Arc::new(MemoryBackend::new()));
        assert_eq!(tools.len(), 6);
        let names: Vec<String> = tools.iter().map(|t| t.spec().name).collect();
        assert!(names.contains(&"sources".to_string()), "names: {names:?}");
        let spec = tools
            .iter()
            .find(|t| t.spec().name == "sources")
            .unwrap()
            .spec();
        assert!(spec.group.is_none(), "ungrouped like the other five");
        assert!(spec.has_effect(flux_spec::Effect::Read));
    }
}
