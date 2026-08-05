# Archived first-class board design before Decision 0010

This is the verbatim pre-Decision-0010 design record. The active replacement is
[../../../docs/designs/native-board-fleet-cli.md](../../../docs/designs/native-board-fleet-cli.md).

---

# Design: the first-class board — one fixed tool surface, pluggable backends

**Status:** proposed · **Pillar:** Agent · **Epic:**
[A-148](../stories/A-148-first-class-board-epic.md) · **Stories:**
[L-130](../stories/L-130-board-declaration-in-flux-lang.md) ·
[A-134](../stories/A-134-sdk-seam-for-a-workboard.md) ·
[A-115](../stories/A-115-jira-board.md) · [A-118](../stories/A-118-gitlab-board.md) ·
**Related:** [fleet-coordinator.md](fleet-coordinator.md) (shipped the port, MemoryBoard,
MarkdownBoard and the generated ops this design re-homes) ·
`../flux-roadmap/decisions/0006-datasources-are-declared-read-surfaces.md` (the decision this
design executes)

## Why

Flux-roadmap Decision 0006 gives the family one datasource definition — a named, declared,
**read-only** record surface; *operations do; datasources know* — and the work board fails that
family test on purpose: it mutates. Today the board is bound through the `datasource` declaration
with a `kind "board:…"` prefix hack, its subjects live in an unprefixed `<name>/item/<id>` grammar,
and an embedder cannot bind one at all (A-134). The decision's answer: **the board leaves the
datasource vocabulary and becomes a first-class Flux concept** — its own declaration, its own
`board:` subject namespace, its own SDK seam.

What must *not* change while that happens is the thing that makes boards work for models: the board
contract is deliberately shaped for model use — **one small fixed tool surface with a closed state
machine that stays identical regardless of backend**. The 11 generated operations (`list`, `get`,
`create`, `transition`, `claim`, `comment`, `record_dispatch`, `query`, `comments`, `reassign`,
`record_evidence` under the declared name) and the closed `State` machine
(`crates/flux-datasource/src/board.rs`) are the contract; backends are behind it.

## The model

### A first-class declaration (L-130)

```flux
board tasks
  kind "markdown"
  path "./board"
```

The declaration's name is the operation prefix, exactly as today. Board kinds become the bare
backend names (`markdown`, `memory`) in their own kind namespace — the `board:` prefix existed only
to disambiguate inside the datasource slot, and it retires with the slot. Unknown kinds stay hard
startup errors naming the kinds that exist. The `kind "board:*"` datasource spelling is retired
with a migration note: the loader tells the author the exact `board <name>` replacement. After
L-130, a `datasource` declaration cannot bind anything that mutates, by construction.

### The SDK seam (A-134)

`ClientBuilder::try_with_work_board` (name decided in the story) binds an `Arc<dyn WorkBoard>`
under a domain with the same all-in-one guarantees `try_with_live_datasource` protects: generated
ops, evidence group and ambient signal install together, collisions are source-labelled build
errors, and the ops surface only when a board is actually bound. The registration function already
exists one layer down (`flux_capabilities::try_register_work_board`); the seam is the missing SDK
carry.

### One subject namespace

Board subjects move to `board:<name>/item/<id>` (`board:<name>/item/new` for `create`), a namespace
of their own beside the canonical `datasource:<name>/<entity>[/<id>]` grammar — shared work with
D-251, done once. Mutating subjects stay concrete per item, never widened.

### Pluggable backends — memory and markdown today, vendor trackers later

The backend seam is the shipped `WorkBoard` port (A-113) with `MemoryBoard` and `MarkdownBoard`
(A-114) behind it, both passing one shared contract suite. Vendor trackers arrive through the
Decision 0006 **declared-surface pattern**, generalized from datasources — the datasource is the
read instance of that pattern; the board is the write-capable instance:

- **Flux** owns the contract and the fixed tool surface: the 11 ops, the closed state machine, the
  contract suite.
- **flux-connectors** declares a vendor **board member** as a projection over that connector's
  operations: the status↔`State` mapping and per-verb operation bindings.
- **flux-exchange** binds a member per tenant to a connection label and executes every mutation as
  an admitted, granted operation — writes stay operations, so grants and approval need no new
  machinery, and the backend declares no vendor network access of its own.

A-115 (Jira) and A-118 (GitLab) hold that charter. Both were originally written over
`plugins/jira` / `plugins/gitlab`, a path Milestone 5 deletes; they are re-pointed, not rewritten —
the mapping-as-configuration and suite-unmodified acceptance carry over intact.

## Sequencing

What lands now is the vocabulary, the Flux-side board split (L-130, A-134) and the re-pointing.
The vendor generalization — connector board members, Exchange tenant Board bindings — is named here
and designed with Milestone 3, per the decision. Nothing in this design touches the Milestone 1
first-run path.

## Non-goals

- Changing the 11-op surface or the state machine while re-homing the declaration — the fixed
  contract is the point.
- A per-backend tool surface, or backend-specific ops leaking into the catalog.
- Designing the connector board-member IR or the Exchange Board binding here — that is Milestone 3
  work with its own cross-repository design; this document only fixes what it must generalize over.
- Re-admitting boards to the datasource registry, `sources`, or `datasource.read` reasoning.

## Story map

1. **L-130** — the `board <name>` declaration; retire `kind "board:*"` with a migration note.
2. **A-134** — the SDK seam, absorbed from the fleet-coordinator epic.
3. **A-115** — JiraBoard through Exchange-governed operations (Milestone 3+).
4. **A-118** — GitlabBoard, the generalization proof (Milestone 3+, after A-115).

Subject-grammar work is shared with [D-251](../stories/D-251-datasource-authority-subject-grammar.md).
