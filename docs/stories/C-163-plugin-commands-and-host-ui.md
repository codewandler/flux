---
id: C-163
title: Plugin-registered commands and host UI prompts
pillar: Core
status: backlog
priority:
epic:
design:
note: "plugins expose OPS only — PluginCapabilities (flux-plugin-protocol lib.rs:515-578) has no ui/command verb, and the host-callback surface is just host.read/host.write — so a plugin cannot add a slash command or ask the user a question; the frame protocol is command-keyed (lib.rs:44-59) on an independent additive 1.x line, which is exactly the seam this needs"
---

# Plugin-registered commands and host UI prompts

## Goal
Let a plugin contribute to the *human* surface, not just the model surface. Two additive capability
verbs on the existing protocol: a plugin may **register a command** (a `/name` the user can invoke,
routed to one of the plugin's ops) and may **ask the host a question** (notify / confirm / input /
select) while an op runs. Today a plugin's only expression is an op projected to the model, so a
plugin that needs a user decision has to fail and explain, and a plugin that ships a genuinely
user-facing workflow has no way to offer it.

## Acceptance
- [ ] Two new deny-by-default entries in `PluginCapabilities`
      (`crates/flux-plugin-protocol/src/lib.rs:515-578`) gate the new verbs — a plugin whose
      manifest omits them cannot register a command or prompt the user, pinned by test (the
      existing capability-gate pattern, not a new one).
- [ ] A manifest-declared command appears in the CLI/TUI command list and dispatches to the named
      op through the **normal envelope** (authorization → approval → guarded IO) — a command is not
      a bypass. Failing-first test asserting a plugin command's op call is policy-gated identically
      to a model-issued one.
- [ ] A host UI request (`confirm` / `input` / `select` / `notify`) from a running op renders in
      both the plain CLI and the TUI, and **cannot be used as an approval substitute**: a UI
      confirm never satisfies the approval gate for a destructive op. Test pins that a plugin
      cannot self-approve.
- [ ] Headless behavior is defined and tested: under `--yes` / non-interactive / served contexts, a
      UI request resolves by a declared default or fails the op honestly — it never blocks forever.
- [ ] Prompt text from a plugin is untrusted input and is rendered as text, never interpreted
      (the C-113/C-114 approval-modal lesson).
- [ ] The wire additions ride the protocol's additive `1.x` line with the golden JSON fixtures and
      the version-bump guard updated (C-141…C-147 machinery).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp plugins register tools *and* commands
  (`amp.registerCommand`) and can drive UI interactions (notify / confirm / input / select).
- Evidence the gap is real: `crates/flux-plugin-protocol/src/lib.rs:515-578` (`PluginCapabilities`
  — process / secrets / http / conn / blob / discover / credential / fs; no command or UI verb) and
  the host-callback command set, which is `host.read` / `host.write` only.
- Why the seam fits: `Frame` is command-keyed (`lib.rs:44-59`) and the protocol crate sits on its
  own additively-versioned `1.x` line, so both verbs are additions rather than breaks — the
  decoupling epic (C-141…C-147) built exactly this affordance.
- The security question to settle *in the design*, before any code: a plugin that can pop a dialog
  is a plugin that can phish the user inside a trusted surface. Constrain the rendering (plugin
  name always shown, no styling control) rather than relying on plugin good behavior.
