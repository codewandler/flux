---
title: Flux full-project independent adversarial review B — security and production readiness
date: 2026-07-30
kind: internal-review
lens: security-and-production-readiness
method: >-
  Independent source-level desk review of the root and plugin workspaces, safety envelope, guarded
  IO, server, plugins, CI, dependency policy, and release configuration; compared only with the
  established 2026-07-29 historical baselines. Executed both workspaces' existing test suites and
  the architecture, action-pin, and no-direct-IO gates. No fuzzing, exploitation, live-provider or
  live-network testing, release-publication lookup, load testing, or real platform-sandbox exercise.
reviewer: agent
subject:
  repo: codewandler/flux
  version_in_tree: 0.37.0
  published_release_at_review: v0.37.0 (latest repository tag; external publication not network-verified)
  workspace_crates: 38
  nested_plugin_workspace_crates: 21
  commit: cb3bb057c961db70769330b375299e09a2fabfcb
  distance_from_release_tag: 28 commits after v0.37.0
overall_rating: 7/10
verdict: Materially hardened security-engineered beta; suitable for isolated pilots, but not yet a self-sufficient trusted boundary for unattended valuable workloads.
ratings: { security_architecture: 8.5/10, secure_defaults: 5.5/10, implementation_quality: 8/10, security_assurance: 6.5/10, release_supply_chain: 7/10, product_maturity: 6/10, community_bus_factor: 2/10, production_readiness: 6/10 }
verification:
  status: verified against tree at 0.37.0 (cb3bb057c961db70769330b375299e09a2fabfcb) on 2026-07-30
  outcome: no critical or high-severity bypass confirmed; four medium production risks and two low assurance gaps remain
  material_errors: none
top_findings:
  - "OS process isolation remains opt-in, network-open by default, degradable under `on`, and unavailable on Windows"
  - "The REST SSE route does not cancel work on disconnect and uses an unbounded event channel"
  - "The auto-approving HTTP daemon still has no request rate limit or general concurrency budget"
  - "Core release artifacts are checksummed but neither signed nor accompanied by consumer-verifiable provenance"
  - "Security CI now scans dependencies, but still has no fuzzing, Miri/sanitizer, or SAST lane"
  - "The no-direct-IO gate is a useful lexical tripwire, not a complete invariant checker"
---

## Verdict

Flux has moved materially since the verified 2026-07-29 security-posture baseline. The earlier
action-pin, dependency-scanning, server-body/timeout, router-construction, SQLite escape, and
no-direct-IO findings have all received concrete controls. The current tree's central dispatch
chain is unusually disciplined for a pre-1.0 agent platform, and both Rust workspaces pass their
existing tests at the reviewed commit.

The remaining gap is narrower than the baseline's, but still important: host isolation is not the
default, the HTTP daemon has lifecycle and abuse-control gaps around expensive auto-approved work,
and consumers cannot independently authenticate core release artifacts. I found no confirmed
critical or high-severity envelope bypass in this pass. I would approve an isolated internal pilot;
I would not treat Flux alone as the security boundary for unattended execution on a valuable host
or expose its auto-approving daemon directly to the Internet.

## Ratings

| Area | Rating | Change from 2026-07-29 baseline | Assessment |
| --- | ---: | :---: | --- |
| Security architecture | **8.5/10** | +0.5 | One shared dispatch gate, physical path identity, immutable turn identity, guarded egress, and capability-scoped plugins |
| Secure defaults | **5.5/10** | +0.5 | Server defaults improved; OS confinement and sandbox network isolation remain opt-in |
| Implementation quality | **8/10** | +0.5 | Strong failure handling and regression tests; REST streaming lifecycle is behind the A2A path |
| Security assurance | **6.5/10** | +1.5 | Advisory/license/source scanning and new structural gates landed; no fuzz/SAST/Miri or external audit evidence |
| Release/supply chain | **7/10** | +0.5 | SHA-pinned actions, locked graphs, signed plugin index; core artifacts remain unsigned/unattested |
| Product maturity | **6/10** | +1 | Broad, tested surface, but still pre-1.0 and moving quickly (28 commits beyond the latest tag) |
| Community/bus factor | **2/10** | = | `git shortlog -sne --all` resolves all 1,132 commits to one maintainer identity across three addresses |
| Production readiness | **6/10** | +1 | Reasonable under external isolation and ingress controls; not a standalone hardened daemon boundary |

