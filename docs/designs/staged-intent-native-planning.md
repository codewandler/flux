# Staged intent and native-schema planning

**Status:** superseded by [A-73](adaptive-outer-loops.md) · **Story:**
[A-71](../stories/A-71-staged-intent-native-planning.md) · **Scope:** historical PoC

> This document records the opt-in experiment that proved native-schema stages. The shipped design
> no longer has a compiler fallback, `PlanningMode`, or `--staged`; use
> [Flux-authored adaptive outer loops](adaptive-outer-loops.md) and
> [`scripts/eval-adaptive-support.sh`](../../scripts/eval-adaptive-support.sh).

## Problem

Flux's default planner gives the model one real provider tool, `emit_plan`. The operation selected
inside that plan is represented generically as `op: string` plus `args: Node[]`; operation-specific
JSON Schemas remain in the runtime registry and reach the model only as compact catalog prose. The
runtime catches bad arguments safely, but a weak or fast model often spends another full planner
round repairing a shape it could have produced correctly from a native tool schema.

The July 13 support-workspace fixture makes the failure concrete. The request requires four local
sources, two small calculations, and exact citations. Codex became reliable after argument/provenance
repairs but previously invented source paths. Gemini Flash was correct twice and fabricated every
central fact once after a failed plan. DeepSeek V4 Flash Nitro was correct three times but needed
4–10 provider calls and attempted direct tools that were unavailable on the planner surface. The
runtime remained safe; the compiler interface was needlessly difficult for those models.

The target loop is:

```text
query
  -> typed intent declaration
  -> registered capability signal
  -> bounded exploration/gathering with real operation schemas
  -> frozen atomic Flux plan
  -> deterministic validation
  -> existing plan approval
  -> existing execution runtime
```

The LLM is still not the runtime. Native tool calls are either bounded reads executed through the
same envelope or inert proposed actions lowered into Flux-Lang. They are never an alternate direct
effect path.

## Constraints

1. **One turn loop.** The opt-in branch lives in the shipped `agent-loop.flux`; it invokes one new
   reflexive host operation. There is no second agent runtime or independent approval mechanism.
2. **No authority from intent.** Intent only narrows what the model can see. The selected operations
   are a subset of the already-assembled registry, and every executed operation still crosses
   authorization → approval → guarded IO.
3. **No mutation during exploration.** A call is executed during exploration only when its declared
   contract is low-risk, idempotent, carries no write/process/browser/local-system effect, and its
   concrete `Tool::intents(input)` is neither mutating nor destructive. `Read + Network` is allowed
   only for an idempotent Low-risk read, so datasource retrieval can be explored without admitting
   arbitrary process or browser actions.
4. **No plan-shaped prose.** Proposed effects are stored as typed `(operation, JSON input)` records.
   Finalization deterministically creates a `DraftAst`; prose is never parsed into executable work.
5. **No default behavior change.** `PlanningMode::Flux` remains the default. Disabled staged mode
   returns a `legacy` sentinel without consulting a provider, and the existing orient/gather/execute
   branch proceeds byte-for-byte as before.
6. **No downstream coupling.** No `ai-agent-platform` files, schemas, flags, or dependency pins are
   part of this PoC.

## Agent-facing configuration

`flux-agent` gains a typed setting rather than an environment-only fork:

```rust
pub enum PlanningMode {
    Flux,
    StagedNative,
}

pub struct AgentSpec {
    // existing fields...
    pub planning_mode: PlanningMode,
}
```

`Flux` is the default. The CLI exposes an explicit `--staged` PoC switch that sets
`StagedNative`; SDK users can set the enum directly. Apps, roles, sub-agents, A2A, and voice are not
implicitly opted in. A future product decision may expose the setting in role/app source, but the
PoC does not silently broaden that surface.

## Capability index: small, semantic, and wired

The intent call must not recreate the 636-operation prompt. It receives a compact family index, not
every operation schema.

Physical families come from evidence-gated `ToolGroup`s. A physical family is intent-discoverable
when either:

- it is already active for the turn, or
- its `surface_when` contract includes `turn.intent` (the implicit installed-plugin groups).

This preserves explicit hard gates: for example, the `shell` family remains unavailable unless the
operator opted it in, while a registered Slack family can be discovered semantically. A family with
no operation in the assembled registry is omitted, so installed-but-failed/unwired integrations do
not become candidates.

Ungrouped core operations are placed into deterministic virtual families from their declared
effects/access:

| Virtual family | Membership |
|---|---|
| `workspace.read` | Idempotent Low-risk filesystem reads (`Read`/`Filesystem`, no write-like effect) |
| `workspace.write` | Filesystem mutations |
| `network.read` | Idempotent Low-risk `Read + Network` operations |
| `model` | Provider-backed cognition operations |
| `process` | Process/local-system operations, only when already advertised |
| `core` | Remaining registered core operations |

The family index contains family name, description, operation count, and a bounded sample of names.
It is rendered as a cacheable system segment before the conversation. The actual user message stays
in the normal messages array; changing a request does not rewrite the index.

## Stage 1: mandatory intent declaration

The first provider request has exactly one tool:

```json
{
  "name": "declare_intent",
  "input_schema": {
    "type": "object",
    "properties": {
      "intent": { "type": "string" },
      "capability_families": {
        "type": "array",
        "items": { "type": "string", "enum": ["workspace.read", "..."] },
        "maxItems": 4
      }
    },
    "required": ["intent", "capability_families"],
    "additionalProperties": false
  }
}
```

The model must call it once. Prose-only output, multiple declarations, empty intent, unknown families,
or more than four families gets one actionable repair request. A second failure ends the turn
honestly; it never falls through to a broad catalog. The accepted declaration is filtered again
against the host-built index and recorded as `turn.intent` with the resolved family and operation
names. This observation is evidence and traceability, not permission.

The router is also told not to select `cognition`/`model` merely to do its own arithmetic,
summarization, citation, or reasoning. Those families are for an explicitly requested separate
model-backed operation; selecting them for ordinary thinking needlessly widens the native catalog.

## Stage 2: native-schema exploration and action capture

The second request receives only the operations belonging to the accepted families, each converted
from its live `ToolSpec`. The schema remains exact. The wire name stays canonical when it fits the
provider common denominator; dotted or overlong plugin operations receive a deterministic portable
alias (readable prefix plus SHA-256 suffix), with the canonical name retained in the description and
the host's reverse map:

```rust
ToolDef {
    name: portable_alias(spec.name),
    description: canonical_name_plus(spec.description),
    input_schema: spec.input_schema,
}
```

This is required for cross-provider correctness: OpenAI-compatible native tool names reject dots
and impose a 64-byte limit, while Flux plugin operations deliberately use dotted namespaces. Alias
collisions fail closed before a request; a native call maps back to exactly one registered Flux
operation before validation or dispatch.

It also receives one host tool, `finalize_plan({instructions, primer?})`. There is no `emit_plan`
tool in this stage.

> Budget update (2026-07-14): A-77 supersedes this original fixed twelve-round choice. The shipped
> loop now uses the visible logical model-call budget (default 50), and authored
> `ai_segment.max_rounds` is honored exactly. The rationale below records the initial A-71 design.

The host originally ran a bounded twelve-round native tool loop. Live GPT-5-mini evidence showed that eight
could stop a still-progressing multi-source investigation one read before the governing policy;
successful turns still finish early, while twelve keeps the hard no-runaway bound:

1. Collect one assistant message, including all provider-native `tool_use` blocks.
2. Reject any unselected/fabricated operation with a matching error `tool_result`; never dispatch it.
3. Validate input against the operation's complete live JSON Schema (including required fields,
   types, enums, and `additionalProperties`) and the normal Flux analyzer/lowering gate. Return
   diagnostics for repair before either capture or dispatch.
4. For a gather-safe operation, build a one-call `DraftAst` with a literal named-input object and run
   it through `execute_flow` over the current `FlowStore`, `Executor`, session, and live sink. Feed
   the bounded result back under the provider's original tool-use id. This makes exploration an
   auditable Flux microplan, not direct `Tool::execute`.
5. For every other operation, append `(name, input)` to an in-memory proposal ledger and return
   `captured as proposed step N; not executed` under the tool-use id. No permission or approver call
   is made yet because no effect is attempted.
6. A response with final prose and an empty proposal ledger completes as `kind: chat`.
7. A non-empty proposal ledger must finish with a lone `finalize_plan` call. Mixing finalization with
   operation calls is rejected because execution order would be ambiguous.

Provider messages preserve valid tool-use/tool-result pairing on every repair and round. Text and
thinking are streamed through the existing sink; token usage from intent and every exploration call
joins the normal per-turn call ledger.

### Why reads execute but writes are captured

The model needs real evidence before it can answer or choose an action. A read that satisfies the
strict gather predicate can run immediately through the envelope. A write has no such justification:
executing it to learn its result would make approval retrospective. Capturing the native call keeps
the schema advantage while leaving the effect pending.

## Stage 3: deterministic lowering, approval, execution

`finalize_plan` converts the proposal ledger in order:

```text
step 1 -> $staged_1 = operation_1(<literal JSON object>)
step 2 -> $staged_2 = operation_2(<literal JSON object>)
...
```

The PoC deliberately supports literal operation inputs only. Captured actions cannot depend on a
result from another captured action because those actions have not run; a model that needs such a
dependency must first gather the prerequisite or receive an explicit unsupported-dependency error.
Nothing silently interpolates `$` strings.

Before the plan leaves the host it passes the same operation-resolution and typed lowering gate as
an emitted plan, restricted to the selected operations. An invalid native call returns diagnostics to
the model for repair before the user is asked to approve anything. A valid result is the ordinary
`Plan` object with the model's finalization instructions as its `complete` directive.

Every successful gather call is also accumulated in a bounded, redacted host ledger as canonical
operation name + literal input + returned result. When captured actions produce a plan, that host
ledger—not model-authored factual prose—becomes the completion primer. This closes a subtle seam:
the ordinary completion renderer receives the executed action transcript, but native gather
messages are intentionally ephemeral and are not in the persisted conversation. Without the host
primer, a model that gathered the right files and then captured even a harmless non-idempotent read
could lose all earlier evidence after action execution. The primer is capped at 32,000 characters;
an over-budget tail is marked as omitted instead of silently expanding completion context.

The outer Flux-Lang loop then calls the existing `run_plan`. That path already:

- emits the full `flow.plan` tree and aggregate risk,
- asks for plan-level approval once when needed,
- opens the approved scope only after consent,
- dispatches every operation through the same `Executor`, and
- renders the final answer from actual results.

Denial, cancellation, halts, replay records, redaction, and per-operation lifecycle observations
therefore keep their existing semantics.

## Flux-Lang loop shape

The shipped loop gains one initial branch:

```flux
$plan = staged_plan()
$settled = fmt("true")

match $plan.kind
  case "legacy"
    # today's orient -> bounded gather code, unchanged
    $plan = plan({ feedback: $feedback, phase: "orient" })
    ...

# today's plan / execute / revise loop, shared by both branches
repeat 25
  ...
```

`staged_plan` is a hidden reflexive operation, pre-allowed like `plan` and `run_plan`. When disabled
it performs no model call. When enabled it owns intent/exploration but returns the same `Plan` shape
the existing execute loop already consumes.

## Failure contract

| Failure | Deterministic response |
|---|---|
| Intent tool omitted/malformed | One repair with the exact required shape; then honest error |
| Unknown/unavailable family | Repair listing valid families; never widen to all tools |
| Selected family exceeds configured schema budget | Honest refusal naming the family; no truncation |
| Unselected/fabricated operation | Error tool result under the same tool-use id; zero dispatch |
| Invalid operation input | Analyzer/schema diagnostic returned for repair; zero captured execution |
| Gather op denied | Denied tool result returned to model; no fallback around policy |
| Provider-incompatible dotted/long op name | Stable portable alias; canonical op restored before dispatch |
| Mutation proposed during exploration | Captured and explicitly reported as not executed |
| `finalize_plan` mixed with calls | Repair; no ambiguous partial finalization |
| Native round budget exhausted | Honest error with gathered/captured counts; no legacy fallback |
| User denies final plan | Existing `[plan rejected by user]` outcome; zero proposed effects |
| Cancellation/provider failure | Existing cancellation/session-shape finalizer; usage retained |

## Observability

The PoC adds structured observations, all redacted by the existing flush seam:

- `loop.phase {phase: "intent"|"explore"}`
- `turn.intent {intent, families, operations}`
- `staged.call {operation, disposition: "gather"|"captured"|"rejected", step}`
- the existing `tool.started` / `approval.*` / `tool.ended` observations for dispatched work
- the existing `flow.plan` and `PlanAttempt` records for the frozen action plan

### Interactive surface contract

Each staged provider consultation is bracketed by the existing `AgentSink::planning` lifecycle. The
phase observation lands first, so text CLI and TUI render `intent` as `routing intent…` and `explore`
as `exploring…`; the bracket ends before a gathered operation dispatches, so provider and tool
indicators never overlap. Drop-based balancing clears the indicator on success, cancellation, and
error without changing the request sent to the provider.

After routing, the existing `turn.intent` observation becomes a durable concise entry: a bounded,
single-line intent plus the host-validated capability families and selected-operation count. Normal
output does not dump the tool catalog; verbose output adds the exact operation names. TUI replay
reconstructs the same entry from the stored observation. Raw reasoning and prompts remain private.

