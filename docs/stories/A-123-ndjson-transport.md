---
id: A-123
title: "A later NDJSON transport can attach foreign CLI workers to AgentRuntime"
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-runtime, flux-orchestrate]
note: "Decision 0010 excludes direct Claude/Codex process workers from native fleet V1; they are CLI clients now, and this remains a later transport story"
---

# A later NDJSON transport can attach foreign CLI workers to AgentRuntime

## Goal

After the native local fleet is proven, evaluate a framed NDJSON transport that composes with
`AgentRuntime` for foreign CLI workers without changing the board/fleet CLI contract or reviving a
runtime-selecting address URI.

## Acceptance

- [ ] The transport remains independent from Board/Fleet identity and composes with the existing
      runtime port; no Claude/Codex-specific field enters the public coordinator contract.
- [ ] The wire reuses C-160's published vocabulary and the guarded child-process supervisor.
- [ ] Offline tests cover persistent second turn, cancellation, terminal process loss and the lack
      of retained-task parity with A2A.
- [ ] The story is not a dependency of Decision 0010 V1 and is promoted only after native fleet
      dogfood establishes a concrete need.

## Progress

- 2026-08-05 — moved from ready to backlog by Decision 0010. Claude and Codex call `flux board` and
  `flux fleet`; they are not V1 worker transports.

## Notes

- The earlier detailed transport acceptance remains design input, not current scheduled work.