The reviewed tree declares `0.37.0` at `Cargo.toml:47-54`, contains the 38 root members enumerated at
`Cargo.toml:3-42`, and excludes the separately tested plugin workspace at `Cargo.toml:43-45`.

## Strengths

- **The envelope is one real code path, not only a diagram.** `Executor::dispatch` enters the shared
  gate at `crates/flux-runtime/src/lib.rs:3554-3583`; filesystem subjects are resolved to physical
  targets before matching at `crates/flux-runtime/src/lib.rs:3637-3660`; default-deny policy is
  evaluated before permission rules at `crates/flux-runtime/src/lib.rs:3663-3697`; approval is forced
  for destructive, policy-sensitive, and unscoped-write calls at
  `crates/flux-runtime/src/lib.rs:3853-3886`; only then can `Tool::execute` run, with output redacted at
  `crates/flux-runtime/src/lib.rs:3993-4011`.

- **Server authentication is structurally difficult to omit.** `ServerAuth::Open` is documented and
  represented as loopback-only at `crates/flux-server/src/lib.rs:48-72`; router construction itself
  receives the bind address and refuses open non-loopback use at
  `crates/flux-server/src/lib.rs:637-655`, rather than trusting only the high-level server launcher.
  Protected routes are mounted behind one auth layer at `crates/flux-server/src/lib.rs:754-766`, and
  duplicate authorization headers are rejected before constant-time shared-secret comparison at
  `crates/flux-server/src/lib.rs:972-1006`.

- **The prior server resource-limit finding was substantially closed.** Every body-buffering route
  receives a 1 MiB default cap and non-streaming work receives a 300-second default timeout
  (`crates/flux-server/src/lib.rs:563-629`). The cap is applied outermost to the full single-agent
  router at `crates/flux-server/src/lib.rs:760-766` and the multi-agent router at
  `crates/flux-server/src/lib.rs:948-960`. Existing tests prove 413 and 408 behavior at
  `crates/flux-server/src/lib.rs:1789-1850`.

- **Native web egress handles the hard SSRF cases.** Redirects are disabled in the underlying client
  and followed manually (`crates/flux-web/src/egress.rs:14-24`); every hop is re-guarded, HTTPS
  downgrade is refused, and cross-origin headers are cleared at
  `crates/flux-web/src/egress.rs:88-120`. Each connection is pinned to the addresses already vetted,
  and an empty pin set fails closed at `crates/flux-web/src/egress.rs:124-150`.

- **Plugin distribution and host capabilities are explicit about their boundary.** Signed-pack
  verification is fail-closed before archive bytes are trusted at
  `crates/flux-plugin/src/pack.rs:164-190`; installed hashes are rechecked at spawn through
  `crates/flux-plugin/src/host/loading.rs:739-788`. Private-network access requires both a manifest
  declaration and an operator grant, a conjunction asserted at
  `crates/flux-plugin/src/host.rs:3319-3345`. The user documentation correctly states that native
  plugins remain trusted code, not an OS sandbox (`website/docs/plugins/using-plugins.md:8-19`).

- **The earlier SQLite escape is closed with layered controls.** The statement-type allowlist strips
  leading comments (`crates/flux-tools/src/extra.rs:245-275`), the path is confined to the workspace
  or `~/.flux` (`crates/flux-tools/src/extra.rs:290-323`), and the connection is opened read-only only
  after allowlist admission (`crates/flux-tools/src/extra.rs:373-425`). The exceptional direct IO is
  named and mechanically visible instead of pretending to satisfy the general rule.

