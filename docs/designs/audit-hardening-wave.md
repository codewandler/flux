# Design: Audit hardening wave

**Status:** implemented (2026-07-10; C-50 waves 1–3) · **Pillar:** Core · **Story:**
[C-50](../stories/C-50-audit-hardening-wave.md)

## Why

The 2026-07-10 audit found several places where a check is correct at the API boundary but loses its
meaning before the effect completes: redirects are authorized only at the first URL, filesystem scopes
name a lexical alias rather than the object used by IO, child output is capped after allocation, and
cancelled async work can skip cleanup. It also found measurement seams where missing data is encoded as
zero, allowing a failed candidate to look cheaper than a successful one.

These are one class of defect: **the invariant must cover the complete lifetime of an effect, not just
its admission point**. The implementation therefore hardens lifetimes and identities at their owning
layer instead of adding surface-specific exceptions.

## Invariants

1. **Authorization follows identity.** Every redirect target is guarded before connection. Filesystem
   policy is evaluated against the canonical in-workspace target actually opened; unresolved write
   tails retain a canonical existing parent plus their lexical suffix.
2. **Secrets are origin-bound.** User/plugin-provided authorization and secret-derived headers are
   stripped on a cross-origin redirect. Redirect count and response bytes are bounded.
3. **Resource limits apply during production.** Process pipes and response/file bodies are drained
   incrementally into bounded buffers. Cancellation and timeout kill and reap owned children.
4. **Cancellation is structured.** A cancelled branch cannot abandon mandatory cleanup or leave a
   request/response protocol half-consumed. Where a dependency cannot be safely cancelled, a driver
   owns it to completion or the connection is explicitly poisoned and restarted.
5. **Invalid measurements fail closed.** Crash, parse failure, missing required telemetry, or an empty
   candidate is represented explicitly and cannot win a comparison through default zeroes.
6. **Configuration is executable truth.** User-facing limits such as `max_iterations`, declared MSRV,
   config keys, and batch atomicity are wired to the behavior they claim to control and pinned by tests.

## Implementation waves

### Wave 1 — immediate release blockers

- Native/plugin HTTP use redirect-disabled clients and a shared bounded manual-follow loop. Each `Location`
  is resolved, scoped through the existing network guard, and compared by scheme/host/effective port
  before forwarding sensitive headers.
- `System` owns bounded concurrent stdout/stderr drains and child termination/reaping. Unix process-group
  cleanup is used where the existing process abstraction can support it without creating a bypass path.
- Flow termination fixes make session-shape repair and usage accounting unconditional. The agent loop's
  repeat budget is supplied by engine configuration rather than duplicated in the asset.
- Eval result validity becomes explicit. Scoring first compares validity/correctness, then cost among
  valid comparable results; absent telemetry stays absent.

### Wave 2 — identity and structured cancellation

- `flux-system` exposes one workspace-confined subject-resolution operation so runtime policy and guarded
  IO share the same path identity. Plugin host filesystem capabilities use that same guarded surface.
- Plugin framed callbacks gain a cancellation-safe owner. A half-finished exchange either completes in
  the driver or invalidates the child; no next operation reuses ambiguous framing state.
- Flux-Lang race/timeout branches use scoped cancellation semantics. Cleanup nodes run on all exits and
  the analyzer applies the same binding-disjointness rule to concurrent race branches as parallel ones.

### Wave 3 — bounded cost and contract truth

- File/HTTP body limits move before allocation, eval trials gain a configurable concurrency ceiling,
  retries respect server delay signals with bounded jitter, and nested usage aggregates exactly once.
- Event backends implement batch append as one transaction; config parsing reports unknown keys; workspace
  manifests inherit a tested MSRV; terminal-bench telemetry distinguishes unavailable from zero.
- The adaptive-thinking decision becomes an explicit setting/capability decision, not a side effect of
  attaching a sink. A focused regression test or measurement decides the default.

## Verification

Each bullet starts with a failing regression test at the narrowest owning crate. Security tests exercise
the denied destination, not merely client configuration. Cancellation tests prove both prompt return and
a successful subsequent operation. The final gate is the repository contract: workspace build/tests,
clippy with warnings denied, rustfmt, and `flux-codegate`; self-improvement flow/docs sync tests run when
that path changes.

## Risks and mitigations

- **Canonicalization of not-yet-created paths:** canonicalize the nearest existing ancestor, retain and
  validate the missing suffix, then let the guarded open perform its normal race-resistant confinement
  check. Tests cover symlinked ancestors and create paths.
- **Redirect compatibility:** preserve ordinary same-origin redirects and method rules, but cap hops and
  reject unsupported/ambiguous locations with an actionable error.
- **Cancellation complexity:** prefer explicit ownership and poisoning over pretending a dropped future
  stopped external work. Tests always include a post-cancel reuse attempt.
- **Benchmark throughput:** concurrency defaults conservatively and result ordering remains stable so
  audit logs and comparisons are reproducible.
