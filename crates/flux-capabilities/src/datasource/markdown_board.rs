//! [`MarkdownBoard`] — the file-per-item [`WorkBoard`] backend (A-114).
//!
//! One markdown file per work item, plus a **derived** index: the `docs/stories` + `/track:board`
//! pattern flux already dogfoods, behind the port A-113 defined. It is the backend that makes the
//! coordinator usable with no Jira at all, and it is the first board a human can read, diff, review
//! and edit by hand.
//!
//! Three properties carry the design, and each is a test rather than a claim:
//!
//! * **Write contention is resolved structurally, never by a lock.** One file per item means two
//!   workers touching *different* items never share bytes; two workers touching the *same* item
//!   resolve by compare-and-set on that item's file
//!   ([`System::update_file_reserved`](flux_system::System::update_file_reserved) — an
//!   exclusively-created sibling committed by atomic rename). The loser is told it lost; it never
//!   clobbers the winner.
//! * **The index is derived and never authoritative.** `list` answers from the item files and
//!   *then* refreshes `index.md` from that same scan. The index is an output, never an input, so a
//!   stale, corrupt or missing index cannot shadow a real item or invent a phantom one. Mutations
//!   do not touch it at all — that is what keeps concurrent writes to different items free of any
//!   shared mutable file.
//! * **A refused write never starts.** The edge check runs inside the compare-and-set window,
//!   before a byte reaches the staging file, and the destination is only ever replaced by a rename.
//!   An illegal transition, an unknown id or a claim conflict therefore leaves the item file
//!   byte-identical, and an interrupted write leaves either the old item or the new one — never a
//!   truncated one.
//!
//! Being file-backed also makes this the first backend that can *prove* the durability
//! [`WorkBoard::record_dispatch`] demands: a dispatch record leaves the process that wrote it, so a
//! coordinator restarted against the same root re-reads the run it was executing rather than only
//! the item's state. That is what lets the design call the board the run registry
//! (`docs/designs/fleet-coordinator.md` §5), and it is why the record rides the same
//! compare-and-set as every other write — a torn or lost record would orphan a live run.
//!
//! All IO goes through [`flux_system::System`], and the board owns its own — rerooted at the board
//! directory, which may sit anywhere the host chooses and need not be the coordinator's cwd
//! (`docs/designs/fleet-coordinator.md` §7). No [`WorkspaceContext`] change is implied: this is a
//! `System` *construction* detail.
//!
//! [`WorkspaceContext`]: flux_runtime::WorkspaceContext
//!
//! # On-disk layout
//!
//! ```text
//! <board root>/
//!   index.md          derived, regenerated on read, never authoritative
//!   items/
//!     item-0001.md    `+++` TOML frontmatter (the item) over a markdown body (title + notes)
//! ```
//!
//! The frontmatter is TOML rather than YAML because [`Item`] round-trips through it with serde and
//! `toml` is already a dependency here — a hand-rolled YAML subset would be a quoting-and-escaping
//! bug farm for no gain, and no YAML parser is reachable from this layer.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use flux_core::{Error, Result};
use flux_datasource::board::{is_retry, validate_transition, BoardSchema, Item, ItemDraft, State};
use flux_datasource::live::{FilterValue, Filters, Page, PageRequest};
use flux_runtime::ToolContext;
use flux_system::{System, Workspace};

use super::board::WorkBoard;

/// Directory holding one file per item, relative to the board root.
const ITEMS_DIR: &str = "items";

/// The derived index, relative to the board root.
const INDEX_FILE: &str = "index.md";

/// Frontmatter fence. `+++` is the established delimiter for a **TOML** block, and deliberately not
/// `---`, so a reader is never misled about which dialect the block is.
const FENCE: &str = "+++";

/// Ceiling on how many ids `create` will try before giving up. Only contention consumes attempts,
/// so reaching this means something is badly wrong rather than merely busy.
const MAX_ID_ATTEMPTS: u32 = 1024;

/// A work board stored as one markdown file per item under a directory of its own.
pub struct MarkdownBoard {
    /// The board's own guarded IO seam, rooted at the board directory.
    system: Arc<System>,
}

