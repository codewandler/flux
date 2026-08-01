# Design: flux recipes — real programs that make the difference click

**Status:** proposed · **Pillar:** Core · **Stories:** [C-425](../stories/C-425-the-flagship-recipe-tracking-as-a-flux-app.md) · [C-426](../stories/C-426-the-determinism-proof.md) · [C-427](../stories/C-427-the-recipe-contract.md) · [C-428](../stories/C-428-the-example-coverage-census.md) · [C-429](../stories/C-429-the-recipes-surface-and-positioning.md)

## Why

Someone evaluating flux against any other agent sees a feature list and a folder of examples, and has
no way to tell that flux is a *different kind of thing*. The vision states the thesis in one line —
**the LLM is not the runtime** — but a thesis is not a demonstration. This epic builds the
demonstrations: real programs doing real work, each of which makes one guarantee visible enough that a
reader can check it themselves.

### The examples we have are language samples, not recipes

Measured across the 16 files in `examples/` (keyword sweep, 2026-08-01):

| surface | example files using it |
|---|---|
| `agent_loop` — the adaptive agent loop | **0** |
| `await` — durable suspension | **0** |
| `datasource`, `agent` declarations | **0** |
| `checkpoint`, `memo`, `throttle`, `debounce`, `risk` | **0** |
| `try` / `catch` / `finally`, `race`, `match` | **0** |
| `journey`, `trigger`, `channel` | **1** each — all in `channels-app.flux` |
| `saga`, `step`, `once`, `route`, `verify` | **1** each |
| `parallel`, `branch` | 6 |

The corpus is CI-gated and honest about what it is — the README calls it *"runnable `.flux` flows and
programs demonstrating the language and runtime."* But **the durability and resilience vocabulary is
the part that separates flux from a chat loop, and it is almost entirely undemonstrated.** A durable
journey that suspends on an event and resumes days later with no model re-spend is not a language
feature you appreciate from a grammar table; it needs a program that does it.

### Why the tracking framework is the right flagship

The `track` plugin (codewandler/agentplugins) maintains a spec-driven backlog: stories with
frontmatter, a generated board, a CHANGELOG, epics and designs. Its one deterministic component is
`gen_board.py`. **Everything else is a model following markdown instructions** — keep frontmatter
valid, regenerate the board after a status change, file a story for unscoped work, keep the roadmap in
sync.

That architecture drifts, and we have first-person evidence rather than a hypothesis. Sessions using
`track` on this very repo have found: epics carrying open work with no tracker and no narrative
(C-406, C-418…C-420); a dangling `C-330` reference cited by a story that was never filed; 185
non-`ready` stories still carrying a stale `priority`; nine priority values shared by two or more
`ready` stories, so the "rank" does not rank; and a board audit whose own regex produced 319 phantom
findings. None of these are model failures in any interesting sense. **They are what happens when
invariants live in prose that a model is asked to honour, instead of in a runtime that enforces them.**

Rebuilt as a flux app, the split is the thesis made concrete:

- **The runtime owns the mechanical half** — frontmatter validation, board regeneration, the
  epic-tracker audit, CHANGELOG sync, priority-collision detection. These become authored flow with
  declared bounds. They do not get skipped when the context is long, and they run in the same order
  every time.
- **The model owns the semantic half** — writing the story, judging whether a finding duplicates an
  existing one, deciding whether an epic is an initiative or a remediation bucket. Bounded jobs with
  typed inputs and outputs.

And then the recipe can *prove* it: run it twice and diff the result; `flux replay` it offline and
model-free. **A prompt-driven equivalent cannot make that claim at all** — not because it is badly
built, but because there is no artifact to replay.

### ⚠ On comparing to named competitors

The ask names another agent. **The recipes and the positioning page should argue against the
architecture pattern — the transcript as the runtime contract — not against a named product.** Three
reasons, and the third is the one that matters:

1. Specific claims about another system's internals cannot be verified from here, and a wrong one is a
   liability that outlives the post.
2. Competitors ship; a page pinned to their 2026 behaviour ages into a misrepresentation.
3. **It is weaker.** "Here is a run you can replay, fork and diff yourself, and here is why an agent
   whose contract is its transcript structurally cannot" is a stronger argument than any comparison
   table, and the reader draws the conclusion — which is what makes it click.

