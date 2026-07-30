---
id: C-269
title: "A `System` trait — give guarded IO a seam a non-native backend can implement"
pillar: Core
status: ready
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
      as an implementor; call sites move to the trait.
- [ ] The safety invariant is preserved **and still mechanically enforced**: one guarded path starts
      every OS process, argv-only, workspace-pinned cwd, env cleared to a minimal allow-list, output
      byte-capped. Introducing a trait must not create a second `Command::new` path —
      `scripts/check-no-direct-io.sh` and `cargo test -p flux-codegate` still pass, and if the trait
      widens what the lint can see past, say so.
- [ ] A failing-first test proves the seam is real: a second, non-native implementation (a test
      double is enough) is accepted by a consumer that previously required the concrete type.
- [ ] `flux-plugin`'s `SystemSource` is reconciled with the new trait rather than left as a parallel
      abstraction over the concrete type.
- [ ] Full gate green in both workspaces.

## Progress

- (not started)

## Notes

- `crates/flux-system/src/lib.rs:1077` — `pub struct System`, with the guarded operations as inherent
  methods (`read_file_scoped`, `path_identity`, `host_path_identity`, `run`, `write_file_atomic`, …).
  The method set is the trait's shape; this is a wide-but-shallow change.
- Precedent for the idiom is already in `flux-runtime`: `LoopHost`, `Spawner`, `DispatchLedger`,
  `SkillLoader`, `SurfaceSink`.
- ⚠ Do not let this become "add a trait and a `dyn` everywhere". The point is a seam at the guarded
  boundary, not indirection for its own sake — if a consumer only needs two methods, it should depend
  on the narrow thing.
