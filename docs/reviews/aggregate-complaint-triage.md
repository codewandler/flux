---
title: Aggregate review complaint triage and claim-validation ledger
kind: review-aggregate
scope: all documents present under docs/reviews at aggregation time
status: validated-2026-08-01
source_documents: 5
source_numbered_findings: 31
---

# Purpose

This document combines the complaints in the five review documents listed below into one
normalized ledger. It is a triage artifact, not a finding of fact: every claim starts as
`unvalidated`, even when several reviewers reported it or cited tests. The source reviews examined
different snapshots and used different methods, so agreement raises validation priority but does not
prove that a complaint still applies to the current tree.

The ledger is designed to support a later validation pass that can, for each normalized claim:

1. identify the exact reviewed snapshot and current implementation;
2. separate source inspection from executable and operational evidence;
3. reproduce or falsify the narrow claim;
4. record whether a fix exists and whether a regression test observes it; and
5. preserve disagreements, prerequisites, and scope instead of flattening them into one verdict.

# Validation pass — 2026-08-01

Seven independent validators re-checked every claim in this ledger against the tree at `c2c5d17d`.
Method per validator: source trace with `file:line`, the named regression test and whether it would
still pass with the fix removed, plus mutation checks where a gate was the subject. Two validators
executed code (one server test, one Flux-Lang flow); one queried GitHub read-only. No validator
edited the tree.

The ledger's own anti-overclaim rules were applied: no claim is `historical-fixed` merely because
code changed, compound claims were split, and runtime claims about a live staged turn are recorded
as such rather than settled from static inspection.

| ID | Status | Residual work |
| --- | --- | --- |
| `NET-01` | `historical-fixed` | C-346, C-347, C-348 — the fix was applied per **reviewed** adapter; three unreviewed egress paths still resolve twice |
| `NET-02` | `historical-fixed` | C-348 — pinning is proven at the sole constructor, not through the registered op |
| `PROC-01` | `historical-fixed` | C-351 — `flux_bin` is rejected and credentials are allow-listed, but the schema is open and `trials` has no ceiling |
| `PROC-02` | `historical-fixed`; underlying invariant still false | C-349 — `core.fsmonitor` executes under `git_status` **and** `git_diff` today; reproduced empirically |
| `SANDBOX-01` | `partially-reproduced` / `design-decision` | C-350 — the CLI fails closed; `flux-sdk`/`flux-server` and un-flagged `app run` daemons do not, and the docs claim they do |
| `REL-01` | `partially-reproduced` — bootstrap `historical-fixed`, authority `reproduced` | C-353, C-354, C-355 |
| `REL-02` | `historical-fixed` from `v0.38.0` | C-356 — attestations verified live for `v0.44.0`; verification is not on the primary install path |
| `OUTCOME-01` | `historical-fixed` | C-373, C-374 — `suspended`/`max_iter`/`cancelled`/denied still report as `ok` |
| `SRV-01` | `historical-fixed` | C-372 — no wall-clock ceiling; C-370 — the same defect is unfixed in the channel adapters |
| `SRV-02` | `partially-reproduced` | C-370, C-371, C-372 — false for `flux-server`, true for the channel-adapter ingress; queue depth unbounded everywhere |
| `ASSURE-01` | `partially-reproduced` (lanes split) | C-359, C-360, C-361, C-362 — SAST/Miri/attestation fixed; fuzzing and sanitizers absent; three lanes never executed |
| `ASSURE-02` | `partially-reproduced` | C-364 (live un-waived hit), C-365, C-366 |
| `ASSURE-03` | `partially-reproduced` | C-367 (a second production catalog no census covers), C-368 |
| `ASSURE-04` | `reproduced`; the external half is now **verified absent**, not unknown | C-353, C-357 — no branch protection, no rulesets, no release environment, zero merged PRs, one admin |
| `HAR-01` | `partially-reproduced` (compound) | C-376, C-377 — a runner **is** advertised; `flow_run` has no path parameter and routes to `core` |
| `HAR-02` | `reproduced` (mechanism-absent reading) | C-379 |
| `HAR-03` | `reproduced`, one half re-scoped | C-380 — no model-facing validate op exists, so the mislabelling had no harness target |
| `HAR-05` | `reproduced` | C-378 |
| `ROUTE-01` | `partially-reproduced` / split | C-381 — richer intent fields landed untracked in `edcd9dcc`; hints and measurement are open |
| `GIT-01` | `reproduced` | C-383 |
| `GIT-02` | `partially-reproduced` | C-384, C-385 — path-level attribution exists; it cannot disambiguate a mixed-hunk file |
| `GIT-03` | `reproduced` by construction | C-386 — argument-independent classifier; needs a deliberate envelope decision |
| `HAR-04` | `reproduced` in the product, `historical-fixed` in the library | C-387, C-388 |
| `HAR-06` | `design-decision` on read-back, `reproduced` on wording | C-389 — C-306 owns the contract; the results still claim visibility |
| `LANG-01` | `not-reproduced` as a language gap | C-390 — the collapsed form runs today; the verbosity is one demo flow and a docs row |

