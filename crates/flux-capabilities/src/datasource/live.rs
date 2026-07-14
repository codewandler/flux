//! Async live-system datasource contract.
//!
//! This trait sits beside [`super::DatasourceBackend`]: the existing backend owns a local indexed
//! snapshot, while a live backend reads a remote system of record on demand. Implementations receive
//! the guarded [`ToolContext`] and declare their exact external resource families up front.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_datasource::live::{
    FilterKey, FilterType, Filters, LiveEntity, LiveSchema, Page, PageRequest, Reference, Row,
};
use flux_runtime::{Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// External guarded resource used by a live backend in addition to its datasource identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiveAccess {
    /// HTTP or other URL-addressed network egress.
    Network {
        /// Exact policy subject, normally a guarded origin or URL.
        subject: String,
    },
    /// Raw or driver-owned connection target.
    Connection {
        /// Exact policy subject, such as `tcp:db.example:5432`.
        subject: String,
    },
}

impl LiveAccess {
    /// Concrete policy subject carried by this declaration.
    pub fn subject(&self) -> &str {
        match self {
            Self::Network { subject } | Self::Connection { subject } => subject,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Network { .. } => "network",
            Self::Connection { .. } => "connection",
        }
    }
}

/// A live, async system-of-record backend.
///
/// Implementations must perform real IO through flux's guarded host surfaces. Receiving
/// [`ToolContext`] keeps filesystem/process access on the canonical runtime context; URL and
/// connection clients must still use the corresponding DNS/private-network or host-capability
/// guards described by their [`LiveAccess`] declaration.
#[async_trait]
pub trait LiveDatasource: Send + Sync {
    /// Model-facing entities, filter contracts, and page bounds.
    fn schema(&self) -> LiveSchema;

    /// Concrete external resources needed by `list` and `get`. Empty means an in-process backend.
    fn access(&self) -> Vec<LiveAccess> {
        Vec::new()
    }

    /// Fetch one page of an entity using already-validated filters and paging.
    async fn list(
        &self,
        ctx: &ToolContext,
        entity: &str,
        page: PageRequest,
        filters: &Filters,
    ) -> Result<Page<Row>>;

    /// Resolve one row by stable entity/id, re-entering host-owned authentication and connection
    /// state rather than consuming a model-held capability.
    async fn get(&self, ctx: &ToolContext, entity: &str, id: &str) -> Result<Option<Row>>;
}

/// Build the uniform `<domain>.list` and `<domain>.get` operations for one live backend.
///
/// The backend contract is snapshotted and validated once so the schemas advertised at
/// registration are the same entity/filter vocabulary used to route calls. Runtime enforcement of
/// filter contracts and page bounds is the next projection phase; this layer performs only the
/// structural conversion needed by the typed backend.
pub fn live_datasource_tools(
    domain: &str,
    backend: Arc<dyn LiveDatasource>,
) -> Result<Vec<Arc<dyn Tool>>> {
    let schema = backend.schema();
    validate_live_contract(domain, &schema, &backend.access())?;

    let projection = Arc::new(LiveProjection {
        domain: domain.to_string(),
        schema,
        backend,
    });
    let list = LiveListOp {
        spec: list_spec(&projection.domain, &projection.schema),
        projection: projection.clone(),
    };
    let get = LiveGetOp {
        spec: get_spec(&projection.domain, &projection.schema),
        projection,
    };
    Ok(vec![Arc::new(list), Arc::new(get)])
}

/// Atomically install exactly the list/get pair for one live datasource domain.
///
/// Both tools share an auditable source label. [`ToolRegistry::try_register_all_from`] assembles on
/// a clone, so a collision or invalid declaration leaves the caller's registry unchanged.
pub fn try_register_live_datasource(
    registry: &mut ToolRegistry,
    domain: &str,
    backend: Arc<dyn LiveDatasource>,
) -> Result<()> {
    let tools = live_datasource_tools(domain, backend)?;
    registry.try_register_all_from(live_source(domain), tools)
}

fn live_source(domain: &str) -> String {
    format!("flux-capabilities live datasource `{domain}`")
}

struct LiveProjection {
    domain: String,
    schema: LiveSchema,
    backend: Arc<dyn LiveDatasource>,
}

