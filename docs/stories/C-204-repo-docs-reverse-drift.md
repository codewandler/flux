---
id: C-204
title: "Reverse drift — README and docs/usage.md are stale where the website is correct"
pillar: Core
status: done
priority: 16
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "README documents `flux run --program`, a flag that does not exist in args.rs; docs/usage.md is missing 5 shipped subcommands and calls read_many a legacy alias while the registry says to prefer it"
---

# Reverse drift — README and `docs/usage.md` are stale where the website is correct

## Goal
Four of the audit's findings point the other way: the public website is right and the in-repo docs
are wrong. The README is the first thing a new reader sees and it documents a CLI flag that does
not exist. Fix the repo docs against the code — and do **not** "fix" the website to match them.

## Acceptance
- [x] `README.md` no longer documents `flux run --program`. No such argument exists in
      `crates/flux-cli/src/args.rs`; the real spellings are `flux app run <prog.flux>` and bare
      `flux run <app.flux>` (`args.rs:253, 351, 603`). `website/docs/agent/programs.md` and
      `language/modules-and-programs.md:105` already get this right.
- [x] `docs/usage.md` documents all 26 subcommands in `args.rs`. It currently has 21 — missing
      `doctor`, `export`, `record`, `test` and `wakeups`, all of which `website/docs/agent/cli.md`
      already covers.
- [x] The `read_many` contradiction is **resolved, then written down**: `docs/usage.md` calls it "a
      legacy alias" while `crates/flux-tools/src/lib.rs:1669-1671` registers it with *"Read several
      known files in one operation… Prefer this over sequential `read` calls."* Decide which is
      true against the code, then correct whichever document is wrong. The website
      (`language/ops.md:31`) matches the registry.
- [x] `README.md`'s provider table is brought level with `website/docs/agent/providers.md:44-52` —
      it is missing the `fable` alias and the `mock` provider.
- [x] `website/docs/language/overview.md:58` says "two interchangeable representations"; there are
      three front-ends — JSON `DraftAst`, `.flux` text, and the Rust DSL builders (`flux_lang::dsl`,
      re-exported at `crates/flux-sdk/src/lib.rs:315`), as `docs/language.md` §1 states. The DSL is
      acknowledged only at `sdk/overview.md:87`. Name the third on the language overview.
- [x] Verified against the code, not against either document — `args.rs` for the subcommand list,
      the registry for `read_many`, the provider registry for the table.

## Progress

**2026-07-29 — done.** Every claim re-derived from the code first; the audit's counts held up.

### Subcommand count: 26, confirmed independently
Enumerated the top-level variants of `Commands` in `crates/flux-cli/src/args.rs` (lines 250–579)
rather than trusting the audit: `run`, `tui`, `fork`, `a2a`, `eval`, `app`, `flow`, `render`,
`review`, `loop`, `sessions`, `wakeups`, `usage`, `replay`, `record`, `test`, `diff`, `export`,
`auth`, `plugin`, `endpoint`, `skill`, `changelog`, `completion`, `preset`, `doctor` — 26, none
`hide`-annotated, so all 26 are public. `docs/usage.md` covered 21; the five absent were exactly
`doctor`, `export`, `record`, `test`, `wakeups`. All five now have entries in the "Other surfaces"
block, written from the `args.rs` doc comments (which *are* the `--help` text clap renders), with
`website/docs/agent/cli.md` used only as a cross-check. Placement follows the block's existing
grouping: `wakeups` after `sessions`; `export`/`record`/`test` after the `replay`/`fork`/`diff`
Time-Machine run; `doctor` at the end.

### `read_many` — the registry is right, `docs/usage.md` was wrong
`crates/flux-tools/src/lib.rs:1669–1671` registers it as *"Read several known files in one operation
… Prefer this over sequential `read` calls once multiple relevant paths are known."* Nothing in the
tree deprecates it: it is in the built-in catalog assertion at `lib.rs:4191`, it is in the default
tool lists of the shipped eval roles (`crates/flux-eval/agents/{planner,worker}.md`), it is called
by the embedded `strict_review` flow (`docs/designs/strict-review-flows.md`), and
`crates/flux-flow/docs/ops-reference.md:40` documents it as a distinct op with its own guidance
("prefer single `read` when you need to embed a file's text into a later string"). So `read_many` is
a live, first-class op, not an alias.

