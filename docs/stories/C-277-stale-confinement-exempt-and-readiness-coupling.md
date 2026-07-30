---
id: C-277
title: "Two unpinned couplings around child-process spawning: a stale `Confinement::Exempt` doc and a cross-crate readiness string"
pillar: Core
status: in-progress
priority: 7
epic: security-assurance
design: docs/designs/security-assurance.md
note: "both found during C-243's review round — a doc comment naming a call site that does not exist, and a worker-readiness marker that no test can pin from either side"
---

# Two unpinned couplings around child-process spawning

## Goal

Close two small, related defects found while reviewing C-243, both of the same shape: something states
a relationship that nothing verifies.

## Acceptance

**1. `Confinement::Exempt`'s doc names a call site that does not exist.**

- [x] The doc comment at `crates/flux-system/src/sandbox.rs:892-897` describes the local-eval child
      flux host as using `Confinement::Exempt` *"because it needs provider network access"*. Verified:
      the variant has **no call site** outside `sandbox.rs`, and
      `flux-eval`'s `model_reachable_eval_runner_has_no_sandbox_exemption` (`runner.rs:515`) asserts the
      opposite. Either the doc is corrected, or the exemption is genuinely wired and the test updated —
      decide which, and record why.
      → **The doc is corrected.** Re-verified against `2abd0a13` rather than the story text: the claim
      still stood after C-276/C-282/C-289. `flux-eval` spawns its child through the *sandboxed*
      `run_with_env_streamed`/`run_with_env` (`runner.rs:375,378`) and pins that itself, so wiring the
      exemption would regress a deliberate posture to satisfy a comment.
- [x] Whatever the outcome, a comment cannot be the only thing asserting a call site exists. If the
      variant is retained unused, say so in the doc; if it is removed, say what replaced it.
      → `the_exempt_doc_names_exactly_the_seams_that_exist` (`sandbox.rs`) resolves the functions that
      actually pass `Confinement::Exempt` from source and asserts the doc's bullet list matches them
      **in both directions**. `run_with_env_streamed_exempt` has no product caller and is now recorded
      as retained-unused rather than deleted (public API of a published crate).
- [x] ⚠ This one matters out of proportion to its size: the stale comment was cited as *precedent* in a
      rework instruction during this wave, and an implementor correctly refused to follow it. A doc that
      describes a design that was never built will be followed by someone eventually.
      → The exact defect class is now a test failure: inserting a fabricated `spawn_local_eval_host`
      bullet reports `documented but nonexistent: ["spawn_local_eval_host"]`.

**2. Worker readiness is an unpinnable cross-crate string.**

- [x] `crates/flux-orchestrate/src/worker.rs` decides a worker is live by matching
      `"listening on http://"` against the line printed by `flux_server` (`lib.rs:471`). The constant is
      private, and `flux-server` is L6 while the consumer is L3 — so **no test can pin the pair from
      either side**. Give it a pinnable form: a shared constant in a crate both can depend on, a real
      handshake (poll `/health` or the agent card), or a test that asserts the two strings agree.
      → **Shared constant**, in the new `flux_core::readiness` (L0 — already a dependency of both, so
      no manifest change and no layering-map change). The producer renders with
      `serving_announcement`, the consumer matches with `announces_serving`; both private literals are
      gone.
- [x] State the failure mode that motivates it: a rewording does not fail loudly — `fleet.start`
      degrades to a 60-second timeout and reports a worker that never announced itself, which reads as
      a slow or hung worker rather than a broken contract.
      → Stated in the `flux_core::readiness` module doc, and again at each of the three call sites.
- [x] Whichever fix is chosen, a rewording of the server's line must make a test fail.
      → Verified by mutation, not by assertion: changing `SERVING_MARKER` to `"now serving http://"`
      fails **10** tests in `flux-orchestrate`, led by
      `the_stand_in_worker_announces_exactly_what_the_real_server_announces`.

## Progress

- **Done** — landed on `impl/C-277` off `2abd0a13`.
- **Part 1.** Re-verified against the current tree before acting (C-276/C-282/C-289 had rewritten this
  file; the defect survived them). Corrected three stale docs — the `Confinement::Exempt` variant, and
  `run_with_env_exempt`/`run_with_env_streamed_exempt`, both of which also named local-eval — and made
  the inventory machine-checked. Real seams: `spawn_debug_pipe`, `run_guarded_probe`,
  `run_with_env_exempt` (one product caller, `flux-cli`'s plugin-pack source builder), and
  `run_with_env_streamed_exempt` (**none**, retained and documented as such).
- **Part 2.** Found a **fourth** copy of the readiness wording the story does not mention:
  `tests/fixtures/stand-in-worker.sh`, which every `ProcessRuntime` lifecycle test proves against. It
  agreed with the *consumer*, so a rewording of the server would have left the suite green while every
  real worker timed out. It is now pinned to the shared contract by test.
- **Not done deliberately:** no readiness *handshake*. `fleet.start` declares no network access at all,
  and the netns gap a handshake would close is already refused up front (`worker.rs`, C-243 finding B2).
  Recorded in the module doc.

## Notes

- Both found by the independent review of **C-243** (`impl/C-243`), which judged neither blocking on
  its own. The readiness coupling was explicitly deferred by the coordinator in favour of a story rather
  than a rework round, since it cannot be pinned from either side without a structural change.
- The two are filed together because they are the same failure class — an unverified claim about how
  child-process spawning works — and both live in the blast radius of the same code.
- A real handshake would also fix a second-order problem: the current marker proves the server *printed*
  a line, not that it is *accepting connections*, which C-243's netns finding showed are different
  things.
