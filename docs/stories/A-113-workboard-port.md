---
id: A-113
title: The WorkBoard port — L0 contracts, L5 registration generating six ops, MemoryBoard
pillar: Agent
status: ready
priority: 3
epic: fleet-coordinator
design: docs/designs/fleet-coordinator.md
areas: [flux-datasource, flux-capabilities]
note: "⚠ touches flux-datasource (protocol line) — obliges an explicit version decision; the mutating ops' concrete permission_subjects are the main safety surface"
---

# The WorkBoard port — L0 contracts, L5 registration generating six ops, MemoryBoard

## Goal
Give the coordinator a **write-capable** state source whose implementation is swappable, following
the convention `LiveDatasource` already established (`crates/flux-capabilities/src/datasource/live.rs:60`):
a backend declares a schema plus its external authority, is validated once at registration, and the
host generates uniform ops with stable permission subjects, a tool group and an ambient signal.

`LiveDatasource` is strictly read-only (`list` + `get`). This adds the sibling port with a typed
work-item state machine, so the coordinator can *reason* about the board — dependency waves from
`depends_on`, stuck detection from `state` + `attempts` — rather than shuffle opaque rows.

## Acceptance
- [x] `flux-datasource` (L0) gains a `board` module: `Item`, `ItemDraft`, `State`, `BoardSchema`,
      reusing `live::{Page, PageRequest, Filters, FilterValue, Reference}` verbatim — no parallel
      vocabulary.
- [x] `flux-capabilities` (L5) gains a `board` module with the `WorkBoard` trait (`schema`, `access`,
      `list`, `get`, `create`, `transition`, `claim`, `comment`) and
      `try_register_work_board(registry, domain, backend)` generating `board.list` / `.get` /
      `.create` / `.transition` / `.claim` / `.comment` — mirroring `live_datasource_tools` /
      `try_register_live_datasource` (`live.rs:130`), registering atomically on a clone.
- [x] Failing-first test: an **illegal state transition errors and performs no write** — the item is
      byte-identical afterwards. Legal edges (`Ready → Claimed → InProgress → Review → Done`, plus
      `Blocked` and `Failed → Ready` with `attempts += 1`) succeed.
- [x] Failing-first test: every mutating op reports **concrete** `permission_subjects`
      (`<domain>/item/<id>`; `create` reports `<domain>/item/new`) — never `*`, never empty. Per
      AGENTS.md:98 an empty-subject `Write` is forced to approval; this test pins that we do not
      dodge gating.
- [x] `MemoryBoard` ships as the offline test double (mirroring `datasource/memory.rs`), and a
      **shared contract-test suite** runs against it — reusable verbatim by A-114/A-115/A-118, and
      runnable with no credentials and no network.
- [x] `cargo test -p flux-codegate` green: the layer map is untouched (L0 → L5 edges only).
- [x] Explicit version decision recorded for `flux-datasource` (protocol line).

## Progress
- **Landed** on `impl/A-113`. L0 `flux_datasource::board` (`Item`, `ItemDraft`, `State`,
  `BoardSchema`, `EDGE_DIAGRAM`, `validate_transition`, `is_retry`, `IllegalTransition`) + L5
  `flux_capabilities::datasource::board` (`WorkBoard`, `try_register_work_board`,
  `work_board_tools`, `validate_board_contract`, `WorkBoardSurface`) + `MemoryBoard`.
- **The state machine is the operational reading, not the design's ASCII diagram.** Edge set:
  spine `Ready → Claimed → InProgress → Review → Done`; `{Ready, Claimed, InProgress, Review} →
  Blocked → Ready`; `{InProgress, Review} → Failed → Ready` (`attempts += 1`); `Done` terminal.
  The diagram in `docs/designs/fleet-coordinator.md` §2 draws a narrower machine (`Blocked`
  rejoining at `Claimed`, `Failed` only from `Review`) and **needs updating to match** — a crashed
  worker is in `InProgress`, which is exactly what §5's sweep inspects. Every edge lives in one
  `const EDGES` table in L0; `State::allowed_next` / `validate_transition` are its only readers.
- **`flux-datasource` bumped `1.0.0` → `1.0.1`** (additive module, no existing signature touched).
  ⚠ This obliges a **`plugins/Cargo.lock` regeneration** — that nested workspace still pins
  `1.0.0`, and `flux-codegate`'s `plugin_builds_exclude_host_only_crates` resolves it with
  `--locked`, so the gate is red on exactly that one test until the lockfile is refreshed.
- `BoardSchema` deliberately carries **no `capabilities` field** (design §2 sketches one): the
  trait makes all six ops mandatory, so it would have no consumer or test here. A-115 can add
  `states: Vec<State>` when it has a test that needs it.
- Filter/paging normalization is **shared with `live.rs`** rather than forked — `normalize_filters`
  / `normalize_limit` / `filter_schema` / `valid_domain` widened to `pub(super)` and generalized;
  live's existing error strings are preserved via a `scope` parameter.
- The contract suite is `crates/flux-capabilities/tests/board_contract/mod.rs`. A-114/A-115/A-118
  reuse it with one `mod board_contract;` — no manifest change, no public-API cost.

## Notes
- Design: [fleet-coordinator.md §2, §3](../designs/fleet-coordinator.md) — including why extending
  `LiveDatasource` with optional mutations, and a generic mutable-record port, were both rejected.
- ⚠ `flux-datasource` is a protocol-line crate: `scripts/check-crate-versions.sh` in CI is the only
  thing that catches a missing bump.