Epics: C-345 (egress and confinement), C-352 (release trust), C-358 (assurance lanes), C-363
(structural gate blind spots), C-369 (serving surfaces and turn outcome), C-375 (harness route
integrity), C-382 (change recovery and provenance).

## What this pass did not establish

- No exploitation was performed for any P0 claim; source plausibility is not reported as reproduced
  exploitability.
- `HAR-01`, `HAR-02` and `GIT-03` are claims about a live staged turn. Validation established that
  the mechanisms they say are missing are in fact missing, and that the classifiers involved are
  argument-independent — **not** that the specific reported sessions behaved as described. No
  authoritative transcript reader exists to check (that is `HAR-04`).
- Succession, incident exercises, audit history and organisation-level policy remain
  `external-unknown`.


# Source index and comparability

| Key | Source | Subject / method boundary |
| --- | --- | --- |
| A | `docs/reviews/2026-07-30-independent-adversarial-review-a.md` | Security desk review plus four targeted checks; commit `cb3bb057c961db70769330b375299e09a2fabfcb` |
| B | `docs/reviews/2026-07-30-independent-adversarial-review-b.md` | Security desk review plus root/plugin tests and structural scripts; same commit |
| P | `docs/reviews/2026-07-30-independent-adversarial-review-primary.md` | Read-only security/configuration review; same commit; no Cargo build/test |
| C | `docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md` | Session retrospective over staged planning, flow execution, and Git recovery; no flow or Cargo execution |
| H | `docs/reviews/2026-07-31-harness-tooling-friction-review.md` | Interactive pane exercise plus source comparison; worktree included uncommitted staged-intent changes |

The three security reviews are directly comparable at the stated commit, but they do not always
agree. The two harness reviews concern session-specific capability sets and partially mutable
worktree state; their availability claims must be reproduced in an equivalent staged-planning turn,
not inferred only from the current static catalog.

# Validation status vocabulary

| Status | Meaning |
| --- | --- |
| `unvalidated` | Aggregated from a review; no new check performed for this ledger |
| `reproduced` | The narrow complaint was demonstrated on its stated snapshot or current tree |
| `partially-reproduced` | Only part of a compound claim was demonstrated |
| `not-reproduced` | A suitable attempt did not demonstrate it; limitations are recorded |
| `historical-fixed` | It applied to the reviewed snapshot, and a later fix plus observing regression test were verified |
| `false` | The cited implementation or behavior does not support the claim on its stated snapshot |
| `design-decision` | The behavior is intentional and accurately documented; any desired change is product policy, not a defect |
| `external-unknown` | Validation requires deployment, account, publication, or organizational evidence absent from the repository |

A claim is not `historical-fixed` merely because code changed. Record the fixing commit and a test
that fails when the fix is removed. For operational/default complaints, also test the assembled
product path rather than only the helper in isolation.

# Priority model

