//! [`MemoryBoard`] — the in-process [`WorkBoard`] test double (A-113).
//!
//! The board-shaped sibling of [`MemoryBackend`](super::MemoryBackend): no persistence, no network,
//! no credentials. It exists so the port's shared contract suite is runnable offline, which
//! AGENTS.md requires of every test, and so `MarkdownBoard` / `JiraBoard` / `GitlabBoard` have a
//! reference implementation of the semantics they must reproduce.
//!
//! It is also the place those semantics are written down once:
//!
//! * `transition` calls [`validate_transition`] **before** touching the entry, so a refused edge
//!   cannot leave a partial write behind — the check is not "then roll back", it is "never start".
//! * [`Item::attempts`] moves, and the run identity (`runner`/`task_id`) is dropped, on exactly the
//!   edges [`is_retry`] names and no others — while `assignee` survives all of them.
//! * `claim` is idempotent for the current holder and a conflict for anyone else; `reassign` is the
//!   deliberate exception that moves an item off a holder that is gone.

use std::sync::Mutex;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_datasource::board::{
    is_retry, validate_transition, BoardSchema, DependencyMatch, Item, ItemDraft, State,
    DEPENDS_ON_FILTER,
};
use flux_datasource::live::{FilterValue, Filters, Page, PageRequest, Reference};
use flux_runtime::ToolContext;

use super::board::WorkBoard;

/// One stored item plus the notes left against it.
///
/// Comments sit *beside* the [`Item`] rather than inside it: the contract suite asserts that a
/// refused transition leaves the item byte-identical, and an append-only note log is not part of
/// that identity.
#[derive(Debug, Clone)]
struct Entry {
    item: Item,
    comments: Vec<String>,
}

/// An in-memory work board. Cheap, dependency-free, and the default double for board tests.
#[derive(Default)]
pub struct MemoryBoard {
    entries: Mutex<Vec<Entry>>,
}

impl MemoryBoard {
    /// An empty board.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many items the board holds.
    pub fn len(&self) -> usize {
        self.locked().len()
    }

    /// Whether the board holds no items.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Vec<Entry>> {
        self.entries.lock().expect("board entries poisoned")
    }
}

/// The error every operation returns for an id the board does not hold.
fn absent(id: &str) -> Error {
    Error::Other(format!("work board: no item `{id}`"))
}

impl MemoryBoard {
    /// Locate one entry by id, or fail with the shared "absent" message.
    fn index_of(entries: &[Entry], id: &str) -> Result<usize> {
        entries
            .iter()
            .position(|entry| entry.item.id == id)
            .ok_or_else(|| absent(id))
    }
}

#[async_trait]
impl WorkBoard for MemoryBoard {
    fn schema(&self) -> BoardSchema {
        BoardSchema::default()
    }

    async fn list(
        &self,
        _ctx: &ToolContext,
        filters: &Filters,
        page: PageRequest,
    ) -> Result<Page<Item>> {
        // The host validated `state` against the closed `State` set before we got here, so an
        // unparseable value is a host bug rather than caller input — fail rather than silently
        // returning everything.
        let wanted = match filters.get("state") {
            Some(FilterValue::String(value)) => Some(State::parse(value).ok_or_else(|| {
                Error::Other(format!("work board: unknown state filter `{value}`"))
            })?),
            Some(other) => {
                return Err(Error::Other(format!(
                    "work board: state filter must be a string, got {other:?}"
                )))
            }
            None => None,
        };
        // The same reasoning for `depends_on` (C-236): the host validated it against the closed
        // `DependencyMatch` set, and the rule itself lives in `flux-datasource` so both backends
        // apply one definition of "unblocked".
        let dependencies = match filters.get(DEPENDS_ON_FILTER) {
            Some(FilterValue::String(value)) => {
                Some(DependencyMatch::parse(value).ok_or_else(|| {
                    Error::Other(format!("work board: unknown depends_on filter `{value}`"))
                })?)
            }
            Some(other) => {
                return Err(Error::Other(format!(
                    "work board: depends_on filter must be a string, got {other:?}"
                )))
            }
            None => None,
        };

        // Insertion order is the board's order: deterministic, and it makes the opaque cursor a
        // plain offset.
        let offset: usize = match &page.cursor {
            Some(cursor) => cursor
                .parse()
                .map_err(|_| Error::Other(format!("work board: bad cursor `{cursor}`")))?,
            None => 0,
        };

        let entries = self.locked();
        // Dependencies resolve against the whole board, not the filtered page — an item's blockers
        // are usually in a state the caller is not asking for.
        let state_of = |id: &str| {
            entries
                .iter()
                .find(|entry| entry.item.id == id)
                .map(|entry| entry.item.state)
        };
        let matching: Vec<Item> = entries
            .iter()
            .map(|entry| entry.item.clone())
            .filter(|item| wanted.is_none_or(|state| item.state == state))
            .filter(|item| dependencies.is_none_or(|match_| match_.matches(item, state_of)))
            .collect();
        let rows: Vec<Item> = matching
            .iter()
            .skip(offset)
            .take(page.limit)
            .cloned()
            .collect();
        let consumed = offset + rows.len();
        Ok(Page {
            next: (consumed < matching.len()).then(|| consumed.to_string()),
            rows,
        })
    }

