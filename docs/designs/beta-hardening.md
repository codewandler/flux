# v0.6.0 beta hardening (epic)

**Status:** ✅ implemented 2026-07-08 (all 12 stories done; full gate green) · **Pillar:** cross-cutting (Agent / Language / Core) · **Epic slug:** `beta-hardening`

Tracks the fixes surfaced by the first external beta test of a shipped release (`flux 0.6.0`),
sequenced into a ranked backlog. This design is the *why, the evidence, and in what order* — the
per-finding acceptance lives in the child stories.

## Context

An external tester (Codex, in a clean `/tmp` workspace against the published `0.6.0` release binary
and source checkout) exercised the product end-to-end: release download/checksum, public docs vs.
observed behavior, OpenRouter- and Codex-backed turns, native Flux-Lang `flow run`/replay/resumable
flows, the built-in A2A server (card, blocking/non-blocking send, `tasks/get`, cancel, resubscribe,
stream, push), program `app run`/`emit`/`spawn`/`ask` and custom A2A serving, the plugin layer
(status, dry-run, websearch, Hugging Face, GitLab refusal, generated skill, replay), the filesystem
boundary flags, interactive plan approval/denial, multi-file coding tasks, Flux-Lang fixtures `01`
through `66`, and sub-agent delegation (role scopes, `with_tools` narrowing, correlation/replay).

**The core thesis held.** From the report: *"model output becomes visible plans, runtime guardrails
are real enforcement points, recorded sessions replay offline, plugin calls are host-mediated, and
sub-agent tool scopes are bounded."* Confirmed strengths: auditable plan execution, offline replay
(reads, model-backed nodes, writes, plugin calls, direct flow runs, correlated child streams),
usable stateful A2A wire surface, real runtime guardrails (`budget`/`with_tools`/`try`/`timeout`/
`scope`/`once`/`throttle`/`saga`/top-level-await halt), live composite ops and app programs,
practical plugin-skill generation, served program agents honoring read/no-read tool scopes,
recovery-and-replay on a multi-file coding task, conservative resumable-flow fast-forward, and
correctly-scoped sub-agent roles.

> **The report's own bottom line:** *"Flux v0.6.0 is credible as a beta … The release needs a
> focused hardening pass before broad beta use. Most issues are fixable with targeted docs/runtime
> alignment and surface parity work rather than a redesign."*

The issues cluster into: (1) **docs/runtime mismatches** (public docs overstate or contradict
observed behavior), and (2) **surface-specific gaps** in resumable flows, direct-`flow run`
sub-agent correlation, program `--serve` provider parity, and a SIGPIPE panic. None implies a
redesign.

## The findings (embedded — the source report lived in an ephemeral `/tmp` workspace)

The original artifacts (`/tmp/flux/beta-test/{final-report,report,findings,matrix,log}.md`,
trial fixtures, saved A2A responses) are not in-repo and will not survive. The essence is captured
here so the backlog stays grounded after they vanish.

| ID | Finding | Kind |
|---|---|---|
| F-001 | **Mock docs overpromise** — getting-started makes mock mode sound like a representative agent demo; observed mock runs return canned `Finished.`. Useful for wiring smoke tests, not behavioral validation. | docs |
| F-002 | **`openai/gpt-4o-mini` failed planner contract** — a trivial plan request failed after validator repair attempts. OpenRouter and Codex controls worked. | model robustness |
| F-003 | **Safety docs conflict on `--yes`** — public safety docs say destructive ops always re-confirm even with `--yes`; README/internal docs/source show `--yes` installs a headless allow-all approver, including destructive ops. | docs vs. behavior |
| F-004 | **Weak model repeated plugin reads** — an OpenRouter model called `websearch.search` then repeated the same op instead of answering. Codex completed the same task. | loop robustness |
| F-005 | **A2A docs/card protocol-version mismatch** — public docs mention A2A "v1.0"; live AgentCard reports `protocolVersion: "0.3.0"`. | docs |
| F-006 | **Broken-pipe panic** — `flux sessions \| head -8` printed rows then panicked on broken pipe. | runtime bug |
| F-007 | **Flux-Lang docs examples rejected by v0.6.0** — text `expr(...)`, JSON `"returns":"Object"`, and rendered non-writable `expr(...)` trees did not round-trip as runnable syntax. | docs vs. runtime |
| F-008 | **Text scalar binds stringify booleans/numbers** — bare text binds like `$n = 1`, `$ok = false` become strings, affecting `match` and structured output. | language bug |
| F-009 | **`peek` docs overstate usability** — docs show `peek` as bindable; the tested bind shape was rejected. | docs vs. runtime |
| F-010 | **Validation diagnostic header misleading** — unrelated validation failures appear under `diagnostics - the plan references unknown operations`, even when the bullet itself is correct. | diagnostics |
| F-011 | **Context-pack shrinkage not obvious in CLI** — member dropping is visible in returned metadata but not clearly surfaced in plain CLI run output. | UX |
| F-012 | **`parse` less composable than other pure nodes** — works in bind position; rejected in object-template leaves and direct returns. | language bug |
| F-013 | **`fluxlang compile` rejects modules with leading `op`** — the developer CLI rejects a module that `flux flow run` executes successfully. | dev-CLI bug |
| F-014 | **Program `--serve` mock-provider path fails** — `app run <program> --serve … -m mock` exposed the card but `message/send` used the Anthropic path and failed on low credits; `-m codex/gpt-5.5` worked. | surface parity |
| F-015 | **`flow run --resume` fast-forwards `await` without binding its value** — top-level await halts cleanly (`kind:"awaiting"`), but `--resume` has no payload option; resume advanced past the await and failed with `unbound symbol $reply`. | runtime bug |
| F-016 | **Direct `flow run` sub-agent children not correlated to parent streams** — child streams had `agent_id=subagent:<role>` and `correlation_id:null`, so `replay --sub-agents` did not recurse. Normal `flux run` agent turns set correlation correctly. | replay bug |

