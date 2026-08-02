# Interactive Flux presentation

**Story:** C-491
**Status:** implemented; awaiting a green shared-worktree clippy gate

## Decision

The existing Docusaurus artifact gains one native `/presentation/` page. It is served publicly at
the site's `/flux/presentation/` base path and embedded byte-for-byte in `flux docs`; `/console/`
links to it. A small React deck owns navigation and presentation state without adding a slide
framework dependency or a server route.

The deck is ten short chapters for a developer and SRE audience: problem and thesis, deterministic
execution path, guarded local example, product surfaces, connectors, Exchange, local/shared
topologies, operational posture, shipped gaps, and next steps. Slide position lives in the URL hash.
Arrow keys, Page Up/Down, Home/End and Space mirror visible controls; fullscreen uses the browser
API; reduced-motion and compact layouts retain the same information without animation.

## Runtime boundary

The one runnable chapter reuses L-128's checked-in `rust-files` fixture and the shared
`FluxWorkbench`. The hosted build receives no runtime bootstrap and stays edit/check-only. A
loopback `flux docs` process exposes only L-128's existing cookie-bound scratch execution. This
story adds no fixture, operation, permission, credential, network path, shell, plugin, or server
capability.

## Truth boundary

Mutable ecosystem claims carry an explicit 2026-08-03 snapshot label and point to the upstream
READMEs. The presentation and ecosystem summary separate what ships from the charter:

- flux-connectors publishes a compiler, catalogue, manifests, and host Tool pack; Flux itself
  currently consumes connector manifests for a deliberately narrow inbound webhook channel, not an
  installed outbound connector catalogue.
- flux-exchange serves OIDC sign-in, tenant connections/settings, metadata grants, a human admin
  console, and guarded HTTP `invoke`. Agent tokens can be minted but do not authenticate yet.
- Flux has no Exchange client binding today; Exchange `subscribe`, hosted channels, stored
  workflows, and execution records remain unbuilt. Flux stays complete without Exchange.

The source snapshots are flux-connectors main reporting v0.16.0 and flux-exchange main reporting
v0.13.0 on 2026-08-03. Exact catalogue counts are not used as timeless marketing claims.

## Verification

The embedded-router contract names the new entry point so a source-only page cannot be reported as
shipped before the tracked archive is regenerated. Website contract tests pin the content boundary
and stale-claim removal. The ordinary website build, mirror regeneration checks, deterministic
embedded bundle check, and full Rust gate remain the completion criteria.
