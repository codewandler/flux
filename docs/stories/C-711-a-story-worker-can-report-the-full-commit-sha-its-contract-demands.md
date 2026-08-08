---
id: C-711
title: "A story worker can report the full commit SHA its contract demands"
pillar: "Core"
status: backlog
epic: fleet-harness-throughput
areas: [flux-capabilities, flux-tools]
note: "both wave-602 workers wrote an apology paragraph: typed git ops return abbreviated hashes only and a writer has no shell by design, yet the assignment asks for the 40-char SHA"
---

# A story worker can report the full commit SHA its contract demands

## Goal

The story-worker assignment ends with "Report the full commit SHA, observed write set, exact
validation argv and before/after evidence." A worker cannot do the first one. The typed git
operations it is given return abbreviated hashes, there is no `rev-parse` and no `--format`, and a
writer deliberately has no shell — so the assignment asks for a value the tool ceiling makes
unobtainable.

Both `wave-602` workers hit it independently and both spent output tokens apologising for it, which
is how you know it is a contract defect and not a worker defect:

> Caveat on the SHA: the typed git operations available here return abbreviated hashes only
> (`git_log` → `773cf0f8`); with no shell I cannot print the 40-char form. The commit tool's own
> line was `[fleet/wave-602/flux/story/C-543 773cf0f8]`.

> The exposed typed git operations report abbreviated hashes only (`git_log` → `978387ab`); no
> `rev-parse`/`--format` operation is available here to print the 40-character SHA.

Nothing is lost downstream today — the fleet derives the full SHA host-side from the branch, so the
handoff record is correct — which is precisely why this has survived: the damage is not a wrong
record, it is a worker being asked for something impossible on every single assignment, discovering
that mid-flight, and spending rounds and prose explaining the gap instead of doing the work. An
assignment that cannot be satisfied teaches the worker that its instructions are approximate, which
is a bad thing to teach an agent whose value depends on following them exactly.

Two coherent fixes, and this should pick one rather than both: give the typed git surface a way to
return the full object id, or stop asking for it in the assignment and let the host supply it. The
first is better — "which commit exactly" is a reasonable thing for an agent to be able to state, and
abbreviated hashes are ambiguous by construction.

## Acceptance

- [ ] A writer holding the `git` capability can obtain the full 40-character object id of a commit it made, through a typed operation, without a shell.
- [ ] `git_commit` reports the full id of the commit it just created, so the common case costs no second call.
- [ ] The story-worker assignment and the typed git surface agree: whatever the assignment asks a worker to report, the worker's operations can produce. A test asserts each field the assignment demands is reachable from the capability set the worker is dispatched with.
- [ ] The change does not widen a writer's authority — this reads back an object id the worker itself just created; it is not new reach.
- [ ] Failing first: a test drives a worker-shaped capability set, commits, and asserts a full 40-character id is obtainable — today only the abbreviated form is.

## Notes

Found by reading `wave-602`'s worker transcripts. See also
[C-616](C-616-a-story-worker-authors-its-own-handoff-instead-of-a-third-party-transcribing-it.md):
a worker that records its own handoff natively needs the full id anyway, so both want the same
primitive.
