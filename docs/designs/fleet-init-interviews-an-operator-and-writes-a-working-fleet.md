# Design — `fleet:init` interviews an operator and writes a working fleet

## Why

Standing up the fleet in this workspace took many hours across several sessions, and almost none of
that time went into decisions an operator would consider interesting. It went into discovering
constraints that only announce themselves as failures:

- A worker's `tools:` list must stay inside the live operation ceiling; a list naming one unsurfaced
  toolchain group killed every worker instantly, with the reason visible only in `events.ndjson`.
- Toolchain groups surface from root markers, so naming `npm` for a repository without a
  `package.json` at the configured root silently produces an unrunnable loop.
- A member's items are read at its `canonical_ref`, so a story that is committed but unpushed does not
  exist as far as the Board is concerned — and a fleet pinned to `origin/main` cannot schedule work it
  can plainly see on disk.
- `max_workers` is not width. Width is disk, then memory. Configuring eight workers on a filesystem
  that fits one produces a fleet that dispatches one worker and reports no reason.
- All four artifacts — `board.toml`, `fleet.toml`, agent instructions, loop files — must agree about
  repository ids, roots and refs, and a disagreement surfaces much later as an unrelated error.

None of these are choices. They are consequences, and someone configuring a fleet for the first time
learns each of them by losing a wave. The knowledge already exists in this repository's designs,
stories and hard-won comments, which is exactly the kind of knowledge a guided setup can carry.

## Approach

**An interview, not a template.** `flux fleet init` asks the operator what only they know, derives
everything that follows, and writes the whole set of artifacts consistently. The questions are the
short list of genuine decisions:

- Which repositories are in this fleet, and where are their roots?
- For each, which ref is the truth for planning — and is that ref advanced locally or by publishing?
- What is the repository's gate?
- Which provider and model, for the coordinator and for workers?
- Where may build artifacts live, and how much of that filesystem may the fleet use?
- May a decision that needs a human proceed unattended, or must it stop?

**Everything else is derived and shown, not asked.** Capability bundles from detected toolchain
markers; the operation ceiling from those bundles; the worker `tools:` list intersected with the live
ceiling; fences from the repository's shared ledgers; width from measured free space on the chosen
build filesystem and from total memory. Each derivation is reported with its reason, because the value
is not the number — it is knowing why it is that number.

**Detection precedes every question it can answer.** Root markers, existing boards, remotes and their
tracking refs, available toolchains, free space, memory, and whether a sandbox backend exists on this
platform. An interview that asks what it could have measured wastes the operator's attention and
invites a wrong answer.

**It ends by proving itself, not by claiming success.** `init` finishes with the validation an operator
would otherwise run by hand: every authored loop analysed, every worker `tools:` list checked against
the live ceiling, every configured ref resolved, every gate command found on `PATH`, and one dry-run
wave planned end to end without writing. A fleet that cannot pass that is not written to disk as
though it were fine — the refusal names which answer to change.

**Idempotent and non-destructive.** Re-running against a configured fleet reports the diff and asks
before touching anything. Runtime state is never rewritten; `init` writes configuration and
instructions only.

**A skill, then a command.** The interview is worth having as `/fleet:init` for conversational setup,
but the derivations and the final validation belong in the binary, so the CLI and the skill cannot
drift and the same checks run unattended.

## Stories

- `flux fleet init` detects the environment it will configure — roots and markers, boards, remotes and
  tracking refs, toolchains, free space, memory, sandbox availability — and reports each finding.
- The interview asks only the genuine decisions, and derives capabilities, ceiling, worker tools,
  fences and width from detection, showing each derivation's reason.
- `init` writes `board.toml`, `fleet.toml`, agent instructions and loop files as one consistent set,
  or writes nothing.
- `init` validates before finishing: loops analysed, tool lists inside the live ceiling, refs resolved,
  gate commands present, one dry-run wave planned without writing.
- Re-running `init` on a configured fleet reports a diff and never rewrites runtime state.
- A `/fleet:init` skill drives the same derivations and validation as the command.
