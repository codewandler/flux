---
id: C-338
title: "Four copies of the same total-walk redaction logic, which is how the node-kind hole recurred"
pillar: Core
epic: road-to-stable
status: done
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

- [x] One shared total-walk, used by all four call sites. **The obstacle is real and is the story:**
      consolidating needs either a new `pub` item on the published `codewandler-flux-secret`, or a
      dependency edge from `flux-web` into it — and `flux-web` takes a redaction *closure* today
      precisely to avoid that edge. Decide which cost to pay and say what you rejected.
- [x] **Failing-first, and it must be structural rather than behavioural**: after consolidation,
      adding a hypothetical new `serde_json::Value` variant — or narrowing the shared walk — must red
      a named test or fail to compile. A test that merely re-checks today's four call sites would not
      have prevented this story.
- [x] Every existing pin stays green, especially the four C-323 added and C-216's corpus.
- [x] ⚠ **The cassette's two-path split must survive or be replaced deliberately, not lost in a
      refactor.** C-323 kept an order-preserving textual rewrite for string-leaf-only cases because
      naive re-serialization sorts keys (`serde_json::Map` is a `BTreeMap`) and changed the capped
      view's head, and because textual substitution of a *numeric* credential can splice a quoted
      string into the middle of a number and leave `input_view` unparseable — which the TUI
      re-parses (`crates/flux-tui/src/lib.rs:2680`). Any consolidation has to honour both constraints
      or explicitly change them.
- [x] Full gate green in both workspaces.

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

## Progress

**Done.** The walk is `flux_core::redact_json_total` (`crates/flux-core/src/redaction.rs`), taking
the redaction closure and returning a `JsonRedaction` report. All four hand-rolled copies are
deleted; net −170 lines.

**Where it lives, and what was rejected.** `flux-core` (L0), for the reason `percent_encode_component`
is there — C-313's sibling consolidation already answered "where do shared primitives live", and the
Notes above asked whether both want the same answer. They do. Every one of the four crates already
depends on `flux-core`, so this costs **no new dependency edge and no manifest change at all**.

Both options the Acceptance named were rejected, and for the same reason: each is a fenced manifest
edit rather than a judgement call.

- *A new `pub` item on `codewandler-flux-secret`.* `flux-secret` carries `serde_json` only as a
  **dev**-dependency; the walk would have forced it into `[dependencies]` on a published, protocol-
  lined L0 crate — and would *still* have needed the flux-web edge below, since flux-web does not
  depend on flux-secret at all.
- *A `flux-web` → `flux-secret` dependency edge.* flux-web takes a redaction closure precisely to
  avoid this. Taking the closure in `flux-core` instead keeps that seam exactly where flux-web put
  it: the walk knows how to visit every node, the caller knows what a secret is.

**The guard is structural, per `capability_widenings`.** The walk's `match` is exhaustive by variant
with **no catch-all arm** — all four copies ended in a `scalar => …` rest arm, and that rest arm is
what let three of them keep a node kind out of the redactor unnoticed. A new `serde_json::Value`
variant now fails the build in two places (the walk, and `classify` in the anchor). Narrowing the
walk reds `the_total_walk_offers_every_json_node_kind_to_the_redactor` by name:
`crates/flux-core/tests/json_redaction_totality.rs` names, per node kind, the exact text only a walk
that reaches that kind could hand the redactor.

**The cassette's two paths survive**, and are now *stronger*: the choice between them is the single
walk's own report (`needs_reencode`) instead of a second traversal (`string_leaf_replacements`)
re-deciding which nodes had been rewritten. Two walks meant two chances to narrow and a chance for
the two to disagree — and a disagreement there is exactly the unparseable `input_view` the TUI chokes
on. Both branches were re-verified load-bearing by forcing each unconditionally, as C-323 did:
forcing the textual path reds `recorded_input_view_redacts_a_registered_numeric_credential_and_keeps_parsing`;
forcing the re-encode path reds `recorded_input_view_is_redacted_capped_and_backward_compatible`.

**Public API.** `codewandler-flux-core` gains two items (`redact_json_total`, `JsonRedaction`) —
additive. `check-crate-versions.sh` PASSes, but it is structurally blind to workspace-versioned
published crates, so that PASS is not evidence nothing is owed; the version decision is the
coordinator's. No crate version was changed here. `flux-plugin-protocol` was not touched.

**Not done here.** C-339's fifth site (`crates/flux-sdk/src/test.rs` `redact_and_hash_request`) is
now unblocked — it redacts the *serialized* text and falls back to the **unredacted** value when the
result no longer parses (`serde_json::from_str(&redacted_str).unwrap_or(canonical)`), which is
precisely what a total node-level walk removes the need for. Left to C-339.
