//! The write-capable [`WorkBoard`] port and its six generated operations (A-113).
//!
//! This is the sibling of [`super::live::LiveDatasource`], and deliberately the same shape: a
//! backend declares a schema plus its external authority, the contract is snapshotted and validated
//! **once** at registration, and the host then generates uniform operations with stable permission
//! subjects, a per-domain [`ToolGroup`] and an ambient signal, installed atomically on a clone.
//! Everything a reader already knows about `try_register_live_datasource` transfers.
//!
//! What is new is that four of the six operations **write**. That changes two things and nothing
//! else:
//!
//! * **Permission subjects stay concrete.** `board.transition` on `PROJ-42` reports
//!   `board/item/PROJ-42`, never `board/item/*` and never nothing, so a grant scoped to one item
//!   cannot silently move another. AGENTS.md:98 is explicit that an unscoped `Write` either gets
//!   forced to approval or matches a broad `*` grant; [`UNRESOLVED_ID`] is why even a malformed
//!   call reports something concrete. `board.create` has no id yet and reports
//!   `<domain>/item/new` — a *deliberately distinct* subject a policy can grant on its own.
//! * **The state machine is enforced, not advertised.** `transition` validates the edge against
//!   [`flux_datasource::board::validate_transition`]; an illegal edge is an error and **not a
//!   write**. That is what makes a crashed coordinator recoverable: reconciliation can re-derive an
//!   item's position only because the legal state set is closed and every write went through the
//!   check.
//!
//! No intents are declared, for the same reason [`super::live`] declares none: the
//! [`IntentBehavior`](flux_spec::IntentBehavior) vocabulary is filesystem/network/process/browser,
//! and a board mutation is none of those. The honest typed contract is the
//! [`AuthorityRequirement::datasource_write`] this module returns, paired with `Effect::Write`.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_datasource::board::{BoardSchema, Item, ItemDraft, State, EDGE_DIAGRAM};
use flux_datasource::live::{FilterKey, FilterType, Filters, Page, PageRequest, Reference};
use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_runtime::{AuthorityRequirement, Tool, ToolContext, ToolRegistry, ToolResult};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use super::live::{
    filter_schema, normalize_filters, normalize_limit, parse, valid_domain, LiveAccess,
};

/// The single entity a board exposes. Boards are item-shaped by construction, so this is a constant
/// rather than a schema dimension — it is the `<entity>` in the `<domain>/<entity>` subject that
/// [`super::live`] derives per entity.
const ENTITY: &str = "item";

/// The id half of the subject a `create` reports, since the item does not exist yet.
///
/// Distinct from any mutation of an existing item on purpose: an operator can grant "may open new
/// work" without granting "may move existing work".
const NEW_ID: &str = "new";

/// The id half of the subject a mutating call reports when it carries no usable id.
///
/// Such a call fails at input validation — but `permission_subjects` is computed *before* that, and
/// returning `*` or an empty vec there is exactly the gating dodge AGENTS.md:98 forbids. The angle
/// brackets keep it from colliding with a real backend id while staying readable in an approval
/// prompt, and it contains no `*`, so `flux_policy::wildcard_match` can only match it literally.
const UNRESOLVED_ID: &str = "<unresolved>";

/// The `state` filter the host always declares on `list`. Reserved: a backend may not redeclare it.
const STATE_FILTER: &str = "state";

/// A write-capable work board: a typed item state machine behind a swappable implementation.
///
/// Jira is one implementation, a markdown file store is another; the coordinator agent only ever
/// sees the generated operations. Implementations must perform real IO through flux's guarded host
/// surfaces exactly as a [`LiveDatasource`](super::live::LiveDatasource) does — the
/// [`ToolContext`] keeps filesystem and process access on the canonical runtime context, and any
/// URL or connection client must still pass the guards its [`LiveAccess`] declaration names.
///
/// **Every implementation must pass the shared contract suite verbatim**
/// (`crates/flux-capabilities/tests/board_contract/mod.rs`). In particular the three properties the
/// generated operations cannot enforce for you:
///
/// * an illegal edge errors and performs **no write** — the item is byte-identical afterwards;
/// * the `Failed → Ready` retry edge increments [`Item::attempts`], and no other edge touches it;
/// * `claim` is idempotent for the same assignee and conflicts for a different one.
#[async_trait]
pub trait WorkBoard: Send + Sync {
    /// Model-facing filter contract and page bounds.
    fn schema(&self) -> BoardSchema;

