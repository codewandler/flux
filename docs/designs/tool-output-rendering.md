# Tool-output rendering — a correct, legible transcript card pipeline

Status: **reviewed 2026-08-05 — filed as epic `tool-output-rendering` (C-531…C-539)**

This doc records a source-walked review of how tool calls and results render in the TUI transcript
(and, through the shared seam, the CLI). The pipeline's architecture is right — one color-free
semantic formatter, `crates/flux-tui/src/toolview.rs`, shared by both surfaces, with each surface
mapping `DetailKind` → style — but the review found two correctness defects that can make the
transcript lie, a cluster of rendering gaps, and one dead-end affordance. Every finding below was
verified against the tree, not inferred.

Review provenance: at review time a merge of `origin/main` was in progress locally; F-1's trigger
(C-528 concurrent native call batches, `flush_parallel_native_calls`) is canonical on origin and
arrives here with that merge. The finding is stated against the post-merge tree.

## Verdict table

| # | Finding | Verdict | Story |
|---|---------|---------|-------|
| F-1 | Concurrent same-name calls cross-attach results/timings (name-based LIFO matching) | **correctness defect, common under C-528** | C-531 |
| F-2 | Yank copies the model-facing view; canonical content unavailable to live surfaces | **correctness defect** | C-532 |
| F-3 | Tool output reaches ratatui spans with ESC/control bytes intact | **correctness defect** | C-533 |
| F-4 | Diff rendering keyed on op name; `git_diff`/`patch` output renders flat | **valid gap** | C-534 |
| F-5 | `web.fetch`/`proc.run`/`task` summaries generic; `grep`/`glob` rows unstyled | **valid gap** | C-535 |
| F-6 | Wrapped continuation rows lose the gutter rail and card indent | **valid gap** | C-536 |
| F-7 | V-8 code-block gutter in flux-markdown (sole survivor of the prior UX review) | **valid, unfiled until now** | C-537 |
| F-8 | `… N more lines` is inert — no runtime path past `MAX_DETAIL` | **valid affordance gap** | C-538 |
| F-9 | Truncation policy drifts between TUI and CLI (30 vs 40/500 + per-tool heads) | **valid hygiene debt** | C-539 |

## Correctness — the transcript must not lie

**F-1 — results attach to cards by tool name, newest-first.** `progress_tool`, `finish_tool` and
`time_tool` all scan `entries.iter_mut().rev()` for the newest result-less card with the same op
name (`crates/flux-tui/src/lib.rs:1857-1948`), on the documented assumption that "ops dispatch
sequentially". C-528 removed that assumption: `flush_parallel_native_calls`
(`crates/flux-flow/src/staged.rs:2938-2949`) runs admitted same-response native calls under
`join_all`, each through a `SharedSink` clone of the one `AgentSink` — call/result events
interleave live. Admission (`native_call_parallel_safe`) selects idempotent read-only ops, i.e.
exactly the same-name `read`/`grep`/`glob` batches models emit constantly. For the interleaving
`[call a, call b, result a, …]` the LIFO scan pins a's result to b's card and vice versa; timings
cross the same way. The resume path pairs by step id and is correct (`lib.rs:2968-2971`), so a
resumed session can silently contradict what the operator watched live.
*Fix shape:* mint a dispatch id per call in `run_call` (`crates/flux-lang/src/runtime.rs:3736`) —
call and result bracket one await, so pairing is exact by construction — and carry it through
`FlowSink` (`crates/flux-lang/src/sink.rs`) and `AgentSink` (`crates/flux-flow/src/agent_sink.rs`)
to every sink implementor; the TUI matches on id. Both crates are published: **breaking signature
change ⇒ workspace MINOR**, clean cutover, no compat bridges. The whatif `RerunRecordingSink`'s
documented FIFO pairing hazard is fixed by the same id.
*Acceptance:* a failing-first flux-flow test proves each result event carries its call's id under
concurrent same-name dispatch; a failing-first TestBackend test feeds `call(id1) call(id2)
result(id1)` and asserts card 1 resolves while card 2 stays `◌ running` (today the LIFO scan
resolves card 2).

