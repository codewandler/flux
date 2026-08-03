---
title: Security in plain terms
description: What flux does to keep you, your files, and your passwords safe — explained without jargon, for people who use flux rather than build it.
---

# Security in plain terms

This page is for people who **use** flux and want to know it's safe — no engineering background
needed. If you're a developer or you run flux for a team, the [Security overview](./overview.md)
goes deeper.

## The one idea

flux is an AI assistant that can really do things: read and change your files, run commands, and
connect to the internet. That's powerful, so flux is built around a single rule:

> **The AI never touches your files, runs a command, or opens a network connection directly.**
> Every request has to pass through one checkpoint that decides whether it's allowed.

You can think of it like a careful assistant who isn't allowed to act on their own. Before doing
anything that matters, they check with you, and they only have the keys you handed them — nothing
more. That rule is built into how flux works; it can't be switched off by the AI or by a clever
prompt.

## What that means for you

- **Interactive runs pause before destructive work.** Deleting files, overwriting work, or running
  risky commands asks for approval in the normal interactive modes. If you deliberately use
  `--yes`, flux answers those prompts for admitted actions, including destructive ones. It does not
  authorize anything excluded by policy or by an app/agent ceiling.
- **You approve before anything big runs in interactive mode.** You can say yes once, always allow a
  routine action, or say no. No is always the safe default.
- **Use secret references; never paste a password or API key into a prompt.** On supported paths the
  model receives a name or location while the host or external connector handles the value. Locally
  materialized values are scrubbed from captured output only after Flux has registered them. A value
  pasted into a prompt is unknown to that redactor, is sent to the model, and is written into the
  durable session log.
- **Plugin access through flux is narrow.** Host callbacks are limited to the programs, files,
  secrets, connections, and network hosts declared by the plugin. A plugin is still a trusted native
  program and is not OS-sandboxed by default in interactive use; malicious code could make its own
  system calls. Install only plugins you trust, and turn on the OS sandbox for defense in depth.
- **Host-mediated network access is guarded.** flux resolves destinations and blocks private and
  internal addresses unless you grant them. A trusted native plugin can bypass its host callbacks
  when it is not OS-sandboxed, which is why plugin trust still matters.
- **Interrupted sessions remain usable.** Cancellation and limits leave valid conversation history
  that can be resumed. Effects that already completed are not rolled back automatically.
- **Host-mediated work leaves a record.** flux logs the operations, approvals, and destructive
  markers that cross its runtime. Direct system calls made by an unsandboxed native plugin are
  outside that audit trail.

## What you still own

No tool removes all risk, and flux won't pretend to. A few things stay in your hands:

- **Be thoughtful about auto-approve.** There's a mode that says yes to every admitted action for
  unattended runs. It is convenient, but it means no one is double-checking within the configured
  ceilings. Use it only when you trust the task completely.
- **Only install add-ons you trust.** A plugin is a real program. The signed pack is signature- and
  checksum-verified; local and source installs are explicitly unverified paths. Capability checks
  cannot make malicious native code safe, so treat every plugin like any other dependency you run.
- **Keep your login file private.** On your own computer, flux stores your sign-in tokens in a file
  that only your account can read. Don't share it or loosen its permissions. (An embedding host can
  inject a Vault-backed store for a team deployment.)
- **Treat session history as sensitive.** Raw prompts and answers are saved without redaction at the
  moment they are written. Export catches recognizable credential shapes, but it cannot reconstruct
  every secret a past run knew. Keep credentials out of prompts and use the supported references in
  [Credentials & secrets](./credentials.md).

## Where to go next

- **Just using flux?** The runtime checks and private-network guard are active out of the box. The OS
  process sandbox is off for ordinary interactive use; see the overview before running unfamiliar
  native plugins.
- **Want the how and why?** Start with the [Security overview](./overview.md).
- **Running flux for a team or exposing it on a network?** Read
  [Server authentication & tenancy](./server-auth.md).

## Found a security problem?

Please tell us privately rather than posting it publicly. Open a **private security advisory** on
[GitHub](https://github.com/codewandler/flux/security/advisories/new) and we'll take it from there.
Anything that lets flux act outside the checkpoint above — touching files it shouldn't, skipping an
approval, leaking a secret — is treated as a serious issue.
