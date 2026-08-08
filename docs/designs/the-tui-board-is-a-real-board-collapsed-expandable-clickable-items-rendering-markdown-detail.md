# Design — The TUI board is a real board: collapsed, expandable, clickable items rendering markdown detail

## Why

The TUI presents the Board as flat text. An operator watching a Fleet cannot see the board *as a board*:
what is ready, what is in flight, what is blocked, and what a given item actually says. The information
already exists — `flux board items/get/query` return it as typed JSON, and story bodies are markdown —
but the surface flattens it, so reading one item's contract means leaving the TUI.

The operational cost is concrete. Deciding whether a wave should be dispatched, or why a story is
parked, currently means running Board CLI calls by hand and reading raw frontmatter. The operator is
the one party the Fleet cannot query, so the surface they watch has to carry the detail.

## Approach

Render items as **collapsed boxes by default, expandable in place**. Collapsed shows the decision-grade
facts only: id, title, status, priority, and whether a wave is in flight for it. Expanding renders the
story body as markdown inside the pane — Goal, Acceptance checkboxes, Notes — without leaving the TUI.

Constraints that shape it:

- **Read-only.** Nothing here mutates planning state. Status changes go through the Board CLI, which
  validates transitions; a clickable surface must not become a second, unvalidated mutation path.
- **Grouped by status, ordered by priority**, so the collapsed view answers "what is next" without
  expanding anything.
- **Bounded rendering.** A story body can be long and the board has >1100 items. The pane must page or
  virtualize rather than build every box up front, and markdown rendering must be width-aware — the
  transcript already had an off-by-one wrap defect from `area_width` vs usable columns.
- **Mouse and keyboard parity.** Clicking a box expands it; the same must be reachable by keyboard, since
  the TUI runs in tmux where mouse capture is not always available.

## Stories

- Collapsed item boxes grouped by status, priority-ordered, with in-flight wave indication.
- Expand in place to a markdown-rendered story body (Goal, Acceptance, Notes), bounded and width-aware.
- Click and keyboard parity for expand/collapse, with a visible selection that survives a refresh.
- Board pane stays read-only: no status mutation from the surface, and that invariant is tested.