The vision already frames it this way (*"Mainstream agents let an LLM's transcript become the runtime
contract"*). Follow it.

## Approach

Five stories. The flagship lands **first** and the general contract is derived from it — a recipe
contract written before any recipe exists is speculation, and the shape of the real thing is what
teaches you which rules matter.

### C-425 — the flagship: `track` as a flux app

A runnable program under `examples/` (program form, `flux app run`) that maintains a tracked backlog:
validate frontmatter, regenerate the board, audit epics for missing trackers, keep the CHANGELOG in
sync — deterministic ops in authored flow — with model-authored stages for the parts that genuinely
need judgement. Sized to be *readable end to end*: a recipe nobody finishes reading demonstrates
nothing.

### C-426 — the determinism proof

The flagship's claim, made checkable: the same input produces the same board twice; the run replays
offline via `flux replay`; a deliberate change shows up in `flux diff` as a plan change rather than as
noise. ⚠ Separate from C-425 on purpose. Folded in, this becomes a sentence in a README that nobody
ever runs — and an unverified determinism claim on a page arguing *for* determinism is the worst
possible failure.

### C-427 — the recipe contract and where recipes live

Generalize: what makes a recipe a recipe (a real task, runnable from a clean checkout, and an explicit
statement of *which guarantee it demonstrates and how the reader checks it*), where they live relative
to `examples/`, and how they inherit the existing CI sweep — `crates/flux-eval/tests/examples_validate.rs`
already sweeps the whole directory with no hand-picked list, which is the property to preserve.

### C-428 — the coverage census

Ship the keyword-coverage sweep as a repeatable check rather than a one-off table, and classify each
gap: *needs a recipe*, *covered by docs*, or *deliberately unshown*. The table above is the first pass
and is not authoritative until this story confirms it against the grammar rather than against a grep.

### C-429 — the recipes surface and the positioning narrative

The website page that ties them together, argued as above. Each recipe entry states the guarantee and
the command that verifies it. This is the artifact the ask is really about; it lands last because it is
worthless without the recipes underneath it.

## Alternatives considered

- **More examples in `examples/`, no new concept.** Cheapest, and it keeps one CI sweep. Rejected as
  the framing: the gap is not quantity, it is that a language sample and a recipe answer different
  questions. Physically they may still share a directory — C-427 decides.
- **A separate `flux-recipes` repository.** Rejected: it would escape the CI gate that keeps the
  current corpus honest, and a broken recipe is worse than a missing one.
- **A comparison table against named agents.** Rejected — see above.
- **Lead with the coverage census.** Tempting (it is measured and immediately actionable), but a census
  produces a list of gaps, and gaps do not make anything click. The flagship does.

## Risks & open questions

- ⚠ **A recipe that flatters flux is worse than no recipe.** The pressure is toward a demo tuned until
  it looks inevitable. Every recipe must run from a clean checkout, and the ones needing credentials or
  a model must say so at the top.
- ⚠ **The flagship must not quietly become a product.** A tracking app good enough to use is a
  maintenance burden with users. It is a *recipe*: complete, readable, and explicitly not supported as
  a tool.
- **Determinism has honest limits.** Model-authored stages are not deterministic; the *shape* of the
  run is. C-426 must state precisely which layer the claim covers, or the first reader to get two
  different stories out of it will conclude the whole page is marketing.
- **Sizing.** The real `track` framework is large. Which subset makes the point without becoming
  unreadable is undecided; C-425 owns it.
- **Open:** whether recipes get their own directory or stay in `examples/`. Sharing the directory
  inherits the sweep for free; separating them lets a recipe carry a prose walkthrough beside the code.
- **Open:** whether a recipe may depend on a plugin. `advanced-code-review.flux` is already pinned at
  parse-only for exactly this reason, so a plugin-using recipe weakens its own gate.

## Acceptance / done

- A newcomer can run one command from a clean checkout and see flux do something real and useful.
- At least one recipe demonstrates a guarantee that an LLM-as-runtime agent structurally cannot offer,
  and ships the command that lets the reader verify it rather than believe it.
- The keyword-coverage census is repeatable, and every zero is either closed by a recipe or recorded as
  a deliberate omission.
- The recipes page argues from the architecture pattern, names no competitor, and every claim on it is
  backed by a command in a recipe.
