//! [`MemoryBoard`] against the shared [`WorkBoard`] contract suite (A-113).
//!
//! `MemoryBoard` is the offline test double — the reason the port's contract is runnable with no
//! credentials and no network, the way `MemoryBackend` is for the index-shaped datasource.

mod board_contract;

use std::sync::Arc;

use codewandler_flux_capabilities::{MemoryBoard, WorkBoard};
use flux_datasource::board::{ItemDraft, State};
use flux_datasource::live::{FilterValue, Filters, PageRequest};
use flux_runtime::ToolContext;
use flux_system::{System, Workspace};

fn ctx() -> ToolContext {
    let root = std::env::temp_dir().join(format!(
        "flux-memory-board-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    std::fs::create_dir_all(&root).unwrap();
    ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())))
}

#[tokio::test]
async fn memory_board_satisfies_the_shared_work_board_contract() {
    let board: Arc<dyn WorkBoard> = Arc::new(MemoryBoard::new());
    board_contract::assert_work_board_contract(board, &ctx()).await;
}

/// The story's first failing-first property, stated directly against the backend: a rejected edge
/// leaves the item **byte-identical**, not merely "in the same state".
#[tokio::test]
async fn an_illegal_transition_leaves_the_item_byte_identical() {
    let board = MemoryBoard::new();
    let ctx = ctx();
    let item = board
        .create(
            &ctx,
            ItemDraft {
                title: "unchanged by a refusal".into(),
                assignee: Some("worker-a".into()),
                depends_on: vec!["OTHER-1".into()],
                repo: Some("codewandler/flux".into()),
            },
        )
        .await
        .unwrap();

    let before = serde_json::to_vec(&board.get(&ctx, &item.id).await.unwrap().unwrap()).unwrap();
    let error = board
        .transition(&ctx, &item.id, State::Done)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("`ready` -> `done`"),
        "the refusal must name the edge, got: {error}"
    );
    let after = serde_json::to_vec(&board.get(&ctx, &item.id).await.unwrap().unwrap()).unwrap();
    assert_eq!(
        before, after,
        "an illegal transition must perform no write at all"
    );
}

#[tokio::test]
async fn a_retry_is_the_only_edge_that_moves_the_attempt_counter() {
    let board = MemoryBoard::new();
    let ctx = ctx();
    let item = board
        .create(
            &ctx,
            ItemDraft {
                title: "retried".into(),
                ..ItemDraft::default()
            },
        )
        .await
        .unwrap();

    for to in [
        State::Claimed,
        State::InProgress,
        State::Review,
        State::Failed,
    ] {
        assert_eq!(
            board.transition(&ctx, &item.id, to).await.unwrap().attempts,
            0
        );
    }
    assert_eq!(
        board
            .transition(&ctx, &item.id, State::Ready)
            .await
            .unwrap()
            .attempts,
        1
    );
    // A second lap adds exactly one more.
    for to in [
        State::Claimed,
        State::InProgress,
        State::Review,
        State::Failed,
    ] {
        board.transition(&ctx, &item.id, to).await.unwrap();
    }
    assert_eq!(
        board
            .transition(&ctx, &item.id, State::Ready)
            .await
            .unwrap()
            .attempts,
        2
    );
}

#[tokio::test]
async fn list_pages_deterministically_and_filters_by_state() {
    let board = MemoryBoard::new();
    let ctx = ctx();
    let mut ids = Vec::new();
    for n in 0..5 {
        ids.push(
            board
                .create(
                    &ctx,
                    ItemDraft {
                        title: format!("item {n}"),
                        ..ItemDraft::default()
                    },
                )
                .await
                .unwrap()
                .id,
        );
    }
    board.claim(&ctx, &ids[1], "worker-a").await.unwrap();

    let first = board
        .list(
            &ctx,
            &Filters::new(),
            PageRequest {
                cursor: None,
                limit: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        first.rows.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        ids[..2]
    );
    let cursor = first.next.expect("a partial page carries a cursor");
    let second = board
        .list(
            &ctx,
            &Filters::new(),
            PageRequest {
                cursor: Some(cursor),
                limit: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        second.rows.iter().map(|i| i.id.clone()).collect::<Vec<_>>(),
        ids[2..4]
    );

    let mut ready = Filters::new();
    ready.insert("state", FilterValue::String("ready".into()));
    let ready_page = board
        .list(
            &ctx,
            &ready,
            PageRequest {
                cursor: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(ready_page.rows.len(), 4, "the claimed item is filtered out");
    assert!(ready_page.rows.iter().all(|i| i.state == State::Ready));
    assert!(ready_page.next.is_none(), "an exhausted page has no cursor");
}
