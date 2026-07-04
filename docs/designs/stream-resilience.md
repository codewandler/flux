# Design: Stream resilience — provider bytes never kill a turn

**Status:** designed 2026-07-04 · **Pillar:** Agent · **Stories:** [A-33](../stories/A-33-stream-decode-backstop.md) · [A-34](../stories/A-34-openai-wire-envelope-tolerance.md) · [A-35](../stories/A-35-messages-wire-envelope-tolerance.md) · [A-36](../stories/A-36-bedrock-frame-decode.md) · [A-37](../stories/A-37-parse-enforcement.md) · [A-38](../stories/A-38-planner-trace.md)

Wave 2 of [parse resilience](parse-resilience.md) (that epic stays closed — its scope was tool-args;
this one is everything around them).

## Why

The parse-resilience epic (A-30/A-31/C-31/A-32) hardened the **tool-args** layer, and s_368-class
sessions still die. Four user-pasted kills so far, all
`runtime error: step plan failed: serialization error: …` — the last two (2026-07-04, columns 5224
and 2394) from a pre-A-32 binary, but the class survives A-32 because the SSE **envelope** parses
were never touched:

| site | what | failure |
|---|---|---|
| `flux-providers/src/openai.rs:269` | chat SSE chunk envelope | bare `?` → `Error::Serde` → the literal `serialization error:` |
| `flux-providers/src/openai.rs:870` | Responses SSE event envelope | same |
| `flux-providers/src/messages/mod.rs:381` | Messages SSE event envelope | `map_err(…)?` → `Error::Provider` — same fatality, different string |
| `flux-providers/src/bedrock.rs:236` | AWS event-frame chunk payload | same |

And the failure is *structural*, not local to any one site:

1. **Mid-stream errors are never retried.** `NativeProvider::stream`'s retry loop
   (`flux-provider/src/lib.rs:407`) covers only the connection attempt; once the stream is open, the
   first `Err` item is final (documented at lib.rs:275).
2. **Partial work is discarded.** `stream_blocks` (`flux-flow/src/compile.rs:1033`) `?`-propagates
   the first bad chunk, dropping every accumulated block *and* the call's usage (the accumulate at
   compile.rs:556 is skipped) — the C-31 problem, one level down.
3. **The planner's reject/repair loop never engages.** A-31's `last_reject` feedback machinery only
   sees errors that reach the emit_plan decode; envelope errors `?`-escape `compile_turn` entirely.
4. **Nothing stops the next regression.** No lint or corpus test guards the invariant; every new
   codec or parse site re-introduces the bug class (A-32's fix itself was a port of a repair the
   Messages wire had had for days).

Also found during design: the Messages wire's `StreamEvent` enum has no unknown-variant tolerance —
a *well-formed* vendor event with a new `type` kills the stream today, same as garbage.

## Approach

Enforce the invariant — **model/provider-originated bytes must never kill a turn** — at three
layers, so no single unhardened parse site can ever be fatal again:

- **[A-33](../stories/A-33-stream-decode-backstop.md) — planner backstop (lands first, defines the
  seams).** New `flux_core::Error::StreamDecode(String)` classifies "provider bytes failed to
  decode" distinctly from transport (`Provider`) and workspace-wide `Serde`. New
  `Chunk::StreamDiagnostic { dropped_frames, detail }` (non-serialized; all consumers match
  non-exhaustively) lets codecs report tolerated drops. `stream_blocks` returns usage alongside its
  result (C-31 pushed down); in `compile_turn_inner`, decode-class errors (`StreamDecode`, and
  `Serde` in this stream context — any serde error there is provider-originated by construction)
  accumulate usage, set `last_reject` ("the provider stream broke while decoding the model's
  output: …"), and `continue` — a decode failure costs one step of the existing `max_steps` budget,
  never the turn. Non-decode errors (`Api`/`Http`/transport) keep propagating: availability is a
  different class with its own connection-level retry.
- **[A-34](../stories/A-34-openai-wire-envelope-tolerance.md) /
  [A-35](../stories/A-35-messages-wire-envelope-tolerance.md) /
  [A-36](../stories/A-36-bedrock-frame-decode.md) — per-codec envelope tolerance.** Per-frame
  policy: **skip + count + end-of-stream diagnostic**. An unparseable `data:` frame is skipped and
  counted; at stream end, `dropped > 0` yields one `StreamDiagnostic` chunk (plus `tracing::warn!`
  for consumer-less paths). Not end-with-what-we-have: mid-stream vendor/keep-alive junk must not
  truncate a good tail — and the SSE decoder only surfaces *complete* events, so a provider dying
  mid-emission never delivers the partial frame anyway; a complete frame carrying truncated JSON is
  by definition the last meaningful thing a broken upstream sent, so skipping it and letting the
  byte stream end naturally is equivalent where it matters. **Guardrail:** frames that parse into
  *declared* provider errors (`response.failed`, `StreamEvent::Error`, bedrock `exception`) keep
  their fatal semantics, pinned by `*_stay_fatal` tests — tolerance is for unparseable bytes only,
  or real outages would be masked as empty turns. Bedrock's integrity failures (CRC mismatch,
  header overrun, truncated tail) stay errors but are reclassified `StreamDecode`, so the A-33
  backstop retries the call instead of killing the turn.
