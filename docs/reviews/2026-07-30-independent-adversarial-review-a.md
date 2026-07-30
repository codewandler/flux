---
title: Flux independent adversarial review A
date: 2026-07-30
kind: independent-review
lens: security-and-production-readiness
method: source-level adversarial desk review with targeted non-destructive tests (no fuzzing or exploitation)
reviewer: independent review A
subject:
  repo: codewandler/flux
  exact_commit: cb3bb057c961db70769330b375299e09a2fabfcb
  version_in_tree: 0.37.0
  latest_semver_tag: v0.37.0
  latest_semver_tag_commit: a0ad8219706ad14966db3d420ca814e8a30d8baf
  commits_after_tag: 28
  workspace_crates: 38
overall_rating: 6/10
verdict: Security-engineered beta; suitable for constrained pilots, not yet a trusted unattended boundary
ratings:
  security_architecture: 8/10
  secure_defaults: 5/10
  implementation_quality: 7.5/10
  security_assurance: 6/10
  release_supply_chain: 5/10
  product_maturity: 5.5/10
  community_bus_factor: 2/10
  production_readiness: 5/10
verification:
  status: independently verified against commit cb3bb057c961db70769330b375299e09a2fabfcb
  outcome: findings below are source-anchored; four targeted checks passed
  material_errors: none known within stated limitations
top_findings:
  - Plugin HTTP, OAuth, and TCP callbacks re-resolve DNS after the egress guard and remain rebinding-vulnerable
  - Core release jobs execute remotely downloaded installers without a locally verified digest or signature
  - git_diff is classified as a low-risk observer but can invoke configured external diff programs
  - Process sandboxing and sandbox network isolation remain opt-in, with no Windows backend
  - The authenticated daemon has body, timeout, and A2A in-flight bounds but no request rate limiter
---

## Verdict

**Overall rating: 6/10 — security-engineered beta; suitable for constrained pilots, not yet a trusted unattended boundary.**

Flux's central design is unusually serious for an agent framework: typed authority requirements,
default-deny policy evaluation, deny-first permissions, approval, physical-path normalization, guarded
I/O, and output redaction all converge at one dispatch path. The current tree also closes several
material gaps from the 2026-07-29 baseline: third-party actions are commit-pinned, dependency and
license scanning is scheduled, server routers impose request-body and response-production bounds,
and unauthenticated non-loopback routers are refused at construction.

That architecture is not yet a sufficient production boundary. The most important defect found in
this review is a concrete mismatch inside the advertised egress envelope: native web operations pin
connections to DNS answers vetted by the SSRF guard, while plugin HTTP, OAuth-token, and raw TCP
paths discard those answers and resolve the hostname again when connecting. A granted
attacker-controlled hostname can therefore rebind from a public address during validation to a
private or link-local address during connection. Release jobs also bootstrap executable tooling from
remote scripts without an independently pinned digest or signature, then use that tooling in the
artifact publication path.

Recommended posture:

- **Local experimentation and controlled internal pilots:** reasonable with the hardening below.
- **Unattended execution on valuable repositories:** only inside an additional disposable VM or
  container, with Flux sandbox mode `require` and narrowly scoped credentials.
- **Internet-exposed, auto-approving deployment or plugins with network callbacks:** not recommended
  until the plugin DNS-pinning defect is fixed and independently regression-tested.

This is a desk review, not a penetration test. No exploit was attempted.

## Review target and method

The inspected tree was clean at the start of review and pinned to commit
`cb3bb057c961db70769330b375299e09a2fabfcb`, described as
`v0.37.0-28-gcb3bb057`. The workspace declares version `0.37.0` and enumerates 38 members
(`Cargo.toml:3-42`, `Cargo.toml:47-54`). The latest semantic tag is `v0.37.0`, peeled to commit
`a0ad8219706ad14966db3d420ca814e8a30d8baf`; the reviewed tree is 28 commits beyond it.

The review read security-relevant source, tests, CI workflows, release configuration, and the dated
2026-07-29 baseline. Documentation and design claims were used for orientation only; findings rely
on executable source or workflow configuration. I deliberately did not inspect other reviewers'
new artifacts under `docs/reviews/`.

Targeted checks run:

| Check | Result |
| --- | --- |
| `cargo test -p flux-codegate` | Passed: 13 tests, including layering and direct-I/O/process choke-point gates |
| `bash scripts/check-action-pins.sh` | Passed: all 63 third-party action references pinned to commit SHAs |
| `cargo test -p codewandler-flux-system net::tests --lib` | Passed: 10 tests |
| `cargo test -p codewandler-flux-plugin host::tests --lib` | Passed: 81 tests |

I also searched `.github/`, `scripts/`, and `crates/` for CodeQL, fuzz, Miri, sanitizer, SLSA, and
attestation configuration; there were no relevant matches. The absence claim in Finding 6 is bounded
to those repository-controlled paths.

## Ratings

| Area | Rating | Assessment |
| --- | ---: | --- |
| Security architecture | **8/10** | Strong centralized authorization/approval/guarded-I/O design; plugin egress is a material implementation exception |
| Secure defaults | **5/10** | Default-deny policy is good, but OS confinement and sandbox network denial remain opt-in |
| Implementation quality | **7.5/10** | Careful Rust, extensive tests, explicit invariants; a few security claims are stronger than the code paths they describe |
| Security assurance | **6/10** | Dependency scanning and architecture gates improved; no visible fuzzing, SAST, sanitizers, Miri, or independent audit gate |
| Release/supply chain | **5/10** | Exact-SHA candidate promotion, action pins, and plugin signatures are strong; core release bootstrapping and provenance are weak |
| Product maturity | **5.5/10** | Broad and rapidly improving, but still pre-1.0 and 28 commits beyond the latest tag on the review date |
| Community/bus factor | **2/10** | `git shortlog -sne --all` attributes all 1,132 visible commits to one person under three addresses |
| Production readiness | **5/10** | Credible for constrained pilots; not yet suitable as a privileged, exposed, auto-approving service |

## Strengths

### The runtime has a real centralized enforcement path

`Executor::gate` checks disabled operations and capability scope before evaluating a tool, resolves
filesystem permission subjects to their physical identities, derives authority requirements, applies
default-deny policy decisions, and then applies permission rules (`crates/flux-runtime/src/lib.rs:3619-3707`).
It forces approval for destructive calls, policy-gated calls, and writes without subjects
(`crates/flux-runtime/src/lib.rs:3853-3886`). Execution occurs only after those gates, and both
successful output and errors are redacted before returning (`crates/flux-runtime/src/lib.rs:3993-4010`).
The policy evaluator itself returns `Deny` when no grant matches (`crates/flux-policy/src/lib.rs:465-515`).

The targeted `flux-codegate` run passed all 13 tests, including the structural rules that model-facing
tools do not open raw project I/O and process creation remains behind `flux-system`. This does not prove
the absence of semantic misclassification, but it materially reduces bypass surface.

### Native web egress handles DNS rebinding correctly

The shared network layer explicitly identifies connect-time DNS re-resolution as a cloud-metadata
SSRF risk and exposes vetted socket addresses for callers to pin (`crates/flux-system/src/net.rs:96-108`).
The native web client fails closed on an empty vetted set and uses
`resolve_to_addrs` for each redirect hop (`crates/flux-web/src/egress.rs:124-150`). The ten focused
network-guard tests passed. This is the correct pattern; Finding 1 is that plugin consumers do not
follow it.

### Server authentication and basic resource bounds are materially stronger

Router construction rejects an open non-loopback bind (`crates/flux-server/src/lib.rs:642-655`), and
the route layout structurally separates public discovery endpoints from authenticated routes while
applying a global 1 MiB body cap and a 300-second default response-production timeout
(`crates/flux-server/src/lib.rs:563-575`, `crates/flux-server/src/lib.rs:706-765`). Authenticated modes
reject duplicate authorization headers, compare shared secrets in constant time, and apply a realm
guard to session routes (`crates/flux-server/src/lib.rs:975-1065`). A2A background work additionally
has a default cap of 64 live tasks per realm and rejects before session creation
(`crates/flux-server/src/a2a.rs:249-260`, `crates/flux-server/src/a2a.rs:430-446`).

### CI and plugin release hygiene show meaningful discipline

The main CI pins Rust 1.97, checks formatting, runs warning-denying Clippy, builds and tests the whole
workspace, and executes the architecture lint (`.github/workflows/ci.yml:14-66`). The security
workflow runs weekly and on pushes/PRs, scanning both root and plugin lockfiles with `cargo-deny` and
`cargo-audit` (`.github/workflows/security-audit.yml:19-88`). The action-pin script independently
passed all 63 references. Plugin releases sign an aggregate index whose entries carry artifact
SHA-256 values, and refuse a publish when the Minisign key is absent
(`.github/workflows/release-plugins.yml:176-199`).

