---
id: C-269
title: "A `System` trait — give guarded IO a seam a non-native backend can implement"
pillar: Core
status: in-progress
priority: 4
epic: portable-wasm-runtime
design: docs/designs/portable-wasm-runtime.md
note: "the epic's main blocker — flux-system::System is a concrete struct (lib.rs:1077), so nothing can substitute a non-syscall backend; wide-but-shallow, the method set already dictates the trait"
---

# A `System` trait — give guarded IO a seam a non-native backend can implement

## Goal

`flux-system::System` is the one guarded path for all filesystem, process and network access — an
invariant CI enforces. It is also a concrete struct, so there is no way to substitute an
implementation that reaches host imports instead of syscalls. Introduce the trait seam, so guarded IO
becomes a port with the native syscall implementation as its first implementor.

## Acceptance

- [ ] A trait abstracts the guarded surface consumers actually use, with the existing native `System`
      as an implementor; call sites move to the trait. — **partially: the process / host-scoped-read /
      env families only.** `crates/flux-system/src/port.rs` states them as three narrow ports with
      `System` as the native implementor; `flux-plugin` (the whole capability bridge) and
      `flux-capabilities::StaticResolver` moved onto them. The workspace-confined *file* family
      (`read_file`/`write_file`/…) is **not** ported — see Progress.
- [x] The safety invariant is preserved **and still mechanically enforced**: one guarded path starts
      every OS process, argv-only, workspace-pinned cwd, env cleared to a minimal allow-list, output
      byte-capped. Introducing a trait must not create a second `Command::new` path —
      `scripts/check-no-direct-io.sh` and `cargo test -p flux-codegate` still pass, and if the trait
      widens what the lint can see past, say so. — the widening is real and is now closed by a new
      whole-tree gate, `no_unreviewed_guarded_port_backend_outside_system`; see Progress.
- [x] A failing-first test proves the seam is real: a second, non-native implementation (a test
      double is enough) is accepted by a consumer that previously required the concrete type. —
      `crates/flux-plugin/tests/non_native_system.rs`.
- [x] `flux-plugin`'s `SystemSource` is reconciled with the new trait rather than left as a parallel
      abstraction over the concrete type. — it now yields `Arc<dyn PluginSystem>`.
- [x] Full gate green in both workspaces.

## Progress

**Landed (C-269, `impl/C-269`).** The seam exists and one real consumer is on it.

- `crates/flux-system/src/port.rs` — three narrow ports, split by guarded resource, with **no god
  trait**: `GuardedProcess` (argv-only exec), `GuardedHostFiles` (scope-admitted host reads),
  `GuardedEnv` (env lookups). `System` is the native implementor; the impls are pure delegation, and
  because inherent methods win method resolution no existing `system.run(..)` call site changed
  behaviour. Dyn-compatible via a hand-rolled `Guarded<'a, T>` boxed future rather than `async-trait`,
  which keeps `flux-system`'s dependency set at `flux-core` + `tokio` + `url` (it has to compile for
  the portable target) — and adding a dependency was out of fence for this story anyway.
- `GuardedProcess` requires **one** primitive, `run_with_env`; `run`/`run_observed`/
  `run_with_env_observed` default-delegate to it exactly as `System`'s own conveniences do, and
  `run_with_stdin`/`spawn_background` **deny by default**. A substrate therefore implements one method
  and denies what it cannot serve, instead of fabricating it.
