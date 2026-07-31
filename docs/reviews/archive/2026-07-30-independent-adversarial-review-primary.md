---
title: Flux full-project adversarial review — primary pass
date: 2026-07-30
kind: internal-review
lens: security-and-production-readiness
method: >
  Independent source-and-configuration desk review of the local worktree, its safety envelope,
  model-facing operation packs, fleet/A2A egress, process launcher, server, plugins, CI, and release
  workflow. Historical reviews were used only as a baseline to re-check. No fuzzing, exploitation,
  live-provider calls, network probing, or Cargo build/test was performed.
reviewer: agent (primary)
subject:
  repo: codewandler/flux
  version_in_tree: 0.37.0
  published_release_at_review: v0.37.0
  workspace_crates: 38
  commit: cb3bb057c961db70769330b375299e09a2fabfcb
overall_rating: 5.5/10
verdict: >
  The core envelope is carefully engineered and its assurance improved materially, but two current
  outer-surface paths defeat its intended network/process containment and unattended failures still
  report success; do not treat this tree as a trusted unattended security boundary.
ratings:
  security_architecture: 8/10
  secure_defaults: 5/10
  implementation_quality: 7/10
  security_assurance: 6/10
  release_supply_chain: 5/10
  product_maturity: 5.5/10
  community_bus_factor: 2/10
  production_readiness: 4.5/10
verification:
  status: verified against local tree at 0.37.0 (cb3bb057) on 2026-07-30
  outcome: two high-severity containment gaps and one high-severity release-pipeline exposure confirmed
  material_errors: none known; desk-review limitations and open questions are stated below
top_findings:
  - "HIGH: fleet.* guards only the first DNS answer, then reconnects through an unpinned, redirect-following client"
  - "HIGH: eval_run lets model input select a sandbox-exempt executable that receives raw provider credentials"
  - "HIGH: release jobs execute unsigned remote installers while holding write-capable GitHub tokens"
  - "MEDIUM: provider-stage failures are converted to successful turn results and exit zero"
  - "MEDIUM: OS confinement remains off by default, with sandboxed network open by default"
triage:
  kind: single
  status: handled
  triaged_on: 2026-08-01
  aggregated_into: docs/reviews/aggregate/2026-08-01-aggregate-complaint-triage.md
  normalized_claims: [NET-02, PROC-01, REL-01, REL-02, OUTCOME-01, SANDBOX-01, SRV-02, ASSURE-02, ASSURE-03]
  filed_as: [C-255, C-345, C-352, C-363, C-369]
  note: >-
    Every numbered finding in this pass was normalized into the aggregate ledger, validated
    against the tree on 2026-08-01, and either confirmed closed or filed as a residual story.
    Archived: cited as evidence, not awaiting triage. Do not re-triage in isolation — the
    ledger records the cross-review disagreements this document cannot see on its own.
---

# Verdict

Flux's central design remains unusually strong: operations are typed, authorization and approval
share one dispatcher gate, filesystem access is centralized, and the repository has invested in
specific regression checks rather than relying on prompt-level promises. Since the 0.33 baseline,
the tree has added SHA-pinned Actions, dependency/license scanning, server body and timeout limits,
construction-time loopback auth, a direct-IO lint, and fixes for the previously confirmed SQLite
escape.

That progress is real, but the present tree also demonstrates the same structural risk the earlier
review warned about: a sound envelope only protects effects whose outer adapter faithfully uses it.
The newly registered fleet surface performs a check-then-re-resolve network request, and the eval
operation pack treats a model-supplied executable as a trusted sandbox exemption while injecting raw
credentials. Both are concrete gaps between the security claim and the implementation.

This is a desk review, not a penetration test. The worktree was clean and 28 commits ahead of
`origin/main` when reviewed. The exact local commit above is the subject, not the remote branch.

# Ratings

| Area | Rating | Assessment |
| --- | ---: | --- |
| Security architecture | **8/10** | Central policy/approval/guarded-IO design is coherent and strongly tested where reached |
| Secure defaults | **5/10** | Sandbox and sandbox network isolation remain opt-in; local policy is deliberately permissive |
| Implementation quality | **7/10** | High-quality defensive code, offset by two adapter-level contradictions of explicit safety contracts |
| Security assurance | **6/10** | Stronger scans and bespoke gates; still no visible fuzzing/SAST/Miri/sanitizer program, and a key lint excludes a model-facing pack |
| Release/supply chain | **5/10** | SHA-pinned Actions and checksums are positive; unsigned remote scripts still execute in privileged release jobs |
| Product maturity | **5.5/10** | Broad and sophisticated, but rapidly changing pre-1.0 surfaces still expose correctness gaps |
| Community/bus factor | **2/10** | Local history is effectively one maintainer under three email identities |
| Production readiness | **4.5/10** | Controlled pilots are plausible; trusted unattended or exposed operation is not yet justified |

