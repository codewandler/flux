---
title: Agent Lab
description: "Test and tune an embedded agent against recorded runs, then recover durable accepted work with an explicit at-least-once boundary."
---

# Deterministic Agent Lab — test, tune, resurrect

A recorded flux run contains canonical accepted-plan text, durable usage, and redacted cassette
cells for leaf-op results. The accepted-plan terminology names the durable replay contract: on the
adaptive loop, its Flux-Lang text is host-derived replay metadata, not control flow emitted by the
model. The Agent Lab exposes three distinct uses of that substrate:

| Door | Surface | What it buys you |
|---|---|---|
| **Test** | `flux_sdk::test::Scenario` (feature `test-kit`) | Replay the recorded plan offline, or use `check` to re-drive the current agent loop against the recorded model and op world |
| **Tune** | `Session::what_if()` / `Client::what_if_over()` | Re-run a recorded session under exactly one changed variable, the rest of the world byte-frozen |
| **Resurrect** | `Session::resurrect()` / `ClientBuilder::auto_resurrect` | Finish an accepted durable plan without calling the model again; an effect can repeat if the process died before recording its result |

All three build on the same recordings that power the [Time Machine](../agent/time-machine.md)
verbs (`flux replay` / `fork` / `diff`) — the Lab doesn't replace them, it turns them into a
product for embedders.

### Replay and check are different tests

| | `Scenario::replay` | `Scenario::check` |
|---|---|---|
| Runs the current agent loop? | No — re-executes the already accepted plan | Yes — starts the current loop with the recorded input |
| Model behavior | Never calls a model | Serves matching requests from `model.jsonl`; a miss falls through to the configured provider and is counted in `model_live` |
| Operation behavior | Serves every leaf result from the recorded cassette | Pins operation dispatches to the recorded world and halts on an off-tape request |
| Best for | Plan/cassette integrity and assertions about the recorded run | Detecting whether prompt, model, tool, or loop changes alter behavior |
| Offline guarantee | Fully offline | Offline only when every model request is covered (`model_live == 0`) |

## Test — golden scenarios in `cargo test`

Replay validates the stored host-derived flow and recorded world without starting a new turn or
calling the model. Use `check` when you want to exercise today's agent loop and compare its behavior
with the golden.

The test kit lives behind a default-off `test-kit` Cargo feature. Keep the feature on the SDK entry
in `dev-dependencies`: ordinary production builds do not compile the test API or its extra `toml`
dependency, while subsequent test commands are plain `cargo test`:

```bash
cargo add --dev codewandler-flux-sdk --features test-kit
cargo add --dev codewandler-flux-provider
cargo add --dev tokio --features macros,rt-multi-thread
```

**Record once**, normally against a live provider. Recording spends model tokens; later live
`check` misses, judge updates, and `OffTape::Live` counterfactuals can spend too:

```rust
use flux_sdk::test::Scenario;

// Writes the fixture directory tests/scenarios/triage/ — commit it.
Scenario::record(&client, "triage the failing build", "tests/scenarios/triage").await?;
```

**Replay offline.** `replay` re-executes the recorded plans hermetically: no model call,
no live IO, side effects never re-fire. It is safe — and recommended — to run it under a deny-all
approver and the network-free `NullProvider`; replay never consults that provider:

```rust
use flux_provider::NullProvider;
use flux_sdk::{Client, test::Scenario};

#[tokio::test]
async fn triage_recording_stays_valid() -> Result<(), Box<dyn std::error::Error>> {
    // No auto_approve: the default approver denies everything. The provider is never called.
    let client = Client::builder().build(Box::new(NullProvider), ".")?;

    let scenario = Scenario::load("tests/scenarios/triage")?;
    let outcome = scenario.replay(&client).await?;

    outcome.assert_faithful();                     // zero divergence from the recording
    outcome.assert_plan_snapshot();                // canonical Flux-Lang vs plan.flux.snap
    outcome.assert_calls(&["read", "bash"]);
    outcome.assert_never_calls("write");           // a property of the recorded accepted plan
    outcome.assert_text_contains("root cause");
    outcome.assert_cost_under(0.05);                // prices usage captured by the recorded run
    Ok(())
}
```

`replay` takes the `&Client` because hermetic re-execution still needs the client's op catalog —
and because the deny-all/network-free posture above *is* client configuration.