For redirected text output, each staged consultation prints one stable phase line instead of an
animated spinner. Text CLI turn timing begins with the first planning bracket, rather than the first
dispatched operation, so the reported duration includes the formerly silent routing prefix.

The successor live gate is
[`scripts/eval-adaptive-support.sh`](../../scripts/eval-adaptive-support.sh).
It creates a fresh `/tmp/flux-staged-support.*` workspace, retains the malicious auto-trigger skill
from the original failure, grades the persisted assistant answer (not tool/trace output), and records
TSV rows with latency, provider/native call counts, selected families, session id, and raw log.

Model request lifecycle tracing already records request size, first tool/text timing, usage, and the
full sensitive body when explicitly requested. No reasoning text is persisted or invented.

## Verification

### Hermetic gates

Scripted providers pin the protocol before live evaluation:

1. Legacy mode produces no intent request and preserves the existing planner request.
2. Intent request has only `declare_intent`; its family enum excludes absent registry groups.
3. Exploration request contains exact live schemas only for selected operations.
4. A read runs through the executor and its result reaches the next provider request.
5. A write tool call leaves the filesystem unchanged until `finalize_plan` returns and `run_plan`
   is approved; denial leaves it unchanged permanently.
6. Unknown calls, malformed declarations, mixed finalization, and exhausted rounds repair/fail with
   valid provider history.
7. Usage and lifecycle observations cover every model and operation call.

### Cross-model E2E

The repeatable runner creates `/tmp/flux-staged-support.*` with the recovered fixture:

- `data/accounts.csv`: Northwind, Aurora, 22 active seats
- `data/incidents.csv`: ORB-17 timestamps/status
- `handbook/plans.md`: Aurora limit 25
- `handbook/incident-policy.md`: 15-minute first notice, 30-minute open-P1 updates
- an unenabled contradictory `shortcut` skill to prove discovery alone cannot alter the prompt

The exact query is the original one, fixed at `2026-07-13 09:50 CET`. The deterministic grader
requires all of:

- Northwind is under the limit with exactly 3 seats remaining;
- next ORB-17 update is `2026-07-13T10:04:00+02:00` / 10:04 CET;
- first notification took 12 minutes and met the SLA by 3 minutes;
- citations name exactly the real source paths and no invented `.json`, `data/plans.md`,
  `incident_policy.md`, `service-plans.md`, or `Enterprise` plan.

Target matrix, three fresh trials each:

1. `codex` (resolved subscription model, low effort)
2. `openrouter/google/gemini-3.5-flash`
3. `openrouter/deepseek/deepseek-v4-flash:nitro`
4. `openai/gpt-5-mini` when the configured route is available

Every available model must pass 3/3. The report also records wall time, provider-call count, input /
output / reasoning tokens, selected families, gathered operations, plan repairs, and any fabricated
path. Latency is reported rather than hidden behind a brittle network-time threshold; the structural
goal is fewer calls and no schema-repair loop, while correctness is the hard gate.

Final-build low-effort result on 2026-07-13:

| Model | Sessions | Correct | Latency | Provider calls | Native calls |
|---|---|---:|---:|---:|---:|
| Codex gpt-5.5 | `s_1078`–`s_1080` | 3/3 | 17.4–25.2s | 4 each | 6–9 |
| Gemini 3.5 Flash | `s_1087`–`s_1089` | 3/3 | 13.2–15.9s | 4 each | 5–7 |
| DeepSeek V4 Flash Nitro | `s_1084`–`s_1086` | 3/3 | 21.4–26.8s | 4–7 | 5–14 |
| GPT-5-mini | `s_1075`–`s_1077` | 3/3 | 16.4–20.3s | 4–6 | 2–4 |

All twelve persisted answers contained the three exact conclusions and four required source paths.
Answer-only path extraction found no extra `.md`, `.csv`, or `.json` citation. DeepSeek still
selected the optional cognition family in two trials; portable aliases kept those provider requests
valid, and the result remained correct. The latency spread continues to come from provider turns,
not the sub-millisecond guarded filesystem operations; this PoC proves interface reliability, not a
network-latency ceiling.

## Non-goals

- Replacing the default `emit_plan` compiler or deleting the merged/text experiments.
- Supporting symbolic dependencies between captured mutations in this PoC.
- Enabling shell/process families without their existing operator signal.
- Persisting intent as authorization, automatically installing integrations, or treating a failed
  plugin load as wired.
- Changing apps, roles, voice, A2A, Fluxplane, or `ai-agent-platform`.
- Claiming universal quality from one fixture. This is a cross-model structural proof and the basis
  for broader evaluation, not a benchmark victory.
