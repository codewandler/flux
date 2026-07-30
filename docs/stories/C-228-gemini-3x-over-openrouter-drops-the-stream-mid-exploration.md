---
id: C-228
title: "Gemini 3.x over OpenRouter drops the stream mid-exploration, reproducibly"
pillar: Core
status: ready
priority: 18
epic: unattended-run-integrity
design: docs/designs/unattended-run-integrity.md
note: "gemini-3.6-flash and gemini-3.5-flash both die with `stream closed before completion` during exploration at 12-21k ctx; gemini-2.5-flash (no reasoning stream) survives the same workload — points at reasoning-delta handling on the Messages path, not at OpenRouter generally"
---

# Gemini 3.x over OpenRouter drops the stream mid-exploration, reproducibly

## Goal
`openrouter/google/gemini-3.x` is not usable for any sustained agentic run today: exploration dies
with `provider error: api_error: stream closed before completion`, reproducibly, part-way through a
task. Short turns survive, which is why this does not show up in a smoke test. The OpenRouter
provider is otherwise a headline capability — "every model over its native Anthropic Messages
endpoint" — so a whole vendor silently failing on long runs is a real hole in it.

Find out whether flux's Messages codec is mishandling Gemini's reasoning/thinking deltas, or whether
OpenRouter is genuinely closing the connection, and fix or document accordingly.

## Evidence
Same task (implement one story in a Rust worktree), same prompt, four runs:

| Model | Died at | Failure |
|---|---|---|
| `google/gemini-3.6-flash` | step 5, ctx 14.7k | `stream closed before completion` |
| `google/gemini-3.6-flash` | step 3, ctx 12.0k | `stream closed before completion` |
| `google/gemini-3.5-flash` | step 16, ctx 21.1k | `stream closed before completion` |
| `google/gemini-2.5-flash` | step 7, ctx 13.5k | upstream rate-limit (a different, unrelated failure) |

Control: `openrouter/anthropic/claude-haiku-4.5` and `openrouter/google/gemini-3.1-pro-preview` both
completed an equivalent read-heavy exploration task (3 and 11 steps) with no transport failure, so
this is not "OpenRouter is flaky" in general.

**The discriminating observation:** the two failing models are Gemini 3.x, which emit reasoning
content — visible as thinking blocks rendered mid-run. `gemini-2.5-flash`, which does not, completed
the same exploration workload cleanly. That makes reasoning-delta handling on the Messages path the
first place to look.

Ruled out already: prompt caching. `OpenRouterProfile::quirks_for`
(`crates/flux-providers/src/openrouter.rs:55-57`) gates `cache_control` on a `anthropic/` vendor
prefix, so no cache breakpoints are sent to a `google/` slug. That hypothesis is dead — do not
re-run it.

## Acceptance
- [ ] Determine, with captured wire evidence, whether the stream is closed by OpenRouter or whether
      flux's codec ends it — e.g. an unhandled/misparsed reasoning envelope on the Messages SSE path.
      State the finding either way; "could not reproduce" is an acceptable outcome only with the
      reproduction attempt described.
- [ ] If flux is at fault: fix it, with a **failing-first test** driven from a recorded envelope in
      the codec's corpus (`crates/flux-providers/src/envelope_corpus.rs` is the existing home for
      exactly this).
- [ ] Check this against the **stream-resilience invariant**: "codecs skip + count an unparseable
      SSE/frame envelope (surfacing `Chunk::StreamDiagnostic` at stream end) instead of
      `?`-propagating". A hard `api_error` that kills the turn is the shape that invariant exists to
      prevent, so establish whether this path is a genuine transport close or an invariant regression
      — and if the envelope is being rejected rather than skipped, that is the bug.
- [ ] If OpenRouter/Google is at fault: document the limitation where a user choosing a model will
      see it, rather than leaving it to be discovered mid-task.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — found while dogfooding `flux run` as a headless sub-agent implementor over OpenRouter.
  Four runs, three distinct Gemini versions; the table above is the raw result set.

## Notes
- Seams: `crates/flux-providers/src/openrouter.rs`, `crates/flux-providers/src/messages/`,
  `crates/flux-providers/src/envelope_corpus.rs`.
- `crates/flux-providers/clippy.toml` already bans bare `serde_json::from_*` in this crate precisely
  so a codec cannot hard-fail on an unexpected envelope — worth checking whether the reasoning path
  has a route around that ban.
- This bug is what made [C-227](C-227-no-automatic-resume-on-transport-class-provider-failure.md)
  findable. Fixing this one narrows the blast radius; fixing C-227 is what makes flux survive the
  next provider that does this.
