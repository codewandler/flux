---
id: C-282
title: "Two hand-rolled sandbox-env forwarders sit downstream of the posture floor and can push `off`"
pillar: Core
status: in-progress
priority: 4
epic: security-assurance
design: docs/designs/security-assurance.md
note: "found by C-276's review — the floor guarantee is a property of `posture_env`, not of the whole spawn path; runner.rs and worker.rs read ambient env and push FLUX_SANDBOX into the caller-override slot, which lands AFTER it. Pre-existing on main, behaviour-identical before and after C-276"
---

# Two hand-rolled sandbox-env forwarders sit downstream of the posture floor and can push `off`

## Goal

C-276 made a spawn forward its resolved sandbox posture to its children, and made that forwarding a
**floor**: `posture_env` renders every value from the resolved `Sandbox` alone, an `Off` posture
forwards nothing, and an open network forwards nothing — so it can only ever tighten a child.

That guarantee is real, and it is a property of **`posture_env`**, not of the whole spawn path. Two
call sites hand-roll their own sandbox-env forwarding and push it into the *caller-override* slot,
which `System::build_command` applies **after** the allow-list and therefore after `posture_env`:

- `crates/flux-eval/src/runner.rs:244-246,366`
- `crates/flux-orchestrate/src/worker.rs:309-315`

Both read the ambient environment and can push `FLUX_SANDBOX`, including `off`. So the value a child
finally sees is not necessarily the floor the parent resolved — the last writer wins, and the last
writer is a hand-rolled copy of a decision that now has one correct implementation.

**This is pre-existing on `main` and behaviour-identical before and after C-276.** It is not a
regression, and C-276 was right not to widen its scope into it. It is filed because C-276's review
found that the two mechanisms now overlap, and an overlapping hand-rolled copy of a safety decision
is exactly the drift shape C-249 was filed for.

## Acceptance

- [x] A failing-first test demonstrates the gap concretely: a process with a resolved non-`Off`
      posture spawns through one of these two paths under an ambient `FLUX_SANDBOX=off`, and the
      child receives `off` rather than the parent's floor. Assert the **whole** forwarded posture,
      not the one key you expect to move — C-276's tests use a `forwarded_posture` helper for exactly
      this reason and it is the shape to copy.
- [x] Both call sites stop hand-rolling the decision. The values must come from the same source
      `posture_env` reads (the resolved `Sandbox`), not from `std::env` — **this is the identical
      defect C-276's round 1 shipped and was reworked for**: gating on the resolved posture while
      taking the value from the ambient environment. Read that story's Progress before choosing a
      shape; the trap is documented there in full.
- [x] If either call site has a legitimate reason to override the posture *downward*, it is stated
      and enforced rather than left implicit. "It reads ambient env because it always has" is not a
      reason. If there is no such reason, the override should not be reachable at all.
- [x] The ordering asymmetry is documented where it will be hit: `posture_env` is applied *before*
      caller overrides (a posture is an inherited default a trusted call site may override) while
      the `FLUX_SANDBOXED` marker is applied *after* them (so no call site can forge or clear it).
      That difference is deliberate and is what makes this story possible; say so at the override
      slot, not only at `posture_env`.
- [x] Full gate green, including `FLUX_BWRAP_BIN=/nonexistent/bwrap`.

## Progress

- **Both hand-forwarders are deleted, not rewritten.** `flux-eval`'s `SANDBOX_CHILD_ENV_KEYS` /
  `sandbox_child_env` / `sandbox_child_env_from` and `flux-orchestrate`'s `SANDBOX_POSTURE_ENV` are
  gone. Neither call site needed a corrected copy of the decision, because there is nothing left for
  a copy to do: both spawn through a `System` that already carries the resolved `Sandbox`, and
  `apply_safe_env` renders the posture from it (`sandbox::posture_env`, C-276) on *every* spawn path
  regardless of `Confinement`. The hand-forwarders were pure redundancy — right up until the resolved
  posture and the ambient env disagreed, which is exactly what `System::with_sandbox` exists to
  create. `worker_env` no longer takes a `&System` at all, since it has nothing left to read from it.
