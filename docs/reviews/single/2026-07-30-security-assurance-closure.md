---
title: Security-assurance closure — the 2026-07-29 baseline re-verified against the shipped tree
date: 2026-07-30
kind: internal-review
lens: security-and-production-readiness
method: >
  closure verification, not a fresh review. Each finding of the two 2026-07-29 baseline reviews was
  re-read against the tree at 0.38.0 and mapped to the commit, test name and file:line that closes
  it — reading the code and the CI wiring, never the story text. Every control was additionally
  checked for *production reachability* (is it constructed on a path a running flux takes, or only
  in a test?). Test evidence is cited by name and was executed: `cargo test --workspace
  --no-fail-fast`, `cargo test -p flux-cli --test website_contract`, `cargo test -p
  codewandler-flux-lang --test website_in_sync`. NOT done: no fuzzing, no exploitation, no attempt
  to weaken a guard to prove reachability, no new adversarial pass at a fresh lens, no CI run
  observed on GitHub (workflow YAML read statically), no audit of the surfaces the baseline's
  envelope-integrity pass left in Open questions.
reviewer: agent
subject:
  repo: codewandler/flux
  version_in_tree: 0.38.0
  published_release_at_review: v0.38.0
  workspace_crates: 38
  commit: 588144a2
overall_rating: 7.5/10
verdict: Every 2026-07-29 finding this epic accepted is closed with evidence and reachable in production — the assurance/architecture spread has largely collapsed, but one LOW finding was never filed and the epic still carries an open child.
ratings:
  security_architecture: 8.5/10
  secure_defaults: 6.5/10
  implementation_quality: 8/10
  security_assurance: 7.5/10
  release_supply_chain: 8/10
  product_maturity: 5.5/10
  community_bus_factor: 2/10
  production_readiness: 6.5/10
verification:
  status: verified against tree at 0.38.0 (588144a2) on 2026-07-30
  outcome: >
    desk-review findings 1-4 and the classification-trust concern are CLOSED with evidence and
    production-reachable; envelope-integrity findings 1-3 are CLOSED; envelope-integrity finding 4
    was still OPEN at the time of this pass, verbatim, and had never been filed as a story — it has
    since been filed as C-275 and CLOSED (see the finding-4 row). No child marked `done` was found
    to have an absent or structurally unreachable control.
  material_errors: none in the baseline reviews; two stale claims found in this repo's own ledgers (see "What this pass found that the ledgers did not")
top_findings:
  - "CLOSED (C-275) — envelope-integrity finding 4 (`file_stat` reads the file twice and discards it). OPEN and unfiled when this pass ran; this artifact is what caused it to be filed"
  - "The board's hand-written `## Status` claims the gate is green in both workspaces; `cargo test --workspace` is red at 588144a2 — 8 tests across 3 `codewandler-flux-lang` targets, traced to 3e2a8b89"
  - "C-186 cannot close as `done`: C-266 is a `ready` child filed the same day as this closure story"
  - "The desk review's headline finding (sandbox off by default) is only PARTIALLY closed — unattended and serving surfaces fail closed (C-262), the interactive default is still `Off` with network open"
  - "C-205 is a deliberate, defensible non-closure: the advisory is `unsound`-class and unreachable, and the fix needs a breaking ratatui major bump"
triage:
  kind: single
  status: open
  owner_stories: [C-186, C-267]
  aggregated_into: null
  note: >-
    Not yet rolled into an aggregate ledger. Its findings are tracked directly by the owner
    stories above; C-186 is still in progress, which is why this pass stays in single/.
---

## Verdict

**The spread the baseline identified has largely collapsed, and it collapsed for the right reason:
each finding was answered with a mechanism rather than a promise.** Security architecture was rated
8/10 from the outside on 2026-07-29 and security assurance 5/10; the gap between those two numbers
*was* the C-186 epic. Every finding the epic accepted is now closed by something that fails —
a CI job, a construction-time refusal, a registry-wide invariant walk, a statement allowlist — not
by a comment asserting the property holds.

That is the good news, and it is the substance of this artifact: the next reviewer can verify the
closure from the table below instead of re-deriving it.

The honest remainder is small but real, and it is stated up front because an unsupported tick here
would be worse than an open finding:

- **One baseline finding was still open when this pass ran** — envelope-integrity finding 4, LOW,
  dead code in `file_stat`. It survived verbatim in the tree. It was never filed as a story, which
  is *how* it survived: it fell out of the epic's scope silently rather than by decision.
  **Since closed by C-275**, which this artifact caused to be filed — the process gap below is the
  part worth keeping.
- **This epic still has an open child** (C-266, `ready`) and one deliberately blocked child (C-205).
- **Two of this repo's own ledger claims are stale**, including one that says the gate is green when
  it is not.

Nothing in this pass found a child marked `done` whose control was absent or unreachable. That was
looked for deliberately — see "The reachability check" — because this repo has been bitten by that
exact pattern before (C-233 and C-234 are both instances of it, found by earlier passes).

## Ratings

