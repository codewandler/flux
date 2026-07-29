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
- [ ] `flux-datasource` (L0) gains a `board` module: `Item`, `ItemDraft`, `State`, `BoardSchema`,
      reusing `live::{Page, PageRequest, Filters, FilterValue, Reference}` verbatim — no parallel
      vocabulary.
- [ ] `flux-capabilities` (L5) gains a `board` module with the `WorkBoard` trait (`schema`, `access`,
      `list`, `get`, `create`, `transition`, `claim`, `comment`) and
      `try_register_work_board(registry, domain, backend)` generating `board.list` / `.get` /
      `.create` / `.transition` / `.claim` / `.comment` — mirroring `live_datasource_tools` /
      `try_register_live_datasource` (`live.rs:130`), registering atomically on a clone.
- [ ] Failing-first test: an **illegal state transition errors and performs no write** — the item is
      byte-identical afterwards. Legal edges (`Ready → Claimed → InProgress → Review → Done`, plus
      `Blocked` and `Failed → Ready` with `attempts += 1`) succeed.
- [ ] Failing-first test: every mutating op reports **concrete** `permission_subjects`
      (`<domain>/item/<id>`; `create` reports `<domain>/item/new`) — never `*`, never empty. Per
      AGENTS.md:98 an empty-subject `Write` is forced to approval; this test pins that we do not
      dodge gating.
- [ ] `MemoryBoard` ships as the offline test double (mirroring `datasource/memory.rs`), and a
      **shared contract-test suite** runs against it — reusable verbatim by A-114/A-115/A-118, and
      runnable with no credentials and no network.
- [ ] `cargo test -p flux-codegate` green: the layer map is untouched (L0 → L5 edges only).
- [ ] Explicit version decision recorded for `flux-datasource` (protocol line).

## Progress
- (not started)

## Notes
- Design: [fleet-coordinator.md §2, §3](../designs/fleet-coordinator.md) — including why extending
  `LiveDatasource` with optional mutations, and a generic mutable-record port, were both rejected.
- ⚠ `flux-datasource` is a protocol-line crate: `scripts/check-crate-versions.sh` in CI is the only
  thing that catches a missing bump.
