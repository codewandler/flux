---
title: A workflow family in one file
description: A first tutorial for named entrypoints and the git ops — build a small family of deterministic workflows, land a branch with a real merge commit, and undo it without rewriting history.
slug: a-workflow-family-in-one-file
tags: [tutorial, flux-lang, git]
date: 2026-07-30
---

Two features that landed recently pair better than either reads alone: **named entrypoints**
(`flux run <file> --entry <name>`, 0.38.0) let one `.flux` file hold a family of related workflows,
and the **git family** (`git_branch` / `git_merge` / `git_revert`, 0.37.0) lets a flow integrate work
end to end. Together they cover a real job: prepare a branch, land it with a proper merge commit, and
back it out if you were wrong — with the flow, not the model, deciding every step.

Nothing here needs an API key or a network call. Every command and every output below was run against
a throwaway git repository, and pasted verbatim.

<!-- truncate -->

## Set up a scratch repository

Do this somewhere disposable. The flows below make real commits.

```bash
mkdir /tmp/flux-tutorial && cd /tmp/flux-tutorial
git init -q
git config user.email you@example.com
git config user.name "You"
echo "# demo" > README.md
git add . && git commit -qm "initial commit"
```

## One file, several workflows

Before named entrypoints, a file held one workflow and a family meant a file each. Now a module can
declare several top-level flows and the caller picks one by name.

Save this as `repo-notes.flux`:

```flux
flow status -> Any
  state = git_status()
  recent = git_log(limit: 5)
  return { state, recent }

flow start(branch: String) -> Any
  switched = git_checkout(branch: branch, create: true)
  return { switched }

flow save(message: String) -> Any
  git_stage(paths: ["."])
  committed = git_commit(message: message)
  return { committed }

flow land(onto: String, branch: String) -> Any
  git_checkout(branch: onto)
  merged = git_merge(branch: branch, no_ff: true)
  return { merged }

flow undo(commit: String) -> Any
  reverted = git_revert(commit: commit, mainline: 1)
  return { reverted }
```

Five workflows, one file. Two things in that source are worth naming, because both are newer than
most examples you will find:

- **No `$` sigils.** `state = git_status()` binds a result directly. The older `$state = …` spelling
  still parses, but this is the canonical form the formatter now emits.
- **Named inputs look named.** `git_merge(branch: branch, no_ff: true)` instead of wrapping arguments
  in an object literal. `{ state, recent }` is shorthand for `{ state: state, recent: recent }` —
  punning is allowed only where the names are identical, so it cannot quietly change meaning.

Run the read-only one first:

```bash
flux run repo-notes.flux --entry status --yes
```

```
flow  low · mutating · 2 op(s)

→ git_status
  ✓ ?? repo-notes.flux  · exec 6ms + approval 1µs

→ git_log   limit=5
  ✓ fa51d90 initial commit  · exec 6ms + approval 1µs
{"recent":"fa51d90 initial commit","state":"?? repo-notes.flux"}
──────────────────────────────────────────────────────────────── 2 steps · 33ms
```

Every operation is listed with its arguments before its result. That trace is not a log the flow
chose to print — it is the execution path itself, and the ops in it are exactly the ops the source
names. No model decided to call `git_log`; the flow did.

`--entry` reuses the strict authored-flow input path, so a flow's declared parameters are checked.
Ask for a flow that does not exist, or omit a required input, and it fails before doing any work
rather than midway through.

## Landing a branch

Commit the module, then start a branch through the flow:

```bash
git add repo-notes.flux && git commit -qm "add the workflow module"
flux run repo-notes.flux --entry start --arg branch=feature/notes --yes
```

```
→ git_checkout branch="feature/notes", create=true
  ✓ Switched to a new branch 'feature/notes'  · exec 6ms + approval 1µs
```

Make a change and save it. `save` takes its message as an input, so the *commit text* is a caller
decision while the *sequence* — stage, then commit — is fixed by the flow:

```bash
echo "hello" > note.txt
flux run repo-notes.flux --entry save --arg message="add a note" --yes
```

```
→ git_commit message="add a note"
  ✓ [feature/notes 40c5b1f] add a note
     1 file changed, 1 insertion(+)
     create mode 100644 note.txt  · exec 7ms + approval 1µs
```

Now land it. `no_ff: true` forces a real merge commit even where a fast-forward was possible, so the
branch stays visible in the history:

```bash
flux run repo-notes.flux --entry land --arg onto=main --arg branch=feature/notes --yes
```

```
→ git_merge branch="feature/notes", no_ff=true
  ✓ Merge made by the 'ort' strategy.
     note.txt | 1 +
     1 file changed, 1 insertion(+)
```

```
*   082036e Merge branch 'feature/notes'
|\
| * 40c5b1f add a note
|/
* 71fda9e add the workflow module
* fa51d90 initial commit
```

A merge is `Risk::High` and a conflict is a **recoverable** error: it names the conflicting files,
aborts, and restores the tree. It is never left half-merged for something else to trip over. If a
merge is already in progress `git_merge` refuses outright and aborts nothing — that in-flight
resolution may be work someone has not saved.

## Undoing without rewriting

This is the part worth internalising. `git_revert` **appends the inverse** of a commit. It never
resets, and it never rewrites history.

For a merge commit you must say which parent to keep, which is what `mainline: 1` means — the branch
you merged *into*:

```bash
flux run repo-notes.flux --entry undo --arg commit=082036e --yes
```

```
→ git_revert commit="082036e...", mainline=1
  ✓ [main b6f5817] Revert "Merge branch 'feature/notes'"
     1 file changed, 1 deletion(-)
     delete mode 100644 note.txt
    reverted 082036e — b6f5817 Revert "Merge branch 'feature/notes'"
```

```
* b6f5817 Revert "Merge branch 'feature/notes'"
*   082036e Merge branch 'feature/notes'
|\
| * 40c5b1f add a note
|/
* 71fda9e add the workflow module
* fa51d90 initial commit
```

`note.txt` is gone from the working tree, and **both the merge and the original commit are still
reachable**. The record of what happened survives the decision to undo it — which is what you want
when an agent, rather than a person, made the call.

:::warning A rename you may need to act on
The step that discards changes and returns the checkout to a snapshot used to be called `git_revert`,
which described the wrong thing — it resets. It is now **`git_reset`**, and `git_revert` means the
appending, non-destructive operation above. **There is no alias**, so an old flow calling `git_revert`
expecting a reset will not silently do the wrong thing: the new `git_revert` refuses a snapshot
argument instead of resetting anything. If you have flows from before 0.37.0, rename those calls.
:::

## Where to go next

Three more things landed in the same window that extend this shape:

- **Work boards** can be read as structured data, not just text — a flow can loop over items, filter
  to what is ready *and* not blocked by unfinished dependencies in one call, and read back its own
  notes. Since 0.38.0 a board can also hand an item to a different worker when its holder dies, and
  attach evidence such as a commit or PR link to a finished item. See
  [datasources](/docs/agent/datasources).
- **Delegated helpers are visible while they work.** When flux hands work to helper agents, a status
  line shows each one's role, how long since it last did anything, and whether it is working or
  stuck. Their inputs and outputs are never shown — only that activity happened — so a helper
  handling secrets cannot leak them onto your screen. Local helpers today; remote workers stay silent
  for now.
- **A flux program can drive its own release**, having a model draft the changelog prose while the
  version number is derived from commit titles rather than from anything the model says. See
  [improvement](/docs/agent/improvement).

If you want the deterministic-execution model behind all of this, start with
[Flux-Lang overview](/docs/language/overview); for the operation catalogue, see
[operations](/docs/language/ops).
