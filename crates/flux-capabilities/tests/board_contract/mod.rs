//! The shared [`WorkBoard`] contract suite (A-113).
//!
//! Every board backend must pass this verbatim. It is a plain module rather than a test binary so a
//! new backend's test file picks it up with one `mod board_contract;` and calls
//! [`assert_work_board_contract`] — `MarkdownBoard` (A-114), `JiraBoard` (A-115) and `GitlabBoard`
//! (A-118) reuse it unchanged.
//!
//! It runs **offline**: no credentials, no network. A backend that cannot be exercised without
//! either does not belong behind this port.
//!
//! What it pins, in the order the assertions run:
//!
//! 1. `create` starts an item at `Ready` with zero attempts and a backend-assigned id.
//! 2. The whole legal spine succeeds, and `Failed → Ready` increments `attempts`.
//! 3. **An illegal edge errors and writes nothing** — the item is byte-identical afterwards.
//! 4. `claim` is idempotent for the same assignee and conflicts for a different one.
//! 5. `list` honours the declared page bounds and the `state` filter.
//! 6. **The `depends_on` filter treats an item as blocked until every dependency is `done`** —
//!    no dependencies is trivially unblocked; an absent dependency never resolves (C-236).
//! 7. `comment` and `get` behave for present and absent ids.
//! 8. **`comments` reads back what `comment` wrote**, oldest first, and errors on an absent id
//!    (C-236).
//! 9. **A dispatch is recorded durably** — `runner` + `task_id` survive a fresh read, replace on a
//!    redispatch, and never move the state machine (A-130).

#![allow(dead_code)]

use std::sync::Arc;

use codewandler_flux_capabilities::WorkBoard;
use flux_datasource::board::{Item, ItemDraft, State};
use flux_datasource::live::{FilterValue, Filters, PageRequest};
use flux_runtime::ToolContext;

/// Drive one backend through the full port contract. Panics with the failing property.
pub async fn assert_work_board_contract(board: Arc<dyn WorkBoard>, ctx: &ToolContext) {
    let schema = board.schema();
    assert!(
        schema.default_page > 0 && schema.default_page <= schema.max_page,
        "page bounds must be usable: default {} max {}",
        schema.default_page,
        schema.max_page
    );

    created_items_start_ready(&board, ctx).await;
    the_legal_spine_succeeds_and_a_retry_increments_attempts(&board, ctx).await;
    an_illegal_transition_errors_and_writes_nothing(&board, ctx).await;
    claim_is_idempotent_for_one_assignee_and_conflicts_for_another(&board, ctx).await;
    list_honours_declared_page_bounds_and_the_state_filter(&board, ctx).await;
    the_depends_on_filter_treats_an_item_as_blocked_until_every_dependency_is_done(&board, ctx)
        .await;
    comment_and_get_agree_about_which_items_exist(&board, ctx).await;
    comments_read_back_what_comment_wrote(&board, ctx).await;
    a_dispatch_is_recorded_and_survives_a_fresh_read(&board, ctx).await;
}

fn draft(title: &str) -> ItemDraft {
    ItemDraft {
        title: title.to_string(),
        ..ItemDraft::default()
    }
}

async fn create(board: &Arc<dyn WorkBoard>, ctx: &ToolContext, title: &str) -> Item {
    board
        .create(ctx, draft(title))
        .await
        .unwrap_or_else(|error| panic!("create({title}) failed: {error}"))
}

async fn fetch(board: &Arc<dyn WorkBoard>, ctx: &ToolContext, id: &str) -> Item {
    board
        .get(ctx, id)
        .await
        .unwrap_or_else(|error| panic!("get({id}) failed: {error}"))
        .unwrap_or_else(|| panic!("get({id}) returned nothing"))
}

async fn created_items_start_ready(board: &Arc<dyn WorkBoard>, ctx: &ToolContext) {
    let item = create(board, ctx, "starts ready").await;
    assert_eq!(item.state, State::Ready, "a new item must start Ready");
    assert_eq!(item.attempts, 0, "a new item has not been attempted");
    assert!(!item.id.is_empty(), "the backend must assign an id");
    assert_eq!(item.title, "starts ready");
    assert_eq!(fetch(board, ctx, &item.id).await, item, "create then get");
}

