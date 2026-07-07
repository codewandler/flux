---
id: C-39
title: Repair the live smoke gate — subcommand CLI + a hermetic shape guard
pillar: Core
status: done
design:
epic:
note: steps 1–5 of scripts/smoke-live.sh invoke the retired flag-style CLI (`flux -p -m`, `--agent`, `--serve`) — the pre-release session-shape gate is silently broken; steps 7/8 already use the current form
---

# Repair the live smoke gate — subcommand CLI + a hermetic shape guard

## Goal
`scripts/smoke-live.sh` is the standing pre-release gate for the one bug class the offline mock
cannot catch (provider message-shape 400s), but its steps 1–5 still invoke the pre-subcommand CLI
(`flux -p -m …`, `flux --agent --yes … -p`, `flux --serve`) — those top-level flags no longer exist
on the clap tree (`crates/flux-cli/src/main.rs`), so the first five checks fail on argument parsing,
not on what they were built to test. Fix the invocations and add the guard that keeps the script
from rotting silently again.

## Acceptance
- [x] Steps 1–5 rewritten onto the subcommand CLI (`flux run -p`, `flux run --yes`,
      `flux app run --serve` / the current serve form) and verified with one full live run
      (`FLUX_SMOKE_MODEL` per docs/model.md; report the run in Progress).
- [x] A hermetic drift guard: the script's invocation shapes run against the `mock` provider in CI
      (or a dedicated shape-check mode), so a future CLI-surface change fails the gate instead of
      breaking the script unnoticed. Failing-first: the guard must fail against the current
      (pre-fix) script.
- [x] The "Standing pre-release gate" section of docs/roadmap.md still describes the script
      accurately after the change.

## Progress
- 2026-07-07: Confirmed the exact clap tree in `crates/flux-cli/src/main.rs` — there is no
  top-level `-p`/`--agent`/`--serve` on `Cli` any more. `-p`/`--print` and `--agent` are now
  *hidden no-op* fields on `AgentFlags` (flattened into `run`/`plan`/`tui`/`review`); a bare
  `flux run <prompt>` is already one-shot. Serving now lives at `flux app run --serve <addr>`
  (`AppAction::Run`, D-23) — with no `<program>` it serves the built-in coding agent (the former
  `flux serve`).
- Rewrote steps 1–5 in `scripts/smoke-live.sh` onto four shared wrapper functions
  (`flux_oneshot`/`flux_agentic`/`flux_continue`/`flux_serve`) so the live legs and the new
  `--shapes` guard call the *identical* invocation lines — one edit keeps both in sync:
  - 1. `flux run -m "$MODEL" '<prompt>'`
  - 2/3. `flux run --yes -m "$MODEL" ['-c'] '<prompt>'`
  - 4. same as 3, under `FLUX_COMPACT_CHARS=1500`
  - 5. `flux app run --serve "$ADDR" -m mock --yes` (A2A section stays pinned to `mock`, unchanged
    from before). Preserved `exec` inside the wrapper so `$!` still names the real `flux` PID for
    the trap's `kill`.
- Added the hermetic guard: `scripts/smoke-live.sh --shapes` (or `FLUX_SMOKE_SHAPES=1`) forces
  `MODEL=mock`, skips every live/credential-needing step, and runs `check_parses`/
  `check_serve_parses` against the same wrappers in scratch dirs. The signal is exit code 2 +
  a `^error: ` stderr line — clap's own (verified: every one of flux's own error paths only ever
  exits 1, never 2) — so it only ever flags a genuine parse-time regression, not a business-logic
  failure. Wired as a new step in the `check` job of `.github/workflows/ci.yml`, right after
  `Build`, reusing that job's debug binary (`FLUX_BIN="$PWD/target/debug/flux"`) — no extra build,
  no credentials.
  - **Failing-first, demonstrated**: copied the fixed script to a scratch file and swapped only the
    four wrapper bodies back to the pre-fix forms (`"$FLUX" -p -m …`, `"$FLUX" --agent --yes … -p`,
    `"$FLUX" --agent --yes … -c -p`, `"$FLUX" --serve …`), keeping the guard harness byte-identical.
    Running it with `--shapes` failed all 5 checks, e.g. `shape drift: 1. one-shot — no longer
    parses (error: unexpected argument '-p' found)` / `... '--agent' found` / `... '--serve'
    found`. Running the *actual* fixed script with `--shapes` afterwards: `SHAPE CHECK PASS — 5
    checks (0 skipped)`.
