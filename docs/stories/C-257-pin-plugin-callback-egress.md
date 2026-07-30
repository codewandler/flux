---
id: C-257
title: "Pin plugin HTTP, OAuth, and TCP callbacks to guard-vetted addresses"
pillar: Core
status: done
priority: 2
epic: adversarial-review-remediation-2026-07-30
design: docs/designs/adversarial-review-remediation-2026-07-30.md
areas: [flux-plugin, flux-credentials, flux-system]
note: "HIGH — redirect control is sound, but HTTP/OAuth/TCP discard vetted DNS answers before connect"
---

# Pin plugin HTTP, OAuth, and TCP callbacks to guard-vetted addresses

## Goal

Ensure a plugin capability grant to a public hostname cannot rebind at connection time into
loopback, RFC1918, link-local, metadata, or another private destination.

## Acceptance

- [x] Failing-first injected-resolver tests cover ordinary HTTP, each followed redirect, OAuth token
      refresh, and `conn.dial` with validation/connect answers that differ.
- [x] Plugin HTTP builds a pinned client for each guarded hop and fails closed when no address was
      vetted; existing cross-origin credential stripping and downgrade refusal remain intact.
- [x] Guarded HTTP and OAuth transports ignore ambient proxy variables; proxy-side DNS cannot
      replace the admitted peer.
- [x] OAuth refresh accepts a guarded/pinned transport rather than receiving a URL string that a
      fresh client resolves again.
- [x] TCP connects directly to a vetted `SocketAddr` set and never re-resolves the hostname after
      authorization; connection grants and private-network audit records remain accurate.
- [x] Unix-socket wildcard grants match one path segment only and reject `.`/`..` components before
      the kernel can resolve a granted spelling outside its declared scope.
- [x] Unix-socket grants are enforced against the socket's physical identity, so a granted-looking
      symlink cannot reach an out-of-grant listener, while a symlink resolving inside the grant
      stays authorized.
- [x] Plugin host/network tests and the standard root/plugin gates are green.

## Progress

- Plugin HTTP now guards and builds a fresh pinned, redirect-disabled client for every hop while
  retaining bounded GET/HEAD redirects, downgrade refusal, and cross-origin credential stripping.
- OAuth refresh lazily receives that pinned transport only when a stale stored token requires IO;
  absent/fresh tokens remain independent of DNS availability.
- `conn.dial` resolves once, validates the complete answer set, and connects directly to those
  `SocketAddr`s. Audit classification consumes the same vetted addresses instead of re-resolving.
- Injected rebinding/empty-answer tests cover HTTP hops, OAuth refresh, and TCP. Full touched-crate
  tests, scoped Clippy, the integrated workspace build/test/Clippy/format gate, and `flux-codegate`
  pass.
- The final closure review found that reqwest's ambient proxy support could still route guarded
  HTTP through an unvetted peer. Plugin and native web pinned transports now disable proxies, with
  isolated-process regressions proving only the guard-vetted listener is contacted.
- The same closure pass found that the documented single-segment Unix wildcard consumed `/` and
  `..`. Grant matching now rejects dot-component paths and constrains `*` to one socket-name segment;
  a regression denies a Docker-socket traversal from a nominally private plugin directory.
- That closure pass left its own last test red: it added
  `unix_conn_grant_does_not_follow_symlink_outside_granted_directory` but never landed the fix, so
  a dot-free single-segment name like `alias.sock` still matched its grant and the kernel followed
  it out. `conn.dial` now reduces the Unix target *and* each Unix grant's literal directory prefix
  to a physical identity through a new `System::host_path_identity` seam — the same reduce-both-sides
  rule `read_file_scoped` already applies to `fs.read`, and the reason a `/tmp` vs `/private/tmp`
  spelling difference does not become a spurious denial. Only the literal prefix before a wildcard is
  resolved; the wildcard segment stays pattern text. The paired
  `..._allows_symlink_that_resolves_inside_granted_directory` test keeps the fix a containment
  boundary rather than a ban on symlinked sockets, and checking the resolved path is what gets dialed
  removes the check-then-dial window. `flux-plugin` production code takes no direct filesystem
  dependency, so `check-no-direct-io.sh` still passes without an allow-reason.

## Notes

- Evidence: review A finding 1. The primary review correctly saw redirect disabling but overstated
  that as complete egress containment; DNS pinning was still absent.