- `flux-plugin` — `SystemSource::system()` returns `Arc<dyn PluginSystem>` instead of
  `Arc<flux_system::System>`. `PluginSystem` is the *consumer's own bundle* of the three ports it uses,
  so the required surface is visible at the consumer. The two axes stay separate and both are needed:
  `SystemSource` answers *which* system is active (C-122's worktree-transition snapshot),
  `PluginSystem` answers *what a system is* — which had no answer before. Every existing
  `SystemHostCaps::new(Arc<System>)` call site is unchanged: `Arc<System>` coerces.
- `flux-capabilities::StaticResolver` now holds `Arc<dyn GuardedEnv>` — it only ever read an env var,
  so it depends on the narrowest port, which is the story's "narrow thing" rule applied literally.

**The lint blind spot, and what closed it.** The pre-existing gates bound *syscall construction*
(`no_raw_process_command_outside_system` over both workspaces, two allowances, both in flux-system) and
*direct-IO API calls in the eight model-facing packs*. Both are API-shape based, so the port creates no
gap in either — confirmed green, still exactly two command-construction allowances. But they never
bounded the *semantics* of a guard, and that is exactly what the port makes substitutable: a type can
now satisfy `GuardedProcess` while enforcing none of argv-only / pinned-cwd / cleared-env /
capped-output, and before C-269 the type system structurally forbade that at the `SystemSource` seam.
That is a genuine, narrow reduction in structural guarantee and it is inherent to the story — you
cannot have a substitutable backend *and* a type-enforced single backend. It is now mechanically
enumerated instead: `flux-codegate`'s `no_unreviewed_guarded_port_backend_outside_system` walks every
production source in both workspaces for `impl <port trait> for <T>` against a single-use allowance
list holding only flux-system's three native impls, and it runs from `scripts/check-no-direct-io.sh`.
Verified to bite (a rogue `impl GuardedProcess` in `flux-web/src` reds it), and blanket impls are
reported as `<generic>` rather than dropped.

**The gate resolves renamed trait imports** (review follow-up). Its first cut matched only the final
path segment's literal ident, so `use flux_system::port::GuardedProcess as Exec; impl Exec for Rogue {}`
walked through clean — while the incumbent sibling gate already defended the identical evasion
(`use std::process::Command as Exec` is caught, because that scanner carries `ProcessAliases`). The
newer, security-relevant gate being the weaker of the two was not defensible, so `PortAliases` now
mirrors that pattern: identity-seeded canonical names plus renamed imports, grouped renames, module
renames, and rename *chains* resolved to a fixed point (hence order-insensitive). Hits carry the
**canonical** trait name so an allowance cannot be dodged by renaming, with the written spelling
reported in the diagnostic (`GuardedProcess implemented for Rogue (written as `Exec`)`). Locked in by
`port_impl_scanner_resolves_renamed_trait_imports` and re-verified against the tree with the
reviewer's exact snippet.

Also locked in: `#[cfg(test)]` is the *only* configuration the gate excuses —
`#[cfg(feature = "wasm")]` and `#[cfg(all(unix, feature = "remote"))]` backends ship to users and are
reported (`port_impl_scanner_excuses_only_cfg_test_not_other_cfgs`).

**Known, pre-existing scanner limits.** Macro-expanded impls and sources pulled in from outside `src/`
via `#[path]` escape this gate — but they equally escape the incumbent `Command` gate, because both
share `workspace_source_files`. Tree-wide AST-scanner limitation, not specific to the port; noted here
so the next reader does not mistake it for a C-269 regression.

**The port traits are unsealed and the gate is in-repo only** — both deliberate, now disclosed in
`port.rs`'s module docs. Any downstream crate can implement the three traits; that is the epic's whole
point, and it is not an escalation, since such a crate could already call `Command::new` directly. The
traits are a *contract*, not a permission. The gate's reach ends at `crates/*/src` + `plugins/*/src`:
inside flux the one-guarded-path invariant is mechanically enforced, outside it a consumer takes
responsibility for the guarantees itself.

**Deliberately not done, and why.**

- The **workspace-confined file port** (`read_file`, `write_file`, `read_file_bytes`, `is_dir`,
  `walk_files`, `path_identity`, …). Every consumer that would move onto it lives in `flux-tools`,
  `flux-flow`, `flux-orchestrate` or `flux-eval`; defining the trait without them would land an
  abstraction with zero call sites, which is the "indirection for its own sake" this story forbids.
  This is the main follow-up and it should land with its consumers in one change.
  `flux_runtime::ToolContext::system` is the pivot: it is still `Option<Arc<System>>`, and retyping it
  is what unlocks `flux-tools`.
- **Native-only operations stay inherent on `System`**, documented in `port.rs`: `rerooted` (returns
  `Self`), `workspace()`/`sandbox()` (native path resolution + OS sandbox posture),
  `run_with_env_exempt` and `run_with_env_streamed*` (sandbox *exemption* is meaningless with no OS
  sandbox), `spawn_interactive`/`spawn_debug_pipe` (a tty and a POSIX fd pair). Putting these on the
  port would let an implementation claim a posture it cannot hold.
- `spawn_background` is the one port operation whose *result* is irreducibly native (`ManagedChild`
  owns a live `tokio::process::Child`), so it is on the port with a fail-closed default rather than
  left off it. A non-native substrate denies `process.spawn`, which is also the behaviour you want.

## Notes

- `crates/flux-system/src/lib.rs:1077` — `pub struct System`, with the guarded operations as inherent
  methods (`read_file_scoped`, `path_identity`, `host_path_identity`, `run`, `write_file_atomic`, …).
  The method set is the trait's shape; this is a wide-but-shallow change.
- Precedent for the idiom is already in `flux-runtime`: `LoopHost`, `Spawner`, `DispatchLedger`,
  `SkillLoader`, `SurfaceSink`.
- ⚠ Do not let this become "add a trait and a `dyn` everywhere". The point is a seam at the guarded
  boundary, not indirection for its own sake — if a consumer only needs two methods, it should depend
  on the narrow thing.
