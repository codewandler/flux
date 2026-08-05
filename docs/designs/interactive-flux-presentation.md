# Interactive Flux presentation

**Story:** C-491 (ten-chapter deck, shipped 0.53.0) · C-540 (thirteen chapters, UX hardening,
refreshed truth boundary)
**Status:** implemented; amended 2026-08-05 by C-540

## Decision

The existing Docusaurus artifact gains one native `/presentation/` page. It is served publicly at
the site's `/flux/presentation/` base path and embedded byte-for-byte in `flux docs`; `/console/`
links to it. A small React deck owns navigation and presentation state without adding a slide
framework dependency or a server route.

The deck is thirteen short chapters for a developer and SRE audience: orientation and thesis, the
transcript-as-runtime problem, the deterministic execution path, the adaptive agent loop, the
guarded local example, product surfaces, connectors, Exchange, local/shared topologies, sessions as
the operational record, operational posture with shipped gaps, model strategy, and next steps —
about twenty minutes end to end.

Slide position lives in the URL hash and in history: navigation pushes state, so the browser Back
button steps to the previous chapter. Arrow keys, Page Up/Down, Home/End and Space mirror visible
controls and keep working while a deck control has focus (Space still activates a focused control);
a contents menu jumps to any chapter; horizontal swipe advances on touch. All chapters render in
the DOM with hidden ones inert, so printing yields a complete handout with a static listing in
place of the editor; the Monaco workbench mounts only once the demo chapter is first shown and
stays mounted. Fullscreen uses the browser API; reduced-motion and compact layouts retain the same
information without animation.

## Runtime boundary

The one runnable chapter reuses L-128's checked-in `rust-files` fixture and the shared
`FluxWorkbench`. The hosted build receives no runtime bootstrap and stays edit/check-only. A
loopback `flux docs` process exposes only L-128's existing cookie-bound scratch execution. This
story adds no fixture, operation, permission, credential, network path, shell, plugin, or server
capability.

## Truth boundary

Mutable ecosystem claims carry an explicit dated snapshot label and point to the upstream READMEs.
The presentation and the ecosystem summary ([docs/ecosystem.md](../ecosystem.md), the source of
truth for these claims) separate what ships from the charter:

- flux-connectors publishes a compiler, catalogue, manifests, and host Tool pack. Connector
  manifests also drive Flux's deliberately narrow inbound channel: declarative webhooks plus
  generated RFC 6455 socket subscriptions, unsigned-only until the HMAC verifier ships.
- flux-exchange serves OIDC sign-in, tenant connections/settings, metadata grants, a human admin
  console, guarded HTTP `invoke`, and Service Account bearer authentication. General inbound
  lifecycle and execution records remain direction.
- Flux embeds the Exchange Service Account client (C-503): one account's effective catalogue is
  projected at turn boundaries and admitted operations invoke through Exchange's one-shot HTTP
  route. Streaming invocation and the general stream/lease protocol are not built. Flux stays
  complete without Exchange.

The source snapshots are flux-connectors main reporting v0.20.0 and flux-exchange main reporting
v0.17.0, verified against both repositories' `origin/main` on 2026-08-05. Exact catalogue counts
are not used as timeless marketing claims.

## Verification

The embedded-router contract names the new entry point so a source-only page cannot be reported as
shipped before the tracked archive is regenerated. Website contract tests pin the content boundary,
stale-claim removal (including the retired pre-C-503 Exchange gap claim), one distinctive claim per
chapter added by C-540, the dated snapshot labels, and the non-console entry points (footer,
landing page, docs overview) so the deck cannot become an orphan again. The ordinary website build,
mirror regeneration checks, deterministic embedded bundle check, and full Rust gate remain the
completion criteria.