# Strengths verified in the current tree

- **The process choke point is real.** Production `Command::new` construction is centralized in
  `crates/flux-system/src/lib.rs:2090-2148`; it uses argv, a workspace-pinned cwd, environment
  clearing, process-group isolation where applicable, and one sandbox wrapping seam. Output is
  capped at 1 MiB for bounded commands and 256 KiB per managed stream
  (`crates/flux-system/src/lib.rs:518`, `:932`).
- **The prior SQLite escape is closed in depth.** `sqlite_query` now admits only four leading
  statement types (`crates/flux-tools/src/extra.rs:206-276`), confines database paths to the
  workspace or `~/.flux` (`:290-324`), and opens read-only; regression tests explicitly cover
  `VACUUM INTO` (`:719-828`).
- **Web and plugin HTTP paths show the right egress pattern.** Native web operations use
  `guard_url_scoped_pinned` and disable automatic redirects
  (`crates/flux-web/src/fetch.rs:176-188`, `crates/flux-web/src/egress.rs:21`); the plugin host also
  constructs a redirect-disabled client (`crates/flux-plugin/src/host.rs:235`) and rechecks scoped
  capabilities at callback time.
- **Server hardening materially improved.** Open auth is rejected during router construction for a
  non-loopback bind (`crates/flux-server/src/lib.rs:482-498`), and every router receives a finite body
  cap plus non-streaming timeout (`:586-635`, `:688-765`). A2A additionally caps in-flight tasks per
  realm (`crates/flux-server/src/a2a.rs:234-260`).
- **CI closed several prior assurance findings.** Third-party Actions are commit-SHA-pinned and
  guarded (`.github/workflows/ci.yml:126-139`); root and plugin dependency graphs receive both
  `cargo-deny` and `cargo-audit`, including a weekly schedule
  (`.github/workflows/security-audit.yml:19-88`). The local read-only checks confirmed all 63 Action
  references are pinned and the scoped direct-IO scan is currently clean.
- **Sub-agent privilege propagation is thoughtfully bounded.** Children inherit the authorization
  floor, default to denying destructive work, intersect tool scopes on descent, and cap recursion
  (`crates/flux-orchestrate/src/lib.rs:48-82`, `:179-250`).

# Findings

## 1 — HIGH · `fleet.*` is vulnerable to DNS rebinding and redirect-based SSRF after its initial guard

The CLI registers `fleet.dispatch`, `fleet.status`, and `fleet.cancel` unconditionally and describes
their sole authority as the egress guard (`crates/flux-cli/src/execution.rs:1188-1198`). Their shared
client constructor calls `guard_url_scoped(endpoint, ...)`, discards the returned URL, and constructs
an `A2aClient` again from the original string (`crates/flux-orchestrate/src/fleet.rs:258-262`).

The shared network module documents exactly why that non-pinned API is insufficient: reqwest resolves
again at connection time, permitting a low-TTL attacker hostname to answer public during the guard
and private/link-local during connect; callers must use `guard_url_scoped_pinned` to close it
(`crates/flux-system/src/net.rs:96-117`). The A2A client builds a default reqwest client
(`crates/flux-a2a/src/client.rs:54-75`) and sends directly to its stored URL (`:154-158`) without
pinning or revalidating a redirect destination. By contrast, the native web and plugin paths disable
redirects and/or manually re-guard each hop, as cited under Strengths.

**Impact.** A model-named public worker can change DNS between the guard and connection, or answer
with an HTTP redirect to a private, loopback, link-local, or metadata address. The follow-up request
then occurs outside the private-network grant. `fleet.dispatch` can preserve POST across a 307/308,
so this is not merely a connectivity probe. This violates the repository's release-blocking egress
invariant.

**Bounds.** The default CLI currently supplies no worker bearer token
(`crates/flux-cli/src/execution.rs:1099-1108`), so this path does not expose such a token in the stock
assembly. Exploitation also requires the fleet op to be called and the initial public destination to
be attacker-controlled. Those limits reduce reach, not the boundary violation.

## 2 — HIGH · `eval_run` runs a model-selected executable outside the sandbox with provider secrets