- **CI has become materially more security-relevant.** Third-party actions are SHA-pinned and guarded
  against regression (`.github/workflows/ci.yml:126-139`). `cargo-deny` and `cargo-audit` cover both
  root and nested plugin graphs on pushes, pull requests, and a weekly schedule
  (`.github/workflows/security-audit.yml:19-26`, `:31-88`). The advisory exceptions are individually
  reasoned rather than blanket ignores (`deny.toml:47-74`).

## Findings

### 1 — MEDIUM · Host process isolation remains an opt-in, network-open control

The default `SandboxSettings` is `Off`, permits network, and adds no writable restriction
(`crates/flux-system/src/sandbox.rs:58-66`). Environment resolution repeats those defaults when the
operator provides no setting (`crates/flux-system/src/sandbox.rs:74-93`). On platforms other than
Linux and macOS, the backend is always unsupported (`crates/flux-system/src/sandbox.rs:440-445`). On
Linux/macOS, `on` deliberately degrades to unconfined execution if the functional backend probe is
unavailable; only `require` converts that state to an error
(`crates/flux-system/src/sandbox.rs:533-538`, `:322-337`).

This is not an observed authorization bypass: the Rust policy, permission, guarded-path, and redaction
layers remain active, and `require` is genuinely fail-closed. The new once-per-process disclosure is
a real improvement: an `on` request that degraded says it is running unconfined and tells the operator
to select `require` (`crates/flux-system/src/sandbox.rs:264-320`). But disclosure does not make the
default confined. A tool-classification defect, native plugin defect, browser defect, or future
guarded-IO mistake still has host-level consequences in the normal posture.

This is primarily a product/default decision, not a narrow code bug. For unattended operation,
`require` plus an outer VM/container remains necessary, especially on Windows.

### 2 — MEDIUM · REST SSE work outlives disconnects and can buffer without bound

The REST route `GET /sessions/{id}/stream` creates an **unbounded** MPSC channel and detaches the turn
with `tokio::spawn` (`crates/flux-server/src/lib.rs:1229-1244`). It passes a fresh
`CancellationToken` directly into the turn at `crates/flux-server/src/lib.rs:1245-1253`, but retains no
handle and installs no drop guard. If the HTTP client disconnects, the response receiver disappears
and sends fail, but the model/tool turn continues. If a connected client stops draining while the
response body remains live, the unbounded channel can retain one event per emitted delta/tool event.

The repository already contains the correct shape in the newer A2A streaming path: a bounded channel,
backpressure semantics, and a drop guard that cancels the turn when the stream is dropped
(`crates/flux-server/src/a2a.rs:1701-1710`). Its registry also caps A2A in-flight tasks per realm
(`crates/flux-server/src/a2a.rs:234-260`, `:434-445`). The REST route does not reuse either control.

Impact is bounded by authentication and by the engine's own turn serialization, but it is material for
an auto-approving service: disconnected clients can leave provider spend and approved tool activity
running, while slow readers can consume memory. The server test suite covers the REST stream's timeout
exemption (`crates/flux-server/src/lib.rs:1853-1870`) but has no equivalent disconnect-cancels-work or
stalled-consumer bound for this route.

### 3 — MEDIUM · The daemon has no general request rate limit or concurrency budget

The server's own resource-limit documentation explicitly leaves rate limiting out of scope
(`crates/flux-server/src/lib.rs:577-584`). The single-agent router mounts authenticated REST, webhook,
and A2A work at `crates/flux-server/src/lib.rs:723-758` with body and timeout layers, but no rate or
concurrency layer. A2A's per-realm 64-task cap is useful and specific; it does not cover REST session
creation, blocking message turns, webhooks, authentication attempts, or long-lived REST SSE requests.

This is not an authentication failure. It is an abuse-control and cost-containment gap after a token
is stolen, shared too broadly, or legitimately held by a noisy tenant. Body limits prevent one large
allocation and timeouts prevent an individual blocking request from hanging forever; neither limits
request arrival rate, queued work, provider spend, or auth-backend load. Internet-facing deployments
need a reverse proxy/API gateway with per-principal quotas, connection caps, and rate limits.