impl MarkdownBoard {
    /// Open a board at `root`, which **must already exist** — the same contract
    /// [`Workspace::new`] carries, and the reason root creation is the host's job rather than the
    /// backend's: a `Workspace` is what makes filesystem access guarded, so nothing under the
    /// board path may be created before one exists.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            system: Arc::new(System::new(Workspace::new(root)?)),
        })
    }

    /// Open a board at `root` while inheriting `system`'s access posture — read-only roots,
    /// `@named` roots, and the resolved sandbox.
    ///
    /// This is the constructor a host uses when the board lives outside the coordinator's own
    /// workspace: the root is chosen once, by the host, at registration. The model never supplies
    /// it, and every path below it is still resolved through the workspace guard.
    pub fn rooted_in(system: &System, root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            system: Arc::new(system.rerooted(root)?),
        })
    }

    /// The directory this board's files live under.
    pub fn root(&self) -> &Path {
        self.system.workspace().root()
    }

    /// Every item on the board, ordered by id.
    ///
    /// This is *the* read path. Nothing here consults [`INDEX_FILE`], which is why a stale index
    /// can neither hide an item nor conjure one. A file that does not parse is an error naming the
    /// file, never a silent skip — silently dropping a malformed item is exactly how a board loses
    /// work.
    async fn scan(&self) -> Result<Vec<Item>> {
        if !self.system.path_exists(ITEMS_DIR).await? {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for name in self.system.list_dir(ITEMS_DIR).await? {
            let Some(id) = item_id_of(&name) else {
                continue;
            };
            let path = item_path(id);
            let Some(text) = self.system.read_optional_text(&path)? else {
                // Raced with a rename: the entry was listed, then replaced. The next read sees it.
                continue;
            };
            items.push(parse_item(id, &text)?);
        }
        items.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(items)
    }

    /// Rewrite [`INDEX_FILE`] from a completed scan, but only when it has actually drifted.
    ///
    /// Skipping an unchanged write keeps the ordinary read path free of filesystem writes, and the
    /// write itself is atomic, so two readers regenerating at once cannot tear it.
    async fn refresh_index(&self, items: &[Item]) -> Result<()> {
        let rendered = render_index(items);
        if self.system.read_optional_text(INDEX_FILE)?.as_deref() == Some(rendered.as_str()) {
            return Ok(());
        }
        self.system.write_file_atomic(INDEX_FILE, &rendered)
    }

    /// Read–modify–write one item file under the compare-and-set, returning the committed item.
    ///
    /// `edit` sees the parsed item and its markdown body and may refuse. It runs *inside* the
    /// reservation window and *before* any byte is staged, so a refusal is not a rollback — the
    /// destination is never opened, let alone renamed over.
    async fn edit_item(
        &self,
        id: &str,
        edit: impl FnOnce(&mut Item, &mut String) -> Result<()>,
    ) -> Result<Item> {
        if !is_valid_id(id) {
            return Err(absent(id));
        }
        let committed = RefCell::new(None);
        let held = self
            .system
            .update_file_reserved(&item_path(id), |current| {
                let text = current.ok_or_else(|| absent(id))?;
                let (mut item, mut body) = parse_document(id, text)?;
                edit(&mut item, &mut body)?;
                let rendered = render_item(&item, &body)?;
                *committed.borrow_mut() = Some(item);
                Ok(rendered)
            })?;
        if !held {
            return Err(contended(id));
        }
        committed
            .into_inner()
            .ok_or_else(|| Error::Other(format!("work board: item `{id}` committed nothing")))
    }
}

/// The error every operation returns for an id the board does not hold.
///
/// Deliberately the same wording [`MemoryBoard`](super::MemoryBoard) uses: a caller switching
/// backends should not have to learn a second vocabulary for the same condition.
fn absent(id: &str) -> Error {
    Error::Other(format!("work board: no item `{id}`"))
}

/// The error a writer gets when another writer holds the item's reservation.
fn contended(id: &str) -> Error {
    Error::Other(format!(
        "work board: item `{id}` is being written by another worker; the write was refused, not merged"
    ))
}

