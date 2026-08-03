---
id: C-510
title: "Install and supervise a verified local Exchange release"
pillar: Core
status: ready
priority: 0
epic: connector-native-integrations
design: docs/designs/ecosystem.md
note: "Milestone 1 runtime prerequisite: a pinned Exchange release, atomic verified cache and same-binary authenticated supervisor — never PATH, PID signalling or an unsigned fallback"
---

# Install and supervise a verified local Exchange release

## Goal

Make `flux exchange local start|status|stop` a trustworthy clean-machine lifecycle for the exact
separately released Exchange build this Flux release supports. A hidden instance of the same shipped
Flux binary supervises the Exchange child for its entire lifetime; later commands authenticate to
that supervisor instead of rediscovering or signalling a process. Flux remains a client and process
owner, never an Exchange runtime, binary distributor or credential holder.

## Acceptance

### One pinned release and one trust contract

- [ ] The Flux build pins one `ExchangeReleasePin`: exact release tag and semver, 40-lowercase-hex
      source commit, build id, manifest SHA-256, ordered signing-key ids, and the exact accepted
      values for Exchange API, effective-catalogue, invoke, `exchange.connection-plan` and readiness-schema
      protocols. The pin is compiled into Flux and cannot be changed by configuration, environment,
      project files, model input or an unversioned/latest lookup. Every `start`, including a cache
      hit, revalidates the manifest, installed bytes, compatibility JSON and pin before execution.
- [ ] C-510 consumes the following canonical UTF-8 JSON contract from Exchange X-126. Unknown or
      duplicate fields, non-canonical encodings, numbers outside their declared integer domain and a
      manifest not byte-identical to its RFC 8785 serialization refuse. The manifest is at most
      256 KiB and has exactly this v1 shape (pretty-printed here for review; every field is required,
      `signing_key_ids` and `assets` are sorted):

      ```json
      {
        "schema": "exchange.release-manifest.v1",
        "origin": "https://github.com/codewandler/flux-exchange",
        "tag": "refs/tags/vX.Y.Z",
        "version": "X.Y.Z",
        "source_commit": "<40 lowercase hex>",
        "build_id": "<1..128 printable ASCII bytes>",
        "protocols": {
          "exchange_api": "<versioned id>",
          "effective_catalogue": "<versioned id>",
          "invoke": "<versioned id>",
          "connection_plan": "exchange.connection-plan.v1"
        },
        "readiness_schema": "exchange.supervisor-ready.v1",
        "signing_key_ids": ["flux-exchange-release-2026-01"],
        "assets": [
          {
            "target": "<closed supported target>",
            "archive": "<basename>",
            "format": "tar.zst|zip",
            "archive_bytes": 1,
            "archive_sha256": "<64 lowercase hex>",
            "executable": {
              "path": "<single-root relative path ending flux-exchange|flux-exchange.exe>",
              "bytes": 1,
              "sha256": "<64 lowercase hex>"
            },
            "other_members": [
              {
                "path": "<single-root relative path>",
                "kind": "documentation|license",
                "bytes": 1,
                "sha256": "<64 lowercase hex>"
              }
            ],
            "provenance": "<archive basename>.intoto.jsonl"
          }
        ]
      }
      ```

      There are exactly five target entries in X-126 v1. Integer domains are `1..=268435456` for an
      archive/member and at most `536870912` summed expanded bytes; `other_members` has zero to 15
      entries. Every `other_members` entry is an
      archive member; the executable plus that list is the complete member set. Each provenance name
      is a release basename, never a URL, and the live verifier proves its repository/workflow,
      immutable tag and source SHA before Flux admits the archive.
- [ ] Minisign over the canonical manifest is the sole v1 authenticity mechanism. For each id in
      `signing_key_ids`, the release contains exactly
      `flux-exchange-release-manifest.json.<key-id>.minisig`, at most 4 KiB, and Flux verifies it with
      the compile-time public key under the same id; provenance complements and never substitutes for
      that signature. `flux-exchange-release-2026-01` is the initial id. Rotation introduces exactly
      `flux-exchange-release-2026-02`: a Flux release first ships both public keys while its pin still
      accepts `...-01`; the transition Exchange manifest then declares the ordered two-id set and must
      carry valid signatures from both keys; a later Flux release pins that transition while trusting
      both; only then may Exchange publish a `...-02`-only release; another Flux release pins that
      new-only release while still trusting both; only a still-later Flux release may remove
      `...-01`. Missing one transition signature, an unknown id, id/signature
      disagreement, a signer switch without overlap or accepting any unlisted extra signature refuses.
      Production roots and enforcement have only an injected test-double seam under `cfg(test)`.
