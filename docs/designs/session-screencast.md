# Design: Session screencast — render a recorded run as a terminal cast

**Status:** proposed · **Pillar:** Core · **Stories:** [C-421](../stories/C-421-tui-takes-a-task-from-the-cli.md) · [C-422](../stories/C-422-the-render-projection.md) · [C-423](../stories/C-423-flux-cast.md) · [C-424](../stories/C-424-a-cast-is-a-publishing-act.md)

## Why

Demos, docs and blog posts need to *show* flux working. Today the only way to get that is to point a
screen recorder at a live run and hope it goes well — which makes every asset a one-take performance,
impossible to regenerate when the UI changes, and impossible to produce in CI.

flux already records runs. The premise of this epic is that a screencast should be a **render of a
recording**, not a capture of a performance: run the task once, then re-render it as often as the
theme, the layout or the narration needs to change.

### ⚠ "Everything is recorded anyway" is half-true, and the half that isn't shapes this epic

That premise is the reason this looked like a small job. It does not survive contact with the code,
and the epic is sequenced around the gap:

**What is genuinely there.** `flux-events` is one append-only log holding every durable fact, and its
rows carry `ts INTEGER NOT NULL` at **millisecond** resolution (`crates/flux-events/src/store/sqlite.rs`
~line 481, stamped by `now_ms()`). `EventKind` holds `SessionStarted`, `TurnStarted`, `Message`,
`PlanAttempted`, `Compacted`, `ModelChanged` and `Run(RunEvent)`, and since C-43 `RunEvent::OpRecorded`
carries **redacted op output** durably (~442 B/cell, 1 MiB cap). So the *content* and the *timing* of a
run are on disk. Nothing new needs to be captured to know what happened and when.

**What is not there.** The TUI's on-screen surface is `UiEvent`, which is `pub(super)` — internal to
`flux-tui` — and **ephemeral**. It is the live render stream; it is never persisted. The TUI's
existing path from durable state back to screen state is
`crates/flux-tui/src/projection.rs::historical_observation_entry`, and it is **100 lines that handle
five observation kinds** (`flow.brief`, `flow.halt`, `skill.activated`, `KIND_TURN_INTENT`,
`KIND_DESTRUCTIVE`) — against **26 `UiEvent` variants** on the live side.

> **The data is largely there. The projection is not.** Replaying a session into the TUI today gets a
> thin summary, not the screen the operator saw. That ratio — 5 of 26 — is the actual work, and it is
> why C-422 gates the renderer instead of being folded into it.

