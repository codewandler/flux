# Unobserved wiring — the pin census

**Story:** [C-328](../stories/C-328-pin-census-wiring-declares-its-test.md) (Half A, the static
census) · **Follow-ons:** C-329 (the dynamic runner), C-330 (widen the predicate), C-331
(compile-time destructure anchor)

## The debt

Nineteen stories have found a production wiring line that is **correct** and that **no test
observes** — found only by deleting the line and seeing nothing change colour.

- **C-305**: deleting two `flux-tui` wiring lines left **474 tests green** while no model pane could
  ever reach a terminal.
- **C-314**: deleting **both** `[limits]` wirings — `crates/flux-cli/src/lab_cmd.rs`'s
  `.resource_limits(cli_resource_limits(&cfg))` and `crates/flux-cli/src/review.rs`'s
  `.resource_limits(resource_limits)` — left the **entire `flux-cli` suite green**.

The bug is not the debt. The debt is that **each instance was answered by authoring a new bespoke
guard**: there are ~10 now, each with its own mechanism, its own scanner and its own anti-vacuity
proof. Guard #11 was always someone's next story.

C-328 introduces one mechanism instead:

> A wiring line declares, in-source, the test that dies without it. A census proves the declaration
> exists and resolves.

## The predicate

`flux_codegate::pin_seams(src) -> Vec<Seam>` finds, in **production** Rust only:

