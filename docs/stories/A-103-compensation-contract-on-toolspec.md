---
id: A-103
title: "The Compensation contract on ToolSpec — every mutating op declares how it is reversed"
pillar: Agent
status: in-progress
priority: 14
epic: transactional-turns
design: docs/designs/transactional-turns.md
note: "Inverse | Snapshot | NotNeeded | None{why}; a registry-walk test fails on any mutating built-in with no declaration, which is what stops the contract rotting as ops are added"
---

# The Compensation contract on ToolSpec — every mutating op declares how it is reversed

## Goal
Give every operation a declared answer to "how is this undone?" as a sibling of the existing
`effects: Vec<Effect>` on `ToolSpec` (`flux-spec/src/lib.rs:267`). Declaration is static and
therefore available at approval time, which is what makes the irreversibility risk signal (A-106)
possible even though the concrete reverse action can only be materialized at execution time.

## Acceptance
- [ ] `Compensation` enum in `flux-spec`: `Inverse { op }`, `Snapshot { capture, op }`,
      `NotNeeded`, `None { why }`. `NotNeeded` is the default for read-only ops so they need no
      annotation.
- [ ] `ToolSpec::with_compensation` builder, mirroring `with_effects`.
- [ ] Every mutating built-in op declares one. **Failing-first test**: a registry walk that fails
      on any op whose effects include a non-`Read` effect and whose compensation is unset — it must
      fail before the declarations are added, and it is the mechanism that keeps future ops honest.
- [ ] `None { why }` is a first-class, documented answer — `send_external`, `money`, and `bash`
      declare it with a real reason string, not a placeholder. A test asserts `bash` is `None`
      (flux cannot know what arbitrary argv did).
- [ ] The `why` string is surfaced verbatim by the consumers in A-105/A-106 — assert it is not
      `&'static str`-erased at any seam.
- [ ] No behaviour change: nothing reads `Compensation` yet.

## Progress
- 2026-07-31 — **attempted, BLOCKED on a fence, not on the design.** The contract shape is settled
  (below) and the whole story is a single-pass job once the fence is lifted; no code landed, because
  none of it can compile inside the dispatched write set.

  **Why it is blocked.** `ToolSpec` is constructed by *exhaustive struct literal* at 84 sites in 32
  files — not one of them uses `..Default::default()`, so **adding any field is a compile error at
  every site**. Two of those files are fenced to concurrent stories:
  - `crates/flux-lang/src/opspec.rs:60` (C-300) — `OpSpec::lower()`;
  - `crates/flux-capabilities/src/endpoint/ops.rs:381` (C-214) — `endpoint.import`.

  Compiler proof, with the field added and nothing else changed:
  ```
  error[E0063]: missing field `compensation` in initializer of `ToolSpec`
    --> crates/flux-lang/src/opspec.rs:60:9
  ```
  There is no green subset to fall back on: **`flux-tools` depends on `flux-lang`**, so the story's
  own write set (`flux-spec` + `flux-tools`) cannot build, and the registry-walk test cannot be run
  at all — not even to fail honestly. `crates/flux-runtime/**`, `crates/flux-tui/**` and
  `crates/flux-evidence/**` are *not* affected (0 literal sites); the fence collision is exactly
  those two files.

  `endpoint.import` is not a mechanical fixup either: it declares `Effect::LocalSystem`, so it is
  consequence-bearing and needs a real declaration (an inverse that removes the record from
  `~/.flux/endpoints.toml`, or an honest `None { why }`) — a substantive edit inside C-214's fence.
  The other 82 sites take `compensation: Option::None` (= *undeclared*), which is the honest value
  for every op outside the built-in pack until the wider-catalog follow-up lands.

- **Version implication: MINOR → `flux-spec` `1.4.0`.** The Notes below were stale: `1.3.0` is
  **published** (crates.io `codewandler-flux-spec` 1.3.0, 2026-07-30T19:58Z), so there is no
  unreleased version left to fold this into. `scripts/check-crate-versions.sh` currently passes
  (`0 changed crate(s)`), and will demand the bump the moment `crates/flux-spec/` is touched. Both
  lockfiles need re-locking (`plugins/` references `flux-spec` by path).