- **The answer to "is there a legitimate downward override?" is no, at both sites, and the slot is
  closed rather than documented.** Deleting the forwarders alone would have been a *regression*: both
  sites relied on landing last to defend against an untrusted env source that lands in the same
  caller-override slot. `runner.rs`'s own comment said so ("benchmark-controlled `spec.env` must not
  be able to downgrade the parent CLI's resolved sandbox mode"). So the defence moved to where it
  belongs — a refusal at the source:
  - `extend_task_env` now drops the posture keys from a task fixture's `env`, alongside the provider
    credentials it already dropped, and for the same reason (both land in a slot applied last).
  - `worker_env` now drops them from `with_startup`'s `env` — the same treatment `DEPTH_ENV` already
    gets, and stated in the same terms: a worker is a full `flux` that must confine its descendants
    exactly as its parent would, so no call site gets to move that downward.
- **Two lists, filtered against, never copied.** `flux_system::sandbox` now publishes
  `POSTURE_ENV_KEYS` / `is_posture_env_key` (exactly what `posture_env` renders) and
  `SANDBOX_ENV_KEYS` / `is_sandbox_env_key` (that list **plus `FLUX_SANDBOXED`** — what a call site
  must refuse). Both filters read the second. Two unit tests hold them: one drives `posture_env` over
  every backend × mode × network × writable shape and asserts the emitted key set equals
  `POSTURE_ENV_KEYS` **exactly** — neither narrower (a hole that looks closed) nor wider (a filter
  dropping a variable for no reason); the other asserts `SANDBOX_ENV_KEYS` is that list plus the
  marker and nothing else, so forgetting to widen it alongside a new posture key cannot pass.
- **Round 2, correction 1 — the diff asserted something false about the marker, and now does not.**
  Three comments said no call site can forge or clear `FLUX_SANDBOXED`. Review disproved it against an
  **inactive** sandbox, which is the default posture: `sandbox_marker` returns `Some` only for a
  `Sandboxed` spawn over an *active* sandbox (`sandbox.rs:904-910`) and `build_command` writes the key
  only on `Some` (`lib.rs:2209-2211`), so when nothing is wrapped nothing is written after the
  caller's env and a supplied `FLUX_SANDBOXED=1` goes through verbatim. Verified in-tree before
  rewriting. All three now state the real scope — unforgeable *when the spawn is genuinely wrapped*,
  undefended when it is not — and name the behaviour gap as pre-existing and separately filed. This
  mattered because the diff had made the false claim newly load-bearing: it was the stated reason for
  omitting the marker from the published key list.
- **Round 2, correction 2 — the two refusal tests no longer assert more than the filters guarantee.**
  Both asserted `!key.starts_with("FLUX_SANDBOX")` (which covers `FLUX_SANDBOXED`) while the filters
  called `is_posture_env_key` (which does not); they passed only because neither fixture input named
  the key. **Chose to widen the filter, not narrow the assertion** — given correction 1, forging the
  marker is *worse* than pushing `FLUX_SANDBOX=off` (it suppresses the child's own wrapping rather
  than declining to demand it), these two call sites are exactly the ones assembling a child env from
  untrusted material, and the blast radius is nil (no production `with_startup` caller, no in-tree
  fixture naming it). `POSTURE_ENV_KEYS` was left alone rather than widened, because its completeness
  test defines it as precisely what `posture_env` emits — hence the second list. Both fixture inputs
  now name `FLUX_SANDBOXED`, and the assertions were confirmed to **fail** with the filter reverted to
  `is_posture_env_key` (`[("FLUX_SANDBOXED", "1"), ("TASK_FIXTURE", "kept")]` /
  `[("FLUX_SANDBOXED", "1"), ("WORKER_LABEL", "kept"), ("FLUX_FLEET_DEPTH", "1")]`), so neither is
  vacuous.
