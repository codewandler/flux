---
id: C-510
title: "Install and supervise a verified local Exchange release"
pillar: Core
status: in-progress
priority: 0
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Milestone 1 runtime prerequisite: a signed monotonic Exchange channel, atomic verified cache and same-binary authenticated supervisor — never PATH, PID signalling or an unsigned fallback"
---

# Install and supervise a verified local Exchange release

## Goal

Make `flux exchange local start|status|stop` a trustworthy clean-machine lifecycle for the newest
compatible Exchange build in the separately signed stable channel. A hidden instance of the same shipped
Flux binary supervises the Exchange child for its entire lifetime; later commands authenticate to
that supervisor instead of rediscovering or signalling a process. Flux remains a client and process
owner, never an Exchange runtime, binary distributor or credential holder.

## Acceptance

### One provider-owned channel and one trust contract

- [ ] Flux begins from exactly one authenticated `ExchangeChannelPolicy` bootstrap contract, either
      embedded in the shipped Flux build or supplied through explicitly user-administered trust
      configuration. It contains only the initial minisign trust anchors/threshold, X-126's exact
      stable-channel request origin and redirect/transport policy, the provider-owned trust/channel/
      manifest/compatibility/readiness schema ids, and the provider protocol ids/versions this client
      supports. It embeds no routine online signer and never pins or selects an Exchange tag, semver,
      source commit, build id, manifest/executable digest or connector version. Ordinary runtime
      configuration, environment, project files, model input and an unsigned `latest` lookup cannot
      replace or widen this bootstrap policy.
- [ ] The compiled protocol set consumes X-126's exact six provider values:
      `exchange_api=exchange.api.v1`,
      `effective_catalogue_response=exchange.effective-catalogue-response.v1`,
      `invoke_request=exchange.invoke-request.v1`,
      `invoke_response=exchange.invoke-response.v1`,
      `connection_plan=exchange.connection-plan.v1` and
      `supervisor=exchange.supervisor-ready.v1`. X-125/X-129's provider fixtures, not a package
      version or local alias, prove those wire identities. Channel selection reads these values from
      the signed channel entry; those protocol values in the selected manifest, compatibility output
      and readiness record must later agree exactly.
- [ ] Exchange X-126/X-128 own the exact canonical v1 schemas and positive/adversarial fixtures:
      `exchange.release-trust.v1` (at most 64 KiB, 1..=4 keys per delegated role),
      `exchange.release-channel.v1` (at most 256 KiB and 1..=128 release entries),
      `exchange.release-manifest.v1` (at most 256 KiB, exactly five target assets and the declared
      16-member/256 MiB-member/512 MiB-expanded bounds), `exchange.compatibility.v1`, and
      `exchange.supervisor-ready.v1` (at most 16 KiB). Flux consumes their identifiers, fields,
      RFC 8785 byte canonicalization, bounds and fixture bytes verbatim. It does not publish local
      aliases, normalize provider field names or maintain a second independently shaped v1. A
      provider fixture change without the matching supported schema id makes the cross-repository
      conformance test fail before implementation.
- [ ] Flux applies X-126's canonical numeric and identifier domains verbatim: every JSON integer is
      at most `9007199254740991`, full-width native values use the canonical bounded decimal-string
      encoding, and key ids, protocol ids, stable SemVer/tags and derived basenames satisfy the
      provider ASCII grammars before path construction or selection. Minisign keys must be canonical
      56-character base64 42-byte `Ed` packets and signatures use the prehashed `ED` algorithm;
      malformed packets, embedded-id disagreement or repeated Ed25519 material under another id,
      role or offline/online root refuses. Flux consumes these provider rules and fixtures rather
      than defining another grammar or integer mapping.
- [ ] Fetches begin only from X-126's fixed HTTPS requests: trust at
      `https://github.com/codewandler/flux-exchange/releases/download/exchange-trust-v1/flux-exchange-release-trust.json`,
      channel at
      `https://github.com/codewandler/flux-exchange/releases/download/exchange-stable-v1/flux-exchange-release-channel.json`,
      and selected immutable release inputs at
      `https://github.com/codewandler/flux-exchange/releases/download/vX.Y.Z/<signed-basename>`.
      Metadata signatures use the same directory and exact derived basename. Source URLs have the
      exact scheme, default port, host, repository/path and no userinfo, fragment or
      caller-controlled component. Flux ignores proxy environment/config and sends no credential,
      cookie or proxy authorization.
