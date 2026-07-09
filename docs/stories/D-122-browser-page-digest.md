---
id: D-122
title: browser page digest — condensed content + resolved action space with stable refs
pillar: Agent
status: backlog
priority:
epic: web-capabilities
design: docs/designs/web-capabilities.md
note: "the heart of non-visual browsing: browser.snapshot joins the accessibility tree with DOM node identity → url·title header, byte-budgeted condensed content, and an actions table (e<N> role \"name\" state) with refs stable across observations; DOM-heuristic fallback for unlabeled div-soup clickables; deterministic ordering; needs D-121"
---

# browser page digest — condensed content + resolved action space with stable refs

## Goal
Let the agent "see" a page the smart way: `browser.snapshot` returns a bounded **digest** — what a
screen reader sees (roles, names, states) plus condensed readable text — never HTML source, never
a screenshot. The digest *is* the action space: every interactive element gets a stable ref the
act ops (D-123) target.

## Acceptance
- [ ] Digest builder over `Accessibility.getFullAXTree` joined with DOM node identity
      (`backendNodeId`): header (`url · title`), `## content` (condensed readable text — reuse the
      D-120 condenser or AX text), `## actions` table — `e<N> role "accessible name" (state)`,
      states covering disabled/checked/expanded/current value for inputs.
- [ ] Interactive filter by AX role (button, link, textbox, combobox, checkbox, radio, tab,
      menuitem, …) **plus** a DOM-heuristic fallback so unlabeled div-soup clickables still
      surface (fixture: a page whose only "button" is a `div` with a click handler).
- [ ] Stable refs: `e<N>` ↔ `backendNodeId` map held session-side; re-observation preserves live
      refs; dead nodes are marked dead, never silently renumbered. Test: two snapshots around a
      partial DOM mutation keep untouched refs identical.
- [ ] Both sections byte-budgeted with omission markers, `len <= cap` pinned by test (A-24
      lesson); caps overridable per call.
- [ ] Deterministic output ordering (document order; ties broken stably) — replay/`flux diff`
      friendly. Failing-first: canned AX/DOM payload through the builder → golden digest.
- [ ] `browser.snapshot {session, view: full|actions|content}`; `browser.open`/`goto` (D-121) now
      return the digest as their result.

## Progress
- 2026-07-09 — Filed with the epic; needs [D-121](D-121-browser-plugin-cdp-foundation.md).

## Notes
- Hermetic-first: the builder is pure over captured AX/DOM payloads, so goldens don't need Chrome;
  the env-gated live smoke re-captures fixtures.
