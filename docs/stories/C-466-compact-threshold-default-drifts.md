---
id: C-466
title: "The CLI hard-codes the compaction default twice instead of reading the constant it already depends on"
pillar: Core
status: done
priority: 7
areas: [flux-cli, flux-agent]
note: "the CLI resolver has no literal default copy and now delegates malformed/missing handling to the shared flux-agent contract; C-507 closed the served-path diagnostic divergence"
---

# Two surfaces, one number, no link

## Goal

Have the CLI's compaction default come from `DEFAULT_COMPACT_THRESHOLD_CHARS`, so the served path, the
CLI path and the documentation cannot drift apart.

## The finding

`crates/flux-agent/src/lib.rs:168` owns the number:

```rust
pub const DEFAULT_COMPACT_THRESHOLD_CHARS: usize = 48_000;
```

`crates/flux-app/src/app.rs:1823` reads it. `crates/flux-cli/src/execution.rs:342` does not — it writes
the literal **twice**, plus a third copy in prose:

```rust
Ok(s) => s.parse().unwrap_or_else(|_| {
    eprintln!("... using the default 48000", ...);   // :349  — third copy, in a string
    48_000                                           // :352
}),
Err(_) => 48_000,                                    // :354
```

`flux-cli` already depends on `flux-agent` (it constructs `AgentSpec`), so there is no dependency
argument for the duplication.

⚠ **What makes this worth a story rather than a tidy-up:** C-441 added
`crates/flux-cli/tests/website_contract.rs:1830`, which pins the documented threshold by parsing
`pub const DEFAULT_COMPACT_THRESHOLD_CHARS: usize = ` out of `flux-agent`. So the pin verifies the
*constant* against the *website*. Change the constant and the pin fires; but the CLI keeps using
48,000 regardless, and the pin stays green while the CLI contradicts the page it just validated. The
guard now covers two of the three surfaces and reads as if it covered all three.

## Acceptance

- [x] A failing-first test: changing `DEFAULT_COMPACT_THRESHOLD_CHARS` changes the CLI's effective
      default. It must fail at the merge base.
- [x] `crates/flux-cli/src/execution.rs` has no literal copy of the default left — including the one
      inside the warning message, which should interpolate.
- [x] The precedence is unchanged: `FLUX_COMPACT_CHARS` > default, `0` disables, and a malformed value
      still warns rather than silently reverting (the C-4xx reasoning at `:345-347` stays).
- [x] ⚠ Grep the tree for any other copy of the number before closing — this story is only worth doing
      once, and a fourth surface holding a literal defeats it.

## Progress

- 2026-08-03: added the failing-first
  `cli_compaction_resolution_tracks_the_agent_default_and_preserves_env_precedence` test before its
  pure resolver existed; it failed to compile on the missing seam. The finished test pins the
  agent-owned fallback, explicit values, `0`, malformed fallback, and the absence of numeric/prose
  copies in the resolver body.
- 2026-08-03: `compact_threshold_from_env` now returns
  `DEFAULT_COMPACT_THRESHOLD_CHARS` for missing or malformed values, and the warning interpolates
  that same constant. The environment-reading wrapper remains the production entry point, so
  precedence and behavior are unchanged.
- 2026-08-03: a scoped Rust sweep found no other compaction-default copy. Other `48_000` literals are
  48 kHz audio sample rates; website prose remains intentionally numeric and is already checked
  against the owner constant by C-441's website contract.
- 2026-08-03: filed the served-path silent malformed-value behavior as
  [C-507](C-507-served-compaction-env-typo-is-silent.md), keeping diagnostic behavior out of this
  no-behavior-change consolidation. C-507 subsequently closed it with one shared parse/outcome
  contract for the CLI and served paths.

## Notes

- Not the same bug as [C-462](C-462-compaction-threshold-is-context-window-blind.md), which has now
  decided 48,000 is the right fixed default. This story asks that there be **one** source of that
  intentional value, so the CLI cannot drift from the decision and its documentation.
- The third divergence in the same area was a behaviour difference rather than duplication: a
  malformed `FLUX_COMPACT_CHARS` warned on the CLI path but was silently ignored on the served path.
  [C-507](C-507-served-compaction-env-typo-is-silent.md) subsequently closed it by moving parsing and
  the surface-neutral diagnostic into `flux-agent` and making both surfaces render that outcome.
- Related: [C-441](C-441-context-management-doc.md) (the documentation and the pin),
  [C-465](C-465-compact-claims-success-on-five-no-ops.md) (the other defect from the same review).
- Filed 2026-08-02 out of C-441's review.
