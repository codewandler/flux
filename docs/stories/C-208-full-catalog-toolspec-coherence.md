---
id: C-208
title: "Extend ToolSpec coherence to the full production catalog, and settle the Network-without-Read posture"
pillar: Core
status: in-progress
priority: 16
epic: security-assurance
design: docs/designs/security-assurance.md
note: "C-191 gated try_register_builtins only; 11 known violations sit outside it, including improve_log — [Write, Filesystem] at Risk::Low, C-191's own title case"
---

# Extend ToolSpec coherence to the full production catalog, and settle the Network-without-Read posture

## Goal
C-191 established the metadata-coherence invariants (`flux_spec::coherence`) and gated them over
`try_register_builtins` at build time, plus plugin manifests at load. That is a real gate, but it is
not the production catalog. The registry a running agent actually dispatches against
(`crates/flux-cli/src/execution.rs:1301-1444`) additionally registers the cognition pack,
`flux_eval::try_register_eval_ops`, `try_register_reflect`/`try_register_flows`/`try_register_render`,
`flux_web::try_register_web`, datasource + endpoint ops, `TaskTool`, and config-authored model
stages. Every one reaches the same `Executor::dispatch` and the same `RiskApprover`.

Close the gap — and settle the product question that closing it forces, rather than loosening the
invariants to fit the catalog.

## Acceptance
- [x] Failing-first: a test over the **production** registry (not `try_register_builtins`) asserts
      metadata coherence and fails against the tree as it stands, naming the violators.
      → `every_operation_in_the_production_catalog_is_metadata_coherent`
      (`crates/flux-cli/src/catalog_coherence.rs`). First run: **22 violations across 19 ops** —
      the 11 in the table below plus `explore` and `grade`.
- [x] The gate lives where it can see the full catalog. Note the layering constraint: it cannot live
      in `flux-tools`, because `flux-web` / `flux-eval` / `flux-cognition` sit above it — `flux-cli`
      is the natural home. Do not weaken `flux-codegate`'s layering rule to place it.
      → `crates/flux-cli/src/catalog_coherence.rs`, a `#[cfg(test)]` module inside the binary
      (`flux-cli` has no lib target), so it can drive the private `register_tool_packs` production
      registrar rather than a copy. `flux_codegate::layer` is untouched; `cargo test -p
      flux-codegate` is green (13 passed).
- [x] **The `Network`-without-`Read` posture is decided and written down** before any declaration is
      edited. 8 of the 11 known violators declare `[Network]` at `Risk::Low` with no `Read`. Adding
      `Read` makes them honest *and* gather-safe — the adaptive loop could then fetch URLs and call
      models pre-approval. Raising them to `Medium` puts an approval prompt in front of every model
      call. Both are product changes; pick one, state why, and record it in the design doc.
      → `docs/designs/security-assurance.md`, "The `Network`-without-`Read` posture (C-208)",
      committed before any declaration was edited. Group A (`web.fetch`, `web.crawl`) gains `Read`;
      Group B (billable model calls) rises to `Medium`. The sorting test is **cost, not mutation**.
- [x] Each of the 11 known violations is either corrected or an explicit allowlist entry with a
      justification (`Exemption.reason` is load-bearing since C-191 — an entry with no reason or an
      unknown invariant id fails the build).
      → all 13 (the 11 + `explore` + `grade`) **corrected in place**. `flux_spec::coherence::EXEMPT`
      gained no entries, so `flux-spec` was not touched at all.
- [x] The two open registration seams are covered or explicitly scoped out with reasoning:
      `flux_sdk::FlowClient::try_register_op`/`try_register_pack` (`crates/flux-sdk/src/flow.rs:313,331`)
      and the sub-agent `child_base` registry (`crates/flux-cli/src/execution.rs:1281`).
      → `child_base` **covered**: it is exactly `try_register_builtins`, asserted a coherent subset
      of the catalog by `the_sub_agent_base_registry_is_a_coherent_subset_of_the_catalog`. The SDK
      seam is **scoped out** with written reasoning in the design doc and in the gate's `EXCLUDED`
      list — it is the same generic-registration call C-191 declined to gate, and third-party
      metadata is checked where it crosses the trust boundary (the plugin loader), not at a
      registration call.

## Progress
- **Done.** The gate is `crates/flux-cli/src/catalog_coherence.rs` (4 tests): the coherence walk over
  the production catalog, a width check proving the census is strictly wider than C-191's, the
  sub-agent subset check, and a **drift guard** that scans `execution.rs` for `try_register*` seams
  and fails on any it has not been told about — so adding a pack without adding it to the census is
  a build failure rather than a silent coverage loss. That guard is the direct answer to this
  story's own note that "a future gate should not assume crate proximity implies coverage".
- Two violators were found that the table below does not list: `explore` (`[Network]` @ Low — a
  billable `LoopHost::explore` provider call) and `grade` (`[Read, Process]` @ Low + `Idempotent`,
  on an op that runs a **caller-supplied** command). Both corrected.
- `detect_intent` was moved from the posture decision's Group A to Group B: despite its local-sounding
  description it runs `flux-flow`'s `detect_intent_stage`, a provider call that records model usage,
  so the "does it cost money?" test lands it with the model ops. Reasoning recorded in the design doc.
