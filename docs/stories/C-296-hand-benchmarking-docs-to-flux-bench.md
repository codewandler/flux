---
id: C-296
title: "Hand the public benchmarking story to flux-bench, and make `flux eval` point at it"
pillar: Core
status: ready
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

- [ ] The substance of `website/docs/agent/improvement.md` that is really *about benchmarking* moves
      to flux-bench (its `README.md`, or `docs/` if it outgrows the README). **Cross-repo:** this
      story cannot be closed by a flux-side commit alone; say which flux-bench commit carries it.
- [ ] What stays on flux's site is a short, honest stub: flux-bench is where harness benchmarking
      lives, with a link. It must not read as an apology or a redirect notice — a reader landing there
      should get a useful sentence about *what flux-bench measures* before the link.
- [ ] **Separate the two things that page currently conflates.** It documents (a) `flux eval`, a
      measurement and audit harness, and (b) the repository **self-improvement loop**, which is the
      part that is on hold and unproven. Only (a) is what flux-bench replaces. Decide explicitly what
      happens to the self-improvement material — it is still real, still shipped, still runnable, and
      `docs/self-improvement/STATUS.md` is its dated record. Moving it to flux-bench would be wrong;
      deleting it would be dishonest.
- [ ] `flux eval` prints a short pointer to flux-bench. It **runs unchanged** — same suites, same
      exit codes, same output otherwise. A test pins that the pointer does not disturb the machine-
      readable paths (`--report`, any JSON output): a pointer on stdout that lands in a parsed stream
      is a regression, so send it where it cannot corrupt output a caller parses.
- [ ] `flux eval --help` says where the supported benchmark lives.
- [ ] The website contract tests stay green — `crates/flux-cli/tests/website_contract.rs` checks that
      every shipped subcommand is documented and that Flux fences in `website/docs` parse. If the stub
      drops content those tests assert on, fix the test's expectation deliberately rather than
      deleting the assertion.
- [ ] Full gate green in both workspaces.

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
