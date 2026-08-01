---
id: C-395
title: "State the workspace-confined file surface as a port"
pillar: Core
status: done
design: docs/designs/execution-substrate.md
epic: execution-substrate
note: "C-269 deferred this on the stated grounds that its consumers all hold a concrete System, so a trait would be indirection without a seam — a second consumer is exactly the condition that expires that reasoning"
---

# State the workspace-confined file surface as a port

## Goal

`port.rs` states `GuardedEnv`, `GuardedProcess` and `GuardedHostFiles`, and deliberately omits the
workspace-confined file surface (`read_file`, `write_file`, …). Add it, so a consumer that holds only
the port gets the same confinement a consumer holding a concrete `System` gets.

## Acceptance

- [x] The workspace-confined file operations are reachable through a trait in `flux_system::port`,
      with the native `System` as the first implementor by pure delegation.
- [x] **Failing-first test** — a consumer holding only the trait is refused the same escapes a
      concrete-`System` consumer is refused: a lexical `..`, and a symlink that canonicalizes outside
      the root. The test must fail before the delegation is written, and it must exercise the
      *trait*, not the struct.
- [x] Read/write asymmetry survives the port: `read_roots` remain readable and not writable through
      the trait, exactly as through the struct.
- [x] Optional operations default to denial, matching the existing port's fail-closed posture.
- [x] `cargo test -p flux-codegate` stays green with no new backend allowance.

## Progress
- `port::GuardedWorkspaceFiles` — two required primitives, one per direction (`read_file_bytes`
  resolves on the read path, `write_file_bytes` on the write path); `read_file`/`write_file` reduce
  to them; `append_file`, `read_file_bytes_capped`, `file_size`, `path_exists`, `is_dir`,
  `file_mtime`, `list_dir` and `walk_files` deny by default.
- The native `System` delegates **every** operation, including the two the trait would default, so
  the port and the struct are one code path rather than two that agree.
- Four tests in `port.rs`: the escape refusals through trait *and* struct (checking the outside
  directory afterwards, since a refusal that still wrote is not a refusal), the read-root asymmetry,
  the native backend answering every optional operation, and a memory-backed substrate proving the
  reductions and the denials.
- Landed as two commits so the failing-first half is a commit, not a claim: at the first the tests
  fail with `E0277: System: GuardedWorkspaceFiles is not satisfied`.

## Open — for the reviewer
- `write_file_atomic` / `update_file_reserved` stayed inherent: their contract is over filesystem
  primitives (`O_EXCL`, same-directory `rename`), and the latter takes a caller closure, so it is not
  dyn-compatible at all. Recorded in the module docs beside the other native-only operations.
- **The new port is outside `flux-codegate`'s backend gate.** Acceptance forbade a new backend
  allowance, and registering `GuardedWorkspaceFiles` in `GUARDED_PORT_TRAITS` would have forced one
  for the native delegation. The gap is stated in `port.rs` rather than left implicit; closing it is
  two lines whenever the allowance is acceptable.

## Notes
- The deferral this closes is recorded in `crates/flux-system/src/port.rs` — *"a trait with no call
  sites would be indirection without a seam"*. That was correct; the seam now exists.
- Do **not** add a god trait. The port is split by guarded resource on purpose; a consumer names only
  what it uses.
