# Design: flux-lang v1 hardening — close the review findings, ship an honest v1

**Status:** ✅ shipped 2026-07-02 (all six stories done; full gate green; residuals recorded below) · **Pillar:** Language (+ Core for the compile-path slice) · **Stories:**
[C-17](../stories/C-17-compile-path-plan-gates.md) ·
[L-15](../stories/L-15-analyzer-unbound-vars-required-params.md) ·
[L-16](../stories/L-16-analyzer-contract-completion.md) ·
[L-17](../stories/L-17-runtime-semantics-hardening.md) ·
[L-18](../stories/L-18-roundtrip-totality-parser-locators.md) ·
[L-19](../stories/L-19-flux-lang-docs-truth-pass.md)

## Why

A full review of flux-lang (2026-07-02: three scoped deep-dives — runtime, analyzer/optimizer,
emission/integration — plus first-hand parser/AST/spec reading and empirical round-trip probes)
confirmed the thesis and architecture are sound: the dispatch envelope is real, the reliability
tier works, SSOT doc/schema discipline is unusually good. But it surfaced defects clustered in
exactly the places a *model-authored* language gets hurt: one safety bypass on the compile path,
an analyzer that under-delivers its documented contract, duplicated runtime eval paths that have
already diverged once, an overclaimed round-trip invariant with one **confirmed silent-corruption
case**, and spec/docs describing behavior that does not exist. This epic fixes all of it — the
bar is "v1 honestly shippable": every documented claim either true or removed.

### Findings ledger (condensed; the epic's scope)

| # | Finding | Where | Story |
|---|---|---|---|
| F1 | Hidden-op gate skipped on the plain-text plan fallback — a prose-JSON plan calling a registered-but-unsurfaced op executes | flux-flow/compile.rs:311-321 vs :366 | C-17 |
| F2 | Accepted-with-diagnostics plans executed blind by the engine loop | flux-flow/engine.rs:380-390 | C-17 |
| F3 | Multiple `emit_plan` calls in one message: last silently wins | compile.rs:389/404 | C-17 |
| F4 | `unsafe` raw-pointer sink reborrow in the planner loop | compile.rs:284-291 | C-17 |
| F5 | No symbol-definedness analysis; unknown `$var` types as `Any`, dies at runtime | analyze.rs:231 | L-15 |
| F6 | No required-param presence check before dispatch | opspec/check_call_types | L-15 |
| F7 | Analyzer accepts expression positions the runtime rejects (call-in-arg, bad cond kinds) | analyze.rs:693 vs runtime.rs:2543/2479 | L-16 |
| F8 | `SymbolName` unvalidated → dotted name silently reparses as `jq` through the text round-trip (**empirically confirmed**: `Var{"a.b"}` → `$a.b` → `Jq{".b", $a}`) | ast.rs/analyze.rs/parse.rs:1576 | L-16/L-18 |
| F9 | Type checker (`lower`) never runs on the production path | flux-flow/compile.rs, registry.rs:197, flux-cli/main.rs:1987 | L-16 |
| F10 | `repeat max: 0` / absurd max / empty bodies pass analysis silently | analyze.rs | L-16 |
| F11 | Diagnostics carry no locators (no spans, no node paths) | analyze.rs:12, error.rs | L-16/L-18 |
| F12 | `_ =>` wildcards in `nested_bodies`/`node_contains_return` — new node kinds silently escape checks | analyze.rs:149/1100 | L-16 |
| F13 | Statement-position `jq` diverges from bind-position (string-stored JSON not parsed) | runtime.rs:1946 vs :1010 | L-17 |
| F14 | Error-type erasure defeats `retry` fatality — denied `confirm` inside `loop`/composite is retried | runtime.rs:1573-1580, 1712, 426-459 | L-17 |
| F15 | `parallel`: sibling error drops completed branches' buffered output; cross-branch binds race | runtime.rs:1500-1510 | L-17 (+L-16 analyzer check) |
| F16 | `race`: all-failed reported as timeout; losers' audit discarded; `budget` undercounts | runtime.rs:1771-1783 | L-17 |
| F17 | Checkpoint keying split (name vs body-hash) between run/resume; edited flow fast-forwards wrongly | runtime.rs:640 vs :657, :63-71 | L-17 |
| F18 | `throttle` counts body entries not dispatches; non-atomic bucket | runtime.rs:1814-1834 | L-17 |
| F19 | `debounce` is a sleep stub (`name` unused, no coalescing) | runtime.rs:1846-1854 | L-17 |
| F20 | Trim/`last_value`/`StepId`/`each`-scoping inconsistencies | runtime.rs:1422,1492,1245+,315,1319 | L-17 |
| F21 | Round-trip totality claim false for non-identifier names (loud) and dotted expr names (silent, F8) | format.rs name positions | L-18 |
| F22 | No property test behind the "every DraftAst" claim; parse errors carry no line numbers | parse.rs/tests | L-18 |
| F23 | syntax.md documents unimplemented constructs (`"""`, named-arg comma form, multi-line call args, `watch`/`block`) | crates/flux-lang/docs/syntax.md | L-19 |
| F24 | reference.md promises race-in-order/throttle-dispatches/debounce-coalescing the runtime doesn't do | reference.md:483,637,662 | L-19 |
| F25 | emission-ab.md stale (arm 1 shipped); opspec.rs doc rot; error.rs overclaims | docs | L-19 |
| F26 | Plan-approval render hides `obj`/`list` template contents; `children()` wildcard is a silent hole | render.rs:199,410-428 | L-19 |
| F27 | skill.rs hand-written examples have no parse-as-DraftAst drift guard | skill.rs:53-159 | L-19 |

