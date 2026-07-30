---
id: C-226
title: "A failed turn is indistinguishable from a successful one to every machine consumer"
pillar: Core
status: done
priority: 17
epic: unattended-run-integrity
design: docs/designs/unattended-run-integrity.md
note: "detect_intent/explore convert a provider Err into an Ok value tagged kind=error (loop_host.rs:563-586) — so a turn that never ran exits 0, emits no NDJSON `error` line, and reports the failure as prose inside `turn_end.answer`"
---

# A failed turn is indistinguishable from a successful one to every machine consumer

## Goal
A caller driving `flux` as a subprocess — CI, an editor extension, a coordinator fanning work out to
sub-agents — must be able to tell "the turn completed" from "the turn died on a provider error"
**without parsing prose**. Today it cannot, on any channel: not the exit code, not the NDJSON
protocol, not stderr.

The information already exists. `LoopHost::detect_intent` and `LoopHost::explore`
(`crates/flux-flow/src/loop_host.rs:563-586`) already tag the failure `"kind": "error"` — and then
return it as `Ok`:

```rust
match run.result {
    Ok(value) => Ok(value),
    Err(error) => Ok(json!({
        "kind": "error",
        "text": format!("Exploration failed: {error}"),
    })),
}
```

Because the `Err` never propagates, `run_turn` returns `Ok`, the CLI exits 0, and
`StreamJsonSink::turn_end` (`crates/flux-cli/src/stream_json.rs:257`) emits an ordinary `turn_end`
whose `answer` happens to be an apology. The NDJSON `error` line — whose documented source is
"`run_turn`'s `Err(_)`" — can never fire for this class at all. This is the narrowness the protocol
design already flagged under "A note on `error`'s narrowness"; this story closes it at the source
rather than documenting around it.

Like C-217, this is a surfacing story, not a computing story: the failure is already classified
internally and is being discarded on the way out.

## Reproducer (deterministic, no flaky provider needed)
```console
$ OPENROUTER_API_KEY=sk-or-v1-bogus flux run --yes --stream-json \
    -m openrouter/anthropic/claude-haiku-4.5 "say hi"; echo "EXIT=$?"
{"type":"turn_start","v":1,"session":"s_1627","model":"anthropic/claude-haiku-4.5","input":"say hi"}
{"type":"turn_end","v":1,"session":"s_1627","answer":"Intent detection failed: api error (status 401): {\"error\":{\"message\":\"User not found.\",\"code\":401}}","usage":null,"cost_usd":null}
EXIT=0
```
Nothing on stderr. A consumer that trusts `type == "turn_end"` records this as a completed turn.

## Acceptance
- [x] A stage failure in `detect_intent` / `explore` propagates as a **typed outcome** rather than
      being laundered into `Ok`. The `"kind": "error"` payload at
      `crates/flux-flow/src/loop_host.rs:563-586` is the existing signal — carry it, don't recompute
      it. Keep the apologetic answer text: the *human* surface must not regress into a raw stack.
- [x] `turn_end` carries a machine-readable outcome (e.g. `"outcome": "ok" | "error"`, with the
      error detail alongside). **`usage: null` + `cost_usd: null` is not an acceptable substitute** —
      it is a coincidence of the failure path, not a contract, and a consumer keying on it will
      silently misclassify the first failure that happens to have partial usage.
- [x] The NDJSON `error` line fires for a provider/flow-level turn failure, so the documented
      vocabulary matches observable behaviour. If `error` stays reserved for `run_turn`'s `Err(_)`,
      say so in the design doc and make `turn_end.outcome` the sole contract — but the two must not
      disagree.
- [x] **`flux run` exits non-zero when the turn failed.** This is the signal every subprocess driver
      reaches for first, and today it is actively misleading.
- [x] **Failing-first test**: drive a turn whose model stage errors (the bogus-credential path above,
      or a stub provider that returns `Err`) and assert (a) non-zero exit and (b) a typed error
      outcome on the NDJSON stream. Both assertions fail today — the run exits 0 and the stream shows
      a clean `turn_end`.
- [x] Protocol version handling is decided explicitly: `v: 1` is documented as unstable and the tag
      set as open/additive, so adding a field is in-contract — state in the design doc whether this
      lands under `v: 1` or bumps it.
- [x] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — found while driving `flux run` headlessly as a sub-agent implementor from a
  coordinator. Four consecutive runs died on provider errors (`stream closed before completion`,
  then an upstream rate-limit) and **every one returned exit 0 with nothing committed**; the
  coordinator only detected the failures by diffing the git branch afterwards. The 401 reproducer
  above was then constructed to pin the same behaviour deterministically.
- 2026-07-30 — the adaptive host now carries its existing tagged stage-failure value through normal
  turn finalization, preserving the authored human answer and valid provider history while returning
  `Err` and recording the durable turn outcome as `error`. NDJSON v1 additively gains
  `turn_end.outcome` plus optional `error`; a failure emits a dedicated `error` record followed by a
  final, agreeing `turn_end`, and the CLI exits non-zero. A deterministic mock-provider failure test
  covers the real binary, while the engine regression repeats a failed turn and validates session
  shape after each attempt. Verification: `cargo test -p codewandler-flux-flow` (227 passed),
  `cargo test -p flux-cli` (249 unit tests plus all integration suites passed),
  `cargo clippy -p codewandler-flux-flow -p flux-cli --all-targets -- -D warnings`,
  `cargo test -p flux-codegate` (17 passed), and `cargo fmt --all -- --check`.

## Notes
- Seams: `crates/flux-flow/src/loop_host.rs:563-586` (`detect_intent`, `explore` — the swallow);
  `crates/flux-flow/src/engine.rs:879` (`turn_terminal`);
  `crates/flux-cli/src/stream_json.rs:257` (`turn_end` emitter) and `:275` (`run_stream_json`).
- ⚠ **Do not regress the session-shape invariant.** A new turn-termination path is exactly the class
  AGENTS.md names as having recurred three times: whatever propagates must still leave the log free
  of an empty assistant message, a split `tool_use`/`tool_result` pair, or user-after-user. The mock
  provider will not catch a regression here.
- The **only** signal that works today is out-of-band: "did the agent commit anything?". Any
  automation driving flux has to be written that way until this lands — see [C-227](C-227-no-automatic-resume-on-transport-class-provider-failure.md),
  which is what makes the failure frequent enough to matter.
- Consumer-side note: `usage`/`cost_usd` being `null` on failure also means a driver cannot account
  spend for failed turns. Worth confirming that is intended.
