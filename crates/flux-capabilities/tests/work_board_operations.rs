//! The eleven generated `<domain>.*` board operations (A-113, A-130, C-236, C-240).
//!
//! Mirrors `live_datasource_operations.rs` — the board port follows `try_register_live_datasource`
//! exactly for op generation, atomic registration on a clone, and the evidence surface. What is new
//! here, and what carries the review weight, is the **mutating** half: seven ops declaring
//! `Effect::Write` whose `permission_subjects` must stay concrete.

use std::collections::HashSet;
use std::sync::Arc;

use codewandler_flux_capabilities::{try_register_work_board, MemoryBoard, WorkBoard};
use flux_datasource::board::{ItemDraft, State};
use flux_datasource::live::Reference;
use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_runtime::{tool_fn, AuthorityRequirement, ToolContext, ToolRegistry};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::{System, Workspace};
use serde_json::{json, Value};

/// The seven ops that write. Every assertion about gating below is driven off this list, so adding a
/// mutating op without deciding its subject shape makes the tests fail rather than pass silently.
const MUTATING: [&str; 7] = [
    "board.create",
    "board.transition",
    "board.claim",
    "board.comment",
    "board.record_dispatch",
    "board.reassign",
    "board.record_evidence",
];

fn ctx() -> ToolContext {
    let root = std::env::temp_dir().join(format!(
        "flux-board-ops-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())))
}

fn registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    try_register_work_board(&mut registry, "board", Arc::new(MemoryBoard::new())).unwrap();
    registry
}

fn operation(registry: &ToolRegistry, name: &str) -> Arc<dyn flux_runtime::Tool> {
    registry
        .get(name)
        .unwrap_or_else(|| panic!("missing operation {name}"))
}

/// Params that reach each mutating op with a concrete item id.
fn mutating_params(op: &str, id: &str) -> Value {
    match op {
        "board.create" => json!({"title": "a new item"}),
        "board.transition" => json!({"id": id, "to": "claimed"}),
        "board.claim" => json!({"id": id, "assignee": "worker-a"}),
        "board.comment" => json!({"id": id, "text": "a note"}),
        "board.record_dispatch" => {
            json!({"id": id, "runner": "https://worker-1.internal:8787", "task_id": "t_1"})
        }
        "board.reassign" => json!({"id": id, "assignee": "worker-b"}),
        "board.record_evidence" => json!({"id": id, "entity": "commit", "entity_id": "deadbeef"}),
        other => panic!("unclassified mutating op {other}"),
    }
}

#[test]
fn registration_installs_eleven_source_labelled_generated_contracts() {
    let registry = registry();
    assert_eq!(
        registry.names(),
        [
            "board.claim",
            "board.comment",
            "board.comments",
            "board.create",
            "board.get",
            "board.list",
            "board.query",
            "board.reassign",
            "board.record_dispatch",
            "board.record_evidence",
            "board.transition",
        ]
    );
    for name in registry.names() {
        assert_eq!(
            registry.source(&name),
            Some("flux-capabilities work board `board`"),
            "{name} must carry the shared audit label"
        );
    }

    let transition = operation(&registry, "board.transition").spec();
    assert_eq!(
        transition.input_schema["properties"]["to"]["enum"],
        json!([
            "ready",
            "claimed",
            "in_progress",
            "review",
            "done",
            "blocked",
            "failed"
        ]),
        "the target-state enum is the closed State set"
    );
    assert_eq!(transition.input_schema["required"], json!(["id", "to"]));
    let list = operation(&registry, "board.list").spec();
    assert_eq!(list.input_schema["properties"]["limit"]["maximum"], 100);
    assert_eq!(list.input_schema["properties"]["limit"]["default"], 20);
}

#[test]
fn the_surface_groups_all_eleven_operations_behind_the_domain_signal() {
    let mut registry = ToolRegistry::new();
    let surface =
        try_register_work_board(&mut registry, "board", Arc::new(MemoryBoard::new())).unwrap();

    assert_eq!(surface.ambient_signal, "board");
    assert_eq!(
        surface.group,
        ToolGroup {
            name: "board".into(),
            description: "Work board operations for `board`.".into(),
            tools: vec![
                "board.list".into(),
                "board.get".into(),
                "board.create".into(),
                "board.transition".into(),
                "board.claim".into(),
                "board.comment".into(),
                "board.record_dispatch".into(),
                "board.query".into(),
                "board.comments".into(),
                "board.reassign".into(),
                "board.record_evidence".into(),
            ],
            surface_when: vec![SignalMatch {
                kind: KIND_SIGNAL.into(),
                signal: Some("board".into()),
            }],
        }
    );
    assert!(registry
        .active_specs(std::slice::from_ref(&surface.group), &HashSet::new())
        .is_empty());
    assert_eq!(
        registry
            .active_specs(
                std::slice::from_ref(&surface.group),
                &HashSet::from([surface.ambient_signal.clone()])
            )
            .len(),
        11
    );
}

/// **The safety surface (AGENTS.md:98).** A `Write` op reporting no subjects — or a `*` — either
/// gets forced to approval or matches a broad path grant. Neither is acceptable here: a grant
/// scoped to one item must not move another.
#[test]
fn every_mutating_operation_reports_a_concrete_permission_subject() {
    let registry = registry();

    for op in MUTATING {
        let tool = operation(&registry, op);
        let spec = tool.spec();
        assert!(
            spec.effects.contains(&Effect::Write),
            "{op} must declare Effect::Write"
        );

        let subjects = tool.permission_subjects(&mutating_params(op, "PROJ-42"));
        assert_eq!(subjects.len(), 1, "{op} reports exactly one subject");
        let subject = &subjects[0];
        assert!(!subject.is_empty(), "{op} must not report an empty subject");
        assert!(
            !subject.contains('*'),
            "{op} must not report a wildcard subject, got `{subject}`"
        );
        let expected = if op == "board.create" {
            "board/item/new".to_string()
        } else {
            "board/item/PROJ-42".to_string()
        };
        assert_eq!(subject, &expected, "{op} subject");

        assert_eq!(
            tool.authority_requirements(&mutating_params(op, "PROJ-42"), &subjects)
                .unwrap(),
            vec![AuthorityRequirement::datasource_write(expected)],
            "{op} authority"
        );
    }

    // A different item is a different subject — the whole point of scoping.
    let transition = operation(&registry, "board.transition");
    assert_eq!(
        transition.permission_subjects(&json!({"id": "OTHER-9", "to": "claimed"})),
        vec!["board/item/OTHER-9"]
    );

    // `create` has no id before the call, so it reports a *deliberately distinct* subject a policy
    // can grant separately from mutating items that already exist.
    let create = operation(&registry, "board.create");
    assert_eq!(
        create.permission_subjects(&json!({"title": "x"})),
        vec!["board/item/new"]
    );
    // Known and accepted: an item whose id is literally `new` shares `create`'s subject. The story
    // mandates `<domain>/item/new`, and no id-independent subject can avoid this. It is a
    // *narrowing* collision — granting `board/item/new` grants creation plus mutation of one
    // oddly-named item — not a widening one, so it cannot reach an item a grant did not name.
    assert_eq!(
        transition.permission_subjects(&json!({"id": "new", "to": "claimed"})),
        vec!["board/item/new"]
    );
}

/// A malformed call still has to report something concrete. The op rejects it at input validation,
/// but the subject is computed *before* that — falling back to `*` or `[]` here would be the exact
/// gating dodge AGENTS.md:98 names.
#[test]
fn a_mutating_call_with_no_usable_id_still_reports_a_concrete_subject() {
    let registry = registry();
    for op in MUTATING {
        if op == "board.create" {
            continue;
        }
        let tool = operation(&registry, op);
        for params in [json!({}), json!({"id": ""}), json!({"id": null})] {
            let subjects = tool.permission_subjects(&params);
            assert_eq!(
                subjects,
                vec!["board/item/<unresolved>"],
                "{op} with {params}"
            );
            assert!(!subjects[0].contains('*'), "{op} with {params}");
        }
    }
}

#[test]
fn the_read_operations_are_scoped_to_the_item_they_touch() {
    let registry = registry();
    let list = operation(&registry, "board.list");
    assert_eq!(list.permission_subjects(&json!({})), vec!["board/item"]);
    assert_eq!(
        list.authority_requirements(&json!({}), &["board/item".to_string()])
            .unwrap(),
        vec![AuthorityRequirement::datasource_read("board/item")]
    );

    let get = operation(&registry, "board.get");
    assert_eq!(
        get.permission_subjects(&json!({"id": "PROJ-42"})),
        vec!["board/item/PROJ-42"]
    );
    assert_eq!(
        get.authority_requirements(
            &json!({"id": "PROJ-42"}),
            &["board/item/PROJ-42".to_string()]
        )
        .unwrap(),
        vec![AuthorityRequirement::datasource_read("board/item/PROJ-42")]
    );

    // C-236: `query` pages the whole board like `list`; `comments` touches one item like `get`.
    let query = operation(&registry, "board.query");
    assert_eq!(query.permission_subjects(&json!({})), vec!["board/item"]);
    assert_eq!(
        query
            .authority_requirements(&json!({}), &["board/item".to_string()])
            .unwrap(),
        vec![AuthorityRequirement::datasource_read("board/item")]
    );

    let comments = operation(&registry, "board.comments");
    assert_eq!(
        comments.permission_subjects(&json!({"id": "PROJ-42"})),
        vec!["board/item/PROJ-42"]
    );
    assert_eq!(
        comments
            .authority_requirements(
                &json!({"id": "PROJ-42"}),
                &["board/item/PROJ-42".to_string()]
            )
            .unwrap(),
        vec![AuthorityRequirement::datasource_read("board/item/PROJ-42")]
    );
}

/// C-191's metadata-coherence invariants, applied to a generated catalog: a `Write` op may not keep
/// the read-only tier.
#[test]
fn the_generated_specs_are_metadata_coherent() {
    let registry = registry();
    for name in registry.names() {
        let tool = operation(&registry, &name);
        let spec = tool.spec();
        let violations = flux_spec::metadata_violations(&spec, &tool.semantic_effects());
        assert!(violations.is_empty(), "{name}: {violations:?}");

        if MUTATING.contains(&name.as_str()) {
            assert_ne!(spec.risk, Risk::Low, "{name} writes; it is not Risk::Low");
            assert_ne!(
                spec.idempotency,
                Idempotency::Idempotent,
                "{name} writes; repeating it is not free"
            );
            assert_eq!(spec.access, vec![AccessKind::Datasource]);
        } else {
            assert_eq!(spec.effects, vec![Effect::Read]);
            assert_eq!(spec.risk, Risk::Low);
        }
    }
}

#[tokio::test]
async fn the_generated_operations_drive_the_backend_and_render_consistently() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();

    let created = operation(&registry, "board.create")
        .execute(
            &ctx,
            json!({"title": "port the board", "repo": "codewandler/flux", "depends_on": ["A-112"]}),
        )
        .await
        .unwrap();
    assert_eq!(
        created.content,
        "[item item-1] port the board — ready (attempts 0)\ndepends_on: A-112\nrepo: codewandler/flux"
    );

    let claimed = operation(&registry, "board.claim")
        .execute(&ctx, json!({"id": "item-1", "assignee": "worker-a"}))
        .await
        .unwrap();
    assert!(claimed.content.contains("claimed"), "{}", claimed.content);
    assert!(claimed.content.contains("worker-a"), "{}", claimed.content);

    let moved = operation(&registry, "board.transition")
        .execute(&ctx, json!({"id": "item-1", "to": "in_progress"}))
        .await
        .unwrap();
    assert!(moved.content.contains("in_progress"), "{}", moved.content);

    operation(&registry, "board.comment")
        .execute(&ctx, json!({"id": "item-1", "text": "worker started"}))
        .await
        .unwrap();

    let listed = operation(&registry, "board.list")
        .execute(&ctx, json!({"filters": {"state": "in_progress"}}))
        .await
        .unwrap();
    assert_eq!(
        listed.content,
        "[item item-1] port the board — in_progress (attempts 0) assignee worker-a"
    );

    let got = operation(&registry, "board.get")
        .execute(&ctx, json!({"id": "item-1"}))
        .await
        .unwrap();
    assert!(got.content.starts_with("[item item-1]"), "{}", got.content);

    let missing = operation(&registry, "board.get")
        .execute(&ctx, json!({"id": "nope"}))
        .await
        .unwrap();
    assert_eq!(missing.content, "not found");

    let empty = operation(&registry, "board.list")
        .execute(&ctx, json!({"filters": {"state": "done"}}))
        .await
        .unwrap();
    assert_eq!(empty.content, "no items");
}

