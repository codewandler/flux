# Design: plugin protocol decoupling — a release that leaves the plugin pack alone

**Status:** planned 2026-07-28 · **Pillar:** Core · **Stories:** [C-141](../stories/C-141-relocate-flow-effect-cut-flux-lang-edge.md), [C-142](../stories/C-142-extract-flux-plugin-protocol-crate.md), [C-143](../stories/C-143-independent-protocol-version-line.md), [C-144](../stories/C-144-enforce-protocol-marker-and-wire-fixtures.md), [C-145](../stories/C-145-old-binary-compatibility-test.md), [C-146](../stories/C-146-publish-only-changed-crates.md), [C-147](../stories/C-147-cut-script-ergonomics.md)

## Why

The plugin pack pays a release tax it does not owe. `scripts/cut-release.sh` rewrites
`plugins/Cargo.toml`'s version pins, bumps `plugins/host-kit/Cargo.toml` in lockstep, and re-locks
the nested workspace on **every** flux cut — `plugins/Cargo.lock` changed in five of the last eight
commits that touched it, every one a release cut. The wire contract those plugins actually speak
has changed twice in its history (`3d9178b` guest-dependency partition, `5e80cfe` C-90's optional
`process` field — additive both times).

Three findings, each verified against the tree at 0.28.0:

1. **Nothing enforces host↔plugin compatibility.** `crates/flux-plugin/src/protocol.rs:10` defines
   `PROTOCOL: &str = "flux.plugin.v1"` and stamps it into every `Frame`, but no host code reads it
   back — there is no version check anywhere in `crates/flux-plugin/src/host.rs`, and the pack
   index carries no compatibility metadata. The version lockstep is a ritual standing in for a
   contract nobody wrote down.
2. **Every plugin compiles `flux-lang`.** `flux-plugin` depends on it non-optionally and the guest
   wire surface names exactly one type from it: `FlowEffect` (`protocol.rs:7` and `:140`, where
   `semantic_effects` is documented as a *tag vocabulary*). `cargo tree -i codewandler-flux-lang`
   from `plugins/` shows `flux-lang → flux-plugin → host-kit → all 21 plugins`, pulling a 75-crate
   subtree (flux-core, flux-policy, rowan, futures, sha2, …) into every plugin build for one enum.
   C-69 partitioned the guest dependencies but stopped short of this edge.
3. **`host-kit` republishes on every flux release** as one of the 28 crates in the closure, with
   unchanged content.

## Approach

**One new crate on its own semver line.** `crates/flux-plugin-protocol`
(`codewandler-flux-plugin-protocol`) starts at `1.0.0` and moves only when the wire format moves —
never because flux cut a release. `flux-plugin` keeps the host half and re-exports the wire types,
so no host call site changes.

The dependency shape the epic is aiming at:

```
crates/flux-plugin-protocol  1.0.0        wire types + guest stdio SDK (serde only)
        ↑                        ↑
crates/flux-plugin 0.29.0    plugins/host-kit 1.0.x
   (host half)                   ↑
                             plugins/*  →  flux-plugin-protocol = "1", host-kit = "1"
```

**Order matters.** C-141 (relocate `FlowEffect`) lands first and alone is worth shipping: it
deletes the 75-crate subtree from every plugin build without any version-line change, so the win
is bankable even if the rest slips. C-142 then moves `protocol.rs` wholesale; only after the crate
exists and its dependency graph is clean does C-143 split the version lines.

**What sits on the protocol line.** The wire vocabulary reaches into serde-only leaf crates:
`flux-spec` (`Effect`, `Idempotency`, `Risk`, `StagingDisposition`), `flux-evidence`
(`SignalMatch`, `ToolGroup`, `KIND_TURN_INTENT`), `flux-datasource` (`Declaration`), `flux-secret`.
These come off `version.workspace = true` and join the `1.x` line, so the protocol crate's graph
contains nothing that a flux cut moves. This is a deliberate, documented exception to the
one-version-for-every-crate rule in AGENTS.md — the rule exists to stop version drift going
unnoticed, and C-146's changed-crate assertion replaces that guarantee mechanically.

**Decoupling is only safe if drift is caught**, so the guards land with the split, not after:
the host validates the `PROTOCOL` marker and fails with an actionable message (C-144); golden
JSON fixtures pin the *wire* rather than the Rust signatures (C-144); a snapshot guard in the
style of `shipped_flux_corpus_agreement` forces a deliberate version bump when the surface changes
(C-144); and CI runs the previous pack's **released binary** against the current host (C-145) —
the test that actually proves a plugin built against `1.0` still works against a much later flux.

**Publishing follows for free.** `scripts/publish-crates-io.sh` already treats "already published"
as success, so once the protocol line stops moving, those crates stop being republished. C-146
adds a crates.io pre-check so an unchanged crate also skips *packaging*, asserts the inverse
(content changed ⇒ version changed), and moves `host-kit` out of the flux closure into the pack
release.