- [ ] Because real GitHub Release assets return a redirect, the fetcher permits exactly one response
      accepted by X-126's transport fixture: `302 Found`, with an absolute HTTPS
      `Location` with default port, no userinfo/fragment, host exactly
      `release-assets.githubusercontent.com`, and length at most 8192 bytes. Its ASCII path must match
      `/github-production-release-asset/[1-9][0-9]{0,19}/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}`;
      its query is at most 6144 bytes, has no duplicate or percent-encoded names, and has non-empty,
      at-most-2048-byte valid percent-encoded values with no decoded control character. The only
      names admitted are `jwt`, `response-content-disposition`,
      `response-content-type`, `rscd`, `rsct`, `se`, `sig`, `ske`, `skoid`, `sks`, `skt`, `sktid`,
      `skv`, `sp`, `spr`, `sr` and `sv`. The follow-up request is newly constructed and forwards no
      cookie, authorization, proxy authorization or other credential-bearing header. A relative,
      oversized or malformed location, different host/scheme/port, disallowed source path, missing
      location, second redirect or final status other than `200` refuses. The transient CDN URL is
      never logged or persisted. Redirect transport never establishes identity: root/role signatures,
      signed origin/tag/basename, SHA-256 and response byte bounds still decide acceptance.
- [ ] Flux first verifies current root-threshold signatures over X-126's trust metadata, then its
      monotonic version, time and delegated channel/release role thresholds, validity intervals and
      key usage. Document issuance admits only the provider's overflow-checked five-minute future
      skew; delegated keys use exactly `not_before <= now < not_after` with no skew, and equality with
      any expiry is expired. It refreshes trust before rejecting an otherwise unknown online signer.
      Unknown root/role, role confusion, threshold failure, not-yet-valid/expired metadata or key,
      lower version, or changed bytes at one version refuses before the channel is read. Production
      root/trust policy has only an injected `cfg(test)` seam.
- [ ] Rollback floors advance by authenticated metadata layer, never by successful installation.
      After a higher trust document passes its own signature/canonical/time/key checks, Flux fsyncs
      and atomically replaces the global trust `{version,sha256}` before reading a channel. After a
      higher threshold-valid canonical/current stable channel passes the one global generation/
      equivocation check, Flux fsyncs and atomically replaces the complete trust/channel tuple before
      compatibility selection or any manifest/target fetch. Trust/signer rotation never creates a
      new generation namespace. A crash exposes the complete old or new tuple, never mixed fields.
- [ ] Only after that channel floor is durable does Flux choose the greatest stable SemVer whose six
      signed channel-entry protocol identities are supported. It skips a newer incompatible entry;
      a valid higher channel with no compatible release reports `incompatible` after advancing the
      floor, retains but does not launch the old install, and a later lower generation refuses.
      Manifest/signature/network/archive/compatibility failure likewise retains the prior install
      byte-for-byte while preserving the advanced floors and never falls back to an older channel
      entry for a new start. The selected manifest must match the channel identity/digest and current
      delegated release role. Package version, install time, `latest`, `PATH` and sibling checkouts
      never substitute for compatibility.
- [ ] There is no production reset/downgrade/ignore-expiry switch. A stopped/new `start`, import,
      reinstall and final target commit read the injected clock after bounded input and require both
      trust and channel to satisfy `now < expires_at`; expiry during download leaves advanced floors,
      removes staging and starts nothing. Cached metadata is usable without network only while
      current; otherwise fresh online metadata or a fully verified unexpired offline set is required.
      A verified healthy child accepted before expiry is not revoked: local `status` remains
      `healthy` with the corresponding trust/channel-expired diagnostic, repeated `start` returns the
      same child without replacement, and `stop` works. Once stopped, expired metadata cannot launch
      it again.
