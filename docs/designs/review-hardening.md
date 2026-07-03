# Design: Review hardening — 0.2.11 diff-review residuals

**Status:** implemented 2026-07-03 (all 12 stories done, one sub-agent per story in parallel; every
finding grounded against flux invariants before filing; every fix shipped with a failing-first test) ·
**Pillar:** Agent / Core / Language (cross-cutting) · **Layer:** L0–L6 (flow, runtime,
providers, plugin host, markdown, server) · **Owner:** Timo · **Stories:**
security/correctness — [C-27](../stories/C-27-nested-destructive-refire.md) ·
[L-32](../stories/L-32-envelope-denial-classification.md) ·
[C-28](../stories/C-28-codex-ws-fallback-hardening.md) ·
[L-33](../stories/L-33-markdown-writer-fence-length.md) ·
[D-52](../stories/D-52-scram-iteration-bound.md);
enforcement/robustness — [A-26](../stories/A-26-turn-budget-cumulative.md) ·
[A-27](../stories/A-27-identical-plan-skip-stall-guard.md) ·
[A-25](../stories/A-25-nested-delegation-cap-scope.md) ·
[L-30](../stories/L-30-composite-surfacing-transitive.md) ·
[C-29](../stories/C-29-a2a-queued-session-retention.md);
hygiene — [L-31](../stories/L-31-cap-scope-parallel-position.md) ·
[L-34](../stories/L-34-markdown-parser-thematic-break.md)

## Why

An xhigh workflow-backed code review of the 0.2.11 diff (192 changed files) fanned out six finder
angles → 38 candidates → an independent verifier per (file, line) → 36 confirmed → 15 reported after
merging duplicates. Left there, the report ranked four "enforcement-boundary bypass" findings as the
gravest defects in the codebase. **That ranking did not survive grounding.** The finders reasoned from
generic correctness, not from flux's architecture, so several "security bypasses" were legibility
gaps and one "regression" was the intended design.

Before filing, each surviving finding was re-verified by an independent Opus reader **told the specific
flux invariant to test it against**:

- The **safety envelope** (authorization → approval → guarded IO) is the security boundary. Every
  effect traverses it; there are no bypass paths.
- **Evidence-gated op surfacing** (A-04) and the **gather gate** (A-13/L-29) control what is
  *advertised to the model*. They are legibility / context-hygiene contracts layered *on top of* the
  envelope — a hidden or un-advertised op that gets called anyway still hits approval + guarded IO.
- **Plugins are references-only** and never read env or hold credentials; the host terminates auth
  (endpoint epic, D-25..D-32). Non-secret connection metadata lives in the DSN.
- **Capability scoping** (`with_tools` / CapScope, L-11) is an authorization primitive: narrow-only on
  descent.

## Method

Six finder angles + a cleanup sweep produced 38 candidates; each was verified at its exact location by
an independent agent (two candidates refuted in-review — a flux-app park-attribution "race" that does
not exist because deliveries serialize through one guard, and an `include_str!` non-issue). The 15
reported findings then went through a per-finding grounding pass against the invariants above. The
outcomes below are the point of this epic: **what the raw review got wrong is as important as what it
got right.**

## Grounding outcomes (what changed vs. the raw review)

- **WITHDRAWN — SQL_USERNAME "regression"** (`plugins/sql/src/main.rs`). The finder called the D-31
  removal of `SQL_USERNAME`/`MYSQL_USERNAME` a silent regression. It is the intended design: a username
  is **non-secret connection metadata**, `grep` confirms **zero** `env::var` reads under `plugins/`, and
  the endpoint model puts the user in the DSN (`postgres://alice@host/db`, host-resolved via the
  non-secret `config` read). The old `host.secret("username")` path wrongly classified a username as a
  secret; D-31 corrected that. Not a defect — at most a one-line migration note the manifest already
  carries.
- **DOWNGRADED to correctness — composite hidden-op** (`crates/flux-flow/src/registry.rs`). Mechanism
  confirmed: `hidden_ops_in` does not recurse into composite bodies, so a turn-registered composite can
  name a non-advertised op. But `bash`-via-composite **still traverses approval + guarded IO**, and the
  A-13 gather gate *is* honored transitively (the composite must declare `LocalSystem`, which trips
  `mutating_ops_in`). Only A-04's surfacing/legibility gate is non-transitive → L-30, low-medium, framed
  as a gate-completeness fix, **not** a security bypass.
- **DOWNGRADED to latent — cap-scope in `parallel`** (`crates/flux-flow/src/runtime.rs`). Concurrency is
  real (unlike the refuted park finding: `join_all` shares one executor and one cap-scope stack), so the
  soundness gap is genuine. But **zero shipped `.flux` programs use `with_tools`**, and the strict-review
  flow scopes via sub-agents. Latent hazard → L-31, fixed the way flux already handles
  non-composing-in-concurrent-position constructs: static rejection.