**F-2 — the two faces of a `ToolResult` collapse at the sink boundary.** `ToolResult` separates
canonical `content` from the model-facing `view` (`crates/flux-runtime/src/lib.rs:70-80`), and the
event store persists both. But `run_call` sends surfaces `content: outcome.view, view: None`
(`crates/flux-lang/src/runtime.rs:3760-3771`) — so the live card correctly shows the numbered/diff
view, while the focused-entry `y` yank (`focused_entry_text`, `lib.rs:2397-2417`) copies that
*numbered* view — the one payload nobody wants to paste — and no live surface (CLI, SDK, server,
stream-json) can reach the canonical content at all. Resume, reading the durable event, has both
and keeps the view for display (`lib.rs:2983-2989`) but discards the canonical face too.
*Fix shape:* stop flattening at `run_call` — emit both faces (no trait signature change;
`OpOutcome` already carries both fields); surfaces display `view`-else-`content`, yank and machine
paths take `content`. Inventory whatif cassette hashing and stream-json consumers before landing;
sequence with C-526 so its PTY test pins canonical-content yanks.
*Acceptance:* expanded `read` card still shows the numbered view (frame test unchanged); a
failing-first test on `focused_entry_text` proves yank returns un-numbered content; stream-json
carries both fields with a release note.

**F-3 — no control-byte sanitization on transcript tool output.** Subprocess ESC/CR/BEL bytes go
verbatim into `Span::styled` cells, while every neighboring surface sanitizes: agent panes
(`trust.rs:26-28`), approval prompts, fleet names. **Strip, do not interpret**: interpreting SGR
via `ansi-to-tui` would let payload bytes reach a ratatui `Style` — the exact primitive the C-222
trusted-chrome boundary denies — and would bypass `NO_COLOR`/mono discipline. `ansi-to-tui` stays
reserved for the harness-authored plan tree (`plan.rs`).
*Acceptance:* failing-first frame test — a bash result containing `\x1b[31m`, `\r`, `\x07` renders
with no control cells, live-tail partials included; the plan-tree ANSI path is untouched.

## Rendering richness — inside the toolview seam

**F-4 — diff rendering is keyed on op name, not shape.** `format_diff` handles only `edit`/`write`
(`toolview.rs:203-260`). `git_diff` returns a raw unified diff as content
(`crates/flux-tools/src/lib.rs:2404-2415`); `patch` returns a status line + unified diff view
(`edit_result`, `flux-tools/src/lib.rs:925`, used at `:2111-2115`) — which live *is* the card's
content (F-2). Both render as flat muted lines while the diff renderer sits 1,500 lines away.
*Acceptance:* `format_diff` gains a `patch` arm synthesized from the `edits` input array (args are
exact and pre-result, same as `edit`); `format_detail` gains a unified-diff content classifier
(`+++`/`---`/`@@`/`+`/`-` → `DetailKind::{Meta,Hunk,Add,Del}`) so `git_diff` output — and any
diff-shaped result — colors correctly; toolview unit tests plus one frame test pin it. The C-195
no-redaction stance is restated where extended, never reopened.

**F-5 — per-tool call/summary coverage.** `web.fetch` has no `format_result` arm (card summary
falls back to the first body line); `proc.run` has no `format_call` arm (header shows a `k=v`
dump); `task` is generic; `grep`/`glob` expanded rows are unstyled `path:line: text` although the
pattern is present in the input. The CLI's semantic previews (first matches for grep, last line for
bash — `flux-cli/src/rendering.rs:292-364`) show the summaries can be richer without noise.
*Acceptance:* toolview arms for `proc.run` (verify field names at the flux-tools registration),
`web.fetch` (size/line summary), `task`; a color-free row classification for grep/glob expanded
detail (path:line prefix + match emphasis) that the TUI styles glyph-safe under mono; unit tests
per arm.