The replay itself spends $0. `assert_cost_under` is deliberately different: it prices the
`CallUsage` stored by the original run, so it can enforce a production-cost budget without claiming
that replay generated those tokens.

A failing replay assertion renders the canonical plan and cassette-divergence detail. It tells you
whether the recorded artifact can still be replayed faithfully; it cannot tell you whether today's
agent loop would make the same calls and record the same host-derived flow, because replay never
drives a new turn or calls the model. Use `check` for that question.

`assert_never_calls` makes a precise statement about the committed run: for example, "this accepted
plan never called `write`." Pair it with `check` if you also need to prove that current configuration
still produces that plan.

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

Recorded model calls are served from the fixture's model cassette (deterministic, $0); a new request
falls through to your configured provider, can incur cost, and is counted in `model_live`.
`plan_changed` means the current loop produced different plan content; whether that is a regression
is your policy decision. `left_world` means the re-drive requested or observed something the frozen
operation world cannot explain. `Report::is_clean()` also requires `model_live == 0`.

### Re-baselining

When a change is intentional, re-baseline the same way you'd use `cargo insta`, except no extra
tool is needed:

```bash
FLUX_GOLDEN=update cargo test
```

For a replay assertion, `FLUX_GOLDEN=update` rewrites `plan.flux.snap` from the fixture's replayed
plan; it does not make a model call. It also lets `Scenario::record` overwrite an existing fixture,
and makes `Scenario::check` re-record the fixture through the live client. Those latter two paths can
spend money. Without the variable, snapshots fail loudly and `record` refuses to clobber a committed
golden.

### Judge assertions — grading text output

`replay`'s other assertions are exact: the plan matched, or it didn't. Some outputs don't have one
canonical answer — a summary, an explanation, an email draft — so the Test Kit's complementary axis
grades them with an LLM judge against a plain-English criterion:

```rust
let outcome = scenario.replay(&client).await?;

let rubric = flux_sdk::test::Rubric::model("anthropic/claude-haiku-4.6");
let verdict = scenario
    .judge(&client, "the answer cites the refund policy", outcome.text(), &rubric)
    .await?;
verdict.assert_pass(); // panics with the judge's own rationale on a FAIL
```

The judge's own model call is a cassette citizen, exactly like an agent model call: its canonical
(redacted) request is hashed and looked up in the fixture's `judge.jsonl` first. A hit is served
straight from disk — the judge provider is never touched, so a plain `cargo test` run costs
nothing, the same hermeticity `replay` itself proves. A miss (the first time this exact call is
made, or the judged text/criterion/model changed since the last recording) is a **hard error** —
never a silent live fall-through, and never a silent pass against a stale grade: a regressed answer
changes the hash. `FLUX_GOLDEN=update` records a fresh verdict against a live judge provider, the
same re-baseline convention as everything else in this crate:

```bash
FLUX_GOLDEN=update cargo test
```

`rubric.model` is always explicit — there is no default judge model, so no assertion can spend
without the caller naming a target for it. `Scenario::assert_judge` is the panicking one-liner
(`judge` + `Verdict::assert_pass` in one call) for tests that don't need the raw verdict.

## The fixture format

A scenario is a plain directory — `tests/scenarios/<name>/`:

| File | Contents |
|---|---|
| `events.db` | The recorded event stream — plans, cassette cells, turn structure |
| `flow.db` | Companion flow store (empty), making the directory a valid `Storage::dir` |
| `model.jsonl` | The model cassette: one line per model call, keyed by a hash of the canonical **redacted** request, with the recorded response chunks |
| `plan.flux.snap` | Canonical Flux-Lang of every accepted plan — the snapshot baseline |
| `judge.jsonl` | Only present once a scenario uses `Scenario::judge`/`assert_judge` — one committed verdict per distinct (criterion, target, judge model), same keyed-by-hash shape as `model.jsonl`, accumulated additively |
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
and `cost(&pricing)`.

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

Resurrection applies only after a plan was accepted and stored. It never calls the model again for
that accepted plan. Its side-effect guarantee depends on the durable boundary: an eligible crash-tail
op with a matching recorded cassette cell is not dispatched again, while an effect that happened
before its cell was appended can run again. A crash before the runtime stores an accepted plan has
nothing to resume and is reported as an error.

