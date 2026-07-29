//! [`MarkdownBoard`] against the shared [`WorkBoard`] contract suite, plus the properties a
//! file-backed board has to earn that an in-memory one gets for free (A-114).
//!
//! 1. **Two workers claiming the same item resolve to exactly one winner.** The loser is told it
//!    lost; it never clobbers the winner's file.
//! 2. **Two workers touching different items never contend**, because a mutation writes exactly one
//!    file — its own item — and the derived index is refreshed only on read.
//! 3. **A refused write never starts**, and a committed write replaces the file by rename rather
//!    than truncating it in place.
//! 4. **A dispatch record outlives the board that wrote it** (A-130). The shared suite drives the
//!    write and the read-back through one `Arc<dyn WorkBoard>`, so it cannot tell durable bytes from
//!    an in-memory cache; this backend is the first that can, by dropping the writer entirely and
//!    reopening the directory. That is what makes the board a restart-survivable run registry.

mod board_contract;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use codewandler_flux_capabilities::{MarkdownBoard, WorkBoard};
use flux_datasource::board::{ItemDraft, State};
use flux_datasource::live::{FilterValue, Filters, PageRequest};
use flux_runtime::ToolContext;
use flux_system::{System, Workspace};

/// A fresh, uniquely-named directory. One helper for every fixture root in this file, so a
/// concurrent test never adopts another's board (C-209's rule, applied here because this backend
/// *is* the filesystem).
fn fixture_dir(prefix: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "flux-{prefix}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A board plus the coordinator context it is driven from. The two are rooted **separately** on
/// purpose: nothing in this backend may reach for `ctx`'s workspace.
fn board_and_ctx(prefix: &str) -> (Arc<MarkdownBoard>, ToolContext, PathBuf) {
    let root = fixture_dir(prefix);
    let board = Arc::new(MarkdownBoard::new(&root).unwrap());
    (board, coordinator_ctx(), root)
}

/// A coordinator context rooted somewhere else entirely.
fn coordinator_ctx() -> ToolContext {
    let cwd = fixture_dir("markdown-board-cwd");
    ToolContext::new(Arc::new(System::new(Workspace::new(&cwd).unwrap())))
}

fn draft(title: &str) -> ItemDraft {
    ItemDraft {
        title: title.into(),
        ..ItemDraft::default()
    }
}

/// Every entry under `items/`, so a test can assert nothing but item files survives a write.
fn item_dir_entries(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join("items"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn markdown_board_satisfies_the_shared_work_board_contract() {
    let (board, ctx, _root) = board_and_ctx("markdown-board-contract");
    board_contract::assert_work_board_contract(board, &ctx).await;
}

/// **Failing-first (story Acceptance 3).** Eight workers race for one item. Exactly one may win;
/// every loser must get a conflict error and the file must still be a single, parseable item owned
/// by the winner — not a clobbered or interleaved one.
///
/// The barrier is what makes this a race rather than a sequence: all eight tasks are released into
/// `claim` at once, on a multi-threaded runtime, and `claim` holds no `.await` inside, so they are
/// genuinely concurrent on distinct OS threads.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_claims_on_one_item_resolve_to_exactly_one_winner() {
    const WORKERS: usize = 8;
    let (board, ctx, root) = board_and_ctx("markdown-board-claim-race");
    let item = board.create(&ctx, draft("contested")).await.unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(WORKERS));
    let mut racers = Vec::new();
    for n in 0..WORKERS {
        let board = board.clone();
        let barrier = barrier.clone();
        let id = item.id.clone();
        let ctx = coordinator_ctx();
        racers.push(tokio::spawn(async move {
            barrier.wait().await;
            let assignee = format!("worker-{n}");
            board
                .claim(&ctx, &id, &assignee)
                .await
                .map(|item| item.assignee.unwrap_or_default())
        }));
    }

    let mut winners = Vec::new();
    let mut conflicts = Vec::new();
    for racer in racers {
        match racer.await.unwrap() {
            Ok(holder) => winners.push(holder),
            Err(error) => conflicts.push(error.to_string()),
        }
    }
    assert_eq!(
        winners.len(),
        1,
        "exactly one claim may win; winners={winners:?} conflicts={conflicts:?}"
    );
    assert_eq!(conflicts.len(), WORKERS - 1);
    for conflict in &conflicts {
        assert!(
            conflict.contains("already claimed by") || conflict.contains("another worker"),
            "a loser must get a conflict error, got: {conflict}"
        );
    }

    // The file agrees with the one winner, and is still exactly one readable item.
    let stored = board.get(&ctx, &item.id).await.unwrap().unwrap();
    assert_eq!(stored.state, State::Claimed);
    assert_eq!(stored.assignee.as_deref(), Some(winners[0].as_str()));
    assert_eq!(
        item_dir_entries(&root),
        vec![format!("{}.md", item.id)],
        "a lost race must not leave a staging file behind"
    );
}

/// **Failing-first (story Acceptance 4).** Sixteen workers each drive their *own* item. None may
/// contend, because a mutation writes one file — its own — and touches no shared one.
///
/// The second half is the index: it is not written by any mutation, and when `list` finally
/// derives it, it holds every item. A stale or missing index is therefore never authoritative and
/// never loses an item.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_writes_to_different_items_never_contend_and_the_index_is_derived_on_read() {
    const WORKERS: usize = 16;
    let (board, ctx, root) = board_and_ctx("markdown-board-parallel");
    let mut ids = Vec::new();
    for n in 0..WORKERS {
        ids.push(
            board
                .create(&ctx, draft(&format!("item {n}")))
                .await
                .unwrap()
                .id,
        );
    }

    let barrier = Arc::new(tokio::sync::Barrier::new(WORKERS));
    let mut workers = Vec::new();
    for (n, id) in ids.iter().cloned().enumerate() {
        let board = board.clone();
        let barrier = barrier.clone();
        let ctx = coordinator_ctx();
        workers.push(tokio::spawn(async move {
            barrier.wait().await;
            board.claim(&ctx, &id, &format!("worker-{n}")).await?;
            board.transition(&ctx, &id, State::InProgress).await?;
            board.comment(&ctx, &id, "started").await?;
            board.transition(&ctx, &id, State::Review).await
        }));
    }
    for (n, worker) in workers.into_iter().enumerate() {
        let item = worker
            .await
            .unwrap()
            .unwrap_or_else(|error| panic!("worker {n} contended on its own item: {error}"));
        assert_eq!(item.state, State::Review);
    }

    // No mutation wrote the index — that is what "no shared mutable file on the write path" means.
    assert!(
        !root.join("index.md").exists(),
        "mutations must not touch the derived index"
    );

    let page = board
        .list(
            &ctx,
            &Filters::new(),
            PageRequest {
                cursor: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        page.rows.len(),
        WORKERS,
        "every item survived the parallel run"
    );
    let index = std::fs::read_to_string(root.join("index.md")).unwrap();
    for id in &ids {
        assert!(index.contains(id.as_str()), "{id} missing from the index");
    }
}

/// The index is an **output**, never an input: reads answer from the item files, so a stale index
/// can neither hide a real item nor conjure a phantom one, and a missing one loses nothing.
#[tokio::test]
async fn a_stale_or_missing_index_is_never_authoritative() {
    let (board, ctx, root) = board_and_ctx("markdown-board-index");
    let mut ids = Vec::new();
    for n in 0..3 {
        ids.push(
            board
                .create(&ctx, draft(&format!("item {n}")))
                .await
                .unwrap()
                .id,
        );
    }
    let all = |board: Arc<MarkdownBoard>, ctx: ToolContext| async move {
        let page = board
            .list(
                &ctx,
                &Filters::new(),
                PageRequest {
                    cursor: None,
                    limit: 100,
                },
            )
            .await
            .unwrap();
        page.rows
            .into_iter()
            .map(|item| item.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(all(board.clone(), coordinator_ctx()).await, ids);

    // A hand-mangled index that drops two real items and invents one.
    std::fs::write(
        root.join("index.md"),
        "# Board index\n\n| Item | State |\n| --- | --- |\n| phantom-9999 | done |\n",
    )
    .unwrap();
    assert_eq!(
        all(board.clone(), coordinator_ctx()).await,
        ids,
        "the item files win over the index"
    );
    assert!(board.get(&ctx, "phantom-9999").await.unwrap().is_none());
    let rewritten = std::fs::read_to_string(root.join("index.md")).unwrap();
    assert!(!rewritten.contains("phantom-9999"), "{rewritten}");
    assert!(rewritten.contains("Never authoritative"), "{rewritten}");

    // Deleting it loses nothing, and the next read puts it back.
    std::fs::remove_file(root.join("index.md")).unwrap();
    assert_eq!(all(board.clone(), coordinator_ctx()).await, ids);
    assert!(root.join("index.md").exists());
}

/// A rejected edge leaves the item file **byte-identical** and the directory free of staging
/// debris — the A-113 invariant, restated against real bytes rather than a parsed struct.
#[tokio::test]
async fn an_illegal_transition_leaves_the_item_file_byte_identical() {
    let (board, ctx, root) = board_and_ctx("markdown-board-illegal");
    let item = board
        .create(
            &ctx,
            ItemDraft {
                title: "unchanged by a refusal".into(),
                assignee: Some("worker-a".into()),
                depends_on: vec!["item-0001".into()],
                repo: Some("codewandler/flux".into()),
            },
        )
        .await
        .unwrap();
    let path = root.join("items").join(format!("{}.md", item.id));
    let before = std::fs::read(&path).unwrap();
    let before_inode = std::fs::metadata(&path).unwrap().ino();

    for to in [State::InProgress, State::Review, State::Done, State::Failed] {
        let error = board
            .transition(&ctx, &item.id, to)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("ready") && error.contains(to.as_str()),
            "the refusal must name the edge, got: {error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "ready -> {to} wrote");
        assert_eq!(
            std::fs::metadata(&path).unwrap().ino(),
            before_inode,
            "ready -> {to} replaced the file"
        );
    }
    assert_eq!(item_dir_entries(&root), vec![format!("{}.md", item.id)]);
    // Nothing was wedged: the next legal edge still commits.
    assert_eq!(
        board
            .transition(&ctx, &item.id, State::Claimed)
            .await
            .unwrap()
            .state,
        State::Claimed
    );
}

/// A committed write **replaces** the file rather than truncating it in place. The inode changing
/// is the observable signature of write-then-rename, and it is what makes an interrupted write
/// leave either the old item or the new one — never a half-written one. A reader holding the old
/// file open sees the whole old item throughout.
#[tokio::test]
async fn a_committed_write_replaces_the_item_file_instead_of_truncating_it() {
    use std::io::Read as _;

    let (board, ctx, root) = board_and_ctx("markdown-board-atomic");
    let item = board
        .create(&ctx, draft("replaced in place"))
        .await
        .unwrap();
    let path = root.join("items").join(format!("{}.md", item.id));

    let before = std::fs::read_to_string(&path).unwrap();
    let before_inode = std::fs::metadata(&path).unwrap().ino();
    let mut open_before = std::fs::File::open(&path).unwrap();

    board.claim(&ctx, &item.id, "worker-a").await.unwrap();

    assert_ne!(
        std::fs::metadata(&path).unwrap().ino(),
        before_inode,
        "a committed write must rename a new file over the old one"
    );
    let mut through_old_handle = String::new();
    open_before.read_to_string(&mut through_old_handle).unwrap();
    assert_eq!(
        through_old_handle, before,
        "the old item stayed whole for anyone already reading it"
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(after.contains("worker-a"), "{after}");
    assert!(after.starts_with("+++\n"), "{after}");
    assert_eq!(item_dir_entries(&root), vec![format!("{}.md", item.id)]);
}

/// The board root is the host's choice and need not be the coordinator's cwd — a `System`
/// construction detail, with no `WorkspaceContext` change (design §7). Nothing may land under the
/// context's workspace.
#[tokio::test]
async fn the_board_root_is_configurable_and_independent_of_the_coordinator_cwd() {
    let cwd = fixture_dir("markdown-board-cwd-only");
    let elsewhere = fixture_dir("markdown-board-elsewhere");
    let coordinator = System::new(Workspace::new(&cwd).unwrap());
    let ctx = ToolContext::new(Arc::new(coordinator));

    // Constructed from the coordinator's own `System`, so the board inherits its access posture.
    let board =
        MarkdownBoard::rooted_in(&System::new(Workspace::new(&cwd).unwrap()), &elsewhere).unwrap();
    assert_eq!(board.root(), elsewhere.canonicalize().unwrap());

    let item = board.create(&ctx, draft("lives elsewhere")).await.unwrap();
    assert!(elsewhere
        .join("items")
        .join(format!("{}.md", item.id))
        .exists());
    assert!(
        std::fs::read_dir(&cwd).unwrap().next().is_none(),
        "the coordinator's workspace must stay untouched"
    );
    assert_eq!(board.get(&ctx, &item.id).await.unwrap(), Some(item));
}

/// A hand-authored item file — the whole point of a markdown board — is a first-class item, and a
/// hand-broken one is a loud error rather than a silently missing row.
#[tokio::test]
async fn a_hand_written_item_file_is_read_and_a_broken_one_is_reported() {
    let (board, ctx, root) = board_and_ctx("markdown-board-handwritten");
    std::fs::create_dir_all(root.join("items")).unwrap();
    std::fs::write(
        root.join("items/A-114.md"),
        "+++\nid = \"A-114\"\ntitle = \"written by a human\"\nstate = \"blocked\"\nattempts = 3\n+++\n\n# written by a human\n",
    )
    .unwrap();

    let item = board.get(&ctx, "A-114").await.unwrap().unwrap();
    assert_eq!(item.state, State::Blocked);
    assert_eq!(item.attempts, 3);
    assert_eq!(item.title, "written by a human");

    let mut blocked = Filters::new();
    blocked.insert("state", FilterValue::String("blocked".into()));
    let page = board
        .list(
            &ctx,
            &blocked,
            PageRequest {
                cursor: None,
                limit: 100,
            },
        )
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 1);

    // `create` must not collide with a hand-chosen id, and a broken file is never skipped quietly.
    board.create(&ctx, draft("minted")).await.unwrap();
    std::fs::write(root.join("items/A-115.md"), "not a board item at all\n").unwrap();
    let error = board
        .list(
            &ctx,
            &Filters::new(),
            PageRequest {
                cursor: None,
                limit: 100,
            },
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("A-115") && error.contains("malformed"),
        "{error}"
    );
}

/// **The assertion nothing else in-tree can make.** A dispatch record is durable across board
/// *instances*, not merely across a `get` on the board that wrote it.
///
/// The shared suite cannot express this: it takes one `Arc<dyn WorkBoard>` and drives both the write
/// and the read-back through it, so a backend that cached the record in memory would satisfy
/// `a_dispatch_is_recorded_and_survives_a_fresh_read` while a real restart recovered nothing.
/// `MemoryBoard` cannot close the gap either — its storage *is* the instance. This backend is the
/// first whose durability outlives the object, so this is the first place the design's headline
/// claim ("the board is the run registry", hence restart → sweep → re-derive) is actually testable.
///
/// The writer instance is therefore **dropped** before the reader is opened: the only thing the two
/// share is the directory.
#[tokio::test]
async fn a_recorded_dispatch_survives_dropping_the_board_that_wrote_it() {
    let root = fixture_dir("markdown-board-dispatch");
    let ctx = coordinator_ctx();

    // --- The coordinator that dispatches, and then dies. ---
    let (id, path, before, before_inode, mut open_across_the_write) = {
        let board = MarkdownBoard::new(&root).unwrap();
        let item = board
            .create(&ctx, draft("handed to a worker"))
            .await
            .unwrap();
        board.claim(&ctx, &item.id, "worker-1").await.unwrap();

        let path = root.join("items").join(format!("{}.md", item.id));
        let before = std::fs::read_to_string(&path).unwrap();
        let before_inode = std::fs::metadata(&path).unwrap().ino();
        // Opened *before* the record and read *after* it: a concurrent reader must never observe a
        // torn file, which is what committing through the reserved-update primitive buys.
        let open_across_the_write = std::fs::File::open(&path).unwrap();

        board
            .record_dispatch(&ctx, &item.id, "https://worker-1.internal:8787", "t_1")
            .await
            .unwrap();
        (item.id, path, before, before_inode, open_across_the_write)
    };
    // Nothing of the writer survives but the bytes under `root`.

    assert_ne!(
        std::fs::metadata(&path).unwrap().ino(),
        before_inode,
        "the record must be committed by rename, like every other write on this backend"
    );
    let mut through_old_handle = String::new();
    std::io::Read::read_to_string(&mut open_across_the_write, &mut through_old_handle).unwrap();
    assert_eq!(
        through_old_handle, before,
        "a reader holding the file across the record saw a whole item, never a torn one"
    );

    // --- The restart: a second, independent board over the same directory. ---
    let restarted = MarkdownBoard::new(&root).unwrap();
    let recovered = restarted
        .get(&coordinator_ctx(), &id)
        .await
        .unwrap()
        .expect("the restarted board must still see the item");
    assert_eq!(
        recovered.runner.as_deref(),
        Some("https://worker-1.internal:8787"),
        "a restarted coordinator must recover the run, not only the item's state"
    );
    assert_eq!(
        recovered.task_id.as_deref(),
        Some("t_1"),
        "the worker-minted handle is what the sweep needs; losing it orphans the run"
    );
    // Recording a dispatch is a field write, not an edge: nothing else moved.
    assert_eq!(recovered.state, State::Claimed);
    assert_eq!(recovered.assignee.as_deref(), Some("worker-1"));
    assert_eq!(recovered.attempts, 0);

    // Replacing, not appending: a retried item is dispatched again, and a stale task id would send
    // the sweep after a run that no longer exists.
    restarted
        .record_dispatch(&ctx, &id, "https://worker-2.internal:8787", "t_2")
        .await
        .unwrap();
    let stored = std::fs::read_to_string(&path).unwrap();
    assert!(stored.contains("worker-2.internal:8787"), "{stored}");
    assert!(stored.contains("t_2"), "{stored}");
    assert!(
        !stored.contains("worker-1.internal") && !stored.contains("t_1"),
        "a stale dispatch record must not survive a re-dispatch: {stored}"
    );
    assert_eq!(
        item_dir_entries(&root),
        vec![format!("{id}.md")],
        "the record left no staging debris behind"
    );
}
