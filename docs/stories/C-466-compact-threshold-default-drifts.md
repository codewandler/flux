---
id: C-466
title: "The CLI hard-codes the compaction default twice instead of reading the constant it already depends on"
pillar: Core
status: ready
priority: 7
areas: [flux-cli, flux-agent]
note: "spun out of C-441: execution.rs writes 48_000 twice (and \"48000\" in a warning string) while flux-app reads DEFAULT_COMPACT_THRESHOLD_CHARS; the new website pin reads the constant, so the CLI can drift away from documentation that still looks verified"
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

- [ ] A failing-first test: changing `DEFAULT_COMPACT_THRESHOLD_CHARS` changes the CLI's effective
      default. It must fail at the merge base.
- [ ] `crates/flux-cli/src/execution.rs` has no literal copy of the default left — including the one
      inside the warning message, which should interpolate.
- [ ] The precedence is unchanged: `FLUX_COMPACT_CHARS` > default, `0` disables, and a malformed value
      still warns rather than silently reverting (the C-4xx reasoning at `:345-347` stays).
- [ ] ⚠ Grep the tree for any other copy of the number before closing — this story is only worth doing
      once, and a fourth surface holding a literal defeats it.

## Notes

- Not the same bug as [C-462](C-462-compaction-threshold-is-context-window-blind.md), which asks
  whether 48,000 is the *right* number. This story asks that there be **one** of it. C-462 gets
  materially easier once this lands, because there will be a single place to change.
- ⚠ There is a third divergence in the same area, and it is a behaviour difference rather than
  duplication: a malformed `FLUX_COMPACT_CHARS` **warns** on the CLI path (`execution.rs:344`) but is
  **silently ignored** on the served path — `app.rs:1819-1822` does `.ok().and_then(|s| s.parse().ok())`,
  so `FLUX_COMPACT_CHARS=48k` falls through to the default with no trace at all. An operator typos the
  env var on a served agent and gets no signal. Fix it here or file it; do not leave it unmentioned.
- Related: [C-441](C-441-context-management-doc.md) (the documentation and the pin),
  [C-465](C-465-compact-claims-success-on-five-no-ops.md) (the other defect from the same review).
- Filed 2026-08-02 out of C-441's review.