- [ ] Routine delegated signer rotation follows X-126's root-threshold overlap fixtures and requires
      no Flux release. Compatible Exchange releases, including releases carrying new connector
      catalogues, likewise require no Flux release. Only a root/trust-policy change not already
      admitted by the authenticated bootstrap policy, or an unsupported schema/protocol/client change,
      requires Flux to change.
- [ ] The selected manifest's exact release identity/digests become installed audit, cache-validation
      and process-ownership metadata, never compiled compatibility policy. Flux admits only its exact
      target from X-126's five-platform closed set, whose publication already requires X-127's native
      owner-only persistence/restart proof. Extraction and side-effect-free `compatibility --json`
      use the provider manifest fixtures and refuse every undeclared member, path trick, size/digest/
      identity mismatch or protocol outside the compiled policy before execution. Provider v1
      provenance is deliberately publication-only: it is absent from the manifest, downloader,
      cache identity and client trust path.

### Atomic cache, offline import and quarantine

- [ ] A verified executable is installed into a versioned, owner-only Flux cache under a
      per-release lock. Download/import, bounded extraction, compatibility execution and all digest
      checks happen in a newly created owner-only staging directory on the same filesystem; the
      complete directory becomes visible only by atomic rename. Directory/file permissions and
      no-follow ownership checks are revalidated at every cache hit. Concurrent, interrupted,
      partial and repeated installs never expose a half-installed or permission-widened executable.
- [ ] With no live supervisor, `start` resolves the current channel and atomically installs the
      selected release when it differs from the audited install. With a healthy supervisor, repeated
      `start` is idempotent—even when update metadata has since expired—and never hot-swaps or creates
      a second child; the next stop/start (or an explicit stopped `reinstall`) requires current
      metadata and selects the then-current newest compatible release. A channel update or expiry
      does not silently rewrite or revoke the exact identity attached to an already-owned process.
- [ ] Flux alone owns offline installation through
      `flux exchange local import --trust <path> --root-signature <path>... --channel <path> --channel-signature <path>... --manifest <path> --release-signature <path>... --archive <path>`.
      Signature options are repeatable; each set must use only keys named for that role and satisfy
      the provider fixture's threshold. Exchange
      has no importer/downloader and `start` has no artifact-path or URL option. Import performs the
      identical offline-root trust, role/time validity, channel authenticity, expiry,
      global floor transaction, newest-compatible selection,
      manifest/signature, bounds, platform, archive, executable and compatibility checks as network
      installation. Production has no unsigned, skip-verification, alternate-key or
      allow-incompatible override and never searches `PATH`, a sibling checkout, a Cargo target
      directory or an operator-selected executable. Offline import requires the root-signed trust
      metadata, delegated signed channel, selected signed manifest and archive as one closed set; it
      has no provenance option, reduced offline asset set or direct-manifest shortcut.
- [ ] A failed candidate is removed from staging and cannot disturb a currently verified install.
      If a previously visible install fails cache-hit revalidation, Flux atomically moves that whole
      directory to an owner-only, non-executable `quarantine/<release>/<incident-id>` and returns
      `install_verification_refused`; it never executes, repairs or falls back to quarantined bytes.
      Quarantine holds at most one bounded install for each exact installed release, replacing the older one
      without ever making it executable. The same invocation does not hide the incident by
      redownloading. Recovery is explicit: with the supervisor stopped,
      `flux exchange local reinstall` resolves the current newest compatible stable-channel release,
      or `import` supplies the identical signed channel/release set offline,
      and atomically publishes it only after all checks pass. Neither command implicitly stops a live
      instance or deletes a known-good install before its replacement is verified.

### Same-binary supervision, authenticated control and child identity

- [ ] `start` launches a hidden, non-model-reachable supervisor mode through the absolute path of the
      currently running shipped Flux binary. The short-lived command transfers lifecycle state over
      inherited handles, never argv/environment/stdin/stdout, and exits only after the supervisor has
      accepted ownership or returned a typed refusal. The supervisor owns the verified Exchange child
      handle for the child's entire lifetime and supplies X-128's separate inherited liveness
      capability. It binds Exchange to an OS-selected loopback port; no caller chooses a bind address
      or port.
