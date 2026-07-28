# Design: NDJSON agent protocol — drive and observe a turn over stdio

**Status:** in progress 2026-07-28 · **Pillar:** Agent · **Stories:** [C-160](../stories/C-160-ndjson-agent-protocol.md)

## Why

Today the only machine-readable ways to drive flux are `flux-sdk` (Rust), the HTTP server, and A2A —
all heavier than "pipe a subprocess". A CI job, an editor extension, or a downstream service that
wants to drive one flux turn and observe its structure has to either link Rust or scrape
human-formatted prose off stdout. `AgentFlags` (`crates/flux-cli/src/args.rs:77-250`) carries no
output-format flag at all; `--format json` exists on exactly one subcommand, `flux review`.

The two hard halves already exist, so this is a projection and a schema, not new machinery:

- **The event stream.** Every turn already reports its structure in real time through
  [`AgentSink`](../../crates/flux-flow/src/agent_sink.rs) — the same trait `CliSink`
  (`crates/flux-cli/src/rendering.rs`) renders from for the human terminal UI — plus the durable
  `flux-events` log for after-the-fact projections (`flux replay`, `flux export`, C-132).
- **Mid-turn steering.** A-94 shipped `SteeringQueue` (`crates/flux-flow/src/steering.rs`): a
  surface pushes text, the adaptive loop drains it at the head of the next planner round and folds
  it into the model conversation as an attributed block, without disturbing an in-flight op or a
  pending approval. The TUI is the only current caller.

## Constraint this must not lose

Every emitted line crosses a trust boundary. Redaction must be enforced **on the protocol itself**,
not assumed from a renderer that happens to be careful. Concretely: `Executor::dispatch`
(`crates/flux-runtime/src/lib.rs`) redacts a `ToolResult`'s `content`/`view` (both the cache-hit path
and the fresh-execution path) before it ever reaches `AgentSink` — but it does **not** redact the
model-proposed *input* arguments of a tool call (`sink.tool_call(name, &input)`), because that value
never flows through dispatch's result-scrubbing step at all. A secret assembled into a tool
argument (rather than echoed back in a result) is exactly the shape the human terminal renderer has
never had to worry about (it prints tool args too, but a terminal is not a wire format anyone parses
mechanically) — the NDJSON protocol makes that gap load-bearing, so it gets its own scrub.

## Non-goals (v1)

