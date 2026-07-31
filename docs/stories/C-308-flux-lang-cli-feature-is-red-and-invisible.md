---
id: C-308
title: "A red test hides from the workspace gate: `flux-lang --features cli` fails on main"
pillar: Core
status: done
priority: 6
areas: [flux-lang, ci]
note: "L-96 made `confirm \"y\", risk: high` VALID canonical syntax, so a test using it as malformed input now parses and its expect_err panics. Nothing in the workspace enables flux-lang's `cli` feature, so `cargo test --workspace` is green while the dev loop documented in crates/flux-lang/AGENTS.md is red"
---

# A red test hides from the workspace gate: `flux-lang --features cli` fails on main

## Goal

Make the failure visible, then fix it — in that order, because the visibility gap is the more
expensive half.

## The defect

`crates/flux-lang/src/bin/fluxlang.rs` → `tests::rail_reports_the_existing_parser_diagnostics` uses
`confirm "y", risk: high` as its *malformed* input. **L-96 made that spelling valid canonical
syntax**, so the source now parses cleanly and the test's `expect_err` panics.

Confirmed on `main` at the time of filing:

```
$ cargo test -p codewandler-flux-lang --features cli --bin fluxlang
test tests::rail_reports_the_existing_parser_diagnostics ... FAILED
test result: FAILED. 10 passed; 1 failed
```

**The fix itself is one line** — pick input that is still malformed. That is not what this story is
about.

## Why this is worth a story rather than a drive-by

`cargo test --workspace` is **green**, because nothing in the workspace enables `flux-lang`'s `cli`
feature. But `crates/flux-lang/AGENTS.md` documents a dev loop that *does* pass `--features cli`. So
the project's own documented command has been red while every gate — local and CI — reported green.

That is the interesting failure: **a feature-gated test target that no gate exercises is a test that
does not exist**, and it decays silently the moment the language moves under it. This one was found
only because an unrelated implementor happened to run the documented command.

## Acceptance

- [x] `rail_reports_the_existing_parser_diagnostics` passes, using input that is genuinely malformed
      under **current** canonical syntax. Do not weaken the assertion to make it pass — the test
      exists to prove the rail surfaces parser diagnostics, so it must still prove that.
- [x] **The gate sees this target.** Either the root gate grows a `--features cli` leg, or CI does, or
      the feature is removed as a concept. Whichever is chosen, a failure in this target must red a
      run that someone actually watches. State the choice and why.
- [x] **Audit for siblings.** Enumerate every feature-gated test target in both workspaces and report
      which are exercised by a gate and which are not. `--no-default-features` and
      `--features postgres` are already run for some crates; this is asking for the complete picture,
      not a spot check. Anything unexercised is in the same class as this bug and should be listed
      even if not fixed here.
- [x] Full gate green, plus the newly-covered leg.

## Notes

- Found 2026-07-31 by L-97's implementor while working in `flux-lang`, and independently reproduced
  before filing. It is genuinely pre-existing — it reproduces at `c5c69fed` with none of L-97's code.
- ⚠ Related class, worth keeping in mind while auditing: `AGENTS.md` already records that this
  machine has `bwrap` where CI runners do not, so a sandbox test can pass locally and red CI. That is
  the same shape of defect — an environment or feature dimension the default gate does not cover —
  approached from the other direction.
- Related: [L-96](L-96-canonical-named-option-headers.md) made the option-header spelling canonical
  and is what invalidated the fixture; [L-97](L-97-flux-glyph-notation.md) found it.

## Progress

### The fix (the small half)

`crates/flux-lang/src/bin/fluxlang.rs:256` — the fixture is now `flow x\n  confirm "y\n`, an
**unterminated string literal**. That is a deliberate change of kind, not just of spelling: the old
fixture was "a statement the parser happens to reject today", and the vocabulary of valid statements
grows, so L-96 invalidated it without touching the test. A lexical impossibility does not become
valid syntax. The assertions are unchanged — `rail_src` and `compile_src` must still both error,
their diagnostics must still be byte-identical, and the text must still contain `parse error`.
Verified against today's parser before adopting: `fluxlang rail` reports
`parse error: line 2: unterminated string: missing closing "`.

### The choice: CI grows the leg, via a script that also refuses to let the hole reopen

**CI, not the local dev loop.** The local loop is not the thing that failed — `crates/flux-lang/AGENTS.md`
already documented `--features cli`, and a human running it is exactly how this was found. What was
missing is a run *somebody watches*: the leg is now a step in ci.yml's `check` job (the warm,
always-watched one, so it costs no extra runner and no extra cache).

Removing the `cli` feature was rejected: it exists so library consumers of the published L0 crate
don't pull `clap`, which is a real constraint, and deleting a feature to fix a gate hole would leave
the other eight holes below untouched.