## Findings

### 1. High — Plugin HTTP, OAuth, and raw TCP callbacks are vulnerable to DNS-rebinding TOCTOU

**Evidence.** The network module states the exact invariant: returning only a URL discards vetted DNS
addresses (`guard_url_scoped_with_resolver` maps away the second tuple element), while the pinned API
returns the addresses needed to prevent reqwest from re-resolving at connection time
(`crates/flux-system/src/net.rs:81-108`). Plugin HTTP validation nevertheless calls the URL-only API
(`crates/flux-plugin/src/host.rs:1629-1634`). Its reusable reqwest client only disables redirects and
does not install vetted address overrides (`crates/flux-plugin/src/host.rs:231-237`); the send loop
then calls that client with the hostname, causing a fresh resolution
(`crates/flux-plugin/src/host.rs:649-713`). Both endpoint-ref and raw-URL request paths feed this
unpinned loop (`crates/flux-plugin/src/host.rs:1134-1179`).

The same defect reaches more than ordinary plugin requests:

- Host-injected query or header credentials are added after validation
  (`crates/flux-plugin/src/host.rs:1187-1209`), so a rebinding destination may receive them. HTTPS
  certificate validation limits some attacks, but HTTP endpoints remain exposed and the SSRF itself
  does not require secret theft.
- Plugin OAuth token URLs are checked by the same URL-only guard, then passed as a string into the
  credential resolver (`crates/flux-plugin/src/host.rs:358-381`). Refresh uses a new unpinned reqwest
  client and POSTs the refresh token (`crates/flux-credentials/src/lib.rs:558-568`,
  `crates/flux-credentials/src/lib.rs:595-633`).
- TCP dialing validates one DNS answer, discards it, and calls `TcpStream::connect` on the hostname,
  which resolves again (`crates/flux-system/src/net.rs:293-301`,
  `crates/flux-system/src/net.rs:327-344`). The plugin exposes that path after its manifest target
  check (`crates/flux-plugin/src/host.rs:1278-1348`).

**Impact and prerequisites.** A plugin must already have HTTP/connection capability to an
attacker-controlled hostname admitted by its manifest or configured endpoint. The attacker can
answer the guard's lookup with a public address and the connection lookup with loopback, RFC1918,
link-local, or cloud-metadata space. That crosses the advertised egress boundary and can reach
services the private-network policy intended to deny. Raw TCP makes this independent of HTTP/TLS.
This is High rather than Critical because a destination grant and attacker-controlled DNS are
prerequisites, and the review did not demonstrate a complete exploit.

**Recommendation.** Make the pinned result the only egress-guard API available to network callers.
Build a fresh pinned HTTP client per hop for plugin HTTP and OAuth exactly as native web does. Change
TCP guard/dial to connect directly to the vetted `SocketAddr` set without a hostname re-resolution.
Add injected-resolver regression tests that change answers between validation and connection for
HTTP, redirects, OAuth refresh, and `conn.dial`; fail closed when validation yields no vetted address.

### 2. High (supply chain) — Core release jobs bootstrap executable tooling without local integrity verification

**Evidence.** The release planning job downloads the versioned cargo-dist installer and pipes it
directly to a shell, then caches the installed executable for later jobs
(`.github/workflows/release.yml:103-112`). Container build jobs similarly pipe the rustup installer to
`sh` and execute a generated `matrix.install_dist.run` command (`.github/workflows/release.yml:227-235`).
The publishing host downloads the cached `dist` executable, marks it executable, and runs it to upload
and release artifacts (`.github/workflows/release.yml:376-405`). The final GitHub release is created
with a repository-scoped `Contents: write` PAT (`.github/workflows/release.yml:450-461`). No digest,
signature, SLSA attestation, or equivalent local verification precedes execution of the downloaded
installers in this workflow. User documentation also recommends piping the latest installer directly
to `sh` or `iex` (`README.md:56-65`).

**Impact and constraints.** Compromise of an upstream installer endpoint, owner account, or replaceable
release asset can alter release tooling and therefore the binaries/installers users trust. The exact
cargo-dist version and TLS reduce accidental drift, and checkout credentials are disabled in relevant
jobs, but a versioned URL is not content authentication. The workflow's exact-SHA candidate receipt is
a strong repository provenance check (`.github/workflows/release.yml:139-190`); it does not authenticate
the external bootstrap executable.