async fn the_legal_spine_succeeds_and_a_retry_increments_attempts(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    let item = create(board, ctx, "walks the spine").await;

    // Lap 1 — a worker dies mid-flight. `InProgress -> Failed` is the edge the sweep journey
    // needs: a crashed worker never reaches `Review`.
    walk(
        board,
        ctx,
        &item.id,
        &[State::Claimed, State::InProgress],
        0,
    )
    .await;
    walk(board, ctx, &item.id, &[State::Failed], 0).await;
    let retried = board
        .transition(ctx, &item.id, State::Ready)
        .await
        .expect("Failed -> Ready is the retry edge");
    assert_eq!(retried.state, State::Ready);
    assert_eq!(retried.attempts, 1, "the retry edge increments attempts");
    assert_eq!(
        fetch(board, ctx, &item.id).await,
        retried,
        "the bump persists"
    );

    // Lap 2 — blocked, requeued, then a rejected review. Blocking is not a retry.
    walk(
        board,
        ctx,
        &item.id,
        &[
            State::Blocked,
            State::Ready,
            State::Claimed,
            State::InProgress,
            State::Review,
            State::Failed,
        ],
        1,
    )
    .await;
    assert_eq!(
        board
            .transition(ctx, &item.id, State::Ready)
            .await
            .unwrap()
            .attempts,
        2,
        "the second retry bumps again"
    );

    // Lap 3 — all the way to the terminal state.
    walk(
        board,
        ctx,
        &item.id,
        &[
            State::Claimed,
            State::InProgress,
            State::Review,
            State::Done,
        ],
        2,
    )
    .await;
}

/// Drive one item along a run of legal edges, asserting the attempt counter stays put.
async fn walk(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
    id: &str,
    edges: &[State],
    attempts: u32,
) {
    for to in edges {
        let moved = board
            .transition(ctx, id, *to)
            .await
            .unwrap_or_else(|error| panic!("legal edge to {to} rejected: {error}"));
        assert_eq!(moved.state, *to);
        assert_eq!(moved.attempts, attempts, "only a retry moves attempts");
    }
}

/// The property the story exists to pin: a rejected edge is **not a partial write**.
async fn an_illegal_transition_errors_and_writes_nothing(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    let item = create(board, ctx, "refuses illegal edges").await;
    let before = fetch(board, ctx, &item.id).await;

    // Ready may only go to Claimed or Blocked. Everything else must bounce.
    for to in [State::InProgress, State::Review, State::Done, State::Failed] {
        let error = board
            .transition(ctx, &item.id, to)
            .await
            .err()
            .unwrap_or_else(|| panic!("Ready -> {to} must be rejected"));
        let message = error.to_string();
        assert!(
            message.contains("ready") && message.contains(to.as_str()),
            "the error must name both endpoints, got: {message}"
        );
        assert_eq!(
            fetch(board, ctx, &item.id).await,
            before,
            "Ready -> {to} was rejected but the item changed"
        );
    }

    // A terminal state refuses everything, and still writes nothing.
    board
        .transition(ctx, &item.id, State::Claimed)
        .await
        .unwrap();
    board
        .transition(ctx, &item.id, State::InProgress)
        .await
        .unwrap();
    board
        .transition(ctx, &item.id, State::Review)
        .await
        .unwrap();
    let done = board.transition(ctx, &item.id, State::Done).await.unwrap();
    for to in State::ALL {
        assert!(
            board.transition(ctx, &item.id, to).await.is_err(),
            "Done -> {to} must be rejected"
        );
        assert_eq!(
            fetch(board, ctx, &item.id).await,
            done,
            "Done -> {to} was rejected but the item changed"
        );
    }
}

async fn claim_is_idempotent_for_one_assignee_and_conflicts_for_another(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    let item = create(board, ctx, "is claimed once").await;
    let claimed = board.claim(ctx, &item.id, "worker-a").await.unwrap();
    assert_eq!(claimed.state, State::Claimed);
    assert_eq!(claimed.assignee.as_deref(), Some("worker-a"));

    let again = board.claim(ctx, &item.id, "worker-a").await.unwrap();
    assert_eq!(again, claimed, "re-claiming by the same worker is a no-op");

    let conflict = board
        .claim(ctx, &item.id, "worker-b")
        .await
        .expect_err("a second worker must not steal a claimed item");
    assert!(
        conflict.to_string().contains("worker-a"),
        "the conflict must name the holder, got: {conflict}"
    );
    assert_eq!(
        fetch(board, ctx, &item.id).await,
        claimed,
        "a rejected claim writes nothing"
    );
}

