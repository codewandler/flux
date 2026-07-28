---
id: A-95
title: Freeze the advertised tool set within a turn — stop cold-writing the prefix mid-loop
pillar: Agent
status: ready
priority: 6
epic: llm-cache-review
design: docs/designs/llm-cache-review.md
note: "`req.tools` is rebuilt every explore round from selected_specs_for_state (staged.rs:1098) and capability_signal expands it mid-loop; tools render BEFORE system, so one expansion invalidates every system breakpoint too and the next round pays a full cold write"
---

# Freeze the advertised tool set within a turn — stop cold-writing the prefix mid-loop

## Goal
Make the advertised tool set byte-stable across the rounds of one turn in the common case, so the
cached tools+system prefix survives the whole turn instead of being re-written whenever the model
signals a new capability family. Serves Agent: the adaptive loop's own behavior is what evicts its
cache.

## Acceptance
- [ ] Failing-first test in `crates/flux-flow` proving the current regression: a scripted turn where
      round 2 fires `capability_signal` produces a round-3 `tools` array that differs from round 1's.
      After the fix, the recorded requests for the rounds of one turn have byte-identical `tools`
      in the common case.
- [ ] The chosen mechanism is stated in Progress and justified. Two candidates from the design:
      (a) resolve the capability ceiling once at the turn boundary, or (b) admit an expansion once
      and keep it monotonically for the remainder of the engine session — the pattern turn-intent
      plugin surfacing already uses (`docs/designs/turn-intent-plugin-surfacing.md:40` explicitly
      cites "preserving monotonic catalog growth and prompt caching").
- [ ] No authority regression. `selected_specs_for_state` re-expands from the **live** registry on
      every call specifically so wiring/policy/tool-subset changes stay fail-closed
      (`staged.rs:1805-1822`), and `live_visible_specs` is the hard ceiling. Freezing the *advertised
      list* must not freeze the *authorization check*: a family that becomes unavailable mid-turn
      must still fail closed. Test covers this explicitly — it is the one way this story could do
      real harm.
- [ ] The `stale_capability_state_error` path still fires when the live registry no longer contains a
      previously selected family.
- [ ] Live-validated with the C-133 harness against `claude/*`: on a turn that fires at least one
      capability signal, the post-signal round reads the prefix from cache where the baseline writes
      it. Before/after in the design doc.
- [ ] Standard gate green (build, test, clippy `-D warnings`, fmt, `flux-codegate`).

## Progress
- (not started)

## Notes
- Ordering is already deterministic — `selected_specs` builds a `BTreeMap` (`staged.rs:1769`) and
  `ambient_specs` sorts by name (`staged.rs:1501`). The cost is membership change, not ordering, so
  this story is about *when* the set may change, not how it is serialized.
- The control tools (`finalize_tool`, `decision_tool`, `capability_signal_tool`) are already
  loop-invariant — `capability_signal_tool` takes `families`, which derives only from the immutable
  `ctx` (`staged.rs:1069-1073`). No work needed there.
- Interaction with C-134: because `tools` renders before `system`, tool churn invalidates the system
  breakpoints *and* any tail breakpoint. This story therefore multiplies C-134's value on
  signal-heavy turns rather than being independent of it.
- If (a) turns out to hurt capability discovery on genuinely ambiguous turns, (b) is the safer
  fallback — it keeps the model's ability to widen the ceiling and only removes the *shrink*.