### 4 — MEDIUM · Core release authenticity is same-origin checksums, not signatures or attestations

The core release verifier requires `sha256.sum`, installer scripts, and platform archives
(`scripts/verify-github-release.sh:67-105`). The release workflow uploads those artifacts and creates
or refreshes the GitHub Release (`.github/workflows/release.yml:440-489`), but contains no core-binary
Minisign/Cosign/Sigstore step, SBOM attestation, or SLSA provenance publication. Repository searches
for `minisign|cosign|sigstore|slsa|attest|sbom` returned no hit in the core release workflow, verifier,
or `dist-workspace.toml`; the plugin release is the separate signed exception.

Checksums are useful against accidental corruption but do not add an independent authenticity root
when the binaries, hash file, and one-line installer arrive from the same compromised release origin.
The README's primary install path executes that remote script directly on Unix and Windows
(`README.md:54-66`), before a user can separately inspect or verify an artifact. SHA-pinned actions and
exact-SHA candidate receipts reduce producer-side risk, but they are not consumer-verifiable release
signatures or provenance.

This is a supply-chain assurance gap, not evidence that a release is compromised. Until signing is
available, sensitive users should build from a reviewed tag/commit or download a version-pinned
archive and verify its checksum out of band rather than pipe the latest installer into a shell.

### 5 — LOW · Security automation still lacks adversarial execution lanes

The main CI gate is format, Clippy, build, tests, layering, and hermetic smoke shape
(`.github/workflows/ci.yml:14-66`). The security workflow adds strong dependency, license, and source
policy (`.github/workflows/security-audit.yml:31-88`). Searches across workflows and the repository
found no cargo-fuzz/libFuzzer harness directory, Miri lane, sanitizer lane, CodeQL/other SAST workflow,
or release-oriented SBOM/provenance generator.

This is not a current vulnerability and is correctly low severity. It matters because the most exposed
surfaces are parser/state-machine heavy: provider streaming codecs, Flux-Lang parsing, JSON-RPC/A2A,
plugin framing, SQL admission, URL normalization, and archive extraction. The existing hand-authored
tests are extensive, but their authors choose the inputs. Fuzz/property targets for the highest-risk
pure parsers and one sanitizer/Miri lane would increase confidence without changing product defaults.

### 6 — LOW · The direct-IO gate is a lexical tripwire, not complete enforcement

CI accurately labels the job as grep-only at `.github/workflows/ci.yml:141-155`. The scanner scopes
itself to three crates (`scripts/check-no-direct-io.sh:56-57`) and matches a fixed textual pattern at
`scripts/check-no-direct-io.sh:145-155`. That pattern catches `std::fs::`, raw `Command`, selected
`Connection::open`, and selected stream-connect spellings, but it cannot resolve imports or types. For
example, `use std::fs; fs::write(...)`, `std::fs::File::create(...)` expressed through an imported
`File`, an aliased database constructor, or a newly introduced socket/client API is outside the
pattern even though it can violate the same invariant.

The scanner and its self-test passed over all 43 currently scoped source files, and this review found
no current unannotated bypass in those crates. The finding is about assurance strength: the gate makes
known risky spellings visible but must not be cited as proof that all model-facing IO is structurally
impossible. An AST/HIR-aware lint or a more restrictive dependency/interface design would carry that
claim more honestly.

## Open questions

- **Real sandbox behavior on supported hosts.** I did not exercise Bubblewrap or Seatbelt against a
  hostile process tree, mount topology, writable-root edge case, or network namespace. Existing tests
  and source were reviewed, but actual kernel/policy behavior remains unverified here.

- **Windows containment.** Source confirms there is no backend. I did not test whether the outer
  process cleanup and environment controls behave consistently on Windows under child/grandchild
  process trees.

- **Live provider and malformed-stream resilience.** The root suite exercises mock and codec fixtures;
  this review made no billable provider call and did not replay a live malformed SSE response.