Δ is against the 2026-07-29 desk review, which is the baseline this epic must diff against.

| Area | 2026-07-29 | 2026-07-30 | Δ | What moved |
| --- | ---: | ---: | :---: | --- |
| Security architecture | 8/10 | **8.5/10** | ▲ 0.5 | The non-loopback auth invariant became structural (C-190); the guarded-IO perimeter gained a mechanical gate (C-194/C-263) |
| Secure defaults | 5/10 | **6.5/10** | ▲ 1.5 | Unattended and serving surfaces now fail closed on sandbox posture (C-262); `on` discloses its resolved posture instead of degrading silently (C-217). The interactive default is unchanged |
| Implementation quality | 7.5/10 | **8/10** | ▲ 0.5 | The confirmed bypass is closed by an allowlist, not a patched denylist. One LOW dead-code finding is still open |
| Security assurance | 5/10 | **7.5/10** | ▲ 2.5 | The headline movement. Advisory + license + source scanning (C-188), CodeQL and Miri lanes (C-264), four coherence gate seams (C-191/C-208/C-233/C-234), a `syn`-based direct-I/O scanner (C-263) |
| Release/supply chain | 6.5/10 | **8/10** | ▲ 1.5 | Every third-party action SHA-pinned with a CI pin guard (C-187); core artifacts now carry provenance attestation (C-259); the docs no longer promote a blind `curl … \| sh` |
| Product maturity | 5/10 | **5.5/10** | ▲ 0.5 | Five releases on; still pre-1.0 and moving fast |
| Community/bus factor | 2/10 | **2/10** | = | Structural. No code change addresses it; context, not a defect |
| Production readiness | 5/10 | **6.5/10** | ▲ 1.5 | The confirmed guarded-IO bypass no longer ships; the daemon has body limits, timeouts and rate limits; unattended execution fails closed |

**Assurance moved 2.5 points and is now the *second*-highest axis rather than the lowest.** The
lowest is bus factor, which is exactly where the baseline said an unfixable-by-code number should
sit. That is the shape a closed assurance epic is supposed to leave behind.

## Reading "findings 1–4 and classification trust"

C-186's last acceptance bullet, and the design doc's own acceptance paragraph, both promise to mark
*"findings 1–4 and classification trust closed with evidence"*. **That phrase is ambiguous between
the two baseline reviews, and the ambiguity is load-bearing, so it is settled here explicitly rather
than resolved silently.**

