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

- `ordinal` is the hunk's 1-based position in the current diff. It is a **readability and
  duplicate-disambiguation device only** — never the integrity check.
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
on any edit anywhere in the file — the agent could stage exactly one hunk per `git_hunks` call and
would have to re-read after each. Content is the identity; position is not. This is what makes
"stage hunk 1, then stage hunk 3" work in the split-author case without a re-read in between.

The cost is that two byte-identical hunks in one file hash the same. The `h{ordinal}-` prefix
disambiguates them, and because the ordinal is compared only after the hash matches, a stale
ordinal can never redirect a selection to unrelated content — worst case it fails to match and the
op refuses.

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
all-or-nothing across the hunks in a single patch, so a partial stage is not a reachable state.

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
  separate hunks naturally.
- **Cross-file selection.** One path per call, which is what keeps `permission_subjects` exact.
- **Staging *deletions* of whole files or renames.** Those have no hunk granularity to select; they
  remain whole-path `git_stage` operations.
