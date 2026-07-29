---
title: Flux envelope integrity — is there a path to effect that skips the envelope?
date: 2026-07-29
kind: internal-review
lens: envelope-integrity
method: >
  source-level desk review of the dispatch chain, the built-in tool catalog's effect declarations,
  and the guarded-IO seam. SQLite semantics were pinned empirically in an isolated scratch database
  (not against flux). No fuzzing, no exploitation of flux, no runtime testing of flux, no guard was
  weakened to test reachability. The plugin host, flux-lang interpreter and server router surfaces
  were NOT audited at this lens — see Open questions.
reviewer: agent
subject:
  repo: codewandler/flux
  version_in_tree: 0.33.1
  published_release_at_review: v0.33.1
  workspace_crates: 38
  commit: f8e90d7
overall_rating: 6/10
verdict: The envelope holds wherever it is reached — but reaching it is a per-tool promise with no mechanical check, and one default built-in already breaks it.
ratings:
  security_architecture: 8/10
  secure_defaults: 5/10
  implementation_quality: 7/10
  security_assurance: 4.5/10
  release_supply_chain: 6.5/10
  product_maturity: 5/10
  community_bus_factor: 2/10
  production_readiness: 4.5/10
verification:
  status: verified against tree at 0.33.1 (f8e90d7) on 2026-07-29
  outcome: one confirmed bypass of guarded IO in a default-registered built-in; envelope chain itself found sound on every path examined
  material_errors: none
top_findings:
  - "`sqlite_query` reaches arbitrary-path file creation via `VACUUM INTO`, outside guarded IO, authorized as a workspace read"
  - "`is_write_sql` is a prefix denylist documented as an allowlist; a leading SQL comment defeats it entirely"
  - "The two invariants this lens rests on have no mechanical enforcement — `validate_authority_contracts` checks declaration coherence, not fidelity to `execute`"
  - "`file_stat` reads the entire file a second time and discards the result (dead code)"
---

## Verdict

**The envelope itself is sound. The defect is at its perimeter: a tool's declared effects are taken
on trust, and nothing in the tree checks that declaration against what the tool can actually do.**

The baseline review ([2026-07-29 security posture](2026-07-29-security-posture-desk-review.md))
rated `security_architecture` 8/10 from the outside, reasoning from docs. Reading the chain from the
inside confirms that number and in places exceeds it — the shared `gate` implementation between the
live and authorize-only paths, the capability-scope check ordered *before* pre-tool hooks, and
filesystem subject normalization against in-workspace symlink aliasing are all better than the
external reviewer could see.

That makes the finding below more interesting, not less. The chain is not what failed. What failed is
the assumption the chain is built on — `AGENTS.md:16`'s *"there are no bypass paths"* is enforced by
review discipline, not by the compiler or the test suite. One default-registered built-in already
demonstrates the gap.

## Ratings

| Area | Rating | Δ vs baseline | Assessment |
| --- | ---: | :---: | --- |
| Security architecture | **8/10** | = | Dispatch chain is genuinely non-bypassable *for calls that reach it* |
| Secure defaults | **5/10** | = | Out of lens; the offending tool being a default built-in is a nudge down, not a change |
| Implementation quality | **7/10** | ▼ 0.5 | One hand-rolled jail that is wrong; one dead-code path |
| Security assurance | **4.5/10** | ▼ 0.5 | The specific absence: no lint or test enforcing the IO/classification invariants |
| Release/supply chain | **6.5/10** | = | Not examined at this lens — carried from baseline, not re-verified |
| Product maturity | **5/10** | = | Carried from baseline |
| Community/bus factor | **2/10** | = | Structural, not fixable by a code change |
| Production readiness | **4.5/10** | ▼ 0.5 | A confirmed guarded-IO bypass ships in the default catalog |

## Strengths (verified, not assumed)

These are stated as specifically as the criticisms because they are the reason the finding below is
an outlier rather than a pattern.