**Recommendation.** Pin each bootstrap artifact by SHA-256 or a verified upstream signature; preferably
use a repository-controlled, hash-locked tool acquisition step. Give planning/build jobs read-only
permissions and isolate publication credentials to the smallest final job. Produce and publish signed
SLSA provenance for core artifacts, and cryptographically sign core manifests/artifacts as the plugin
index already does. Change install documentation to a version-pinned download plus explicit signature
or checksum verification, keeping pipe-to-shell only as a clearly labeled convenience path.

### 3. Medium — `git_diff` can execute configured external programs despite its low-risk observer classification

**Evidence.** `git_diff` declares `Effect::Process` but `Risk::Low`; the comment and coherence exemption
justify that posture as a fixed, observation-only `git diff` invocation
(`crates/flux-tools/src/lib.rs:2304-2321`, `crates/flux-spec/src/coherence.rs:97-123`). Its argv omits
`--no-ext-diff` (`crates/flux-tools/src/lib.rs:2346-2357`). Git can therefore invoke a program selected
by `diff.external` or a configured per-path external driver. Elsewhere in the same crate, the hunk
implementation correctly adds `--no-ext-diff` because user Git configuration can otherwise change
execution (`crates/flux-tools/src/lib.rs:3172-3188`).

**Impact and constraints.** This is not arbitrary model-supplied command execution. It requires a
malicious or previously configured Git external-diff program on the host/repository. The default
policy also marks `process.exec` as approval-required (`crates/flux-policy/src/lib.rs:447-450`). The
problem is that an operation described and exempted as a low-risk observer has a wider execution
surface than its security metadata claims; in an approved plan or explicitly auto-approving server,
the approval layer does not compensate for that mismatch.

**Recommendation.** Add `--no-ext-diff` and audit whether `--no-textconv` is also required for a truly
built-in-only observer. Add a regression test with a configured external driver that proves it is not
started. Remove or tighten the I1 exemption if any configuration-dependent code execution remains.

### 4. Medium (deployment risk) — OS sandboxing and network isolation remain opt-in

**Evidence.** `SandboxSettings::off` is the default, with unrestricted network; an absent or unknown
`FLUX_SANDBOX` value resolves to `Off`, and absent `FLUX_SANDBOX_NET` resolves to open
(`crates/flux-system/src/sandbox.rs:35-93`). `require` fails closed when no backend is available, but
`on` explicitly continues unconfined (`crates/flux-system/src/sandbox.rs:322-337`). Linux and macOS
have Bubblewrap and Seatbelt backends; Windows is explicitly unsupported
(`crates/flux-system/src/sandbox.rs:1-11`).

**Impact and constraints.** The sandbox is documented as defense in depth rather than the primary
authorization boundary, so this is not itself a bypass. It materially increases consequence when
metadata, an external program, a plugin binary, or a guarded-I/O implementation is wrong. The new
operator disclosure for degraded `on` mode is valuable, but it does not confine the process.

**Recommendation.** Require `sandbox=require` with network closed for unattended and server use, and
fail deployment preflight otherwise. Consider secure profile presets rather than silently changing
interactive developer defaults. On Windows, require an outer VM/container until a real backend ships.

### 5. Medium (production hardening) — The daemon has no general request rate limiter

**Evidence.** `ServerLimits` bounds body size and response-production time but explicitly leaves rate
limiting out of scope (`crates/flux-server/src/lib.rs:563-581`). The single- and multi-agent routers
compose authentication, body-limit, and timeout layers without a request-rate or general concurrency
layer (`crates/flux-server/src/lib.rs:706-765`, `crates/flux-server/src/lib.rs:935-960`). The A2A task
registry does cap live background turns per realm at 64 (`crates/flux-server/src/a2a.rs:249-260`), but
that is not a general limiter for REST sessions, webhook requests, blocking A2A sends, or model spend.

**Impact and constraints.** Exploitation requires a valid bearer token (or a loopback client in open
mode). Such a client can create request queues, consume provider budget, or generate sustained tool
work up to the existing timeout/concurrency boundaries. Authentication, the 1 MiB body cap, serialized
turn gates, and A2A cap reduce the blast radius, so this is Medium rather than High.

