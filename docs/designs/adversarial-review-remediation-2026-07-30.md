# Adversarial review remediation — 2026-07-30

## Context

Three independent reviews inspected commit `cb3bb057` and reached the same deployment verdict:
Flux is suitable for constrained pilots, but is not yet a self-sufficient security boundary for
unattended valuable work. This design turns every actionable finding into one remediation program
without duplicating stories that were already on the board.

Review evidence:

- [`primary`](../reviews/2026-07-30-independent-adversarial-review-primary.md)
- [`review A`](../reviews/2026-07-30-independent-adversarial-review-a.md)
- [`review B`](../reviews/2026-07-30-independent-adversarial-review-b.md)

## Finding-to-story traceability

| Finding | Story |
| --- | --- |
| Fleet A2A guard/connect DNS TOCTOU and redirect revalidation | C-256 |
| Plugin HTTP/OAuth/TCP guard/connect DNS TOCTOU | C-257 |
| Model-selected, sandbox-exempt `eval_run` executable receives provider credentials | C-258 |
| Release jobs execute unauthenticated bootstrap tools; core artifacts lack signatures/provenance | C-259 |
| REST SSE work survives disconnects and buffers through an unbounded channel | C-260 |
| Daemon lacks principal-aware rate, concurrency, and spend controls | C-261 |
| Sandbox and sandbox-network confinement are not fail-closed defaults for unattended surfaces | C-262 |
| The direct-I/O guard excludes a production model-facing pack and is lexical rather than structural | C-263 |
| No fuzzing, SAST, Miri/sanitizer, or release-attestation assurance lane | C-264 |
| Project role shadowing gives auto-approved `flux review` children write authority | C-265 |
| `git_diff` can invoke configured external programs | Existing C-218 |
| Provider-stage failure is reported as a successful turn | Existing C-226 |
| Published risk table silently skips non-built-in operations | Existing C-233 |
| Catalog registration-seam census scans only `execution.rs` | Existing C-234 |

The reviews' bus-factor ratings and requests for an external penetration test are risks, not code
changes. C-264 creates repeatable independent-input automation, but does not pretend CI can solve
maintainer continuity or substitute for a human audit.

## Ordering

1. Close active containment defects first: C-256, C-257, C-258, and C-218.
2. Close exposed-service lifecycle and abuse paths: C-260 and C-261.
3. Make delivery and deployment fail more honestly: C-259, C-262, and C-226.
4. Make recurrence mechanically harder: C-263, C-264, C-233, and C-234.

Work may proceed in parallel where files do not overlap. Every behavioral story begins with a test
that demonstrates the current failure. No story is complete until its scoped checks and the root
gate are green, its frontmatter is `done`, and the engineering changelog records the change.

## Security decisions

- A DNS guard that does not bind the connection to the vetted addresses is not a guard. Network
  adapters must consume `guard_url_scoped_pinned` (or an equivalent API that cannot discard the
  vetted addresses), fail closed on an empty vetted set, bypass ambient proxies, and re-authorize
  every redirect hop. An ambient proxy is a separate, unvetted connection peer and cannot inherit
  the destination's authorization.
- A plugin Unix-socket grant names path segments, not an arbitrary prefix and suffix. Reject dot
  components before matching, and never let one `*` consume a path separator that the manifest did
  not grant.
- Sandbox exemptions are for host-selected trusted executables only. A model-controlled field must
  never select an exempt executable, and provider credentials must be passed by reference or by a
  narrowly justified allow-list rather than copied wholesale into child environments.
- Auto-approval does not waive resource governance. Server limits are keyed to the authenticated
  principal/realm and must bound queued work as well as request bodies.
- Producer-side checksums from the same workflow are integrity metadata, not an independent trust
  root. Bootstrap tools must be content-authenticated before execution, and release consumers need
  a signature or verifiable provenance rooted outside the artifact set itself.
- Names invoked by an embedded security protocol are not repository extension points. Built-in
  strict review uses only its embedded `tools: []` roles and inherits the same fail-closed sandbox
  floor as every other auto-approved surface; project-specific checks require a separate typed seam.
- A structural source gate must follow local callable aliases as well as imports and type aliases;
  otherwise moving a forbidden constructor into a `let` binding turns the same effect invisible.

## Closure proof

After all children are done, re-run three independent adversarial passes against the resulting exact
working tree. The epic closes only if no review can reproduce the containment defects and any
residual production caveat is explicitly documented rather than silently accepted.
