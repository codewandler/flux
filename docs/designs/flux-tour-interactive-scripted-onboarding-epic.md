# Design — flux tour: interactive scripted onboarding (epic)

## Why

The TUI is dense and capable — transcript, approval sheet, panes, fleet and board views, pickers,
themes — and none of it is discoverable to a newcomer beyond a static help overlay. The project
also has an unusually well-curated vocabulary (`docs/concepts.md`: ~45 terms in a parseable
`**Term** — definition` form, deliberately organized around confusable pairs, mirrored to the
website and guarded by `website_in_sync`), which today only helps people who already read docs.
An interactive tour turns that raw material into onboarding: the fastest path from "installed
flux" to "understands sessions, ops, approvals and where the boundaries are".

## Approach

**A tour is data, not code.** A tour script is a declarative list of steps: the region of the
screen it spotlights, the copy it shows, and optionally a simulated event (a staged prompt, an
approval request, a pane opening). The engine executes steps deterministically — Next/Back/Skip,
progress indicator — over the real TUI with staged content, so what the user sees is the actual
product, not screenshots. Precedents in-tree: the `loopmock` staged-content module for realistic
fake data, and `drive_event_loop_headless` for proving every step renders and advances under a
`TestBackend` — a tour is testable end to end without a terminal.

**Spotlight, don't fork the UI.** The tour renders as an overlay (dim + callout + key hints)
following the TUI's one-overlay-chrome discipline; it never duplicates a view. Steps that
reference vocabulary pull definitions from `docs/concepts.md`'s term list rather than restating
them, so the tour cannot drift from the canonical glossary. The Flux-Lang half has a free,
always-current question/definition source in `flux_lang::schema::node_kind_rows()`.

**Entry points.** `flux tour` starts the default tour; a first-run hint (no session history yet)
offers it without forcing it. Tours are authorable (a documented step format), so later tours can
target specific surfaces — "your first approval", "reading the fleet view", "the ops explorer".

**Safety posture.** A tour drives staged content only. It never executes real operations, never
touches a live session's state, and the approval-sheet step shows a *simulated* request clearly
labelled as such — the tour must not train users to approve things reflexively.

## Stories

Candidates to file when the epic is picked up (kept unfiled until then, per backlog hygiene):

- tour engine: declarative step schema, deterministic advance/back, headless step tests
- spotlight overlay chrome: dim/callout/progress in the shared overlay language
- `flux tour` entry + first-run hint
- authoring format doc + a second tour proving the format generalizes

Related epics: `ops-explorer` (a natural tour destination), `docs-reader` (deep links from tour
steps into the reader for "learn more").
