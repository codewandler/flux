//! The write-capable [`WorkBoard`] port and its eleven generated operations (A-113, A-130, C-236,
//! C-240).
//!
//! This is the sibling of [`super::live::LiveDatasource`], and deliberately the same shape: a
//! backend declares a schema plus its external authority, the contract is snapshotted and validated
//! **once** at registration, and the host then generates uniform operations with stable permission
//! subjects, a per-domain [`ToolGroup`] and an ambient signal, installed atomically on a clone.
//! Everything a reader already knows about `try_register_live_datasource` transfers.
//!
//! What is new is that seven of the eleven operations **write**. That changes two things and nothing
//! else:
//!
//! * **Permission subjects stay concrete.** `board.transition` on `PROJ-42` reports
//!   `board:board/item/PROJ-42`, never `board:board/item/*` and never nothing, so a grant scoped to one item
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
use flux_datasource::board::{
    BoardSchema, DependencyMatch, Item, ItemDraft, State, DEPENDS_ON_FILTER, EDGE_DIAGRAM,
};
use flux_datasource::live::{FilterKey, FilterType, Filters, Page, PageRequest, Reference};
use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_runtime::{
    AuthorityRequirement, DispatchLedger, Tool, ToolContext, ToolRegistry, ToolResult,
};
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

/// The reserved `depends_on` filter the host declares on `query` (C-236).
///
/// Query-only on purpose: `list` is the human, prose surface and keeps its original filter
/// vocabulary; the structured surface is where a coordinator expresses "ready and unblocked". The
/// semantics every backend applies live in [`DependencyMatch`] — like [`STATE_FILTER`], a backend
/// may not redeclare the name.
fn depends_on_filter() -> FilterKey {
    FilterKey {
        name: DEPENDS_ON_FILTER.to_string(),
        ty: FilterType::Enum(
            DependencyMatch::ALL
                .iter()
                .map(|m| m.as_str().to_string())
                .collect(),
        ),
        required: false,
        description: Some(
            "Dependency gating: `satisfied` keeps items whose every dependency is `done` (no \
             dependencies is trivially satisfied); `unsatisfied` keeps items still waiting on at \
             least one. An absent dependency never resolves."
                .to_string(),
        ),
    }
}

