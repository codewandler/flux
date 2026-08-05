---
id: C-355
title: Bind artifact digests into the release-candidate receipt
pillar: Core
status: done
priority: 3
epic: release-trust-residuals
design: docs/designs/release-trust-residuals.md
note: "v0.56.0 blocker — receipt v3 binds the exact seven non-expired artifacts-* ZIPs by name, immutable ID, size and GitHub-reported sha256 digest before namespaced extraction or publication"
---

# Bind artifact digests into the release-candidate receipt

## Goal

Authenticate the build-to-publish handoff: candidate receipt v3 names every expected Actions
artifact and binds the immutable raw ZIP bytes that the publishing run must consume.

## Acceptance

- [x] `flux-release-candidate-v3` retains the exact version, lowercase 40-hex commit SHA and positive
      run ID. The SHA must be the resulting canonical `main` SHA returned after C-516's normal cut PR
      merges through its required `ci`; a local cut commit, PR head, release-branch head or direct
      push cannot become a candidate. The receipt contains exactly one canonical record for each of
      these seven non-expired uploads, with no extra `artifacts-*` upload:

      ```text
      artifacts-plan-dist-manifest
      artifacts-build-local-aarch64-apple-darwin
      artifacts-build-local-aarch64-unknown-linux-gnu
      artifacts-build-local-x86_64-apple-darwin
      artifacts-build-local-x86_64-unknown-linux-gnu
      artifacts-build-local-x86_64-pc-windows-msvc
      artifacts-build-global
      ```

- [x] Each record binds the API-reported artifact `name`, positive immutable database `id`, positive
      `size_in_bytes`, and exact `digest` spelling `sha256:<64 lowercase hex>`. IDs and
      names are unique; records use one deterministic order/encoding; missing, expired, duplicate,
      malformed or extra artifacts make recording and verification fail closed.
- [x] The narrow host-owned promotion job creates `release-candidates/<tag>` only after the cut PR
      has merged, verifies the ref equals the returned merged canonical-main SHA, and dispatches the
      candidate run from that ref. Receipt recording paginates and selects artifacts from that exact
      repository + run after all producer jobs are successful and uploads are final. It obtains
      identity/size/digest from GitHub's artifact API, not from an extracted file or
      producer-written checksum. Candidate discovery and consumers require v3; v2 is not accepted as
      a compatibility substitute.
- [x] Promotion downloads each archive by its receipt-bound immutable artifact ID. Before opening or
      extracting it, the consumer checks the raw response is a regular ZIP, hashes those exact raw
      bytes, compares `sha256:<lowerhex>` and API size/identity to the receipt, and refuses redirects
      or API metadata that resolve to a different artifact.
- [x] Each verified ZIP extracts into a fresh directory named only from its receipt record. Safe
      extraction rejects absolute paths, `..` traversal, backslashes/drive or UNC paths, NUL/control
      names, symlinks/hardlinks/devices/FIFOs, duplicate members and any cross-archive destination
      collision. Only after all seven namespaces verify may an allowlisted merger assemble the host
      input; `merge-multiple: true` is not the trust boundary.
- [x] Failing-first fixtures cover missing/extra/expired artifacts; duplicate name or ID; wrong
      run/name/ID/size; absent, uppercase or malformed digest; reordered/noncanonical receipt;
      byte-tampered raw ZIP; non-ZIP/truncated response; zip-slip, drive/UNC, symlink, special-file and
      duplicate-member entries; and cross-archive collisions. A success fixture contains the exact
      five-target + global + plan set and proves deterministic write/verify round-trip.
- [x] Workflow-order tests prove the merged-main-SHA check, v3 verification and safe extraction
      complete before tag creation, `dist host`, staged asset verification, attestation, GitHub
      publication or Cargo publication. A receipt or byte failure leaves the candidate ref available
      for diagnosis and cannot create a tag or Release. Updating an existing release tag is forbidden
      by C-353's no-bypass immutability ruleset, never a recovery option.
- [x] BUILD-ONCE publication documentation names receipt v3 and its raw-ZIP trust boundary. Focused
      script self-tests and the release policy gate run in CI.

## Progress

- 2026-08-05 — implemented. Failing-first: `scripts/test_candidate_artifacts.py` was written first
  and run against the pre-change tree, where it aborted with
  `FileNotFoundError: [Errno 2] No such file or directory: '.../scripts/candidate_artifacts.py'`
  while `scripts/release-candidate.sh write` emitted `schema=flux-release-candidate-v2` with no
  artifact bindings at all. `scripts/candidate_artifacts.py` now owns the v3 format and the consumer;
  `scripts/release-candidate.sh` is the stable wrapper. Recording reads the paginated artifacts API
  of the exact run; the consumer checks API identity, then hashes the raw response bytes, then the
  ZIP structure, then extracts into a fresh per-record namespace, and only merges once all seven
  namespaces verify. `release.yml`'s `host` no longer downloads the promotion source with
  `pattern: artifacts-*` + `merge-multiple: true`. 37 fixtures cover every named corruption class
  plus a deterministic round-trip; ordering is pinned by `scripts/check-release-integrity.sh`,
  `scripts/test-promote-release-flow.sh` and
  `crates/flux-eval/tests/release_authority.rs::the_tag_run_consumes_the_candidate_bytes_through_the_receipt`.
- 2026-08-05 — one nuance recorded rather than glossed: Python's ZIP reader truncates a member name
  at the first NUL before the extractor sees it, so the NUL case is proved by outcome (no NUL ever
  reaches a destination path, and what survives stays inside the namespace) while the other control
  characters are rejected by name. Hardlinks have no ZIP representation beyond the Unix mode bits,
  which `_check_member_kind` rejects along with symlinks, devices, FIFOs and sockets.
- 2026-08-01 — filed from validation of REL-01 subclaim (d).
- 2026-08-04 — contract raised to `ready` at canonical
  `9e3108b1b6856e30fa2e0baa2475d75d21fbc19f`. The exact current producer closure is one plan upload,
  five target uploads and one global upload; receipt v2 binds none of their IDs, sizes or digests.
  Every acceptance box remains open.

## Notes

- The GitHub API digest authenticates the Actions artifact ZIP. Checksums inside the ZIP authenticate
  neither the transport nor the API handoff and cannot replace the receipt field.
- C-516 separately owns the 28 files assembled from these verified archives and their live Release
  state.
