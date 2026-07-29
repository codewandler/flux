---
id: C-206
title: "Guidance-fragment discovery walks subdirectories while the contract promises a flat directory"
pillar: Core
status: in-progress
priority: 17
epic:
design:
note: "context.rs promises `.flux/context.d` is flat and auditable with one `ls`, but calls the recursive walk_files — a fragment at context.d/sub/x.md loads into the system prompt from a path the docs say is never scanned"
---

# Guidance-fragment discovery walks subdirectories while the contract promises a flat directory

## Goal
`crates/flux-runtime/src/context.rs:22-24` states the contract for path-scoped guidance fragments:

> Where path-scoped guidance fragments live… **A flat directory**, matching how `.flux` already
> houses skills, agents, and flows — so what can load is **auditable with one `ls`**, without a tree
> walk or a mention syntax.

The implementation does the opposite. `context.rs:346` calls
`system.walk_files(FRAGMENT_DIR, FRAGMENT_SCAN_CAP)`, and `walk_files`
(`crates/flux-system/src/lib.rs:1571-1630`) is a stack-based **recursive** walk — it pushes every
non-skipped subdirectory at `:1618`, and its own test `walk_files_lists_recursively_and_skips_noise`
asserts it returns `src/util/helper.rs` from an `src` base. The only filtering `context.rs` applies
to the result is `.ends_with(".md")` (`:352`), with no depth check.

So a fragment at `.flux/context.d/sub/deep/x.md` **does** load into the system prompt, from a
location both the code's own contract and the public documentation say is never scanned. Make the
code match the contract.

## Acceptance
- [x] Failing-first: a test placing a fragment at `.flux/context.d/sub/x.md` asserts it is **not**
      loaded, and fails against the tree as it stands. →
      `context::tests::fragments_ignore_subdirectories`
- [x] Fragment discovery reads only the top level of `.flux/context.d`. Prefer constraining the scan
      at the `context.rs` call site over changing `walk_files`, whose recursive behaviour is correct
      and depended on by `glob`/`grep` (see Notes). → `context.rs` `ContextFragments::render`;
      `walk_files` untouched.
- [x] The `FRAGMENT_SCAN_CAP` bound still applies, and an absent directory is still the tolerated
      common case rather than an error. → `context::tests::fragment_scan_stops_at_the_cap` and the
      pre-existing `fragments_none_when_directory_absent`.
- [x] `website/docs/agent/project-context.md`'s claim — "The directory is flat — subdirectories are
      not scanned, so one `ls` tells you everything that can load" — becomes true in fact, not only
      in intent. No wording change should be needed. → verified: the page's wording is already
      correct, so it is unchanged.

## Progress
- 2026-07-29 — found while adding "Related docs" footers under the
  [website-truth-and-identity](../designs/website-truth-and-identity.md) epic (C-203). Verified
  against the tree at `0.33.1` before filing: the recursion is real, and the `.md` filter is the
  only filter.
- 2026-07-29 — implemented on `impl/C-206`. `ContextFragments::render` now resolves `FRAGMENT_DIR`
  through the same confined workspace (`System::workspace().resolve_read`) and reads that **one**
  directory with `tokio::fs::read_dir`, keeping only entries whose `DirEntry::file_type` is a
  regular file ending in `.md` — which drops subdirectories and symlinks in a single test, since
  `file_type` does not follow links (the escape guard `walk_files` gave for free). Collection stops
  at `FRAGMENT_SCAN_CAP`; the result is still sorted, so the prompt prefix stays stable.
  `flux-system`'s `walk_files` is deliberately untouched — `glob`/`grep` still need the recursion.
  Failing-first evidence: with only the test added, `fragments_ignore_subdirectories` panicked with
  "fragment from a subdirectory leaked into the prompt" listing `## y.md` / `## x.md`; it passes
  after the change. Full gate green (build, 143 test suites 0 failed, clippy `-D warnings`, fmt,
  codegate).

## Notes
- **Severity: auditability, not privilege escalation.** Anyone who can write
  `.flux/context.d/sub/x.md` can equally write `.flux/context.d/x.md`, so the recursion grants no
  capability that a flat scan withholds. What it breaks is the property the design was *chosen* for:
  that one `ls` enumerates everything able to reach the system prompt. A reviewer auditing a repo's
  guidance surface per that promise would miss nested fragments entirely.
- **Do not "fix" this in the docs.** The page and the code comment describe the same deliberate
  contract, and that contract is the right one — a tree walk was explicitly rejected in the original
  design. The defect is in the implementation.
- `walk_files` itself should almost certainly stay recursive: `glob` and `grep` depend on it, and its
  test pins that behaviour. The narrow fix is at the fragment call site — a `read_dir` of the one
  directory, or filtering the walk to entries with no path separator after the base.
- Related: this is the second finding in this class (documented contract vs. actual scan behaviour).
  If a third appears, the mechanical no-drift lint pattern from C-194 may be the better answer than
  another one-off test.