- **Round 2, correction 3 —** `worker.rs`'s `env` field doc no longer links the deleted
  `SANDBOX_POSTURE_ENV` or describes the inverted behaviour; it now states both refusals a
  `with_startup` caller is subject to.
- **The ordering asymmetry is now stated at the override slot**, in `apply_safe_env` where the
  caller's `env` is applied: posture *before* (an inherited default a trusted call site may
  override), `FLUX_SANDBOXED` *after* (so no call site can forge or clear it), and therefore what a
  call site must do about it. It was previously documented only on `posture_env` and `build_command`
  — neither of which a call site is reading when it decides what to put in `env`.
- Proof (`crates/flux-orchestrate/src/worker.rs`):
  `a_worker_receives_the_coordinators_resolved_posture_not_the_ambient_one`. At the merge base
  `389f1c95`, with the coordinator pinned `On` and the ambient env saying `off`, the child's own env
  dump read `FLUX_BWRAP_BIN=/usr/bin/bwrap  FLUX_SANDBOX=off  FLUX_SANDBOX_NET=0` against a floor of
  `FLUX_BWRAP_BIN=/usr/bin/bwrap  FLUX_SANDBOX=on` — the kill switch, delivered. Asserted as a
  **differential** against a spawn with no caller env at all, so the expectation is the real
  `posture_env` output on that host rather than a second copy of its rules; the whole forwarded
  posture is compared, per C-276's `forwarded_posture` shape (which the test re-states locally rather
  than reading `POSTURE_ENV_KEYS`, so it cannot agree with a wrong production list).
  Two hermetic companions: `a_startup_env_may_not_push_a_sandbox_posture_at_a_worker` and
  `flux-eval`'s `a_task_fixture_may_not_name_the_eval_childs_sandbox_posture`; both also failed at
  the base.
- **The note's ⚠ is answered: they are the same two sites, not a third and fourth.** C-276's
  `SANDBOX_POSTURE_ENV` / `sandbox_child_env` pair *is* `runner.rs:244-246,366` /
  `worker.rs:309-315`. A repo-wide grep for `FLUX_SANDBOX` outside `flux-system`/`flux-cli` finds no
  other forwarder. `flux-codegate`'s identically-named `SANDBOX_POSTURE_ENV` is unrelated — it is
  C-266's list of env keys whose *appearance in a test spawn's builder chain* counts as a posture
  declaration, a lint input, not a forwarder — and was deliberately left alone.
- The falsified doc C-276 flagged (`worker.rs:1579`, "None of `FLUX_SANDBOX*` is in
  `build_command`'s `SAFE_ENV`") is gone with the test it annotated; the replacement asserts the
  inverse property, that `worker_env` forwards *none* of the posture.

## Notes

- Provenance: the independent review of [C-276](C-276-sandbox-posture-does-not-reach-a-child-flux.md)'s
  rework, filed as a MINOR alongside a `PASS`. The reviewer was explicit that it is not blocking and
  not caused by that diff — quoted here so nobody re-derives the severity: *"Pre-existing on main and
  behaviour-identical before and after this diff … so not blocking, but the diff's floor guarantee is
  a property of `posture_env`, not of the whole spawn path."*
- ⚠ Check whether these two forwarders duplicate each other as well as `posture_env`. C-276's
  Progress section flagged two redundant hand-forwarders (`SANDBOX_POSTURE_ENV` /
  `sandbox_child_env`) and said to file them; the review confirmed no story covered them. If they are
  the same two sites, this story owns both. If they are a third and fourth, say so here — a
  half-converted set is worse than none, because it looks done.
- Related: [C-276](C-276-sandbox-posture-does-not-reach-a-child-flux.md) built the floor;
  [C-262](C-262-fail-closed-unattended-sandbox-profile.md) installed the fail-closed switch that an
  ambient `off` short-circuits; [C-249](C-249-git-family-clean-tree-policy-and-stash-wording.md) is
  the precedent for replacing hand-copied policy with one enforced implementation.