/// Whether `id` may name a file in the board directory.
///
/// The workspace guard already refuses an escape, but an id reaches this backend from model input
/// through a generated operation, and the honest answer for a malformed one is "no such item" — not
/// a path error. Rejecting separators also keeps every item in the one directory [`scan`] walks, so
/// an id can never park work somewhere the index and `list` do not look.
///
/// [`scan`]: MarkdownBoard::scan
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// The item id a directory entry carries, or `None` if the entry is not an item file.
///
/// Staging siblings (`.item-0001.md.reserved`, `.item-0001.md.tmp-…`) start with a dot and so are
/// filtered by [`is_valid_id`]'s leading-dot rule — the same rule that keeps a hidden file from
/// being addressable as an item.
fn item_id_of(name: &str) -> Option<&str> {
    let id = name.strip_suffix(".md")?;
    is_valid_id(id).then_some(id)
}

/// Workspace-relative path of one item file.
fn item_path(id: &str) -> String {
    format!("{ITEMS_DIR}/{id}.md")
}

/// Split one item file into its frontmatter item and its markdown body.
fn parse_document(id: &str, text: &str) -> Result<(Item, String)> {
    let malformed = |what: &str| {
        Error::Other(format!(
            "work board: {} is malformed: {what}",
            item_path(id)
        ))
    };
    let rest = text
        .strip_prefix(FENCE)
        .and_then(|rest| rest.strip_prefix('\n'))
        .ok_or_else(|| malformed("no opening `+++` frontmatter fence"))?;
    let (front, body) = match rest.split_once("\n+++\n") {
        Some((front, body)) => (front, body),
        None => match rest.strip_suffix("\n+++") {
            Some(front) => (front, ""),
            None => return Err(malformed("no closing `+++` frontmatter fence")),
        },
    };

    let item: Item = toml::from_str(front).map_err(|error| malformed(&error.to_string()))?;
    // The **filename is the identity**. A frontmatter id that disagrees would be a second source of
    // truth for the one thing every other field hangs off, so it is an error rather than a silent
    // preference for either side.
    if item.id != id {
        return Err(malformed(&format!(
            "frontmatter id `{}` does not match the filename",
            item.id
        )));
    }
    Ok((item, body.to_string()))
}

/// Parse one item file, discarding the body.
fn parse_item(id: &str, text: &str) -> Result<Item> {
    parse_document(id, text).map(|(item, _)| item)
}

/// Render one item file: a `+++` TOML frontmatter block over `body`, kept verbatim.
fn render_item(item: &Item, body: &str) -> Result<String> {
    let front = toml::to_string(item).map_err(|error| {
        Error::Other(format!(
            "work board: cannot render item `{}`: {error}",
            item.id
        ))
    })?;
    Ok(format!(
        "{FENCE}\n{}\n{FENCE}\n{body}",
        front.trim_end_matches('\n')
    ))
}

/// The body a freshly created item starts with — a heading and an empty notes section, so the file
/// reads as a document rather than as a serialized struct.
fn initial_body(title: &str) -> String {
    format!("\n# {}\n\n## Notes\n", one_line(title))
}

/// Collapse newlines so a value stays on the markdown line it was written to.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_string()
}

/// Render the derived index.
///
/// The banner is not decoration: a human who edits this file needs to know it will be overwritten
/// from the item files on the next read, and which side wins if they disagree.
fn render_index(items: &[Item]) -> String {
    let mut out = String::from("# Board index\n\n");
    out.push_str(
        "<!-- Derived from `items/` and regenerated on read. Never authoritative: if this file and \
         an item file disagree, the item file wins. Edit `items/<id>.md`, not this. -->\n\n",
    );
    if items.is_empty() {
        out.push_str("_No items._\n");
        return out;
    }
    out.push_str("| Item | State | Assignee | Attempts | Title |\n");
    out.push_str("| --- | --- | --- | --- | --- |\n");
    for item in items {
        out.push_str(&format!(
            "| [{id}]({ITEMS_DIR}/{id}.md) | {state} | {assignee} | {attempts} | {title} |\n",
            id = cell(&item.id),
            state = item.state,
            assignee = cell(item.assignee.as_deref().unwrap_or("—")),
            attempts = item.attempts,
            title = cell(&item.title),
        ));
    }
    out
}

/// Escape a value for a markdown table cell.
fn cell(text: &str) -> String {
    one_line(text).replace('|', "\\|")
}

