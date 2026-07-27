---
title: Agent Lab
description: "Test, tune, and crash-proof an embedded agent against its own recorded runs: golden scenarios in cargo test, counterfactual what-ifs, and transparent resurrection."
---

# Deterministic Agent Lab — test, tune, resurrect

Because a flux run is a deterministic artifact — every accepted plan is canonical, re-parseable
Flux-Lang text and every leaf-op result is a redacted cassette cell — flux is the only agent SDK
where your agent is **testable**, **tunable against a frozen world**, and **crash-proof**. The
Agent Lab is three doors on that one substrate:

| Door | Surface | What it buys you |
|---|---|---|
| **Test** | `flux_sdk::test::Scenario` (feature `test-kit`) | Record a run once, commit it, re-run the *real* agent offline in `cargo test` — $0, no key, no flakes |
| **Tune** | `Session::what_if()` / `Client::what_if_over()` | Re-run a recorded session under exactly one changed variable, the rest of the world byte-frozen |
| **Resurrect** | `Session::resurrect()` / `ClientBuilder::auto_resurrect` | Finish a crashed turn with zero model re-spend and no duplicate side effects |

All three build on the same recordings that power the [Time Machine](../agent/time-machine.md)
verbs (`flux replay` / `fork` / `diff`) — the Lab doesn't replace them, it turns them into a
product for embedders.

## Test — golden scenarios in `cargo test`

Every other agent SDK forces a choice between calling the live model in tests (nondeterministic,
costs money, flaky) or hand-mocking it (you test your mocks, not your agent). flux persists the
canonical plan and a redacted op cassette, so it alone can re-run the real agent offline and assert
on *how it reasoned*.

The test kit lives behind a default-off `test-kit` cargo feature, so the default build stays
dependency-free:

```bash
cargo add --dev codewandler-flux-sdk --features test-kit
```

**Record once**, against a live provider — this is the only step that spends money:

```rust
use flux_sdk::test::Scenario;

// Writes the fixture directory tests/scenarios/triage/ — commit it.
Scenario::record(&client, "triage the failing build", "tests/scenarios/triage").await?;
```

**Replay forever**, offline. `replay` re-executes the recorded plans hermetically: no model call,
no live IO, side effects never re-fire. It is safe — and recommended — to run it under a deny-all
approver and a provider that panics if called, which proves your test never touches the network:

```rust
use flux_provider::NullProvider;
use flux_sdk::{Client, test::Scenario};

#[tokio::test]
async fn triage_agent_stays_on_rails() -> anyhow::Result<()> {
    // No auto_approve: the default approver denies everything. The provider is never called.
    let client = Client::builder().build(NullProvider, ".")?;

    let scenario = Scenario::load("tests/scenarios/triage")?;
    let outcome = scenario.replay(&client).await?;

    outcome.assert_faithful();                     // zero divergence from the recording
    outcome.assert_plan_snapshot();                // canonical Flux-Lang vs plan.flux.snap
    outcome.assert_calls(&["fs.read", "shell.exec"]);
    outcome.assert_never_calls("fs.write");        // a safety property, not a transcript grep
    outcome.assert_text_contains("root cause");
    outcome.assert_cost_under(0.01);               // replay is ~$0 by construction
    Ok(())
}
```

`replay` takes the `&Client` because hermetic re-execution still needs the client's op catalog —
and because the deny-all/never-called posture above *is* client configuration.

A failing assertion doesn't dump an opaque transcript: it renders the canonical plan source plus a
world diff, so the failure message tells you whether the *reasoning* changed or the *world* did.

`assert_never_calls` is the headline: "my agent never runs `shell.exec`" becomes a committed,
offline regression test on the plan itself.

### Fault injection

`inject_at` forks the recorded run at a node, injects an error as that op's result, and re-executes
the tail so you can assert your compensation logic actually fires:

```rust
let cf = scenario
    .inject_at(&client, 3, &serde_json::json!({"error": "disk full"}))
    .await?;
cf.assert_compensated_with("notify.send");  // the saga cleanup ran
cf.assert_diverges_at(3);                   // and nothing before the fault changed
```

### `check` — did my change alter the agent's reasoning?

Where `replay` is the faithful CI guard, `check` re-drives the **real agent loop** — your client's
*current* prompt, model, and tools — against the frozen recorded world, then diffs the result
against the golden:

```rust
let report = scenario.check(&client).await?;
assert!(!report.plan_changed, "this config change altered the agent's plan");
assert!(!report.left_world, "the re-run stepped off the recorded world");
```

Recorded model calls are served from the fixture's model cassette (deterministic, $0); only a
genuinely new model request falls through to your provider, and the report counts both
(`model_served` / `model_live`). A `plan_changed` report is a reasoning regression; `left_world`
means an op read something the recording can't explain — a world regression.

### Re-baselining

When a change is intentional, re-baseline the same way you'd use `cargo insta`, except no extra
tool is needed:

```bash
FLUX_GOLDEN=update cargo test --features test-kit
```

`FLUX_GOLDEN=update` rewrites `plan.flux.snap` from the new run, and lets `Scenario::record`
overwrite an existing fixture (recording against a live client). Without it, snapshots fail loudly
and `record` refuses to clobber a committed golden.

## The fixture format

A scenario is a plain directory — `tests/scenarios/<name>/`:

| File | Contents |
|---|---|
| `events.db` | The recorded event stream — plans, cassette cells, turn structure |
| `flow.db` | Companion flow store (empty), making the directory a valid `Storage::dir` |
| `model.jsonl` | The model cassette: one line per model call, keyed by a hash of the canonical **redacted** request, with the recorded response chunks |
| `plan.flux.snap` | Canonical Flux-Lang of every accepted plan — the snapshot baseline |
| `scenario.toml` | Manifest: scenario name, flux version, recording time, input, model, cassette cap, redaction marker — drift diagnostics and `check`'s re-drive input |

Because it is a real `Storage::dir` store, the same directory opens in the
[Time Machine](../agent/time-machine.md): `flux replay`, `flux fork`, and `flux diff` all work on a
committed fixture.

**Redacted by construction.** Cassette cells and stored plan text pass through the same redactor
as the rest of the durable event log before anything hits disk, and model cassette entries are
redacted *before* hashing. Secrets are never stored, so fixtures are safe to `git commit` with no
extra scrubbing step.

**Truncated cells fail honestly.** An op output over the `FLUX_CASSETTE_MAX_BYTES` cap (default
1 MiB) is stored truncated, and replay refuses to serve it as if it were the whole world. The kit
surfaces this as an actionable error — *re-record with a larger `FLUX_CASSETTE_MAX_BYTES`* — and
`assert_faithful` treats it as a diagnostic, never a flaky pass.

Scenarios are single-turn in v1; `record` errors if the recorded turn ends suspended.

## Tune — counterfactual what-ifs

"Should I ship this model/prompt/policy change?" is usually answered by comparing two noisy live
samples against ever-moving APIs. `Session::what_if()` instead re-runs a recorded session under
**exactly one changed variable** with everything else byte-frozen, so the diff is a pure causal
readout:

```rust
use flux_sdk::whatif::OffTape;

// Pure substitution: swap one recorded op output, no model call at all.
let cf = session
    .what_if()
    .turn(2)
    .substitute("http.request", serde_json::json!({"status": 503}))
    .run()
    .await?;
assert!(cf.hermetic());              // stayed entirely on tape — the diff is complete
let diff = cf.diff()?;               // exactly the rows your substitution caused

// Re-plan the same turn under a different model, against the same frozen world.
let cf = session
    .what_if()
    .model("anthropic/haiku")
    .off_tape(OffTape::Halt)         // hermetic: halt rather than touch the live world
    .run()
    .await?;
if !cf.hermetic() {
    println!("first divergence: {:?}", cf.first_divergence());
}
```

The builder's variables: `.turn(n)` picks the turn, `.model(..)` and `.system_prompt(..)` re-plan
under a different model or prompt, `.substitute(op, output)` / `.substitute_at(node, output)` swap
recorded outputs without any model call, and `.off_tape(Halt | Live)` chooses what happens when the
re-run needs something the tape doesn't have — halt hermetically, or bridge to real IO through the
full live envelope (real approver included).

`.policy(perms)` adds the policy variable: re-run the recorded plan against the frozen world but
re-decide every dispatch under a different permission set — the "would the tightened policy have
blocked that?" gate. The rules replace the recording's wholesale rather than merging with them (the
question is about the policy as given). An op the new rules refuse records the envelope's **real
refusal** and halts the plan as a denial instead of being handed the taped output, and the run is
reported non-hermetic — it left the recorded world, which is the answer you asked for. An equally
permissive policy changes nothing and stays on tape. No model call either way.

The result is a `Counterfactual`: `session()` (the counterfactual run is itself a real session —
replayable, forkable, diffable), `diff()` against the original, `first_divergence()`, `hermetic()`,
and `cost()`.

### The honesty contract

Hermeticity is reported, never faked. A pure `substitute` is fully offline and produces a
*complete* diff. A `model`/`system_prompt` variant is hermetic only up to the point where the
re-plan reads a different input — that miss *is* the causal boundary, and the API says so:
`hermetic()` returns `false` and `first_divergence()` localizes it. The Lab never fabricates a
complete diff past the point where the frozen world stopped explaining the run.

### Sweeps

`Client::what_if_over` runs one change across a corpus of recorded sessions and aggregates:

```rust
// A spec is a captured builder — build one from any session's `what_if()`, then apply it
// to the whole corpus. (`WhatIfSpec` is also `Default` + `#[non_exhaustive]`, so you can
// construct one field-wise: `WhatIfSpec { model: Some(..), ..Default::default() }`.)
let spec = client.open_session(&any_id)?.what_if().model("anthropic/haiku").spec();
let report = client.what_if_over(session_ids, spec).await?;
// report.outcomes — one row per session (a failing session is an error row, not a sweep abort)
// report.changed / report.total — how much of the corpus diverged
// report.offline_cost — what the sweep actually spent
```

## Resurrect — transparent crash recovery

Long-running agents get OOM-killed and redeployed mid-task. An LLM-as-runtime SDK re-calls the
model on restart (re-spend, and a *different* plan) and re-fires side effects. flux has the plan as
durable source, every completed op in the cassette, and a deterministic substrate — so an
interrupted turn can simply be **finished**:

```rust
let client = Client::builder()
    .storage(Storage::dir("./state"))
    .auto_resurrect(true)   // the default for Storage::dir
    .build(provider, ".")?;

let session = client.open_session(&id)?;

// A later turn on a session with an interrupted predecessor resumes that
// predecessor first — reported through TurnOutput::resurrected, never silent.
let out = session.send("continue").await?;
if let Some(report) = out.resurrected.as_deref() {
    println!("resurrected: {}", report.outcome);
}
```

Or drive it explicitly:

```rust
if let Some(turn) = session.interrupted()? {
    println!("turn {} died mid-execution", turn.turn_id);
    let report = session.resurrect(&mut sink).await?.expect("open turn");
    println!(
        "fast-forwarded {} statements, {} ops from cassette, {} run live",
        report.statements_fast_forwarded,
        report.ops_served_from_cassette,
        report.ops_run_live,
    );
}
```

Resurrection re-parses the interrupted turn's accepted plan (no model call — the plan is durable
source), fast-forwards every completed statement, serves every op with a recorded cassette cell
from tape (its side effect does **not** re-fire), and runs only the crash tail live — through the
real approval envelope, exactly like any live run. If a served cell's re-derived input hash doesn't
match, resurrection stops with a loud divergence error rather than improvising.

### Exactly-once, honestly

The guarantee mirrors Temporal's activity semantics, and is stated rather than hand-waved:
**exactly-once for every op with a recorded cassette cell; at-least-once for an op interrupted
during dispatch** — one whose side effect fired but whose process died before the cell was
appended. That one op re-fires live on resume. Ops whose results were recorded never re-fire, and
the model is never re-invoked for a plan that was already accepted.

## The CLI surface

The same doors are exposed through the CLI (the reference app built on the SDK), **shipping with
the same release** as the SDK surfaces above:

```bash
flux record triage "triage the failing build"   # writes tests/scenarios/triage/ (live, once)
flux test                                       # replay every scenario offline: $0, no key,
                                                # non-zero exit + plan/world diff on regression
flux test triage --json                         # one scenario, machine-readable
```

`--dir <DIR>` relocates the scenarios root (default `tests/scenarios`) for both commands, and
`FLUX_GOLDEN=update flux test` re-baselines a fixture exactly as it does under `cargo test`.
`flux test` with no fixtures at all is an error, not a green gate.

Fixtures are plain `Storage::dir` directories, and the Time Machine verbs gain a `--store <DIR>`
flag to open them (and any other store directory) in place:

```bash
flux replay --store tests/scenarios/triage last
flux diff --store ./agent-state s_1 s_2
flux sessions --store ./agent-state             # interrupted turns are marked in the listing
```

`--store` is a global flag, so it may appear before or after the subcommand.

A CLI turn on a session a crash killed mid-turn finishes that turn first, printing what was
fast-forwarded, served from the cassette, and re-run live (`FLUX_AUTO_RESURRECT=0` opts out).
`flux sessions` *flags* an interrupted session rather than resurrecting it — finishing a turn runs
its live tail through the approval envelope, which must not be a side effect of asking what sessions
exist. See the [CLI reference](../agent/cli.md) for the full flag surface.

## Limits (v1)

- **Scenarios are single-turn.** Multi-turn fixtures and error-outcome substitution are explicit
  v1 non-goals.
- **Only recorded runs travel.** Sessions captured with `FLUX_CASSETTE=0` cannot be replayed,
  checked, or resurrected — the same rule as the [Time Machine](../agent/time-machine.md#limits).
- **Redactor parity matters.** Fixtures record redacted cells; the fixture pins its redaction
  marker so a drifted redactor config is a loud diagnostic, not a silent mismatch.
- **This is not an eval service.** The Lab tests *your* agent, local-first, no telemetry, no
  LLM-judged scoring — and test doubles are ordinary registered ops, not a mocking framework.

## Related docs

- [Time Machine](../agent/time-machine.md) — the `flux replay` / `fork` / `diff` verbs the Lab
  builds on, and how cassettes work.
- [Sessions & persistence](./sessions.md) — `Storage::dir`, the layout fixtures share with the CLI.
- [SDK overview](./overview.md) — the front doors, providers, and the safety envelope.
- [Durability](../language/durability.md) — the in-language markers (`await`, `checkpoint`,
  `once`, `saga`) that pair with resumable execution.
