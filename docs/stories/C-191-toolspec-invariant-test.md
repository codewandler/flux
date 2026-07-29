---
id: C-191
title: "Registry-wide ToolSpec invariant test — a mutating op must not keep a read-only risk class"
pillar: Core
status: done
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
- [x] A test iterating every **built-in** registered `ToolSpec` asserts the metadata combination is
      coherent. At minimum: a spec declaring any non-`Read` effect must not carry `Risk::Low`; a
      destructive effect must carry `Risk::Destructive`; a spec with mutating effects must not be
      `Idempotency::Idempotent` unless explicitly annotated as safely repeatable.
      **Scope narrowed from "every registered `ToolSpec`" during implementation** — see
      "Why the catalog scope was narrowed" below. The full production catalog is C-191b.
- [x] The exact invariant set is agreed and written down in the test as prose before it is coded —
      an invariant nobody can state is an invariant nobody can enforce.
- [x] Failing-first proof: a deliberately mis-declared spec (mutating effect, `Risk::Low`) fails the
      test.
- [x] The test covers plugin-supplied specs at registration, not only built-in ops — plugins declare
      the same metadata and are trusted the same way. Applied at plugin **load** over the projected
      spec; reports rather than refuses (see "Why the plugin gate warns").
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
  Plugin-supplied ops: `flux_plugin`'s `op_coherence_warnings`, at load, since a plugin's metadata
  is authored outside this repo and there is no compile-time list of its ops to walk.
- **The published risk column is now held to the registry.** `crates/flux-flow/docs/ops-reference.md`
  documented `append` and `git_unstage` as `Low`; AGENTS.md requires the reference to mirror the
  catalog and nothing enforced it. Corrected, and pinned by a test — the risk tier is the exact
  field this story is about, so a doc that contradicts it is the same defect one layer out.
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

### Why the plugin gate warns instead of refusing
The first cut failed the whole manifest closed at load. Run against the plugins this repo actually
ships it dropped all 24 `kubernetes` ops off the agent surface over two mis-declared idempotency
fields, and dropped an installed third-party plugin entirely over 51 operations tagging
`delete`/`money` below `Risk::Destructive` — on a process that still exits 0, so the capability just
vanished silently. Refusal is the wrong remedy for an under-declaration: it removes the operation
rather than gating it, it breaks plugin authors with no correction window, and it is *less* safe
than loading loudly. The load path now names every violation on every run and loads. The in-repo
half of that population is fixed here (`plugins/kubernetes`, 2 ops); the hard cutover needs a
deprecation window and belongs to its own story.

### Why the catalog scope was narrowed
The gate covers `try_register_builtins`. The production registry
(`crates/flux-cli/src/execution.rs:1300-1445`) additionally assembles the cognition pack,
`flux_eval::try_register_eval_ops`, `try_register_reflect`/`try_register_flows`/`try_register_render`,
`flux_web::try_register_web`, datasource + endpoint ops, `TaskTool`, and config-authored model
stages — all reaching the same `Executor::dispatch` and the same `RiskApprover`. A gate over that
catalog cannot live in `flux-tools` (`flux-web`/`flux-eval`/`flux-cognition` sit above it); it has
to live in `flux-cli`.

Extending it was not attempted here because closing it requires a **safety-posture decision this
story did not scope**: eight of the eleven violations below are `Effect::Network` without
`Effect::Read` at `Risk::Low`. Adding `Read` would make them honest *and* make them gather-safe —
the adaptive loop could then fetch URLs and call models pre-approval. Raising them to `Medium`
would put an approval prompt in front of every model call. Both are product changes. The invariants
are not the problem, the catalog is; the debt is real and itemised:

| op | declaration | violates | source |
|---|---|---|---|
| `web.fetch` | `[Network]` @ Low + Idempotent | I1, I3 | `crates/flux-web/src/fetch.rs:90,101` |
| `web.crawl` | `[Network]` @ Low + Idempotent | I1, I3 | `crates/flux-web/src/crawl.rs:155,180` |
| `browser.snapshot` | `[Browser]` @ Low | I1 | `crates/flux-web/src/browser.rs:1014` |
| `browser.close` | `[Process]` @ Low | I1 | `crates/flux-web/src/browser.rs:1131` |
| `consult` | `[Network]` @ Low | I1 | `crates/flux-cognition/src/consult.rs:121` |
| `ai.*` / `synth` | `[Network]` @ Low | I1 | `crates/flux-cognition/src/lib.rs:113,183` |
| `detect_intent` | `[Network]` @ Low | I1 | `crates/flux-tools/src/reflect.rs:183` |
| config-authored model stages | `[Network]` @ Low | I1 | `crates/flux-tools/src/reflect.rs:87` |
| `improve_log` | `[Write, Filesystem]` @ Low | I1 | `crates/flux-eval/src/ops.rs:524` |
| `gate_check` | `[Process, LocalSystem]` + Idempotent | I3 | `crates/flux-eval/src/gate.rs:71` |
| `endpoint.import` | `[LocalSystem]` @ Low + Idempotent | I1, I3 | `crates/flux-capabilities/src/endpoint/ops.rs:397` |

`improve_log` is this story's own title case — a mutating op that kept a read-only risk class — and
the narrowed gate cannot see it. Two further open registration seams for that story to cover:
`flux_sdk::FlowClient::try_register_op`/`try_register_pack` (`crates/flux-sdk/src/flow.rs:313,331`)
and the sub-agent `child_base` registry (`crates/flux-cli/src/execution.rs:1281`).

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