- **[A-37](../stories/A-37-parse-enforcement.md) — structural enforcement.** A crate-local
  `crates/flux-providers/clippy.toml` bans `serde_json::from_str/from_slice/from_value/from_reader`
  (disallowed-methods) — tolerant helpers live in one allow-listed module; the existing
  `clippy --workspace -D warnings` gate enforces it with zero CI changes. Plus a malformed-envelope
  corpus test (`#[cfg(test)] mod envelope_corpus`): truncate valid fixture streams at every byte
  offset, inject junk frames, corrupt single frames — assert no `Err` ever escapes any codec
  (bedrock: any `Err` is `StreamDecode`). The lint stops new bare parse sites at merge time; the
  corpus proves the runtime invariant actually holds.
- **[A-38](../stories/A-38-planner-trace.md) — `FLUX_PLANNER_TRACE=1`.** The parse-resilience
  residual, promoted now that the backstop makes failures *quieter* (retries instead of crashes):
  env-gated per-step stderr trace (step, stop reason, tool names, reject/decode text, dropped-frame
  diagnostics) so the next s_360/s_368-class forensic needs zero ad-hoc instrumentation.

Sequencing: A-33 first (both seams), then A-34/A-35/A-36 in parallel (disjoint files) with A-38
after A-33 (shares compile.rs), A-37 last (lints the final surface).
[C-34](../stories/C-34-openrouter-reported-cost.md) (provider-reported cost, own design:
[openrouter-reported-cost](openrouter-reported-cost.md)) shares files with A-34/A-35 and runs
before the wave.

## Alternatives considered

- **End-stream-on-first-bad-frame** (keep what we have, stop): truncates good tails on interleaved
  junk; strictly worse than skip+count given complete-event SSE framing.
- **Classify EOF-shaped errors as stream-end, junk-shaped as skip:** buys nothing (see framing
  argument above) and misclassifies mid-stream junk that happens to look unterminated.
- **String-marker on `Error::Provider` instead of a new variant:** marker-matching is exactly the
  whack-a-mole this epic ends; a variant is compiler-checked at every consumer.
- **Retry inside `NativeProvider::stream` on decode errors:** wrong layer — the provider can't know
  whether a fresh identical call is safe or affordable; the planner's step budget already prices
  exactly that decision (and A-31/C-31 give it feedback + accounting).
- **Corpus-only (no lint) or lint-only (no corpus):** the lint can't catch a tolerant-looking helper
  that still `?`-propagates; the corpus rots when a new codec ships without registering. Both are
  cheap; take both.

## Risks & open questions

- **Silent-loss trade-off:** a dropped mid-stream frame carrying a text delta yields subtly
  shortened prose. Accepted vs. certain total turn loss; mitigated by the diagnostic chunk + warn.
- **Crate-local clippy.toml under `--workspace`** is the one speculative mechanism — A-37 verifies
  it fires before relying on it (fallback: flux-codegate source-scan test, same gate slot).
- **`Chunk` variant ripple:** all verified consumers match non-exhaustively, but flux-sdk/a2a
  re-exports weren't exhaustively audited — compiler-led mechanical fixes budgeted in A-33.
- **Decode retries cost real tokens** (fresh full-prompt call, ~37k input in s_360-class sessions):
  bounded by the existing `max_steps` budget and correctly accounted thanks to C-31 + A-33's
  usage-on-error plumbing.
- Pre-existing, out of scope: an OpenRouter mid-stream `{"error":…}` frame deserializes as an
  all-default `ChatChunk` and is *already* silently ignored today (openai.rs:269 + all-optional
  fields). Noted for a future story if it bites.

## Acceptance / done

Union of the six stories: a mid-stream decode failure costs one planner step and the turn survives
(usage recorded); all three SSE codecs skip unparseable frames and surface a diagnostic while
declared provider errors stay fatal; bedrock integrity failures classify as `StreamDecode`; the
clippy ban + envelope corpus are green in the gate and a bare `serde_json::from_str` added to
flux-providers fails clippy; `FLUX_PLANNER_TRACE=1` emits per-step traces. Full gate green (build,
test, clippy `-D warnings`, fmt, codegate — both workspaces).
