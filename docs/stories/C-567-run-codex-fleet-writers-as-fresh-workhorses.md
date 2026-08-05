---
id: C-567
title: "Codex Fleet writers run as fresh bounded workhorses"
pillar: Core
status: ready
priority: 0
epic: fleet-loop
design: docs/designs/native-board-fleet-cli.md
areas: [flux-cli, flux-runtime]
depends_on: [C-566]
note: "five-writer dogfood stop-line — the recursive adaptive explorer exhausted 50 calls or 512 KiB in every implementation lane before any commit"
---

# Codex Fleet writers run as fresh bounded workhorses

## Goal

Let an explicitly configured Codex-backed Fleet story template execute one assigned contract as a
fresh implementation workhorse instead of routing the job through Flux's recursive adaptive
explorer. The worker keeps its admitted repository, mode, capability and session boundaries while
producing a small durable Fleet receipt that can be continued and handed off normally.

## Acceptance

- [ ] Failing first, a hermetic five-writer-shaped fixture proves the existing Fleet launcher sends
      every Codex story worker through the adaptive `detect_intent`/`explore` loop and cannot
      distinguish a useful partial tree from the observed 50-call/history-budget terminal failure.
- [ ] An agent template can explicitly select the versioned Codex workhorse runner. Validation
      requires a `codex/...` model and an available compatible Codex executable before admitting or
      preparing work; existing Flux-native templates remain unchanged.
- [ ] Every newly admitted workhorse gets a fresh isolated Codex home and fresh thread. It may read
      only the host authentication bridge plus its repository-owned instructions and `AGENTS.md`;
      it never inherits the coordinator conversation, user config, history, memories, plugins,
      apps, goals or another worker's thread.
- [ ] Write and read-only modes map to fail-closed process/workspace sandboxes under Fleet's outer
      writable-root/read-root/fence envelope. The admitted runner, model, mode, capability digest,
      worktree and thread are durable snapshots; message, rework and resume use that exact runner
      and thread, and runner drift requires explicit re-admission.
- [ ] Codex JSONL is normalized into `flux.fleet-agent-turn/v1` with the final answer, usage, bounded
      event counts/digests and typed terminal error. Tool inputs, command output, diffs, repository
      contents and reasoning are never copied into default Fleet state or receipts.
- [ ] Hermetic lifecycle coverage uses a fake Codex executable to prove fresh admission, exact
      continuation, cancellation, non-zero/malformed/missing-terminal refusal, byte ceilings and
      concurrent workers without provider credentials or a mutable user Codex home.
- [ ] The roadmap dogfood launches five fresh writers on five different stories across Flux,
      Connectors and Exchange; all five retain their admitted ceiling, create exact story commits
      and produce handoff-ready bounded receipts.
- [ ] The public Fleet guide, design, changelog, website mirror and embedded documentation explain
      the explicit workhorse runner, prerequisites, isolation, recovery and the Flux-native
      fallback without implying that `working` alone proves progress.