- **`authorize` and `dispatch` cannot drift.** Both consume one `gate` implementation
  (`crates/flux-runtime/src/lib.rs:3259`); the authorize-only path is *synchronous*, which makes
  "no execution side effect" structural — `Tool::execute` and `Approver::request` are both `async`
  and therefore unreachable from it (`:3207`).
- **Capability scope is checked before hooks.** `cap_scope_gate` runs at `:3403`, ahead of the
  pre-tool hook loop at `:3416`, so a hook never observes or rewrites the input of a call the active
  `with_tools` allowlist already forbids.
- **Filesystem subjects are normalized to physical identity before matching** (`:3282-3300`). Without
  this, an allow rule like `read(allowed/**)` could reach `secret/**` through an in-workspace symlink
  while guarded IO correctly kept both inside the jail. The comment at `:3279` states the attack it
  closes.
- **Unscoped writes are forced to approval** (`:3488`) — a write tool reporting no subjects prompts
  rather than resolving to a wildcard authorization.
- **Undisclosed destructive ops re-fire the gate inside an approved plan scope** (`:3502-3509`),
  keyed on the innermost scope's own disclosure flag, so a nested plan approved `destructive:false`
  re-prompts even when an outer scope disclosed.
- **The op cache is probed only after every gate passes** (`:3569-3579`), and excludes anything
  approval-shaped, destructive, non-idempotent, or carrying a non-`Read` effect.
- **No direct `Tool::execute` call exists in production code.** Every `.execute(` hit outside
  `flux-runtime` is in a `#[cfg(test)]` module (`flux-web/src/http.rs`, `flux-cognition/`,
  `flux-web/src/fetch.rs` — all test-gated).
- **The workspace root is not model-reachable.** Every non-test `Workspace::new` /
  `with_workspace` site is host assembly (`flux-cli/src/execution.rs:756,1481,2036`,
  `flux-sdk/src/lib.rs:763`, `flux-cli/src/app_cmd.rs:530`). No tool can re-root the jail.
- **`tools: []` is correctly distinguished from absent.** `crates/flux-agent/src/role.rs:70` —
  `None` inherits all, `Some([])` grants none; asserted at `:341`.
- **JS hooks have no filesystem surface.** `crates/flux-plugin/src/hooks.rs:96-97` injects exactly
  one global, `__flux_ctx`, as a JSON *string*. Runaway and memory-bomb hooks are killed
  (`:155`, `:165`).

## Findings

### 1 — HIGH · `sqlite_query` creates files at arbitrary absolute paths, outside guarded IO

`crates/flux-tools/src/extra.rs:309` (`SqliteQueryTool::execute`)

The tool declares itself read-only:

```rust
// crates/flux-tools/src/extra.rs:278-282
effects: vec![Effect::Read, Effect::Filesystem],
risk: Risk::Low,
idempotency: Idempotency::Idempotent,
access: vec![AccessKind::Filesystem],
```

With no `Effect::Write`, the default derivation at
`crates/flux-runtime/src/lib.rs:2335-2341` emits **`workspace_read(<db path>)`** and nothing else.
The `unscoped_write` approval trigger (`:3488`) tests `spec.effects.contains(&Effect::Write)` and is
therefore false. So the policy engine authorizes a read of one workspace file, and no approval gate
fires.

The tool then opens the database with `rusqlite` **directly** — not through `flux-system`
(`extra.rs:341-348`) — and executes the model-supplied `sql` string via `prepare` + `query`
(`:350-360`). `SQLITE_OPEN_READ_ONLY` is the only thing standing between the model and the
filesystem.

That flag does not cover `VACUUM INTO`, which is read-only *with respect to the source database* and
writes a fresh file at any path SQLite can reach. `VACUUM` is absent from the `is_write_sql` denylist
(`:209-212`).

Pinned empirically against an isolated scratch database (SQLite semantics only — flux was not run):

