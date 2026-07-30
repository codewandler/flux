---
id: C-253
title: "A wall-clock assertion in the checkpoint test fails under concurrent build load, so a busy machine reds the gate on no defect"
pillar: Core
status: ready
priority: 12
areas: [flux-events]
note: "918ms observed against its threshold while four sibling builds ran; 5/5 pass in isolation at 0.22-0.36s — the assertion measures the machine, not the code"
---

# A wall-clock assertion in the checkpoint test fails under concurrent build load, so a busy machine reds the gate on no defect

## Goal
`store::tests::sqlite_tests::checkpoint_hook_never_blocks_or_errors_under_writer_contention`
(`crates/flux-events/src/store/mod.rs:3920`) asserts an elapsed wall-clock bound:

```
checkpoint must not wait out the writer's lock, took 918.764664ms
```

The property it wants is real and worth keeping — **the checkpoint hook must not block on the
writer's lock** (C-126 WAL hygiene). But it is asserted as a *duration*, and a duration measures the
machine as much as the code. Observed 2026-07-30 during an integration gate run with four sibling
`cargo` builds saturating the CPU: it failed at 918ms. Immediately afterwards, in isolation and with
five `rustc` processes still running, it passed **5/5 at 0.22–0.36s** — an order of magnitude under
the bound.

So the failure was CPU starvation. It cost a full gate re-run and an investigation into whether
C-230's WAL changes (which touch exactly this area — `set_wal_mode`, busy-handler ordering) had
regressed checkpoint behaviour. They had not: the same test passed in C-230's and C-217's green gates.

**Why fix it rather than shrug:** flux's own gate now runs with up to five concurrent worktree builds
by design (the impl-coord fan-out), and CI runners are shared and noisy. A timing assertion that fails
on a busy machine produces a red gate attributable to nothing, which is the same "red that isn't a
defect" pathology as C-252 — and it invites the next person to go looking for a regression in the
crate that the diff happens to touch.

## Acceptance
- [ ] The test still fails when the checkpoint genuinely **does** wait out the writer's lock — that
      property must not be weakened. **Failing-first**: an injected blocking checkpoint (or a
      deliberately held writer lock) makes it red.
- [ ] It does **not** fail purely because the machine is loaded. Prove it: run it under deliberate CPU
      saturation (e.g. `nproc`-many spinners, or alongside a workspace build) and show it green.
- [ ] The assertion tests **causality, not latency** — that the checkpoint returned *without* having
      waited for the writer to release, rather than that it returned within N milliseconds. Candidate
      shapes: observe lock-acquisition ordering, assert the checkpoint completed while the writer still
      held the lock, or count a "would have blocked" signal — anything that does not read the clock.
- [ ] If a duration bound genuinely cannot be avoided, it is scaled or retried with the reason stated
      in a comment, and the failure message says "this may be load, not a defect" so the next reader
      is not misdirected. **Prefer removing the clock to widening it** — a wider bound just moves the
      flake to a busier day.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-30 — found during C-252's integration gate. C-252's diff is a shell script, a workflow YAML,
  a markdown doc and a story file — **no Rust at all** — so the failure was structurally unrelated to
  the change being gated, which is what made it worth filing rather than fixing in passing.

## Notes
- Sibling flake already known and documented: `flux-flow`'s
  `surfacing_is_monotonic_across_a_marker_flip` reddens when a stray empty `/tmp/.git` exists. Two
  load/environment-sensitive tests is a pattern; if a third appears, the pattern is the story.
- Related precedent in this repo: a `flux-system` process test that fails under parallelism and passes
  serially, and 13 process tests that failed under a restricted-`PATH` shell during a release cut.
  Both were environment sensitivity misread as regression, which is exactly the cost being avoided
  here.
- Do not simply mark it `#[ignore]`. The property is a real C-126 guarantee, and an ignored test is a
  guarantee nobody checks.