    async fn get(&self, _ctx: &ToolContext, id: &str) -> Result<Option<Item>> {
        Ok(self
            .locked()
            .iter()
            .find(|entry| entry.item.id == id)
            .map(|entry| entry.item.clone()))
    }

    async fn create(&self, _ctx: &ToolContext, draft: ItemDraft) -> Result<Item> {
        let mut entries = self.locked();
        let item = Item {
            id: format!("item-{}", entries.len() + 1),
            title: draft.title,
            state: State::Ready,
            assignee: draft.assignee,
            runner: None,
            task_id: None,
            depends_on: draft.depends_on,
            repo: draft.repo,
            attempts: 0,
            evidence: Vec::new(),
        };
        entries.push(Entry {
            item: item.clone(),
            comments: Vec::new(),
        });
        Ok(item)
    }

    async fn transition(&self, _ctx: &ToolContext, id: &str, to: State) -> Result<Item> {
        let mut entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        let from = entries[index].item.state;
        // Validate first, mutate second. An illegal edge returns here having written nothing.
        validate_transition(from, to).map_err(|error| Error::Other(error.to_string()))?;

        let item = &mut entries[index].item;
        item.state = to;
        if is_retry(from, to) {
            item.attempts += 1;
            // The run that was executing this item is over, so the item must not still name it — a
            // sweep reading a stale `task_id` chases a process that no longer exists (C-240).
            // `assignee` is deliberately untouched: the holder outlives one run.
            item.runner = None;
            item.task_id = None;
        }
        Ok(item.clone())
    }