- `P0`: alleged boundary bypass, credential exposure, or release-pipeline compromise path; validate first.
- `P1`: unattended correctness, resource, recovery, or route-integrity failure with material impact.
- `P2`: assurance, provenance, product-default, or missing-control gap that weakens confidence.
- `P3`: bounded ergonomics or documentation issue with no demonstrated integrity failure.

Priority expresses validation order, not confirmed severity.

# Executive triage

| Workstream | IDs | Main question |
| --- | --- | --- |
| Network boundary | `NET-01`, `NET-02` | Are vetted DNS answers and redirect decisions preserved through connection time on every outer adapter? |
| Process / credential boundary | `PROC-01`, `PROC-02`, `SANDBOX-01` | Can observer or model-selected process paths execute more authority than their metadata/defaults claim? |
| Release trust | `REL-01`, `REL-02` | Are producer tools authenticated before execution, and can consumers authenticate outputs independently? |
| Server lifecycle and abuse | `SRV-01`, `SRV-02` | Does work stop/buffer safely, and are arrival/concurrency/spend bounded? |
| Turn outcome integrity | `OUTCOME-01` | Can provider-stage failure be mistaken for successful completion? |
| Assurance coverage | `ASSURE-01`–`ASSURE-04` | Do tests and gates observe the security claims they are cited to support? |
| Harness route integrity | `HAR-01`–`HAR-03` | Can the requested flow route run, and is completion bound to evidence that it did? |
| Git/change recovery | `GIT-01`–`GIT-03` | Can the harness identify its changes, inspect state immediately, and safely uncommit? |
| Harness epistemics | `HAR-04`–`HAR-07` | Can static validation, preflight, pane state, and transcript scope be reported without overclaiming? |
| Routing/ergonomics | `ROUTE-01`, `LANG-01` | Can narrow capability routing work first-pass, and can authored timing be less repetitive? |

# Normalized claim ledger

## P0 — alleged boundary and supply-chain paths

### NET-01 — Plugin HTTP, OAuth, and TCP may re-resolve after egress validation

- Status: `unvalidated`
- Sources: A finding 1 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:156-195`).
- Normalized claim: plugin HTTP, OAuth token refresh, and raw TCP paths allegedly validate one DNS
  result but connect through a later hostname resolution, permitting DNS-rebinding access outside the
  private-network grant.
- Preconditions/bounds: a plugin already has a destination capability; the hostname is attacker
  controlled; HTTP secret exposure depends on transport/TLS details. Raw TCP does not depend on HTTP.
- Disagreement: P's strengths section says plugin HTTP uses the right pattern
  (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:86-90`), while A later alleges
  redirect disabling is insufficient because addresses are not pinned. Validate the full connect path,
  not merely redirect policy.
- Validation evidence needed: trace each callback from capability check to socket connect; use an
  injected resolver whose second answer changes; cover initial requests, redirects, credential
  injection, OAuth refresh, and TCP; verify empty vetted sets fail closed.
- Closure evidence: fixing commit plus regression tests that fail if connection-time pinning is removed.

### NET-02 — Fleet/A2A clients may re-resolve or follow redirects outside the egress guard

- Status: `unvalidated`
- Sources: P finding 1 (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:106-130`).
- Normalized claim: `fleet.dispatch`, `fleet.status`, and `fleet.cancel` allegedly discard guarded
  addresses, construct a separate A2A client from the original URL, and do not re-guard redirects.
- Preconditions/bounds: a fleet operation must be available and called; the initially admitted host is
  attacker controlled; P reports that the stock CLI assembly supplied no worker bearer token.
- Validation evidence needed: inspect current registration and client construction; run DNS-answer
  swap and redirect fixtures for all three operations, including 307/308 POST behavior; assert private
  destinations are never contacted without a scoped grant.
- Closure evidence: integration tests through the production registration path, not only a URL helper.

### PROC-01 — `eval_run` may execute a model-selected sandbox-exempt binary with raw credentials

- Status: `unvalidated`
- Sources: P finding 2 (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:132-157`).
- Normalized claim: production registration allegedly allows model input to select `flux_bin`, then
  runs it through a trusted-host sandbox exemption while injecting provider credentials.
