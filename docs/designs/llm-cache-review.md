# Design: LLM cache review — prompt-cache correctness for the `claude` and `codex` providers

**Status:** proposed · **Pillar:** Core (one Agent story) · **Stories:** see it clearly first
([C-133](../stories/C-133-cache-efficiency-telemetry-and-harness.md),
[C-139](../stories/C-139-tui-header-cache-tiers.md),
[C-140](../stories/C-140-tui-usage-overlay.md)), then claude
([C-134](../stories/C-134-conversation-tail-cache-breakpoint.md),
[C-135](../stories/C-135-one-hour-cache-ttl-stable-prefix.md),
[A-95](../stories/A-95-freeze-advertised-tool-set-per-turn.md)), then codex
([C-136](../stories/C-136-codex-prompt-cache-key.md),
[C-137](../stories/C-137-codex-instructions-volatile-tail.md)), then close-out
([C-138](../stories/C-138-cache-layout-contract-and-live-ab.md)).

## Baseline — measured 2026-07-28

`flux usage --harness flux --last 14d` over the local event store (62 sessions, 813 calls, 28.8M
tokens, $155.85). **Hit % is `cache read / ctx`.** These numbers are trustworthy: `flux usage` builds
records from per-call `CallUsage` events (`crates/flux-cli/src/usage.rs:767-793`), one per model
call, so unlike the live displays it is not biased to a turn's last round.

| model | calls | ctx | cache read | cache write | hit % |
|---|---:|---:|---:|---:|---:|
| `claude/claude-fable-5` | 208 | 11.0M | 3.2M | 679.1k | 29% |
| `codex/gpt-5.5` | 223 | 7.8M | 2.4M | — | 31% |
| `claude/claude-opus-4-8` | 96 | 3.6M | 1.7M | 128.6k | 47% |
| `openrouter/anthropic/claude-opus-4.6` | 91 | 2.1M | — | — | **0%** |
| `codex/gpt-5.6-sol` | 36 | 1.2M | 181.2k | — | 15% |
| `openrouter/moonshotai/kimi-k3` | 38 | 1.0M | 816.1k | — | 82% |
| `claude/claude-opus-4-5` | 35 | 874.0k | 443.0k | 54.8k | 51% |
| `claude/claude-haiku-4-5` | 14 | 334.1k | 144.4k | 37.5k | 43% |
| `claude/claude-opus-5` | 21 | 228.7k | 89.4k | 101.0k | 39% |
| `openrouter-anthropic/…/claude-sonnet-4.6` | 26 | 169.0k | 116.9k | 38.9k | 69% |
| **all** | **813** | **28.8M** | **9.2M** | **1.0M** | **32%** |

Three findings that re-shape the epic's own premise:

1. **`claude/*` is not the worst offender — it is mid-pack.** Weighted across its models, `claude/*`
   is **35%** and `codex/*` is **29%**. The epic's original framing ("claude fails to cache") came
   from reading the request builder, not from measurement, and the measurement does not support it
   as a *relative* claim. It does support the absolute one: 32% overall means roughly two thirds of
   every prompt is paid at full input rate.
2. **The largest single waste is outside this epic's scope.** `openrouter/anthropic/claude-opus-4.6`
   ran 2.1M context and $11.10 at **literally zero** caching. C-35 enabled `cache_control` on the
   `openrouter-anthropic` codec only; the plain `openrouter` chat path still pins
   `prompt_caching: false`. The same model family via `openrouter-anthropic` hits 69%. Not filed as
   a story here — the user scoped this epic to claude and codex — but it is the highest
   dollars-per-unit-effort fix visible in the data.
3. **`claude/claude-opus-5` is the only row showing the predicted failure mode.** 89.4k read against
   101.0k *written* — a ~1:1 write:read ratio, i.e. the prefix is being re-written about as often as
   it is reused. That is the tool-churn / TTL-expiry signature. Sample is small (21 calls), so
   C-133's harness should confirm it before C-134/C-135/A-95 are judged on it.

Interpretation: the structural gap (no breakpoint in `messages`) is real and read directly from the
code, but its *size* is now bounded by data rather than asserted. The ceiling on any fix is the ~65%
of tokens currently arriving fresh; the floor is whatever share of that is genuinely new content
that no cache could have served.

## Why