- A latent shadowing bug fell out of the same pass: `CognitionOp::spec` re-declared `Risk::Low` after
  lowering `OpKind::opspec()`, overriding the typed contract. Removed; the tier is declared once, and
  a test now pins `opspec().risk == registered spec().risk`.
- Gate green: `cargo build --workspace`, `cargo test --workspace`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo test -p flux-codegate`.
- `crates/flux-flow/docs/ops-reference.md` updated for the three risk-column rows that moved
  (`browser.snapshot`, `browser.close`, `consult`).

### Rework after review (REWORK → addressed)

- **Blocking, fixed.** `web.fetch` / `web.crawl` kept `Idempotency::Idempotent` from
  `ToolSpec::read_only`. The table above lists both as violating **I1 and I3**; only I1 was
  corrected, and adding `Effect::Read` took them out of `is_consequence_bearing`, so I3 stopped
  firing and the untruth became undetectable — the declaration moved out from under the rule while
  staying wrong. Both are now `Conditional`, with the reasoning this commit already applied to
  `gate_check`, `grade` and `endpoint.import`. Each is pinned by an explicit assertion, since no
  invariant backstops it any more.
  - **Generalised lesson, recorded in the design doc:** when a fix narrows what an invariant
    classifies, re-check every invariant that was firing before the narrowing.
- Comment accuracy on `grade` / `gate_check`: neither was ever auto-approved
  (`AccessKind::Process` → a `process.exec` requirement whose default grant sets
  `requires_approval: true`) nor cache-replayable (the op cache admits only all-`Read` effect sets).
  The real defect is the declaration untruth plus `PlanRisk::summary` rendering "low" verbatim to a
  human. Both comments now claim only what is true, and say why the declaration is still corrected
  rather than left leaning on an undeclared dependency.
- Drift-guard hole **closed**, not just documented. `try_register_from` sat in `COVERED` by
  function name, so a new `registry.try_register_from("new pack", …)` inherited an approved name
  and escaped the census. The guard now also classifies the **source label** (the first argument),
  with `TaskTool`'s entry derived from the same constant the census registers with. Verified by
  injecting `registry.try_register_from("a brand new pack nobody classified", …)` into
  `execution.rs`: the guard fails on it and passed before the fix. The residual limits (only
  `execution.rs` is scanned; a reused label still passes) are now stated in the docstring.
- `RiskApprover` docstring inaccuracy fixed: the CLI installs `StdinApprover` / `AllowApprover` via
  `resolve_permissions`.

## Notes
Known violations, inherited verbatim from the C-191 review (verified `path:line`):

| op | declaration | violates | evidence |
|---|---|---|---|
| `web.fetch` | `read_only` + `[Network]` → Low/Idempotent | I1, I3 | `crates/flux-web/src/fetch.rs:90,101` |
| `web.crawl` | same shape | I1, I3 | `crates/flux-web/src/crawl.rs:155,180` |
| `browser.snapshot` | `[Browser]` @ Low | I1 | `crates/flux-web/src/browser.rs:1014-1015` |
| `browser.close` | `[Process]` @ Low | I1 | `crates/flux-web/src/browser.rs:1131-1132` |
| `consult` | `[Network]` @ Low | I1 | `crates/flux-cognition/src/consult.rs:121-122` |
| `ai.*` / `synth` | `[Network]` @ Low | I1 | `crates/flux-cognition/src/lib.rs:113,183` |
| `detect_intent` | `[Network]` @ Low | I1 | `crates/flux-tools/src/reflect.rs:183,196` |
| config-authored model stages | `[Network]` @ Low | I1 | `crates/flux-tools/src/reflect.rs:87-88` |
| `improve_log` | `[Write, Filesystem]` @ Low | I1 | `crates/flux-eval/src/ops.rs:524-525` |
| `gate_check` | `[Process, LocalSystem]` + Idempotent | I3 | `crates/flux-eval/src/gate.rs:71-73` |
| `endpoint.import` | `[LocalSystem]` @ Low + Idempotent | I1, I3 | `crates/flux-capabilities/src/endpoint/ops.rs:397-399` |

- `improve_log` is the sharpest one: `[Write, Filesystem]` at `Risk::Low` is literally C-191's title
  case — a mutating op that kept a read-only risk class — and C-191's gate cannot see it.
- Two of the misses (`detect_intent`, model stages) live *inside* `flux-tools`, the gate's own crate,
  just in packs `try_register_builtins` never calls. A future gate should not assume crate proximity
  implies coverage.
- The invariants themselves were reviewed and held up — the catalog is the problem, not the rules.
  Resist redesigning `is_consequence_bearing` to make violations disappear; it is the exact negation
  of `flux-flow`'s `gather_safe` (`crates/flux-flow/src/staged.rs:2456-2473`) and that correspondence
  is load-bearing.

## Related
- C-191 (`docs/stories/C-191-toolspec-invariant-test.md`) — established the invariants and the
  built-in + plugin gates.
- The plugin-side hard cutover (warn → refuse) is deliberately NOT this story: it needs a deprecation
  window for third-party authors. See C-191's `op_coherence_warnings` rationale in
  `crates/flux-plugin/src/host/loading.rs`.