/// A write-capable work board: a typed item state machine behind a swappable implementation.
///
/// Jira is one implementation, a markdown file store is another; the coordinator agent only ever
/// sees the generated operations. Implementations must perform real IO through flux's guarded host
/// surfaces exactly as a [`LiveDatasource`](super::live::LiveDatasource) does — the
/// [`ToolContext`] keeps filesystem and process access on the canonical runtime context, and any
/// URL or connection client must still pass the guards its [`LiveAccess`] declaration names.
///
/// **Every implementation must pass the shared contract suite verbatim**
/// (`crates/flux-capabilities/tests/board_contract/mod.rs`). In particular the properties the
/// generated operations cannot enforce for you:
///
/// * an illegal edge errors and performs **no write** — the item is byte-identical afterwards;
/// * a retry edge increments [`Item::attempts`], and no other edge touches it;
/// * a retry edge clears `runner`/`task_id` and leaves `assignee` alone (C-240);
/// * `claim` is idempotent for the same assignee and conflicts for a different one;
/// * the reserved `depends_on` filter treats an item as blocked until **every** dependency is
///   `done` — an absent dependency never resolves (C-236);
/// * `comments` reads back what `comment` wrote, oldest first (C-236);
/// * a recorded dispatch is **durable** — `runner` and `task_id` survive a fresh read;
/// * `reassign` moves the holder so the new one can `claim` where it would have conflicted, and
///   `record_evidence` appends a durable [`Reference`] (C-240).
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
    /// before writing anything, and on exactly the edges
    /// [`is_retry`](flux_datasource::board::is_retry) names they must both increment
    /// [`Item::attempts`] and clear [`Item::runner`] / [`Item::task_id`] — never
    /// [`Item::assignee`]. That predicate is the single copy of both rules; a backend that
    /// re-derives either is what C-240 fixed.
    async fn transition(&self, ctx: &ToolContext, id: &str, to: State) -> Result<Item>;

    /// Take ownership of an item. Idempotent for the current holder; a conflict for anyone else.
    async fn claim(&self, ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item>;

    /// Hand an item to a different worker (C-240).
    ///
    /// [`claim`](WorkBoard::claim) conflicts for anyone but the current holder, which is right for
    /// two live workers racing and wrong for the case the sweep actually meets: the holder is dead
    /// and the work has to move. `reassign` is therefore a deliberately **forcible** takeover — it
    /// does not consult the current holder. Its gate is authority, not state: like every other
    /// mutation it reports a concrete `<domain>/item/<id>` subject, so an operator grants the power
    /// to move *this* item and nothing else.
    ///
    /// Two obligations:
    ///
    /// * **The dead run goes with the old holder.** Setting `assignee` also clears
    ///   [`Item::runner`] and [`Item::task_id`], for the reason
    ///   [`is_retry`](flux_datasource::board::is_retry) gives: a record naming the previous
    ///   worker's run would have the coordinator report progress on a process that is gone.
    /// * **Not a state change.** No edge, no `attempts`.
    ///   [`transition`](WorkBoard::transition) stays the single entry point into the state machine.
    ///
    /// Idempotent for the assignee named: reassigning to the same worker rewrites the same fields.
    async fn reassign(&self, ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item>;

    /// Append a weak locator for an artifact produced against `id` (C-240).
    ///
    /// [`Item::evidence`] round-tripped through the backends from the start but nothing could write
    /// it — the same defect A-130 fixed for `runner`/`task_id`. It is the diff-handoff channel: a
    /// worker records the commit it produced (`commit/<sha>`) and the review that accepted it (a
    /// URL), and a coordinator reads them back off the item.
    ///
    /// Three obligations, all pinned by the contract suite:
    ///
    /// * **Durable and appending.** A subsequent [`get`](WorkBoard::get) by an unrelated reader sees
    ///   every reference recorded so far, oldest first. Unlike
    ///   [`record_dispatch`](WorkBoard::record_dispatch) this is a log, not a slot: an item
    ///   legitimately carries a commit *and* a PR.
    /// * **A repeat of the same reference is a no-op.** A reworked item records the same commit
    ///   again; a duplicate entry carries no information, so an already-present reference is
    ///   dropped rather than appended. That is what makes the operation safe to replay.
    /// * **Not a state change.** No edge, no `attempts`, no assignee.
    async fn record_evidence(
        &self,
        ctx: &ToolContext,
        id: &str,
        reference: Reference,
    ) -> Result<Item>;

    /// Record that `id` was dispatched to a worker: bind it to the worker's address (`runner`) and
    /// the worker-minted handle (`task_id`). This is the write that makes the board a **run
    /// registry** — see `docs/designs/fleet-coordinator.md` §5 — and without it a restarted
    /// coordinator can re-derive an item's *state* but never the run executing it.
    ///
    /// Three obligations, all pinned by the contract suite:
    ///
    /// * **Durable, not returned-only.** A subsequent [`get`](WorkBoard::get) by an unrelated
    ///   reader must see both fields.
    /// * **Replacing, not appending.** A retried item is dispatched again; a stale `task_id` would
    ///   send the sweep after a run that no longer exists.
    /// * **Not a state change.** It writes those two fields and nothing else — no edge, no
    ///   `attempts`, no assignee. [`transition`](WorkBoard::transition) stays the single entry
    ///   point into the state machine, because a second one could not be edge-checked.
    async fn record_dispatch(
        &self,
        ctx: &ToolContext,
        id: &str,
        runner: &str,
        task_id: &str,
    ) -> Result<Item>;

    /// Append a note to an item. An absent id is an error.
    async fn comment(&self, ctx: &ToolContext, id: &str, text: &str) -> Result<()>;

    /// Read back the notes left on one item, oldest first. An absent id is an error — the same
    /// one [`comment`](WorkBoard::comment) reports, so the read path and the write path agree
    /// about which items exist.
    ///
    /// Notes sit *beside* the [`Item`], not inside it: they are not part of the item's identity
    /// (a refused transition still leaves the item byte-identical, comments and all), and reading
    /// them changes nothing. This is the read half of the sweep's evidence trail — what a worker
    /// recorded with `comment`, a coordinator can see (C-236). For a document-backed board the
    /// mapping is explicit: [`super::MarkdownBoard`] renders a comment as a markdown bullet in the
    /// item's document, so read-back reports every top-level bullet of that document in order.
    async fn comments(&self, ctx: &ToolContext, id: &str) -> Result<Vec<String>>;
}

/// Evidence-gated catalog metadata returned with one registered board.
///
/// The sibling of [`LiveDatasourceSurface`](super::live::LiveDatasourceSurface): hosts install
/// [`group`](Self::group) beside the generated tools and add
/// [`ambient_signal`](Self::ambient_signal) whenever the configured backend is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkBoardSurface {
    /// Per-domain group containing exactly the eleven generated operations.
    pub group: ToolGroup,
    /// Ambient project signal emitted because this board is configured.
    pub ambient_signal: String,
}

