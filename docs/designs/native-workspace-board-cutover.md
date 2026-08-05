# Native workspace Board cutover

**Status:** accepted by flux-roadmap Decision 0013 · **Story:**
[C-588](../stories/C-588-native-workspace-board-cutover.md)

## Outcome

A workspace can be operated with the installed Flux CLI alone. `.flux/board.toml` is the independent
workspace Board binding and cross-repository program schedule. Fleet consumes that resolved Board;
plain `flux board` auto-selects it; no Fleet config, README instruction, AGENTS coordination prompt,
TSV parser or private helper script is required to discover authorized work.

## Configuration

The closed v1 shape is:

```toml
schema = "flux.board-workspace/v1"
id = "workspace"
default = true
active_milestone = "1"
vision = "VISION.md"
roadmap = "ROADMAP.md"
decisions = "decisions"
designs = "docs/designs"

[[members]]
id = "flux"
root = "../flux"
board = "default"
canonical_ref = "origin/main"

[[program]]
id = "fleet-loop-binding"
item = "flux/C-569"
milestone = "1"
order = 47
depends_on = ["flux/C-566"]
outcome = "Resolve one explicit loop binding at every agent start."

[[waves]]
id = "flux-fleet-worker-boundaries-1"
state = "active"
repository = "flux"
items = ["flux/C-569", "flux/C-567"]
depends_on = []
```

Member roots and document paths resolve under the workspace root and cannot escape through symlinks.
Program ids are unique. `(milestone, order)` and lane ids are unique. Every program item and wave
item is an exact configured-member `BoardRef`; every wave is repository-local and contains at most
ten items. A program dependency may add cross-repository ordering to the repository story's own
dependencies but never removes or replaces them.

One `active_milestone` replaces the old tranche state. A program entry has no status: Board reads it
from the authoritative member story at its pinned canonical ref/current configured checkout. An
operational note with no story is a document or decision, not a fake schedulable lane.

## Discovery and authority

`BoardCommand.scope` records whether the caller supplied `--scope`. When omitted, Flux looks for a
valid default `.flux/board.toml`: present means workspace; absent means the existing repository
default. Explicit session/repository/workspace selection still wins. Mutations against a workspace
still require `--board MEMBER` and route through that member's permission subject.

The workspace Board owns only program documents and references. It does not copy member story files,
status, Goal or Acceptance. Revision hashing includes the Board config and every member revision, so
optimistic writes notice program/member changes.

## Scheduling

`board next` and `fleet schedule` share one projection:

1. select program entries in the active milestone;
2. read authoritative member story state;
3. combine story dependencies with configured cross-repository dependencies;
4. retain explicit ready items whose complete dependency set is done;
5. preserve program order;
6. project eligible items into configured repository waves.

When a program catalogue exists, neither surface falls back to unrelated repository-ready work.
Wave dependencies and the ten-item/repository-local constraints are validated before scheduling.

## Documents, statistics and history

Workspace document roots are explicit. The legacy roadmap decision form `**Status:** accepted` maps
to Board `decided`; `open`, `decided`, `accepted` and `superseded` are normalized without treating a
missing frontmatter block as an open decision. Only a genuinely open record with a question creates
human attention.

The metric cube exposes `program_stories`, `milestone_lanes`, `waves` and members. Each rollup derives
done from authoritative story state; no tranche field or zero-filled placeholder remains. History
uses the same program catalogue at the relevant revision where available and labels unavailable
historical configuration explicitly.

## Compatibility and migration

Repository Boards keep their existing default and CLI contract. Fleet execution configuration keeps
models, capabilities, gates, fences, concurrency and worktree policy, but no longer owns program
groups. The roadmap migration writes `.flux/board.toml`, proves a real three-member smoke fixture,
then deletes the TSV/coordination helpers and their README/AGENTS instructions. Git history is the
only compatibility archive.
