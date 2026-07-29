# Design: hunk-level git staging

**Status:** implemented · **Story:** [C-92](../stories/C-92-git-hunk-level-ops.md)

## Problem

`git_stage` takes whole paths. When two authors are editing one file — the common case for a
shared `engine.rs` or a shared design doc — an agent that wants to commit *its* change has exactly
two options: sweep the coworker's in-flight hunks into its own commit, or hand the task back to the
human. `git add -p` solves this for humans; the guarded git surface has no equivalent, and it
cannot grow one by copying `add -p`, because the guarded envelope has no interactive TTY.

So the op must take an **explicit hunk selection**. The whole design reduces to one question:

> what is a hunk *selector*, such that it still means the right thing when the file moves
> underneath you between the moment the agent reads the hunks and the moment it stages one?

That window is not hypothetical. It is the entire premise of the story: a coworker is editing the
same file, concurrently, right now.

## The decision

**Content-addressed hunk IDs, re-verified against a freshly-computed diff at stage time.**

A hunk's identity is a hash of *what it changes*, not *where it sits*:

```
id = "h{ordinal}-{16 hex of SipHash(path \x01 hunk-body-lines)}"
```

- `ordinal` is the hunk's 1-based position in the current diff. It is a readability and
  duplicate-disambiguation device; the hash is the integrity check. **Both halves are compared**,
  because matching is a full-string equality on the id — see the correction below.
- The hash covers the path and the hunk body verbatim (each `+`/`-`/` ` line, including its
  prefix). It deliberately **excludes the `@@ -a,b +c,d @@` line numbers**.

Two ops:

- `git_hunks { path }` — read. Runs `git diff --unified=3 -- <path>` (index vs worktree), splits
  the output into hunks, and returns each one's `id`, its `@@` header, its add/remove counts, and
  its verbatim patch text so the model can see what it is choosing.
- `git_stage_hunks { path, hunks: [id, …] }` — write. **Recomputes the diff itself**, recomputes
  the IDs, matches the requested ones, reassembles a patch from only those hunks, and feeds it to
  `git apply --cached`.

The staging op never trusts a snapshot the caller carries. The IDs it matches against are derived
from the working tree *as of the staging call*.

## Why line numbers are excluded from the hash

If hunk 1 is staged (or edited), every later hunk's `@@` line numbers shift, even though nothing
about those hunks changed. Including position in the identity would invalidate every ID in the file
on any edit anywhere in the file. Content is the identity; position is not.

### Correction (C-92 rework): the ordinal *is* compared, so ids do not survive a stage

An earlier draft of this note claimed the above bought "stage hunk 1, then stage hunk 3 without a
re-read in between", and that "the ordinal is compared only after the hash matches". **Both were
false for the shipped code**, and the code is what is right here. Matching is a plain equality on
the whole `h{ordinal}-{hash}` string, and the ordinal is assigned from the hunk's position in the
*current* diff — so staging hunk 1 renumbers hunk 3 to hunk 2 and its previously-issued id stops
matching. A re-read is required after every stage.

This is a real limitation, and it was left in place deliberately rather than fixed, because the two
properties are not jointly achievable:

- *Surviving renumbering* requires dropping the ordinal from the comparison, i.e. matching on the
  hash alone.
- *Distinguishing two byte-identical hunks* requires position, because their hashes are equal by
  construction.

Matching on the hash alone would close the first at the cost of the second, and the second is the
safety-relevant one: with hash-only matching, reverting one of two identical hunks would let a
stale id silently resolve to the surviving one — a redirect to a different region of the file.
Keeping the ordinal in the comparison fails *safe* instead: the selection stops matching and the op
refuses. The cost is a re-read per stage, which is cheap; the alternative costs a wrong stage.

What was genuinely wrong was the *diagnosis*: the refusal used to say "the file changed underneath
this selection" even when nothing had changed but the numbering. `stale_hunk_guidance` now compares
the hash halves and reports renumbering as renumbering.

Note also that the id excluding line numbers still earns its keep: it is what makes an id survive an
edit *elsewhere in the file* between the read and the stage, which is the concurrent-coworker case
the story is actually about.

## Why not the alternatives

**Bare positional index (`hunks: [0, 2]`).** Cheapest to implement and the worst failure mode
available. If the coworker's editor saves between the read and the stage, index 2 is now a
different hunk — and the op stages *their* work into *our* commit, silently, which is precisely the
bug this story exists to prevent. A selector that cannot detect drift is worse than no selector,
because it converts a visible refusal into an invisible wrong commit.

**Caller-supplied unified-diff patch bytes.** Drift *is* detected — `git apply` rejects a patch
whose context no longer matches — so this is safe in the way the positional index is not. It is
rejected for two other reasons. First, it makes the model the author of executable patch text;
whitespace and context corruption in model-echoed diffs is a routine, well-known failure, and it
turns a *selection* into an *authoring* task with a far larger error surface. Second, it inverts
the trust direction: a guarded op should derive the bytes it applies from the trusted working tree,
not accept them from model input. Under the content-ID scheme, model input is only ever a lookup
key — the bytes handed to `git apply` are always ones flux itself just read out of `git diff`.