/// C-236: `query` is the machine-readable sibling of `list` — a JSON array of typed rows with
/// every field present (absent optionals are `null`, never missing keys), and an `output_schema`
/// that says so. `list` keeps its prose for humans.
#[tokio::test]
async fn query_returns_typed_rows_under_an_output_schema() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();

    let spec = operation(&registry, "board.query").spec();
    let output_schema = spec
        .output_schema
        .as_ref()
        .expect("board.query must declare an output_schema");
    assert_eq!(output_schema["type"], "array");
    let row = &output_schema["items"];
    for field in [
        "id",
        "title",
        "state",
        "assignee",
        "runner",
        "task_id",
        "depends_on",
        "repo",
        "attempts",
    ] {
        assert!(
            row["properties"].get(field).is_some(),
            "output_schema row is missing `{field}`: {row}"
        );
        assert!(
            row["required"]
                .as_array()
                .expect("row required is an array")
                .iter()
                .any(|r| r == field),
            "every row carries `{field}` (null when absent), so `$item.{field}` never errors"
        );
    }

    operation(&registry, "board.create")
        .execute(
            &ctx,
            json!({"title": "port the board", "repo": "codewandler/flux", "depends_on": ["A-112"]}),
        )
        .await
        .unwrap();
    operation(&registry, "board.claim")
        .execute(&ctx, json!({"id": "item-1", "assignee": "worker-a"}))
        .await
        .unwrap();
    operation(&registry, "board.record_dispatch")
        .execute(
            &ctx,
            json!({"id": "item-1", "runner": "https://worker-1.internal:8787", "task_id": "t_1"}),
        )
        .await
        .unwrap();

    let queried = operation(&registry, "board.query")
        .execute(&ctx, json!({}))
        .await
        .unwrap();
    let rows: Vec<Value> = serde_json::from_str(&queried.content)
        .expect("board.query returns a JSON array, not prose");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0],
        json!({
            "id": "item-1",
            "title": "port the board",
            "state": "claimed",
            "assignee": "worker-a",
            "runner": "https://worker-1.internal:8787",
            "task_id": "t_1",
            "depends_on": ["A-112"],
            "repo": "codewandler/flux",
            "attempts": 0,
        })
    );

    // An empty page is an empty array, still machine-readable.
    let none = operation(&registry, "board.query")
        .execute(&ctx, json!({"filters": {"state": "done"}}))
        .await
        .unwrap();
    assert_eq!(none.content, "[]");
}

