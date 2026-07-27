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

- **Nothing destructive happens silently.** Deleting files, overwriting work, or running risky
  commands always pauses to ask you first. Reading a file usually just works; changing one asks.
- **You approve before anything big runs.** You can say yes once, always allow a routine action, or
  say no. No is always the safe default.
- **Your passwords and API keys stay hidden.** flux remembers *where* a key is stored, not the key
  itself, and it scrubs secrets out of anything shown on screen. The AI only ever sees a name for
  the secret, never the value.
- **Add-ons can't snoop.** Plugins (integrations for things like GitLab or Slack) only get the exact
  permissions you grant — they can't read your passwords or wander the network on their own.
- **flux won't quietly send your data somewhere it shouldn't.** It blocks connections to private and
  internal network addresses unless you explicitly allow them, and it won't trust a site just
  because its *name* looks safe.
- **It stays on track.** If a run gets interrupted or hits a limit, flux picks up cleanly instead of
  leaving a half-finished mess or a corrupted conversation.
- **There's a record.** flux keeps a log of what it did, so you can always look back and see exactly
  what happened.

## What you still own

No tool removes all risk, and flux won't pretend to. A few things stay in your hands:

- **Be thoughtful about auto-approve.** There's a mode that says yes to everything for unattended
  runs. It's convenient, but it means no one is double-checking. Use it only when you trust the task
  completely.
- **Only install add-ons you trust.** A plugin is a real program. flux checks that it's the genuine
  article and limits what it's allowed to do, but it can't make a malicious add-on safe. Install
  plugins the way you'd install any software — from sources you trust.
- **Keep your login file private.** On your own computer, flux stores your sign-in tokens in a file
  that only your account can read. Don't share it or loosen its permissions. (Teams can keep tokens
  in a proper vault instead.)

## Where to go next

- **Just using flux?** You're done — the defaults above are on out of the box.
- **Want the how and why?** Start with the [Security overview](./overview.md).
- **Running flux for a team or exposing it on a network?** Read
  [Server authentication & tenancy](./server-auth.md).

## Found a security problem?

Please tell us privately rather than posting it publicly. Open a **private security advisory** on
[GitHub](https://github.com/codewandler/flux/security/advisories/new) and we'll take it from there.
Anything that lets flux act outside the checkpoint above — touching files it shouldn't, skipping an
approval, leaking a secret — is treated as a serious issue.