- [ ] Network installation derives every URL itself from the fixed origin
      `https://github.com/codewandler/flux-exchange/releases/download/vX.Y.Z/`; the signed manifest's
      `origin` must equal the compiled repository identity but carries no download URL. The client
      permits HTTPS on that exact host, repository and tag path, ignores proxy environment/config,
      sends no release credential, accepts no redirect, query or fragment,
      proxy-selected replacement or mutable release API response, and uses the guarded pinned-address
      HTTP seam. The manifest basename is exactly `flux-exchange-release-manifest.json`; signature,
      archive and provenance basenames must be the ones closed over by the manifest contract.
      Manifest/signature bodies are capped as above; an archive is capped at 256 MiB both
      by declared and received bytes with a 10-second connect and 120-second total deadline.
- [ ] The signed asset set is closed and equals Exchange X-126/X-127's released supported-platform
      set; Flux selects only its exact target. Extraction admits at most 16 regular-file members,
      240 UTF-8 bytes per relative path, 256 MiB per member and 512 MiB total expanded bytes. It
      rejects absolute/parent
      paths, links, devices, FIFOs, duplicate or case-colliding paths, trailing data, an undeclared
      member, more than one executable, the wrong executable basename, size/digest disagreement and
      bytes whose side-effect-free `compatibility --json` identity differs from the pin/manifest.
      A target entry itself is the signed released-platform claim and is accepted only from an X-126
      release whose native gate includes X-127's fail-closed owner-only restart proof for credentials,
      settings, grants, labelled connections and Service Accounts; an ad-hoc/cross-compiled asset or
      `/health` response cannot manufacture platform support.

### Atomic cache, offline import and quarantine

- [ ] A verified executable is installed into a versioned, owner-only Flux cache under a
      per-release lock. Download/import, bounded extraction, compatibility execution and all digest
      checks happen in a newly created owner-only staging directory on the same filesystem; the
      complete directory becomes visible only by atomic rename. Directory/file permissions and
      no-follow ownership checks are revalidated at every cache hit. Concurrent, interrupted,
      partial and repeated installs never expose a half-installed or permission-widened executable.
- [ ] Flux alone owns offline installation through
      `flux exchange local import --manifest <path> --signature <path>... --archive <path> --provenance <path>`
      (the signature option is repeatable and must supply exactly the ids the manifest declares);
      Exchange
      has no importer/downloader and `start` has no artifact-path or URL option. Import performs the
      identical pin, signer, canonical-schema, bounds, platform, archive, executable and compatibility
      checks as network installation. Production has no unsigned, skip-verification, alternate-key or
      allow-incompatible override and never searches `PATH`, a sibling checkout, a Cargo target
      directory or an operator-selected executable. Offline import accepts the manifest plus every
      minisign file its `signing_key_ids` requires, the selected archive and its provenance; it has no
      reduced offline asset set.
- [ ] A failed candidate is removed from staging and cannot disturb a currently verified install.
      If a previously visible install fails cache-hit revalidation, Flux atomically moves that whole
      directory to an owner-only, non-executable `quarantine/<release>/<incident-id>` and returns
      `install_verification_refused`; it never executes, repairs or falls back to quarantined bytes.
      Quarantine holds at most one bounded install for the pinned release, replacing the older one
      without ever making it executable. The same invocation does not hide the incident by
      redownloading. Recovery is explicit: with the supervisor stopped,
      `flux exchange local reinstall` fetches the one fixed release, or `import` supplies it offline,
      and atomically publishes it only after all checks pass. Neither command implicitly stops a live
      instance or deletes a known-good install before its replacement is verified.

### Same-binary supervision, authenticated control and child identity

- [ ] `start` launches a hidden, non-model-reachable supervisor mode through the absolute path of the
      currently running shipped Flux binary. The short-lived command transfers lifecycle state over
      inherited handles, never argv/environment/stdin/stdout, and exits only after the supervisor has
      accepted ownership or returned a typed refusal. The supervisor owns the verified Exchange child
      handle for the child's entire lifetime with kill-on-drop semantics: Unix uses a process-owned
      death/child-group mechanism, and Windows a kill-on-close Job Object, so every supervisor exit
      tears down that exact child and descendants. It binds Exchange to an OS-selected loopback port;
      no caller chooses a bind address or port.
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
      owns. A missing/wrong credential, stale state, reused PID, foreign listener, wrong Flux build or
      mismatched supervisor instance returns `foreign_or_stale`; it never kills anything. Repeated
      start/stop is idempotent and a second Exchange child is never silently created.
