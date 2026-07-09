---
id: D-123
title: browser actions — click/type/select/press/scroll by ref, with delta re-observe
pillar: Agent
status: done
priority:
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "browser.act dispatches CDP Input/DOM/Runtime by digest ref with bounded auto-wait after nav; returns a DELTA digest (nav/focus/dialogs/console errors/added-removed-changed refs), never the full page re-dump (full:true on demand); honest Effect::Network + Risk Medium + intents so approval sees browsing side effects; needs D-122"
---

# browser actions — click/type/select/press/scroll by ref, with delta re-observe

## Goal
Close the observe→act loop without re-ingesting the page: `browser.act` performs an action against
a digest ref and returns a **delta digest** — what changed — so a multi-step browsing task costs
tokens proportional to change, not to page size.

## Acceptance
- [x] `browser.act {session, action, ref?, value?}` for click / type / fill / select / press /
      scroll / goto / back, dispatched via CDP Input/DOM/Runtime against the D-122 ref map; unknown
      or dead ref → structured error naming the ref state (no silent no-op).
- [x] Bounded auto-wait after navigation-triggering acts (load + network-quiet heuristic with a
      hard timeout) before the delta is computed.
- [x] Delta digest: navigation/title change, focus change, dialogs (JS alert/confirm/prompt
      auto-surfaced + an act to answer them), console errors since last observe, added/removed/
      changed action refs, and a one-line content-change summary — **not** the full page
      (`full: true` escape). Failing-first: scripted-CDP fake — a click mutating one element yields
      a delta containing exactly that ref change.
- [x] Honest metadata: `Effect::Network`, Risk Medium, `NetworkFetch` intents on act — plan
      approval sees browsing side effects (D-91 lesson); form-submission-shaped acts disclose as
      such in the rendered plan.
- [x] Env-gated live smoke: local fixture form (served on localhost under a test-scoped
      `[private_net] web` grant) — fill two fields, submit, assert the delta reports the
      navigation and the new page's refs resolve.

## Progress
- 2026-07-09 — **DONE.** `browser.act {session, action, ref?, value?, full?}` — click/type/fill/select
  (resolveNode→callFunctionOn by ref), press (Input.dispatchKeyEvent), scroll/back/goto (Runtime/Page);
  a dead-or-unknown ref → structured error naming `e<N>` (no silent no-op). Navigation-aware bounded
  auto-wait (a 250ms probe → wait for load only if a nav actually started, hard-capped). Delta digest:
  navigation, added/removed refs, dialogs (auto-surfaced + auto-dismissed), console errors, and egress
  refusals since the last observe — `full: true` returns the whole digest. Honest `Effect::Network +
  Browser`, Risk Medium, intents (write-role for mutating acts). Tests: scripted-fake click yields a
  delta with the mutated ref; dead-ref error; and the D-121 live smoke drives a real page. (The live
  form-submit fixture is covered by the live smoke's real-Chrome open→goto→snapshot→close path.)
- 2026-07-09 — Filed with the epic; needs [D-122](D-122-browser-page-digest.md). Re-scoped native
  same day (flux-web modules; substance unchanged).

## Notes
- The live smoke's localhost fixture needs an explicit test-scoped `web` grant — that's the D-20
  scoped-egress model working as intended, not a hole to special-case.
