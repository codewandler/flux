---
id: D-121
title: browser plugin foundation — headless Chromium sessions over CDP-on-a-pipe
pillar: Core
status: ready
priority: 19
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "tier 3 keystone: plugins/browser with a minimal hand-rolled CDP client over --remote-debugging-pipe (stdio fd 3/4 — no WS dep, no debug port); Chrome discovery FLUX_BROWSER_BIN→config→PATH, NO auto-download; browser.open/goto/close + session TTL; coarse nav-URL guard until D-124"
---

# browser plugin foundation — headless Chromium sessions over CDP-on-a-pipe

## Goal
The substrate for tier 3 of [web-capabilities](../designs/web-capabilities.md): a `plugins/browser`
crate that spawns headless Chromium, speaks a minimal hand-rolled CDP over the remote-debugging
*pipe* (JSON-RPC on stdio fds 3/4 — no WebSocket dependency, no network control socket, no debug
port to squat), and manages page sessions the later observe/act ops attach to.

## Acceptance
- [ ] Minimal CDP client typed for only the domains the epic uses (Target, Page, DOM,
      Accessibility, Runtime, Input, Fetch): call/response correlation, event subscription,
      `\0`-framed messages on the pipe pair. Failing-first: hermetic tests against a scripted fake
      CDP endpoint (canned JSON-RPC exchanges) — no Chrome needed in CI.
- [ ] Chrome spawn as a manifest-declared process capability (operator-visible): headless (new
      mode), ephemeral isolated profile dir per session, cleaned up on close/TTL. Binary discovery
      `FLUX_BROWSER_BIN` → config → PATH candidates (`chromium`, `google-chrome`, …); **no
      auto-download**; missing browser → actionable error naming the discovery order.
- [ ] Ops: `browser.open {url?} → session` (returns url/title header, digest arrives in D-122),
      `browser.goto {session, url}`, `browser.close {session}`. Session registry in the plugin
      process with an idle TTL; TTL expiry closes Chrome and invalidates the session id.
- [ ] Interim egress gate: navigation URLs guarded via `flux_system::net::guard_url_scoped`
      (public-only; private via the plugin's scoped grant), explicitly documented as
      **insufficient** — subresources/redirects/JS escape it; [D-124](D-124-browser-egress-interception.md)
      is required for epic-done. Test: `browser.goto` to a private-range URL refused without grant.
- [ ] Env-gated live smoke (SKIP when no Chromium on PATH): open → goto a local fixture page →
      close, leaving no orphan Chrome process (test asserts child reaped).

## Progress
- 2026-07-09 — Filed with the epic.

## Notes
- Hand-rolled-over-bindings rationale (chromiumoxide/headless_chrome rejected for v1) is in the
  design's Alternatives; the scripted-CDP fake doubles as the protocol-drift regression net.
- The plugin spawning Chrome itself (vs host-mediated spawn) is a deliberate call: the pipe pair
  can't cheaply cross the host protocol; the egress-relevant channel (page traffic) is governed by
  D-124 interception instead.
