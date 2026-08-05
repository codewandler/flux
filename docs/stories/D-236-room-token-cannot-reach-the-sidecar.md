---
id: D-236
title: "The sidecar reads its room token from an env var flux clears, so it can never authenticate"
pillar: Agent
status: blocked
priority: 3
epic: meeting-rooms
design: docs/designs/meeting-rooms.md
areas: [flux-channels]
note: "sidecar.js reads process.env.FLUX_ROOM_TOKEN; that name is not on flux-system's env allow-list and nothing sets it, so the token is always null. Every other host fact got an argv flag per the file's own premise — the credential is the one that did not"
---

# The one host fact that did not get an argv flag

## Goal

Give the media sidecar a way to actually receive a room token, through the seam the design already
chose for every other host fact, so a real authenticated join is possible.

## The finding

`crates/flux-channels/assets/room-media/sidecar.js:359`:

```js
token: process.env.FLUX_ROOM_TOKEN || null,
```

That is the **sole occurrence of `FLUX_ROOM_TOKEN` in the tree.** Nothing sets it, and it is not on
`flux-system`'s environment allow-list (`crates/flux-system/src/lib.rs:52-54`, which admits exactly
`PATH`, `HOME`, `LANG`, `LC_ALL`, `LC_CTYPE`, `TERM`, `TZ`, `USER`, `LOGNAME`, `TMPDIR`, `RUST_LOG`,
`RUST_BACKTRACE`, `RUSTUP_HOME`, `CARGO_HOME`, `RUSTUP_TOOLCHAIN`, `KUBECONFIG`). flux calls
`env_clear()` first. So the `|| null` fallback is not a fallback — it is the only branch that ever
runs, and the recorded runbook cannot authenticate.

⚠ **The env-clearing is correct and is not what should change.** `sidecar.rs:6-18` states the rule and
why it is the rule working rather than a workaround, and the seam it chose is argv: *"Those are the
sidecar's, and they ride in `argv`"* (`sidecar.rs:55-57`). Every other host fact — the audio server, the
display, the device — got a flag. The credential is the one that did not, and it is the one that cannot
work without one.

The design has already thought about a credential in argv and accepted it: `MediaSidecarConfig`'s
`Debug` is **hand-written** precisely so that an operator who wrote `sidecar ["…", secret "ROOM_TOKEN"]`
does not get the token printed the first time anyone adds a trace line (`sidecar.rs:58-63`). So the
argv path is not merely available, it is the path the code was already hardened for.

## Acceptance

- [ ] A failing-first test: a token supplied through the chosen seam reaches the sidecar's join, and is
      not `null`. It must fail at the merge base.
- [ ] The token arrives by the seam the design already chose — an argv flag — rather than by widening
      `flux-system`'s env allow-list. ⚠ **Adding `FLUX_ROOM_TOKEN` to the allow-list is the wrong fix**
      and must not be the one taken: the allow-list is explicitly a *non-secret* list, and putting a
      credential on it inverts its purpose for every subprocess flux spawns, not just this one.
- [ ] `sidecar.js` no longer reads a credential from `process.env` — a line that cannot work is worse
      than absent, because it reads as a supported configuration path.
- [ ] ⚠ The token does not reach a log, a trace, or a protocol `error` line. See Notes: there is a known
      shape here that is currently moot only because the token is always `null`.
- [ ] The runbook in the story/doc comments is updated to the path that actually works, and someone can
      follow it to an authenticated join.

## Notes

- ⚠ **Related leak shape, currently moot but unmasked by this story.** `sidecar.js:415` writes
  `error.message` into a protocol `error` line, and the `evaluate` expression at `:355` embeds the token.
  A page exception could therefore echo a JWT into the protocol stream. That is unreachable while the
  token is always `null` — and this story is precisely what makes it reachable. Fix both together or the
  fix ships a leak.
- ⚠ Note the tension to resolve deliberately: a credential in argv is visible in `/proc/<pid>/cmdline`
  to anything that can read it on the host. The design accepted that (and hardened `Debug` for it), but
  say so explicitly where an operator will read it, rather than leaving it implied. If that is judged
  unacceptable, a file-descriptor or stdin handoff is the alternative — but then the *design*'s
  argv-only premise changes, and that is a design-doc decision, not an implementation choice.
- Pre-existing from D-208, not introduced by
  [D-232](D-232-the-media-sidecar-harness.md) — verified 2026-08-02 that the line is on `main`. Surfaced
  by D-232's review.
- Related: [D-235](D-235-argv-alone-does-not-reach-the-audio-server.md) is the same shape for the audio
  server (argv necessary but not sufficient). Both come from the same "host facts ride in argv" premise
  meeting reality.

## Progress

- 2026-08-05 — dispatched in wave `flux-wave-20260805-0829` as `ready`; raised to `blocked` without
  an implementation, because the acceptance surface is not on `main`.

  `crates/flux-channels/assets/room-media/sidecar.js` has never existed on `main` and is absent from
  the wave's source commit `dc07e60e`. Verified four ways: `git ls-tree -r` finds no `.js` under
  `flux-channels` on `main` or on the wave branch; `git log --all -- crates/flux-channels/assets`
  returns exactly two commits, both on the unmerged `impl/D-232`; and `FLUX_ROOM_TOKEN` occurs in the
  tree only in this story's own text and the generated board. There is no `process.env` read to
  remove, no join options to thread a token into, and no `:415` error line to harden.

  ⚠ **This corrects a factual claim in this story's own Notes.** "Verified 2026-08-02 that the line
  is on `main`" is wrong: the line is on `impl/D-232`, whose merge-base with `main` is `25fc674a`.
  The finding itself is real — it is simply not yet reachable from `main`.

  The Rust half of the seam needs no change: argv is already opaque to flux and
  `MediaSidecarConfig`'s hand-written `Debug` already redacts, so `sidecar ["…", "--token",
  secret "X"]` works today. The missing half is entirely the consumer.

  **Real dependency: D-232**, which is itself `blocked` on a human confirming an audible tone in an
  operator-owned live room. `impl/D-232`'s tip (`6d1053b1`) already implements most of this story —
  an argv `--token` flag with redaction. Writing a second implementation on `main` would have
  produced a divergent duplicate guaranteed to conflict when that branch lands. The decision owed is
  whether to land `impl/D-232`, not whether to rewrite it.