```
INSERT blocked by read-only flag: OperationalError
VACUUM INTO fresh abs path: True 8192 bytes
overwrite existing: refused — output file already exists
```

**The primitive:** file *creation* at any absolute path the flux process can write, with partially
attacker-controlled content (table names and row values land in the page bytes as plain text), from
an op classified `Effect::Read` / `Risk::Low`, with no approval prompt, and with the path never
passing through `flux-system`'s workspace confinement, symlink rejection, or path canonicalization.

**Bounding it honestly:** the target must not already exist — `VACUUM INTO` refuses to overwrite —
so this is not arbitrary file *modification*, and content control is partial rather than byte-exact.
It is nonetheless a direct counterexample to `AGENTS.md:16` and `docs/architecture.md:157`
(*"guarded IO … the **only** place real filesystem / process / network IO happens"*).

`sqlite_query` is registered by `try_register_extra` (`extra.rs:597-609`), called unconditionally
from `try_register_builtins` (`crates/flux-tools/src/lib.rs:231`); the default-catalog test at
`lib.rs:4200` asserts its presence. It is in the default catalog, not behind a group signal — unlike
`bash`, which is gated by the `shell` group (`lib.rs:4213`).

### 2 — MEDIUM · `is_write_sql` is a prefix denylist documented as an allowlist

`crates/flux-tools/src/extra.rs:207-218`

The description promises *"Only SELECT and PRAGMA statements are allowed"* (`:269-271`) and the
refusal message repeats it (`:322`). The implementation is the inverse: a denylist of ten keywords
matched with `starts_with` on `sql.trim_start().to_ascii_uppercase()`.

Two consequences:

- **`trim_start()` strips whitespace, not comments.** `/*x*/ INSERT …` does not start with `INSERT`
  and passes the check — verified: `denylist sees '/*X*/ ' -> starts_with(INSERT)? False`. SQLite
  parses the comment and executes the statement.
- **The denylist is largely redundant with the open flag.** `SQLITE_OPEN_READ_ONLY` already blocks
  `INSERT`/`UPDATE`/`DELETE`/`DROP`/`ALTER`/`CREATE`/`REPLACE`. The entries that the flag does *not*
  cover are `ATTACH` (bounded in practice — `prepare`+`query` executes only the first statement and
  the connection is dropped per call, so an attach cannot be followed by a select) and, missing
  entirely, `VACUUM`.

So the keyword check contributes almost no defense where it works, and is bypassable where it would
matter. A statement-type allowlist parsed from the prepared statement — or refusing any `sql` whose
first token after comment-stripping is not `SELECT`/`WITH`/`PRAGMA`/`EXPLAIN` — is the shape the
documentation already claims.

### 3 — MEDIUM · The invariants this lens depends on have no mechanical enforcement

`docs/architecture.md:169-170` states two invariants:

> - All IO goes through `flux-system`; tools never touch `std::fs`/`std::process` directly.
> - Every tool runs through `Executor::dispatch`; nothing calls a tool's `execute` directly in prod.

The second holds in the tree (verified above). The first does not — finding 1 *is* its violation,
and it went unnoticed because nothing checks it.

What exists and what it actually covers:

- **`flux-codegate`** (`.github/workflows/ci.yml:54`) is a crate-*dependency*-direction lint. It
  cannot see `std::fs` use inside an allowed dependency edge.
- **`ToolRegistry::validate_authority_contracts`** (`crates/flux-runtime/src/lib.rs:1583-1596`)
  calls `authority_requirements(&json!({}), …)` on each tool and checks it returns `Ok`. It validates
  that a declaration *derives without error on empty input* — internal coherence. It has no way to
  know whether the declaration is faithful to what `execute` does, which is precisely the property
  finding 1 breaks.

