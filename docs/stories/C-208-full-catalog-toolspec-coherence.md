---
id: C-208
title: "Extend ToolSpec coherence to the full production catalog, and settle the Network-without-Read posture"
pillar: Core
status: ready
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
- [ ] Failing-first: a test over the **production** registry (not `try_register_builtins`) asserts
      metadata coherence and fails against the tree as it stands, naming the violators.
- [ ] The gate lives where it can see the full catalog. Note the layering constraint: it cannot live
      in `flux-tools`, because `flux-web` / `flux-eval` / `flux-cognition` sit above it — `flux-cli`
      is the natural home. Do not weaken `flux-codegate`'s layering rule to place it.
- [ ] **The `Network`-without-`Read` posture is decided and written down** before any declaration is
      edited. 8 of the 11 known violators declare `[Network]` at `Risk::Low` with no `Read`. Adding
      `Read` makes them honest *and* gather-safe — the adaptive loop could then fetch URLs and call
      models pre-approval. Raising them to `Medium` puts an approval prompt in front of every model
      call. Both are product changes; pick one, state why, and record it in the design doc.
- [ ] Each of the 11 known violations is either corrected or an explicit allowlist entry with a
      justification (`Exemption.reason` is load-bearing since C-191 — an entry with no reason or an
      unknown invariant id fails the build).
- [ ] The two open registration seams are covered or explicitly scoped out with reasoning:
      `flux_sdk::FlowClient::try_register_op`/`try_register_pack` (`crates/flux-sdk/src/flow.rs:313,331`)
      and the sub-agent `child_base` registry (`crates/flux-cli/src/execution.rs:1281`).

## Progress
- (not started)

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
