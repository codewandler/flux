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
    FilterKey, FilterType, FilterValue, Filters, LiveEntity, LiveSchema, Page, PageRequest,
    Reference, Row,
};
use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_runtime::{
    AuthorityRequirement, OperationPlacement, Tool, ToolContext, ToolRegistry, ToolResult,
};
use flux_secret::endpoint::EndpointRef;
use flux_spec::{AccessKind, Effect, ToolSpec};
use flux_system::port::ExecutionSystem;
use serde::Deserialize;
use serde_json::{json, Map, Value};

/// Where a declared access is reachable from — the third leg of the host/endpoint/datasource
/// composition (C-716).
///
/// A host binding says *where* a connection is made from, an endpoint says *what* is connected to,
/// and this is how the datasource carries the endpoint's answer to the first question into its own
/// contract. `postgres://db.default.svc.cluster.local:5432` is meaningless on a laptop and exactly
/// right inside the cluster; nothing in a URL distinguishes the two, so the binding id does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub enum LiveLocality {
    /// Reachable from wherever the caller already is. The default, and the behaviour of every
    /// declaration written before this existed: no host is required and nothing is admitted.
    #[default]
    Anywhere,
    /// Reachable only from the named `[[host]]` binding — [`EndpointRef::host`].
    Host(String),
}

impl LiveLocality {
    /// The `[[host]]` binding id this locality requires, or `None` for [`Anywhere`](Self::Anywhere).
    pub fn host(&self) -> Option<&str> {
        match self {
            Self::Anywhere => None,
            Self::Host(host) => Some(host.as_str()),
        }
    }
}

/// External guarded resource used by a live backend in addition to its datasource identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LiveAccess {
    /// HTTP or other URL-addressed network egress.
    Network {
        /// Exact policy subject, normally a guarded origin or URL.
        subject: String,
        /// Where the connection has to be made from.
        from: LiveLocality,
    },
    /// Raw or driver-owned connection target.
    Connection {
        /// Exact policy subject, such as `tcp:db.example:5432`.
        subject: String,
        /// Where the connection has to be made from.
        from: LiveLocality,
    },
}

impl LiveAccess {
    /// URL-addressed egress reachable from wherever the caller is. Pair with
    /// [`from_host`](Self::from_host) or [`from_endpoint`](Self::from_endpoint) to bind it to a host.
    pub fn network(subject: impl Into<String>) -> Self {
        Self::Network {
            subject: subject.into(),
            from: LiveLocality::Anywhere,
        }
    }

    /// A driver-owned connection target reachable from wherever the caller is.
    pub fn connection(subject: impl Into<String>) -> Self {
        Self::Connection {
            subject: subject.into(),
            from: LiveLocality::Anywhere,
        }
    }

    /// Bind this access to a `[[host]]` binding: the connection is made from there or not at all.
    pub fn from_host(self, host: impl Into<String>) -> Self {
        self.with_locality(LiveLocality::Host(host.into()))
    }

    /// Take this access's locality from the endpoint record it describes.
    ///
    /// This is the joint: `host` is the endpoint's own statement of where it is reachable from
    /// (C-709), and copying it here rather than re-deriving one keeps the datasource from
    /// disagreeing with the record it reads.
    pub fn from_endpoint(self, endpoint: &EndpointRef) -> Self {
        self.with_locality(match &endpoint.host {
            Some(host) => LiveLocality::Host(host.clone()),
            None => LiveLocality::Anywhere,
        })
    }

    fn with_locality(self, locality: LiveLocality) -> Self {
        match self {
            Self::Network { subject, .. } => Self::Network {
                subject,
                from: locality,
            },
            Self::Connection { subject, .. } => Self::Connection {
                subject,
                from: locality,
            },
        }
    }

    /// Concrete policy subject carried by this declaration.
    pub fn subject(&self) -> &str {
        match self {
            Self::Network { subject, .. } | Self::Connection { subject, .. } => subject,
        }
    }

    /// Where this access has to be connected from.
    pub fn locality(&self) -> &LiveLocality {
        match self {
            Self::Network { from, .. } | Self::Connection { from, .. } => from,
        }
    }