- [ ] The supervisor is a reviewed trusted-service sandbox exception. It and the Exchange service do
      not inherit the ordinary tool sandbox when its wrapper has die-with-parent semantics, but still
      use argv-only, env-cleared, absolute-executable, bounded-output guarded process construction.
      Implementation adds one named product seam to `Confinement::Exempt`'s exhaustive
      source-derived inventory and its bidirectional test, plus tests proving the exception is
      reachable only from the host-owned lifecycle command and that deleting either the inventory
      entry or actual seam fails. No generic public "unsandboxed daemon" primitive is introduced.
- [ ] The supervisor exposes only a length-framed local control protocol capped at 16 KiB per request
      and response with a two-second deadline. Unix uses a socket inside a `0700` owner directory
      with a `0600` socket/state file; Windows uses a named pipe whose ACL admits only the current
      user SID and LocalSystem. Each instance has a CSPRNG 256-bit control credential transferred to
      the supervisor by inherited handle and persisted only in owner-only lifecycle state. Every
      request authenticates before parsing an operation. The control endpoint/credential never
      appears in argv, environment, logs, JSON, model-visible output or Exchange configuration.
- [ ] Later `start`, `status` and `stop` calls use that authenticated channel. They never send a
      signal, open a process handle for termination or make an ownership decision from a recorded
      PID. `stop` asks the live authenticated supervisor to terminate and wait on the child handle it
      owns by closing its liveness writer and waiting. A missing/wrong credential, stale state, reused
      PID, foreign listener, wrong Flux build or mismatched supervisor instance returns
      `foreign_or_stale`; it never kills anything. Repeated start/stop is idempotent and a second
      Exchange child is never silently created.
- [ ] Flux consumes X-128's exact inherited-handle ABI. On Linux/macOS it launches only
      `flux-exchange --supervised`, duplicates the readiness pipe write end to inherited write-only
      FD 3 and liveness pipe read end to inherited read-only FD 4, closes every other nonstandard
      child descriptor, and retains only readiness-read/liveness-write parent ends. On Windows it
      passes only `--supervised --supervisor-readiness-handle <H>
      --supervisor-liveness-handle <H>` with distinct nonzero canonical decimal `usize` HANDLEs and
      admits exactly those pipe handles through `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`. No FD/HANDLE is
      discovered from environment, stdin/stdout or a reserved `STARTUPINFO` field; the numeric
      Windows capabilities are the provider-authorized non-secret argv exception.
- [ ] The supervisor never writes liveness payload. Normal stop/exit closes its writer; `SIGKILL` on
      Linux/macOS or `TerminateProcess` on Windows also closes it. X-128's native thread treats EOF,
      any byte or read error as immediate non-unwinding Exchange exit even when the async runtime is
      wedged. Normal stop waits for the owned process; provider-native forced-death fixtures prove
      bounded process/port disappearance when the supervisor itself cannot wait. The child receives
      no liveness writer, and provider-cleared inheritance prevents readiness/liveness capabilities
      from reaching connector children.
- [ ] Exchange X-128 supplies exactly one readiness record on a dedicated inherited one-shot
      pipe/handle after the listener has bound and before it accepts lifecycle ownership. The record
      is provider-fixture-valid `exchange.supervisor-ready.v1`, at most 16 KiB and followed by EOF.
      Flux commits ownership only when it arrives within ten seconds, is the sole frame, the provider
      bind field is loopback and matches the actual listener, the provider process-start identity
      matches the already-open child handle through the same provider-native source, and every
      provider release/protocol field matches the selected signed channel entry, manifest,
      compatibility output and compiled policy. The accepted closed tags are Linux
      `linux-proc-start` (`boot_id` plus decimal `/proc/<pid>/stat` field-22 ticks), macOS
      `macos-proc-start` (`proc_pidinfo(PROC_PIDTBSDINFO)` seconds/microseconds), and Windows
      `windows-process-creation` (decimal `GetProcessTimes` creation FILETIME), with the exact
      provider shapes/domains and target match; PID alone never proves identity.
      `/health`, a stdout marker, application logs
      or a listener already occupying the port proves none of those facts and cannot substitute.