With that boundary explicit, an interrupted accepted plan can be continued:

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

Resurrection re-parses the interrupted turn's accepted plan, fast-forwards completed statements,
serves eligible ops with recorded cassette cells from tape, and runs the remaining crash tail live
through the real approval envelope. If a served cell's re-derived input hash doesn't match,
resurrection stops with a loud divergence error rather than improvising.

### The at-least-once boundary

The common crash-tail guarantee mirrors Temporal's activity semantics: **a cassette-served op is not
re-dispatched; an op interrupted during dispatch is at-least-once**. If its side effect fired but the
process died before the cell was appended, it re-fires live on resume.

There is a wider conservative case. If the turn died before completing its first top-level statement,
there is no statement boundary from which to anchor a safe cassette tail. Resurrection then runs the
unanchored window live and reports `unanchored_cells`; more than one op can repeat. The model is still
not re-invoked for the already accepted plan.

## The CLI replay surface

The CLI exposes scenario recording and hermetic plan replay. `Scenario::check`, what-if tuning, and
explicit resurrection reports remain SDK surfaces:

```bash
flux record triage "triage the failing build"   # writes tests/scenarios/triage/ (live, once)
flux test                                       # replay every scenario offline: $0, no key,
                                                # non-zero exit + plan/world diff on regression
flux test triage --json                         # one scenario, machine-readable
```

`--dir <DIR>` relocates the scenarios root (default `tests/scenarios`) for both commands.
`FLUX_GOLDEN=update flux test` rewrites `plan.flux.snap` from the same recorded plan; it does not
re-drive the current loop or re-record the fixture. To replace the full recording intentionally, run
`FLUX_GOLDEN=update flux record <name> "<prompt>"` with a live provider. `flux test` with no fixtures
at all is an error, not a green gate.

Fixtures are plain `Storage::dir` directories, and the Time Machine verbs gain a `--store <DIR>`
flag to open them (and any other store directory) in place:

```bash
flux replay --store tests/scenarios/triage last
flux diff --store ./agent-state s_1 s_2
flux sessions --store ./agent-state             # interrupted turns are marked in the listing
```

`--store` is a global flag, so it may appear before or after the subcommand.

Entering a session killed after plan acceptance — through a one-shot `flux run` turn, the
interactive REPL, or the TUI, and on the SDK side
`Session::send`/`send_with`/`stream`/`start_flow` — attempts recovery first, reporting what was
fast-forwarded, served from the cassette, and re-run live (`FLUX_AUTO_RESURRECT=0` opts out). The
same at-least-once window described above applies. `flux sessions` only *flags* an interrupted
session: finishing a turn can dispatch a live tail and must not be a side effect of listing sessions.
See the [CLI reference](../agent/cli.md) for the full flag surface.

## Limits (v1)

- **Scenarios are single-turn.** Multi-turn fixtures and error-outcome substitution are explicit
  v1 non-goals.
- **Only recorded runs travel.** Sessions captured with `FLUX_CASSETTE=0` cannot be replayed,
  checked, or resurrected — the same rule as the [Time Machine](../agent/time-machine.md#limits).
- **Redactor parity matters.** Fixtures record redacted cells; the fixture pins its redaction
  marker so a drifted redactor config is a loud diagnostic, not a silent mismatch.
- **This is not an eval service.** Fixtures and ordinary replay stay in your selected local store,
  and flux adds no telemetry. Recording, a `check` model-cassette miss, judge recording, or an
  `OffTape::Live` run still sends the configured request data to your chosen model provider or live
  operation, exactly as that explicit live action implies. There are no hosted dashboards or
  multi-model bake-offs: `Scenario::judge` is a narrow per-fixture rubric assertion, not a
  corpus-wide eval harness (the internal `flux-eval` crate covers that ground for flux's own
  benchmark loop).

## Related docs

- [Time Machine](../agent/time-machine.md) — the `flux replay` / `fork` / `diff` verbs the Lab
  builds on, and how cassettes work.
- [Sessions & persistence](./sessions.md) — `Storage::dir`, the layout fixtures share with the CLI.
- [SDK overview](./overview.md) — the front doors, providers, and the safety envelope.
- [Durability](../language/durability.md) — the in-language markers (`await`, `checkpoint`,
  `once`, `saga`) that pair with resumable execution.