async fn list_honours_declared_page_bounds_and_the_state_filter(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    let schema = board.schema();
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(create(board, ctx, &format!("listed {n}")).await.id);
    }
    board.claim(ctx, &ids[0], "worker-a").await.unwrap();

    let page = board
        .list(
            ctx,
            &Filters::new(),
            PageRequest {
                cursor: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
    assert!(
        page.rows.len() <= 2,
        "list must respect the requested limit"
    );

    let over = board
        .list(
            ctx,
            &Filters::new(),
            PageRequest {
                cursor: None,
                limit: schema.max_page,
            },
        )
        .await
        .unwrap();
    assert!(
        over.rows.len() <= schema.max_page,
        "list must respect the declared ceiling"
    );

    let mut claimed_only = Filters::new();
    claimed_only.insert("state", FilterValue::String(State::Claimed.to_string()));
    let filtered = board
        .list(
            ctx,
            &claimed_only,
            PageRequest {
                cursor: None,
                limit: schema.max_page,
            },
        )
        .await
        .unwrap();
    assert!(
        filtered
            .rows
            .iter()
            .all(|item| item.state == State::Claimed),
        "the state filter must be applied by the backend"
    );
    assert!(
        filtered.rows.iter().any(|item| item.id == ids[0]),
        "the claimed item must be in the claimed page"
    );
}

/// C-236: the `depends_on` filter makes "ready and unblocked" one query. An item is unblocked
/// exactly when every id in its `depends_on` is `done` — an item with no dependencies is trivially
/// unblocked, and an absent dependency is not `done`, so it keeps the item blocked.
async fn the_depends_on_filter_treats_an_item_as_blocked_until_every_dependency_is_done(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    async fn listed_ids(
        board: &Arc<dyn WorkBoard>,
        ctx: &ToolContext,
        depends_on: &str,
    ) -> Vec<String> {
        let mut filters = Filters::new();
        filters.insert(
            "depends_on",
            FilterValue::String(depends_on.to_string()),
        );
        board
            .list(
                ctx,
                &filters,
                PageRequest {
                    cursor: None,
                    limit: board.schema().max_page,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("list(depends_on={depends_on}) failed: {error}"))
            .rows
            .iter()
            .map(|item| item.id.clone())
            .collect()
    }

    let parent_a = create(board, ctx, "dependency a").await;
    let parent_b = create(board, ctx, "dependency b").await;
    let child = board
        .create(
            ctx,
            ItemDraft {
                title: "blocked on both".to_string(),
                depends_on: vec![parent_a.id.clone(), parent_b.id.clone()],
                ..ItemDraft::default()
            },
        )
        .await
        .expect("create child");
    let orphan = board
        .create(
            ctx,
            ItemDraft {
                title: "blocked on an absent id".to_string(),
                depends_on: vec!["definitely-not-an-item".to_string()],
                ..ItemDraft::default()
            },
        )
        .await
        .expect("create orphan");

    // Nothing is done: both parents (no dependencies) are unblocked; the child and the orphan
    // (an absent dependency is never `done`) are blocked.
    let unblocked = listed_ids(board, ctx, "satisfied").await;
    assert!(unblocked.contains(&parent_a.id), "{unblocked:?}");
    assert!(unblocked.contains(&parent_b.id), "{unblocked:?}");
    assert!(!unblocked.contains(&child.id), "{unblocked:?}");
    assert!(!unblocked.contains(&orphan.id), "{unblocked:?}");
    let blocked = listed_ids(board, ctx, "unsatisfied").await;
    assert!(blocked.contains(&child.id), "{blocked:?}");
    assert!(blocked.contains(&orphan.id), "{blocked:?}");
    assert!(!blocked.contains(&parent_a.id), "{blocked:?}");

    // One of two dependencies done: the child is still blocked.
    walk(
        board,
        ctx,
        &parent_a.id,
        &[State::Claimed, State::InProgress, State::Review, State::Done],
        0,
    )
    .await;
    assert!(
        !listed_ids(board, ctx, "satisfied").await.contains(&child.id),
        "a half-satisfied dependency set is still blocked"
    );

    // Every dependency done: the child unblocks.
    walk(
        board,
        ctx,
        &parent_b.id,
        &[State::Claimed, State::InProgress, State::Review, State::Done],
        0,
    )
    .await;
    let unblocked = listed_ids(board, ctx, "satisfied").await;
    assert!(
        unblocked.contains(&child.id),
        "all dependencies done unblocks the child: {unblocked:?}"
    );
    assert!(
        !unblocked.contains(&orphan.id),
        "an absent dependency never resolves: {unblocked:?}"
    );
}

async fn comment_and_get_agree_about_which_items_exist(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    let item = create(board, ctx, "takes comments").await;
    board
        .comment(ctx, &item.id, "a note the coordinator left")
        .await
        .expect("commenting on a present item succeeds");

    assert!(
        board
            .get(ctx, "definitely-not-an-item")
            .await
            .unwrap()
            .is_none(),
        "get on an absent id is None, not an error"
    );
    assert!(
        board
            .comment(ctx, "definitely-not-an-item", "note")
            .await
            .is_err(),
        "commenting on an absent id is an error"
    );
    assert!(
        board
            .transition(ctx, "definitely-not-an-item", State::Claimed)
            .await
            .is_err(),
        "transitioning an absent id is an error"
    );
    assert!(
        board
            .claim(ctx, "definitely-not-an-item", "w")
            .await
            .is_err(),
        "claiming an absent id is an error"
    );
}

/// C-236: the read half of `comment`. A sweep can see what a worker recorded — the notes come
/// back oldest-first, reading an absent id is the same error the write path reports, and the notes
/// never become part of the item's identity (a read changes nothing).
async fn comments_read_back_what_comment_wrote(board: &Arc<dyn WorkBoard>, ctx: &ToolContext) {
    let item = create(board, ctx, "takes readable notes").await;
    board
        .comment(ctx, &item.id, "worker started")
        .await
        .expect("first comment");
    board
        .comment(ctx, &item.id, "gate is green")
        .await
        .expect("second comment");

    assert_eq!(
        board
            .comments(ctx, &item.id)
            .await
            .expect("reading comments on a present item succeeds"),
        vec!["worker started".to_string(), "gate is green".to_string()],
        "the notes come back oldest-first, exactly as written"
    );
    assert_eq!(
        fetch(board, ctx, &item.id).await,
        item,
        "comments sit beside the item; reading them changes nothing"
    );

    let absent = board
        .comments(ctx, "definitely-not-an-item")
        .await
        .expect_err("reading comments on an absent id is an error")
        .to_string();
    assert!(
        absent.contains("definitely-not-an-item"),
        "the error names the id, got: {absent}"
    );
}

/// A-130: the property that makes `docs/designs/fleet-coordinator.md` §5 true rather than
/// aspirational. A backend that cannot answer "who is running this, under what handle" is not a run
/// registry, and a coordinator restarted over it recovers nothing.
async fn a_dispatch_is_recorded_and_survives_a_fresh_read(
    board: &Arc<dyn WorkBoard>,
    ctx: &ToolContext,
) {
    let item = create(board, ctx, "dispatched to a worker").await;
    board
        .claim(ctx, &item.id, "worker-1")
        .await
        .expect("claiming before dispatch");

    let recorded = board
        .record_dispatch(ctx, &item.id, "https://worker-1.internal:8787", "t_1")
        .await
        .unwrap_or_else(|error| panic!("record_dispatch failed: {error}"));
    assert_eq!(
        recorded.runner.as_deref(),
        Some("https://worker-1.internal:8787")
    );
    assert_eq!(recorded.task_id.as_deref(), Some("t_1"));

    // The only assertion that matters: a reader holding nothing but the board sees it.
    let fresh = fetch(board, ctx, &item.id).await;
    assert_eq!(
        fresh.runner.as_deref(),
        Some("https://worker-1.internal:8787"),
        "the runner address must be durable, not returned-only"
    );
    assert_eq!(fresh.task_id.as_deref(), Some("t_1"));
    assert_eq!(
        fresh.state,
        State::Claimed,
        "recording a dispatch is a field write; the state machine is `transition`'s job alone"
    );
    assert_eq!(fresh.assignee.as_deref(), Some("worker-1"));
    assert_eq!(fresh.attempts, 0, "recording a dispatch is not a retry");

    // A retry dispatches the same item again, so the record must be replaceable rather than
    // append-only — a stale task id would send the sweep after a run that no longer exists.
    board
        .record_dispatch(ctx, &item.id, "https://worker-2.internal:8787", "t_2")
        .await
        .expect("re-recording a redispatched item");
    let replaced = fetch(board, ctx, &item.id).await;
    assert_eq!(
        replaced.runner.as_deref(),
        Some("https://worker-2.internal:8787")
    );
    assert_eq!(replaced.task_id.as_deref(), Some("t_2"));

    assert!(
        board
            .record_dispatch(ctx, "definitely-not-an-item", "https://w.example", "t_3")
            .await
            .is_err(),
        "recording a dispatch against an absent id is an error"
    );
}