#[async_trait]
impl WorkBoard for MarkdownBoard {
    fn schema(&self) -> BoardSchema {
        BoardSchema {
            description: Some(
                "Work items stored one markdown file per item, with a derived index.".into(),
            ),
            ..BoardSchema::default()
        }
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
        let offset: usize = match &page.cursor {
            Some(cursor) => cursor
                .parse()
                .map_err(|_| Error::Other(format!("work board: bad cursor `{cursor}`")))?,
            None => 0,
        };

        let all = self.scan().await?;
        // Derived, and derived *from this scan* — so the index a reader leaves behind is exactly
        // the board the reader just saw, and it never feeds back into the answer.
        self.refresh_index(&all).await?;

        let matching: Vec<Item> = all
            .into_iter()
            .filter(|item| wanted.is_none_or(|state| item.state == state))
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
        if !is_valid_id(id) {
            return Ok(None);
        }
        match self.system.read_optional_text(&item_path(id))? {
            Some(text) => parse_item(id, &text).map(Some),
            None => Ok(None),
        }
    }

    async fn create(&self, _ctx: &ToolContext, draft: ItemDraft) -> Result<Item> {
        // Start past the highest id already on disk, then let the compare-and-set settle any tie:
        // two creators that scan simultaneously pick the same candidate, and exactly one of them
        // gets to mint it.
        let mut next = self
            .scan()
            .await?
            .iter()
            .filter_map(|item| item.id.strip_prefix("item-"))
            .filter_map(|n| n.parse::<u32>().ok())
            .max()
            .unwrap_or(0)
            + 1;

        for _ in 0..MAX_ID_ATTEMPTS {
            let item = Item {
                id: format!("item-{next:04}"),
                title: draft.title.clone(),
                state: State::Ready,
                assignee: draft.assignee.clone(),
                runner: None,
                task_id: None,
                depends_on: draft.depends_on.clone(),
                repo: draft.repo.clone(),
                attempts: 0,
                evidence: Vec::new(),
            };
            let taken = RefCell::new(false);
            let minted = self
                .system
                .update_file_reserved(&item_path(&item.id), |current| {
                    if current.is_some() {
                        *taken.borrow_mut() = true;
                        return Err(Error::Other(format!(
                            "work board: item `{}` already exists",
                            item.id
                        )));
                    }
                    render_item(&item, &initial_body(&item.title))
                });
            match minted {
                Ok(true) => return Ok(item),
                // Someone else holds the reservation for this id, or beat us to committing it.
                Ok(false) => next += 1,
                Err(_) if taken.into_inner() => next += 1,
                Err(error) => return Err(error),
            }
        }
        Err(Error::Other(format!(
            "work board: no free item id after {MAX_ID_ATTEMPTS} attempts"
        )))
    }

    async fn transition(&self, _ctx: &ToolContext, id: &str, to: State) -> Result<Item> {
        self.edit_item(id, |item, _body| {
            let from = item.state;
            // Validate first, mutate second. An illegal edge returns here having staged nothing,
            // so the file on disk is untouched rather than restored.
            validate_transition(from, to).map_err(|error| Error::Other(error.to_string()))?;
            item.state = to;
            if is_retry(from, to) {
                item.attempts += 1;
            }
            Ok(())
        })
        .await
    }

    async fn claim(&self, _ctx: &ToolContext, id: &str, assignee: &str) -> Result<Item> {
        self.edit_item(id, |item, _body| {
            if item.state == State::Claimed {
                return match item.assignee.as_deref() {
                    // Re-claiming what you already hold changes nothing.
                    Some(holder) if holder == assignee => Ok(()),
                    Some(holder) => Err(Error::Other(format!(
                        "work board: item `{id}` is already claimed by `{holder}`, not `{assignee}`"
                    ))),
                    // Reachable: `transition(id, Claimed)` moves the state without naming an owner.
                    None => {
                        item.assignee = Some(assignee.to_string());
                        Ok(())
                    }
                };
            }
            validate_transition(item.state, State::Claimed)
                .map_err(|error| Error::Other(error.to_string()))?;
            item.state = State::Claimed;
            item.assignee = Some(assignee.to_string());
            Ok(())
        })
        .await
    }