- **Live run** (`FLUX_SMOKE_MODEL='openrouter-anthropic/anthropic/claude-sonnet-4.6'` — Anthropic
  direct key is out of credits, OpenRouter's Anthropic-Messages route is the working live model):
  `SMOKE FAIL — 8 passed, 3 failed (0 skipped)`. Honest breakdown:
  - Steps 1–4: **PASS** — one-shot, agentic edit, `--continue`, compaction-then-continue all
    completed with no provider error. This is the direct proof the CLI rewrite works live.
  - Step 5: the CLI half **PASS**es (`app run --serve` starts; `/health`; the discovery card is
    reachable) — proving the `--serve` fix works — but the two JSON-RPC calls **FAIL**:
    `tasks/send`/`tasks/sendSubscribe` → `"Method not found"`. Traced the root cause in
    `crates/flux-server/src/a2a.rs` and `crates/flux-a2a/src/{server,client}.rs`: the live method
    names are `message/send`/`message/stream`, not `tasks/*` — a **separate, pre-existing protocol
    naming drift**, unrelated to the CLI-subcommand issue this story fixes. Left AS-IS (out of
    scope — the story's Note only ever named the flag-style CLI break); flagging for a fast
    follow-up story since it means step 5 will keep failing on the live gate until that's fixed
    too.
  - Step 6 (ollama leg) **FAIL**ed on the *same class* of bug this story fixes
    (`"$FLUX" --agent --yes -m "ollama/$OLLAMA_MODEL" -p …`) — ollama was reachable and the model
    pulled in this environment, so the old-flag call actually ran and hit the clap parse error.
    This step is outside the "steps 1–5" scope given for this story and was left untouched;
    flagging it as the same follow-up-worthy finding (it needs the identical
    `flux_agentic`-style fix).
  - Steps 7/8 (claude/codex subscription legs): **PASS** (unchanged, already on the current form).
  - No repo pollution from the run (verified via `git status` before/after — only the three
    intended files changed).
- Checked `docs/roadmap.md`'s "Standing pre-release gate" section: the one-shot/`--yes` examples
  it already cited were fine (`flux run -p`, `flux run --yes` both still parse — `-p` is the
  hidden no-op alias), so no correction was needed there. Added one short paragraph noting the new
  CI `--shapes` guard so the section describes the full picture (manual live gate + the always-on
  hermetic guard).

## Notes
- Found during the 2026-07-07 hardening/docs/cleanup survey; staleness first flagged in C-19's
  Progress log ("steps 1-5 of the script are stale against the subcommand CLI").
- Steps 7/8 (claude/codex subscription legs) already use `flux run --yes` — leave them as the
  reference form. The manual cancel-then-continue step stays manual (REPL-only Ctrl-C).
- **Follow-up work surfaced during the live run — both CLOSED the same day (2026-07-07) in the
  same hardening push** (fixed in the script directly; small enough that separate stories would
  have been ceremony):
  1. Step 5's `tasks/send`/`tasks/sendSubscribe` JSON-RPC method names were stale against the live
     A2A server — rewritten to `message/send`/`message/stream` with the params shape
     (`parts[].kind`, `contextId`, no `params.id`) pinned by the new C-41 integration tests
     (`crates/flux-server/tests/a2a_message_send.rs` / `a2a_message_stream.rs`); verified against
     a live mock-provider server (`message/send → completed`, `message/stream → working…completed`).
  2. Step 6 (the ollama leg) called the old `--agent --yes … -p` form — rewritten to the current
     `flux run --yes -m ollama/<model>` shape (same as `flux_agentic`, model swapped).