- Preconditions/bounds: the eval family must be surfaced; process execution must be approved or
  auto-approved; the selected executable must already exist and be reachable.
- Validation evidence needed: inspect the exact current schema, group signaling, catalog registration,
  executable constraints, environment construction, and process API; run a harmless sentinel binary
  through the public operation path and assert both confinement and secret non-disclosure.
- Related assurance issue: `ASSURE-02` covers the allegation that the direct-I/O scan excluded this
  model-facing crate on a false premise.

### REL-01 — Release jobs may execute unauthenticated remote tooling with excessive authority

- Status: `unvalidated`
- Sources: A finding 2 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:197-222`);
  P finding 3 (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:159-174`).
- Normalized claim: release planning/build/publish paths allegedly execute downloaded cargo-dist and
  rustup installers without locally verifying a digest or signature; P additionally alleges broad
  write-capable token exposure in jobs that execute them.
- Distinguish during validation: (a) remote bytes are executed, (b) bytes lack independent content
  authentication, (c) a usable write token is present in the same job, and (d) compromised build bytes
  can reach publication. Each subclaim can have a different result.
- Validation evidence needed: workflow job permissions, inherited/default token permissions,
  environments, installer acquisition, cache/artifact trust transitions, and publication credentials;
  validate generated matrix commands rather than treating them as static literals.
- External boundary: GitHub environment protection and token policy may remain `external-unknown`.

## P1 — unattended integrity, lifecycle, and recovery

### OUTCOME-01 — Provider-stage errors may be reported as successful turns

- Status: `unvalidated`
- Sources: P finding 4 (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:176-192`).
- Normalized claim: intent/exploration provider errors allegedly become ordinary data, causing NDJSON
  `turn_end` and process exit zero rather than a typed failed outcome.
- Validation evidence needed: inject failures independently at intent, exploration, planning, and
  execution stages through the real CLI stream path; assert terminal record and exit code; separately
  classify resumable stream failures.
- Closure evidence: end-to-end tests consumed as an automation client would consume them.

### SRV-01 — REST SSE work may outlive disconnects and buffer without bound

- Status: `unvalidated`
- Sources: B finding 2 (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:141-159`).
- Normalized claim: the REST session stream allegedly uses an unbounded channel and detached task with
  no disconnect cancellation, unlike the bounded A2A stream.
- Validation evidence needed: inspect the current route assembly; run disconnect and stalled-reader
  tests that observe cancellation, tool/provider activity, and bounded queue behavior; confirm server
  timeout exemptions do not create an unbounded lifecycle.

### HAR-01 — Arbitrary workspace flows could not be executed in the staged harness

- Status: `unvalidated`
- Sources: C finding 1 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:61-83`).
- Normalized claim: the reviewed staged-planning capability set could inspect/render but not execute
  `examples/commit.flux`, so the requested route could not be dogfooded.
- Scope warning: this is a runtime capability-availability claim, not proof that no flow runner exists
  anywhere in the product.
- Validation evidence needed: reproduce an accepted intent requiring a literal workspace `.flux` path;
  record surfaced schemas, preflight result, action capture, nested approvals, and terminal receipt.

### HAR-02 — Completion was not bound to the user-required execution route

- Status: `unvalidated`
- Sources: C finding 2 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:85-105`).
- Normalized claim: direct Git operations reached a similar repository state without running the
  required flow, yet the agent initially reported the route-specific dogfood task as complete.
- Validation evidence needed: authoritative transcript/receipts for the cited session if available;
  acceptance test with `substitution_allowed: false`; prove that equivalent lower-level calls cannot
  yield a successful route-verification outcome.
- Relationship: `HAR-01` is missing capability; this claim is the separate failure to stop or ask before
  substitution.

### GIT-01 — No constrained uncommit operation preserved a mistaken local commit's patch