A-03 made the *planner prefix* cache-stable and live-verified a 99% cross-process hit. That fix is
still working — and it is also the entire extent of prompt caching in flux. Everything since has
grown around it without extending it, and the result is that both subscription providers now leave
most of a turn's prompt uncached.

The concrete finding, from reading the request path end to end:

**flux never places a cache breakpoint anywhere in `messages`.** Every `cache_control` in the tree
is stamped in the `system` array — `system_field` and `segmented_system_field`
(`crates/flux-providers/src/messages/mod.rs:127,140`) are the only two writers, and
`flux_core::ContentBlock` (`crates/flux-core/src/content.rs:37`) has no field that could carry one.
Anthropic renders `tools` → `system` → `messages`, so the cacheable prefix stops where the system
prompt ends. Steady-state hit rate is bounded by `(tools + system) / (tools + system + messages)`,
which decays for the whole life of a turn as file reads, bash output, and tool results accumulate in
`state.messages`. A-03's 99% was measured at the *start* of a turn, on the prefix; it was never a
claim about the turn as a whole, and the number we render today is not measuring the same thing.

Four aggravators sit on top of that, each independently worth fixing:

1. **Tool-set churn cold-writes the prefix.** `req.tools` is rebuilt every round from
   `selected_specs_for_state` (`crates/flux-flow/src/staged.rs:1098`), and `capability_signal`
   expands the family set mid-loop. Because `tools` renders *before* `system`, one expansion
   invalidates every system breakpoint too. Ordering itself is fine (`selected_specs` builds a
   `BTreeMap`) — it is the membership change that costs.
2. **Only the 5-minute TTL is ever used.** `{"type": "ephemeral"}` with no `ttl` field anywhere in
   the crate. Interactive subscription use — a human reading output for six minutes between turns —
   cold-starts the prefix on the next turn. This is the most plausible reason `claude/*` *feels*
   worse than `anthropic/*` for the same work: it is the usage pattern meeting a 5-minute window,
   not a different code path. The two providers share `AnthropicProfile` and the codec verbatim
   (`crates/flux-providers/src/spec.rs:157`).
3. **The live displays report the worst round of the turn; the offline one is fine.**
   `Usage::accumulate` (`crates/flux-core/src/stream.rs:81-86`) deliberately *replaces* the
   input/cache side with the latest call's. Everything downstream of `TurnEnded.usage` therefore
   shows the final round — the round with the longest message tail and the lowest ratio in the turn.
   That is the CLI turn annotation (`crates/flux-cli/src/rendering.rs:827`) and the TUI header
   (`crates/flux-tui/src/lib.rs:997-1001,1918-1933`), which additionally *sums both cache tiers into
   one figure*, so a session reading 3.2M from cache renders identically to one writing 3.2M into
   it. `flux usage` is **not** affected — it reads per-call `CallUsage` events and is correct today.
   The per-call data the live displays need is already persisted and, in the TUI's replay path,
   already collected (`lib.rs:2270`) and then ignored in favour of `turn_usage`. This is a wiring
   and presentation gap, not an instrumentation one.
4. **`claude` has zero breakpoint headroom.** `OAuthAnthropic::system_prefix`
   (`crates/flux-providers/src/anthropic.rs:117`) injects the Claude-Code identity line, which
   `NativeProvider::stream` (`crates/flux-provider/src/lib.rs:687`) inserts as segment 0 with
   `cache: true`. At the intent stage that is prefix + `INTENT_SYSTEM` + index + base = exactly 4,
   Anthropic's hard maximum (A-23). The breakpoint is not wasted — `tools` precedes it, so it caches
   the tool catalog — but it means any message-level breakpoint pushes claude to 5 and every planner
   call 400s. The cap has to become a shared budget before the headline fix can land.

On the codex side the wire is different and the failure mode is different, but the shape rhymes.
`build_responses_body` (`crates/flux-providers/src/openai.rs:829`) sets `store: false` and re-sends
the full `input` array each round, so OpenAI's automatic prefix caching is the only mechanism
available — and we do nothing to help it hit. Two specifics: we send no cache-routing key, and
`instructions` is built from `req.system_text()` (`crates/flux-provider/src/lib.rs:147`), which
flattens *all* segments including the trailing per-turn one that the Anthropic path deliberately
keeps after the last breakpoint. On Responses that volatile text lands at the very front of the
cacheable prefix. The segment layout that helps claude actively hurts codex.