/// The eleven operation suffixes a board generates, in catalog order.
///
/// The first seven are A-113/A-130's. `query` (C-236) is the machine-readable sibling of `list` —
/// typed rows under an `output_schema` instead of prose — and `comments` the read half of
/// `comment`. `reassign` and `record_evidence` are C-240's: moving an item off a dead worker, and
/// writing the one [`Item`] field nothing could write.
const OPERATIONS: [&str; 11] = [
    "list",
    "get",
    "create",
    "transition",
    "claim",
    "comment",
    "record_dispatch",
    "query",
    "comments",
    "reassign",
    "record_evidence",
];

/// Build the uniform `<domain>.list` / `.get` / `.create` / `.transition` / `.claim` / `.comment`
/// / `.record_dispatch` / `.query` / `.comments` / `.reassign` / `.record_evidence` operations for
/// one board backend.
///
/// The backend contract is snapshotted and validated once, so the filters, page bounds and external
/// authority advertised at registration are the same vocabulary used to route calls — a backend
/// cannot widen its own authority after the fact.
pub fn work_board_tools(domain: &str, backend: Arc<dyn WorkBoard>) -> Result<Vec<Arc<dyn Tool>>> {
    let schema = backend.schema();
    let access = backend.access();
    validate_board_contract(domain, &schema, &access)?;

    let filters = declared_filters(&schema);
    let mut query_filters = filters.clone();
    query_filters.push(depends_on_filter());
    let projection = Arc::new(BoardProjection {
        domain: domain.to_string(),
        filters,
        query_filters,
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

/// Atomically install exactly the eleven operations for one board domain.
///
/// All eleven share an auditable source label. [`ToolRegistry::try_register_all_from`] assembles on
/// a clone, so a collision or an invalid declaration leaves the caller's registry unchanged — there
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
    /// [`filters`](Self::filters) plus the reserved `depends_on` filter — the set `query` accepts
    /// (C-236). `list` never sees it: the human surface keeps its original vocabulary.
    query_filters: Vec<FilterKey>,
    access: Vec<LiveAccess>,
    backend: Arc<dyn WorkBoard>,
}

/// The subject one item occupies: `<domain>/item/<id>`.
///
/// The single spelling of that shape, so the generated operations and [`BoardLedger`] — which gates
/// the *same* write from `fleet.dispatch` — cannot drift into naming the same item two ways. A
/// blank id collapses to [`UNRESOLVED_ID`] rather than to something a wildcard could widen.
fn item_subject(domain: &str, id: &str) -> String {
    let id = match id.trim() {
        "" => UNRESOLVED_ID,
        id => id,
    };
    format!("board:{domain}/{ENTITY}/{id}")
}

impl BoardProjection {
    /// The subject one invocation touches. Never `*`, never empty — see [`UNRESOLVED_ID`].
    fn subject(&self, kind: OpKind, params: &Value) -> String {
        match kind {
            OpKind::List | OpKind::Query => format!("board:{}/{ENTITY}", self.domain),
            OpKind::Create => format!("board:{}/{ENTITY}/{NEW_ID}", self.domain),
            _ => item_subject(
                &self.domain,
                params.get("id").and_then(Value::as_str).unwrap_or_default(),
            ),
        }
    }

    fn requirements(&self, kind: OpKind, params: &Value) -> Vec<AuthorityRequirement> {
        let subject = self.subject(kind, params);
        let mut requirements = vec![if kind.writes() {
            AuthorityRequirement::board_write(subject)
        } else {
            AuthorityRequirement::board_read(subject)
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

/// Which of the eleven an instance is. One `impl Tool` covers all of them because they differ only
/// in their input contract and their one backend call — the subject, authority and spec derivation
/// are shared, which is the whole point of generating them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpKind {
    List,
    Get,
    Create,
    Transition,
    Claim,
    Comment,
    RecordDispatch,
    Query,
    Comments,
    Reassign,
    RecordEvidence,
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
            "record_dispatch" => Self::RecordDispatch,
            "query" => Self::Query,
            "comments" => Self::Comments,
            "reassign" => Self::Reassign,
            "record_evidence" => Self::RecordEvidence,
            other => unreachable!("undeclared board operation `{other}`"),
        }
    }

    /// Whether this operation mutates the board. The seven that do carry `Effect::Write`, a
    /// `datasource_write` requirement, and a non-`Low` risk tier. Stated as the complement of the
    /// reads so a newly declared operation is a write until someone says otherwise.
    fn writes(&self) -> bool {
        !matches!(self, Self::List | Self::Get | Self::Query | Self::Comments)
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
            OpKind::RecordDispatch => {
                let input: RecordDispatchInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let runner = require(op, "runner", &input.runner)?;
                let task_id = require(op, "task_id", &input.task_id)?;
                ToolResult::ok(render_full(
                    &backend.record_dispatch(ctx, id, runner, task_id).await?,
                ))
            }
            OpKind::Query => {
                let input: ListInput = parse(op, params)?;
                let filters = normalize_filters(
                    op,
                    &format!("board `{}`", self.projection.domain),
                    &self.projection.query_filters,
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
                // Typed rows, not prose: one page as a bare JSON array so `each $item in …` can
                // bind the fields directly (the runtime's string-leaf re-parse rule reads the
                // array back). Every row carries every field — absent optionals are `null`, so
                // `$item.runner` never errors on an undispatched item. The cursor stays with
                // `list`: paging prose is the human surface.
                let rows: Vec<Value> = backend
                    .list(ctx, &filters, page)
                    .await?
                    .rows
                    .iter()
                    .map(item_row)
                    .collect();
                ToolResult::ok(serde_json::to_string(&rows)?)
            }
            OpKind::Comments => {
                let input: IdInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let comments = backend.comments(ctx, id).await?;
                ToolResult::ok(serde_json::to_string(&comments)?)
            }
            OpKind::Reassign => {
                let input: ClaimInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let assignee = require(op, "assignee", &input.assignee)?;
                ToolResult::ok(render_full(&backend.reassign(ctx, id, assignee).await?))
            }
            OpKind::RecordEvidence => {
                let input: RecordEvidenceInput = parse(op, params)?;
                let id = require(op, "id", &input.id)?;
                let reference = parse_reference(op, &input)?;
                ToolResult::ok(render_full(
                    &backend.record_evidence(ctx, id, reference).await?,
                ))
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordDispatchInput {
    #[serde(default)]
    id: String,
    #[serde(default)]
    runner: String,
    #[serde(default)]
    task_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordEvidenceInput {
    #[serde(default)]
    id: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    entity: String,
    #[serde(default)]
    entity_id: String,
}

/// Resolve one `record_evidence` call's flat input into the [`Reference`] it names.
///
/// [`Reference`] is a two-variant sum, and a JSON `oneOf` is a shape models reliably get wrong. So
/// the operation takes the two spellings side by side and requires **exactly one**: either `url`, or
/// `entity` + `entity_id` together. Ambiguity is an error rather than a preference for one side —
/// silently picking would let a caller believe it recorded a URL while the board stored an entity.
///
/// The two spellings are the two [`render_full`] prints, so what a reader sees on `get`
/// (`evidence: commit/<sha>`, `evidence: https://…`) is what this accepts back.
fn parse_reference(operation: &str, input: &RecordEvidenceInput) -> Result<Reference> {
    let url = input.url.trim();
    let entity = input.entity.trim();
    let entity_id = input.entity_id.trim();

    match (url.is_empty(), entity.is_empty(), entity_id.is_empty()) {
        (false, true, true) => Ok(Reference::Url {
            url: url.to_string(),
        }),
        (true, false, false) => Ok(Reference::Entity {
            entity: entity.to_string(),
            id: entity_id.to_string(),
        }),
        (true, true, true) => Err(Error::Other(format!(
            "{operation}: name the artifact as either `url` or `entity` + `entity_id`; neither was given"
        ))),
        (true, _, _) => Err(Error::Other(format!(
            "{operation}: an `entity` reference needs both `entity` and `entity_id`"
        ))),
        (false, _, _) => Err(Error::Other(format!(
            "{operation}: `url` and `entity`/`entity_id` are mutually exclusive; name exactly one artifact"
        ))),
    }
}

// ---------------------------------------------------------------------------
// The dispatch ledger
// ---------------------------------------------------------------------------

/// One registered board, viewed as the [`DispatchLedger`] that `fleet.dispatch` writes to (A-130).
///
/// `fleet.dispatch` lives in `flux-orchestrate` (L3) and this port lives here (L5), so the caller
/// can never name [`WorkBoard`] directly. [`DispatchLedger`] is the L2 seam both sides already see;
/// this is the adapter that fills it, and it is deliberately the *whole* adapter — a fleet op holds
/// one of these and nothing else about the board.
///
/// Wiring one to a `fleet.dispatch` is what turns the write-back from advice into a contract: an
/// op with a ledger refuses to leave a dispatched run unrecorded.
pub struct BoardLedger {
    domain: String,
    board: Arc<dyn WorkBoard>,
}

impl BoardLedger {
    /// View `board`, registered under `domain`, as a dispatch ledger. `domain` must be the same one
    /// passed to [`try_register_work_board`] — it is what makes the subject this ledger reports
    /// match the subject the generated `<domain>.record_dispatch` reports for the same item.
    pub fn new(domain: impl Into<String>, board: Arc<dyn WorkBoard>) -> Self {
        Self {
            domain: domain.into(),
            board,
        }
    }
}

#[async_trait]
impl DispatchLedger for BoardLedger {
    fn subject(&self, item: &str) -> String {
        item_subject(&self.domain, item)
    }

    async fn record_dispatch(
        &self,
        ctx: &ToolContext,
        item: &str,
        runner: &str,
        task_id: &str,
    ) -> Result<()> {
        self.board
            .record_dispatch(ctx, item, runner, task_id)
            .await
            .map(|_| ())
    }
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

/// One item as a typed `query` row (C-236).
///
/// Deliberately not [`Item`]'s own serde: that skips absent fields, and a missing key is a *loud
/// error* under the runtime's strict `$item.field` access. A coordinator iterating a mixed board
/// cannot have `$item.runner` blow up on the undispatched rows, so every row carries every field —
/// `null` when absent. `evidence` stays out: the row is the reasoning surface (`id`, `state`,
/// `runner`, `task_id`, `depends_on`, …), and a weak-reference list is what `get` renders for a
/// human.
fn item_row(item: &Item) -> Value {
    json!({
        "id": item.id,
        "title": item.title,
        "state": item.state.as_str(),
        "assignee": item.assignee,
        "runner": item.runner,
        "task_id": item.task_id,
        "depends_on": item.depends_on,
        "repo": item.repo,
        "attempts": item.attempts,
    })
}

/// The `output_schema` of `query` — the machine-readable contract [`item_row`] produces.
fn query_output_schema() -> Value {
    json!({
        "type": "array",
        "description": "One page of typed board rows. Every row carries every field; absent optionals are null.",
        "items": {
            "type": "object",
            "properties": {
                "id": {"type": "string"},
                "title": {"type": "string"},
                "state": {
                    "type": "string",
                    "enum": State::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>()
                },
                "assignee": {"type": ["string", "null"]},
                "runner": {"type": ["string", "null"]},
                "task_id": {"type": ["string", "null"]},
                "depends_on": {"type": "array", "items": {"type": "string"}},
                "repo": {"type": ["string", "null"]},
                "attempts": {"type": "integer", "minimum": 0}
            },
            "required": [
                "id", "title", "state", "assignee", "runner", "task_id", "depends_on", "repo",
                "attempts"
            ]
        }
    })
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
            page_schema(projection, &projection.filters),
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
        OpKind::RecordDispatch => (
            format!(
                "Bind a `{domain}` item to the worker running it, so a restarted coordinator can \
                 find the run again. Records the worker address and its task id; does not move the \
                 item's state."
            ),
            object(
                json!({
                    "id": {"type": "string", "description": "Stable item id"},
                    "runner": {
                        "type": "string",
                        "description": "Address of the worker executing it, e.g. its A2A endpoint"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Handle the worker minted for the run, from fleet.dispatch"
                    }
                }),
                &["id", "runner", "task_id"],
            ),
        ),
        OpKind::Query => (
            format!(
                "Query one page of `{domain}` items as typed JSON rows — the machine-readable \
                 sibling of `{domain}.list`, for `each`/`match` rather than for reading. Every row \
                 carries every field; absent optionals are `null`. The `depends_on` filter makes \
                 \"ready and unblocked\" one call: `satisfied` keeps only items whose every \
                 dependency is `done`."
            ),
            page_schema(projection, &projection.query_filters),
        ),
        OpKind::Comments => (
            format!(
                "Read back the notes left on one `{domain}` item, oldest first — the read half of \
                 `{domain}.comment`."
            ),
            object(
                json!({"id": {"type": "string", "description": "Stable item id"}}),
                &["id"],
            ),
        ),
        OpKind::Reassign => (
            format!(
                "Hand a `{domain}` item to a different worker, for when the current holder is gone \
                 — unlike `{domain}.claim` this does not conflict with the existing assignee. Also \
                 clears the recorded runner and task id, because the previous worker's run is dead; \
                 does not move the item's state."
            ),
            object(
                json!({
                    "id": {"type": "string", "description": "Stable item id"},
                    "assignee": {
                        "type": "string",
                        "description": "Worker the item is handed to; replaces the current holder"
                    }
                }),
                &["id", "assignee"],
            ),
        ),
        OpKind::RecordEvidence => (
            format!(
                "Attach a locator for an artifact produced against a `{domain}` item — a commit, a \
                 pull request, a build. Appends to the item's evidence list; recording the same \
                 artifact twice changes nothing. Name exactly one artifact: either `url`, or \
                 `entity` plus `entity_id`. Does not move the item's state."
            ),
            object(
                json!({
                    "id": {"type": "string", "description": "Stable item id"},
                    "url": {
                        "type": "string",
                        "description": "Navigation URL for the artifact, e.g. a pull request. \
                                        Mutually exclusive with entity/entity_id, and never a \
                                        credential or presigned secret"
                    },
                    "entity": {
                        "type": "string",
                        "description": "Artifact kind, e.g. `commit`. Requires entity_id"
                    },
                    "entity_id": {
                        "type": "string",
                        "description": "Stable id within that kind, e.g. the commit sha"
                    }
                }),
                &["id"],
            ),
        ),
    };

    let spec = ToolSpec::read_only(name, description, schema);
    // The two structured reads are the only board ops a Program consumes as data rather than as
    // prose, so they are the only ones that can honestly advertise an `output_schema` (C-236).
    let spec = match kind {
        OpKind::Query => spec.with_output_schema(query_output_schema()),
        OpKind::Comments => spec.with_output_schema(json!({
            "type": "array",
            "description": "The item's notes, oldest first.",
            "items": {"type": "string"}
        })),
        _ => spec,
    };
    let mut spec = if kind.writes() {
        // C-191's coherence invariants: a `Write` may keep neither the `Risk::Low` tier nor the
        // `Idempotent` claim. Four are genuinely safe to repeat under a stated condition, which is
        // exactly what `Conditional` is for: `claim` for its current holder, `record_dispatch` for
        // the same `(runner, task_id)`, `reassign` for the same assignee — each replays into the
        // same fields with the same values — and `record_evidence` for a reference the item already
        // carries, which it drops rather than duplicating. None may be `Idempotent`, which would
        // license the op cache to skip the call entirely and silently drop the write.
        let mut spec = spec.with_risk(Risk::Medium);
        spec.idempotency = if matches!(
            kind,
            OpKind::Claim | OpKind::RecordDispatch | OpKind::Reassign | OpKind::RecordEvidence
        ) {
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

/// The paging + filter input schema shared by `list` and `query`.
///
/// The two differ in exactly one thing — the filter vocabulary they accept (`query` additionally
/// takes the reserved `depends_on`, C-236) — so the caller passes the set rather than the schema
/// being derived twice.
fn page_schema(projection: &BoardProjection, declared: &[FilterKey]) -> Value {
    let mut properties = Map::new();
    let mut required_filters = Vec::new();
    for filter in declared {
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
        // Both reserved names belong to the host: `state` on every read, `depends_on` on `query`
        // (C-236). A backend that redeclared either would be silently shadowed on one surface and
        // authoritative on the other, so it is refused outright.
        if name == STATE_FILTER || name == DEPENDS_ON_FILTER {
            return Err(Error::Other(format!(
                "work board `{domain}` redeclares the reserved `{name}` filter"
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

    /// C-236: `depends_on` is the host's too. It rides `query` only, but reserving it on the whole
    /// contract is what keeps a backend from being authoritative on `list` and shadowed on `query`.
    #[test]
    fn a_backend_may_not_shadow_the_reserved_depends_on_filter() {
        let mut declared = schema();
        declared.filters.push(FilterKey {
            name: DEPENDS_ON_FILTER.into(),
            ty: FilterType::String,
            required: false,
            description: None,
        });
        let error = validate_board_contract("board", &declared, &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("reserved `depends_on` filter"), "{error}");
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

    /// C-240: `record_evidence` takes the two [`Reference`] spellings side by side and accepts
    /// **exactly one**. Ambiguity is refused rather than resolved — a caller must never believe it
    /// recorded a URL while the board stored an entity.
    #[test]
    fn record_evidence_accepts_exactly_one_reference_spelling() {
        fn input(url: &str, entity: &str, entity_id: &str) -> RecordEvidenceInput {
            RecordEvidenceInput {
                id: "item-1".into(),
                url: url.into(),
                entity: entity.into(),
                entity_id: entity_id.into(),
            }
        }
        assert_eq!(
            parse_reference(
                "board.record_evidence",
                &input("https://x.test/pr/1", "", "")
            )
            .unwrap(),
            Reference::Url {
                url: "https://x.test/pr/1".into()
            }
        );
        assert_eq!(
            parse_reference(
                "board.record_evidence",
                &input("", " commit ", " deadbeef ")
            )
            .unwrap(),
            Reference::Entity {
                entity: "commit".into(),
                id: "deadbeef".into()
            },
            "each half is trimmed, exactly as `require` trims the id"
        );

        for (case, expected) in [
            (input("", "", ""), "neither was given"),
            (input("   ", "  ", " "), "neither was given"),
            (
                input("", "commit", ""),
                "needs both `entity` and `entity_id`",
            ),
            (
                input("", "", "deadbeef"),
                "needs both `entity` and `entity_id`",
            ),
            (
                input("https://x.test", "commit", "deadbeef"),
                "mutually exclusive",
            ),
            (input("https://x.test", "commit", ""), "mutually exclusive"),
        ] {
            let error = parse_reference("board.record_evidence", &case)
                .expect_err("an ambiguous or empty reference is refused")
                .to_string();
            assert!(error.contains("board.record_evidence"), "{error}");
            assert!(error.contains(expected), "expected `{expected}` in {error}");
        }
    }

    /// The one rule the whole story turns on, checked at the lowest level: no mutating operation
    /// can be talked into a wildcard or empty subject by its parameters.
    #[test]
    fn no_parameter_shape_produces_a_wildcard_or_empty_mutating_subject() {
        let projection = BoardProjection {
            domain: "board".into(),
            filters: declared_filters(&schema()),
            query_filters: declared_filters(&schema()),
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
            OpKind::RecordDispatch,
            OpKind::Reassign,
            OpKind::RecordEvidence,
        ] {
            for params in &hostile {
                let subject = projection.subject(kind, params);
                assert!(!subject.is_empty(), "{kind:?} {params}");
                assert_ne!(subject, "*", "{kind:?} {params}");
                // `id: "*"` is a legal *literal* id, so the subject may contain it — but it must be
                // scoped under the domain and entity, never be a bare wildcard the matcher widens.
                assert!(
                    subject.starts_with("board:board/item/"),
                    "{kind:?} {params} -> {subject}"
                );
                assert_eq!(
                    projection.requirements(kind, params)[0],
                    AuthorityRequirement::board_write(&subject),
                    "{kind:?} {params}"
                );
            }
        }
    }
}