    /// The `[[host]]` binding this access requires, or `None` when it needs no particular vantage.
    pub fn required_host(&self) -> Option<&str> {
        self.locality().host()
    }

    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::Network { .. } => "network",
            Self::Connection { .. } => "connection",
        }
    }
}

/// The single `[[host]]` binding a declared access set requires, or `None` when every access is
/// reachable from wherever the caller is.
///
/// Single because a session selects one substrate: [`validate_live_contract`] refuses a declaration
/// naming two, so there is exactly one answer to admit against by the time this is asked.
pub fn declared_host(access: &[LiveAccess]) -> Option<&str> {
    access.iter().find_map(LiveAccess::required_host)
}

/// Where a live backend's operations may run, derived from what its access declares.
///
/// A datasource that needs no particular vantage point stays [`NativeSystemOnly`] exactly as it was.
/// A host-bound one is *not* native-only: hiding it under a selection would refuse it without ever
/// naming the host it needs, and the whole point of the declaration is that the operator is told.
///
/// [`NativeSystemOnly`]: OperationPlacement::NativeSystemOnly
pub(super) fn access_placement(access: &[LiveAccess]) -> OperationPlacement {
    match declared_host(access) {
        Some(_) => OperationPlacement::SelectedExecutionSystem,
        None => OperationPlacement::NativeSystemOnly,
    }
}

/// Admit one live-datasource invocation against the locality its declared access requires.
///
/// This is the admission decision the story asks for: it runs in the generated operation, before
/// the backend is entered, so an endpoint that is only reachable from inside a cluster refuses by
/// *declaration* rather than as a connection timeout several layers down. The refusal names both
/// sides — the host the endpoint needs and the host the session actually selected — because
/// "connection refused" is not something an operator can act on and "select `prod-cluster`" is.
pub fn admit_live_locality(ctx: &ToolContext, domain: &str, access: &[LiveAccess]) -> Result<()> {
    admit_locality(ctx, &format!("live datasource `{domain}`"), access)
}

/// [`admit_live_locality`] with the diagnostic's subject supplied, so the work board says
/// ``work board `sprint` `` where the datasource says ``live datasource `tickets` ``.
pub(super) fn admit_locality(ctx: &ToolContext, what: &str, access: &[LiveAccess]) -> Result<()> {
    let Some(required) = declared_host(access) else {
        return Ok(());
    };
    match ctx.execution_binding() {
        Some(selected) if selected == required => Ok(()),
        Some(selected) => Err(Error::Other(format!(
            "{what}: its endpoint is reachable only from host `{required}`, but this session \
             selected host `{selected}`"
        ))),
        None => Err(Error::Other(format!(
            "{what}: its endpoint is reachable only from host `{required}`, but this session \
             selected no host — select it with `--host {required}`"
        ))),
    }
}

/// The guarded substrate a live datasource backend makes its connection through.
///
/// This is how a backend honours the trait's standing requirement to do real IO through flux's
/// guarded surfaces *and* the locality its [`LiveAccess`] declares, in one call:
///
/// * a host-bound access hands back the **selected** substrate, so the connection is opened from
///   the machine the binding names, with that machine's name resolution and private-network scope
///   rather than the coordinator's;
/// * an access with no declared host hands back [`ToolContext::execution_system`] — the same
///   substrate every port-aware operation already uses, which is the native system for an
///   unselected run.
///
/// It re-runs [`admit_live_locality`] rather than trusting the caller to have done so: the whole
/// value of the declaration is that a mismatch cannot end with a connection from the coordinator,
/// and a backend calling this directly (from a helper, a retry, a second entity) must get the same
/// refusal the generated operation gives.
pub fn live_connection_system(
    ctx: &ToolContext,
    domain: &str,
    access: &[LiveAccess],
) -> Result<Arc<dyn ExecutionSystem>> {
    connection_system(ctx, &format!("live datasource `{domain}`"), access)
}