Grepped for and **not found** anywhere in `crates/`: any test or lint named for or asserting the
no-direct-IO invariant (`no_direct_io`, `no_std_fs`, "never touch std::fs", "all IO goes through" —
zero hits outside the architecture doc's own prose).

This is the structural finding. A denylist-style lint over model-facing tool crates (`flux-tools`,
`flux-web`, `flux-capabilities`) rejecting `std::fs`/`std::process`/`rusqlite::Connection::open*`
outside `#[cfg(test)]` would have caught finding 1 at authoring time, and is the cheapest thing on
this list.

### 4 — LOW · `file_stat` reads the entire file twice and discards the second read

`crates/flux-tools/src/extra.rs:96-107`

```rust
let mode_str = ctx.system().read_file_bytes(path).await.ok()
    .map(|_| { … "(mode unavailable)".to_string() })
    .unwrap_or_else(|| "(mode unavailable)".to_string());
let _ = mode_str; // suppress unused warning — we surface it as a note below
```

Both branches produce the identical string, the value is then explicitly discarded, and the trailing
comment describes a "note below" that does not exist in the emitted result (`:109-119`). The net
effect is a second full `read_file_bytes` of the target per `file_stat` call, for nothing.

Not a security defect — the guarded read is correct, and the comment at `:102` shows the author
*deliberately* declined to call `std::fs::metadata` on the raw path to avoid escaping the jail, which
is exactly the right instinct. It is dead code that should either surface the mode through a guarded
`flux-system` accessor or be deleted.

## Open questions

Things I could not settle at this lens. These are **not findings** — they are unexamined surface.

- **Plugin host authority contracts.** `crates/flux-plugin/src/host/loading.rs` overrides
  `authority_requirements`. Whether a manifest-declared resource set can under-declare what the
  subprocess actually reaches was not audited.
- **`flux-capabilities` endpoint and live-datasource ops.** `endpoint/ops.rs` and
  `datasource/live.rs` both override `authority_requirements`. Same question, same non-answer.
- **The flux-lang reference interpreter.** `docs/architecture.md:85` says effects are "injected via
  traits". I did not verify that no interpreter path constructs an effect handle directly.
- **Approved-scope skip for misclassified non-destructive ops.** Inside `in_approved_scope()` the
  prompt is skipped for anything not flagged destructive (`lib.rs:3511`). A tool misclassified the
  way `sqlite_query` is would also be invisible to the plan preview that produced the approval. I did
  not trace the preview path to confirm whether it derives from the same contract.
- **Server direct router mount.** The baseline established that `serve_on`'s non-loopback guard
  (`flux-server/src/lib.rs:457`) is bypassed by callers mounting the router directly. Not re-examined
  here; it remains open.

## Deployment recommendation

Unchanged from the baseline, plus one addition specific to this lens:

- **Disable `sqlite_query` via `[tools] disable` for any unattended or untrusted-input deployment**
  until finding 1 is closed. That config path is checked first and unconditionally in `gate`
  (`crates/flux-runtime/src/lib.rs:3272`), before scope, hooks, policy and permission rules, so it is
  a reliable kill switch for exactly this case.
- The OS sandbox (`FLUX_SANDBOX=require`) would contain finding 1's blast radius to the sandbox's
  writable set — which is the argument for the baseline's secure-defaults concern, now with a
  concrete instance behind it rather than a hypothetical.

## What this changes

- **Findings 1 and 2 are actionable and narrow** — one tool, one file. Neither needs a design trail.
- **Finding 3 is the one that matters over time.** flux's safety claim is architectural, but its
  enforcement of that claim is per-tool and manual, in a registry the project intends to grow. The
  baseline named "misclassified tool" as a *standing trust assumption*; this pass converts it from
  assumption to demonstrated instance. A lint is the cheapest durable answer.
- **Finding 4 is housekeeping.**
- The spread the baseline identified — architecture 8, assurance 5 — reproduces at this lens and
  sharpens: the architecture is good enough that its *only* observed failure came from outside it.
