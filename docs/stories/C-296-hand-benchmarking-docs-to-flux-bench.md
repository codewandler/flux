---
id: C-296
title: "Hand the public benchmarking story to flux-bench, and make `flux eval` point at it"
pillar: Core
status: in-progress
priority: 3
areas: [flux-cli, website]
note: "flux-bench is a released standalone benchmark (v0.1.0, 19 stories done) that runs the SHIPPED flux binary; flux's own eval docs still read as the primary path while the pillar behind them is on hold"
---

# Hand the public benchmarking story to flux-bench, and make `flux eval` point at it

## Goal

`website/docs/agent/improvement.md` is flux's public answer to "how do I measure this agent". It
opens with a status admission that the Improvement pillar is **de-prioritized and on hold since
2026-07-06**, and that the pillar's headline claim — a repeatable, grader-confirmed gain at
trials ≥ 3 — is *not proven*. Meanwhile [`flux-bench`](https://github.com/codewandler/flux-bench)
shipped v0.1.0 with nineteen stories done, benchmarks the **harness** rather than the model, and runs
the *shipped* flux binary against a corpus with the model held fixed and verified fixed.

So the supported answer and the documented answer are different tools. Close that gap in the
direction reality already went.

**Two decisions are already made and are not open for re-litigation** (user, 2026-07-30):

1. **flux-bench does not get its own published docs site.** The substance moves into flux-bench's
   `README.md` and `docs/`. It has no Docusaurus and no gh-pages workflow — only `ci.yml` and
   `pin-freshness.yml` — and standing a second site up is not worth it.
2. **`flux eval` keeps working exactly as it does today.** It gains a pointer, not a deprecation
   warning and not a removal. No Action-needed item, nothing breaks for anyone scripting it.

## Acceptance

- [x] The substance of `website/docs/agent/improvement.md` that is really *about benchmarking* moves
      to flux-bench (its `README.md`, or `docs/` if it outgrows the README). **Cross-repo:** this
      story cannot be closed by a flux-side commit alone; say which flux-bench commit carries it.
      → flux-bench `a3da27a` on branch `impl/C-296` (docs only): `docs/from-flux-eval.md`, plus
      `README.md`, `docs/README.md`, `CHANGELOG.md` and a receiving story **B-22**.
- [x] What stays on flux's site is a short, honest stub: flux-bench is where harness benchmarking
      lives, with a link. It must not read as an apology or a redirect notice — a reader landing there
      should get a useful sentence about *what flux-bench measures* before the link.
      → `website/docs/agent/improvement.md` "Benchmarking the harness: flux-bench" — what it
      measures, the noise floor, gradeable restraint, then the link.
- [x] **Separate the two things that page currently conflates.** It documents (a) `flux eval`, a
      measurement and audit harness, and (b) the repository **self-improvement loop**, which is the
      part that is on hold and unproven. Only (a) is what flux-bench replaces. Decide explicitly what
      happens to the self-improvement material — it is still real, still shipped, still runnable, and
      `docs/self-improvement/STATUS.md` is its dated record. Moving it to flux-bench would be wrong;
      deleting it would be dishonest.
      → **Decision: the loop stays in flux, in full, and the page now says why** ("Why the loop lives
      in this repository"): it edits flux's own tree, and an instrument the measured harness can
      rewrite is exactly what flux-bench's vision principle 1 forbids. The on-hold status note is
      re-scoped from the whole page to the loop section, so it no longer reads as a disclaimer over
      `flux eval`. The dated record link is kept.
- [x] `flux eval` prints a short pointer to flux-bench. It **runs unchanged** — same suites, same
      exit codes, same output otherwise. A test pins that the pointer does not disturb the machine-
      readable paths (`--report`, any JSON output): a pointer on stdout that lands in a parsed stream
      is a regression, so send it where it cannot corrupt output a caller parses.
      → `bench_pointer()` in `crates/flux-cli/src/review.rs` writes to **stderr**; pinned by
      `crates/flux-cli/tests/bench_pointer.rs::the_bench_pointer_goes_to_stderr_and_never_into_stdout_or_the_report`.
- [x] `flux eval --help` says where the supported benchmark lives.
      → `crates/flux-cli/src/args.rs` `after_help` "BENCHMARKING THE HARNESS"; pinned by
      `bench_pointer.rs::eval_help_names_where_the_supported_harness_benchmark_lives`.
- [x] The website contract tests stay green — `crates/flux-cli/tests/website_contract.rs` checks that
      every shipped subcommand is documented and that Flux fences in `website/docs` parse. If the stub
      drops content those tests assert on, fix the test's expectation deliberately rather than
      deleting the assertion.
      → 19/19 green with **no expectation changed**: the page keeps its path (so every inbound link
      and the sidebar entry still resolve) and `cli.md` keeps its `flux eval` row.
- [x] Full gate green in both workspaces.

## Progress

- **Cross-repo pairing.** flux-bench carries the migrated substance in commit **`a3da27a`** on branch
  `impl/C-296` (`docs/from-flux-eval.md` + README/docs-map/CHANGELOG + story B-22, `status: done` so
  the generated board is untouched). Neither repo is pushed.
- **What "moved" actually means.** flux-bench's `README.md`/`vision.md` already carried the noise
  floor, gradeable restraint, and the saturation argument. The genuinely new material was the
  *practice*: how many trials a claim needs, when a case is unscoreable, how to audit a score back to
  its run, and a `flux eval` → `fluxbench` command mapping. That is what `from-flux-eval.md` holds,
  rather than a copy of text flux-bench already had.
- **`flux eval` output routing.** The pointer is on stderr, following the precedent
  `crates/flux-cli/tests/sandbox_posture.rs` documents for the resolved-posture disclosure. stdout
  keeps the scored summary and `--report` keeps a byte-identical Markdown artifact.
- **The failing-first test caught its own fixture.** The first version named its temp dir
  `flux-bench-pointer-…`, and `flux eval` echoes the `--report` path on stdout — so the "not on
  stdout" assertion went green for the wrong reason. The tag is now `bptr`, with a comment saying
  why.
- **`cargo test -p flux-codegate` bounced the new test once** (C-262): a bulk-argv spawn with no
  declared sandbox posture. Fixed by declaring `FLUX_SANDBOX=off` in the spawn — honest here because
  the zero-task filter means no case, and therefore no child process, ever runs.
- **Two intermittent failures were seen once each, in crates this diff does not touch**
  (`codewandler-flux-runtime` `metadata::tests::skill_directory_with_no_frontmatter_name_takes_directory_name`,
  `codewandler-flux-sdk` `tests::sdk_skills_require_an_explicit_agent_spec` — both skill-discovery
  tests, the latter failing with a bare `NotFound`). Neither reproduced afterwards: the full gate was
  re-run clean twice on the branch and twice at the detached merge base, and each crate's lib suite
  clean on both. They read as load-dependent races inside their own test binaries; this diff adds
  only `flux-cli` source and one `flux-cli` integration test, neither of which those suites link.
  Not proven pre-existing — recorded because it was observed, not because it was diagnosed.

## Notes

- ⚠ **Do not touch `crates/flux-eval`.** This story is documentation plus one CLI pointer. The
  evaluation crate keeps working; the self-improvement loop keeps working. Nothing here is a code
  deprecation, and nothing here is a breaking change.
- flux-bench's own framing, worth reusing rather than rewriting: *"A benchmark for a coding-agent
  harness — the system prompt, the built-in tools, the agent loop — rather than for a model."* Its
  differentiator is that **restraint is gradeable**: a case can forbid an action, matched against the
  tool call's input on flux's `--stream-json` wire, so "did not hijack the user's audio device" is a
  measurable outcome rather than a hope.
- The three flux pages that mention eval today: `website/docs/agent/improvement.md` (the main one),
  `website/docs/reference/config.md`, `website/docs/agent/cli.md`. Check all three for claims that
  become stale.
- ⚠ The flux → flux-bench link direction matters for release cadence: flux-bench runs the **shipped**
  flux binary, so it depends on flux releases and not the other way round. A flux page linking to
  flux-bench is safe; flux-bench pinning a flux version is its own concern in that repo.