The organizing idea of this epic: **cache behavior is currently unowned and unmeasured.** Nothing
pins the breakpoint layout, nothing reports a turn-level hit rate, and nothing fails when a new
system segment or a tool-set change silently halves the cache. Fix the measurement first, then the
claude path, then the codex path, then pin the contract so the next change to either can't regress
it quietly.

## Approach

Nine stories in four waves. Measurement and display lead deliberately — every later story's
acceptance is a before/after number, and until the live displays agree with `flux usage` nobody can
watch a fix land.

### Wave 0 — see it clearly (C-133, C-139, C-140)

The baseline above proves the offline path is sound, so this wave is narrower than first scoped: it
is about making the *live* surfaces tell the same truth, and about being able to reproduce a
measurement on demand.

- **C-133 — turn-level accounting + trace fields + a harness.** `Usage::accumulate`'s replace
  semantics are correct for *context-window occupancy* (the `ctx` figure) and wrong for *cache
  efficiency*; add a separate cumulative accumulator rather than changing it. Add the realized
  breakpoint count to `begin_model_trace` (`crates/flux-provider/src/lib.rs:601`) and the per-round
  cache split on the response side, so `FLUX_MODEL_TRACE=1` alone answers "what fraction of this
  round was cached, and where did the breakpoints land". Add a fixed scripted multi-round turn under
  `bench/` as the repeatable A/B instrument.
- **C-139 — fix the TUI header.** Feed it from per-call usage instead of `TurnEnded.usage`, and
  split the two cache tiers so read and write stop being one number.
- **C-140 — an in-TUI `/usage` overlay.** A live per-session dashboard: this turn's context, hit
  rate, and read/write/fresh split, plus a per-round bar list that makes a mid-turn cache collapse
  visible *as it happens* — which is precisely how tool-set churn (A-95) and TTL expiry (C-135)
  announce themselves.

### Wave 1 — claude (C-134, C-135, A-95)

**C-134, the headline fix: a cache breakpoint on the conversation tail.** Requires a carrier for
`cache_control` on message content. Preferred shape: keep `ContentBlock` clean and put the decision
on `Request` (e.g. `cache_tail: bool`), letting `build_messages_body` stamp the last content block
of the last message. That keeps the wire concern in the codec, keeps `flux-core` free of an
Anthropic-specific field, and means non-Anthropic codecs ignore it for free.

Two constraints the implementation must respect:

- **The ≤4 cap becomes a global budget.** `cache_breakpoints` (`messages/mod.rs:178`) currently caps
  *system* segments at 4 with no knowledge of a message breakpoint. It must take the number of
  non-system breakpoints as input and cap the union. Given claude already sits at 4 at the intent
  stage, the tail breakpoint has to displace a system one — and the tail is worth more than the
  smallest system segment, so the existing "keep the largest" rule extends naturally.
- **The 20-block lookback.** Anthropic walks back at most 20 content blocks from a breakpoint to find
  a prior entry. A round that appends more than 20 blocks (a wide parallel tool call: one assistant
  message of N `tool_use` blocks plus one user message of N `tool_result` blocks) will silently miss.
  Either bound it or place an intermediate breakpoint; the story must state which and test it.

**C-135: `ttl: "1h"` on the stable prefix breakpoint.** The 2× write premium pays back in three
requests, which any single turn clears. Scope it to the tools+system prefix breakpoint (the part
that is genuinely stable across turns) and leave the conversation tail on the 5-minute default,
where it is rewritten every round anyway.

**A-95: freeze the advertised tool set for the turn.** Move `capability_signal`'s expansion to a
turn boundary, or admit the expansion into the round's tool set only once and keep it thereafter
(monotonic growth is already the pattern in turn-intent surfacing). Either way the goal is that
`req.tools` is byte-identical across the rounds of one turn in the common case.

### Wave 2 — codex (C-136, C-137)

**C-136: cache-routing key.** OpenAI's Responses prefix caching benefits from an explicit routing
key so successive requests in one session land on the same cache shard. The exact parameter name and
semantics must be confirmed against current OpenAI Responses documentation as the first step of the
story — this design does not assert it from memory. The flux-side shape is settled regardless: derive
a stable per-session key from `RequestTrace.session_id`
(`crates/flux-provider/src/lib.rs:83`), which every engine-issued request already carries.