Some of the 21 are genuinely unrecoverable rather than merely unprojected — `ToolProgress` (C-158's
live tail under a running tool's card), spinner frames, and `Retry` countdowns are *by construction*
about a moment that has passed. **A cast must not silently invent them.** Deciding, per variant,
between *faithful · approximated · absent* is C-422's deliverable, and it must be written down rather
than encoded in whatever the renderer happens to do.

### Where it sits

This is the visual sibling of the **Time Machine** epic. A-45 `flux replay` re-executes a recorded run
offline and model-free; A-46 `flux fork` branches it; C-44 `flux diff` compares two. All three are
about *re-running*. This epic adds the fourth verb — *re-showing* — and unlike the others it needs no
execution at all: it reads the log and paints. A-47 (the optional TUI time-machine cockpit) is the
interactive cousin; a cast is its non-interactive, publishable output.

Precedent for flux rendering its own artifacts is **flux-render** (L-74…L-78): flux source and plans
to SVG and PNG. A cast is the same instinct applied to a session instead of a flow.

## Approach

Four stories, sequenced. C-421 is independent and ships first because it is useful on its own; C-422
gates C-423; C-424 gates anything leaving the machine.

### C-421 — `flux tui <prompt>`: start the TUI with a task

`Commands::Tui` currently takes only `AgentFlags` (`crates/flux-cli/src/args.rs:298`) — there is no way
to hand the TUI a task. Every demo therefore opens with someone typing, which is slow, error-prone and
unrepeatable. `flux run` and `flux app run` already take a prompt; the TUI is the outlier.

Add a positional prompt, matching the spelling those verbs already use (`prompt: Vec<String>`, per
`args.rs:294`) so `flux tui fix the failing test` works without quoting. Absent a prompt, behaviour is
exactly today's. This is small, has no bearing on the renderer, and is the thing that makes a demo
*scriptable* — which is what makes it re-recordable.

### C-422 — the render projection: rebuild a session's visual timeline

One function, in `flux-tui`, from a session id to an ordered `Vec<(Duration, Frame-affecting change)>`
— the timeline the renderer paints. It reuses the store's `ts` for pacing and the existing `Entry`
vocabulary for content, so a cast and a live run share one notion of what a turn looks like.

The deliverable that matters is not the function; it is the **fidelity table** — every `UiEvent`
variant classified *faithful* (rebuilt from a durable event), *approximated* (synthesised, and the cast
says so), or *absent* (not recorded; the cast shows nothing rather than a guess). ⚠ This repo's
recurring defect class is *a guard or comment that agrees with its own assumption*; a projection that
quietly interpolates a plausible-looking tool tail is exactly that bug wearing a demo costume. An
approximation must be visible in the output, not just in a doc comment.

⚠ **`Compacted` is the sharp edge.** A session that compacted has *had messages replaced* in the log.
A naïve timeline replays post-compaction state as though it were what the operator saw. C-422 must
decide whether a cast renders the pre- or post-compaction view and say which.

### C-423 — `flux cast`: paint the timeline, headless

A new CLI verb rendering a session to **asciicast v2** — the asciinema format: a JSON header line then
`[time, "o", "output"]` events, one per line.

Deliberately *not* a video encoder. asciicast is text, so a cast **diffs in review**, compresses well,
costs flux no image/GIF dependency, and converts to GIF or SVG with existing tools (`agg`, `svg-term`)
when a blog needs one. It also means the terminal frames are produced by the same ratatui widgets the
live TUI uses, driven by a fixed viewport size rather than a real terminal — so the cast shows the real
UI, not a mock of it.

Pacing is a knob, not a fact: `--speed`, and an idle-squash so a 40-second model call does not become
40 seconds of a spinner nobody will watch. ⚠ Squashing changes what the viewer believes about latency,
so the default must be honest — squash long *waits*, never compress the tail of streamed output, and
make the applied factor discoverable.

### C-424 — a cast is a publishing act

A screencast's entire purpose is to leave the machine. That makes `flux cast` different in kind from
`flux replay`, whose output stays local.

The recorded material is redacted — `OpRecorded` cells are redacted at capture — but **redaction has
failed open in this codebase before**: [C-339](../stories/C-339-redaction-falls-back-to-the-unredacted-value.md)
found `redact_and_hash_request` returning the *unredacted* value when redacted text stopped parsing.
The blast radius of that bug in a published GIF is unrecoverable in a way it is not in a local log.

So this story owns: re-running the `Redactor` over rendered frames at render time rather than trusting
capture-time redaction alone (defence in depth — the two are independent passes); refusing to write a
cast when redaction is unavailable rather than writing an unredacted one; and an adversarial test that
plants a credential and asserts it is absent **from the rendered frames**, not from the event payloads.
⚠ The test must attack the *rendered output*, because the whole point is that rendering is a second
chance to leak: a secret can be absent from a payload and present in a wrapped, ANSI-styled frame that
was reassembled from it.

## Alternatives considered

- **Capture the terminal live (`asciinema rec flux tui …`).** Zero flux code, and it is the right
  answer for a one-off. Rejected as the epic's basis because it is a capture of a performance: it
  cannot be regenerated when the theme changes, cannot run in CI, records typing latency and typos, and
  has no redaction pass at all — it films whatever was on screen.
- **Persist `UiEvent` to the event store.** Would make the cast trivially faithful. Rejected: it makes
  a `pub(super)` render-loop detail into a durable schema, so every TUI layout change becomes a store
  migration, and it duplicates content already in the log. The projection is the cheaper seam even
  though it is more work.
- **Emit GIF/MP4 directly.** Rejected: an image or video encoder is a heavy dependency for a
  presentation concern, the output is unreviewable in a diff, and `agg`/`svg-term` already do it well
  from asciicast.
- **Fold the projection into the renderer (three stories, not four).** Rejected: the fidelity question
  is the epic's real risk, and a story that must ship a renderer will settle it with whatever makes the
  demo look good.

## Risks & open questions

- ⚠ **The projection can lie attractively.** The pressure on a demo tool is toward a good-looking
  result, and this is the repo's known defect class. Mitigation: the fidelity table is a C-422
  deliverable, and approximations must be visible in output.
- ⚠ **Redaction is the one that cannot be walked back.** A published asset with a live credential is
  unrecoverable. C-424 exists for this and must not be folded into C-423 for convenience.
- **Compaction changes history.** Undecided: pre- or post-compaction view. C-422 owns it.
- **Theme and width.** A cast is rendered at a fixed viewport; too wide is unreadable embedded, too
  narrow misrepresents the layout. Probably a documented default plus a flag, but unmeasured.
- **Approval modals.** A recorded run that hit a y/a/N modal has a real pause with no keystroke record.
  Faithful, approximated, or absent — C-422 decides.
- **Sub-agent activity** (`UiEvent::SpawnActivity`, the fleet pane) may be the least recoverable region
  of the surface; A-79's `subagent.activity` observation is the thing to check first.
- **Open:** whether a cast should be reproducible byte-for-byte from the same session. Desirable for
  CI regression ("the UI changed unexpectedly"), and it costs the timestamps being derived only from
  the log rather than from wall-clock at render time — cheap if decided up front, expensive later.

## Acceptance / done

- `flux tui <prompt>` starts the TUI already working on a task; no prompt behaves exactly as today.
- A recorded session renders to a valid asciicast v2 file that plays in `asciinema play` and converts
  with `agg`, showing the real TUI widgets, paced from the recorded millisecond timestamps.
- The fidelity table exists and is honest: every `UiEvent` variant classified faithful / approximated /
  absent, approximations visible in the rendered output.
- A planted credential does not appear in the rendered frames, proven by a test that attacks the frames
  rather than the payloads; a cast is refused rather than written unredacted.
- Regenerating a docs or blog asset is one command against a stored session, not a new recording
  session.