**Cut-script ergonomics (C-147)** cleans up what hurt while cutting 0.28.0: the script mutates
changelogs and manifests *before* running the gate, so a gate failure leaves both changelogs
rolled and a re-run mints a phantom version section (the documented 0.14.3 gap) — this happened
again on 0.28.0 and had to be finished by hand. Docs restamping (`docs/roadmap.md`'s "Status as
of" line, the board's Status block) is hand-done and drifts.

## Decisions

- **A new crate, not just independent versions on the existing ones.** Splitting
  `flux-plugin`'s host and guest halves by *feature* while giving the whole crate one version
  would leave "`flux-plugin` 1.4 inside flux 0.29" — a crate whose version means two different
  things depending on which half you use. A separate crate makes the contract nameable.
- **`FlowEffect` moves down and is re-exported, not duplicated.** A parallel enum in the protocol
  crate would drift silently; a re-export from `flux_lang::ast` keeps every existing path working
  and keeps one definition.
- **`PROTOCOL` stays a string, not a number.** It is already on the wire as `flux.plugin.v1` in
  shipped binaries; changing its shape would itself be a breaking wire change.
- **Out of scope:** the pack index gaining compatibility metadata (a natural follow-on once the
  protocol has a version worth recording), and any change to the plugin capability model.

## As built

Where each guarantee physically lives, once the epic landed:

| Guarantee | Artifact |
| --- | --- |
| A guest compiles the wire, not the host | `crates/flux-plugin-protocol` (serde-only; `flux-plugin` re-exports it, so no host call site moved) |
| The wire cannot change unnoticed | `crates/flux-plugin-protocol/tests/wire_contract.rs` + `tests/golden/{frame,manifest}.json`; the maximal instances use exhaustive struct literals, so a *new field* fails to compile there |
| An incompatible plugin says so | `check_protocol` at the load seam (`crates/flux-plugin/src/host/loading.rs`), proven by the `future_protocol_plugin` fixture in `crates/flux-plugin/tests/host.rs` |
| Yesterday's binary still works | `scripts/check-plugin-compat.sh`, run TWICE by CI job `plugin-compat` (C-166) as two separate, never-conflated assertions: (1) **the guarantee** — pinned via `COMPAT_BASELINE_TAG=plugins-v0.1.1`, a pack whose target commit predates `crates/flux-plugin-protocol` existing as its own crate (`git merge-base --is-ancestor d19212f <commit>` fails for it); (2) the newest pack, which is still worth knowing works but proves a weaker claim ("today's pack works with today's host", not "an old one still does"). Each run installs the pack's binaries into a throwaway `FLUX_HOME` and round-trips one read-shaped op against a `flux` built from this tree, logging the pack's release age and the protocol marker its binaries were built against (read from the release's `plugins-index.json`) so a PASS never reads the same for both runs. Exit 2 (release genuinely absent) is a logged skip; an incompatibility exits 1 and fails |
| A changed crate changed its version | `scripts/check-crate-versions.sh` (CI job `crate-versions`), scoped to crates that set their own version — workspace-inherited ones are swept by the cut. `--self-test` is the failing-first proof that a stale version is detectable |
| Published host-kit never lags the live protocol version (C-167) | `scripts/check-host-kit-protocol-drift.sh` (CI job `host-kit-protocol-drift`) — compares the live crates.io `codewandler-flux-plugin-protocol` version against the protocol dependency requirement the currently-published `codewandler-flux-host-kit` records, and fails naming the pack version to cut via `release-plugins.yml … publish: true` if it's behind. Runs on every push to main (the flux release path itself), not only inside the pack workflow that may never be dispatched. `--self-test` is the offline failing-first proof (a stale requirement, and an absent dependency — the exact v0.29.0 incident shape — both fail; a current one passes) |
| A flux cut leaves `plugins/` alone | `scripts/cut-release.sh` — no `plugins/` sed, no nested `cargo update`, no `plugins/` path in the commit; the plugins-workspace `cargo fmt --check` stays in the gate |
| A failed cut is safe to re-run | `scripts/cut-release.sh` snapshots every file it touches and an EXIT trap restores them on any non-zero exit before the commit |

**The closure now has two publishers**, because it spans two version lines:
`scripts/publish-crates-io.sh` ships the root workspace on the flux line, and
`.github/workflows/release-plugins.yml` ships `codewandler-flux-host-kit` with the pack. Ordering
is a real constraint — host-kit's dependencies are published by the flux closure — so the pack
workflow pre-checks that its `codewandler-flux-plugin-protocol` version is already live and fails
with that instruction rather than an opaque resolution error.
`flux_codegate::tests::publish_script_covers_a_registry_resolvable_closure` checks both halves, so
a crate cannot fall between them.

## Related

- **C-39** — repair the live smoke gate: steps 7/8 report "no claude/codex credential" while the
  script's own pre-flight lists both as configured, so the subscription legs never run.
- **C-47** — release-publication reliability: cause identified and fixed in `a707a35`
  (`Cleanup`'s `rm -f artifacts/*-dist-manifest.json` also ate the plan job's
  `plan-dist-manifest.json` in the candidate-promotion path). v0.27.0 still needs its asset
  backfilled before the story leaves Blocked.
- **C-69** — partition plugin guest dependencies: this epic finishes what it started.
- **C-122** — plugin hosts should follow a worktree transition (unrelated coupling, same seam).