Provenance of the error: `git log -S` puts the "legacy alias" wording in `4189963` (2026-07-04,
"docs: accuracy sweep"), the same commit that documented `read`'s new array/glob form. The sweep
inferred from `read` gaining an array form that `read_many` had been demoted — a doc-side deduction
that the registry never made. `read` and `read_many` overlap but are not the same call: the model is
steered to `read_many` once it already knows several paths, and to `read` when it needs one file's
text to flow into a later string. Corrected `docs/usage.md` to describe `read_many` as a first-class
bulk read and to quote the registry's actual preference rule. The website
(`website/docs/language/ops.md:29`) already matched the registry and was left alone.

### `flux run --program` does not exist
No `--program` argument anywhere in `args.rs`. Programs are reached two ways: `flux app run
<program.flux>` (`AppAction::Run`, `args.rs:603`, where `program` is a positional), and bare `flux
run <app.flux>` — `crates/flux-cli/src/dispatch.rs:406` routes to the app path when the first prompt
word ends in `.flux`, keying on the extension *only* so a prompt beginning with a filename is still
a prompt. README's Presets-and-programs section now names both spellings and shows both.

### Providers: 9 rows, not 8
`crates/flux-providers/src/spec.rs:17` holds `KNOWN_PROVIDERS` — `anthropic`, `claude`, `openai`,
`codex`, `aws`, `openrouter`, `ollama`, `ollama-anthropic` (8). `mock` is deliberately *not* in that
list: the CLI intercepts it ahead of `spec::build` (`crates/flux-cli/src/execution.rs:69`,
`app_cmd.rs:263`, `review.rs:121`), so `-m mock` is a real selectable provider even though it never
reaches the credentialed factory. That makes 9 user-visible `-m` prefixes, matching
`website/docs/agent/providers.md`. The README table listed 8 and omitted `mock` — despite the
README's own Quickstart already showing `flux run --yes -m mock`. Added the `mock` row.

Bare aliases come from two places: `provider_prefix` (`spec.rs:46`) maps `sonnet`/`opus`/`haiku`/
`fable`/`mock` to `anthropic` and bare `claude`/`codex`/`aws` to themselves, and
`crates/flux-providers/src/anthropic.rs:218` resolves `fable` → `claude-fable-5`. `fable` was the
one Anthropic alias the README never mentioned; the table now carries a following sentence naming
the full bare-alias set rather than widening the table to a fourth column.

### Language overview: three front-ends, not two
`website/docs/language/overview.md` claimed "two interchangeable representations". The third is the
Rust DSL: `flux_lang::dsl`, re-exported as `flux_sdk::dsl` at `crates/flux-sdk/src/lib.rs:315`
("builder primitives that construct the Flux-Lang AST"), demonstrated at
`website/docs/sdk/overview.md` and in `crates/flux-sdk/examples/dsl_loops.rs`. `docs/language.md` §1
already said all three. Renamed the section to "The three front-ends", added the DSL bullet with a
relative link to `../sdk/overview.md` (resolves; no `#anchor` used, so `onBrokenAnchors: 'throw'` is
not at risk), and reworded the follow-up paragraph from "Both are normalized" to "All three are
normalized". No inbound link targeted the old `#the-two-forms` slug — grepped `website/docs`,
`website/src`, `docs/`, `README.md` and `crates/` before renaming.

### Recommendation: extend the coverage guard to `docs/usage.md` — yes
`docs/usage.md` drifted by five subcommands while `website/docs/agent/cli.md` did not, and the only
difference between them is that `cli_reference_covers_every_public_subcommand` reads one and not the
other. That is a mechanical, cheap check with an already-written harness: the test parses `flux
--help`, so extending it is a matter of asserting the same `` `flux {name}` `` substring against a
second file. The one caveat is intent — `agent/cli.md` is a *reference* (a row per command),
`docs/usage.md` is a *surface map* (prose plus one annotated block), so the guard should assert
mention, not completeness of options, which the existing substring test already does. Recommend
extending it; the test itself is out of this story's scope (`crates/flux-cli/tests/website_contract.rs`
is owned centrally).

### Files touched
`README.md`, `docs/usage.md`, `website/docs/language/overview.md`, this story. Nothing else — in
particular `website/sidebars.js` and `crates/flux-cli/tests/website_contract.rs` were left untouched,
and no website page was edited to match a stale repo doc.

## Notes
- This is the one story in the epic that mostly edits files outside `website/`. It is filed here
  because the audit that found it was a website audit, and because leaving it out would mean the
  epic knowingly ships a correct website beside an incorrect README.
- `docs/usage.md` has no coverage test, which is why it drifted while `agent/cli.md` (guarded by
  `cli_reference_covers_every_public_subcommand`) did not. Extending that guard to `docs/usage.md`
  is worth considering while the fix is in hand — record the decision either way.