The reading used in this artifact: **findings 1–4 are the four items in the design doc's own
risk × reachability ÷ cost ranking** (`docs/designs/security-assurance.md`, "Ordering, and why it is
not the review's ordering"), and **classification trust is that ranking's item 5**. This reading is
forced by the epic's own acceptance structure — its first three bullets enumerate exactly
C-187/188/189/190, then C-191 "converting the review's *classification trust* concern from an
assumption into a gate", then C-192/193/194 for the envelope review. Bullet 4 is the closure of
bullets 1 and 2. It is also the only reading under which the phrase is satisfiable at all: the desk
review's own `top_findings` list is numbered 1–5 with the deferred sandbox default at 1 and the
unfixable bus factor at 5.

**Under the competing reading — the envelope-integrity review's explicitly numbered findings 1–4 —
that bullet would NOT be tickable, because its finding 4 is open.** Both sets are therefore mapped
below, and finding 4 of the envelope review is reported as open regardless of which reading governs
the tick. A reader who disagrees with the reading above still gets the true state of every finding.

## Desk-review findings — the closure table

Baseline: [`reviews/single/2026-07-29-security-posture-desk-review.md`](2026-07-29-security-posture-desk-review.md).

### 1 — GitHub Actions pinned to movable tags · **CLOSED** (C-187)

| | |
| --- | --- |
| Baseline evidence | `actions/checkout@v4`+`@v6`, `actions/upload-artifact@v4`+`@v7`, `Swatinem/rust-cache@v2`, `dtolnay/rust-toolchain@stable` — no SHA pins |
| Closing commit | `efac6efd` *ci(actions): pin third-party GitHub Actions to commit SHAs (C-187)*, merged `23c2d39f` |
| Control | Every `uses:` in all 9 workflows carries a 40-hex commit SHA with the version in a trailing comment |
| Gate | `scripts/check-action-pins.sh`, wired at `.github/workflows/ci.yml:137-139`, run with `--self-test` first so a broken guard fails loudly rather than passing vacuously |
| Verified in tree | `grep -rnE 'uses: [^ ]+@' .github/workflows/ \| grep -vE '@[0-9a-f]{40}'` → **zero matches** |

The `--self-test`-before-real-run pattern is worth naming: it is what makes the pin guard evidence
rather than decoration, and every `scripts/check-*.sh` in `ci.yml` follows it.

### 2 — No dependency-advisory scanning · **CLOSED** (C-188), for the advisory slice only

| | |
| --- | --- |
| Baseline evidence | `.github/workflows/*.yml` — zero hits for `cargo-audit`, OSV, `cargo-deny`, CodeQL, fuzzing, Miri |
| Closing commit | `e3d67e39` *ci(security): add cargo-audit + cargo-deny advisory scanning (C-188)*, merged `36ee42d5` |
| Control | `.github/workflows/security-audit.yml` — `cargo deny check` over both workspaces (`:51`, `:58`) and `cargo audit --deny warnings` over both lockfiles (`:79`, `:88`). Two tools on purpose: `--deny warnings` also fails on `unsound`/`notice`, and the tools keep separate advisory-db clones |
| Trigger | `push: [main]`, `pull_request`, **and** `schedule: "0 6 * * 1"` (`:19-26`) — so an advisory disclosed against an unchanged tree is still caught |
| Config | `deny.toml`, whose ignore list requires a stated reason per entry (`:50-51`: *"An unexplained ignore is a silent regression: do not add a bare RUSTSEC id here"*) |

**This is the one row where the epic must not take more credit than it earned.** The baseline's
concern was a *list*: advisory scanning, license/source enforcement, CodeQL or another SAST, fuzzing,
Miri/sanitizers, and reproducible-build/SLSA provenance. C-188 closed the first two. The rest were
never in C-186's scope and were closed later, by a **different epic**:

- CodeQL (Rust, `security-extended`) and a Miri lane — `.github/workflows/adversarial-assurance.yml`,
  from **C-264** under the C-255 epic.
- Core-artifact provenance — `actions/attest@…v4` at `.github/workflows/release.yml:440`, from
  **C-259** under the C-255 epic.

So the baseline's assurance bullet is now substantially closed, but **C-186 closed roughly a third of
it and C-255 closed the rest.** Reproducible builds remain unaddressed.

### 3 — Server has no body limit, request timeout or rate limiting · **CLOSED** (C-189 + C-260/C-261)

| | |
| --- | --- |
| Baseline evidence | `crates/flux-server/src/lib.rs` — routers at `:584,:593,:603,:765,:775` carry no `DefaultBodyLimit`, `TimeoutLayer`, `ConcurrencyLimit` or rate-limit layer |
| Closing commit | `c9c5086e` *feat(server): body limits and request timeouts on every router (C-189)*, merged `1d338db7` |
| Control — body | `DefaultBodyLimit::max(limits.max_body_bytes)` applied outermost on **both** routers: `crates/flux-server/src/lib.rs:913` (single-agent) and `:1120` (multi-agent) |
| Control — timeout | `request_timeout_layer` (`:745`) using `TimeoutLayer::with_status_code`, so the timeout answers a real `408`; plus `cancellable_request_timeout` (`:754`) so a timed-out A2A turn is cancelled and finalized rather than orphaned |
| Tests | `body_over_limit_is_rejected_with_413` (`:2136`) · `slow_handler_times_out_with_408` (`:2167`) · `blocking_a2a_timeout_cancels_and_finalizes_before_408` (`:2208`) · `sse_stream_route_is_exempt_from_the_request_timeout` (`:2267`) |
| Production reachability | `router_with_ttl` calls `ServerLimits::from_env()` (`:822`), so **every** production mount of either router gets the limits; tests inject tiny ones through the private `…_with_limits` seam. Not a test-only construction |

The deliberate SSE exemption is pinned by its own test rather than left as a comment, which is the
right shape: an exemption nobody asserts is an exemption that silently widens.

**Rate limiting was not C-189's** — C-189 delivered body limits and timeouts. Rate limiting and
per-principal resource budgets arrived under **C-260/C-261** (C-255 epic):
`request_rate_limit_rejects_before_session_mint` (`:2368`),
`request_rate_covers_authenticated_protected_reads` (`:2445`),
`live_work_limit_is_shared_across_rest_and_webhook_before_mint` (`:2559`). The baseline's row is now
fully closed, across two epics.

### 4 — Non-loopback auth invariant lives only in `serve_on` · **CLOSED** (C-190), by construction

| | |
| --- | --- |
| Baseline evidence | `lib.rs:457` — the refusal lives in `serve_on`, so a caller mounting the router directly gets no guard |
| Closing commit | `8e322973` *feat(server)!: refuse unauthenticated non-loopback router at construction (C-190)*, merged `f1333842` |
| Control | `guard_open_bind` (`crates/flux-server/src/lib.rs:512`) is called from `router` (`:792`) and from `router_multi`, so `router` returns `Result` and the *unauthenticated + off-loopback* combination is unrepresentable as a built router |
| Test | `unauthenticated_non_loopback_router_is_refused_at_construction` (`:2687`) — and it drives the **real** `router()`, not a hand-built `guarded_app`, asserting all three cases: open+routable refused, authenticated+routable builds, open+loopback builds *and reaches a protected route unauthenticated* (which is why the first case matters) |
| Production reachability | The exact caller the baseline named as the bypass — the `a2a` channel mounting the router into its own `axum::serve` — is `crates/flux-channels/src/adapters/a2a.rs:151`, and it goes through `flux_server::router`. It **inherits** the refusal now |

This is the strongest row in the table. The baseline described a bypass path that already existed;
the closure makes the unsafe state unconstructible, and the test proves it on the production
constructor. `serve_on` (`:484`) now documents that it simply inherits the refusal rather than
re-deriving it.

### 5 — Classification trust · **CLOSED** (C-191), and materially wider than the epic asked

| | |
| --- | --- |
| Baseline concern | *"A misclassified tool could pass through the risk approver without receiving the expected prompt … it remains a major trust assumption in a large and fast-growing tool registry"* |
| Closing commit | `d25eeab6` *feat(spec,tools,plugin): gate ToolSpec metadata coherence on every build*, merged `da3a675d` |
| Invariants | `crates/flux-spec/src/coherence.rs` — **I1** risk floor (`:256`), **I2** destructive floor (`:268`), **I3** repeatability floor (`:280`), composed by `metadata_violations` (`:230`) |
| Exemption discipline | `EXEMPT` (`:102`) is per-invariant, not blanket, each entry carrying a reason; its documented goal state is empty (`:101`) |

The reason this row exceeds its brief is that it was extended four times, each time by a pass that
found the *previous* gate too narrow — which is the behaviour an assurance epic is supposed to
produce. The gate now runs at four registration seams and two drift guards:

| Seam | Test | Where | Story |
| --- | --- | --- | --- |
| Built-in pack | `every_registered_builtin_spec_is_metadata_coherent` | `crates/flux-tools/tests/toolspec_invariants.rs:101` | C-191 |
| Plugin manifests | `plugin_declarations_are_held_to_the_same_invariants` | `toolspec_invariants.rs:184`; loader at `crates/flux-plugin/src/host/loading.rs:343` | C-191 |
| **Production catalog** | `every_operation_in_the_production_catalog_is_metadata_coherent` | `crates/flux-cli/src/catalog_coherence.rs:365` | C-208 |
| Sub-agent base registry | `the_sub_agent_base_registry_is_a_coherent_subset_of_the_catalog` | `catalog_coherence.rs:446` | C-208 |
| Published risk column | `the_published_risk_column_matches_the_production_catalog` (`:291`), `a_non_builtin_published_risk_drift_is_caught` (`:319`) | `catalog_coherence.rs` | C-233 |
| Registration-seam drift | `every_registration_seam_in_the_cli_assembly_is_classified` | `catalog_coherence.rs:781` | C-234 |

Two of those seams — C-233 and C-234 — exist **because an earlier pass caught a gate that was
narrower than it claimed**: the risk-column guard walked only built-ins, and the registration-seam
scan read only `execution.rs`. Both landed in `05139f9d`. C-210 (`933624c0`) closed a third such
gap, making both `gather_safe` and the coherence classifier read `semantic_effects` instead of being
blind in the same place. The negative results are as much the evidence here as the positive ones: the
census raised 22 violations across 19 operations the first time it ran, including two operations
(`explore`, `grade`) that no story had itemised.

**Fidelity limit, stated so it is not read as closed:** these invariants check declaration
*coherence*, not *fidelity to `execute`*. A tool whose `execute` does more than its spec admits is
still only caught by the direct-I/O scanner (envelope finding 3) and by review. The two instruments
are complementary, and neither subsumes the other.

## Envelope-integrity findings — the closure table

Baseline: [`reviews/single/2026-07-29-envelope-integrity.md`](2026-07-29-envelope-integrity.md).

### 1 — HIGH · `sqlite_query` creates files at arbitrary paths via `VACUUM INTO` · **CLOSED** (C-192)

The epic's only *confirmed bypass* of the envelope, rather than a missing assurance step.

| | |
| --- | --- |
| Baseline evidence | `crates/flux-tools/src/extra.rs:309` — `VACUUM INTO` reaches `rusqlite` directly, outside `flux-system`, from an op declaring `Effect::Read` / `Risk::Low` |
| Closing commit | `5031bd30` *fix(tools): admit sqlite_query by statement allowlist, close VACUUM INTO escape (C-192, C-193)*, merged `6e806bff` |
| Control | `ALLOWED_STATEMENT_KEYWORDS = ["SELECT","WITH","PRAGMA","EXPLAIN"]` (`extra.rs:223`), enforced at `:384-392` before the SQL reaches `prepare`. `VACUUM` is refused *as a statement type* — it is not on the allowlist, rather than being added to a denylist |
| Tests | the `VACUUM INTO` escape to an absolute path outside the workspace (`:732`) · the workspace-*internal* `VACUUM INTO`, still a misdeclared write (`:765`) · the refusal must come from flux's allowlist and not merely bounce off `SQLITE_OPEN_READ_ONLY` (`:797`) |
| Defence in depth | `jail_sqlite_path` + `SQLITE_OPEN_READ_ONLY` + the allowlist (`:414-420`) — three layers, with the test at `:797` asserting the *outermost* one is what fires |

The design of the fix is what closes the finding durably. An allowlist over statement types cannot be
defeated by a keyword nobody thought of, which is precisely how `VACUUM` got through the denylist.

### 2 — MEDIUM · `is_write_sql` is a prefix denylist documented as an allowlist · **CLOSED** (C-193)

| | |
| --- | --- |
| Baseline evidence | `extra.rs:207-218` — `trim_start()` strips whitespace, not comments; `/*x*/ INSERT …` passed |
| Closing commit | same as finding 1 |
| Control | `leading_statement_keyword` (`:234`) resolves the leading token *as SQLite will parse it* — comment-aware and case-insensitive — and `:275` tests it against the allowlist. The tool description (`:332-336`) and the refusal message (`:389-392`) now both describe an allowlist, matching the implementation |
| Test | `sqlite_query_allowlist_reads_past_comments_and_case` (`:828`) — a comment-prefixed lower-case `SELECT` is admitted *and returns its row* (`:844`, so the test is not vacuous), while a comment-prefixed lower-case `VACUUM` is refused (`:859`) |

The doc/implementation inversion the baseline flagged is gone in both directions: the code became an
allowlist *and* the prose was corrected to describe it.

### 3 — MEDIUM · The no-direct-IO invariant has no mechanical enforcement · **CLOSED** (C-194), hardened by C-263

Called by the baseline *"the structural finding"* and *"the cheapest thing on this list"*.

| | |
| --- | --- |
| Baseline evidence | grepped for and not found: any test or lint asserting `docs/architecture.md`'s *"All IO goes through `flux-system`"* — zero hits outside the doc's own prose |
| Closing commits | `0c529310` *ci(tools): enforce the no-direct-IO invariant with a named CI lint (C-194)*; `5b253e6a` made the scan string/comment-aware after review caught the first cut bypassable in the unsafe direction; merged `705cd445` |
| Control today | `scripts/check-no-direct-io.sh` → `flux-codegate`'s `syn`-based scanner, `no_unreviewed_direct_io_in_model_facing_operation_crates`. It resolves imports, renamed imports, module/type aliases, local callable aliases and multiline calls across filesystem, process, socket, HTTP-client and database opens |
| Gate | `.github/workflows/ci.yml:155-157` — `--self-test` (four named alias/bypass fixture tests) then the real scan |
| Hardened by | **C-263** (C-255 epic) replaced the text-pattern scan with the `syn` scanner and deleted the weaker fallback outright: the wrapper *"intentionally carries no crate list or weaker text-pattern fallback"* |

Two things make this row credible rather than nominal. First, `direct_io_allowance_requires_a_real_reason_immediately_above_the_call` — an allowance needs a stated reason at the call site, so the escape hatch is auditable. Second, the first implementation was **review-caught as bypassable**, reworked, and re-verified against a novel bypass; a gate that survived an attack on itself is worth more than one that was merely written.

### 4 — LOW · `file_stat` reads the entire file twice and discards the second read · **CLOSED** (C-275)

> **Closure note, added after this pass.** Filed as **C-275** — the story this artifact existed to
> provoke — and closed on branch `impl/C-275`. The second `read_file_bytes`, the `mode_str` binding
> and the `let _ = mode_str;` line are gone from `crates/flux-tools/src/extra.rs`; `file_stat` now
> reads the target exactly once, for `line_count`.
>
> **The choice made on mode: report none, and say so nowhere.** An honest mode needs a guarded
> accessor on `System`, which does not exist; the only other route is `std::fs::metadata` on the
> caller's raw string, which escapes the jail — precisely what the original author declined, and
> what `scripts/check-no-direct-io.sh` refuses. So the op reports no mode, and the *spec
> description* stopped advertising one too: it promised the model "octal mode" that no emitted
> field ever carried. The emitted contract is unchanged at `{path, size_bytes, line_count,
> mtime_unix}`.
>
> **Pinned by two tests** in `crates/flux-tools/src/extra.rs`, both red before the fix:
> `file_stat_reads_the_target_exactly_once` (a source scan of the `FileStatTool` declaration —
> behaviour cannot witness a discarded read, so the contract is held at the source, and the scan
> panics rather than passing vacuously if it loses its anchor) and
> `file_stat_reports_no_mode_anywhere_in_its_contract` (spec description, emitted JSON keys, view).
>
> The original analysis is kept verbatim below: it is the evidence, and **the process lesson is the
> part that outlives the fix.**

**Below, as recorded on 2026-07-30 — the finding was not closed then. It survived verbatim in the
shipped tree, and no story had ever been filed for it.**

`crates/flux-tools/src/extra.rs:96-107`, in `FileStatTool::execute`:

```rust
let mode_str = ctx
    .system()
    .read_file_bytes(path)          // :98 — a second full read of the target
    .await
    .ok()
    .map(|_| { … "(mode unavailable)".to_string() })
    .unwrap_or_else(|| "(mode unavailable)".to_string());
let _ = mode_str; // :107 — suppress unused warning — we surface it as a note below
```

Every element the baseline described is still present:

- both branches of the `map`/`unwrap_or_else` produce the identical string, so the read's result is
  never consulted;
- the value is explicitly discarded at `:107`;
- the trailing comment promises *"a note below"* which does not appear in the emitted result
  (`:108-119` build `content` and `view` from `path`/`size`/`line_count`/`mtime` only);
- the net effect is one redundant full `read_file_bytes` of the target per `file_stat` call.

**Severity is unchanged: LOW, and it is not a security defect.** The guarded read is correct, and the
comment at `:102` shows the author deliberately declined `std::fs::metadata` on the raw path to avoid
escaping the jail — the right instinct, and worth preserving in whatever fix lands. It is a
performance and clarity defect: a file of any size is read twice for nothing, on an op whose entire
purpose is to *avoid* reading content.

**Why it is reported rather than fixed here:** this is a verification story, and the review skill's
own boundary is *review, don't repair*. It needs a story.

**Why it matters out of proportion to its severity:** it did not survive because someone decided it
was not worth fixing. It survived because it was never filed. The epic's three envelope children
(C-192/193/194) map to envelope findings 1–3, and finding 4 simply fell off the edge — no story, no
"won't do", no reason recorded. That is a *process* gap, not a code gap, and it is the same gap that
would let a HIGH finding fall off next time. C-195 is the counter-example that shows the process
working: it was declined **on the merits**, in writing, with a test pinning the decision.

## The reachability check

C-267's acceptance requires that a child marked `done` whose claimed control is *absent or
structurally unreachable* in the shipped tree be treated as a finding in its own right rather than a
tick. This repo has been bitten by that pattern repeatedly — C-233 and C-234 are both instances,
found by earlier passes — so it was looked for deliberately.

**Result: no such child was found.** What was checked, and how:

| Control | Reachability question | Answer |
| --- | --- | --- |
| C-190 `guard_open_bind` | Is it on the path the `a2a` channel takes, or only in `serve_on`? | Reachable — `crates/flux-channels/src/adapters/a2a.rs:151` calls `flux_server::router`, which calls `guard_open_bind` at `:792` |
| C-189 limits | Does production get real limits, or only tests via the injection seam? | Reachable — `router_with_ttl` (`:822`) calls `ServerLimits::from_env()` for every production mount; the `…_with_limits` seam is private and test-only |
| C-208 catalog census | It is a `#[cfg(test)]` module inside the `flux-cli` **binary** — is that a hole? | No — deliberate and documented (`catalog_coherence.rs:28-31`). `flux-cli` has no lib target, and being inside the binary is what lets the census call the *same private* `register_tool_packs` production code instead of a drifting parallel copy. It runs under `cargo test -p flux-cli` |
| C-194 direct-I/O scan | Is the script wired into CI, or merely present in `scripts/`? | Wired — `.github/workflows/ci.yml:155-157`, `--self-test` first |
| C-187 pin guard | Same question | Wired — `.github/workflows/ci.yml:137-139` |
| C-188 advisory scan | Does it run on PRs, or only on a schedule nobody watches? | Both — `push: [main]`, `pull_request`, and a weekly cron (`security-audit.yml:19-26`) |
| C-217 posture disclosure | Asserted against the real binary, or a unit test of a helper? | Real binary — `crates/flux-cli/tests/sandbox_posture.rs` spawns `flux` and reads stderr (`:121`, `:156`, `:184`, `:202`) |

**Limit on this result:** reachability was established by reading call chains and CI wiring
statically. It was not established by observing a CI run on GitHub, nor by running flux against a
live non-loopback bind. A control could still be reachable in source and broken at runtime for a
reason this method cannot see.

## Deliberately unclosed, with reasons

An epic closing over known-open items must say so out loud. These are decisions, not oversights.

### C-205 — `lru` RUSTSEC-2026-0002 · `blocked`, and correctly so

The epic's one deliberately unclosed child. All three legs of the justification were re-verified:

1. **The dependency is transitive, not direct.** `cargo tree -i lru --workspace` →
   `lru v0.12.5 └── ratatui v0.29.0`, reached through `ansi-to-tui 7.0.0` and
   `codewandler-flux-markdown`. flux declares no dependency on `lru`. Reaching `>= 0.16.3` therefore
   requires a **breaking `ratatui 0.30.x` upgrade**, which is a TUI-wide change, not a lockfile bump.
2. **The advisory is `unsound`-class, not a vulnerability.** `deny.toml:68-74` records it as
   *"lru IterMut unsound (not a vulnerability)"*.
3. **It is unreachable from flux.** The unsoundness requires `LruCache::iter_mut`. Every `iter_mut`
   call in `crates/` is on a `Vec`/slice/`HashMap`; there is no `LruCache` in flux's source at all,
   since flux never depends on `lru` directly.

The suppression is honest rather than silent: it carries a stated reason in **both** tools
(`deny.toml:74` and `security-audit.yml:83`), and `deny.toml:50-51` forbids adding a bare id without
one. The entry disappears on its own the moment the lockfile moves past `0.16.3`.

**Trade-off stated plainly:** flux ships a crate with a known unsound advisory, and accepts that in
exchange for not taking a breaking UI-framework major bump for an unreachable defect. That is the
right call on the merits, and it is a call, not an accident.

### C-195 — approval-sheet redaction · `done` as **WON'T DO**

Recorded here because the board shows `done` and a reader could otherwise assume redaction was
implemented. **It was not, deliberately.**

The decision — *the approval sheet does not redact; no `flux-secret` edge is added to `flux-tui`* —
is a rejection on the merits, argued at length in `docs/designs/security-assurance.md` ("The approval
sheet does not redact (C-195)") and recorded at the seam in `crates/flux-tui/src/toolview.rs:191-201`
so it is not re-filed from the code side. The core argument: redaction is a *boundary* control, and
the approval sheet is not a boundary — it is an in-process render, to the operator's own TTY, of
bytes the operator is at that moment being asked to authorize. Redacting it would erase the highest-
value catch the surface exists to make (*"this write puts a live credential into a file"*) while the
real value still landed on disk.

What makes this a legitimate closure rather than a deferral dressed up: the decision is **pinned by a
test that fails if it is ever quietly reversed** —
`diff_does_not_redact_credentials_by_decision` (`toolview.rs:468`), which uses the exact literal from
`flux-secret`'s own `redacts_credential_shapes` test, so adding a `Redactor` to this path turns the
test red. The design doc also names the two developments that should reopen it.

### The sandbox default · **PARTIALLY** closed — the honest state

The desk review's **headline** finding. The epic deferred it by design, and the deferral was
discharged into C-217 (step 1). Since then C-262 went further than C-186 planned. The accurate state
is neither "closed" nor "still open":

**Closed:**
- `on` no longer degrades *silently*. `C-217` (`f616b1ff`, merged `e9d9d2fb`) makes it disclose its
  resolved posture on stderr, asserted against the real binary
  (`sandbox_posture.rs:121`), kept out of `--json` and `--stream-json` stdout (`:202`, `:346`), and
  quiet when the posture is `off` or outer confinement is asserted (`:156`). `require` still fails
  closed rather than disclosing and continuing (`:184`).
- **Unattended and serving surfaces now fail closed.** C-262 (C-255 epic) raises auto-approved
  non-interactive and serving surfaces to `SandboxMode::Require` before any work
  (`crates/flux-cli/src/dispatch.rs:156-161`), so a host with no backend refuses to start instead of
  running unconfined. Sandbox network defaults **closed** for those surfaces (`:219+`). An explicit
  `--no-sandbox` / `FLUX_SANDBOX=off` bypass is loudly audited rather than silent (`:201-212`), and
  `unattended_and_serving_surfaces_fail_closed_before_work` (`sandbox_posture.rs:237`) pins it.

**Still open, exactly as the baseline stated it:**
- **The interactive default is unchanged.** `SandboxSettings::default()` is
  `mode: SandboxMode::Off, network: true` (`crates/flux-system/src/sandbox.rs:63-64`), and
  `from_env` resolves `Off` absent `FLUX_SANDBOX` (`:82-89`) — still pinned by
  `from_env_defaults_off_with_open_network_and_no_extra_writable` (`:1288`). `dispatch.rs:219`
  states it: *"Interactive/local operation retains the pre-C-262 unrestricted default."*
- **No Windows backend exists.** Only Bubblewrap and Seatbelt are implemented. This is the reason
  the flip cannot simply be made, and it is why step 2 still needs its own design doc.

So: the *dangerous* half of the headline finding — unattended, auto-approving execution running
unconfined — is closed. The default itself is not, and C-266 exists because neither side of the new
fail-closed switch is proven in CI.

## What this pass found that the ledgers did not

Both are ledger defects rather than code defects, and both are the kind that tell a later reader not
to look.

**1 — The board claims a green gate that is not green.** `docs/stories/README.md`'s hand-written
`## Status` block ends with *"**Gate:** green in **both** workspaces — `cargo test` · `clippy
--all-targets -D warnings` · `fmt` · the `flux-codegate` layering lint · every `scripts/check-*.sh`
policy gate."* At `588144a2`, on a clean tree, `cargo test --workspace --no-fail-fast` is **red**:

```
codewandler-flux-lang --lib             : 2 failed (372 passed)
    highlight::tests::interior_form_keywords_and_labels
    parse::tests::guardrail_and_sugar_nodes_round_trip_natively
codewandler-flux-lang --test cst_agreement : 3 failed (1 passed)
    compact_readable_spellings_lower_to_the_existing_ast
    named_call_labels_are_semantic_and_duplicates_are_rejected
    native_spelling_battery_agreement
codewandler-flux-lang --test roundtrip_property : 1 failed
    random_draft_asts_round_trip_exactly
```

All failures are confined to `codewandler-flux-lang` and trace to `3e2a8b89`
*refactor(lang): complete the CST parser cutover* — `parse($raw.price, as: "f64")` is rejected by the
new parser with *"expects `<value>, as: \"type\"`"*, and `highlight` classifies a token `Op` where
the test expects `Punct`. 163 other test targets pass. This is deterministic, not flaky, and it is
not this story's diff — it reproduces on an unmodified checkout of the merge base.

Not corrected here: C-267's fence permits editing only the stale C-186 sentence in that block.

**2 — The C-186 epic's `## Progress` still lists work that has since landed.** It records as *"still
open before this epic closes"* both C-191 (*"remains `backlog` on purpose"*) and the sandbox-default
deferral. C-191 is `done`; the deferral became C-217, which is `done`. Corrected as part of this
story's ledger work, since the epic file is in scope.

## Open questions

Unexamined surface, not findings. Kept because dropping them silently is how the `file_stat` finding
survived.

- **The envelope-integrity review's own open questions are all still open.** Plugin-host authority
  contracts (`flux-plugin/src/host/loading.rs` overriding `authority_requirements`),
  `flux-capabilities` endpoint and live-datasource ops, the flux-lang reference interpreter's effect
  injection, and the approved-scope prompt skip for misclassified non-destructive ops. This pass
  verified closures; it did not audit new surface. Note that the plugin seam is now *partly*
  instrumented — `op_coherence_warnings` (`loading.rs:343`) applies I1/I2/I3 to plugin manifests —
  but that checks declaration coherence, not fidelity, which is the question the baseline asked.
- **No CI run was observed.** Every workflow claim above rests on reading the YAML. A job that is
  present, correct and *failing on GitHub right now* would look identical to this method.
- **`cargo clippy -D warnings`, `cargo fmt --check` and the seven `scripts/check-*.sh` gates were
  not run** in this pass. Only the three test commands named in `method:` were executed.
- **The plugins workspace was not exercised** beyond confirming that `security-audit.yml` covers its
  lockfile.
- **Reproducible builds** remain unaddressed from the baseline's assurance list. Provenance
  attestation (C-259) is a different property: it says who built an artifact, not that the build is
  reproducible.

## Deployment recommendation

Materially relaxed from the baseline, and for once the relaxation is specific rather than a vibe.

**Withdrawn from the baseline:** *"Disable `sqlite_query` via `[tools] disable` for any unattended or
untrusted-input deployment"* — the escape it guarded against is closed at the statement-admission
layer (envelope finding 1) and pinned by three tests. The kill switch still exists and is still
first in `gate`; it is no longer needed for this reason.

**Still recommended, and now partly enforced rather than merely advised:**

```text
sandbox mode:     require   — ENFORCED for unattended/serving surfaces (C-262); still opt-in interactively
sandbox network:  disabled  — now the DEFAULT for unattended surfaces
container/VM:     dedicated and unprivileged — MANDATORY on Windows, which still has no backend
workspace:        disposable clone
credentials:      short-lived and narrowly scoped
server binding:   loopback only — unauthenticated + non-loopback is now UNCONSTRUCTIBLE (C-190)
approval:         interactive for every write/process/network operation
plugins/hooks:    disabled initially
```

The judgement the baseline offered — *"promising security-engineered beta, not yet a trusted security
boundary"* — is still the right one, but the reason has changed. On 2026-07-29 the reason was that
almost nothing outside the envelope proved it held. That is no longer true. The remaining reasons are
bus factor, no external audit, pre-1.0 velocity, and the absence of a Windows confinement backend —
and of those, only the last is a code problem.

## What this changes

- **The epic's deliverable is now delivered.** C-186's promise was *"leave a trail that lets the next
  review verify the closure instead of re-deriving it."* The tables above are that trail: commit,
  test name, `file:line`, per finding, plus a reachability answer per control.
- **C-186 does not close as `done`.** It has a `ready` child (C-266), a `blocked` child (C-205), and
  one baseline finding with no story (envelope finding 4). `in-progress` is what the evidence
  supports.
- **One new story is owed** — envelope-integrity finding 4. Filing it is the user's call, per the
  review skill's *review, don't repair* boundary.
- **Assurance is no longer this project's weakest axis.** That is the single most useful sentence in
  this artifact, and it is why the epic was worth running: the 8-vs-5 spread that defined it is now
  8.5-vs-7.5.
- **The process lesson is the durable one.** Every closure that held up under this pass was closed by
  something that *fails* — a job, a refusal, an invariant walk. The one finding that did not close
  was the one nobody wrote down. Filing is the cheapest control in the epic and the only one it
  skipped.

---

*Baseline diffed against:*
[`2026-07-29-security-posture-desk-review.md`](2026-07-29-security-posture-desk-review.md) (external,
`0.33.1`, 6/10) and [`2026-07-29-envelope-integrity.md`](2026-07-29-envelope-integrity.md) (internal,
`0.33.1`/`f8e90d7`, 6/10).

*Not a substitute for, and not substituted by:* the three independent reviews of **2026-07-30** in
`docs/reviews/` (recorded in `bcfab0ad`, against `cb3bb057`), which became the **C-255** epic and
shipped as remediation in `0.38.0`. Those target a newer tree than the 2026-07-29 baseline, belong to
a different epic, and C-255 carries its own separate outstanding closure bullet. They are cited above
only where C-255 work closed a 2026-07-29 finding that C-186 did not — and each such row says so.
