---
id: A-120
title: The flux-fleet crate and AgentAddress — one URI whose scheme picks the runtime
pillar: Agent
status: backlog
epic: agent-fleet-runtime
design: docs/designs/agent-fleet-runtime.md
areas: [flux-fleet]
note: "new crate ⇒ must be classified in flux-codegate's layer() map at L5 or the architecture lint fails"
---

# The flux-fleet crate and AgentAddress — one URI whose scheme picks the runtime

## Goal
Create `flux-fleet` (L5) and give the fleet one addressing vocabulary. An `AgentAddress` names both
where an agent is and who owns its lifecycle, and it is the coordinator's primary key for an agent —
the string written onto a board item's `runner` field.

## Acceptance
- [ ] `crates/flux-fleet` exists at **L5**, classified in `crates/flux-codegate/src/lib.rs`'s
      `layer()` map. `cargo test -p flux-codegate` is green — a new crate that is *not* classified
      fails that lint, so this is the story's structural proof.
- [ ] `AgentAddress::parse` accepts, and round-trips through `Display`:
      `a2a://host:port/id`, `https://host/path`, `proc://flux?program=w.flux`,
      `proc://claude?proto=ndjson`, `docker://image:tag`, `k8s://ns/kind/name`.
- [ ] Failing-first test: the scheme resolves to a `Runtime` (`external` for both `a2a` and
      `https`/`http`; `proc`, `docker`, `k8s`), and the transport **defaults per target** —
      `a2a` everywhere except `proc://claude` / `proc://codex`, which default to `ndjson` — with
      `?proto=` overriding.
- [ ] Failing-first test: an **unknown query key is rejected**, not ignored. A typo'd `?porgram=`
      must be an error, because a silently-ignored runtime param starts the wrong agent.
- [ ] Failing-first test: `proc://` targets are restricted to an allowlist of agent kinds
      (`flux`, `claude`, `codex`); `proc:///bin/sh` and `proc://../../evil` are rejected. A model
      can supply an address, so this is an RCE boundary, not a validation nicety.
- [ ] Addresses with a network authority expose the origin needed for `guard_url_scoped`, so the
      caller can guard egress the same way every other network op does.

## Progress
- (not started)

## Notes
- Design: [agent-fleet-runtime.md](../designs/agent-fleet-runtime.md) — "Two axes" and "The address".
- The crate is new, so it needs its own version and a `publish` decision consistent with the other
  `codewandler-flux-*` crates. Flag it; do not decide the release mechanics alone.