/// C-236: "ready and unblocked" is one call — the `depends_on` filter rides `query` (and only
/// `query`: `list` keeps the human filter vocabulary unchanged).
#[tokio::test]
async fn query_filters_ready_and_unblocked_in_one_call() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();

    let parent = board
        .create(
            &ctx,
            ItemDraft {
                title: "parent".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();
    board
        .create(
            &ctx,
            ItemDraft {
                title: "child".into(),
                depends_on: vec![parent.id.clone()],
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();

    let query = operation(&registry, "board.query");
    async fn query_titles(
        query: &Arc<dyn flux_runtime::Tool>,
        ctx: &ToolContext,
        filters: Value,
    ) -> Vec<String> {
        let out = query
            .execute(ctx, json!({"filters": filters}))
            .await
            .unwrap();
        let rows: Vec<Value> = serde_json::from_str(&out.content).unwrap();
        rows.iter()
            .map(|row| row["title"].as_str().unwrap().to_string())
            .collect()
    }

    // The child is ready but blocked; only the parent is ready and unblocked.
    assert_eq!(
        query_titles(
            &query,
            &ctx,
            json!({"state": "ready", "depends_on": "satisfied"})
        )
        .await,
        vec!["parent".to_string()]
    );
    assert_eq!(
        query_titles(
            &query,
            &ctx,
            json!({"state": "ready", "depends_on": "unsatisfied"})
        )
        .await,
        vec!["child".to_string()]
    );

    // `list` does not take the filter — the human surface is unchanged.
    let error = operation(&registry, "board.list")
        .execute(&ctx, json!({"filters": {"depends_on": "satisfied"}}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("board.list.filters.depends_on: unknown filter"),
        "{error}"
    );

    // A bad filter value is rejected with the closed enum's vocabulary.
    let error = query
        .execute(&ctx, json!({"filters": {"depends_on": "maybe"}}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("board.query.filters.depends_on: expected one of"),
        "{error}"
    );
}

/// C-236: the read half of `board.comment` — a sweep can see what was recorded, oldest first.
#[tokio::test]
async fn comments_reads_back_what_comment_wrote() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();

    let spec = operation(&registry, "board.comments").spec();
    assert_eq!(
        spec.output_schema.as_ref().expect("comments output_schema")["type"],
        "array"
    );

    operation(&registry, "board.create")
        .execute(&ctx, json!({"title": "noted"}))
        .await
        .unwrap();
    operation(&registry, "board.comment")
        .execute(&ctx, json!({"id": "item-1", "text": "first note"}))
        .await
        .unwrap();
    operation(&registry, "board.comment")
        .execute(&ctx, json!({"id": "item-1", "text": "second note"}))
        .await
        .unwrap();

    let out = operation(&registry, "board.comments")
        .execute(&ctx, json!({"id": "item-1"}))
        .await
        .unwrap();
    let notes: Vec<String> = serde_json::from_str(&out.content).unwrap();
    assert_eq!(notes, vec!["first note", "second note"]);

    let error = operation(&registry, "board.comments")
        .execute(&ctx, json!({"id": "nope"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("no item `nope`"), "{error}");
}

/// C-240: `reassign` is the one path that moves an item off a holder that is gone, and it takes the
/// dead holder's run with it. Driven through the generated operation, because that is the only door a
/// Program has.
#[tokio::test]
async fn reassign_moves_the_holder_and_drops_the_dead_run() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();

    operation(&registry, "board.create")
        .execute(&ctx, json!({"title": "handed over"}))
        .await
        .unwrap();
    operation(&registry, "board.claim")
        .execute(&ctx, json!({"id": "item-1", "assignee": "worker-a"}))
        .await
        .unwrap();
    operation(&registry, "board.record_dispatch")
        .execute(
            &ctx,
            json!({"id": "item-1", "runner": "https://worker-a.internal:8787", "task_id": "t_1"}),
        )
        .await
        .unwrap();

    // `claim` still refuses a non-holder — that behaviour is unchanged.
    let conflict = operation(&registry, "board.claim")
        .execute(&ctx, json!({"id": "item-1", "assignee": "worker-b"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(conflict.contains("already claimed by `worker-a`"), "{conflict}");

    let moved = operation(&registry, "board.reassign")
        .execute(&ctx, json!({"id": "item-1", "assignee": "worker-b"}))
        .await
        .unwrap();
    assert!(moved.content.contains("assignee worker-b"), "{}", moved.content);
    assert!(
        !moved.content.contains("runner") && !moved.content.contains("task_id"),
        "the rendered item must not still advertise the dead run: {}",
        moved.content
    );
    assert!(moved.content.contains("claimed (attempts 0)"), "{}", moved.content);

    // The property the story names: the claim that conflicted now succeeds.
    operation(&registry, "board.claim")
        .execute(&ctx, json!({"id": "item-1", "assignee": "worker-b"}))
        .await
        .expect("the new holder may claim what it now holds");

    let stored = board.get(&ctx, "item-1").await.unwrap().unwrap();
    assert_eq!(stored.assignee.as_deref(), Some("worker-b"));
    assert_eq!(stored.runner, None);
    assert_eq!(stored.task_id, None);
}

/// C-240: `record_evidence` is the write `Item::evidence` never had. Both `Reference` spellings reach
/// the backend, `get` renders them back, and an ambiguous or empty reference is refused with the
/// operation path.
#[tokio::test]
async fn record_evidence_appends_both_reference_spellings_and_refuses_an_ambiguous_one() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();

    operation(&registry, "board.create")
        .execute(&ctx, json!({"title": "cites artifacts"}))
        .await
        .unwrap();

    let recorded = operation(&registry, "board.record_evidence")
        .execute(&ctx, json!({"id": "item-1", "entity": "commit", "entity_id": "deadbeef"}))
        .await
        .unwrap();
    assert!(
        recorded.content.contains("evidence: commit/deadbeef"),
        "{}",
        recorded.content
    );
    operation(&registry, "board.record_evidence")
        .execute(&ctx, json!({"id": "item-1", "url": "https://example.test/pr/1"}))
        .await
        .unwrap();
    // A replayed record does not double the list.
    operation(&registry, "board.record_evidence")
        .execute(&ctx, json!({"id": "item-1", "entity": "commit", "entity_id": "deadbeef"}))
        .await
        .unwrap();

    let got = operation(&registry, "board.get")
        .execute(&ctx, json!({"id": "item-1"}))
        .await
        .unwrap();
    assert_eq!(
        got.content,
        "[item item-1] cites artifacts — ready (attempts 0)\nevidence: commit/deadbeef\nevidence: https://example.test/pr/1"
    );
    assert_eq!(
        board.get(&ctx, "item-1").await.unwrap().unwrap().evidence,
        vec![
            Reference::Entity {
                entity: "commit".into(),
                id: "deadbeef".into()
            },
            Reference::Url {
                url: "https://example.test/pr/1".into()
            },
        ]
    );

    // `query` rows stay the reasoning surface: a weak-reference list is what `get` renders.
    let rows: Vec<Value> = serde_json::from_str(
        &operation(&registry, "board.query")
            .execute(&ctx, json!({}))
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    assert!(rows[0].get("evidence").is_none(), "{}", rows[0]);
}

#[tokio::test]
async fn an_illegal_transition_errors_through_the_operation_without_writing() {
    let mut registry = ToolRegistry::new();
    let board = Arc::new(MemoryBoard::new());
    try_register_work_board(&mut registry, "board", board.clone()).unwrap();
    let ctx = ctx();
    let item = board
        .create(
            &ctx,
            ItemDraft {
                title: "guarded".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();
    let before = board.get(&ctx, &item.id).await.unwrap().unwrap();

    let error = operation(&registry, "board.transition")
        .execute(&ctx, json!({"id": &item.id, "to": "done"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("`ready` -> `done`"), "{error}");
    assert_eq!(board.get(&ctx, &item.id).await.unwrap().unwrap(), before);
    assert_eq!(before.state, State::Ready);
}

#[tokio::test]
async fn bad_input_is_rejected_with_the_operation_path_before_backend_entry() {
    let registry = registry();
    let ctx = ctx();

    let cases = [
        (
            "board.transition",
            json!({"id": "item-1", "to": "elsewhere"}),
            "board.transition.to: unknown state `elsewhere`",
        ),
        (
            "board.transition",
            json!({"id": "  ", "to": "claimed"}),
            "board.transition.id: must not be blank",
        ),
        (
            "board.claim",
            json!({"id": "item-1", "assignee": ""}),
            "board.claim.assignee: must not be blank",
        ),
        (
            "board.comment",
            json!({"id": "item-1", "text": "   "}),
            "board.comment.text: must not be blank",
        ),
        (
            "board.create",
            json!({"title": ""}),
            "board.create.title: must not be blank",
        ),
        (
            "board.list",
            json!({"limit": 0}),
            "board.list.limit: must be greater than zero",
        ),
        (
            "board.list",
            json!({"filters": {"nope": "x"}}),
            "board.list.filters.nope: unknown filter",
        ),
        (
            "board.list",
            json!({"filters": {"state": "elsewhere"}}),
            "board.list.filters.state: expected one of",
        ),
        (
            "board.reassign",
            json!({"id": "item-1", "assignee": " "}),
            "board.reassign.assignee: must not be blank",
        ),
        (
            "board.record_evidence",
            json!({"id": "item-1"}),
            "board.record_evidence: name the artifact as either `url` or `entity` + `entity_id`",
        ),
        (
            "board.record_evidence",
            json!({"id": "item-1", "entity": "commit"}),
            "board.record_evidence: an `entity` reference needs both",
        ),
        (
            "board.record_evidence",
            json!({"id": "item-1", "url": "https://x.test", "entity": "commit", "entity_id": "d"}),
            "board.record_evidence: `url` and `entity`/`entity_id` are mutually exclusive",
        ),
    ];
    for (op, params, expected) in cases {
        let error = operation(&registry, op)
            .execute(&ctx, params.clone())
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(expected),
            "expected `{expected}` in `{error}` for {op} {params}"
        );
    }
}

#[test]
fn a_collision_rejects_the_whole_pack_without_leaking_a_partial_registration() {
    let existing = tool_fn(
        ToolSpec::read_only("board.claim", "existing", json!({"type": "object"})),
        |_params| async { Ok(json!("existing")) },
    );
    let mut registry = ToolRegistry::new();
    registry.try_register_from("fixture", existing).unwrap();

    let error = try_register_work_board(&mut registry, "board", Arc::new(MemoryBoard::new()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("board.claim"), "{error}");
    assert!(error.contains("fixture"), "{error}");

    assert_eq!(registry.names(), ["board.claim"]);
    assert_eq!(registry.source("board.claim"), Some("fixture"));
    for name in [
        "board.list",
        "board.get",
        "board.create",
        "board.transition",
    ] {
        assert!(registry.get(name).is_none(), "{name} leaked");
    }
}

#[test]
fn an_invalid_domain_is_refused_at_registration() {
    let mut registry = ToolRegistry::new();
    let error = try_register_work_board(&mut registry, "Board", Arc::new(MemoryBoard::new()))
        .unwrap_err()
        .to_string();
    assert!(error.contains("[a-z][a-z0-9_]*"), "{error}");
    assert!(registry.names().is_empty());
}