> a call to a [pinned setter](#the-pinned-setter-set) on a method chain whose receiver bottoms out
> at `flux_sdk::Client::builder` or `flux_sdk::FlowClient::builder`.

Resolution follows the precedent every other scanner in `flux-codegate` sets — `ImportAliases`
resolves plain imports, renamed imports (`use flux_sdk::Client as C;`), glob imports, module aliases
and type aliases to a canonical spelling before the match; `#[cfg(test)]` modules, functions and impl
methods are skipped structurally; comments and strings can never match because the scan is over a
`syn` tree, not text. A chain split across statements (`let b = Client::builder(); b.resource_limits(..)`)
is anchored through a local binding, mirroring `ambient_sandbox_spawns`.

Each `Seam` records a **byte span**, not a line: `span_start .. span_end` covers the leading `.`
through the closing `)`, receiver excluded. C-329's runner excises exactly that range to prove the
pinned test actually dies, and a chain link can span several lines:

```rust
.resource_limits(
    cli_resource_limits(&cfg),
)
```

A line number could not name that region, and excising a line would leave a chain that does not
parse. The span is the API contract with C-329; the `line` field exists only so the shared waiver
reader can find the comment block above and so violations read like every other gate's.

### The pinned setter set

**One entry: `resource_limits`.** This is the narrowest predicate that covers the observed defect,
and the narrowness is deliberate rather than provisional:

- Widening to "every configuration call in an SDK builder chain" would flag **twelve** call sites in
  the shipped tree (`.model`, `.auto_approve`, `.storage`, `.build` on three chains). Eleven of them
  are observed by any end-to-end test of the surface — `.model` regressions surface immediately —
  so the census would be mostly noise, and a census that is mostly noise is a census nobody reads.
- `resource_limits` is the one where the failure is **silent**: a client built without ceilings runs
  correctly, produces correct output, and is unbounded. Nothing reds. That is precisely the class
  C-314 is.

C-330 widens the predicate deliberately, seam family by seam family, each with its own evidence.

## The declaration

Pins and exemptions are read by the **existing** waiver reader, `allow_reason(source, line, marker)`
— the same function `flux-allow-direct-io:` and `flux-allow-ambient-sandbox:` use. No second waiver
reader was written. It already enforces that a bare marker with no text is not a waiver, and that
the comment block must be contiguous and immediately above the call.

```rust
// C-307: `flux record` runs a real, live turn, so the operator's `[limits]` ceilings apply to it
// exactly as they do to `flux run`.
// flux-pin: record_client_carries_the_configured_ceiling_to_its_executor
.resource_limits(resource_limits)
```

- `// flux-pin: <test_name> [prose]` — the first whitespace-delimited token is the test function
  name; anything after it is prose for the reader.
- `// flux-pin-exempt: <why>` — a seam deliberately left unobserved.

The name is resolved against every `#[test]`-shaped function in both Cargo workspaces:
`workspace_test_files` plus the `#[cfg(test)]` modules `workspace_source_files` sees.
**A pin naming a nonexistent test reds** — that anti-drift half is free, and it is what keeps a pin
from rotting into a comment after the test it names is renamed.

### Why a marker and not a `const ALLOW` table

The ~10 existing guards key their allowances on a `const ALLOW: &[(&str, &str, &str)]` table because
what they classify has a **stable structural identity** — a module, a seam name, a source.

A pin keys on a **call site**, which has no stable identity but position. A `(file, line)` const
rots on every edit above it: insert a comment forty lines up and every entry below is silently
wrong, pointing at a different line that may well still be a seam. `catalog_coherence` already
avoids this by keying on `(module, seam, source)` rather than position. The marker sidesteps the
problem entirely — **it travels with the line**, through every refactor, rename and reformat, and it
is visible to the person editing the line rather than in a table three files away.

The single-use spirit of the `const ALLOW` tables survives as `MAX_PIN_EXEMPTIONS`: the census caps
the number of `flux-pin-exempt` seams (budget: 1, actual: 0), so an exemption cannot quietly become
the norm. Raising the cap is a diff under review, which is the only way "exempt" stays a decision
rather than a habit.

## Why not `cargo-mutants` — decided, with evidence

**The operator set settles it, before cost is even considered.** `cargo-mutants`' operators are
*function-body replacement* and *binary-operator swaps*. It does **not** delete statements, and it
does **not** drop a call from a method chain.

Both C-314 sites are builder chains inside functions returning non-`Default` types
(`Result<flux_sdk::Client>`, `Result<flux_sdk::FlowClient>`), so the only mutant available at either
site is an unviable whole-body replacement, which cargo-mutants discards as unbuildable. **It would
not have caught C-314 given infinite time.** No configuration, budget or runtime changes that; the
mutation it would need is not in the tool.

Cost is the second argument and only reinforces the first: a full run is ~10–20k mutants across 38
crates against a 6 h runner ceiling.

It remains plausible for a *different* debt — untested branches in the pure L0 crates — scoped and
nightly. That is not this story, and it is not this mechanism.

## Why no existing guard is subsumed

The ~10 bespoke guards ask **"does this exist / is it classified?"**. The census asks **"does a test
observe it?"**. These are orthogonal questions; neither implies the other, and nothing here replaces
anything there.

Two guards in particular **must not** be folded into this mechanism, because they are strictly
stronger than anything test-based can be:

- `capability_widenings` — `crates/flux-plugin/src/host/refresh.rs:403`
- `pin_granted_authority` — `crates/flux-plugin/src/host/refresh.rs:343`

Both fail at **compile time**, via exhaustive destructure. A compile-time failure cannot be skipped,
cannot be flaky, and cannot be green on one machine and red on another. The design pressure runs the
*other* way: whenever an invariant is "a field set is classified", prefer an exhaustive destructure
over a test — see C-331, which does exactly that for `Config`/`Limits`.

## What this deliberately does not cover

Stated here rather than discovered later:

- **[C-313](../stories/C-313-url-encoder-consolidation-and-key-pinning.md)** — an ordinary
  expression, not a builder seam. The predicate anchors on a builder chain and cannot see it.
- **[C-324](../stories/C-324-pane-queue-overflow-is-a-silent-success.md)** — a *missing* signal, not
  an unobserved one. The census reasons about lines that exist; a line that was never written has no
  seam to pin. (This is also why `lab_cmd.rs`'s `offline_client` needs no exemption: it wires no
  ceilings **deliberately**, so there is nothing to observe.)
- **Wiring expressed as data** rather than as a call site — a config table, a registry entry, a
  const array. No call, no seam.
- **The semantics of a pinned test.** A pin proves the named test exists; C-329's runner will prove
  it *dies* when the span is excised. Neither proves it dies **for the right reason** — a test that
  reds because the client failed to build at all satisfies both.

**This is a coverage floor, not a proof. The reviewer still reads the test.**

## What C-328 shipped

| | |
|---|---|
| Scanner | `flux_codegate::pin_seams`, `flux_codegate::test_function_names` |
| Census | `every_sdk_client_wiring_seam_pins_a_test_that_observes_it` (runs under `cargo test -p flux-codegate`, named in CI and in `AGENTS.md`'s dev loop) |
| Anti-vacuity | minimum files scanned, minimum test-name universe, minimum seams found, exemption cap |
| C-314 closed | two *independently attributable* tests — `record_client_carries_the_configured_ceiling_to_its_executor` (`lab_cmd.rs`) and `review_flow_client_bounds_tool_calls_at_the_configured_ceiling` (`review.rs`) |

Both call sites were extracted into named seams (`record_client_from`, `review_flow_client`) because
neither was reachable from a test before: the enclosing functions resolve a cwd, load config, resolve
a live provider and print a report. **That unreachability is how C-314 happened**, and it is the
shape to watch for — a builder chain with no seam between it and the process boundary is a chain no
test can observe.

The two tests observe deliberately different things, and the asymmetry is honest:
`review_flow_client`'s asserts **observed occupancy** — three `parallel` branches, one op inside
`Tool::execute` — following C-299's rule that a wiring story is only done when the wire carries
current. `record_client_from`'s stops one layer earlier, at the ceilings carried by the executor the
client dispatches through, because `flux_sdk::Client` has no post-build op registration and no
blocking probe can be placed in its registry. That an executor carrying those numbers enforces them
is already proven by `a_configured_limits_table_binds_for_the_cli_executor`.