`EvalRunTool` is a real `Tool` registered into the production catalog
(`crates/flux-cli/src/execution.rs:1197-1198`). Its open input object exposes `flux_bin`, described as
the binary under test (`crates/flux-eval/src/ops.rs:35-59`); the execution path resolves that
caller-supplied path without restricting it to the current executable or a trusted build output
(`:131-147`).

The runner places that path in `argv[0]` (`crates/flux-eval/src/runner.rs:279-287`), explicitly injects
`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, and `FLUX_SECRET` from the parent
environment (`:288-303`), then invokes `run_with_env_exempt` / `run_with_env_streamed_exempt`
(`:317-323`). The process API's own contract says this exemption is only for a trusted host and that
**model-selected executables must never use it** (`crates/flux-system/src/lib.rs:1893-1905`).

**Impact.** In an eval-signaled workspace (or an authored flow that invokes the op), a model can point
`flux_bin` at an existing attacker-controlled executable. Under `--yes`, or after the generic
process approval, that executable runs without the configured OS sandbox and receives live provider
credentials. It can read outside the workspace and exfiltrate those credentials over unrestricted
network access. The process is argv-only and output-capped, but those controls do not address this
threat.

**Why the gate missed it.** The direct-IO lint explicitly excludes `flux-eval`, claiming it is not
model-facing and never runs on model-controlled input (`scripts/check-no-direct-io.sh:16-25`), while
the production CLI registration and `EvalRunTool` prove both premises false. The lint scans only
`flux-tools`, `flux-web`, and `flux-capabilities` (`:56-57`). This finding is therefore both a defect
and a demonstrated assurance-coverage gap.

## 3 — HIGH · Privileged release jobs execute unsigned remote installer scripts

The release workflow grants `contents: write` globally (`.github/workflows/release.yml:16-19`). Its
planning job exports the write-capable `GITHUB_TOKEN` as `GH_TOKEN` (`:60-75`), then pipes a
versioned-but-unverified cargo-dist release asset directly into `sh` (`:103-107`). Build jobs likewise
export `GH_TOKEN` (`:193-218`) and, in containers, pipe `https://sh.rustup.rs` into `sh` (`:227-235`).

Action SHA pinning does not cover either path: the pin guard enumerates only YAML `uses:` references
(`scripts/check-action-pins.sh:74-90`). A compromise of either upstream distribution account or its
mutable release asset can therefore execute inside a release job, poison binaries, and—where the
token is usable—mutate repository or release state.

Core release verification checks that `sha256.sum` exists but does not verify a cryptographic
signature or provenance attestation (`scripts/verify-github-release.sh:67-105`). Checksums generated
by the same compromised job do not establish an independent trust root. Plugin packs are better: the
index is Minisign-signed, but that protection does not cover core binaries.

## 4 — MEDIUM · Provider-stage failures are laundered into successful turns

Both intent detection and exploration convert a provider `Err` into `Ok({"kind":"error", ...})`
(`crates/flux-flow/src/loop_host.rs:563-586`). The NDJSON `turn_end` schema carries no outcome field
(`crates/flux-cli/src/stream_json.rs:83-97`), and the CLI emits its typed `error` record / non-zero
return only when `run_turn` itself returns `Err` (`:275-294`). These stage failures therefore end as
ordinary `turn_end` records and exit zero.

**Impact.** CI, editors, coordinators, and fleet drivers cannot distinguish a completed turn from a
turn that never reached execution without parsing human prose. This is especially dangerous for
unattended operation: an automation layer can publish partial work or mark a board item done after a
provider failure. The repository already records the issue as C-226, but it is live in the reviewed
tree and must count against current production readiness.

The adjacent resilience gap is also open: transport-class stream failures have no bounded automatic
resume (C-227), and a reproducible Gemini/OpenRouter stream failure remains under diagnosis (C-228).
Those are reliability findings, not additional security bypasses.

## 5 — MEDIUM · OS isolation remains an opt-in defense, with network open by default

`SandboxSettings::from_env` resolves an unset/unknown mode to `Off` and an unset network policy to
open (`crates/flux-system/src/sandbox.rs:69-105`). `On` is best-effort: an unavailable backend
continues unconfined, while only `Require` fails closed (`:322-337`). Unsupported platforms have no
backend (`:440-445`).

The new resolved-posture disclosure is a meaningful improvement: `On` plus an unavailable backend
now states that execution is unconfined (`:264-319`). It does not change the default. With two
adapter-level defects confirmed in this review, the absence of a default OS boundary is no longer a
purely hypothetical defense-in-depth concern.

