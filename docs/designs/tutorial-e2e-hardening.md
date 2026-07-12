# Tutorial E2E hardening

Status: implemented  
Date: 2026-07-12

## Problem

Following the public beginner tutorial from a fresh `/tmp` workspace exposed five gaps that
syntax-only documentation tests could not catch:

1. `flux plan` documentation still described the pre-A-18 single-pass contract, although plan mode
   may now execute bounded read-only gather rounds before returning the pending plan.
2. A `ctx` value stored only audit metadata (`name`, `members`, `purpose`, `budget`). Passing it to
   `ai.reason` therefore sent symbol names, not the values the flow had read.
3. OpenAI GPT-5 Chat Completions requests used `max_tokens`, which that model family rejects in
   favour of `max_completion_tokens`.
4. `AgentDecl.datasources` was parsed but did not affect the agent prompt or retrieval boundary.
5. A direct SIGINT could leave `flux app run` alive while its interactive stdin read remained
   blocked.

## Decisions

### Context packs materialize once, after gating

`ctx` continues to select and audit symbol references. After private/hidden filtering and
visibility-priority packing, the interpreter now renders each retained bound value as a labelled
section (`## $symbol`) in an internal `content` field on the runtime `Ctx` value. Budget accounting
uses the exact character length of those rendered sections, including separators. This preserves
drop-and-continue packing and ensures the payload consumed by a model cannot exceed the declared
character budget.

`ai.reason` reads only the context purpose and materialized content. Legacy hand-built context
objects without `content`, and plain string contexts, keep their former generic rendering.

The payload is a runtime-produced wire field rather than a new public Rust field on
`flux_lang::prelude::Ctx`; this fixes execution without breaking external struct construction.

### GPT-5 request shaping is model-family-specific

The Chat Completions codec emits `max_completion_tokens` for bare or provider-prefixed GPT-5 family
ids and retains `max_tokens` for older chat models such as GPT-4o. Responses API request shaping is
unchanged.

### Datasource declarations are capability boundaries

Each app agent receives model framing naming its declared sources and requiring grounded retrieval.
Its retrieval tools are wrapped before `AgentSpec` assembly:

- one declared source is injected when `source` is omitted;
- an explicitly undeclared source is rejected before the underlying tool executes;
- multiple sources require an explicit choice;
- `sources` reports only the names declared for that agent.

Journeys keep the app-wide registry; the restriction applies to the agent target whose declaration
owns the datasource list.

### Interactive stdin must not own runtime shutdown

Tokio implements terminal stdin with an uncancellable blocking runtime worker. The CLI channel now
reads stdin on a detached standard thread and forwards lines over an async channel. Cancelling the
app drops the receiver and lets the Tokio runtime finish immediately; a thread still blocked on the
terminal cannot hold process exit.

## Verification

- failing-first unit tests cover GPT-5 token-field selection, context materialization, cognition
  prompt construction, and datasource injection/rejection;
- the website contract executes the tutorial's exact `brief.flux` fence with a capture provider and
  asserts all handbook facts reach `ai.reason`;
- the website contract parses and inspects the exact app fence and guards the plan-mode wording;
- a Unix integration test starts the exact tutorial app with the offline provider, waits for its
  welcome message, sends SIGINT directly, and requires a clean exit within five seconds.
