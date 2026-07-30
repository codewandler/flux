---
id: C-225
title: config.get / config.set over an agent-writable allowlist, disjoint from PinnableKey by test
pillar: Core
status: ready
priority: 16
epic: agent-authored-surface
design: docs/designs/agent-authored-surface.md
areas: [flux-config, flux-tools]
note: "the first model-facing op that writes a file the safety envelope reads back — PinnableKey::ALL (config lib.rs:451-505) already enumerates every security-relevant key, so asserting the agent-writable set is DISJOINT from it makes [permissions]/[sandbox]/workspace.allow_all unrepresentable rather than denied"
---

# Agent-writable config keys

## Goal
Let the agent change how flux behaves for it — theme, effort, model, verbosity, `[consult]` — and
have the change apply to the session it is already in, persisted atomically. Bounded by an allowlist
that cannot drift into the safety envelope, because the disjointness is machine-checked.

## Acceptance
- [ ] `AGENT_WRITABLE_KEYS` lands in `flux-config` (L0) as the single source of truth: canonical
      dotted spellings, matching `PinnableKey`'s `ALL` convention (`crates/flux-config/src/lib.rs:481-489`).
- [ ] **Failing-first test — the load-bearing one:**
      `assert!(AGENT_WRITABLE_KEYS.iter().all(|k| PinnableKey::parse(k).is_none()))`. Adding a key to
      either list without checking the other fails the gate. `policy`, `sandbox.enabled`,
      `sandbox.require`, `sandbox.network`, `workspace.allow_all`, `private_net.web` and
      `tools.disable` are therefore **unknown keys** to `config.set`, not denied ones.
- [ ] `config.get` and `config.set` ops in `flux-tools`, surfaced on the same assembly-time basis as
      the pane ops (C-223). `config.set` on a non-allowlisted key returns an unknown-key error naming
      the writable set — it must not read as "you lack permission", which would invite retry.
- [ ] Persistence reuses the existing atomic, guarded path: `flux_config`'s pure round-tripping
      serializers (`lib.rs:1102-1140`) plus a `flux_runtime::metadata` persister in the family of
      `persist_user_theme` / `persist_allow_rules` (`metadata.rs:145-177`). No raw `std::fs`, and
      unrelated settings round-trip untouched — asserted by test over a config carrying keys the
      writer does not know about.
- [ ] **Scope is explicit and defaulted safely**: `user` (`~/.flux/config.toml`) vs `project`
      (`.flux/config.toml`). A project write is a change to a file the user's repo carries, so it is
      the non-default and is disclosed in the op result.
- [ ] Hot apply, in-session: theme, effort, model spec and verbosity take effect on the live
      `ChatState` / `FlowEngine` and the session re-projects through the existing `project_session` +
      `load_history` that `/resume` uses (`crates/flux-tui/src/lib.rs:3954-3975`). **No process
      re-exec** — a self-restart is a new turn-termination path, the class AGENTS.md flags as having
      recurred three times.
- [ ] An allowlisted key that cannot be hot-applied is persisted and **reported as "takes effect next
      session"**. Test pins the message: silently half-applying would leave the model believing a
      setting is live when it is not.
- [ ] Ops mirrored in `crates/flux-flow/docs/ops-reference.md` and `website_contract.rs`.

## Progress
- (not started — independent of C-221…C-224; can be worked in parallel)

## Notes
- **Why disjointness rather than a denial.** A denial is a decision made at call time by code that
  can be wrong, bypassed, or reordered. Disjointness is a property of two constant lists, checked at
  build time. The envelope's own posture — "there are no bypass paths" — argues for the version that
  cannot be argued with at runtime.
- `PinnableKey`'s doc comments are worth reading before choosing the allowlist: they explain *why*
  each key is security-relevant and how it merges (`tools.disable` union-merged, `private_net.web`
  genuinely widening, `sandbox.*` OR-merged / strictest-wins). None of that reasoning should have to
  be re-derived by whoever adds the next writable key.
- The model changing its **own model spec** is the odd one out — it is allowlisted because `/model`
  already exists as a user affordance and the TUI already mirrors it, but it is the key most worth a
  second look during review. It changes spend and capability, not just presentation.
- Consider whether `config.set` should surface the resulting diff in its result. The user is entitled
  to see what their agent changed about their tooling, and a diff is cheaper to read than a re-read
  of the file.