- **Multi-replica production behavior.** PostgreSQL paths, principal introspection under outage/load,
  cross-replica A2A task projection, and ingress proxy interactions were not tested against deployed
  services.

- **External release state and audit history.** `v0.37.0` is the latest local repository tag and its
  tag target was inspected, but GitHub publication, asset bytes, organizational branch protection,
  secret management, and any non-public third-party audit were outside this no-network pass.

- **Community continuity.** The bus-factor rating is based on local Git history, not an investigation
  of maintainership agreements, funding, succession plans, private users, or organizational support.

## Method limitations and checks run

This was a desk review with non-destructive execution of existing checks, not a penetration test. I
did not weaken guards, add malicious fixtures to the tree, attack live infrastructure, benchmark
under load, inspect runtime secrets, or test undocumented consumer internals.

Checks completed successfully at `cb3bb057c961db70769330b375299e09a2fabfcb`:

- `cargo test --workspace`
- `cargo test --workspace --manifest-path plugins/Cargo.toml`
- `cargo test -p flux-server`
- `cargo test -p flux-codegate`
- `./scripts/check-no-direct-io.sh --self-test`
- `./scripts/check-no-direct-io.sh`
- `./scripts/check-action-pins.sh --self-test`
- `./scripts/check-action-pins.sh`

The root server suite passed with one pre-existing ignored JSON-RPC parse-envelope conformance test;
the ignored case is documented at `crates/flux-server/tests/malformed_json_rpc.rs:86-90` and still
returns a generic HTTP 400 rather than JSON-RPC `-32700`. I treat that as protocol polish, not a
security finding, because the companion test proves malformed input does not panic or return 500.

Not run: `cargo build --workspace`, Clippy, rustfmt, `cargo-audit`, `cargo-deny`, live smoke tests,
platform matrix builds, browser automation, fuzzers, sanitizers, Miri, load tests, or release
publication verification. The two workspace test commands compile their test targets but are not a
substitute for the omitted gates.

## Deployment recommendation

**Controlled internal pilot: yes, with external containment.** Use a dedicated unprivileged VM or
container, a disposable checkout, short-lived narrowly scoped credentials, and
`FLUX_SANDBOX=require`. Disable sandbox network by default and open it only for operations that need
it. Verify at startup that the requested backend is actually active; do not accept `on` degradation.

**Unattended work on valuable repositories: conditional, not as a standalone boundary.** Keep the
workspace and credentials isolated from a developer home directory, SSH agent, browser profile, and
production control plane. Start with plugins/hooks disabled; if plugins are needed, use only the
signed pack and avoid `plugin install --git`/unverified local binaries in production. Prefer the A2A
streaming surface over the REST SSE route until disconnect cancellation and bounded buffering match.

**Internet-facing service: not directly.** Put the daemon behind an authenticating reverse proxy/API
gateway that terminates TLS, fixes the advertised external URL, rate-limits by principal/realm,
limits concurrent and long-lived connections, and enforces spend quotas. Keep Flux's own bind on
loopback or a private service network. Treat bearer possession as high privilege because the HTTP
surface is intentionally assembled with auto-approval.

**Release consumption:** avoid `curl | sh` / `irm | iex` for sensitive environments. Pin the version,
download separately, verify the published SHA-256 through an independently obtained channel where
possible, or build the reviewed tag/commit. Revisit this recommendation when core artifacts gain a
detached signature and consumer-verifiable provenance.

## Bottom line

Compared with the 6/10 baseline, Flux now has evidence that maintainers respond to adversarial
findings with durable gates rather than local patches. That moves it to **7/10** and makes isolated
pilots reasonable. The architecture is ahead of the assurance and operational defaults: sandboxing
is still optional, one server stream lifecycle is unsafe under disconnect/backpressure, ingress abuse
controls are delegated, and the core release channel lacks an independent authenticity root.

The concise deployment verdict is therefore: **materially hardened security-engineered beta;
suitable for isolated pilots, but not yet a self-sufficient trusted boundary for unattended valuable
workloads.**