## 6 — LOW · Server abuse controls stop short of rate limiting

The daemon now has body limits, finite request timeouts, authentication, a serial turn gate, and a
64-task per-realm A2A cap. Its own resource-limit documentation explicitly leaves rate limiting out
of scope (`crates/flux-server/src/lib.rs:571-603`). There is no per-token/principal request-rate or
cost-rate control in the inspected router.

Authentication substantially lowers exposure, so this is not the earlier baseline's unbounded-
server finding. It still matters for an internet-facing multi-principal service: one valid principal
can sustain expensive requests up to the concurrency/timeout ceilings, and a shared-token realm
cannot attribute abuse to an individual caller.

## 7 — LOW · Catalog assurance still has documented silent-coverage holes

The published risk-table test constructs only the built-in registry and silently skips every row it
cannot resolve (`crates/flux-tools/tests/toolspec_invariants.rs:123-166`). The broader catalog census
has a drift guard, but it scans only `flux-cli/src/execution.rs` and explicitly admits that a pack
registered from another module—or one reusing a classified label—can escape
(`crates/flux-cli/src/catalog_coherence.rs:298-324`, `:378-381`).

These are not direct bypasses. They are assurance gaps in exactly the classification layer that
controls approval and audit presentation. C-233 and C-234 already describe them accurately; their
continued backlog status should be read as open risk, not completed mitigation.

# Open questions

- **Fleet exploitability under the exact reqwest redirect semantics.** Source proves the unpinned
  second resolution and absence of redirect revalidation. This review did not run a redirect or
  DNS-rebinding fixture against Flux, per the desk-review boundary.
- **Eval executable reachability in real model catalogs.** The operation pack is production-
  registered and grouped behind the `eval` signal (`crates/flux-eval/src/lib.rs:93-108`). This review
  did not execute a live model to measure how readily it selects `flux_bin`, nor build a proof binary.
- **Current CI health.** No Cargo command was run because the initial user constraint prohibited
  writes and Cargo would create/update build artifacts. The repository's workflow configuration was
  inspected; the current remote CI result was not queried.
- **External assurance.** No independent penetration-test report, fuzz corpus, SLSA attestation, or
  reproducible-build evidence was found in the repository. Absence in-tree does not prove none exists
  privately.
- **Provider endpoint policy.** Native model-provider endpoints use configured credentials and direct
  reqwest clients rather than the model-facing web guard. That appears to be an operator-owned trust
  boundary, but the very broad prose claim that *all* web egress uses one guard should be narrowed or
  this exception should be explicitly justified.

# Deployment recommendation

For local evaluation, use a disposable clone in a dedicated unprivileged VM/container, set
`FLUX_SANDBOX=require`, disable sandbox network unless necessary, use short-lived provider keys, and
keep interactive approval enabled. Disable `eval_run` and the `fleet.*` family with `[tools] disable`
until findings 1 and 2 are closed. Do not expose the server directly to the internet; use principal
auth behind an authenticating/rate-limiting proxy and keep each tenant's credentials narrowly scoped.

Do not use this commit for unattended work on valuable repositories or as a privileged remote agent
boundary. If release integrity matters, build from a pinned source commit in a controlled pipeline;
do not rely solely on the one-line installer or checksums generated by the same release workflow.

# Verification performed

- Read-only source/configuration inspection across the 38-crate root workspace, nested plugin
  integration seams, server, system, tools, eval, orchestration/A2A, CI, and release files.
- `./scripts/check-action-pins.sh` — **PASS**, 63 third-party Action references pinned.
- `./scripts/check-no-direct-io.sh` — **PASS**, 43 files in its declared three-crate scope; the scope
  limitation is finding 2's assurance component.
- `git diff --check` — **PASS** before review artifacts were written.
- No build, test, clippy, fmt, fuzzing, live-provider, or network test was run in this pass.

# Change from the 2026-07-29 baseline

The baseline's cheap assurance findings largely closed: Action tags, dependency advisory scanning,
server body/time limits, direct-router auth, the SQLite escape, and the lack of a direct-IO lint all
have concrete fixes. Security assurance therefore rises from 5 to 6.

The overall verdict does not improve because this pass found two current containment defects in
newer outer surfaces and a privileged release-workflow exposure. The architecture/assurance spread
is narrower than it was; the remaining problem is now less “missing evidence” and more “fast-growing
adapters still violate contracts the core states correctly.”