impl LiveProjection {
    fn entity(&self, operation: &str, entity: &str) -> Result<&LiveEntity> {
        self.schema
            .entities
            .iter()
            .find(|declared| declared.entity == entity)
            .ok_or_else(|| {
                Error::Other(format!(
                    "{operation}: unknown entity `{entity}` for live datasource `{}`",
                    self.domain
                ))
            })
    }
}

#[derive(Deserialize)]
struct LiveListInput {
    entity: String,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    filters: Filters,
}

#[derive(Deserialize)]
struct LiveGetInput {
    entity: String,
    id: String,
}

struct LiveListOp {
    projection: Arc<LiveProjection>,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for LiveListOp {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let operation = &self.spec.name;
        let input: LiveListInput = parse(operation, params)?;
        let entity = self.projection.entity(operation, &input.entity)?;
        let page = PageRequest {
            cursor: input.page,
            limit: input.limit.unwrap_or(entity.default_page),
        };
        let result = self
            .projection
            .backend
            .list(ctx, &input.entity, page, &input.filters)
            .await?;
        Ok(ToolResult::ok(render_page(&input.entity, &result)))
    }
}

struct LiveGetOp {
    projection: Arc<LiveProjection>,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for LiveGetOp {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let operation = &self.spec.name;
        let input: LiveGetInput = parse(operation, params)?;
        self.projection.entity(operation, &input.entity)?;
        let row = self
            .projection
            .backend
            .get(ctx, &input.entity, &input.id)
            .await?;
        Ok(ToolResult::ok(match row {
            Some(row) => render_full_row(&input.entity, &row),
            None => "not found".to_string(),
        }))
    }
}

fn parse<T: serde::de::DeserializeOwned>(operation: &str, params: Value) -> Result<T> {
    serde_json::from_value(params)
        .map_err(|error| Error::Other(format!("{operation}: bad input: {error}")))
}

fn render_page(entity: &str, page: &Page<Row>) -> String {
    let mut output = if page.rows.is_empty() {
        "no records".to_string()
    } else {
        page.rows
            .iter()
            .map(|row| render_compact_row(entity, row))
            .collect::<Vec<_>>()
            .join("\n")
    };
    if let Some(next) = &page.next {
        output.push_str("\nnext: ");
        output.push_str(next);
    }
    output
}

fn render_compact_row(entity: &str, row: &Row) -> String {
    let mut output = format!("[{entity} {}]", row.id);
    if !row.title.is_empty() {
        output.push(' ');
        output.push_str(&row.title);
    }
    if !row.summary.is_empty() {
        output.push_str(" — ");
        output.push_str(&row.summary);
    }
    output
}

fn render_full_row(entity: &str, row: &Row) -> String {
    let mut output = render_compact_row(entity, row);
    if let Some(reference) = &row.reference {
        output.push_str("\nreference: ");
        match reference {
            Reference::Entity { entity, id } => {
                output.push_str(entity);
                output.push('/');
                output.push_str(id);
            }
            Reference::Url { url } => output.push_str(url),
        }
    }
    output
}

fn list_spec(domain: &str, schema: &LiveSchema) -> ToolSpec {
    let variants = schema
        .entities
        .iter()
        .map(list_entity_schema)
        .collect::<Vec<_>>();
    ToolSpec::read_only(
        format!("{domain}.list"),
        format!("List one page from the `{domain}` live datasource."),
        json!({"type": "object", "oneOf": variants}),
    )
    .with_access(vec![AccessKind::Datasource])
}

fn get_spec(domain: &str, schema: &LiveSchema) -> ToolSpec {
    let entities = schema
        .entities
        .iter()
        .map(|entity| entity.entity.clone())
        .collect::<Vec<_>>();
    ToolSpec::read_only(
        format!("{domain}.get"),
        format!("Fetch one full row from the `{domain}` live datasource."),
        json!({
            "type": "object",
            "properties": {
                "entity": {"type": "string", "enum": entities},
                "id": {"type": "string", "description": "Stable row id resolved by the backend"}
            },
            "required": ["entity", "id"],
            "additionalProperties": false
        }),
    )
    .with_access(vec![AccessKind::Datasource])
}