/// [`live_connection_system`] with the diagnostic's subject supplied.
pub(super) fn connection_system(
    ctx: &ToolContext,
    what: &str,
    access: &[LiveAccess],
) -> Result<Arc<dyn ExecutionSystem>> {
    admit_locality(ctx, what, access)?;
    match declared_host(access) {
        // Admission passed, so a binding was selected. A binding that installs no substrate
        // override (a named `local` one) still resolves to the native system here, which is the
        // machine that binding names.
        Some(_) => Ok(ctx
            .selected_execution_system()
            .unwrap_or_else(|| ctx.execution_system())),
        None => Ok(ctx.execution_system()),
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

/// Evidence-gated catalog metadata returned with one registered live datasource.
///
/// Hosts install [`group`](Self::group) beside the generated tools and add
/// [`ambient_signal`](Self::ambient_signal) whenever the configured backend is present. Keeping
/// those values together prevents a registration from accidentally advertising tools without the
/// evidence that makes the integration available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveDatasourceSurface {
    /// Per-domain group containing exactly the generated list/get operations.
    pub group: ToolGroup,
    /// Ambient project signal emitted because this datasource is configured.
    pub ambient_signal: String,
}

/// Build the uniform `<domain>.list` and `<domain>.get` operations for one live backend.
///
/// The backend contract is snapshotted and validated once so the schemas and external authority
/// advertised at registration are the same entity/filter/resource vocabulary used to route calls.
pub fn live_datasource_tools(
    domain: &str,
    backend: Arc<dyn LiveDatasource>,
) -> Result<Vec<Arc<dyn Tool>>> {
    Ok(live_tools(live_projection(domain, backend)?))
}

/// Snapshot and validate one backend's contract exactly once.
///
/// `access()` is called a single time here and never again: the placement, the advertised authority
/// and the admission decision all read the same snapshot, so a backend cannot widen its own
/// locality after registration any more than it can widen its own authority.
fn live_projection(domain: &str, backend: Arc<dyn LiveDatasource>) -> Result<Arc<LiveProjection>> {
    let schema = backend.schema();
    let access = backend.access();
    validate_live_contract(domain, &schema, &access)?;
    Ok(Arc::new(LiveProjection {
        domain: domain.to_string(),
        schema,
        access,
        backend,
    }))
}

fn live_tools(projection: Arc<LiveProjection>) -> Vec<Arc<dyn Tool>> {
    let list = LiveListOp {
        spec: list_spec(&projection.domain, &projection.schema, &projection.access),
        projection: projection.clone(),
    };
    let get = LiveGetOp {
        spec: get_spec(&projection.domain, &projection.schema, &projection.access),
        projection,
    };
    vec![Arc::new(list), Arc::new(get)]
}

/// Atomically install exactly the list/get pair for one live datasource domain.
///
/// Both tools share an auditable source label. [`ToolRegistry::try_register_all_from`] assembles on
/// a clone, so a collision or invalid declaration leaves the caller's registry unchanged.
pub fn try_register_live_datasource(
    registry: &mut ToolRegistry,
    domain: &str,
    backend: Arc<dyn LiveDatasource>,
) -> Result<LiveDatasourceSurface> {
    let projection = live_projection(domain, backend)?;
    let placement = access_placement(&projection.access);
    registry.try_register_all_from_with_placement(
        live_source(domain),
        live_tools(projection),
        placement,
    )?;
    Ok(live_surface(domain))
}

fn live_source(domain: &str) -> String {
    format!("flux-capabilities live datasource `{domain}`")
}

struct LiveProjection {
    domain: String,
    schema: LiveSchema,
    access: Vec<LiveAccess>,
    backend: Arc<dyn LiveDatasource>,
}

struct LiveInvocationContract {
    permission_subject: String,
    requirements: Vec<AuthorityRequirement>,
}

impl LiveProjection {
    fn entity(&self, operation: &str, entity: &str) -> Result<&LiveEntity> {
        self.schema
            .entities
            .iter()
            .find(|declared| declared.entity == entity)
            .ok_or_else(|| {
                Error::Other(format!(
                    "{operation}.entity: unknown entity `{entity}` for live datasource `{}`",
                    self.domain
                ))
            })
    }

    fn invocation_contract(&self, params: &Value) -> LiveInvocationContract {
        let entity = params
            .get("entity")
            .and_then(Value::as_str)
            .filter(|entity| !entity.is_empty())
            .unwrap_or("*");
        let permission_subject = format!("{}/{entity}", self.domain);
        let mut requirements = vec![AuthorityRequirement::datasource_read(
            permission_subject.clone(),
        )];
        requirements.extend(self.access.iter().map(|access| match access {
            LiveAccess::Network { subject, .. } => AuthorityRequirement::network_fetch(subject),
            LiveAccess::Connection { subject, .. } => {
                AuthorityRequirement::connection_dial(subject)
            }
        }));
        LiveInvocationContract {
            permission_subject,
            requirements,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveListInput {
    entity: String,
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    filters: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
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

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        vec![
            self.projection
                .invocation_contract(params)
                .permission_subject,
        ]
    }

    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        Ok(self.projection.invocation_contract(params).requirements)
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let operation = &self.spec.name;
        admit_live_locality(ctx, &self.projection.domain, &self.projection.access)?;
        let input: LiveListInput = parse(operation, params)?;
        let entity = self.projection.entity(operation, &input.entity)?;
        let filters = normalize_filters(
            operation,
            &format!("entity `{}`", entity.entity),
            &entity.filters,
            input.filters,
        )?;
        let page = PageRequest {
            cursor: input.page,
            limit: normalize_limit(operation, entity.default_page, entity.max_page, input.limit)?,
        };
        let result = self
            .projection
            .backend
            .list(ctx, &input.entity, page, &filters)
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

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        vec![
            self.projection
                .invocation_contract(params)
                .permission_subject,
        ]
    }

    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        Ok(self.projection.invocation_contract(params).requirements)
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let operation = &self.spec.name;
        admit_live_locality(ctx, &self.projection.domain, &self.projection.access)?;
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

pub(super) fn parse<T: serde::de::DeserializeOwned>(operation: &str, params: Value) -> Result<T> {
    serde_json::from_value(params)
        .map_err(|error| Error::Other(format!("{operation}: bad input: {error}")))
}

/// Clamp a caller-supplied page limit against a declared `(default, max)` pair.
///
/// Shared with the board port so both surfaces treat "omitted" and "over the ceiling" identically.
pub(super) fn normalize_limit(
    operation: &str,
    default_page: usize,
    max_page: usize,
    requested: Option<usize>,
) -> Result<usize> {
    match requested {
        Some(0) => Err(Error::Other(format!(
            "{operation}.limit: must be greater than zero"
        ))),
        Some(limit) => Ok(limit.min(max_page)),
        None => Ok(default_page),
    }
}

/// Validate and normalize caller filters against a declared filter set.
///
/// `scope` names the thing the filters belong to for error messages (`entity \`ticket\``, `board
/// \`board\``). Shared with the board port: a board filters items the same way a live datasource
/// filters rows, so the vocabulary and the diagnostics stay single-sourced.
pub(super) fn normalize_filters(
    operation: &str,
    scope: &str,
    declared: &[FilterKey],
    supplied: Map<String, Value>,
) -> Result<Filters> {
    for name in supplied.keys() {
        if !declared.iter().any(|filter| filter.name == *name) {
            return Err(Error::Other(format!(
                "{operation}.filters.{name}: unknown filter for {scope}"
            )));
        }
    }

    let mut normalized = Filters::new();
    for filter in declared {
        let Some(value) = supplied.get(&filter.name) else {
            if filter.required {
                return Err(Error::Other(format!(
                    "{operation}.filters.{}: required filter is missing",
                    filter.name
                )));
            }
            continue;
        };
        normalized.insert(
            filter.name.clone(),
            normalize_filter_value(operation, filter, value)?,
        );
    }
    Ok(normalized)
}

pub(super) fn normalize_filter_value(
    operation: &str,
    filter: &FilterKey,
    value: &Value,
) -> Result<FilterValue> {
    let path = format!("{operation}.filters.{}", filter.name);
    match &filter.ty {
        FilterType::String => value
            .as_str()
            .map(|value| FilterValue::String(value.to_string()))
            .ok_or_else(|| Error::Other(format!("{path}: expected string"))),
        FilterType::Int => value
            .as_i64()
            .map(FilterValue::Int)
            .ok_or_else(|| Error::Other(format!("{path}: expected integer"))),
        FilterType::Bool => value
            .as_bool()
            .map(FilterValue::Bool)
            .ok_or_else(|| Error::Other(format!("{path}: expected boolean"))),
        FilterType::Enum(values) => {
            let Some(value) = value.as_str() else {
                return Err(Error::Other(format!("{path}: expected string")));
            };
            if !values.iter().any(|allowed| allowed == value) {
                let expected = values
                    .iter()
                    .map(|allowed| format!("`{allowed}`"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(Error::Other(format!(
                    "{path}: expected one of {expected}, got `{value}`"
                )));
            }
            Ok(FilterValue::String(value.to_string()))
        }
    }
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

fn list_spec(domain: &str, schema: &LiveSchema, access: &[LiveAccess]) -> ToolSpec {
    let variants = schema
        .entities
        .iter()
        .map(list_entity_schema)
        .collect::<Vec<_>>();
    live_spec(
        ToolSpec::read_only(
            format!("{domain}.list"),
            format!("List one page from the `{domain}` live datasource."),
            json!({"type": "object", "oneOf": variants}),
        ),
        domain,
        access,
    )
}

fn get_spec(domain: &str, schema: &LiveSchema, access: &[LiveAccess]) -> ToolSpec {
    let entities = schema
        .entities
        .iter()
        .map(|entity| entity.entity.clone())
        .collect::<Vec<_>>();
    live_spec(
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
        ),
        domain,
        access,
    )
}

fn live_spec(spec: ToolSpec, domain: &str, access: &[LiveAccess]) -> ToolSpec {
    let mut access_kinds = vec![AccessKind::Datasource];
    for kind in access.iter().map(|access| match access {
        LiveAccess::Network { .. } => AccessKind::Network,
        LiveAccess::Connection { .. } => AccessKind::Connection,
    }) {
        if !access_kinds.contains(&kind) {
            access_kinds.push(kind);
        }
    }
    let effects = if access.is_empty() {
        vec![Effect::Read]
    } else {
        vec![Effect::Read, Effect::Network]
    };
    spec.with_effects(effects)
        .with_access(access_kinds)
        .with_group(domain)
}

fn live_surface(domain: &str) -> LiveDatasourceSurface {
    let list = format!("{domain}.list");
    let get = format!("{domain}.get");
    LiveDatasourceSurface {
        group: ToolGroup {
            name: domain.to_string(),
            description: format!("Live datasource operations for `{domain}`."),
            tools: vec![list, get],
            surface_when: vec![SignalMatch {
                kind: KIND_SIGNAL.to_string(),
                signal: Some(domain.to_string()),
            }],
        },
        ambient_signal: domain.to_string(),
    }
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

pub(super) fn filter_schema(filter: &FilterKey) -> Value {
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
        // Keyed on kind + subject rather than the whole value, so one subject declared twice with
        // two different localities is a duplicate rather than two contradictory declarations.
        if !resources.insert((declared.kind(), subject)) {
            return Err(Error::Other(format!(
                "live datasource `{domain}` declares duplicate {} authority `{subject}`",
                declared.kind()
            )));
        }
    }
    validate_locality(&format!("live datasource `{domain}`"), access)
}

/// Validate the locality half of one declared access set (C-716).
///
/// Shared by the live-datasource and work-board contracts — `what` is the already-formatted subject
/// of the diagnostic (``work board `sprint` ``) — because both declare the same [`LiveAccess`] and
/// an unsatisfiable declaration must be refused identically on either surface.
pub(super) fn validate_locality(what: &str, access: &[LiveAccess]) -> Result<()> {
    let mut required: Option<&str> = None;
    for declared in access {
        let Some(host) = declared.required_host() else {
            continue;
        };
        if host.trim().is_empty() || host.trim() != host {
            return Err(Error::Other(format!(
                "{what} {} authority `{}` names a blank or whitespace-padded host binding",
                declared.kind(),
                declared.subject()
            )));
        }
        // One session selects one substrate, so a declaration naming two hosts can never be
        // satisfied. Refusing it at registration is the same fail-closed direction as every other
        // contract error here: an unsatisfiable backend must not advertise operations at all.
        match required {
            Some(first) if first != host => {
                return Err(Error::Other(format!(
                    "{what} declares access from two hosts (`{first}`, `{host}`); one session \
                     selects one host"
                )));
            }
            _ => required = Some(host),
        }
    }
    Ok(())
}

pub(super) fn valid_domain(domain: &str) -> bool {
    let mut chars = domain.chars();
    matches!(chars.next(), Some('a'..='z'))
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}