The step runs **`scripts/check-feature-gated-tests.sh`** rather than a bare `cargo test --features
cli`, because a one-crate leg fixes one instance of a recurring class. The script carries a ledger of
every `(package, feature)` pair in **both** workspaces with a disposition (`run` / `covered` /
`elsewhere` / `skip`), runs the `run` legs, and **fails when a feature exists with no disposition** —
so the next feature added without a gate is a named CI failure instead of a silent decay. Proven:
deleting one ledger line exits 1 with `error: feature(s) with no disposition`.

### The audit — every feature-gated target in both workspaces

Method (not folded from memory): `cargo test --workspace -- --list` gives the names the default gate
actually runs; `cargo test -p <pkg> --features <f> -- --list` gives the names that configuration
runs; `comm -23` of the two is exactly the set of tests the feature hides. The recipe is in the
script's header so any claim below is re-checkable.

**Nested `plugins/` workspace: no feature-gated targets at all.** All 21 packages declare zero
features, so `cargo test --workspace` there covers everything. Nothing to fix; the script asserts it
stays that way.

**Root workspace** — "hidden" = tests reachable only with the feature:

| package | feature | hidden | before this story | now |
| --- | --- | ---: | --- | --- |
| `codewandler-flux-providers` | `realtime` | 17 | **no gate** | run |
| `codewandler-flux-lang` | `cli` | 8 | **no gate** ← this defect | run |
| `codewandler-flux-events` | `otel` | 6 | **no gate** | run |
| `codewandler-flux-a2a` | `utoipa` | 4 | **no gate** | run |
| `codewandler-flux-capabilities` | `embeddings` | 4 | **no gate** | run |
| `codewandler-flux-sdk` | `providers` | 2 | **no gate** | run |
| `codewandler-flux-sdk` | `pricing` | 1 | **no gate** | run |
| `codewandler-flux-capabilities` | `sqlite-vec` | 1 | **no gate** | run |
| `codewandler-flux-sdk` | `plugins` | 4 | **no gate, and RED** | quarantined — see below |
| `codewandler-flux-capabilities` | `local-embeddings` | 0 | **no gate** (code never compiled) | skip: fastembed/ONNX |
| `codewandler-flux-events` | `postgres` | 46 | `postgres` CI job | unchanged |
| `codewandler-flux-capabilities` | `postgres` | 9 | `postgres` CI job | unchanged |
| `codewandler-flux-tools` | `png` | 0 | covered — flux-cli default | unchanged |
| `codewandler-flux-markdown` | `ratatui` | 0 | covered — flux-tui | unchanged |
| `codewandler-flux-markdown` | `terminal` | 0 | covered — flux-cli | unchanged |
| `codewandler-flux-sdk` | `test-kit` | 0 | covered — flux-cli dev-dep | unchanged |
| `flux-auth` | `introspect` | 0 | covered — flux-cli → flux-server | unchanged |
| `flux-server` | `introspect` | 0 | covered — flux-cli | unchanged |
| `codewandler-flux-plugin` | `host` `hooks` `pack` | 0 | covered — pinned on in the root manifest | unchanged |
| `codewandler-flux-events` / `codewandler-flux-flow` | `sqlite` | 0 | covered — default-on | unchanged |
| `flux-channels` | `slack` | 0 | covered — default-on | unchanged |
| `flux-cli` | `slack` `png` | 0 | covered — default-on | unchanged |
| `flux-cli` | `embeddings` | 0 | **no gate** | skip: pure passthrough, gates no test |

Net: **nine** feature-gated configurations hid tests from every gate, not one. Eight are now run;
the ninth is quarantined because it is red.

### Two corrections to the story's premises

- **No gate runs `--no-default-features` for any crate.** The story assumed one existed. Verified by
  grep across `.github/`, `scripts/` and `Taskfile.yml`: the only feature flags any gate passes are
  the two `--features postgres` lines in ci.yml's `postgres` job. Turning defaults *off* removes
  tests rather than adding any (checked for flux-events, flux-flow, flux-channels, flux-cli — all
  zero hidden), so no *test* hides there. What is genuinely unverified is the driver-free **build**:
  nothing compiles `flux-events`/`flux-flow` without `sqlite`, even though C-274 created that
  configuration on purpose. The `portable-wasm` job does not cover it — it builds `flux-lang`'s
  example only, and flux-lang does not depend on flux-flow.
- **`codewandler-flux-sdk --features plugins` is a second live instance of this exact bug.** Both
  tests in `crates/flux-sdk/tests/plugins.rs` fail on the merge base with
  `invalid authority contract for 'fixture.upper' from 'plugin:fixture': tool 'fixture.upper'
  declares a process effect without process access` — an authority-contract tightening the fixture
  never followed, unnoticed because nothing compiled it. Not fixed here: it is a different crate, a
  different defect, and it sits on the safety envelope's authority contract, so it wants its own
  story rather than a drive-by inside C-308. It is `skip`-with-a-reason in the ledger, printed on
  every run so it cannot be forgotten.