- Status: `unvalidated`
- Sources: C finding 3 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:107-129`).
- Normalized claim: the surfaced Git family offered revert but no safe mixed-reset-equivalent for an
  unpushed `HEAD`, preventing recovery to the pre-commit working tree.
- Validation evidence needed: inspect the exact Git operations available after relevant signaling;
  exercise a temporary repository with unpushed/merge/upstream/staged-change cases; assess whether a
  purpose-built operation exists and fails closed on ambiguity.

### SANDBOX-01 — Process confinement is opt-in, network-open by default, and platform-dependent

- Status: `unvalidated`
- Sources: A finding 4 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:245-261`);
  B finding 1 (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:121-139`); P finding 5
  (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:194-204`).
- Normalized claim: unset configuration allegedly selects no OS sandbox and open sandbox networking;
  best-effort `on` can degrade to unconfined execution, only `require` fails closed, and Windows has no
  backend.
- Classification note: reviewers call this defense in depth/product default, not a demonstrated policy
  bypass. It may ultimately be `design-decision` while still motivating deployment preflight changes.
- Validation evidence needed: configuration truth table plus assembled CLI/server behavior on supported
  and unsupported hosts; hostile child/process-tree and network tests; verify operator disclosure.

### SRV-02 — The daemon lacks general request, concurrency, and spend controls

- Status: `unvalidated`
- Sources: A finding 5 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:263-279`);
  B finding 3 (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:161-173`); P finding 6
  (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:206-216`).
- Normalized claim: body/time/A2A bounds allegedly do not provide general per-principal request rate,
  concurrency, queue, or provider-spend limits across REST, webhook, blocking A2A, and streaming paths.
- Severity disagreement: A/B rate this Medium; P rates it Low. Preserve this because authentication and
  deployment topology materially affect impact.
- Validation evidence needed: inventory every router/layer and task registry; load-test each ingress
  class with valid credentials; distinguish native limits from documented reverse-proxy requirements.

## P2 — release authenticity, assurance, and provenance

### REL-02 — Core release artifacts lack an independent consumer authenticity root

- Status: `unvalidated`
- Sources: B finding 4 (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:175-193`);
  A finding 2 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:217-222`); P finding 3
  (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:171-174`).
- Normalized claim: core archives/installers/checksums allegedly lack detached signatures, attestations,
  or consumer-verifiable provenance, while primary install documentation executes same-origin scripts.
- Keep separate from `REL-01`: producer bootstrap integrity and consumer artifact authenticity require
  different evidence and can be fixed independently.
- Validation evidence needed: current release workflow, verifier, published manifest format, docs, and
  actual release assets; external publication state may be `external-unknown` if network/account evidence
  is unavailable.

### PROC-02 — `git_diff` may invoke external diff/textconv programs despite low-risk metadata

- Status: `unvalidated`
- Sources: A finding 3 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:224-243`).
- Normalized claim: `git_diff` allegedly omitted `--no-ext-diff` (and may need `--no-textconv`), allowing
  host/repository Git configuration to execute a program despite observer-style classification.
- Preconditions/bounds: a malicious or preconfigured driver must exist; this is not model-authored argv.
- Validation evidence needed: inspect current argv and metadata; use a temporary repository with
  external diff and textconv sentinels; execute through the dispatcher under the real approval posture.

### ASSURE-01 — Security automation lacks independent/adversarial execution lanes

- Status: `unvalidated`
- Sources: A finding 6 (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:281-301`);
  B finding 5 (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:195-207`).
- Normalized claim: repository-controlled automation allegedly lacked fuzzing, SAST, Miri, sanitizers,
  and core release attestations; security validation was mostly implementer-authored.
- Validation evidence needed: current workflows, scripts, fuzz targets/corpora, scheduled jobs, and
  externally documented audit evidence. Record each lane separately; do not use one addition to close
  the whole compound claim.
- Related subclaims from A: security response timing was unspecified, and independent release/security
  review capacity was weak. Validate those under policy and organizational evidence, not CI alone.

### ASSURE-02 — The direct-I/O gate is lexical, scoped, and may encode false exclusions

- Status: `unvalidated`
- Sources: B finding 6 (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:209-223`);
  P finding 2 assurance component (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:153-157`).
- Normalized claim: the scanner allegedly matches selected spellings in selected crates rather than
  enforcing the invariant semantically; P reports that `flux-eval` was excluded as non-model-facing even
  though `eval_run` was production-registered.
- Validation evidence needed: current scanner scope/pattern/self-tests, catalog registration, aliases and
  alternate APIs; mutation tests that introduce representative violations; inspect whether architecture
  now prevents bypass independently of lexical scanning.

### ASSURE-03 — Tool catalog/risk tests may have silent coverage holes

- Status: `unvalidated`
- Sources: P finding 7 (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:218-228`).
- Normalized claim: risk-table tests allegedly skipped unresolved rows and catalog census logic could
  miss registrations outside one scanned module or under reused labels.
