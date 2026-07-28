---
id: D-193
title: "MCP interop — consume and expose Model Context Protocol (epic)"
pillar: Agent
status: backlog
priority:
epic: mcp-interop
design:
note: "EPIC — mount MCP servers as guarded tool sources through the existing envelope/schema/surfacing pipeline, and expose flux ops/flows as an MCP server; completes the interop dialect the Claude epic (D-186..D-192) started"
---

# MCP interop — consume and expose Model Context Protocol (epic)

## Goal
Close flux's biggest ecosystem gap: an MCP server (stdio/HTTP) mounts as a flux tool source whose
tools flow through the same guarded envelope, schema pipeline, and evidence-gated surfacing as
native plugins (client half); and `flux serve --mcp` exposes flux ops/flows as MCP tools so Claude
Code, IDEs, and other agents can drive flux (server half). Claude interop (D-186…D-192) covered
commands/skills only; this completes the dialect.

## Acceptance
- [ ] A configured MCP server's tools appear as flux ops, dispatch through the guarded envelope
  (approval, policy, audit) exactly like plugin ops, and never bypass evidence-gated surfacing —
  proven by a hermetic stdio MCP stub in a failing-first integration test.
- [ ] MCP tool schemas round-trip into the flux op-schema pipeline (declared input/output schemas
  preserved, mirroring D-164 semantics).
- [ ] `flux serve --mcp` (or equivalent) exposes a configured op/flow set over MCP; a stock MCP
  client can list and call them, with every call passing the authorization policy.
- [ ] Credential handling follows the references-only invariant — no raw secrets to or from the
  MCP boundary.
- [ ] Website docs page states honestly what is and isn't supported (transports, capabilities).

## Progress
- (not started — filed from the 2026-07-28 feature-suggestion pass)

## Notes
- Natural home: an adapter beside `flux-plugin` (client) and a `flux-server` surface (server).
- Reuse the host-capability seam; MCP servers are just another subprocess/HTTP tool host.
