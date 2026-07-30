---
id: C-290
title: "A runtime has no memory ceiling and no general concurrency limit"
pillar: Core
status: ready
priority: 4
areas: [flux-config, flux-runtime, flux-sdk]
note: "surveyed while designing the flux-connectors interop: context_budget/max_iterations/max_tokens/max_calls exist, but nothing bounds memory and the only concurrency control is server-side max_inflight_per_principal — so an embedding host cannot bound a runtime it constructs"
---

# A runtime has no memory ceiling and no general concurrency limit

## Goal

Let a host that constructs a runtime bound its resource use, not just its token spend.

## Acceptance

- [ ] A host can set a **concurrency limit** when building a client — a ceiling on simultaneously
      executing tool calls — and it applies to in-process embedding, not only to `flux-server`.
- [ ] A host can set a **memory ceiling**, or this story records precisely why that is not
      implementable in-process and narrows to what is: a bound on the things that actually grow
      without limit (retained tool results, evidence log, transcript).
- [ ] Whatever lands is reachable from `ClientBuilder` (`crates/flux-sdk/src/lib.rs:371`) alongside
      the existing `context_budget`, `max_iterations`, `max_tokens` and `max_calls`, and from
      `flux-config` for a file-configured host.
- [ ] **Failing-first test:** a runtime configured with a concurrency limit of N never has more than N
      tool executions in flight, demonstrated with a tool that blocks until released.
- [ ] Exceeding a limit is an observable, actionable refusal — never a silent truncation or a hang.
- [ ] The gate is green.

## Notes

- **What exists today**, surveyed rather than assumed: `AgentConfig::max_iterations`,
  `max_model_calls`, `ModelStageConfig::max_tokens`, `ConsultConfig::max_calls`,
  `Limits::turn_token_budget`, and `ServerConfig::max_inflight_per_principal` — that last one being
  the *only* concurrency control, and it is server-side and per-principal.
- So an embedding host today can bound how much a runtime *spends* but not how much it *uses*. That
  asymmetry is the whole of this story.
- The memory half may well be the wrong shape as stated. A process-wide RSS ceiling is not something a
  library can honestly enforce; bounding the specific retained structures is. Prefer narrowing the
  acceptance to something true over shipping a knob that does not bind.
- Raised by the flux-connectors interop design (`docs/designs/connector-tool-pack.md` in that repo).
  Nothing in that work depends on this — it is filed so the gap is recorded rather than rediscovered.
