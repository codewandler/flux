---
id: D-121
title: browser foundation — native headless Chromium sessions over CDP-on-a-pipe
pillar: Core
status: done
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
- [x] Minimal CDP client (`flux-web::cdp`) — call/response correlation by id, event subscription
      (forwarded on an mpsc channel), `\0`-framed JSON, optional `sessionId` routing (flattened mode),
      transport-agnostic over any `AsyncRead`/`AsyncWrite`. Hermetic tests against a scripted fake over
      an in-memory duplex — no Chrome in CI: response correlation + events, CDP-error surfacing,
      session-id routing, and disconnect-fails-pending-calls (no hang).
- [x] Chrome spawned as a direct child via a **new guarded seam** `flux_system::System::spawn_debug_pipe`
      (a full-duplex socketpair mapped onto the child's fd 3/4 in a `pre_exec` hook — async-signal-safe
      `dup2`/`fcntl` only; same `build_command` envelope: argv-only, env-cleared, cwd-pinned). Headless
      (`--headless=new`), ephemeral isolated profile per session (removed on close/TTL). Discovery
      `FLUX_BROWSER_BIN` → config `browser_bin` → PATH candidates; **no auto-download**; missing browser
      → actionable error naming the order. `Effect::Process` on `browser.open`.
- [x] Ops: `browser.open {url?}` (returns `session` id + the first digest — the D-122 digest arrived
      with it), `browser.goto {session, url}`, `browser.close {session}`. `SessionRegistry` behind
      `Arc<Mutex<…>>` with a lazily-swept idle TTL (5 min); close/expiry kills Chrome + removes the
      profile + invalidates the id.
- [x] Evidence-gated surfacing: the `browser` `ToolGroup` (`browser_group()`) is pushed at the two CLI
      group sites; its signal is a `chromium_present` probe in `flux_runtime::detect_signals`. Test
      `browser_ops_are_gated_by_the_browser_signal`: absent from the catalog with no signal, present
      with it.
- [x] Interim egress gate: `browser.goto` guards the nav URL via `guard_url_scoped` with the `web`
      scope — but the real policy is [D-124](D-124-browser-egress-interception.md)'s per-request
      `Fetch` interception (shipped in the same push; the coarse nav check is now belt-and-suspenders).
- [x] Env-gated live smoke `live_smoke_open_goto_snapshot_close_no_orphan` (SKIP when no Chromium):
      launches **real** headless Chrome, opens → navigates a local fixture (under a test-scoped `web`
      grant) → snapshots the digest (asserts page content + the button in the action space) → closes,
      asserting the Chrome child is reaped via `/proc` (no orphan). **Passes on a machine with Chrome.**

## Progress
- 2026-07-09 — **DONE end-to-end** (incl. a real-Chrome live smoke): `flux_system::spawn_debug_pipe`
  (guarded fd-3/4 socketpair via `pre_exec`); `flux-web::browser` — discovery, `launch_session`
  (createTarget/attachToTarget flatten + enable domains + `Fetch.enable`), `SessionRegistry` + TTL,
  `browser.open/goto/snapshot/act/close`, the event pump (load-wait, console/dialog tracking, D-124
  Fetch interception), `browser_group()` + `chromium_present` in `detect_signals`, `browser_bin`
  config. 12 browser tests (hermetic scripted-fake) + the live smoke.
- 2026-07-09 — **CDP client landed** (`flux-web::cdp`, keystone): the hand-rolled `\0`-framed
  JSON-RPC transport with id-correlated calls, an event stream, session routing, and disconnect
  safety — 4 hermetic tests (scripted fake over `tokio::io::duplex`), no Chrome. tokio moved to
  runtime deps. This is the substrate D-122/123/124 attach to.
- 2026-07-09 — **Remaining (next push, deliberately unhurried — safety-critical):**
  1. A **guarded fd-3/4 pipe spawn** on `flux_system::System` (the browser's transport): keep the one
     guarded `build_command` path (argv-only, env-cleared, cwd-pinned) but wire two extra pipes to the
     child's fd 3 (Chrome reads commands) / fd 4 (Chrome writes) via `pre_exec` dup2 — an `unsafe`
     addition to the L2 safety crate that can't be hermetically tested, so it warrants care over speed.
  2. Chrome discovery (`FLUX_BROWSER_BIN` → config → PATH candidates; no auto-download) + headless
     spawn with an ephemeral profile + `Effect::Process` on `browser.open`.
  3. `browser.open`/`goto`/`close` + the `Arc<Mutex<…>>` session registry with an idle TTL.
  4. The evidence-gated `browser` `ToolGroup` (a `chromium_present` probe in `detect_signals` +
     `flux_web::browser_group()` pushed at the two CLI group sites) so the ops stay out of the catalog
     when no Chromium is discoverable.
  5. Interim nav-URL guard (documented insufficient until D-124) + the env-gated live smoke.
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