    /// Concrete external resources needed by every operation. Empty means an in-process backend.
    fn access(&self) -> Vec<LiveAccess> {
        Vec::new()
    }

    /// Fetch one page of items using already-validated filters and paging.
    async fn list(
        &self,
        ctx: &ToolContext,
        filters: &Filters,
        page: PageRequest,
    ) -> Result<Page<Item>>;

    /// Resolve one item by stable id. `None` means absent, which is not an error.
    async fn get(&self, ctx: &ToolContext, id: &str) -> Result<Option<Item>>;

    /// Open a new item. It starts at [`State::Ready`] with zero attempts and a backend-assigned id.
    async fn create(&self, ctx: &ToolContext, draft: ItemDraft) -> Result<Item>;

    /// Move an item along a legal edge.
    ///
    /// Implementations **must** call [`validate_transition`] against the item's current state
    /// before writing anything, and must increment [`Item::attempts`] exactly when
    /// [`flux_datasource::board::is_retry`] says the edge is a retry.
    async fn transition(&self, ctx: &ToolContext, id: &str, to: State) -> Result<Item>;

    /// Take ownership of an item. Idempotent for the current holder; a conflict for anyone else.
    async fn claim(&self, ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item>;

    /// Append a note to an item. An absent id is an error.
    async fn comment(&self, ctx: &ToolContext, id: &str, text: &str) -> Result<()>;
}

/// Evidence-gated catalog metadata returned with one registered board.
///
/// The sibling of [`LiveDatasourceSurface`](super::live::LiveDatasourceSurface): hosts install
/// [`group`](Self::group) beside the generated tools and add
/// [`ambient_signal`](Self::ambient_signal) whenever the configured backend is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBoardSurface {
    /// Per-domain group containing exactly the six generated operations.
    pub group: ToolGroup,
    /// Ambient project signal emitted because this board is configured.
    pub ambient_signal: String,
}

/// The six operation suffixes a board generates, in catalog order.
const OPERATIONS: [&str; 6] = ["list", "get", "create", "transition", "claim", "comment"];

/// Build the uniform `<domain>.list` / `.get` / `.create` / `.transition` / `.claim` / `.comment`
/// operations for one board backend.
///
/// The backend contract is snapshotted and validated once, so the filters, page bounds and external
/// authority advertised at registration are the same vocabulary used to route calls — a backend
/// cannot widen its own authority after the fact.
pub fn work_board_tools(domain: &str, backend: Arc<dyn WorkBoard>) -> Result<Vec<Arc<dyn Tool>>> {
    let schema = backend.schema();
    let access = backend.access();
    validate_board_contract(domain, &schema, &access)?;

    let projection = Arc::new(BoardProjection {
        domain: domain.to_string(),
        filters: declared_filters(&schema),
        schema,
        access,
        backend,
    });
    Ok(OPERATIONS
        .into_iter()
        .map(|op| -> Arc<dyn Tool> {
            Arc::new(BoardOp {
                spec: spec_for(op, &projection),
                kind: OpKind::of(op),
                projection: projection.clone(),
            })
        })
        .collect())
}

/// Atomically install exactly the six operations for one board domain.
///
/// All six share an auditable source label. [`ToolRegistry::try_register_all_from`] assembles on a
/// clone, so a collision or an invalid declaration leaves the caller's registry unchanged — there
/// is no state in which a board is half-registered and three of its writes are reachable.
pub fn try_register_work_board(
    registry: &mut ToolRegistry,
    domain: &str,
    backend: Arc<dyn WorkBoard>,
) -> Result<WorkBoardSurface> {
    let tools = work_board_tools(domain, backend)?;
    registry.try_register_all_from(board_source(domain), tools)?;
    Ok(board_surface(domain))
}

fn board_source(domain: &str) -> String {
    format!("flux-capabilities work board `{domain}`")
}

fn board_surface(domain: &str) -> WorkBoardSurface {
    WorkBoardSurface {
        group: ToolGroup {
            name: domain.to_string(),
            description: format!("Work board operations for `{domain}`."),
            tools: OPERATIONS
                .into_iter()
                .map(|op| format!("{domain}.{op}"))
                .collect(),
            surface_when: vec![SignalMatch {
                kind: KIND_SIGNAL.to_string(),
                signal: Some(domain.to_string()),
            }],
        },
        ambient_signal: domain.to_string(),
    }
}