## Approach

Five file-disjoint workstreams, one story each, implemented by parallel sub-agents on `main`;
shared base edit (name-validity helpers on `ast.rs`) lands first; cross-crate wiring (F9, and the
`analyze_flow` session-symbols cutover from L-15) lands last as one integration step so the
workspace stays green during the fan-out. Full gate + end-to-end verify close the epic.

### Target semantics (normative for L-17 and L-19 — code and docs must both match)

- **`race`** runs branches concurrently; first *success* wins. All-branches-failed is reported as
  a joined branch error, distinct from a timeout. Losing branches' dispatched steps stay in the
  step count and transcript (audit parity with the event log; `budget` counts them).
- **`parallel`** merges branch results in **declaration order**; when a branch fails, completed
  branches' buffered sink output/steps are still merged (deterministic prefix) before the error
  propagates. Cross-branch same-symbol binds (including inner binds) are an **analyzer error**.
- **`throttle`** limits **op dispatches** inside its body per sliding window (budget-style
  counting), with an atomic bucket update keyed by `name`.
- **`debounce`** is keyed cross-turn coalescing: per-`name` last-trigger timestamp in the session
  store; the body runs only once `wait_ms` has elapsed since the key's last trigger.
- **`checkpoint`/resume** use one `flow_key` derivation everywhere: declared name **plus body
  hash**, so an edited flow never fast-forwards past changed statements, and run/resume agree.
- **Fatality** (`AssertFailed`, `ConfirmDenied`, policy denial) survives every wrapping layer
  (`loop` re-wrap, composite-op stringification); `retry` checks `is_fatal()` structurally.

### Design policy adopted with this epic

**Node-catalog freeze:** no new `Node` kinds land until (a) symbol-definedness analysis (L-15)
and (b) diagnostic locators (L-16/L-18) have shipped. The catalog grew 7 → 43; each addition now
multiplies analyzer/runtime/render surface that these two facilities are needed to keep sound.

## Alternatives considered

- **De-scope `throttle`/`debounce` (re-document as-built or delete the nodes).** Rejected by
  owner decision 2026-07-02 — implement full semantics instead.
- **Keep type checking SDK-opt-in and drop the claim.** Rejected — `lower` is built and lenient;
  wiring it into the engine path makes the documented contract true at low rejection risk.
- **Spans in the AST** (full source positions). Deferred — line numbers in parse errors and
  JSON-pointer node paths in analyzer diagnostics deliver most of the repair-loop value without a
  wire-format change.

## Risks & open questions

- `crates/flux-flow/src/compile.rs` carries uncommitted WIP (C-13-adjacent) — C-17 builds on top,
  never reverts.
- Wiring `lower` + session-symbol definedness may reject previously-accepted plans; lenient rules
  and the L-15 order-insensitive design bound the false-positive risk. Canary: flux-flow/flux-cli
  suites + the eval compile-repair loop.
- Debounce/throttle durable state must stay inside the existing store seams (no new IO in
  flux-lang).

## Residuals (recorded at close, 2026-07-02 — small follow-ups, not v1 blockers)

- The engine's `resume_suspended` persists only body+node, so a *named* flow resumed through the
  engine still derives its checkpoint key hash-only until the name is threaded through
  `resume_flow_named` (flux-lang side shipped; flux-flow wiring pending).
- Policy denial surfaces as an in-band `OpOutcome::is_error` string, so it is not representable in
  `FlowError::is_fatal()` yet (documented in error.rs).
- `each` source / `jq` input / `parse` value are `eval_arg` positions the analyzer still accepts
  calls in; `type_check_body` diagnostics don't carry node paths yet.
- `{{sym}}` definedness inside `Fmt` templates deliberately out of v1 (false-positive risk;
  recorded in L-15).

## Acceptance / done

Union of C-17 + L-15..L-19 acceptance. Epic-level: every finding F1–F27 either fixed with a
failing-first test or explicitly re-documented (L-19), full workspace gate green, and the two
empirical round-trip counterexamples (`Var{"a.b"}` in expression position; space-containing
names) either round-trip via `@json` or are rejected by the analyzer.