- Validation evidence needed: enumerate the production catalog from runtime assembly; compare it with all
  metadata/reference tests; mutation-test a registration in another module and an unresolved risk row;
  require the gate to fail loudly.

### ASSURE-04 — Maintainer concentration and external controls remain unverified risks

- Status: `unvalidated`
- Sources: A ratings/finding 6/open questions
  (`docs/reviews/2026-07-30-independent-adversarial-review-a.md:105-106`, `:289-316`); B ratings/open
  questions (`docs/reviews/2026-07-30-independent-adversarial-review-b.md:62-63`, `:242-247`); P ratings
  (`docs/reviews/2026-07-30-independent-adversarial-review-primary.md:72-73`).
- Normalized claim: local Git history appeared attributable to one maintainer, while branch protection,
  required review, release environment policy, incident exercises, and operational controls were not
  repository-verifiable.
- Validation evidence needed: normalize author identities and contribution roles; separately obtain
  organization settings, succession/release authority, audit history, and deployment evidence.
- Likely status split: Git-history concentration can be repository-validated; private organizational
  controls may remain `external-unknown`.

### GIT-02 — Change ownership/provenance is not machine-verifiable

- Status: `unvalidated`
- Sources: C finding 4 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:131-150`).
- Normalized claim: status/diff data did not identify which session or action produced a path/hunk, so
  “commit all your changes” was ambiguous in a mixed user/agent worktree.
- Validation evidence needed: inspect action receipts and staged-planning state for path/blob/hunk
  provenance; create mixed-hunk cases across turns/sessions; verify staging can target only receipt-owned
  changes without trusting conversational memory.

### GIT-03 — Read-only Git observers were captured as deferred actions

- Status: `unvalidated`
- Sources: C finding 5 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:152-169`).
- Normalized claim: `git_status`, `git_log`, and `git_diff` returned proposed-action capture rather than
  immediate evidence in the retrospective turn.
- Validation evidence needed: reproduce at gather and action-planning stages under the same policy;
  record operation effects and capture classifier; verify observation-only arguments cannot mutate and
  do not require deferred approval.

### HAR-03 — Static flow validation was easy to overstate as end-to-end execution

- Status: `unvalidated`
- Sources: C finding 6 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:171-186`).
- Normalized claim: parse/lower validation of examples did not prove Git outputs, approvals, index
  refusal, commit creation, or rollback, but was presented too strongly in the session.
- Validation evidence needed: inspect test scope and user-visible labels; add/run hermetic temporary-repo
  tests through the same public flow route, covering all scenarios listed by C.

### HAR-04 — No authoritative scoped transcript reader supports retrospectives

- Status: `unvalidated`
- Sources: C finding 8 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:202-210`);
  H finding 3 (`docs/reviews/2026-07-31-harness-tooling-friction-review.md:122-137`).
- Normalized claim: current model context cannot prove transcript completeness after compaction/resume,
  while arbitrary session-database filesystem access would violate scope.
- Validation evidence needed: inspect available redacted session-history operations and compaction
  metadata; test pagination, role/event visibility, and authorization. If retrospectives are not a
  supported use case, classify the product decision explicitly.

### ROUTE-01 — First-pass capability routing depends on a sufficiently rich intent contract

