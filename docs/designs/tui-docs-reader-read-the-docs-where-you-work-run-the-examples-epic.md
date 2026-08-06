# Design — TUI docs reader: read the docs where you work, run the examples (epic)

## Why

The public documentation is good and lives exactly where an operator working in a terminal is
not: a website. The release binary already embeds a release-matched copy of that site
(`crates/flux-server/assets/public-docs.zip`, served by `flux docs`), so the corpus problem is
solved — what is missing is a reading surface in the TUI itself and, more importantly, the thing
a website can never do: **run the examples**. Docs pages carry fenced `flux` code blocks; in the
terminal, next to a live engine, those blocks can be executed rather than copy-pasted.

## Approach

**Reader.** A full-screen docs view: navigation tree derived from the site's sidebar order, fuzzy
page search, page rendering through `flux-markdown` (the first-party markdown→ratatui renderer),
cross-page links followable in-TUI. The corpus is the release-matched embedded docs (or the
markdown sources embedded alongside), so the reader is offline-correct and version-honest by
construction.

**Runnable examples — the safety headline.** A fenced `flux` block gets a run affordance. Running
an example goes through the normal engine path: authored Flux-Lang, authorization → approval →
guarded IO, exactly as if the operator had written the flow themselves. No bypass, no "docs mode"
privilege, no pre-approval — a destructive example asks for approval like anything else. This is
a feature *and* the constraint that makes it shippable; the design must never add a second
execution path.

**One index, not two.** docs/designs/agent-native-flux-docs.md (stories C-579/C-580/C-581,
backlog) proposes a `flux-docs` datasource: a release-matched, indexed corpus with
`search`/`get`/`list`/`relation`. The reader is a *consumer* of that datasource once it exists.
Until then it may derive navigation/search from the embedded corpus directly, but it must not
grow a second persistent index format that C-579 would then have to migrate or compete with.

**Relationships.** The ops explorer's docs pane (`ops-explorer` epic, iteration 3) is a
specialization of this reader scoped to one page family; they must share the rendering and corpus
plumbing. A `flux tour` step can deep-link into the reader for "learn more".

## Stories

Candidates to file when the epic is picked up:

- corpus access: enumerate + read the embedded release-matched docs from the TUI process
- reader view: tree, fuzzy page search, flux-markdown rendering, in-TUI link following
- runnable `flux` blocks through the normal engine path, with approval semantics proven in tests
- convergence: ops-explorer docs pane and the reader share corpus + rendering plumbing

Related epics: `ops-explorer`, `flux-tour`.
