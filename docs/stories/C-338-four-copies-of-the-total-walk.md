---
id: C-338
title: "Four copies of the same total-walk redaction logic, which is how the node-kind hole recurred"
pillar: Core
epic: road-to-stable
status: ready
priority: 8
areas: [flux-web, flux-flow, flux-orchestrate, flux-secret]
note: "C-323 had to fix the SAME defect in four separate walkers — flux-web's redact_json, flux-flow's engine evidence flush and cassette input_view, and flux-orchestrate's spawn reporter. Two of them also skipped object KEYS. The duplication is the reason one walker could narrow while the others did not"
---

# Four copies of the total-walk, and no single owner

## Goal

Give "a registered secret is redacted at every node kind" **one** implementation, so the next walker
cannot quietly narrow.

C-323 fixed a hole where `redact_json` skipped `Value::Number` — and found the *same* defect in
three more walkers:

| walker | what it skipped |
|---|---|
| `crates/flux-web/src/http.rs` `redact_json` | non-string scalars |
| `crates/flux-flow/src/engine.rs` (evidence flush → **durable event store**) | non-string scalars **and object keys** |
| `crates/flux-flow/src/cassette.rs` (durable `input_view`) | non-string scalars **and object keys** |
| `crates/flux-orchestrate/src/lib.rs` `redact_spawn_json` (sub-agent live reporter) | non-string scalars |

Four independent hand-rolled traversals of the same shape, each free to narrow on its own. **That
duplication is the mechanism by which the defect existed at all** — there was no place where "total"
was defined once and enforced.

## Acceptance

- [ ] One shared total-walk, used by all four call sites. **The obstacle is real and is the story:**
      consolidating needs either a new `pub` item on the published `codewandler-flux-secret`, or a
      dependency edge from `flux-web` into it — and `flux-web` takes a redaction *closure* today
      precisely to avoid that edge. Decide which cost to pay and say what you rejected.
- [ ] **Failing-first, and it must be structural rather than behavioural**: after consolidation,
      adding a hypothetical new `serde_json::Value` variant — or narrowing the shared walk — must red
      a named test or fail to compile. A test that merely re-checks today's four call sites would not
      have prevented this story.
- [ ] Every existing pin stays green, especially the four C-323 added and C-216's corpus.
- [ ] ⚠ **The cassette's two-path split must survive or be replaced deliberately, not lost in a
      refactor.** C-323 kept an order-preserving textual rewrite for string-leaf-only cases because
      naive re-serialization sorts keys (`serde_json::Map` is a `BTreeMap`) and changed the capped
      view's head, and because textual substitution of a *numeric* credential can splice a quoted
      string into the middle of a number and leave `input_view` unparseable — which the TUI
      re-parses (`crates/flux-tui/src/lib.rs:2680`). Any consolidation has to honour both constraints
      or explicitly change them.
- [ ] Full gate green in both workspaces.

## Notes

- Found by [C-323](C-323-redact-json-skips-numbers.md), which fixed all four and flagged the
  duplication as outside its fence. Its report is the inventory — start there rather than re-deriving.
- Related: [C-313](C-313-url-encoder-consolidation-and-key-pinning.md) is the same shape one layer
  over — a fifth private copy of the RFC 3986 encoder while the design doc claims the tree has one.
  Consider whether both consolidations want the same answer about where shared primitives live.
- **Not the same defect, do not fold it in:** `crates/flux-plugin-protocol/src/lib.rs:241`
  `redact_secret_fields` replaces a *named* field's value whole, of any kind, and never consults a
  `Redactor`. It does not narrow by node kind. C-323 verified this and even tried to document it in
  place — and had to revert, because touching that crate trips `check-crate-versions.sh` on the
  independent 1.x protocol line (C-143).
- Perf note carried over: `Redactor::redact` clones and sorts the registered-value list on every
  call under a mutex, and is now invoked per *scalar*. Not a new cost class, but a shared walk is the
  natural place to fix it if it ever matters.
