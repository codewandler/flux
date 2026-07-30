//! The seven generated `<domain>.*` board operations (A-113, A-130).
//!
//! Mirrors `live_datasource_operations.rs` — the board port follows `try_register_live_datasource`
//! exactly for op generation, atomic registration on a clone, and the evidence surface. What is new
//! here, and what carries the review weight, is the **mutating** half: five ops declaring
//! `Effect::Write` whose `permission_subjects` must stay concrete.

use std::collections::HashSet;
use std::sync::Arc;

use codewandler_flux_capabilities::{try_register_work_board, MemoryBoard, WorkBoard};
use flux_datasource::board::{ItemDraft, State};
use flux_evidence::{SignalMatch, ToolGroup, KIND_SIGNAL};
use flux_runtime::{tool_fn, AuthorityRequirement, ToolContext, ToolRegistry};
use flux_spec::{AccessKind, Effect, Idempotency, Risk, ToolSpec};
use flux_system::{System, Workspace};
use serde_json::{json, Value};

/// The five ops that write. Every assertion about gating below is driven off this list, so adding a
/// mutating op without deciding its subject shape makes the tests fail rather than pass silently.
const MUTATING: [&str; 5] = [
    "board.create",
    "board.transition",
    "board.claim",
    "board.comment",
    "board.record_dispatch",
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
        other => panic!("unclassified mutating op {other}"),
    }
}

#[test]
fn registration_installs_seven_source_labelled_generated_contracts() {
    let registry = registry();
    assert_eq!(
        registry.names(),
        [
            "board.claim",
            "board.comment",
            "board.create",
            "board.get",
            "board.list",
            "board.record_dispatch",
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
fn the_surface_groups_all_seven_operations_behind_the_domain_signal() {
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
        7
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