- **BOUNDED — nested-delegation cap-scope escape** (`crates/flux-orchestrate/src/lib.rs`). Real
  authorization non-transitivity (a grandchild is spawned with an empty cap-scope stack and re-subsets
  the full base registry), but reachable **only** when a Rust embedder opts into `with_max_depth(≥2)`;
  the default `max_depth = 1` keeps every child a leaf, and no CLI/app/SDK surface exposes deeper
  nesting → A-25, security-but-opt-in, forward-looking hardening for D-05.
- **RE-CHARACTERIZED — a2a session prune** (`crates/flux-server/src/a2a.rs`). The mint-before-gate
  ordering is confirmed, but the claimed outcome (queued request fails with JSON-RPC -32603) is
  **refuted**: `run_turn` completes on a pruned session (no FK, `read_context` defaults). The real
  defect is a mid-flight prune that orphans the session's event rows (they escape the TTL sweep forever)
  and drops that turn from usage rollups → C-29, low-medium retention/accounting, not liveness.
- **CONFIRMED as-is — destructive-scope leak** (`crates/flux-runtime/src/lib.rs`). Genuine
  SECURITY-CRITICAL approval-gate bypass: a bare `AtomicU32` depth counter lets a nested
  `destructive:false` plan ride an outer plan's destructive disclosure, so a runtime-assembled `rm -rf`
  dispatches with no re-fire → C-27, priority 1. Reachable via reflexive `run_plan`; does not cross
  sub-agent boundaries.

## Findings → stories

Severity: 🔴 security / silent-correctness · 🟠 robustness / durability · 🟡 hygiene.

- **🔴 C-27 — nested-plan destructive re-fire.** The C-12 gate keys on a shared depth counter, so a
  nested undisclosed destructive op rides an ancestor's disclosure and executes unprompted. (residual of C-12)
- **🔴 L-32 — `is_envelope_denial` misclassifies real failures.** Denial is detected by prefix-matching
  the tool's *content* (`` `{op}` denied by ``), so an op that actually ran and relayed that text is
  escalated to a fatal, never-retried `FlowError::Denied`, killing the turn instead of feeding a
  repairable failure back to the loop. (flux-flow runtime)
- **🔴 C-28 — codex WS transport defeats the guaranteed HTTP fallback.** Three ways: a non-char-boundary
  byte slice panics on a >300-byte error payload; `connect` waits for the first frame with no timeout so
  a blackholing proxy hangs the turn; and a clean close *before* the terminal event silently truncates
  the response. All violate StreamTransport's fail-fast contract. (residual of C-07)
- **🔴 L-33 — markdown writer emits an early-closing fence.** Fence length is computed from backtick runs
  even when a tilde fence is emitted, so a body with a `~~~` run closes the fence early and re-parsing
  splits/loses the code block — the round-trip contract is broken. (residual of L-02)
- **🔴 D-52 — SCRAM PBKDF2 trusts the server iteration count.** The host-terminated PG handshake feeds
  the server-supplied `i=` (up to `u32::MAX`) straight into PBKDF2 with no bound; a malicious/MITM'd
  endpoint pegs a CPU core for minutes (the read timeout doesn't cover pure computation). (residual of D-31)
- **🟠 A-26 — per-turn token budget measures the wrong quantity.** The A-10 budget compares against
  replace-style usage (only outputs sum), so it tracks last-call context occupancy, not cumulative billed
  tokens, and never trips on a runaway multi-call loop — exactly the cost it exists to cap. (residual of A-10)
- **🟠 A-27 — identical-plan skip bypasses the stall guard.** The A-05 skip returns its transcript
  without calling `guard_transcript`, so a model re-emitting the same succeeded plan spins the full
  25-round budget instead of force-stopping. (residual of A-05 / A-20)
- **🟠 A-25 — cap-scope not transitive across sub-agent delegation.** Under opt-in `max_depth ≥ 2` a
  grandchild is spawned with an empty cap-scope stack over the full base registry, so an ancestor
  `with_tools` ceiling isn't enforced two hops down. (residual of L-11 / D-05)
- **🟡 L-30 — surfacing enforcement not transitive through composites.** A turn-registered composite can
  name a non-advertised op; make A-04's check recurse into composite bodies (or validate them against the
  advertised set), symmetric with the gather gate. Envelope unaffected. (residual of A-04 / L-04)
- **🟡 C-29 — queued a2a session pruned mid-flight.** A session minted before the turn gate can age past
  a low TTL while queued and be swept, orphaning its events (unprunable) and dropping its spend from
  rollups. (residual of C-18)
- **🟡 L-31 — reject cap-scope in a concurrent branch.** `with_tools` in a `parallel`/`race` branch
  mutates one shared cap-scope stack across await points; statically reject it, mirroring
  `check_await_position` / `check_checkpoint_position`. Latent (unused today). (residual of L-11)
- **🟡 L-34 — spaced thematic break after a list item.** `parse_list` doesn't check `is_thematic_break`,
  so `- - -` after a bullet parses as a nested empty list instead of a rule. (residual of L-02)

Each ships with the failing-first test named in its Acceptance. Order: security/correctness →
robustness → hygiene, mirroring the [library-hardening](library-hardening.md) epic that preceded it.
