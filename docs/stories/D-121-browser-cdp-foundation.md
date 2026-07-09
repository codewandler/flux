---
id: D-121
title: browser foundation — native headless Chromium sessions over CDP-on-a-pipe
pillar: Core
status: ready
priority: 19
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "tier 3 keystone, native in flux-web: minimal hand-rolled CDP client over --remote-debugging-pipe (stdio fd 3/4 — no WS dep, no debug port); Chrome discovery FLUX_BROWSER_BIN→config→PATH, NO auto-download; browser.open/goto/close + Arc<Mutex> session registry + TTL; `browser` ToolGroup surfaced by a Chromium-discoverable signal; coarse nav-URL guard until D-124; needs D-98"
---

# browser foundation — native headless Chromium sessions over CDP-on-a-pipe

## Goal
The substrate for tier 3 of [web-capabilities](../designs/web-capabilities.md): flux-web modules
(`cdp`, `browser`) that spawn headless Chromium as a direct child, speak a minimal hand-rolled CDP
over the remote-debugging *pipe* (JSON-RPC on stdio fds 3/4 — no WebSocket dependency, no network
control socket, no debug port to squat), and manage page sessions the later observe/act ops attach
to.

## Acceptance
- [ ] Minimal CDP client (`flux-web::cdp`) typed for only the domains the epic uses (Target, Page,
      DOM, Accessibility, Runtime, Input, Fetch): call/response correlation, event subscription,
      `\0`-framed messages on the pipe pair. Failing-first: hermetic tests against a scripted fake
      CDP endpoint (canned JSON-RPC exchanges) — no Chrome needed in CI.
- [ ] Chrome spawned as a direct child with `Effect::Process` disclosed on `browser.open`:
      headless (new mode), ephemeral isolated profile dir per session, cleaned up on close/TTL.
      Binary discovery `FLUX_BROWSER_BIN` → config → PATH candidates (`chromium`,
      `google-chrome`, …); **no auto-download**; missing browser → actionable error naming the
      discovery order.
- [ ] Ops: `browser.open {url?} → session` (returns url/title header, digest arrives in D-122),
      `browser.goto {session, url}`, `browser.close {session}`. In-process session registry behind
      `Arc<Mutex<…>>` (the `EndpointBroker`/`SqliteBackend`/`ReadTracker` stateful-tool shape) with
      an idle TTL; expiry closes Chrome and invalidates the session id.
- [ ] Evidence-gated surfacing: browser ops carry a `browser` `ToolGroup` whose `surface_when`
      signal is *a Chromium binary is discoverable* (a `detect_signals` probe; `FLUX_SURFACE_ALL`
      still forces). Test: with no binary discoverable, browser ops are absent from the advertised
      catalog; with one, present.
- [ ] Interim egress gate: navigation URLs guarded via `flux_system::net::guard_url_scoped` with
      the D-98 `web` scope, explicitly documented as **insufficient** — subresources/redirects/JS
      escape it; [D-124](D-124-browser-egress-interception.md) is required for epic-done. Test:
      `browser.goto` to a private-range URL refused without grant.
- [ ] Env-gated live smoke (SKIP when no Chromium on PATH): open → goto a local fixture page →
      close, leaving no orphan Chrome process (test asserts child reaped).

## Progress
- 2026-07-09 — **Re-scoped native** (user call): plugins/browser becomes flux-web modules; the
  "manifest-declared process capability" framing is replaced by `Effect::Process` disclosure on
  the op spec; added the evidence-gated `browser` group (ungated native ops land in every system
  prompt — gating keeps the catalog honest on machines without Chrome).
- 2026-07-09 — Filed with the epic.

## Notes
- Hand-rolled-over-bindings rationale (chromiumoxide/headless_chrome rejected for v1) is in the
  design's Alternatives; the scripted-CDP fake doubles as the protocol-drift regression net.
- The `shell` group is the opt-in-signal precedent; here the signal is environmental evidence
  (binary discoverable) rather than an env flag.