**F-6 — wrapped continuation rows dissolve the card's left edge.** Logical rows get the gutter
rail prepended per entry (`prepend_gutter`, `lib.rs:2069`) and detail rows a 3-space indent, but
`wrap_styled_lines` (`lib.rs:3190-3243`) is prefix-unaware: an over-width detail line's
continuation rows start at column 0 — outside the rail, outside the card.
*Acceptance:* failing-first narrow-width frame test — a long bash detail line's continuation rows
keep the rail + indent; hanging-prefix budget guards `prefix ≥ width`; C-109 badge pairing (headers
are width-fitted and never wrap) and post-wrap `entry_rows` spans stay intact.

**F-7 — V-8, the prior review's one unshipped survivor.** Fenced code blocks in
`flux-markdown` emit bare lines (`Block::CodeBlock`, `crates/flux-markdown/src/render/layout.rs`)
while `BlockQuote` already has prefix machinery (~`:143-163`). Give code blocks a `▎ ` gutter —
mono-safe by glyph, benefits every Markdown surface.
*Acceptance:* failing-first flux-markdown unit test — every code-block row carries the gutter;
inline code unchanged.

## Affordance

**F-8 — the elision row is a dead end.** Expanded detail caps at `MAX_DETAIL = 30`
(`lib.rs:203-205`); the `… N more lines` row is informational only — the sole escape is restarting
with `-v`, which lifts every cap globally. The C-158 live tail (3 lines, deliberately not lifted)
is a separate, settled decision and stays untouched.
*Acceptance:* on the focused card, Enter cycles collapsed → capped → full → collapsed (full state
only offered when rows were elided; the elision row advertises it); failing-first frame test with a
40-line result; `-v` semantics unchanged; applies to final results only.

## Hygiene

**F-9 — one truncation policy.** TUI `MAX_DETAIL = 30` (`lib.rs:203`) vs CLI `MAX_LINES = 40`,
`MAX_LINE_CHARS = 500`, plus CLI-only per-tool head counts (`flux-cli/src/rendering.rs:20-56,
292-364`). Surfaces may legitimately budget differently — the CLI cannot expand, so it shows more
up front — but the budgets and per-tool semantics must be declared side by side in `toolview`, not
drift silently.
*Acceptance:* a `toolview` policy module both surfaces consume; a drift-pinning test on each side.

## Constraints (pinned; stories must not drift into these)

- **C-195**: `format_diff` and the approval preview render input verbatim — no `Redactor`
  (`toolview.rs:191-202`). Extensions restate the stance in doc comments.
- **Trusted chrome**: model/subprocess-influenced bytes never reach a ratatui `Style`
  (`trust.rs`); sanitize by stripping, never by interpreting.
- **Monochrome discipline**: structure must survive `NO_COLOR`/mono via glyphs + modifiers; frame
  tests assert glyphs, not styles, wherever possible.
- **C-158**: the 3-line live tail is a liveness signal, not a log viewer; `task` cards stay quiet
  (the fleet pane owns sub-agent visibility).
- **toolview stays color-free** (`toolview.rs:1-6`): extend `DetailKind`; surfaces map kind→style.
- **ratatui pinned `>=0.29,<0.30`** (markdown-ratatui seam); no new ratatui-version-dependent API.

## Adjacent tracked work (not duplicated here)

- **C-526** (P1) owns copy *mechanics*; C-532 changes copy *payload*. Sequence so C-526's PTY test
  pins canonical-content yanks.
- **A-137/A-142** (loop view as main display): this epic keeps transcript cards the *compact*
  surface and feeds the future detail pane a correct, well-classified stream (C-531/C-532 are
  prerequisites it will want). Web-fetch body rendering, in-card scrolling/pagers, full-input JSON
  views are deferred to that pane.
- **C-527** (`ui.display`) is the agent-initiated way to show artifacts; unaffected.

## Verification

- Failing-first tests named per story; `cargo test -p flux-tui` (TestBackend frame tests),
  `-p flux-markdown` for C-537, `-p flux-cli --lib` for the C-539 CLI side.
- Full repository gate (workspace build/test/clippy/fmt + `flux-codegate`) before any story is
  declared done — at review time the in-progress merge blocks the workspace gate; targeted checks
  stand in until it lands, and the gate re-runs after.
- Manual smoke: `cargo run -p flux-cli -- tui -m mock` in dark, light, and `NO_COLOR=1` mono.
