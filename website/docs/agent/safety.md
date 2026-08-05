---
title: Safety & approvals
description: "The safety chain for every operation, including authorization, permissions, approvals, and guarded IO."
---

# Safety & approvals

flux is built so every model-stage operation, built-in tool, sub-agent operation, app operation,
and plugin **host-capability request** traverses one mandatory chain before flux performs real IO.
This page explains that chain, what prompts, and how approval decisions interact with policy.

Plugin executables are trusted native dependencies, not OS-sandboxed by default. The chain governs
their projected operations and host callbacks; it cannot prevent a malicious binary from making a
direct syscall unless opt-in OS-level sandboxing (`[sandbox]`) is enabled underneath the envelope.
See [Plugin trust & signing](../security/plugin-trust.md) and
[OS process sandboxing](../security/os-sandbox.md).

## The one envelope

Every operation—from an exploration read, an approved action batch, an authored flow, a plugin op, or a sub-agent—
lowers onto the same chain before it touches the outside world:

```text
pre-tool hooks
    -> authorization policy (default-deny)
        -> permission rules
            -> approval gate
                -> guarded IO
```

1. **Pre-tool hooks** can observe, modify, or deny a call before anything below runs. A runaway
   hook is interrupted and fails closed.
2. **Authorization policy** is pure and **default-deny**: an operation is only permitted if a grant
   covers it. A deny stops the chain; a grant marked "requires approval" forces the gate below.
   Sensible local defaults keep you productive out of the box.
3. **Permission rules** are the ergonomic layer on top (`read`, `Bash(git:*)`, …): deny-first, then
   allow, otherwise prompt.
4. **Approval gate** is forced for destructive intents and unscoped writes — even under an otherwise
   permissive rule.
5. **Guarded IO** is the only production path used by flux operations and conforming plugin
   callbacks. It is workspace-confined, rejects symlink escapes, launches processes with an argv
   vector, caps output, and resolves every URL through an SSRF guard. The opt-in `bash` operation is
   the explicit exception to “no shell interpretation”: it launches `sh -c` through that same
   guarded process path and remains high-risk and approval-gated.

## Autonomy is a posture

Of the three stages above, **approval is the only one with a human in it.** Which posture that stage
runs under is a real choice with more than one right answer — and it is the only stage that varies.
Authorization still decides, guarded IO still executes, and the evidence trail is still recorded,
identically, under every posture below. Nothing on this page turns any of that off, because there is
no way to.

An agent running without a per-effect prompt is therefore not an unguarded agent. What changes is
where the constraint comes from: it moves off *human latency* and onto policy, isolation, budgets and
destination scope — all of which matter **more** once the prompt is gone, not less.

Name the posture with `--posture <name>` (`flux run`, `flux tui`, `flux fork`, `flux record`,
`flux app run`), or `ClientBuilder::posture(..)` / `FlowClientBuilder::posture(..)` in the SDK.

| posture | who answers an effect | it relies on | it does **not** protect against |
|---|---|---|---|
| **supervised** (default) | you, at the terminal, per effect | a human reading each prompt before it lands | approval fatigue — a prompt is a boundary only while it is being read, and a run that asks forty times gets forty reflexive answers. It also confines nothing *between* prompts: no OS sandbox is implied by your being present. |
| **bounded-autonomy** | nobody | authorization policy, a fail-closed OS sandbox with the network closed, and resource budgets | an authorised effect inside the workspace. Everything policy already grants happens without anyone seeing it first, so the working tree is the blast radius. Run it where losing the working tree is survivable — a branch, a worktree, a disposable checkout. |
| **exploratory** | nobody, and interruption is the harm | hard isolation of the host, deliberately wide but bounded grants including network egress, and the complete evidence trail | exfiltration. Egress is open on purpose, and an agent that can read the workspace and reach the internet can move one to the other. What is isolated here is the host, not the data — point it at a disposable checkout, not at a valuable repository holding live credentials. |
| **refusing** | nothing that reaches approval runs | nothing beyond what was already pre-authorised before the agent started | anything that never reaches the approval stage. Refusal sits at that stage only — pre-authorised operations resolve before it, and native processes a surface starts before any effect is requested (plugin binaries at startup) never consult it. Pair it with `[sandbox] require` when that gap matters. |