    async fn record_dispatch(
        &self,
        _ctx: &ToolContext,
        id: &str,
        runner: &str,
        task_id: &str,
    ) -> Result<Item> {
        self.edit_item(id, |item, _body| {
            // Replace rather than append: a retried item is dispatched again, and a stale task id
            // would send the sweep after a run that no longer exists.
            item.runner = Some(runner.to_string());
            item.task_id = Some(task_id.to_string());
            // Nothing else moves. `transition` is the state machine's one entry point, so this
            // write touches neither `state`, nor `attempts`, nor `assignee`.
            Ok(())
        })
        .await
    }

    async fn comment(&self, _ctx: &ToolContext, id: &str, text: &str) -> Result<()> {
        let note = format!("- {}\n", one_line(text));
        self.edit_item(id, |_item, body| {
            if !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&note);
            Ok(())
        })
        .await
        .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use flux_datasource::live::Reference;

    use super::*;

    fn item() -> Item {
        Item {
            id: "item-0007".into(),
            title: "port the board".into(),
            state: State::InProgress,
            assignee: Some("worker-a".into()),
            runner: Some("https://runner.example/a2a".into()),
            task_id: Some("task-9".into()),
            depends_on: vec!["item-0001".into(), "item-0002".into()],
            repo: Some("codewandler/flux".into()),
            attempts: 2,
            evidence: vec![
                Reference::Url {
                    url: "https://example.test/pr/1".into(),
                },
                Reference::Entity {
                    entity: "commit".into(),
                    id: "deadbeef".into(),
                },
            ],
        }
    }

    /// Every field of a fully-populated item survives the file format — including `evidence`,
    /// whose tagged variants are the only nested shape the frontmatter carries.
    #[test]
    fn a_fully_populated_item_round_trips_through_the_file_format() {
        let item = item();
        let body = "\n# port the board\n\n## Notes\n- a note\n";
        let rendered = render_item(&item, body).unwrap();
        assert!(rendered.starts_with("+++\n"), "{rendered}");
        assert!(rendered.contains("\n+++\n"), "{rendered}");

        let (parsed, parsed_body) = parse_document("item-0007", &rendered).unwrap();
        assert_eq!(parsed, item);
        assert_eq!(parsed_body, body, "the body is preserved verbatim");
    }

    /// A title full of frontmatter metacharacters is a quoting problem, and the reason the format
    /// is a real serializer rather than a hand-rolled `key: value` emitter.
    #[test]
    fn a_hostile_title_survives_the_frontmatter_intact() {
        let mut item = item();
        item.title = "a = \"b\" +++ [x]\nsecond line\t\u{1f600}".into();
        let rendered = render_item(&item, "").unwrap();
        assert_eq!(parse_document("item-0007", &rendered).unwrap().0, item);
    }

    #[test]
    fn the_filename_is_the_identity_and_a_disagreeing_frontmatter_id_is_an_error() {
        let rendered = render_item(&item(), "").unwrap();
        let error = parse_document("item-0009", &rendered)
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not match the filename"), "{error}");
    }

    #[test]
    fn an_id_that_could_escape_the_items_directory_is_refused() {
        for bad in ["", "..", "../escape", "a/b", ".hidden", "with space", "☃"] {
            assert!(!is_valid_id(bad), "`{bad}` must not name an item file");
        }
        for good in ["item-0001", "A-114", "PROJ_42", "v1.2"] {
            assert!(is_valid_id(good), "`{good}` is a reasonable id");
        }
        // Staging siblings are never mistaken for items.
        assert_eq!(item_id_of(".item-0001.md.reserved"), None);
        assert_eq!(item_id_of("index.md"), Some("index"));
        assert_eq!(item_id_of("notes.txt"), None);
        assert_eq!(item_id_of("item-0001.md"), Some("item-0001"));
    }

    #[test]
    fn the_index_names_itself_derived_and_escapes_table_cells() {
        let mut item = item();
        item.title = "a | b".into();
        let index = render_index(&[item]);
        assert!(index.contains("Never authoritative"), "{index}");
        assert!(index.contains("| a \\| b |"), "{index}");
        assert!(render_index(&[]).contains("_No items._"));
    }
}