- [ ] Exchange X-128 supplies exactly one readiness record on a dedicated inherited one-shot
      pipe/handle after the listener has bound and before it accepts lifecycle ownership. The record
      is strict JSON, at most 16 KiB, followed by EOF, with this exact shape:

      ```json
      {
        "schema": "exchange.supervisor-ready.v1",
        "release": {
          "tag": "refs/tags/vX.Y.Z",
          "version": "X.Y.Z",
          "source_commit": "<40 lowercase hex>",
          "build_id": "<1..128 printable ASCII bytes>",
          "executable_sha256": "<64 lowercase hex>"
        },
        "protocols": {
          "exchange_api": "<versioned id>",
          "effective_catalogue": "<versioned id>",
          "invoke": "<versioned id>",
          "connection_plan": "exchange.connection-plan.v1"
        },
        "bind": { "scheme": "http", "host": "127.0.0.1|::1", "port": 1 },
        "process": { "pid": 1, "start_identity": "<OS process-start identity>" }
      }
      ```

      Flux commits ownership only when the record arrives within ten seconds, is the sole frame, the
      bind is loopback and matches the actual listener, the process id/start identity matches the
      child handle the supervisor created, and every release/protocol field matches the verified
      manifest, compatibility output and compiled pin. `/health`, a stdout marker, application logs
      or a listener already occupying the port proves none of those facts and cannot substitute.

### Stable status, exits and value-free diagnostics

- [ ] `flux exchange local status --json` emits exactly one object and no progress/prompt with schema
      `flux.exchange-local-status.v1`:

      ```json
      {
        "schema": "flux.exchange-local-status.v1",
        "state": "not_installed|install_verification_refused|stopped|starting|healthy|incompatible|unhealthy|foreign_or_stale|stop_failure",
        "release": null,
        "endpoint": null,
        "diagnostics": [
          { "component": "install|supervisor|control|exchange", "code": "<closed code>" }
        ]
      }
      ```

      `release`, when known, is exactly `{tag,version,source_commit,build_id,target,manifest_sha256,`
      `executable_sha256}` with string values; `endpoint`, only after verified readiness, is exactly
      `{scheme,host,port}`. The closed v1 status diagnostic codes are `manifest_missing`,
      `signature_invalid`, `signing_key_unknown`, `origin_refused`, `archive_invalid`,
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

- [ ] Hermetic fixtures use a test-only Ed25519 key and cover valid network install, valid offline
      import and cache reuse, then manifest/signature/key-id/dual-signature tampering, redirect,
      mutable/wrong origin,
      oversized/slow bodies, target/asset-set drift, archive bombs/path tricks, archive/executable
      digest mismatch, compatibility mismatch, widened cache permissions, concurrent/interrupted
      installs, quarantine and explicit reinstall. Mutation proves removal of signature, executable or
      cache-hit revalidation makes a test fail.
- [ ] Process tests use real OS processes and cover authenticated start/status/stop, two racing starts,
      wrong control credential, stale metadata, PID reuse/foreign listener, supervisor crash, child
      crash, readiness timeout/second frame/wrong bind/wrong start identity/wrong build, diagnostic
      flooding and every JSON state/exit pair. They prove later commands never exercise a PID-signal
      seam, supervisor death leaves no Exchange descendant, and a changed exemption/readiness/status
      field fails from both producer and consumer sides.
- [ ] The managed executable remains a separately downloaded Exchange artifact, never an official
      integration plugin, connector runtime, crates.io artifact or binary copied into Flux's release.
      The two products remain an HTTP process boundary and may use different Rust engine dependency
      lines; compatibility comes only from the pinned release and versioned protocols.

## Progress

- 2026-08-04: Contract repaired against flux-roadmap Decision 0004's accepted supervision/readiness
  boundary and the upcoming Exchange X-127/X-128 platform/readiness contracts; no implementation has
  started.

## Notes

- Cross-repository authority:
  `../flux-roadmap/decisions/0004-flux-manages-a-verified-local-exchange.md` at coordinator commit
  `71fea6c74be93851bd3ad4e095b432026bf8363d`.
- Direct dependencies are Exchange X-126 (signed immutable release manifest/artifacts) and X-128
  (compiled compatibility plus the one-shot readiness record). X-127 is required transitively and
  observably: X-126 may publish a target only after X-127's native owner-only persistence/restart
  gate, and C-510 accepts only that signed released-platform set. C-509 consumes C-510 only after
  those Exchange contracts and
  C-510 itself are released.
- The hidden supervisor is same-binary Flux control-plane code, not a daemon found on `PATH`. The
  Exchange child is still a separately released product and owns all credential-bearing surfaces.
