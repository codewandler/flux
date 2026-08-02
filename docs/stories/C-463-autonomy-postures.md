---
id: C-463
title: "Name the autonomy postures — `auto_approve: bool` is doing the work of a first-class choice"
pillar: Core
status: ready
priority: 3
design: docs/designs/remote-agents.md
epic: remote-agents
areas: [flux-cli, flux-runtime, flux-sdk, docs]
note: "⚠ owner-directed: no per-effect approval is a VALID model, not a degraded one — research, security hardening and long exploration are cases where interrupting the agent per effect is actively the wrong design. flux already ships that posture (C-410: unattended = fail-closed sandbox + auto-approve) and never names it, so it reads as safety switched off"
---

# Autonomy is a posture, not an absence of safety

## Goal

Make the autonomy posture an explicit, named choice — so running an agent without per-effect approval
reads as *"constrain by policy and isolation instead of by prompts"* rather than as *"safety off"*.

## Why this is a correction, not a feature request

Owner-directed, 2026-08-02, correcting my own framing. I had written that Anthropic's Managed Agents
lacking per-effect approval was *"coherent for their product and the opposite of flux's thesis."* That
is too narrow:

> *"it can be a valid model though — depends on the use-case — if you want for example high amount
> autonomy/freedom/exploration (research, security hardening, etc) this is totally fine"*

Correct, and it matters because **flux already ships that posture and does not name it**. C-410 raised
unattended CLI surfaces to a fail-closed `require` sandbox **with** auto-approve — *constrain harder,
prompt never*. That is a deliberate, safe configuration. But the only vocabulary for it is
`auto_approve: bool` and a `--yes` flag, which read as *turning something off*.

## ⚠ The framing that keeps "autonomy" from meaning "unsafe"

The envelope is **authorization → approval → guarded IO**. Of those three, **approval is the only stage
with a human in it.**

- Varying that stage is **choosing a posture**.
- Removing either of the other two is a **bug**.

An autonomous run is not an unguarded run: authorization still decides, guarded IO still executes,
evidence is still recorded. What changes is that the constraint budget moves from *human latency* to
*policy, sandboxing, budgets and destination scope* — all of which flux already has and which get
*more* important, not less, as the prompt goes away.

## The postures, as a starting set

| posture | approval | what constrains it | fits |
|---|---|---|---|
| **supervised** | per effect | a human at a terminal | daily driver, unfamiliar repo |
| **bounded autonomy** | none | policy + fail-closed sandbox + budgets | unattended CLI today (C-410) |
| **exploratory** | none, and interruption is the *harm* | hard isolation + wide-but-bounded grants + full evidence | research, security hardening, long exploration |
| **refusing** | denies everything | — | a served agent with nothing configured |

⚠ Today only the first, the second and an accidental fourth exist, and none is named.

## Acceptance

- [ ] **Failing-first**: a test asserting a named posture selects its approver, sandbox posture and
      budget together as one coherent choice — failing at the merge base, where they are set
      independently.
- [ ] The postures are **named and selectable**, not assembled from three unrelated flags. ⚠ The bug
      this prevents is the one C-444 describes from the SDK side: `auto_approve(true)` not implying
      confinement. A posture that sets approval without setting isolation is the same mistake with a
      nicer name.
- [ ] ⚠ **Nothing in the docs or the CLI presents an autonomous posture as degraded.** No "unsafe
      mode", no warning styling on a legitimate choice. State what each posture relies on instead.
- [ ] ⚠ **Each posture states what it does NOT protect against**, because that is the honest version of
      the above. Exploratory autonomy on a valuable repository is a real risk and the docs should say
      which one — not by discouraging it, but by naming the constraint the operator is now leaning on.
- [ ] Authorization, guarded IO and evidence are **invariant across every posture**, asserted by a test.
      That assertion is what makes the whole idea safe to ship.
- [ ] Existing `--yes` / `auto_approve` keep working and map onto a named posture. ⚠ No flag day.
- [ ] Full gate green.

## Notes

- ⚠ Interacts directly with [C-453](C-453-a-remote-approval-channel.md), in flight: a remote approver is
  the *supervised* posture made reachable over a network, not a new default that other postures deviate
  from. C-453 has been told this.
- Interacts with [C-444](C-444-sdk-secure-defaults.md): the SDK is where posture-as-three-independent-
  flags does the most damage, because an embedder can set one and miss the others.
- ⚠ Do not let this become a permission-preset generator. Four named postures whose contents are
  argued is worth more than an extensible scheme nobody configures correctly.
- The exploratory posture is also the argument for [C-397](C-397-container-process-backend.md) and
  [C-399](C-399-remote-guarded-io-backend.md): if the prompt is gone, isolation is what is left, and
  "run it somewhere disposable" stops being a nicety.

## Progress

- Filed 2026-08-02, owner-directed, correcting the framing in C-453's dispatch.
