---
id: A-114
title: MarkdownBoard — file-per-item with a derived index, IO via flux-system
pillar: Agent
status: in-progress
priority: 30
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-capabilities]
note: "the track-style backend flux already dogfoods; write contention resolved structurally (file-per-item + atomic rename), never a lock"
---

# MarkdownBoard — file-per-item with a derived index, IO via flux-system

## Goal
A zero-dependency, diffable, reviewable `WorkBoard` backend: one markdown file per work item with
frontmatter, plus a generated index — the `docs/stories` + `/track:board` pattern flux already
dogfoods. It is what makes the coordinator usable with no Jira at all, and what makes the board's
history reviewable in git.

## Acceptance
- [x] `MarkdownBoard` implements `WorkBoard` and **passes the shared contract suite from A-113
      unmodified** — `tests/markdown_board.rs::markdown_board_satisfies_the_shared_work_board_contract`
      (`mod board_contract;`, not one byte changed).
- [x] All IO goes through `flux_system::Workspace` — no direct `std::fs` on the backend path.
      Every read is `read_optional_text`/`list_dir`/`path_exists`, every write is
      `update_file_reserved`/`write_file_atomic`, all on the board's own `System`.
- [x] Failing-first test: two concurrent `claim` calls on the **same** item resolve to exactly one
      winner (compare-and-set on the item file), and the loser gets a conflict error, not a
      clobbered file — `concurrent_claims_on_one_item_resolve_to_exactly_one_winner`.
- [x] Failing-first test: two concurrent writes to **different** items never contend — no shared
      mutable file on the write path — and the index is regenerated on read, so a stale or missing
      index is never authoritative and never loses an item —
      `concurrent_writes_to_different_items_never_contend_and_the_index_is_derived_on_read` plus
      `a_stale_or_missing_index_is_never_authoritative`.
- [x] Item writes are atomic (write-then-rename); an interrupted write leaves either the old item or
      the new one, never a truncated file —
      `a_committed_write_replaces_the_item_file_instead_of_truncating_it` pins the inode change and
      that a reader holding the old file still sees a whole item.
- [x] The board root is configurable and may differ from the coordinator's cwd — a `System`
      construction detail, resolved without any `WorkspaceContext` change —
      `MarkdownBoard::new` / `rooted_in`, pinned by
      `the_board_root_is_configurable_and_independent_of_the_coordinator_cwd`.

## Progress
- **Landed** on `impl/A-114`. `flux_capabilities::datasource::markdown_board::MarkdownBoard`:
  one `items/<id>.md` per item (`+++` TOML frontmatter over a markdown body) plus a derived
  `index.md`. The board owns its own `System`, rerooted at the board directory, so the root is a
  construction detail and no `WorkspaceContext` change was needed (design §7).
- **The port needed one new guarded primitive**, added to `flux-system`:
  `System::update_file_reserved(path, update)`. `write_file_atomic` cannot compare-and-set — its
  staging sibling has a *unique* name, so its rename always wins. The new call stages into a
  *deterministically* named sibling created with `O_EXCL`; that create is the compare, the rename
  onto the destination is the set, and it releases the reservation in the same atomic step. This is
  git's ref-lock protocol. `Ok(false)` means another writer holds it; nothing was read or written.
  The `WorkBoard` trait is untouched.
- **Which side wins when the index and the item files disagree: the item files, always.** The index
  is an *output only* — `list` answers from a scan of `items/` and then refreshes `index.md` from
  that same scan. Nothing ever reads the index back. Mutations do not touch it at all, which is what
  keeps concurrent writes to different items free of any shared mutable file.
- **The filename is the item's identity.** A frontmatter `id` disagreeing with the filename is a
  loud error, not a silent preference for either side, and a malformed item file errors naming the
  file rather than being skipped — silently dropping an item is how a board loses work.
- Frontmatter is **TOML behind `+++`**, not YAML behind `---`. No YAML parser is reachable from L5
  without a manifest change (`flux-markdown`/`serde_norway` is not a dependency here and the
  manifest was fenced), and `toml` already is one. `Item` round-trips through serde, so titles full
  of quoting metacharacters and the tagged `evidence` variants are the serializer's problem, not a
  hand-rolled emitter's.
- Failing-first evidence beyond "the type does not exist yet": replacing the compare-and-set with a
  naive read-then-write makes `concurrent_claims_on_one_item_resolve_to_exactly_one_winner` fail
  with **8 winners and 0 conflicts**. Restoring it makes it green.

## Notes
- Design: [fleet-coordinator.md §3, §7](../designs/fleet-coordinator.md). The multi-root question
  folds in here rather than being filed separately: remote A2A workers own their own workspace
  pinning, so the only residue is where this backend's files live.
- Depends on A-113 (port + contract suite).