**`exploratory` is the one to reach for when stopping is the failure** — research, security
hardening, a long refactor you want to come back to finished. It is not `bounded-autonomy` with the
guard-rails loosened; it is the same fail-closed confinement with the network deliberately left open
and the evidence trail deliberately left uncapped, because those are the two things it leans on.

`--yes` is the older spelling of `--posture bounded-autonomy` and keeps working unchanged. Whichever
posture is in effect, an app-declared `permissions` ceiling remains absolute, and a posture never
widens a capability scope — it only decides who answers.

## What prompts, what doesn't

- **Reads** (`read`, `glob`, `grep`, `search`) are pre-allowed — they run without prompting.
- **Writes and commands** prompt for approval under the `supervised` posture, unless you have an
  allow-rule in your config.
- **Destructive operations** (`rm -rf`, `git push --force`, `mkfs`, …) force the approval gate. In an
  adaptive turn they are included in the aggregate action-batch risk before a one-shot receipt is
  issued; execution still rechecks authorization and the exact approved scope. An undisclosed or
  changed action has no matching receipt and cannot run. What the gate *does* is the posture's
  answer: `supervised` prompts you, a sub-agent's approver denies outright, and the autonomous
  postures approve. So an autonomous posture does **not** exempt destructive ops from the gate — it
  answers the gate for them too, along with everything else still admitted by the current capability
  scope.

## Approving a prompt

Under the `supervised` posture, when flux prompts you have three choices:

- **`y`** — approve this one operation.
- **`a`** — always approve operations like this; the choice is persisted to `.flux/config.toml`.
- **`N`** — deny (the default). The operation does not run.

If you want routine steps to flow but destructive ones to still stop and ask, stay `supervised` and
add allow-rules for the routine tools — the destructive gate re-fires past an allow-rule, so those
steps still prompt. That is a different thing from an autonomous posture, which answers every
admitted step, routine and destructive alike, and takes its confinement and its budget in exchange.
See the [CLI reference](./cli.md) for the flags and
[config reference](../reference/config.md) for the persisted `[permissions]` allow/deny lists.

## Secrets stay invisible to the model

Provider keys, program-declared `secret "NAME"` values, and host-materialized plugin credentials
are registered with a redactor and scrubbed from **all** model-visible tool output and logs, on both
success and error. Model stages operate on secret *references*, never their values. See
[Credentials & secrets](../security/credentials.md) for how references, resolution, and the redactor
implement this.

## Sub-agents inherit the policy

A delegated sub-agent runs under the same envelope as its parent and **cannot** approve destructive
operations on its own—its proposed actions use the same batch and intent checks, so a destructive
step is denied, not just a direct call. Capability scope only ever narrows on descent:
a role declared with `tools: []` gets **zero** tools.

## The invariant

> No model-stage operation, built-in tool, sub-agent operation, app operation, or plugin host
> callback reaches real IO without traversing this envelope. Trusted native plugin code remains
> outside that guarantee: it is not OS-sandboxed by default. When an OS backend is active,
> `[sandbox]` reduces that raw-syscall bypass risk by constraining writes and optionally disabling
> network access; v1 still permits filesystem reads, and networking remains open unless
> configuration or the CLI's unattended profile closes it — see [OS process sandboxing](../security/os-sandbox.md).

This is enforced by construction, not by convention — see [Concepts](../concepts.md) for how it fits
the typed-stage/authored-flow model, and the [source on GitHub](https://github.com/codewandler/flux) for the
runtime that enforces it.

## Related docs

- [Credentials and secrets](../security/credentials.md) — how secret values stay out of model-visible output.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — how plugin side effects are scoped.
- [OS process sandboxing](../security/os-sandbox.md) — opt-in confinement underneath this envelope.
- [Server authentication & tenancy](../security/server-auth.md) — who can drive a hosted flux server.
