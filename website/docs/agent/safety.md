---
title: Safety & approvals
description: "The safety chain for every operation, including authorization, permissions, approvals, and guarded IO."
---

# Safety & approvals

flux is built so every model-emitted operation, built-in tool, sub-agent operation, app operation,
and plugin **host-capability request** traverses one mandatory chain before flux performs real IO.
This page explains that chain, what prompts, and how approval decisions interact with policy.

Plugin executables are trusted native dependencies, not OS-sandboxed by default. The chain governs
their projected operations and host callbacks; it cannot prevent a malicious binary from making a
direct syscall unless opt-in OS-level sandboxing (`[sandbox]`) is enabled underneath the envelope.
See [Plugin trust & signing](../security/plugin-trust.md) and
[OS process sandboxing](../security/os-sandbox.md).

## The one envelope

Every operation — from a one-shot prompt, a `/run`, a normal turn, a plugin op, or a sub-agent —
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

## What prompts, what doesn't

- **Reads** (`read`, `glob`, `grep`, `search`) are pre-allowed — they run without prompting.
- **Writes and commands** prompt for approval unless you pass `--yes` or have an allow-rule in your
  config.
- **Destructive operations** (`rm -rf`, `git push --force`, `mkfs`, …) **always re-fire the approval
  gate** — even inside a plan you already approved: a destructive step that wasn't visible in the
  approved plan re-fires the gate when it's dispatched, so it can never slip through unapproved. What
  the gate *does* then depends on the approver in force: the interactive approver prompts you, a
  sub-agent's approver denies outright, and `--yes` (a headless allow-all approver) approves it
  automatically. So `--yes` does **not** exempt destructive ops from the gate — it answers the gate
  "yes" for them too, along with everything else still admitted by the current capability scope. An
  app-declared `permissions` ceiling remains absolute. Use `--yes` only in trusted, unattended contexts.

## Approving a prompt

When flux prompts, you have three choices:

- **`y`** — approve this one operation.
- **`a`** — always approve operations like this; the choice is persisted to `.flux/config.toml`.
- **`N`** — deny (the default). The operation does not run.

Unattended runs use `--yes` to auto-approve **every admitted** step — routine and destructive alike
(it installs a headless allow approver, but never widens an app/agent capability ceiling). Reserve it
for trusted contexts. When you want routine steps
to flow but destructive ones to still stop and ask, don't use `--yes`: run interactively with
allow-rules for the routine tools — the destructive gate re-fires past an allow-rule, so those steps
still prompt. See the [CLI reference](./cli.md) for the flags and
[config reference](../reference/config.md) for the persisted `[permissions]` allow/deny lists.

## Secrets stay invisible to the model

Provider keys, program-declared `secret "NAME"` values, and host-materialized plugin credentials
are registered with a redactor and scrubbed from **all** model-visible tool output and logs, on both
success and error. The model plans against secret *names*, never their values. See
[Credentials & secrets](../security/credentials.md) for how references, resolution, and the redactor
implement this.

## Sub-agents inherit the policy

A delegated sub-agent runs under the same envelope as its parent and **cannot** approve destructive
operations on its own — its emitted *plans* are intent-checked too, so a destructive step buried in
a sub-agent's plan is denied, not just a direct call. Capability scope only ever narrows on descent:
a role declared with `tools: []` gets **zero** tools.

## The invariant

> No model-emitted operation, built-in tool, sub-agent operation, app operation, or plugin host
> callback reaches real IO without traversing this envelope. Trusted native plugin code remains
> outside that guarantee: it is not OS-sandboxed by default, though opt-in OS-level sandboxing
> (`[sandbox]`) closes that gap — see [OS process sandboxing](../security/os-sandbox.md).

This is enforced by construction, not by convention — see [Concepts](../concepts.md) for how it fits
the plan-first model, and the [source on GitHub](https://github.com/codewandler/flux) for the
runtime that enforces it.

## Related docs

- [Credentials and secrets](../security/credentials.md) — how secret values stay out of model-visible output.
- [Plugin capability sandbox](../security/plugin-sandbox.md) — how plugin side effects are scoped.
- [OS process sandboxing](../security/os-sandbox.md) — opt-in confinement underneath this envelope.
- [Server authentication & tenancy](../security/server-auth.md) — who can drive a hosted flux server.