fn list_entity_schema(entity: &LiveEntity) -> Value {
    let mut properties = Map::new();
    let mut required_filters = Vec::new();
    for filter in &entity.filters {
        properties.insert(filter.name.clone(), filter_schema(filter));
        if filter.required {
            required_filters.push(Value::String(filter.name.clone()));
        }
    }

    let mut filters = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false
    });
    if !required_filters.is_empty() {
        filters["required"] = Value::Array(required_filters);
    }

    let mut required = vec![json!("entity")];
    if entity.filters.iter().any(|filter| filter.required) {
        required.push(json!("filters"));
    }
    let mut schema = json!({
        "title": entity.entity,
        "type": "object",
        "properties": {
            "entity": {"const": entity.entity, "type": "string"},
            "page": {"type": "string", "description": "Opaque continuation cursor"},
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": entity.max_page,
                "default": entity.default_page
            },
            "filters": filters
        },
        "required": required,
        "additionalProperties": false
    });
    if let Some(description) = &entity.description {
        schema["description"] = json!(description);
    }
    schema
}

fn filter_schema(filter: &FilterKey) -> Value {
    let mut schema = match &filter.ty {
        FilterType::String => json!({"type": "string"}),
        FilterType::Int => json!({"type": "integer"}),
        FilterType::Bool => json!({"type": "boolean"}),
        FilterType::Enum(values) => json!({"type": "string", "enum": values}),
    };
    if let Some(description) = &filter.description {
        schema["description"] = json!(description);
    }
    schema
}

/// Validate one domain's static contract before any operation is advertised.
pub fn validate_live_contract(
    domain: &str,
    schema: &LiveSchema,
    access: &[LiveAccess],
) -> Result<()> {
    if !valid_domain(domain) {
        return Err(Error::Other(format!(
            "live datasource domain `{domain}` must match [a-z][a-z0-9_]*"
        )));
    }
    if schema.entities.is_empty() {
        return Err(Error::Other(format!(
            "live datasource `{domain}` declares no entities"
        )));
    }

    let mut entities = HashSet::new();
    for entity in &schema.entities {
        let name = entity.entity.trim();
        if name.is_empty() {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares a blank entity"
            )));
        }
        if name != entity.entity {
            return Err(Error::Other(format!(
                "live datasource `{domain}` entity `{}` has surrounding whitespace",
                entity.entity
            )));
        }
        if !entities.insert(name) {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares duplicate entity `{name}`"
            )));
        }
        if entity.default_page == 0 || entity.max_page == 0 {
            return Err(Error::Other(format!(
                "live datasource `{domain}` entity `{name}` page limits must be greater than zero"
            )));
        }
        if entity.default_page > entity.max_page {
            return Err(Error::Other(format!(
                "live datasource `{domain}` entity `{name}` default_page {} exceeds max_page {}",
                entity.default_page, entity.max_page
            )));
        }

        let mut filters = HashSet::new();
        for filter in &entity.filters {
            let filter_name = filter.name.trim();
            if filter_name.is_empty() || filter_name != filter.name {
                return Err(Error::Other(format!(
                    "live datasource `{domain}` entity `{name}` has an invalid blank/whitespace filter name"
                )));
            }
            if !filters.insert(filter_name) {
                return Err(Error::Other(format!(
                    "live datasource `{domain}` entity `{name}` declares duplicate filter `{filter_name}`"
                )));
            }
            if let flux_datasource::live::FilterType::Enum(values) = &filter.ty {
                if values.is_empty() {
                    return Err(Error::Other(format!(
                        "live datasource `{domain}` entity `{name}` filter `{filter_name}` has an empty enum"
                    )));
                }
                let mut seen = HashSet::new();
                for value in values {
                    let trimmed = value.trim();
                    if trimmed.is_empty() || trimmed != value || !seen.insert(trimmed) {
                        return Err(Error::Other(format!(
                            "live datasource `{domain}` entity `{name}` filter `{filter_name}` has a blank, whitespace-padded, or duplicate enum value"
                        )));
                    }
                }
            }
        }
    }

    let mut resources = HashSet::new();
    for declared in access {
        let subject = declared.subject();
        if subject.trim().is_empty() {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares a blank {} authority subject",
                declared.kind()
            )));
        }
        if subject.trim() != subject {
            return Err(Error::Other(format!(
                "live datasource `{domain}` {} authority subject has surrounding whitespace",
                declared.kind()
            )));
        }
        if !resources.insert(declared) {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares duplicate {} authority `{subject}`",
                declared.kind()
            )));
        }
    }

    Ok(())
}

fn valid_domain(domain: &str) -> bool {
    let mut chars = domain.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}