### Stable status, exits and value-free diagnostics

- [ ] `flux exchange local status --json` emits exactly one object and no progress/prompt with schema
      `flux.exchange-local-status.v1`:

      ```json
      {
        "schema": "flux.exchange-local-status.v1",
        "state": "not_installed|install_verification_refused|stopped|starting|healthy|incompatible|unhealthy|foreign_or_stale|stop_failure",
        "channel": null,
        "release": null,
        "endpoint": null,
        "diagnostics": [
          { "component": "install|supervisor|control|exchange", "code": "<closed code>" }
        ]
      }
      ```

      `channel`, when accepted, is exactly
      `{name,trust_version,trust_sha256,generation,index_sha256,expires_at}` (`name` is
      `stable`, generations are JSON integers and the rest strings). `release`, when installed, is
      exactly `{tag,version,source_commit,build_id,target,manifest_sha256,executable_sha256}` with
      string values and is audit/ownership identity, not Flux policy; `endpoint`, only after verified
      readiness, is exactly `{scheme,host,port}`. The closed v1 status diagnostic codes are
      `trust_invalid`, `trust_expired`, `trust_rollback`, `channel_invalid`,
      `channel_expired`, `channel_rollback`, `manifest_missing`, `signature_invalid`,
      `signing_key_unknown`, `origin_refused`, `archive_invalid`,
      `executable_invalid`, `cache_permissions`, `control_unavailable`, `control_auth_failed`,
      `supervisor_mismatch`, `readiness_timeout`, `readiness_invalid`, `bind_mismatch`, `child_exited`,
      `health_failed`, `protocol_incompatible`, `terminate_failed` and `diagnostics_truncated`; a
      component/code combination outside the corresponding typed enum cannot be emitted. No
      other/null-omitted variant is accepted. The exhaustive status exit table
      is: `healthy=0`, `not_installed=20`, `stopped=21`, `starting=22`,
      `install_verification_refused=23`, `incompatible=24`, `unhealthy=25`,
      `foreign_or_stale=26`, `stop_failure=27`; CLI usage is `64` and an internal failure that occurs
      before a status can be classified is `70`. `start`/`stop` return `0` when their requested final
      state (including an idempotent already-started/stopped state) is reached and otherwise use the
      reported status code. Human rendering is derived from the same typed result.
- [ ] Status time semantics consume the provider boundary fixture exactly. At
      `now == trust.expires_at` or `now == channel.expires_at` with no live child, the corresponding
      metadata is expired and start/import/reinstall refuses. With an already-owned child whose
      readiness was accepted while current, `status` stays `healthy` and adds the corresponding
      `trust_expired` and/or `channel_expired` diagnostic; same-child `start` and authenticated
      `stop` still succeed. Status never fetches metadata or turns expiry into remote process
      revocation.
- [ ] Readiness and control never share a stream with child output. The supervisor drains child
      stdout/stderr without retaining arbitrary bytes, so neither a full pipe nor a log flood can
      deadlock or grow lifecycle state; it derives diagnostics only from typed verifier, control,
      readiness, health and exit outcomes. Status returns at most eight de-duplicated
      `{component,code}` pairs from the closed enum above and serializes no free-form child message,
      path, address, argument, context map or value. Lifecycle state and diagnostics contain no vendor
      credential, Service Account token, release-fetch credential, control credential, secret-shaped
      input or raw child output. A sentinel corpus covers successful startup and every child refusal,
      and the JSON/diagnostic byte caps are tested before allocation.

### Failing-first proof and release boundary

