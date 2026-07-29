---
id: C-191
title: "Registry-wide ToolSpec invariant test — a mutating op must not keep a read-only risk class"
pillar: Core
status: in-progress
epic: security-assurance
design: docs/designs/security-assurance.md
note: "REVIEW — the policy engine is only as good as each op's self-declared metadata; ToolSpec::read_only() is a coherent preset but NOTHING asserts the combination stays coherent as an op gains effects"
---

# Registry-wide ToolSpec invariant test — a mutating op must not keep a read-only risk class

## Goal
Approval gating is driven by each operation's self-declared `effects`, `risk`, `idempotency` and
`access`. Those declarations are trusted and never cross-checked, so an op that starts from
`ToolSpec::read_only()` and later gains a mutating effect without upgrading its risk compiles,
ships, and clears a lower approval bar than it deserves. Turn that standing trust assumption into a
gate that runs on every build.

## Acceptance
- [x] A test iterating **every** registered `ToolSpec` asserts the metadata combination is coherent.
      At minimum: a spec declaring any non-`Read` effect must not carry `Risk::Low`; a destructive
      effect must carry `Risk::Destructive`; a spec with mutating effects must not be
      `Idempotency::Idempotent` unless explicitly annotated as safely repeatable.
- [x] The exact invariant set is agreed and written down in the test as prose before it is coded —
      an invariant nobody can state is an invariant nobody can enforce.
- [x] Failing-first proof: a deliberately mis-declared spec (mutating effect, `Risk::Low`) fails the
      test.
- [x] The test covers plugin-supplied specs at registration, not only built-in ops — plugins declare
      the same metadata and are trusted the same way.
- [x] Any legitimate exception is an explicit allowlist entry with a comment, not a loosened rule.

## Progress
- **Invariant set agreed and encoded** in `crates/flux-spec/src/coherence.rs` — three floors over
  the notion of a *consequence-bearing* spec, which is deliberately the same shape `flux-flow`'s
  `gather_safe` refuses (effect branch, plus an access branch that applies only to an empty effect
  set). I1 risk floor, I2 destructive floor, I3 repeatability floor; full derivation in the module
  docs and restated as prose at the head of the test.
- **Rejected the literal reading** "any non-`Read` effect must not be `Risk::Low`": `Effect` mixes
  directional effects with carriers, so `read` itself declares `[Read, Filesystem]` and the literal
  rule would condemn every file read. The rule is derived from the semantics instead.
- **Enforced in two places, for two reasons.** Built-ins: a build-time test over the assembled
  registry (`crates/flux-tools/tests/toolspec_invariants.rs`) — "a gate that runs on every build".
  Plugin-supplied ops: `flux_plugin`'s `validate_op_coherence`, at load, since a plugin's metadata
  is authored outside this repo and there is no compile-time list of its ops to walk.
- **Not enforced in `ToolRegistry::try_register_from`** — tried, reverted. Registration is not a
  trust boundary here: first-party tests deliberately register incoherent specs to prove the
  downstream gates still hold (`flux-runtime`'s `a_write_below_the_threshold_auto_approves`
  registers a `Risk::Low` write precisely to assert `RiskApprover` auto-approves it). Refusing
  those would have deleted the defence-in-depth tests rather than strengthened them.
- **12 real built-in mis-declarations corrected** (`append`, `git_unstage` risk floors; `write`,
  `git_stage`, `git_status`, `git_diff`, `git_log`, `cargo_{build,check,clippy,fmt}`,
  `go_{build,vet}` repeatability floors). No invariant was weakened to accommodate any of them.
- **Allowlist: 3 entries**, all I1, all the same narrow shape — `git_status` / `git_diff` /
  `git_log` run a fixed argv and only observe. I3 still applies to them.

### Follow-up (not this story)
Ops outside the built-in pack that the invariants also flag, found while running the workspace and
deliberately left alone — each needs its own judgement call:
`ai.*` + `detect_intent` (`[Network]` without `Read`, `Risk::Low` — arguably should declare `Read`,
but that would make them gather-safe, a behaviour change); `web.fetch` / `web.crawl` (same shape);
`improve_log` (`[Write, Filesystem]` at `Risk::Low` — the same class of drift as `append`, and the
most clear-cut of these).

## Notes
- Verified: `ToolSpec` carries `effects`, `risk`, `idempotency`, `access`
  (`crates/flux-spec/src/lib.rs:262-274`). `Risk` is `Low | Medium | High | Destructive` (`:205-213`).
- The field design is **sound** and this story should not be read as fixing a broken default:
  `risk` and `idempotency` carry no `#[serde(default)]`, so a plugin-supplied spec cannot omit them,
  and `ToolSpec::read_only()` (`:281-295`) is an internally consistent preset — `Effect::Read` +
  `Risk::Low` + `Idempotent`. The gap is drift over time, not a bad starting point.
- Verified absent: no registry-wide iteration or assertion over op metadata exists in
  `crates/flux-tools/` or `crates/flux-policy/`.
- Kept at `backlog` rather than `ready`: unlike its siblings this one needs the invariant set agreed
  first, and guessing it would produce a test that encodes today's accidents as tomorrow's rules.
- Source: [2026-07-29 review](../../reviews/2026-07-29-security-posture-desk-review.md), finding
  "Default policy is not equivalent to 'no side effects'" — verified, and sharpened here with the
  concrete drift mechanism.
