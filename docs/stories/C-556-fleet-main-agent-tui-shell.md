---
id: C-556
title: "The fleet TUI is centered on the one main coordinator conversation"
pillar: Core
status: in-progress
priority: 0
epic: board-fleet-tui
design: docs/designs/board-fleet-tui.md
areas: [flux-tui, flux-cli, flux-orchestrate]
depends_on: [C-566]
note: "highest-priority Fleet dogfood repair — coordinator-only native Board/Fleet surface plus attention rail; CLI remains automation API"
---

# The fleet TUI is centered on the one main coordinator conversation

## Goal

Give a human one polished conversational surface for supervising the main coordinator while keeping
worker and decision attention visible but secondary.

## Acceptance

- [x] `flux tui` remains an explicitly labelled standalone chat; `flux tui --fleet[=ROOT]` validates
      one Fleet root, opens the main coordinator's isolated durable store and resumes its exact
      recorded session rather than whichever ordinary session happened to be latest.
- [x] The main coordinator transcript/composer owns focus; header shows the attachment mode, Fleet
      root, connection/revision, active goals and wave without allowing a stopped or invalid Fleet
      to masquerade as connected.
- [x] A responsive attention rail summarizes workers, open decisions, blocked work and red gates with
      keyboard navigation, mouse scrolling and a narrow-terminal fallback. `F2`, `/fleet` and
      `/board` open the operations surface; the ordinary composer keeps focus otherwise.
- [ ] Worker phase/attention comes from C-570's acknowledged report projection; raw model prose and
      host-observed tool activity may be shown separately but never impersonate worker status.
- [x] Sending requirements, choosing a suggested decision and acknowledged follow-ups use the same
      typed durable Fleet/Board bridge as the CLI and display accepted/delivered/completed or failed
      state. Observation alone is read-only; a decision changes only after an explicit confirmation.
- [x] The attached main agent receives a bounded native `fleet.agents` read operation over the same
      durable admissions as `flux fleet agents` and the Workers view. It can discover every worker
      without guessed ids or an approval prompt; this remains distinct from point-in-time A2A
      `fleet.worker_status`, and an operator-authored deny still wins.
- [x] The attached main catalog is a hard coordinator ceiling: only typed native Board management,
      typed native Fleet management, `task` for bounded research and hidden authored-loop machinery
      are installed. Shell, file editing, git mutation, web/plugin/eval/pane operations and legacy
      transient-process `fleet.*` operations are absent, not merely discouraged by instructions.
- [x] `task` children started by the main have an independently closed read-only research catalog
      and a separately configured operator-authored loop. They cannot fall back to generic
      `create_plan` or inherit Board/Fleet mutation, shell, edit, git-write or nested-task authority.
- [x] Fleet main config names an operator-authored loop. Each free-form turn hands that loop exactly
      the current request and bounded coordinator operations without the general adaptive
      `detect_intent`/`explore` path or retained-history budgeting. Missing/invalid loop config
      refuses before a model call instead of falling back.
- [x] Native main operations cover the acknowledged services needed to choose and dispatch work:
      bounded Board show/get/next/check and Fleet status/schedule/agents/run/message/cancel/resume.
      Safe reads do not prompt; mutations retain revision, idempotency and acknowledgement rules.
- [x] Live tmux dogfood in `flux:fleet` proves a free-form question, complete worker enumeration,
      dependency-satisfied work selection and one bounded sub-agent dispatch using the installed
      binary. The model never sees a general coding catalog and no terminal keystroke is required
      to reveal operations.
- [x] A repository-local `flux-tui` skill documents the exact installed-build, tmux respawn,
      literal-input, bounded-capture, color, durable-history and Fleet-main boundary checks without
      treating terminal scraping as the Board/Fleet automation API.
- [x] Restart reconstructs the view from durable events without terminal scraping.
- [x] Accessibility, theme, snapshot and interaction tests cover narrow/wide layouts and busy workers.
- [x] The TUI does not gain push/release/deploy or hidden board mutation authority.

## Progress

- 2026-08-05 — promoted under C-582. The explicit `--fleet` launch is deliberate: repository
  detection may offer attachment later, but silently changing an ordinary chat's session store or
  coordinator authority would make the header lie.
- 2026-08-05 — shipped explicit exact-session attachment, the coordinator-first responsive shell,
  durable intake acknowledgement and the typed operations boundary. Failing-first CLI parsing,
  source fixtures, narrow/wide rendering snapshots, the full `flux-cli` and `flux-tui` suites, and
  warning-denying targeted Clippy cover the implementation.
- 2026-08-06 — live `flux tui -m codex --fleet` dogfood showed the main agent asking the operator to
  supply worker ids while the adjacent Workers view already held them. Added an attachment-only
  `fleet.agents` operation backed by native Fleet state; it returns bounded identity/status records
  and never exposes instructions, prompts or retained turn payloads. A second live run caught an
  unnecessary read approval; the validated attachment now pre-authorizes this exact operation.
- 2026-08-06 — the next live turn exposed the larger authority defect: exact Fleet attachment still
  assembled the ordinary coding catalog, legacy process-worker `fleet.*` operations and the generic
  adaptive history path. The main process was intentionally paused. This story now owns the
  coordinator-only catalog, read-only research-child ceiling, operator-authored current-turn loop
  and native acknowledged Board/Fleet controls; these are release-blocking Fleet/Board defects.
- 2026-08-06 — added and validated `.agents/skills/flux-tui` so subsequent changes use one repeatable
  `task install` → `tmux respawn-pane` → send/capture/inspect beta-test loop and diagnose colors,
  tmux scrollback and durable Fleet session history as separate concerns.
- 2026-08-06 — installed/live dogfood proved the closed 18-operation parent catalog, approval-free
  durable worker census, bounded Fleet status and dependency-satisfied C-569 selection. The first
  research delegation exposed a remaining child-loop gap: the read-only child inherited generic
  `create_plan` and raised an approval modal. The beta restart recipe now uses `--yes` so observation
  cannot stall, while the product fix requires `[main].research_loop` and forces that authored loop
  over role defaults; auto-approval is not treated as authority for the unexpected operation.
- 2026-08-06 — the installed fix restarted `flux:fleet` with
  `flux tui -m codex --yes --fleet` (shell PID `3250359` → `3365318`, Flux PID `3365434`) while
  retaining the exact durable main session `s_1` and ANSI color. `/tools` remained the exact closed
  18-operation coordinator catalog. A fresh free-form turn used only authored `model segment`
  stages and dispatched research child `s_3`; its exported trace contains only `glob`, `grep` and
  `read`, with no `create_plan`, adaptive intent/explore, shell, edit, git mutation, Board/Fleet or
  nested-task operation. The deliberately asked-about dirty local method was absent from the
  Board-pinned source snapshot, so its answer was rejected as research evidence while its closed
  authority trace was accepted.
- 2026-08-06 — the durable-main integration now makes a real main turn call `task`, proves the
  configured child loop returns, then proves the next turn resumes the original coordinator session
  rather than the newer correlated child session. Full `codewandler-flux-orchestrate` and `flux-cli`
  suites, warning-denying targeted Clippy, embedded-doc build/check, website mirror tests, local
  skill validation and the complete `task install` workspace-library/install gate pass.