**Recommendation.** Add bounded concurrency and token/principal/realm-aware request and spend quotas,
with `429` responses and observable counters. Put equivalent limits at the reverse proxy until native
controls exist. Treat per-realm budget limits as part of the principal-mode security contract.

### 6. Medium (assurance gap) — Security validation remains largely implementer-authored and non-adversarial

**Evidence.** Main CI is comprehensive for correctness—format, Clippy, build, tests, layering, and
offline smoke shape (`.github/workflows/ci.yml:14-66`). The dedicated security workflow adds
dependency advisories, licenses, and source policy (`.github/workflows/security-audit.yml:31-88`).
However, the repository-controlled CI/scripts/crates search found no CodeQL or equivalent SAST, fuzz
targets, Miri, sanitizers, or SLSA attestations. The security policy promises prompt handling but no
response timeline and supports only the latest `0.x` minor (`SECURITY.md:22-23`, `SECURITY.md:35-38`).
Finally, all 1,132 commits in `git shortlog -sne --all` resolve to one maintainer under three email
addresses.

**Impact and constraints.** This is not a source vulnerability. It lowers confidence that parser,
protocol, concurrency, and boundary errors will be found before release and makes security response
dependent on one person. The plugin DNS gap is an example of a cross-consumer invariant that focused
unit tests did not catch even though both relevant test suites pass.

**Recommendation.** Start coverage-guided fuzzing with network URL/redirect logic, plugin framed NDJSON,
provider codecs, Flux-Lang parsing, and policy/permission normalization. Add a SAST lane, periodic Miri
or sanitizer jobs for suitable crates, release attestations, and a recurring independent audit. Publish
a realistic acknowledgement/triage target and recruit at least one independent release/security
reviewer.

## Open questions

These could not be answered from the repository and should be resolved before a production approval:

1. Are branch protection, required reviews, environment approvals, secret scopes, and immutable-release
   settings enforced in GitHub? Workflow YAML alone cannot prove those external controls.
2. Have any private penetration tests, independent audits, fuzz campaigns, or incident-response
   exercises occurred and produced evidence not committed to this repository?
3. Are release assets or upstream installer artifacts protected against replacement after publication,
   and is the release PAT restricted by environment approval and short rotation?
4. What operational limits exist outside Flux—reverse-proxy rate limits, provider spend caps, log
   retention, alerting, backup/restore drills, and revocation latency for principal tokens?
5. Which plugin manifests currently grant attacker-influenceable HTTP or TCP hostnames, and are any
   OAuth token endpoints served over plain HTTP or otherwise able to bypass meaningful TLS identity?

## Deployment recommendation

For a controlled pilot, use all of the following:

```text
outer isolation: dedicated unprivileged VM/container
workspace: disposable clone; never a home directory or production checkout
Flux sandbox: require
sandbox network: disabled by default; explicit destination grants only
approval: interactive for every write, process, network, and external semantic action
server: loopback-only unless principal auth terminates directly; no shared unattended --yes token
rate/spend control: enforce at reverse proxy and provider account
credentials: short-lived, narrowly scoped, non-exportable where possible
plugins/hooks: disabled; if essential, no attacker-controlled hostnames until Finding 1 is fixed
release install: version-pinned artifact with independently verified checksum/signature
```

Do not treat Flux's default process posture as host isolation. Do not expose an auto-approving daemon
directly to the Internet. Do not enable plugin HTTP, OAuth, or TCP callbacks for destinations whose DNS
can be controlled by an untrusted party until connection pinning is implemented.

Reassess production readiness after Findings 1-3 are fixed with regression tests, release bootstrap is
content-authenticated, rate/spend controls exist, and at least one independent dynamic assessment has
exercised the complete authorization-to-I/O envelope.

## Limitations

- No fuzzing, exploitation, malicious plugin execution, live network test, live provider test, or
  performance/load test was performed.
- The full workspace build/test/Clippy/fmt gate was not rerun. Only the four targeted checks listed in
  the method section were executed.
- GitHub organization settings, branch protections, workflow run history, secret/environment policy,
  registry state, published artifact bytes, and deployment infrastructure were not inspected.
- The review sampled all architectural layers but did not manually inspect every operation or every
  dependency. Passing tests establish only the exercised behavior.
- The tree was 28 commits beyond the latest semantic tag. Findings apply to the exact commit pinned in
  frontmatter, not automatically to the `v0.37.0` release artifact or later commits.