- `--stream-json-thinking` (Amp's reasoning-block flag). No acceptance item names it; add a
  `thinking_delta`-sourced line in a follow-up story if a consumer needs it.
- Projecting every `Observation` kind that exists today (`loop.phase`, `model.call`,
  `skill.activated`, `context.compacted`, …). v1 ships exactly the facts the acceptance names —
  turn start/end, plan, per-op dispatch+result, approval request/decision, usage/cost, error — each
  traced to a concrete existing `AgentSink`/`Observation` source below. A later revision may add a
  dedicated line per additional kind; the protocol must never gain a fact with no upstream source
  (this is the "projection, not a second source of truth" acceptance item).
- Interactive approval **decisions relayed through the input NDJSON stream**. `--stream-json-input`
  reads stdin itself (for the next turn's message and for `steer` lines), so it cannot also be the
  interactive `StdinApprover`'s prompt channel without the two readers racing each other on the same
  fd. v1 requires `--yes` together with `--stream-json-input` (a clear startup error otherwise) —
  approval is auto-allow for that mode, not silently downgraded. Plain `--stream-json` (no stdin
  reading) is unaffected: an approval prompt still reads a line from stdin and writes to stderr
  exactly as it does today, so it composes with `--stream-json` even without `--yes`.
- A compatibility promise. See "Versioning" below — this is deliberately decided, not left
  ambiguous.

## Versioning

**v1 is explicitly unstable.** Every line carries `"v": 1`. The `type` tag set is open and will grow
(new variants are additive); a consumer must skip a `type` it does not recognize rather than error —
but nothing about field *shapes* within a known `v` is promised stable yet. The first release that
wants to make a compatibility promise bumps `v` and freezes the shape it ships; until then, treat
`--stream-json` as a preview surface (documented as such on the website page).

## Line vocabulary

One `\n`-terminated JSON object per line on stdout, flushed after every line (so `| jq` sees it
live, and so a piped/non-tty stdout — which is block-buffered by default — doesn't sit on a line
until a buffer fills). Every variant is internally tagged on `type` and carries `v`:

| `type` | Source (exactly) | Fields |
|---|---|---|
| `turn_start` | CLI-known facts before the turn begins | `session`, `model`, `input` |
| `plan` | `Observation{kind: "action_batch.proposed"}` (`loop_host.rs`'s `approve_batch`, forwarded live via `SharedSink` — the adaptive loop's proposed action batch **is** its plan for the turn; confirmed empirically against a live `-m mock` run, see below) | `session`, `data` (the observation's own payload — `batch_id`/`actions`/`risk`/`batch`, passed through) |
| `tool_call` | `AgentSink::tool_call` | `session`, `name`, `input` |
| `tool_result` | `AgentSink::tool_result` + the immediately preceding `AgentSink::tool_timing` | `session`, `name`, `is_error`, `content`, `view`, `duration_us` |
| `approval` | `Observation{kind: "approval.requested" \| "approval.approved" \| "approval.denied"}` (`loop_host.rs`'s `approve_batch`, forwarded live via `SharedSink` — confirmed the batch-approval path, not just recorded to the durable evidence log) | `session`, `phase` (`requested`/`approved`/`denied`), `data` (the observation's own payload — `scope`/`batch_id`/`actions`/`risk`/`wait_us`; no dedicated `tool` field, since the batch-level approval this fires from has no single-tool subject — a batch can hold several) |
| `steered` | `Observation{kind: "turn.steering"}` | `session`, `messages` |
| `turn_end` | `AgentSink::turn_end` | `session`, `answer`, `usage`, `cost_usd` |
| `error` | `run_turn`'s `Err(_)` | `session`, `message` |

No line type here invents a fact: each row's Source column is a real, already-existing call site.
Adding a new line type in the future means adding a match arm over an existing `AgentSink`/
`Observation` source, never a new field the engine doesn't already produce — this is the acceptance
item "a new event type cannot be added to the protocol without existing upstream" made concrete.

A note on `error`'s narrowness: a **model/flow-level** failure inside a turn (the adaptive loop
couldn't complete, a compaction failed, …) is NOT a distinct signal on `AgentSink` today — the engine
(`FlowEngine::turn_terminal`, `crates/flux-flow/src/engine.rs`) converts it into an ordinary,
apologetic answer text ("I couldn't complete the turn — …") and still calls the normal
`text_delta`/`turn_end` pair; there is no sink-visible `outcome: "error"` flag to key on, and
inventing one by pattern-matching the answer text would violate the "projection, not
reinterpretation" rule. So that case surfaces in v1 as an ordinary `turn_end` (its `answer` explains
the failure in prose) — only a failure that aborts `run_turn` itself *before* it reaches that
conversion (session/lifecycle setup, e.g.) produces the dedicated `error` line. Checked and ruled
out as a v1 source: a `flow.halt` observation kind exists in the human/TUI renderers
(`rendering.rs`, `flux-tui`) purely defensively — nothing in `flux-flow`/`flux-runtime` production
code emits one today, so it is not listed as a source (would be inventing a call site that doesn't
exist); the arm can be added here for free the day something does emit it.

Same caution applied to `plan`: `flow.plan` is the kind `CliSink`/the TUI actually render (they
skip `action_batch.proposed` on purpose — see the table row above), so it was the obvious first
guess. Grepping the whole tree turned up no production emitter for `flow.plan` either — only the
renderers' defensive match arms and their own unit tests construct one by hand. A live `-m mock` run
(`flux run --stream-json --yes -m mock "write a quick note"`) confirmed what actually reaches the
sink: `action_batch.proposed` then `approval.requested`/`approval.approved`, never `flow.plan`. So
`plan` is sourced from `action_batch.proposed` instead — a real, live-verified call site — and
`flow.plan` is not listed as a source anywhere in this document.

`data`/`content`/`view`/`answer`/`input` fields are `Value`/`String` and are **not** re-interpreted —
they carry through whatever the engine already computed, so the human (`CliSink`) and machine
(`StreamJsonSink`) renderers stay two views over one set of facts rather than diverging in what they
consider "the plan" (they now literally read the same observation, just chosen not to render it
identically).

### Redaction

Every line is serialized to its final JSON string, THEN passed through a `Redactor::redact` pass,
THEN written. The sink gets the SAME `Redactor` the executor dispatches through —
`Executor::context()` (`crates/flux-runtime/src/lib.rs`) is already a public accessor onto
`ToolContext`, whose `redactor` field is public; `agent.executor.context().redactor.clone()` shares
the exact value store `build_agent_with` seeded (provider credential env vars + `FLUX_SECRET`) and
picks up anything a plugin registers mid-run through the `credential` capability path too (the
store is behind an `Arc<Mutex<…>>`, so any clone sees every registration). This is the same pattern
`loop_host.rs`'s `approve_batch` already uses to redact a proposed batch before putting it in an
observation (`executor.context().redactor.redact(...)`) — not a new access path.

This redaction pass is still a genuinely SEPARATE scrub from `Executor::dispatch`'s, not a redundant
one: dispatch redacts a `ToolResult`'s `content`/`view` before it reaches any sink, but never the
model-proposed *input* arguments of a tool call — `sink.tool_call(name, &input)` receives whatever
the flow interpreter passed in, unredacted, because that value never flows through dispatch's
result-scrubbing step. `tool_call.input` is the gap this protocol-level pass closes that no surface
closed before (the human terminal renders the same unredacted input, but a terminal is not a
mechanically-parsed wire format, so nothing depended on it being scrubbed there).

### Input framing (`--stream-json-input`)

Requires `--yes` (see Non-goals). Accepts the same one-object-per-line framing on stdin:

```json
{"text": "next thing to do"}
{"text": "actually, focus on the auth module instead", "steer": true}
```

`steer` defaults to `false`. Routing is a pure function of one bit of state — whether a turn is
currently in flight:

- `steer: true` **and** a turn is running → pushed onto the engine's `SteeringQueue`
  (`engine.set_steering(...)`), consumed at the next planner round through the existing A-94 path;
  its consumption is echoed back out as a `steered` line (the engine's own `turn.steering`
  observation, not a synthesized one).
- Anything else (no `steer`, or `steer: true` with no turn running) → queued as the next ordinary
  turn's input, processed strictly in arrival order once the current turn (if any) finishes.

The no-turn-running `steer: true` case falls back to "next turn" rather than being dropped or stuck:
there is nothing running to steer, so honoring the arrival as a fresh turn is the only outcome that
doesn't silently lose the line. This mirrors the TUI's own idle-time behavior
(`SteeringQueue::pop_front`'s doc comment: "leftovers after a turn finishes become ordinary follow-up
turns").

The process keeps reading stdin and running turns, one at a time, until stdin closes (EOF) and no
turn/queued input remains, then exits.

## Where this lives

- `crates/flux-cli/src/stream_json.rs` (new): `ProtocolLine`, the `StreamJsonSink` (`AgentSink` impl
  + the redaction-on-write boundary), the single-turn runner, and the stdin-driven multi-turn runner
  with its pure input-routing function (unit-tested without a live model call).
- `crates/flux-cli/src/args.rs`: `--stream-json` / `--stream-json-input` added directly on
  `Commands::Run` (not on the shared `AgentFlags`, which also flattens into `tui`/`fork`/`app run` —
  keeping the flags Run-only avoids putting a machine-output mode on surfaces that have no
  "stdout is a stream" story yet, e.g. the TUI owns the whole terminal).
- `crates/flux-cli/src/dispatch.rs`: the `Commands::Run` arm routes to the new runners instead of
  `run_prompt` when either flag is set.
- `website/docs/agent/cli.md` (or a new `stream-json` reference page linked from it): the line table
  above plus a worked `jq` example.

## Testing

- Subprocess (black-box, like `crates/flux-cli/tests/mock_smoke.rs`, landed as
  `crates/flux-cli/tests/stream_json_smoke.rs`): `flux run --stream-json -m mock --yes <prompt>` →
  every stdout line parses as JSON with `type`+`v`, and the sequence starts `turn_start`, ends
  `turn_end`, and includes `plan`, `approval`, `tool_call`, `tool_result` (verified live, not just
  assumed from the table above).
- Subprocess: `--stream-json-input` fed two plain lines over stdin → two `turn_start`/`turn_end`
  pairs in one process (the "multi-message conversation" half of the acceptance); a companion test
  feeds a `steer: true` line as the very first (no turn running) and asserts it becomes an ordinary
  turn with no `steered` line — the idle-fallback case.
- Unit (pure, no live turn): the input-routing function, given `(turn_in_flight, steer)`, asserted
  against all three combinations that matter — steer-while-running → `SteeringQueue`, plain line →
  next-turn channel, steer-while-idle → next-turn channel (the "Test covers both" acceptance item).
- Redaction: register a secret, drive a mock turn whose tool-call *input* (not result) carries that
  secret literal (the gap this design calls out above), assert it never appears in captured stdout —
  both as a pure unit test of the write/redact path and as a subprocess test through the real mock
  provider (`FLUX_MOCK_TOOL_INPUT` + `FLUX_SECRET`).