**C-137: keep volatile per-turn text out of `instructions`.** `system_text()` joins every segment,
so the trailing `cache: false` segment — deliberately placed after the last breakpoint for
Anthropic — is hoisted into the front of the Responses prefix. The fix is to respect the
cached/uncached split on the Responses path too: cached segments build `instructions`; the uncached
tail becomes a leading `input` item instead. That preserves ordering semantics for the model while
moving the volatile bytes behind the stable prefix.

### Wave 3 — close-out (C-138)

Pin the contract so this epic doesn't have to be repeated:

- A codec-level regression test that asserts the realized breakpoint layout for both the claude and
  anthropic segment layouts (count ≤ 4, tail stamped, prefix stamped) — the analogue of A-23's cap
  test, extended to the union budget.
- Live A/B for both providers using the C-133 harness, with the numbers recorded in this document.
- A short "cache layout" section in the architecture/provider docs stating the invariant: *tools and
  the cached system segments form the stable prefix; per-turn text goes after the last breakpoint;
  the conversation tail carries the rolling breakpoint; the union stays ≤ 4.*

## Alternatives considered

- **Anthropic's top-level automatic `cache_control`** (auto-place on the last cacheable block)
  instead of explicit breakpoints. Rejected: it gives up the deliberate cache-first segment layout
  A-03 built, and it cannot express "stable prefix on 1h, rolling tail on 5m".
- **Server-side context management / compaction** (`context-management-2025-06-27`,
  `compact-2026-01-12`) instead of caching the tail. Rejected as the fix for *this* problem — it
  reduces the prompt rather than caching it, it is beta on every platform, and flux already owns
  compaction (`crates/flux-flow/src/engine.rs:1440`). Worth revisiting separately.
- **`previous_response_id` on the codex path** to avoid re-sending `input`. Rejected: incompatible
  with the `store: false` posture the codex provider deliberately holds, which exists so no
  conversation state is retained server-side.
- **Changing `Usage::accumulate` to sum the input/cache side.** Rejected: its replace semantics are
  correct for context-window occupancy and are relied on by the `ctx` figure and the server's usage
  rows. Cache efficiency gets its own accumulator instead.
- **Doing codex first.** Rejected per the user's sequencing: claude is the higher-traffic
  subscription path and the one with the structural gap; codex's issues are narrower and partly
  need doc confirmation.

## Risks & open questions

- **The claude-vs-anthropic delta may be entirely usage-pattern.** The two share the profile and
  codec exactly; nothing disables caching for the OAuth transport. C-133 must answer this before
  wave 1 is judged — if the same scripted turn shows the same hit rate on both providers, then
  "claude is worse" is the 5-minute TTL meeting interactive pauses, and C-135 is the whole
  claude-specific fix.
- **Breakpoint budget pressure.** Adding a tail breakpoint on a layout already at 4 means dropping a
  system one. Which one gets dropped changes what survives a tool-set change; the C-134 test has to
  pin the choice, not just the count.
- **Minimum cacheable prefix is not uniform.** 512 tokens on Opus 5, 1024 on Sonnet 5 / Opus 4.8,
  higher on older generations. A breakpoint below the model's minimum silently does not cache —
  `segmented_system_field` has no size gate at all (`CACHE_MIN_CHARS` guards only the unsegmented
  path). Whether to add a gate is open; measuring first (C-133) should decide it.
- **Codex parameter name unconfirmed.** C-136 starts with a documentation check, not an
  implementation.
- **The 20-block lookback bound is untested against real wide-fan-out turns.** We may discover the
  tail breakpoint misses precisely on the turns where it matters most.

## Acceptance / done

The union of the nine stories' acceptance, plus:

- A turn-level cache hit rate is reported and is the number a user sees (C-133).
- On a fixed multi-round scripted turn against `claude/*`, measured cache_read as a share of total
  prompt tokens improves materially against the recorded baseline, with before/after numbers in this
  document (C-134/C-135/A-95, verified via C-138).
- The same harness run against `codex/*` shows a measured improvement against its own baseline
  (C-136/C-137, verified via C-138).
- A regression test pins the realized breakpoint layout, and the cache-layout invariant is documented
  (C-138).
- The standard gate stays green: `cargo build --workspace`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all`,
  `cargo test -p flux-codegate`.