- [ ] Flux runs X-126/X-128's provider-owned positive and adversarial conformance fixtures verbatim,
      including their test root/delegated keys. A checked fixture inventory/digest fails if a local
      copy is hand-edited, omitted or locally reinterpreted under another v1 shape. Those fixtures plus
      Flux lifecycle cases cover valid network install, valid offline import and cache reuse, then
      root/trust/channel/manifest/signature/key-id/role/threshold tampering, expired or overlong
      validity, future issuance, trust-version/channel-generation rollback, same-version/generation
      digest drift, redirect confusion, mutable/wrong origin, oversized/slow bodies, target/asset-set
      drift, archive bombs/path tricks, archive/executable digest mismatch, compatibility mismatch,
      widened cache permissions, concurrent/interrupted installs, quarantine and explicit reinstall.
      It includes the provider's JCS integer/decimal/grammar/key-material cases, higher-channel with
      no compatible release, every later target failure, stopped/live expiry equality, all three
      process-start tags, Unix FD/Windows HANDLE refusals, forced supervisor death with responsive and
      wedged children, and rejection of provenance as client input. Mutation proves removal of
      signature, executable or cache-hit revalidation makes a test fail. A release-cadence test adds
      a greater compatible Exchange version to a higher signed channel generation and proves an
      unchanged Flux binary selects it; a signer-cadence test rotates both delegated roles through a
      root-signed overlap and proves the unchanged binary accepts the new-only successor; an
      incompatible greater version is skipped without weakening protocol checks.
- [ ] Process tests use real OS processes and cover authenticated start/status/stop, two racing starts,
      wrong control credential, stale metadata, PID reuse/foreign listener, supervisor crash, child
      crash, readiness timeout/second frame/wrong bind/wrong start identity/wrong build, diagnostic
      flooding and every JSON state/exit pair. They prove later commands never exercise a PID-signal
      seam, supervisor death leaves no Exchange descendant, and a changed exemption/readiness/status
      field fails from both producer and consumer sides.
- [ ] The managed executable remains a separately downloaded Exchange artifact, never an official
      integration plugin, connector runtime, crates.io artifact or binary copied into Flux's release.
      The two products remain an HTTP process boundary and may use different Rust engine dependency
      lines; compatibility comes only from the signed channel/manifests and versioned protocols.
      Exchange and connector releases may advance independently without rebuilding or releasing Flux
      whenever those schemas/protocols remain supported; Flux embeds no connector version or artifact
      identity.

## Progress

- 2026-08-04: Contract repaired against flux-roadmap Decision 0004's accepted supervision/readiness
  boundary and the upcoming Exchange X-127/X-128 platform/readiness contracts; no implementation has
  started.
- 2026-08-04: Architecture correction replaced the exact-Exchange-release pin with a signed,
  expiry-bounded, monotonic stable channel. Flux now uses only an authenticated bootstrap policy—
  embedded or explicitly administrator-supplied—for initial trust anchors, stable-channel origin/
  transport and supported schema/protocol identities; installed version and digests are audit/
  ownership facts rather than a release-cadence coupling.
- 2026-08-04: Cross-repository reconciliation removed Flux-owned wire shapes and made X-126/X-128's
  canonical schemas, redirect contract and conformance fixtures the single provider-owned source.
- 2026-08-04: Implementation audit aligned the consumer to Exchange provider commit
  `4ade23df62ce9fa8de39e9083ca5e0c98502d838`: actual protocol ids, JCS-safe domains, global
  transactional rollback floors, provenance-free client inputs, native readiness/liveness ABI and
  live-child expiry semantics now precede implementation.

## Notes

- Cross-repository authority:
  `../flux-roadmap/decisions/0004-flux-manages-a-verified-local-exchange.md` at coordinator commit
  `013a2ab0e1632e67f272e0903cde30dfc1c85db8`.
- Direct dependencies are Exchange X-126 (root-signed trust metadata, signed monotonic channel and
  immutable release manifests/artifacts) and X-128
  (the one-shot readiness record/schema/fixtures). X-127 is required transitively and
  observably: X-126 may publish a target only after X-127's native owner-only persistence/restart
  gate, and C-510 accepts only that signed released-platform set. C-509 consumes C-510 only after
  those Exchange contracts and C-510 itself are released. X-125 and X-129 are X-126 prerequisites
  that bind the exact six protocol ids to provider wire fixtures; C-510 consumes those identities
  but does not redefine them.
- The hidden supervisor is same-binary Flux control-plane code, not a daemon found on `PATH`. The
  Exchange child is still a separately released product and owns all credential-bearing surfaces.