    async fn claim(&self, _ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item> {
        let mut entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        let item = &mut entries[index].item;

        if item.state == State::Claimed {
            return match item.assignee.as_deref() {
                // Re-claiming what you already hold changes nothing.
                Some(holder) if holder == assignee => Ok(item.clone()),
                Some(holder) => Err(Error::Other(format!(
                    "work board: item `{id}` is already claimed by `{holder}`, not `{assignee}`"
                ))),
                // Reachable: `transition(id, Claimed)` moves the state without naming an owner.
                // Adopting it is the useful behaviour and takes nothing from anyone.
                None => {
                    item.assignee = Some(assignee.to_string());
                    Ok(item.clone())
                }
            };
        }

        validate_transition(item.state, State::Claimed)
            .map_err(|error| Error::Other(error.to_string()))?;
        item.state = State::Claimed;
        item.assignee = Some(assignee.to_string());
        Ok(item.clone())
    }

    async fn record_dispatch(
        &self,
        _ctx: &ToolContext,
        id: &str,
        runner: &str,
        task_id: &str,
    ) -> Result<Item> {
        let mut entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        let item = &mut entries[index].item;
        // Replace rather than append: a retried item is dispatched again, and a stale task id would
        // send the sweep after a run that no longer exists. Nothing else on the item moves — the
        // state machine has exactly one entry point and this is not it.
        item.runner = Some(runner.to_string());
        item.task_id = Some(task_id.to_string());
        Ok(item.clone())
    }

    async fn reassign(&self, _ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item> {
        let mut entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        let item = &mut entries[index].item;
        // Forcible by design: `claim` protects two live workers from each other, and this is the
        // case where the holder is dead. The old holder's run goes with it, for the same reason a
        // retry drops it.
        item.assignee = Some(assignee.to_string());
        item.runner = None;
        item.task_id = None;
        Ok(item.clone())
    }

    async fn record_evidence(
        &self,
        _ctx: &ToolContext,
        id: &str,
        reference: Reference,
    ) -> Result<Item> {
        let mut entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        let item = &mut entries[index].item;
        // Appending, not replacing — an item legitimately carries a commit *and* the review that
        // accepted it. A reference already present is dropped: a rework records the same commit
        // again, and a duplicate carries no information.
        if !item.evidence.contains(&reference) {
            item.evidence.push(reference);
        }
        Ok(item.clone())
    }

    async fn comment(&self, _ctx: &ToolContext, id: &str, text: &str) -> Result<()> {
        let mut entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        entries[index].comments.push(text.to_string());
        Ok(())
    }

    async fn comments(&self, _ctx: &ToolContext, id: &str) -> Result<Vec<String>> {
        let entries = self.locked();
        let index = Self::index_of(&entries, id)?;
        Ok(entries[index].comments.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use flux_system::{System, Workspace};

    use super::*;

    fn ctx() -> ToolContext {
        let root = std::env::temp_dir().join(format!(
            "flux-memory-board-unit-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&root).unwrap();
        ToolContext::new(Arc::new(System::new(Workspace::new(&root).unwrap())))
    }

    async fn seeded(board: &MemoryBoard, ctx: &ToolContext, title: &str) -> Item {
        board
            .create(
                ctx,
                ItemDraft {
                    title: title.into(),
                    ..ItemDraft::default()
                },
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn comments_accumulate_beside_the_item_and_never_alter_it() {
        let board = MemoryBoard::new();
        let ctx = ctx();
        let item = seeded(&board, &ctx, "commented").await;

        board.comment(&ctx, &item.id, "first").await.unwrap();
        board.comment(&ctx, &item.id, "second").await.unwrap();
        assert_eq!(
            board.comments(&ctx, &item.id).await.unwrap(),
            vec!["first", "second"]
        );
        assert_eq!(board.get(&ctx, &item.id).await.unwrap().unwrap(), item);
        // C-236: the read path and the write path agree about which items exist.
        assert!(board.comments(&ctx, "absent").await.is_err());
        assert!(board.comment(&ctx, "absent", "x").await.is_err());
    }

    #[tokio::test]
    async fn claim_adopts_an_item_transitioned_into_claimed_without_an_owner() {
        let board = MemoryBoard::new();
        let ctx = ctx();
        let item = seeded(&board, &ctx, "ownerless").await;

        let moved = board
            .transition(&ctx, &item.id, State::Claimed)
            .await
            .unwrap();
        assert_eq!(moved.assignee, None);

        let claimed = board.claim(&ctx, &item.id, "worker-a").await.unwrap();
        assert_eq!(claimed.assignee.as_deref(), Some("worker-a"));
        assert_eq!(claimed.state, State::Claimed);
    }

    /// Every active state may divert to `blocked`, and coming back out of it requeues the work at a
    /// cost: `blocked → ready` re-opens work already attempted, so it spends an attempt exactly as
    /// `failed → ready` does (C-240). Diverting *into* `blocked` is free.
    #[tokio::test]
    async fn an_item_may_block_from_any_active_state_and_requeueing_spends_an_attempt() {
        let board = MemoryBoard::new();
        let ctx = ctx();
        for spine in [
            vec![],
            vec![State::Claimed],
            vec![State::Claimed, State::InProgress],
            vec![State::Claimed, State::InProgress, State::Review],
        ] {
            let item = seeded(&board, &ctx, "blockable").await;
            for to in spine {
                board.transition(&ctx, &item.id, to).await.unwrap();
            }
            let blocked = board
                .transition(&ctx, &item.id, State::Blocked)
                .await
                .expect("every active state may block");
            assert_eq!(blocked.attempts, 0, "diverting into blocked is free");
            let requeued = board
                .transition(&ctx, &item.id, State::Ready)
                .await
                .expect("an unblocked item returns to the queue");
            assert_eq!(requeued.state, State::Ready);
            assert_eq!(
                requeued.attempts, 1,
                "requeueing out of blocked re-opens attempted work, so it spends an attempt"
            );
        }
    }

    #[tokio::test]
    async fn a_worker_that_dies_in_progress_can_fail_and_be_retried() {
        let board = MemoryBoard::new();
        let ctx = ctx();
        let item = seeded(&board, &ctx, "crashes").await;
        board.claim(&ctx, &item.id, "worker-a").await.unwrap();
        board
            .transition(&ctx, &item.id, State::InProgress)
            .await
            .unwrap();
        board
            .transition(&ctx, &item.id, State::Failed)
            .await
            .expect("in_progress -> failed is what the sweep needs");
        let retried = board
            .transition(&ctx, &item.id, State::Ready)
            .await
            .unwrap();
        assert_eq!(retried.attempts, 1);
    }

    #[tokio::test]
    async fn a_bad_cursor_is_an_error_rather_than_a_silent_first_page() {
        let board = MemoryBoard::new();
        let ctx = ctx();
        seeded(&board, &ctx, "listed").await;
        let error = board
            .list(
                &ctx,
                &Filters::new(),
                PageRequest {
                    cursor: Some("not-a-number".into()),
                    limit: 10,
                },
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("bad cursor"), "{error}");
    }
}
