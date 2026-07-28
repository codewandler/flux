---
id: C-162
title: "[tools] disable — a plain blocklist for turning ops off"
pillar: Core
status: backlog
priority:
epic:
design:
note: "tool groups (flux-evidence lib.rs:185-193, .flux/groups.toml at flux-config lib.rs:1028) only ever ADD surface when evidence fires — there is no subtractive knob, so 'I never want browser.* in this repo' means hand-writing authorization policy for a prompt-size/attack-surface concern that isn't an authorization question"
---

# `[tools] disable` — a plain blocklist for turning ops off

## Goal
Give operators one obvious way to say *"this repo never uses these ops"* — `[tools] disable =
["browser.*", "web.*"]` in `.flux/config.toml`. Today the only subtractive control is the
authorization policy, which is the right instrument for *authority* and the wrong one for *surface*:
an op the agent should never see still costs prompt tokens, still invites the model to try it, and
still widens the prompt-injection target. Tool groups (`flux-evidence/src/lib.rs:185-193`) are
purely additive — they surface ops when evidence fires, never hide them.

## Acceptance
- [ ] `[tools] disable = [...]` in `.flux/config.toml` accepts exact op names and `family.*` globs;
      a disabled op is absent from the surfaced tool set — failing-first test asserting it never
      reaches the model.
- [ ] Disabling is **surface-only and defense-in-depth, not a security boundary** — a disabled op
      is also refused at dispatch (so a cached plan or a resumed session can't call it), and the
      docs say plainly that the authorization policy remains the security control. Test covers the
      dispatch refusal path.
- [ ] Layering follows the existing config precedence (user → project), and an entry matching no
      known op is a startup warning naming the entry, not a silent no-op.
- [ ] `flux` surfaces what was disabled somewhere discoverable (the natural home is the C-128
      `flux doctor` diagnostics, if that lands first) so a mysteriously-missing op is one command
      from an explanation.
- [ ] Disabling is stable across a turn, so it cannot churn the prompt prefix mid-session (the A-95
      lesson).

## Progress
- (not started — filed from the 2026-07-28 Amp feature-mining pass)

## Notes
- Source: [../research/amp.md](../research/amp.md) — Amp's `amp.tools.disable`, a glob-supporting
  tool-name blocklist.
- Evidence the gap is real: `crates/flux-evidence/src/lib.rs:185-193` (`ToolGroup` — `tools` +
  `surface_when`, additive only) and `crates/flux-config/src/lib.rs:1028` (the `.flux/groups.toml`
  manifest that carries them).
- Small by design. The value is that it is *obvious*: today a user who wants less surface has to
  learn the policy language to express a preference that isn't about permission at all.
- Do not let this become a second permission system. If the two ever disagree, the policy wins and
  the docs must say so.
