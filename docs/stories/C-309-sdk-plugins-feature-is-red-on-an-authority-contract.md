---
id: C-309
title: "`flux-sdk --features plugins` is red on an authority-contract violation, and nothing compiled it"
pillar: Core
status: done
areas: [flux-sdk, flux-plugin, ci]
note: "neither of the story's two readings: the projection under-declared `access`, so the effect-less `[Process, Network]` default and the authority contract were mutually unsatisfiable. Fixed on the access side — the effects-side fix would have removed the authorization floor instead"
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

- [x] State which of the two readings is correct, with evidence — when the tightening landed, what it
      changed, and whether any non-fixture caller was affected. → **Neither, and the story was right to
      demand this be settled first.** See Progress: the fixture is not stale and the contract has not
      regressed; two individually-correct mechanisms composed into an unsatisfiable requirement, and
      the naive fix would have opened a hole rather than closed one.
- [x] Both tests in `crates/flux-sdk/tests/plugins.rs` pass, **without weakening the authority
      contract** unless reading (2) is established and argued explicitly. → both pass; the contract in
      `flux-runtime` is untouched. The fix is in the *projection* (`plugin_tool_spec`), which was
      under-declaring `access`.
- [x] `codewandler-flux-sdk/plugins` moves from `skip` to `run` in
      `scripts/check-feature-gated-tests.sh`'s ledger, so CI compiles and runs it from then on. →
      `scripts/check-feature-gated-tests.sh:60`; the ledger's `skip` count is now 2, both cost-based
      (an ONNX runtime; a pure passthrough) rather than defect-based.
- [x] Full gate green, plus `bash scripts/check-feature-gated-tests.sh`. → `cargo test --workspace`,
      `clippy --all-targets`, `cargo fmt --check` green in **both** workspaces; ledger script green.

## Progress

- **The defect is a composition failure, not a stale fixture and not a contract regression.**
  `plugin_tool_spec` (`crates/flux-plugin/src/host/loading.rs`) derived `access` **only** from the
  manifest's `capabilities`, while defaulting an effect-less op to `[Process, Network]`.
  `flux-runtime`'s authority contract (`crates/flux-runtime/src/lib.rs:2857-2862`) refuses any tool
  declaring an effect it holds no matching access for. So *every* effect-less op of a plugin that
  declares no `process` capability was **impossible to load** — the fixture
  (`crates/flux-sdk/fixtures/plugin_fixture.rs`, `effects: Vec::new()` +
  `PluginCapabilities::default()`) is simply the shape that makes it unavoidable. Both mechanisms are
  individually correct; neither is the bug.
- **The fix direction was load-bearing, and the obvious one was wrong.** Relaxing the *effects*
  default (synthesizing only effects the granted capabilities back) makes the tests pass and is
  strictly worse: authority requirements derive from `access`, **not** `effects`
  (`authority_requirements_from_declaration`, `flux-runtime/src/lib.rs:2700`), so a capability-free op
  would project neither — carrying **no** authority requirement at all and skipping the authorization
  floor entirely. `crates/flux-plugin/tests/host.rs:196` exists to defend exactly that and caught the
  attempt.
- **So the fix is on the access side:** `AccessKind::Process` is now **unconditional** for every
  plugin op, because dispatching one is a process interaction with an already-spawned subprocess of
  arbitrary operator-installed code. `capabilities.process` is a narrower thing — the *further*
  programs the plugin may shell out to through the host callback — and conflating the two is what
  under-declared the access. With empty subjects this yields a `process.exec` requirement on
  `ResourceRef::any(Process)`, so effect-less plugin ops are gated more, not less.
- **Blast radius, measured:** the projection change affects every plugin op, but only ops that were
  *already unloadable* change outcome. Across the shipped pack exactly one non-fixture op declares no
  effects (`plugins/host-kit/src/lib.rs:1416`); the CLI-driven plugins (`kubernetes` line 255, `aws`)
  declare `process` capability and so already carried `Process` access. No shipped plugin loses a gate.
- **Regression coverage:** `every_plugin_op_projects_a_loadable_and_gated_authority_contract`
  (`crates/flux-plugin/src/host.rs`) pins **both** halves across three capability shapes — the contract
  must be loadable *and* must still require `process.exec` — so neither this defect nor the tempting
  wrong fix can return. `plugin_coherence_reads_the_projected_spec_not_the_raw_declaration` was
  updated for the corrected projection; it had encoded the pre-fix `access` derivation.

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