**Context-hash without re-verification** (hash the hunk, but apply from the caller's snapshot) —
same authoring problem, and it drops the second line of defence below.

## Two independent staleness checks

The refusal path (Acceptance 3) is not one check but two, and they catch different things:

1. **ID miss.** The requested ID is absent from the freshly-computed diff. This covers "the hunk's
   own content changed", "the hunk was already staged", and "the file was reverted". The op returns
   a recoverable `ToolResult::error` naming the missing IDs and listing the IDs that *do* currently
   exist, plus a "re-run `git_hunks`" instruction — the fs-tool guidance pattern from C-32, so the
   planner repairs in-turn instead of halting.
2. **`git apply` rejection.** Even with every ID matched, the reassembled patch can fail to apply —
   most plausibly when the *staged* content diverged (the index moved, not the worktree). `git
   apply --cached` refuses, and its stderr is surfaced as a recoverable error too.

Neither path raises a plan-halting `Err`. Both leave the index untouched: `git apply` is
all-or-nothing across the hunks in a single patch.

**That guarantee is conditional, and C-92's rework is what makes it hold.** `git apply` is
all-or-nothing over the patch *it receives*, which is only the patch flux sent if the transfer was
complete and well-formed. Two things had to change:

- The apply invocation used to pass `--recount`, which tells git to recompute each hunk's line
  counts from the body it actually read instead of trusting the header — and therefore to *stop*
  rejecting a patch whose body and header disagree. `--recount` is unnecessary here (dropping
  earlier hunks does not invalidate the remaining hunks' old-side counts, because the index is the
  preimage), and with it a patch truncated at a line boundary applied cleanly with exit 0, staging
  a partial result. Verified against git 2.55.0: dropping the last line of a two-hunk selection
  gave `APPLIED exit=0` with `--recount` and `error: corrupt patch at <stdin>:20` without it. It is
  now omitted, so git's corrupt-patch check is back on.
- `System::run_with_stdin` discarded the result of its stdin write. A short write closes the pipe
  on drop and delivers a *clean* EOF, so the child cannot tell truncation from completion. The
  write result is now checked, and a failed write under a zero exit is a hard error. A failed write
  under a *non-zero* exit is passed through untouched — that is the ordinary "git rejected the
  patch and stopped reading" case, where the exit code carries the real diagnosis.

## Guarded-IO shape

Both ops run `git` argv-only through `ctx.system()` — no second `Command::new`, workspace-pinned
cwd, env-cleared, output-capped, exactly like the existing `git_*` family.

`git apply --cached` needs the patch on **stdin**. Every existing `System::run*` helper nulls
stdin, so this adds one variant, `System::run_with_stdin`, which routes through the same
`build_command` choke point and the same `await_process` capture as `run_with_env` — it changes
only `Stdio::null()` → `Stdio::piped()` for stdin and writes the bytes. The alternative, writing
the patch to a file in the user's workspace and passing its path, would mean a guarded op leaving
litter in the tree it is trying to protect.

## Metadata

| | `git_hunks` | `git_stage_hunks` |
|---|---|---|
| `effects` | `[Process]` | `[Process, LocalSystem]` |
| `access` | `[Process]` | `[Process]` |
| `risk` | `Low` (I1-exempt, see below) | `Medium` |
| `idempotency` | `Conditional` | `Conditional` |
| `permission_subjects` | `[path]` | `[path]` |
| `semantic_effects` | `[]` | `[]` |

`permission_subjects` is the named path in both cases — never empty. Staging is a workspace
mutation scoped to exactly one file, so that is the subject the approval gate must see; an empty
subject list on a `Medium`-risk mutation would match a `*` path grant and is the specific hole
AGENTS.md's "`permission_subjects` must be accurate" invariant closes.

`git_hunks` joins `git_status` / `git_diff` / `git_log` in `flux_spec::coherence`'s I1 allowlist,
on the same grounds and no wider: its argv is fixed by the op (`git diff --unified=3 -- <path>`),
the caller may only narrow it to a path, and it mutates nothing. It is `git diff` with a parser
attached. I3 still applies — its result tracks the working tree rather than its input, hence
`Conditional`, not `Idempotent`.

Both declare no semantic effects: neither reaches a durable store, a model, or the network. Both
specs are checked by `flux_spec::metadata_violations` in test rather than by inspection.

## Non-goals

- **Interactive `add -p` prompting.** No TTY in the guarded envelope; the selection is explicit.
- **Splitting a hunk** (`add -p`'s `s`). A caller wanting finer granularity can lower the context
  radius; `git_hunks` exposes `context` for that, and a smaller radius splits adjacent changes into
  separate hunks naturally. **The radius floor is 1, and 0 is rejected** (C-92 rework): a
  zero-context diff has no context lines for `git apply` to verify against, so it anchors on line
  numbers alone and will stage a pure insertion at the wrong offset while exiting 0 — observed
  against git 2.55.0, where a hunk requested at line 6 of a 30-line file landed at EOF. That is the
  precise failure this design exists to prevent, so the radius that enables it is refused rather
  than supported. `--unidiff-zero` makes the case land correctly, but only by switching the
  verification off, which trades the guarantee for granularity; `context: 1` keeps the check and
  still splits far more finely than the default.
- **Cross-file selection.** One path per call, which is what keeps `permission_subjects` exact.
- **Staging *deletions* of whole files or renames.** Those have no hunk granularity to select; they
  remain whole-path `git_stage` operations. This is now **enforced, not merely documented** (C-92
  rework): the diff preamble is copied verbatim alongside whichever hunks are selected, so a
  `deleted file mode` / `rename from` / `copy from` / mode-change header would otherwise be honoured
  even though the caller only picked content hunks — staging a deletion nobody selected.
  `git_stage_hunks` refuses such a diff and points at `git_stage`.
