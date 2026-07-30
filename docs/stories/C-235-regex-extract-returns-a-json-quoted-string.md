---
id: C-235
title: "`regex_extract` returns a JSON-quoted string, so its output cannot be fed to another op"
pillar: Core
status: done
areas: [flux-tools]
note: "SURFACED BY the A-131 fleet smoke test: the sweep journey re-derived `runner` off the board correctly, then `fleet.status` failed with `invalid url: relative URL without a base` because the extracted value carried literal quote characters"
---

# `regex_extract` returns a JSON-quoted string, so its output cannot be fed to another op

## Goal
`regex_extract` returns its match through `serde_json::to_string(m.as_str())`
(`crates/flux-tools/src/cognition.rs:1176`), which produces a **JSON-encoded** string — the value a
Program receives includes the surrounding quote characters. Interpolating it shows the quotes:

```
$runner = regex_extract({ s: $item, pattern: "runner: (.+)", group: 1 })
send({ channel: "cli", message: fmt("RAW=[{runner}]") })
    ->  RAW=["http://127.0.0.1:9101"]
```

So the value is unusable as an argument to any op that parses its input. Discovered when the
fleet-coordinator sweep journey re-derived a worker address off the board correctly and then died:

```
error: deliver `user_input`: runtime error: step `fleet.status` failed:
       invalid url: relative URL without a base
```

The extraction was right; the encoding made it unusable. There is also **no escape inside
Flux-Lang**: every cognition op encodes its output the same way, so a second `regex_extract` to strip
the quotes returns a freshly quoted string, and no `trim`/`replace`/`json_parse` op exists.

This is what stopped the fleet smoke test from proving its last link in a single journey — the
re-derivation and the dial both work, but they cannot be chained. It is a general defect, not a fleet
one: any Program that extracts a substring and passes it onward hits it.

## Acceptance
- [ ] A string-returning cognition op yields the **string**, not its JSON encoding, so its output is
      directly usable as another op's argument.
      **Failing-first test**: `regex_extract` on `runner: http://h:1` with `group: 1` produces a value
      equal to `http://h:1` — today it equals `"http://h:1"` including quotes.
- [ ] Decide and record whether this is a bug in `regex_extract` alone or in how the flow engine
      interprets a `ToolResult` payload — check `regex_match`, `split` and the other list/string ops
      for the same shape before fixing one in isolation. If the engine is meant to JSON-parse op
      output, the bug is there and the fix is uniform; if not, every string-returning op owes the
      same change. **Say which, with evidence.**
- [ ] A chained journey works end to end: extract a URL from text, pass it to an op that parses a
      URL, and succeed. This is the shape that failed above.
- [ ] Existing consumers do not silently change meaning — anything that currently compensates by
      expecting quotes must be found and fixed in the same change, or the fix is a breaking one and
      is called that.
- [ ] Standard gate green in both workspaces.

## Progress
- 2026-07-29 — filed from the A-131 fleet smoke test. The proof is quoted above, from a real run.

## Notes
- Once fixed, the fleet sweep journey in a reference coordinator Program (A-117) can re-derive
  `runner`/`task_id` off the board and dial the worker **in one journey**. Until then a Program must
  hardcode the worker address, which weakens exactly the claim the sweep exists to demonstrate.
- The equivalent chain is already proven at the Rust level by
  `a_restarted_coordinator_rederives_every_dispatch_from_the_board_alone`
  (`crates/flux-sdk/tests/fleet_board_recovery.rs`), which reads `runner` and `task_id` off the board
  and really dials a loopback worker. So the *capability* is sound; this is a Flux-Lang plumbing gap.
- Related: board ops render human text with no `output_schema`, which is why a regex is needed at all
  rather than `$item.runner`. That is its own gap — a Program reasoning about a board should not be
  text-scraping it.
