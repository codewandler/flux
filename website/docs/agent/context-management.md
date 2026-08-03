---
title: Context management
description: "What fills a session's context, what flux does when it grows too large, what compaction keeps and replaces, and which controls bound it."
---

# What happens when the conversation gets long

Every turn re-sends the whole conversation to the provider. A long session therefore costs more each
turn, and eventually reaches the model's context limit. flux's answer is **compaction**: past a
character budget, the older part of the history is summarized into one message and the rest is kept
verbatim.

This page is about the *session transcript* — the thing that grows as you talk. Two neighbouring
pages answer different questions and neither covers this one:

| Page | Question it answers |
|---|---|
| [Project context](./project-context.md) | What flux tells the agent about your repository *before* the first turn |
| [Context packs](../language/context-packs.md) | How an authored Flux-Lang flow decides what a *single model call* may see (`ctx`) |
| **This page** | What happens to the conversation as it grows, and what you can do about it |

## What fills the context

Each model call carries, in order:

1. **The stage prompt** — flux's own instructions for the typed stage being run (intent detection,
   exploration, presentation). Fixed.
2. **The harness context package** — Flux's embedded runtime protocol, an optional behavior profile
   (`coding` or `general`), authored role/persona instructions, then assembled
   [project context](./project-context.md). Repository policy and workspace snapshots carry explicit
   provenance and are fixed for the session.
3. **Skill bodies** — the full text of every explicitly enabled [skill](./skills-and-roles.md), plus
   a name-and-description listing of the rest. Injected every turn.
4. **Tool specs** — the schemas of the operations currently visible to the model.
5. **The conversation** — every user message, assistant reply, tool call and tool result so far.

Items 1-4 are roughly constant for the session. **Item 5 is the one that grows**, and it is the only
one compaction touches. If your context feels large from the first turn, the cause is a long
conventions file, coding profile, or a large enabled skill, not the transcript — see
[project context](./project-context.md) for how to move repo-wide prose into path-scoped fragments
that load only when relevant.

## What happens when it fills

At the start of every turn, before doing any work, flux measures the session's history and compacts
it if it is over budget. The measure is the **sum of each message's serialized JSON length** — so it
includes roles, tool-call ids, JSON escaping and full tool-result bodies, and is meaningfully larger
than the prose you can see.

Compaction does nothing at all unless every one of these holds:

- the threshold is not `0` (`0` disables compaction entirely),
- the history has **at least four messages**,
- the measured size is **over** the threshold, and
- there is a legal split point — the kept tail must not begin with a `tool_result` whose `tool_use`
  would be summarized away, nor with a user message. If no such split exists, compaction is skipped
  rather than writing a history the provider would reject.

When it does fire, flux makes **one extra provider call**, using the session's own model, asking it
to summarize the older messages into "durable facts, decisions, and open threads", preserving file
paths, names and numbers. That call is billed to the turn and appears in
[cost accounting](./cost.md) like any other.

## What is kept and what is replaced

The result is a new history: **one synthetic message prefixed `[summary of earlier conversation]`,
followed by the most recent messages verbatim.** The tail is at least the last two messages, and more
when the split had to move earlier to keep a tool-call pair intact.

Everything before that boundary is **replaced by the summary** in what the model sees from then on.
Concretely, that means:

- **Detail is gone from the model's view.** Only what the summarizer chose to carry forward survives.
  Exact file contents, full tool output and precise wording of earlier turns are not recoverable by
  asking the agent about them.
- **Tool results are summarized without being read.** The summarizer is shown only the *text* of the
  older messages. Tool-call inputs and tool-result payloads counted toward the threshold but are not
  part of what it sees — so a session dominated by large tool outputs is compacted on the basis of
  the prose around them.
- **The summary is a message, not metadata.** It occupies the first slot of the history and is
  re-measured on later turns like anything else, so a very long session can compact more than once.

:::warning Compaction replaces the live history
This is the part users are most often surprised by. The replacement is durable: it is written to the
session's event log as a `Compacted` event carrying the new messages, and from that point the
conversation *is* those messages.
:::

## What it means for the session afterwards

flux's event log is append-only, and compaction is no exception — writing the `Compacted` event *is*
how the history gets replaced, so a compaction can never happen without being recorded. The
superseded messages are never deleted. So:

- **The model sees the post-compaction history.** Every subsequent turn, and anything reading the
  live conversation (the REPL, the server's session routes, `flux fork`), starts from the summary.
- **The log still holds both.** The original messages remain on the stream, and the `Compacted` event
  records what replaced them — so the record answers both "what is the history now" and "what was
  replaced". Nothing is silently dropped.
- **[`flux replay`](./time-machine.md) is unaffected.** Replay re-executes a run from its recorded
  plans and operation cassette, which are separate events that compaction never touches.
- **`flux export` shows the pre-compaction turns.** The export renders per-turn prompts and answers
  from the turn log, not from the conversation projection — so it shows the original turns, including
  ones the model can no longer see, and does not currently mark that compaction occurred.
- **`flux fork` inherits the compacted history**, because a fork seeds the child from the parent's
  live conversation.

You will see a dim `⊙ context compacted (N → M messages)` line in the CLI when it happens, and a
`◇ context compacted` marker in the TUI transcript. Neither surface shows the current session size or
how close it is to the threshold.

## How to control it

Every value below is specified once, in the [configuration reference](../reference/config.md).

| Control | What it bounds |
|---|---|
| [`FLUX_COMPACT_CHARS`](../reference/config.md) | The compaction threshold, in characters of serialized history. `0` disables compaction. Default 48,000. |
| [`FLUX_TOOL_OUTPUT_CAP`](../reference/config.md) | How much of a *single* tool result is kept in the transcript. `0` disables trimming. Default 20,000 characters. |
| [`FLUX_TURN_TOKEN_BUDGET`](../reference/config.md) | A ceiling on cumulative model tokens for one turn (also `--turn-budget`). A spend guard, not a context guard — it fails the turn rather than shrinking anything. |

```bash
flux run "…"                          # compaction armed at the default
FLUX_COMPACT_CHARS=0 flux run "…"     # never compact (may hit the provider's context limit)
FLUX_COMPACT_CHARS=120000 flux run "…" # compact later, at a larger transcript
```

In the REPL, **`/compact`** runs the same compaction check immediately instead of waiting for the
next turn; it does not force a session below the threshold to compact. The result distinguishes a
real rewrite (`context compacted (N → M messages)`) from an unchanged context, disabled compaction,
and cancellation.

The threshold can also be set per agent, for served and SDK agents, which is the case where it
matters most: those bind a conversation to one long-lived session. The precedence is **per-agent
setting → `FLUX_COMPACT_CHARS` → the default**. On the SDK, that per-agent setting is
`ClientBuilder::with_compaction`; on the served path it is `compact_threshold_chars` in the agent's
settings. A per-agent `0` disables compaction for that agent explicitly.

:::info A typo'd value behaves differently per surface
The CLI prints a warning and falls back to the default if `FLUX_COMPACT_CHARS` is not a number,
because silently reverting would contradict the `0`-disables contract. On the served path the same
unparseable value is ignored without a warning.
:::

## What flux does not manage

If you arrive from another harness, these are the things it is reasonable to assume exist and which
**flux does not do**. None of them is a hidden feature to be found; they are absent.

- **The threshold deliberately does not consult the model's context window.** The fixed history budget
  is applied identically to every model. The transcript is only one part of a request: harness
  instructions, project context, skills, tool schemas and stage prompts consume headroom too, and
  their size is not implied by a model's nominal window. flux also cannot maintain trustworthy
  window metadata for every unknown, local or custom model id. A fixed 48,000-character cap (roughly
  12k tokens) bounds the growing transcript's repeated cost and latency on every provider. If a known
  model and workload can retain more safely, raise `FLUX_COMPACT_CHARS` or the per-agent override;
  small-window deployments can lower it.
- **Compaction rarely fires in practice.** A sweep of a 112,114-event local store found *zero*
  compactions: most sessions are one-shot runs, and the average multi-turn session was under 10% of
  the threshold. Expect it only in a genuinely sustained session — roughly 35-40 substantive
  messages. That measures a workload, not a ceiling; a heavy interactive session will reach it.
- **No automatic summarization outside compaction.** Nothing condenses tool output, distils earlier
  turns, or maintains a running session summary in the background.
- **No retrieval over conversation history.** flux never searches your earlier turns and reinjects
  the relevant ones. Semantic retrieval exists, but only over an explicitly configured
  [datasource](./datasources.md) — never over the transcript.
- **No per-tool context budgets.** `FLUX_TOOL_OUTPUT_CAP` is a single global per-result cap, not a
  per-operation allowance, and there is no budget shared across a turn's tool calls.
- **No message-count cap.** Nothing limits how many messages a history may hold; the only count
  involved is the four-message floor *below* which compaction will not act.
- **No tiered compaction.** Everything before the boundary collapses into one summary in a single
  step. There is no re-summarizing of an existing summary into a shorter one, and no multi-level
  digest.
- **No cap on project context or skills.** Conventions files and enabled skill bodies enter the
  prompt in full, every turn. Keeping them small is your job — use
  [guidance fragments](./project-context.md#guidance-fragments) for rules that only apply sometimes,
  and [skills](./skills-and-roles.md) for reference material the agent should load on demand.

Explicit, budgeted control over what a model call sees does exist — but at the flow level, not the
session level: see [context packs](../language/context-packs.md).

## Related docs

- [Project context](./project-context.md) — what enters the prompt before the conversation starts.
- [Context packs](../language/context-packs.md) — `ctx`, the authored per-call budget in Flux-Lang.
- [Skills and roles](./skills-and-roles.md) — on-demand reference material instead of always-loaded prose.
- [The agent loop](./agent-loop.md) — where the compaction check sits in a turn.
- [Cost](./cost.md) — where the summarization call shows up.
- [Time Machine](./time-machine.md) — replay, fork and diff over a recorded session.
- [Configuration](../reference/config.md) — the authoritative values for every knob above.
