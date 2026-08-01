---
id: C-423
title: "`flux cast` — render a recorded session to an asciicast, headless, from the real TUI widgets"
pillar: Core
status: backlog
design: docs/designs/session-screencast.md
epic: session-screencast
areas: [flux-cli, flux-tui]
note: "BLOCKED on C-422 — without the render projection there is nothing to paint. Emits asciicast v2 (text, diffable, no image dependency); GIF/SVG is `agg`/`svg-term`'s job, deliberately not flux's"
---

# Paint the timeline, headless

## Goal

`flux cast <session|last>` writes a valid asciicast v2 file that plays in `asciinema play`, showing the
real TUI, paced from the recorded millisecond timestamps.

## Why asciicast and not a video

asciicast v2 is a JSON header line followed by `[time, "o", "output"]` lines. That choice buys:

- it **diffs in review** — a UI regression is visible in a pull request;
- **no image or video encoder dependency** in flux for a presentation concern;
- GIF and SVG remain available through existing tools (`agg`, `svg-term`) when a blog needs one.

And the frames come from **the same ratatui widgets the live TUI uses**, driven by a fixed viewport
instead of a real terminal — so a cast shows the product, not a mock of it. A renderer that
re-implements the layout would drift from the TUI within one release and quietly start advertising a UI
that does not exist.

## Acceptance

- [ ] **Failing-first**: a test rendering a fixture session and asserting a parseable asciicast v2
      header plus monotonically non-decreasing event times, failing at the merge base.
- [ ] Renders headless — no real terminal, no TTY — so it runs in CI. ⚠ This is the property that makes
      docs assets regenerable; a renderer needing a TTY is a renderer nobody runs unattended.
- [ ] Frames are produced by the live TUI's own widgets at a fixed viewport size, not by a
      reimplementation. A test must fail if the two diverge, or the story says why that cannot be
      pinned.
- [ ] Timing comes from the recorded `ts`, not from wall-clock at render time.
- [ ] `--speed` and an **idle-squash** for long waits. ⚠ Squashing changes what a viewer believes about
      latency, so: squash long *waits*, never compress the tail of streamed output, and make the
      applied factor discoverable in the output. A demo that silently implies flux is 5× faster than it
      is, is a defect, not a feature.
- [ ] Output plays in `asciinema play` and converts with `agg` — verified once by hand and the command
      recorded in the story, since neither tool can be a test dependency.
- [ ] ⚠ **Does not write a cast unless [C-424](C-424-a-cast-is-a-publishing-act.md)'s redaction gate has
      run.** If C-424 has not landed, this story ships `flux cast` writing to stdout or a temp path with
      an explicit "not redaction-gated" banner — it must not quietly produce a publishable file first
      and gain safety later.
- [ ] Full gate green.

## Notes

- **Blocked on [C-422](C-422-the-render-projection.md).** The projection is what supplies the timeline;
  starting here means inventing one, and inventing one is how the fidelity question gets settled by
  whatever looks good.
- Precedent for flux rendering its own artifacts: **flux-render** (L-74…L-78), flux source and plans to
  SVG/PNG. Read how it handles fixed-size output before choosing a viewport default.
- Open in the design: whether a cast should be byte-for-byte reproducible from the same session. It is
  desirable (a CI check that the UI did not change unexpectedly) and cheap if decided now — it costs
  only deriving every timestamp from the log rather than from render-time wall-clock. Expensive to
  retrofit.
- Viewport width is unmeasured: too wide is unreadable embedded in a docs page, too narrow
  misrepresents the layout. Likely a documented default plus a flag.

## Progress

- Filed 2026-08-01 with the session-screencast epic. `backlog`, not `ready`: C-422 gates it.