- Status: `unvalidated`
- Sources: H finding 1 (`docs/reviews/2026-07-31-harness-tooling-friction-review.md:74-102`).
- Normalized claim: narrow staged surfacing is intentional and desirable, but indirect requests may need
  richer intent fields and family hints to select the right family on the first attempt.
- Important correction: this is not a request for an ambient all-tools catalog. H explicitly rejects
  that interpretation (`docs/reviews/2026-07-31-harness-tooling-friction-review.md:151-169`).
- Validation evidence needed: fixed evaluation set with first-pass family precision/recall, unnecessary
  families, repair rate, surfaced schema bytes, and latency; compare intent/hint variants.

### HAR-05 — Capability availability lacks an exact-flow preflight

- Status: `unvalidated`
- Sources: C finding 7 (`docs/reviews/2026-07-31-commit-workflow-dogfooding-friction-review.md:188-200`).
- Normalized claim: the agent could not determine before mutation whether a specific workspace flow was
  executable with the current operations and approval posture.
- Validation evidence needed: inventory current parse/lower/preflight operations; test a flow with present,
  missing, disabled, and unsignaled families; ensure output distinguishes inspectable from executable.

## P3 — bounded surface and language ergonomics

### HAR-06 — Pane command acceptance cannot establish authoritative visible state

- Status: `unvalidated`
- Sources: H finding 2 (`docs/reviews/2026-07-31-harness-tooling-friction-review.md:104-120`).
- Normalized claim: pane operations are send-only, so the agent cannot verify state after host expiry,
  suppression, resume, capacity decisions, or user interaction.
- Existing framing: H says this is an explicit contract decision tracked by C-306, not an overlooked
  implementation defect.
- Validation evidence needed: inspect current pane result wording and any host-owned query contract;
  test expiry/suppression. Resolve as `design-decision` if write-only behavior remains intentional and
  claims consistently say “accepted” rather than “visible.”

### LANG-01 — Timed pane flows are repetitive to author

- Status: `unvalidated`
- Sources: H finding 4 (`docs/reviews/2026-07-31-harness-tooling-friction-review.md:139-149`).
- Normalized claim: existing Flux-Lang loops can implement cancellable timed animation safely, but one
  loop/update block per frame is verbose.
- Validation evidence needed: compare documented/composite patterns, source size, dispatch count, jitter,
  cancellation, and approval behavior before proposing language or privileged-operation changes.
- Constraint: H explicitly says the exercise did not justify a new `pane.sequence` operation.

# Crosswalk: every numbered source finding

This table prevents deduplication from silently dropping a complaint.

| Source finding | Ledger ID(s) |
| --- | --- |
| A-1 plugin DNS rebinding | `NET-01` |
| A-2 release bootstrap integrity | `REL-01`, `REL-02` |
| A-3 `git_diff` external execution | `PROC-02` |
| A-4 sandbox defaults | `SANDBOX-01` |
| A-5 daemon rate limiting | `SRV-02` |
| A-6 adversarial assurance / bus factor | `ASSURE-01`, `ASSURE-04` |
| B-1 sandbox defaults | `SANDBOX-01` |
| B-2 REST SSE lifecycle | `SRV-01` |
| B-3 daemon rate/concurrency | `SRV-02` |
| B-4 core release authenticity | `REL-02` |
| B-5 adversarial automation | `ASSURE-01` |
| B-6 lexical direct-I/O gate | `ASSURE-02` |
| P-1 fleet DNS/redirect handling | `NET-02` |
| P-2 `eval_run` exemption and lint scope | `PROC-01`, `ASSURE-02` |
| P-3 privileged release installers | `REL-01`, `REL-02` |
| P-4 provider error outcome | `OUTCOME-01` |
| P-5 sandbox defaults | `SANDBOX-01` |
| P-6 daemon rate control | `SRV-02` |
| P-7 catalog coverage | `ASSURE-03` |
| C-1 no surfaced flow execution | `HAR-01` |
| C-2 route substitution / false completion | `HAR-02` |
| C-3 no history-preserving undo | `GIT-01` |
| C-4 no durable change ownership | `GIT-02` |
| C-5 Git observers captured | `GIT-03` |
| C-6 static validation overread | `HAR-03` |
| C-7 no exact-flow preflight | `HAR-05` |
| C-8 no scoped transcript reader | `HAR-04` |
| H-1 intent/routing quality | `ROUTE-01` |
| H-2 pane read-back | `HAR-06` |
| H-3 no scoped transcript reader | `HAR-04` |
| H-4 timed-flow verbosity | `LANG-01` |

