---
id: C-551
title: "The roadmap can retire every fleet coordination helper in favor of Flux CLI"
pillar: Core
status: done
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-tui, examples, docs]
note: "dogfood exit — bounded inspect, activity, progress/report and worktree audit close every current helper-script use case"
---

# The roadmap can retire every fleet coordination helper in favor of Flux CLI

## Goal

Close the operational surface around the durable fleet so this repository and calling agents need no
custom schedule, worker-control, context, activity, progress or visualization scripts.

## Acceptance

- [x] `fleet worktrees`, redacted `events --follow`, `logs`, `agents`, `dashboard` and bounded
      `inspect snapshot|wave|worker|result|activity|worktree|integration|source|search|story|
      pull-request` have deterministic human and JSON fixtures with explicit upper bounds.
- [x] `board stats --history` and `board report --format json|tsv|html|svg` replace the roadmap
      progress collector and visualization with the same current/history facts and stable ordering:
      epic/story/optional-task/criterion/implementation ratios, state histogram, document counts,
      canonical commits, program stories, tranche lanes, waves and groups, plus daily scope-added,
      scope-removed and completed deltas. Renderers consume the JSON metric cube directly.
- [x] A side-by-side harness covers every mapping recorded in Decision 0010: refresh, validate,
      status, schedule, worktrees, start/stop, dispatch, follow-up, maintenance task, coordinator note,
      child status, activity, context, progress and report. Differences are either zero or an
      explicitly accepted schema improvement.
- [x] Redaction corpus tests cover credentials, `.env`, key files, model commentary, commands, diffs
      and JSON fields before persistence and rendering.
- [x] Bounded inspection remains responsive while a worker is busy and does not read unbounded
      terminal history, repository files or diffs.
- [x] The roadmap dogfood fixture uses only declarative Flux configuration plus repository-specific
      gate executables. Static acceptance rejects references to its retired coordinator scripts,
      Track Python generator and private fleet socket client.
- [x] `flux fleet skill` includes the safe dispatch/status/message/resume/apply loop and points to
      `fleet schema`/`inspect` for detail; every example executes against the dogfood fixture.
- [x] Website docs, `WHATS-NEW.md` and changelog describe the scriptless path and explicit limits.
- [x] Full repository gate, embedded-docs check and roadmap side-by-side parity gate are green before
      any helper is deleted.

## Notes

- Depends on A-117, C-242, C-244, C-245 and C-550. Helper deletion is the last step, never the proof
  mechanism.
- 2026-08-05 corrective audit: the helper deletion and static no-reference acceptance did not happen
  in the real roadmap despite this done state. C-588 reopens the adoption boundary as a new explicit
  story and proves it from the actual four-repository fixture.