/// The full filter set `list` accepts: the reserved `state` filter plus whatever the backend added.
fn declared_filters(schema: &BoardSchema) -> Vec<FilterKey> {
    let mut filters = vec![FilterKey {
        name: STATE_FILTER.to_string(),
        ty: FilterType::Enum(State::ALL.iter().map(|s| s.as_str().to_string()).collect()),
        required: false,
        description: Some("Only items in this state.".to_string()),
    }];
    filters.extend(schema.filters.iter().cloned());
    filters
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

struct BoardProjection {
    domain: String,
    schema: BoardSchema,
    /// The reserved `state` filter plus the backend's own, resolved once at registration.
    filters: Vec<FilterKey>,
    access: Vec<LiveAccess>,
    backend: Arc<dyn WorkBoard>,
}

impl BoardProjection {
    /// The subject one invocation touches. Never `*`, never empty — see [`UNRESOLVED_ID`].
    fn subject(&self, kind: OpKind, params: &Value) -> String {
        match kind {
            OpKind::List => format!("{}/{ENTITY}", self.domain),
            OpKind::Create => format!("{}/{ENTITY}/{NEW_ID}", self.domain),
            _ => {
                let id = params
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .unwrap_or(UNRESOLVED_ID);
                format!("{}/{ENTITY}/{id}", self.domain)
            }
        }
    }

    fn requirements(&self, kind: OpKind, params: &Value) -> Vec<AuthorityRequirement> {
        let subject = self.subject(kind, params);
        let mut requirements = vec![if kind.writes() {
            AuthorityRequirement::datasource_write(subject)
        } else {
            AuthorityRequirement::datasource_read(subject)
        }];
        requirements.extend(self.access.iter().map(|access| match access {
            LiveAccess::Network { subject } => AuthorityRequirement::network_fetch(subject),
            LiveAccess::Connection { subject } => AuthorityRequirement::connection_dial(subject),
        }));
        requirements
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

/// Which of the six an instance is. One `impl Tool` covers all of them because they differ only in
/// their input contract and their one backend call — the subject, authority and spec derivation are
/// shared, which is the whole point of generating them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    List,
    Get,
    Create,
    Transition,
    Claim,
    Comment,
}

impl OpKind {
    fn of(suffix: &str) -> Self {
        match suffix {
            "list" => Self::List,
            "get" => Self::Get,
            "create" => Self::Create,
            "transition" => Self::Transition,
            "claim" => Self::Claim,
            "comment" => Self::Comment,
            other => unreachable!("undeclared board operation `{other}`"),
        }
    }

    /// Whether this operation mutates the board. The four that do carry `Effect::Write`, a
    /// `datasource_write` requirement, and a non-`Low` risk tier.
    fn writes(&self) -> bool {
        !matches!(self, Self::List | Self::Get)
    }
}

struct BoardOp {
    projection: Arc<BoardProjection>,
    kind: OpKind,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for BoardOp {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn permission_subjects(&self, params: &Value) -> Vec<String> {
        vec![self.projection.subject(self.kind, params)]
    }

    fn authority_requirements(
        &self,
        params: &Value,
        _subjects: &[String],
    ) -> Result<Vec<AuthorityRequirement>> {
        Ok(self.projection.requirements(self.kind, params))
    }

    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<ToolResult> {
        let op = &self.spec.name;
        let backend = &self.projection.backend;
        Ok(match self.kind {
            OpKind::List => {
                let input: ListInput = parse(op, params)?;
                let filters = normalize_filters(
                    op,
                    &format!("board `{}`", self.projection.domain),
                    &self.projection.filters,
                    input.filters,
                )?;
                let page = PageRequest {
                    cursor: input.page,
                    limit: normalize_limit(
                        op,
                        self.projection.schema.default_page,
                        self.projection.schema.max_page,
                        input.limit,
                    )?,
                };
                ToolResult::ok(render_page(&backend.list(ctx, &filters, page).await?))
            }
            OpKind::Get => {
                let input: IdInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                ToolResult::ok(match backend.get(ctx, id).await? {
                    Some(item) => render_full(&item),
                    None => "not found".to_string(),
                })
            }
            OpKind::Create => {
                let input: CreateInput = parse(op, params)?;
                let title = require(op, "title", &input.title)?;
                let item = backend
                    .create(
                        ctx,
                        ItemDraft {
                            title: title.to_string(),
                            assignee: input.assignee,
                            depends_on: input.depends_on,
                            repo: input.repo,
                        },
                    )
                    .await?;
                ToolResult::ok(render_full(&item))
            }
            OpKind::Transition => {
                let input: TransitionInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let to = State::parse(&input.to).ok_or_else(|| {
                    Error::Other(format!(
                        "{op}.to: unknown state `{}`; the machine is {EDGE_DIAGRAM}",
                        input.to
                    ))
                })?;
                ToolResult::ok(render_full(&backend.transition(ctx, id, to).await?))
            }
            OpKind::Claim => {
                let input: ClaimInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let assignee = require(op, "assignee", &input.assignee)?;
                ToolResult::ok(render_full(&backend.claim(ctx, id, assignee).await?))
            }
            OpKind::Comment => {
                let input: CommentInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let text = require(op, "text", &input.text)?;
                backend.comment(ctx, id, text).await?;
                ToolResult::ok(format!("commented on {id}"))
            }
        })
    }
}

/// Reject a blank required string with the operation-qualified path, before the backend is entered.
fn require<'a>(operation: &str, field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::Other(format!(
            "{operation}.{field}: must not be blank"
        )));
    }
    Ok(trimmed)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInput {
    #[serde(default)]
    page: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    filters: Map<String, Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdInput {
    #[serde(default)]
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateInput {
    #[serde(default)]
    title: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    repo: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransitionInput {
    #[serde(default)]
    id: String,
    #[serde(default)]
    to: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimInput {
    #[serde(default)]
    id: String,
    #[serde(default)]
    assignee: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentInput {
    #[serde(default)]
    id: String,
    #[serde(default)]
    text: String,
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render_page(page: &Page<Item>) -> String {
    let mut output = if page.rows.is_empty() {
        "no items".to_string()
    } else {
        page.rows
            .iter()
            .map(render_compact)
            .collect::<Vec<_>>()
            .join("\n")
    };
    if let Some(next) = &page.next {
        output.push_str("\nnext: ");
        output.push_str(next);
    }
    output
}

fn render_compact(item: &Item) -> String {
    let mut output = format!("[{ENTITY} {}]", item.id);
    if !item.title.is_empty() {
        output.push(' ');
        output.push_str(&item.title);
    }
    output.push_str(&format!(" — {} (attempts {})", item.state, item.attempts));
    if let Some(assignee) = &item.assignee {
        output.push_str(" assignee ");
        output.push_str(assignee);
    }
    output
}

fn render_full(item: &Item) -> String {
    let mut output = render_compact(item);
    if !item.depends_on.is_empty() {
        output.push_str("\ndepends_on: ");
        output.push_str(&item.depends_on.join(", "));
    }
    if let Some(repo) = &item.repo {
        output.push_str("\nrepo: ");
        output.push_str(repo);
    }
    if let Some(runner) = &item.runner {
        output.push_str("\nrunner: ");
        output.push_str(runner);
    }
    if let Some(task_id) = &item.task_id {
        output.push_str("\ntask_id: ");
        output.push_str(task_id);
    }
    for reference in &item.evidence {
        output.push_str("\nevidence: ");
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

// ---------------------------------------------------------------------------
// Specs
// ---------------------------------------------------------------------------

fn spec_for(op: &str, projection: &BoardProjection) -> ToolSpec {
    let domain = &projection.domain;
    let kind = OpKind::of(op);
    let name = format!("{domain}.{op}");
    let (description, schema) = match kind {
        OpKind::List => (
            format!("List one page of items from the `{domain}` work board."),
            list_schema(projection),
        ),
        OpKind::Get => (
            format!("Fetch one full item from the `{domain}` work board."),
            object(
                json!({"id": {"type": "string", "description": "Stable item id"}}),
                &["id"],
            ),
        ),
        OpKind::Create => (
            format!("Open a new `{domain}` item. It starts in `ready`."),
            object(
                json!({
                    "title": {"type": "string", "description": "Short human-facing title"},
                    "assignee": {"type": "string", "description": "Optional initial owner"},
                    "depends_on": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Ids of items that must reach `done` first"
                    },
                    "repo": {"type": "string", "description": "Repository the work belongs to"}
                }),
                &["title"],
            ),
        ),
        OpKind::Transition => (
            format!(
                "Move a `{domain}` item along a legal edge. Illegal edges are refused without \
                 writing. The machine is {EDGE_DIAGRAM}"
            ),
            object(
                json!({
                    "id": {"type": "string", "description": "Stable item id"},
                    "to": {
                        "type": "string",
                        "enum": State::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                        "description": "Target state; must be a legal successor of the current one"
                    }
                }),
                &["id", "to"],
            ),
        ),
        OpKind::Claim => (
            format!(
                "Take ownership of a `{domain}` item. Idempotent for the current holder; a \
                 conflict for anyone else."
            ),
            object(
                json!({
                    "id": {"type": "string", "description": "Stable item id"},
                    "assignee": {"type": "string", "description": "Worker taking the item"}
                }),
                &["id", "assignee"],
            ),
        ),
        OpKind::Comment => (
            format!("Append a note to a `{domain}` item."),
            object(
                json!({
                    "id": {"type": "string", "description": "Stable item id"},
                    "text": {"type": "string", "description": "The note to append"}
                }),
                &["id", "text"],
            ),
        ),
    };

    let spec = ToolSpec::read_only(name, description, schema);
    let mut spec = if kind.writes() {
        // C-191's coherence invariants: a `Write` may keep neither the `Risk::Low` tier nor the
        // `Idempotent` claim. `claim` is the one that is genuinely safe to repeat — for its current
        // holder — which is exactly what `Conditional` is for.
        let mut spec = spec.with_risk(Risk::Medium);
        spec.idempotency = if kind == OpKind::Claim {
            Idempotency::Conditional
        } else {
            Idempotency::NonIdempotent
        };
        spec.with_effects(vec![Effect::Write])
    } else {
        spec.with_effects(vec![Effect::Read])
    };
    if !projection.access.is_empty() {
        spec.effects.push(Effect::Network);
    }
    spec.with_access(access_kinds(&projection.access))
        .with_group(domain)
}

fn access_kinds(access: &[LiveAccess]) -> Vec<AccessKind> {
    let mut kinds = vec![AccessKind::Datasource];
    for kind in access.iter().map(|access| match access {
        LiveAccess::Network { .. } => AccessKind::Network,
        LiveAccess::Connection { .. } => AccessKind::Connection,
    }) {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    kinds
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn list_schema(projection: &BoardProjection) -> Value {
    let mut properties = Map::new();
    let mut required_filters = Vec::new();
    for filter in &projection.filters {
        properties.insert(filter.name.clone(), filter_schema(filter));
        if filter.required {
            required_filters.push(Value::String(filter.name.clone()));
        }
    }
    let has_required = !required_filters.is_empty();
    let mut filters = json!({
        "type": "object",
        "properties": properties,
        "additionalProperties": false
    });
    if has_required {
        filters["required"] = Value::Array(required_filters);
    }

    let mut schema = json!({
        "type": "object",
        "properties": {
            "page": {"type": "string", "description": "Opaque continuation cursor"},
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": projection.schema.max_page,
                "default": projection.schema.default_page
            },
            "filters": filters
        },
        "additionalProperties": false
    });
    // A required filter makes the whole `filters` object required, mirroring how `live` promotes a
    // required filter onto its entity branch.
    if has_required {
        schema["required"] = json!(["filters"]);
    }
    if let Some(description) = &projection.schema.description {
        schema["description"] = json!(description);
    }
    schema
}

// ---------------------------------------------------------------------------
// Contract validation
// ---------------------------------------------------------------------------

/// Validate one board's static contract before any operation is advertised.
///
/// The sibling of [`validate_live_contract`](super::live::validate_live_contract), with the
/// entity dimension collapsed and one extra rule: `state` is the host's filter, so a backend that
/// redeclares it is refused rather than silently shadowed.
pub fn validate_board_contract(
    domain: &str,
    schema: &BoardSchema,
    access: &[LiveAccess],
) -> Result<()> {
    if !valid_domain(domain) {
        return Err(Error::Other(format!(
            "work board domain `{domain}` must match [a-z][a-z0-9_]*"
        )));
    }
    if schema.default_page == 0 || schema.max_page == 0 {
        return Err(Error::Other(format!(
            "work board `{domain}` page limits must be greater than zero"
        )));
    }
    if schema.default_page > schema.max_page {
        return Err(Error::Other(format!(
            "work board `{domain}` default_page {} exceeds max_page {}",
            schema.default_page, schema.max_page
        )));
    }

    let mut names = HashSet::new();
    for filter in &schema.filters {
        let name = filter.name.trim();
        if name.is_empty() || name != filter.name {
            return Err(Error::Other(format!(
                "work board `{domain}` has an invalid blank/whitespace filter name"
            )));
        }
        if name == STATE_FILTER {
            return Err(Error::Other(format!(
                "work board `{domain}` redeclares the reserved `{STATE_FILTER}` filter"
            )));
        }
        if !names.insert(name) {
            return Err(Error::Other(format!(
                "work board `{domain}` declares duplicate filter `{name}`"
            )));
        }
        if let FilterType::Enum(values) = &filter.ty {
            if values.is_empty() {
                return Err(Error::Other(format!(
                    "work board `{domain}` filter `{name}` has an empty enum"
                )));
            }
            let mut seen = HashSet::new();
            for value in values {
                let trimmed = value.trim();
                if trimmed.is_empty() || trimmed != value || !seen.insert(trimmed) {
                    return Err(Error::Other(format!(
                        "work board `{domain}` filter `{name}` has a blank, whitespace-padded, or duplicate enum value"
                    )));
                }
            }
        }
    }

    let mut resources = HashSet::new();
    for declared in access {
        let subject = declared.subject();
        if subject.trim().is_empty() || subject.trim() != subject {
            return Err(Error::Other(format!(
                "work board `{domain}` authority subject `{subject}` is blank or whitespace-padded"
            )));
        }
        if !resources.insert(declared) {
            return Err(Error::Other(format!(
                "work board `{domain}` declares duplicate authority `{subject}`"
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> BoardSchema {
        BoardSchema::default()
    }

    #[test]
    fn a_backend_may_not_shadow_the_reserved_state_filter() {
        let mut declared = schema();
        declared.filters.push(FilterKey {
            name: STATE_FILTER.into(),
            ty: FilterType::String,
            required: false,
            description: None,
        });
        let error = validate_board_contract("board", &declared, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("reserved `state` filter"), "{error}");
    }

    #[test]
    fn page_bounds_and_domains_are_checked_once_at_registration() {
        let mut bad = schema();
        bad.default_page = 0;
        assert!(validate_board_contract("board", &bad, &[])
            .unwrap_err()
            .to_string()
            .contains("greater than zero"));

        let mut inverted = schema();
        inverted.default_page = 200;
        inverted.max_page = 100;
        assert!(validate_board_contract("board", &inverted, &[])
            .unwrap_err()
            .to_string()
            .contains("exceeds max_page"));

        assert!(validate_board_contract("Board", &schema(), &[]).is_err());
        assert!(validate_board_contract("bo.ard", &schema(), &[]).is_err());
        assert!(validate_board_contract("board", &schema(), &[]).is_ok());
    }

    #[test]
    fn duplicate_and_blank_authority_subjects_are_refused() {
        let blank = [LiveAccess::Network {
            subject: "  ".into(),
        }];
        assert!(validate_board_contract("board", &schema(), &blank).is_err());

        let duplicate = [
            LiveAccess::Network {
                subject: "https://board.example".into(),
            },
            LiveAccess::Network {
                subject: "https://board.example".into(),
            },
        ];
        assert!(validate_board_contract("board", &schema(), &duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate authority"));
    }

    /// The one rule the whole story turns on, checked at the lowest level: no mutating operation
    /// can be talked into a wildcard or empty subject by its parameters.
    #[test]
    fn no_parameter_shape_produces_a_wildcard_or_empty_mutating_subject() {
        let projection = BoardProjection {
            domain: "board".into(),
            filters: declared_filters(&schema()),
            schema: schema(),
            access: Vec::new(),
            backend: Arc::new(crate::datasource::MemoryBoard::new()),
        };
        let hostile = [
            json!({}),
            json!({"id": "*"}),
            json!({"id": ""}),
            json!({"id": "   "}),
            json!({"id": null}),
            json!({"id": 42}),
            json!({"id": {"nested": "*"}}),
            json!(null),
        ];
        for kind in [
            OpKind::Create,
            OpKind::Transition,
            OpKind::Claim,
            OpKind::Comment,
        ] {
            for params in &hostile {
                let subject = projection.subject(kind, params);
                assert!(!subject.is_empty(), "{kind:?} {params}");
                assert_ne!(subject, "*", "{kind:?} {params}");
                // `id: "*"` is a legal *literal* id, so the subject may contain it — but it must be
                // scoped under the domain and entity, never be a bare wildcard the matcher widens.
                assert!(
                    subject.starts_with("board/item/"),
                    "{kind:?} {params} -> {subject}"
                );
                assert_eq!(
                    projection.requirements(kind, params)[0],
                    AuthorityRequirement::datasource_write(&subject),
                    "{kind:?} {params}"
                );
            }
        }
    }
}