# Conflicts and anti-overclaim rules

1. Plugin egress is disputed: P describes the plugin path as a strength, while A alleges a connection-
   time DNS-pinning defect. Redirect-disabled is not equivalent to address-pinned; validate both.
2. No security review performed exploitation. Source plausibility must not be reported as reproduced
   exploitability without a controlled fixture.
3. Sandbox defaults are consistently reported but explicitly described as defense in depth. Do not call
   them an authorization bypass unless a separate test demonstrates one.
4. Rate limiting severity varies. Validate authentication, tenancy, reverse proxy, concurrency, and spend
   assumptions before assigning impact.
5. Checksums and signatures solve different problems. Do not use checksum presence to close consumer
   authenticity, or artifact signing to close unauthenticated producer bootstrap execution.
6. Static parse/lower checks do not prove real operation output, approval, cancellation, or rollback.
7. Operation-level success does not prove a user-required mechanism ran. Route-specific tasks require a
   receipt for that route or an explicit substitution decision.
8. Pane command acceptance does not prove current visibility while the contract is write-only.
9. Current model context does not prove complete persisted history; retrospective completeness must remain
   scoped unless an authoritative reader exists.
10. Progressive capability narrowing is not itself a defect. Validate routing quality without proposing an
    all-tools bypass that H explicitly rejected.

# Recommended validation sequence

1. Re-anchor the three security reviews to their exact commit, then diff the cited paths to current `main`.
2. Validate `NET-01`, `NET-02`, and `PROC-01` with controlled sentinel fixtures before reviewing broad
   hardening proposals.
3. Validate `REL-01` as a job-by-job trust/permission graph; validate `REL-02` against actual publication
   artifacts separately.
4. Exercise `OUTCOME-01` and `SRV-01` end to end because both can produce misleading success or work that
   outlives its consumer.
5. Reproduce the commit-flow session in a disposable repository, capturing exact capability schemas and
   receipts for `HAR-01`, `HAR-02`, `GIT-01`, and `GIT-03`.
6. Run product/default and load checks for `SANDBOX-01` and `SRV-02` under explicitly documented deployment
   assumptions.
7. Mutation-test assurance claims `PROC-02` and `ASSURE-02`–`ASSURE-03`; a gate is validated only if the
   representative bad change makes it fail.
8. Evaluate routing and epistemic claims only after the behavior-critical claims, using fixed datasets and
   wording contracts rather than anecdotal success.
9. Split any compound ledger entry whose subclaims receive different results; never force one status over
   mixed evidence.

# Per-claim validation record template

Copy this block under a claim when validation begins:

```text
Validation date:
Validator:
Target commit/version:
Environment/platform:
Status:

Claim narrowed to:
Prerequisites established:
Source path trace:
Test/fixture:
Observed result:
Counterevidence:
Limitations:

Fix commit (if historical-fixed):
Observing regression test:
Test fails when fix is removed: yes/no/not checked
External evidence reference:
Follow-up owner/story:
```

# Scope exclusions

- Strengths, ratings, deployment advice, and open questions were not converted into defects unless they
  expressed a concrete complaint or validation dependency. `ASSURE-04` retains the recurring bus-factor
  and external-control concern because it materially shaped all three security verdicts.
- This ledger does not claim that any finding applies to the current `main` branch.
- No source path cited by a review was re-inspected, no test was run, and no external state was queried
  while producing this aggregate. Its only verified completeness property is the crosswalk over all 31
  numbered findings in the five source documents.
