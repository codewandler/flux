---
id: C-467
title: "`GuardedWorkspaceFiles` implementors are invisible to the guarded-IO backend guard"
pillar: Core
status: done
priority: 2
areas: [flux-codegate, flux-system]
note: "the C-269 guard enumerates three of the four port traits; a new workspace-file backend claiming workspace confinement lands with no gate remarking on it. Surfaced while reviewing C-399, but true on main independently of it"
---

# The fourth port trait nobody enumerated

## Goal

Bring `GuardedWorkspaceFiles` inside the C-269 guard, so every implementor of a `flux_system::port`
trait is as enumerable as the guard's own doc comment claims.

## The finding

`crates/flux-codegate/src/lib.rs:1062`:

```rust
const GUARDED_PORT_TRAITS: &[&str] = &["GuardedProcess", "GuardedHostFiles", "GuardedEnv"];
```

Its own doc comment two lines above states the criterion:

> The `flux_system::port` traits whose implementations *are* a guarded IO backend. Implementing one is
> a claim to enforce the process/filesystem guarantees `System` enforces, so the set of implementors
> has to stay as enumerable as the set of raw `Command` constructions.

`GuardedWorkspaceFiles` (`crates/flux-system/src/port.rs:242`) meets that criterion exactly — it is a
`port` trait, and implementing it is a claim to enforce workspace confinement, which is a filesystem
guarantee `System` enforces. It is not in the list. So the guard covers **three of four** port traits
while documenting itself as covering the set.

Consequence: a new type can implement `GuardedWorkspaceFiles`, declare itself a workspace-file backend,
and land with **no gate remarking on it** — while an equivalent `GuardedHostFiles` backend would be
stopped until explicitly allowed. Confinement is the one guarantee where an unreviewed backend is worst:
a wrong `GuardedProcess` runs a command visibly, a wrong `GuardedWorkspaceFiles` silently reads or
writes outside the workspace root.

Current implementors on `main` (`crates/flux-system/src/port.rs`): `System` at `:438` and the test
`Memory` at `:742`. Neither is a problem — the problem is that nothing counts them.

⚠ **This is not caused by any one branch.** It was surfaced while reviewing C-399 (which adds a further
implementor in `crates/flux-system/src/remote.rs`), and C-399's implementor reported it against itself.
But the gap predates that branch and stands whether or not C-399 merges. Do not close this as part of
C-399.

## Acceptance

- [x] A failing-first test: a fixture implementing `GuardedWorkspaceFiles` for an unallowed type is
      **rejected** by the guard. It must pass (i.e. fail to be rejected) at the merge base.
- [x] `GuardedWorkspaceFiles` is in `GUARDED_PORT_TRAITS`, and every existing production implementor is
      allowed by name — added deliberately, one at a time, not by widening the allowance shape.
- [x] The alias-resistance the guard already has for the other three (a renamed import cannot mint a
      fresh unreviewed identity — see `spelled_as` at `lib.rs:1073`) covers this trait too, verified by
      a test that renames it on import.
- [x] ⚠ Check whether a **fifth** port trait exists that is also outside the list. This story is worth
      doing once; enumerate `crates/flux-system/src/port.rs` rather than fixing only the one that was
      reported.
- [x] `cargo test -p flux-codegate` green, and the workspace gate green — adding a trait to this list
      can legitimately flag pre-existing implementors, and each one needs a decision, not a blanket
      allowance.

## Notes

- The guard's own doc comment is the specification here, and it is already correct. This is the
  repo's recurring defect class in its mildest form: a guard whose comment describes a completeness it
  does not have. Prefer deriving the list from `port.rs` over hand-maintaining it, if that is tractable
  — a hand-maintained list of "every port trait" will drift again the next time a port trait is added.
- ⚠ If deriving proves impractical, then a test that fails when `port.rs` gains a `Guarded*` trait not
  present in `GUARDED_PORT_TRAITS` is the next best thing, and is the part that keeps this from
  recurring.
- Filed 2026-08-02 during the C-399 review.

## Outcome

The scanner now recognizes all four public `Guarded*` traits declared in `port.rs`, including renamed
imports. `System` and C-399's `RemoteSystem` each pay a distinct, single-use
`GuardedWorkspaceFiles` allowance. A census test parses `port.rs` and compares its public guarded
traits with `GUARDED_PORT_TRAITS`, so adding a fifth trait without extending the guard fails the gate.

Failing-first proof: before `GuardedWorkspaceFiles` entered the list,
`port_impl_scanner_rejects_an_unreviewed_workspace_backend` saw zero hits for its `Rogue` fixture;
after the fix it sees the backend and its canonical trait name.
