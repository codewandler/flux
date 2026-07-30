---
id: C-272
title: "The host-import ABI — every guard on the host side, proven unbypassable"
pillar: Core
status: blocked
priority: 5
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "the epic's whole security argument: imports are narrow already-guarded operations, never raw primitives — a module that declines to call a guard must not thereby escape one"
---

# The host-import ABI — every guard on the host side, proven unbypassable

## Goal

Define the imports a Flux module receives, such that a **hostile** submitted program cannot reach
anything it was not granted. The invariant is that the guard runs on the host side of the boundary: a
module controls its own control flow, so any check it is merely expected to call is not a check at all.

## Acceptance

- [ ] Imports are narrow, already-decided operations rather than primitives — `http_get(endpoint_ref)`
      with the host resolving the ref, applying `guard_url_scoped`, pinning the vetted address and
      injecting credentials; scoped path reads via physical-identity matching; a host-supplied clock.
      No import hands the module a raw capability.
- [ ] **Credentials never cross the boundary.** A test asserts a module cannot obtain a secret value,
      by the same reasoning the plugin host relies on.
- [ ] The test that carries the argument: a module that *does not* call a guard still cannot reach an
      ungranted destination, because the host applied it. Written failing-first against an
      intentionally hostile module, not a cooperative one.
- [ ] Private/loopback/link-local/ULA/CGNAT egress is refused unless a scoped grant exists, matching
      native behaviour — the boundary must not become a softer egress path than the native one.
- [ ] The ABI is versioned, so a module built against an older host fails cleanly rather than
      silently getting different semantics.
- [ ] Documentation states what this does not cover: authorized exfiltration, and side channels.

## Progress

- (blocked on C-271 — an ABI without a module to run is unfalsifiable)

## Notes

- ⚠ The failure mode to design against is a host that exports `fetch(url)` and *expects* the module to
  guard it. That is the natural-looking design and it is worthless against an adversary.
- Precedent to follow closely: plugin host capabilities are deny-by-default and manifest-scoped, and
  the plugin process is env-cleared so it cannot read host secrets from the environment. C-256/C-257
  are the address-pinning behaviour this must inherit, including the symlink/physical-identity rule for
  any path-shaped grant.