**Excluded from the beta run:** TUI (per request), and the live scoped-egress allowlist test (the
only concrete private target was an internal corporate GitLab endpoint; default refusal *was*
verified).

## Finding → story map

| Story | Finding(s) | Pillar | Rec. order |
|---|---|---|---|
| [C-45](../stories/C-45-yes-destructive-approver-truth.md) | F-003 | Core | **1** |
| [A-58](../stories/A-58-flow-resume-await-payload.md) | F-015 | Agent | **2** |
| [A-59](../stories/A-59-flow-run-subagent-correlation.md) | F-016 | Agent | **3** |
| [A-60](../stories/A-60-serve-mock-provider-parity.md) | F-014 | Agent | **4** |
| [A-61](../stories/A-61-cli-broken-pipe-no-panic.md) | F-006 | Agent | **5** |
| [L-43](../stories/L-43-text-scalar-bind-types.md) | F-008 | Language | 6 |
| [L-44](../stories/L-44-parse-node-composability.md) | F-012 | Language | 6 |
| [L-45](../stories/L-45-fluxlang-compile-leading-op.md) | F-013 | Language | 6 |
| [A-62](../stories/A-62-validation-diagnostic-headers.md) | F-010 | Agent | 6 |
| [A-63](../stories/A-63-context-pack-shrinkage-surface.md) | F-011 | Agent | 6 |
| [C-46](../stories/C-46-beta-docs-truth-pass.md) | F-001, F-005, F-007, F-009 | Core | 7 |
| [A-64](../stories/A-64-weak-model-planner-robustness.md) | F-002, F-004 | Agent | 7 |

## Recommended fix order (from the report)

1. **F-003** — resolve `--yes` destructive-op docs immediately (C-45).
2. **F-015** — fix or clarify `flow run --resume` await payload binding (A-58).
3. **F-016** — set parent correlation for direct `flow run` sub-agent children (A-59).
4. **F-014** — make program `--serve -m mock` behave like the CLI mock path, or reject it clearly (A-60).
5. **F-006** — handle broken pipe without panic (A-61).
6. **F-007…F-013** — align Flux-Lang docs, diagnostics, and developer CLI behavior (L-43/L-44/L-45/A-62/A-63; docs items fold into C-46).
7. **F-001, F-002, F-004, F-005** — clarify docs/provider guidance and improve planner/model robustness where feasible (C-46 for docs; A-64 for robustness).

**All 12 stories are `done` (2026-07-08).** They were implemented in the recommended order, each with
a failing-first/behavior-lock test, and the full workspace gate (build + test + clippy `-D warnings` +
fmt + `flux-codegate`) is green. Notable decisions: F-003 resolved as "(A) docs were wrong" (`--yes`
is a documented allow-all; `RiskApprover`/allow-rules are the middle ground); F-009 resolved by making
`peek` bindable (runtime enablement) rather than contorting the doc; A-59 placed in flux-flow's
execute entry (not just the CLI) so every direct caller correlates; A-64 delivered the F-004 guardrail
+ an F-002 capability-floor doc (weak-model parity stays a non-goal — F-002's precise root-cause needs
a live `gpt-4o-mini` repro).

## Retest plan (from the report — run after fixes land)

1. Re-run the exact failing rows in the beta repro matrix.
2. Re-run Flux-Lang fixtures, especially `19`, `36`, `37`, `43`, `59`, `60`, `61`–`66`.
3. Re-run A2A custom program serve: mock path, Codex path, read/no-read served agents.
4. Re-run replay: parent flow replay, child sub-agent direct replay, parent `replay --sub-agents`.
5. Re-check docs: `--yes`, A2A protocol version, Flux-Lang syntax examples, `flow run --resume` await guidance.

## Cross-references to existing epics

- **C-46 / F-005** (A2A `protocolVersion`) sits next to [`a2a-conformance`](a2a-conformance.md) /
  [A-49](../stories/A-49-agent-card-conformance-fields.md): the card already *emits*
  `protocolVersion` — the fix is aligning the public prose (which says "v1.0") to the value the
  card advertises (`0.3.0`), or bumping the card if 1.0 is intended. If the card value is wrong
  (not the docs), escalate into `a2a-conformance`.
- **A-59 / F-016** (replay correlation) sits next to the [`time-machine`](time-machine.md) epic
  ([A-45](../stories/A-45-hermetic-replay-engine.md)) and [A-08](../stories/A-08-subagent-audit-default.md)
  (the correlation contract that normal `flux run` turns already satisfy).
- **A-64 / F-002,F-004** (weak-model robustness) sits next to the stall-guards
  [A-27](../stories/A-27-identical-plan-skip-stall-guard.md)/[A-28](../stories/A-28-read-coverage-stall-guard.md)
  and the parse-resilience epic ([`parse-resilience`](parse-resilience.md)).

## Non-goals

- No redesign — the report is explicit that targeted alignment/parity work suffices.
- Not chasing weak-model parity as a hard guarantee (A-64): the aim is a guardrail + honest guidance,
  not making every low-capability model pass the planner contract.
