---
id: C-309
title: "`flux-sdk --features plugins` is red on an authority-contract violation, and nothing compiled it"
pillar: Core
status: ready
priority: 6
areas: [flux-sdk, flux-plugin, ci]
note: "second live instance of C-308's class, and this one sits on the safety envelope: the fixture plugin declares a process effect without process access, so both tests fail to load it. Quarantined in the feature-gate ledger so CI stays honest until it is fixed"
---

# `flux-sdk --features plugins` is red on an authority-contract violation, and nothing compiled it

## Goal

Fix the two red tests behind `codewandler-flux-sdk`'s `plugins` feature, then move it from `skip` to
`run` in the feature-gate ledger so it can never rot unobserved again.

## The defect

```
cargo test -p codewandler-flux-sdk --features plugins --test plugins
invalid authority contract for 'fixture.upper' from 'plugin:fixture':
tool 'fixture.upper' declares a process effect without process access
```

Both tests in `crates/flux-sdk/tests/plugins.rs` fail this way, and they fail **at the merge base** —
this is pre-existing, not caused by C-308.

## Why it matters more than its sibling

C-308 found nine feature configurations that no gate compiled. Eight were merely unobserved. **This
one was unobserved *and* already broken**, and what it is broken on is not incidental: the authority
contract is part of the safety envelope. `AGENTS.md` states the rule this fixture violates —
a tool declaring an effect it has no matching access for is exactly what the contract exists to
refuse.

So there are two possible readings, and the story's first job is to decide which:

1. **The fixture is stale.** An authority-contract tightening landed, every real caller was updated,
   and this fixture was missed because nothing compiled it. Then the fix is to correct the fixture —
   and the interesting question is what *else* went unchecked for the same reason.
2. **The contract regressed.** The tightening is wrong or over-broad, and this fixture is a legitimate
   shape the envelope should admit. Then the fix is in the contract, and it is a safety change.

Do not assume (1) because it is cheaper. Establish which, in writing, before changing anything.

## Acceptance

- [ ] State which of the two readings is correct, with evidence — when the tightening landed, what it
      changed, and whether any non-fixture caller was affected.
- [ ] Both tests in `crates/flux-sdk/tests/plugins.rs` pass, **without weakening the authority
      contract** unless reading (2) is established and argued explicitly.
- [ ] `codewandler-flux-sdk/plugins` moves from `skip` to `run` in
      `scripts/check-feature-gated-tests.sh`'s ledger, so CI compiles and runs it from then on.
- [ ] Full gate green, plus `bash scripts/check-feature-gated-tests.sh`.

## Notes

- Found by **C-308**'s audit, which is the story that made this class visible at all. It could not fix
  it in place: wiring the feature into CI while it was red would have failed the build on a defect that
  story did not cause, so it is quarantined as a `skip` with the reason printed on **every** run —
  visible rather than silent.
- ⚠ Three further holes from the same audit are **not** this story's and are recorded so they are not
  lost:
  - `cargo build -p codewandler-flux-plugin --no-default-features --features host` **does not compile**
    (`E0433`) — a feature combination the manifest advertises and no gate builds.
  - Nothing compiles `flux-events`/`flux-flow` **without** `sqlite`, the driver-free configuration
    C-274 created deliberately. A build-coverage hole rather than a test-coverage one.
  - `codewandler-flux-capabilities/local-embeddings` gates code with **zero tests**, so
    `clippy --workspace --all-targets` never compiles that file at all.
- Also worth knowing: C-308 deliberately added **no clippy legs** for the newly-run features, because
  `--features postgres` already fires `large_enum_variant` locally but not in CI, and it did not want
  to import that divergence. Feature-specific clippy coverage remains a real gap.