- **Not a wire change, and no plugin pack release is owed.** Verified rather than assumed:
  `flux-plugin-protocol` imports only `{Effect, FlowEffect, Idempotency, Risk, StagingDisposition}`
  from `flux-spec` (`crates/flux-plugin-protocol/src/lib.rs:7`) and `plugins/host-kit` re-exports
  only `{Effect, Idempotency, Risk, StagingDisposition}`. **`ToolSpec` never crosses the wire** — it
  is projected host-side by `flux_plugin::host::loading::plugin_tool_spec` from the manifest. A new
  enum plus an `Option` field on a host-side struct is additive to the crate's Rust API and invisible
  to the protocol, so this stays the cheap, additive shape.

- **Settled contract shape** (so the rerun does not re-litigate it):
  - `compensation: Option<Compensation>` on `ToolSpec`, `#[serde(default, skip_serializing_if)]`.
    `Option::None` means *undeclared*; `Compensation::None { why }` means *answered: irreversible*.
    ⚠ The two are spelled almost alike and mean opposite things — document it at the field.
  - `why: String`, **not** `&'static str` as the design doc sketches: `ToolSpec` is
    `Serialize + Deserialize`, a borrowed lifetime cannot round-trip, and Acceptance item 5 requires
    the reason survive verbatim across the A-105/A-106 seams.
  - `ToolSpec::read_only()` supplies `Some(Compensation::NotNeeded)`. That is what makes reads
    annotation-free, and — because nearly every mutating built-in is
    `read_only(...).with_effects(vec![Write, …])` — it is *also* why the floor below must reject
    `NotNeeded` on a mutating spec rather than only rejecting `Option::None`. Checking "unset" alone
    would pass every drifted op in the catalog and ship a guarantee that is a comment.
  - **I4, the compensation floor**, encoded in `flux_spec::coherence` beside I1–I3 (the story's
    "one place that answers *is this op's declaration honest?*"): a spec that is consequence-bearing
    by `is_consequence_bearing_with_effects` must declare `Inverse`/`Snapshot`/`None{why}`; an inert
    spec may only declare `NotNeeded`/unset; and a `why` must clear the length floor
    `the_allowlist_is_well_formed` already uses, so a placeholder fails. Deriving "mutating" from the
    existing C-191/C-210 predicate — rather than the Acceptance's looser "any non-`Read` effect",
    which `coherence`'s own module docs reject as condemning every file read — makes it
    *structurally impossible* for a compensation claim to contradict the op's `Effect`/`Risk`/
    `Idempotency` declarations.
  - **Applied as its own exported function, deliberately NOT folded into `metadata_violations`.**
    Every existing caller of that function asserts an empty result over a catalog this story cannot
    edit (`flux-cli`'s `catalog_coherence`, plus in-crate assertions in `flux-web`,
    `flux-cognition`, `flux-orchestrate`, `flux-capabilities`), so folding I4 in reds the gate on
    ops outside the fence. Scope it to the built-in pack now and itemise the rest as debt — exactly
    how C-191 shipped I1–I3 before C-208 widened them.
  - **The content check that keeps it from being prose**: the registry walk resolves every declared
    `Inverse`/`Snapshot` `op` against the registry and fails if it names an op that does not exist
    (and is not itself mutating). That is a mechanical check on the *declaration's content*, not
    just its presence.

## Notes
- Design: [transactional-turns.md](../designs/transactional-turns.md).
- ⚠ **Adding `Compensation` + `with_compensation` changes `flux-spec`'s public API, and `flux-spec`
  is itself on the independent protocol line (`1.x`)** — separate from the wire-protocol caveat
  below, which is about not touching `flux-plugin-protocol`. The addition is additive, so it needs a
  MINOR bump. ~~check first, because as of C-210 the crate sits at an unreleased `1.2.0`~~ —
  superseded: `1.3.0` is released (see Progress), so the bump is owed unconditionally. Run
  `./scripts/check-crate-versions.sh` before pushing — CI is otherwise the only thing that catches
  this, and it has bitten twice.
- The registry-walk test in the Acceptance is a sibling of `flux_spec::metadata_violations`
  (C-191/C-208/C-210). Consider whether it belongs alongside the existing coherence invariants
  rather than as a separate walk — one place that answers "is this op's declaration honest?" is
  easier to keep true than two.
- Lives in `flux-spec` because plugin manifests will eventually want to declare it too (it belongs
  to the same vocabulary as `semantic_effects`); do **not** add it to the plugin wire protocol in
  this story — the protocol crates are on an independent 1.x line and a wire change is its own
  decision.
- Keep the C-184 vocabulary invariant in mind: `Compensation` names a *mechanism*, never a domain.
