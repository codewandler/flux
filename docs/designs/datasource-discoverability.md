# Design: datasource & endpoint discoverability

**Status:** proposed · **Pillar:** Agent + Core · **Stories:**
[D-114](../stories/D-114-datasource-sources-op.md) ·
[D-115](../stories/D-115-endpoint-group-surfacing.md) ·
[D-116](../stories/D-116-static-endpoint-wiring.md) ·
[D-117](../stories/D-117-endpoints-flows-website-docs.md) ·
related: [D-62](../stories/D-62-async-live-datasource-seam.md) (live-backend seam, unchanged by this epic)

## Why

The question that triggered this epic: *what can the agent do today to enumerate the datasources it
has, and to register new ones — e.g. wire a Postgres endpoint and start querying it?* A grounding
pass over the working tree (2026-07-09) found that **the capability exists and is well-built, but it
is largely undiscoverable** — by the agent at runtime and by users on the website. "Datasource" in
flux today spans three subsystems that do not share a registry:

1. **The knowledge index** (D-07): records ingested from the workspace auto-index (`local`), program
   `datasource` declarations (`markdown` | `openapi` kinds only — `crates/flux-cli/src/main.rs:972`),
   and plugin-emitted records. Read through five always-surfaced ungrouped ops
   (`search`/`get`/`list`/`relation`/`batch_get`, `crates/flux-capabilities/src/datasource/ops.rs`).
   **Gap:** every op requires a known `source` name; there is no op, CLI command, or REPL command
   that enumerates the sources themselves (`DatasourceBackend` has `len`/`is_empty` but no
   `sources()`; only the unexposed `PostgresBackend::namespaces()/scan()` come close). The agent
   cannot answer "what knowledge do I have?".
2. **Endpoint brokerage + the `sql` plugin** (D-25..D-32): the actual "wire a live Postgres and
   query it" path — `endpoint.discover` → `endpoint.select` (weak `EndpointRef`, never a secret) →
   `sql.query {endpoint_ref}` with host-terminated SCRAM (`crates/flux-plugin/src/pg.rs`).
   **Gaps:** (a) the `endpoint` op group surfaces only on the ambient `kubernetes` signal (kubeconfig
   present, `crates/flux-tools/src/groups.rs:114`) — an operator who registered a Postgres endpoint
   in `~/.flux/endpoints.toml` without k8s never sees the ops; (b) the group's tool list names four
   ops while `endpoint_tools()` registers five — `endpoint.import` is ungrouped, so the one
   write-effect endpoint op is *always* advertised while the four read ops are gated (inverted);
   (c) static wiring is half-finished: registration without discovery means hand-writing
   `flux endpoint import --from-json '<EndpointRef JSON>'`, and the `StaticResolver` is constructed
   with an **empty bindings map** ("No host config endpoint bindings are wired yet",
   `crates/flux-cli/src/main.rs:2185-2190`) so config-named refs cannot resolve at IO time.
3. **Saved flows / composite ops** (L-72, L-06): `~/.flux/flows` + project `.flux/flows`,
   `flow_list`/`flow_run`, and agent-side `op.register` (turn|session|project|global scopes). This
   layer is healthy — it is the model for what "register + enumerate" should feel like — but it has
   one paragraph of public documentation.

On the website, the datasources concept page shipped 2026-07-09 (`5b7f9c5`), but the **endpoint
subsystem has zero public documentation** (no concept page; `flux endpoint` missing from the CLI
reference; one incidental line in `plugins/gitlab.md`), and `~/.flux/flows` has only a paragraph in
`modules-and-programs.md`.

The organizing idea: **close the discoverability loop, not build new machinery.** The agent should
be able to enumerate what it can look up (sources), see the endpoint ops whenever endpoints exist,
and an operator should be able to wire a known service in one command — with all of it documented.

## Approach

Four stories — one implement, two harden, one document:

- **D-114 (implement)** — a sources-enumeration op. Add `sources()` to `DatasourceBackend`
  (per source: name, entities, record count), implement on Memory/Sqlite/Postgres backends, expose
  as a sixth always-on read-only retrieval op alongside the existing five. Naming follows the
  existing bare convention (`sources`); the generic-name collision risk the ops audit flagged is
  acknowledged in the story.
- **D-115 (harden)** — surface the `endpoint` group from the endpoints store: `detect_signals`
  emits the (already-honored, never-injected) `endpoint` signal when the persisted endpoint store
  is non-empty; add `endpoint.import` to the group manifest so all five ops gate together.
- **D-116 (harden/implement)** — static endpoint wiring end-to-end: an ergonomic
  `flux endpoint add` (product/url/credential-ref flags → weak ref in `~/.flux/endpoints.toml`),
  and wire `StaticResolver` bindings from host config so statically-registered refs actually
  resolve at IO time (today only discovered `@endpoint/*` refs do). Proof: added Postgres endpoint
  → `endpoint.list` shows it → `sql.query` connects via host-terminated SCRAM.
- **D-117 (document)** — website: an endpoints concept page (weak-ref model, discover/select/
  import, the sql-plugin end-to-end), `flux endpoint` in the CLI reference, a saved-flows page
  (`~/.flux/flows`, `flow_list`/`flow_run`, `op.register`), and cross-links from the datasources
  page answering "how does the agent know which sources exist". **Done 2026-07-10**, expanded into
  a full public-doc truth pass with executable drift guards and release-bound Pages deployment.

**Explicit non-goal:** making a live SQL database a first-class *knowledge* datasource (async paged
backend). That is [D-62](../stories/D-62-async-live-datasource-seam.md) (design-first, backlog) and
this epic neither starts nor blocks it. Also untouched: the dormant `PostgresBackend` record store
(D-74 — implemented, no binary enables the feature), and semantic/embeddings retrieval (deferred in
v1 by design).

## Alternatives considered

- **One `datasource.register` op letting the agent wire arbitrary new datasources at runtime.**
  Rejected for now: knowledge datasources are declared (program/plugin manifest) by design — the
  envelope treats ingestion as an owner decision, not a model decision. The agent-side registration
  story is `op.register` (already shipped) for behavior, and D-116's operator path for endpoints.
- **Surfacing the endpoint group unconditionally** (drop the signal gate). Rejected: evidence-gated
  surfacing is a deliberate invariant; the fix is emitting the right signal, not bypassing gating.
- **Documenting only** (no code). Rejected: the sources-enumeration hole and the inverted
  `endpoint.import` gating are real capability/consistency defects a docs page cannot paper over.

## Risks & open questions

- `sources()` on the trait touches all backends including the feature-gated Postgres one — keep the
  method defaultable or implement it everywhere to avoid a feature-flag build break.
- The `endpoint` signal must read the endpoint store cheaply at signal-detection time (startup-loaded
  registry vs re-reading `endpoints.toml` per turn — prefer the loaded registry).
- D-116's "config bindings" shape (TOML key layout under `[endpoint]`) needs a small design pass in
  the story before implementation; the resolver seam already exists.
- Concurrent sessions: this epic touches `flux-cli/main.rs` wiring that other in-flight work also
  edits — rebase story work on the tree state at pickup time.

## Acceptance / done

Union of the four stories' acceptance. The epic-level proof is the user's original scenario, run
end-to-end without a kubeconfig: register a Postgres endpoint with one CLI command, start an agent
session, watch it *discover* the endpoint ops and the new `sources` op unaided, enumerate its
knowledge sources, and run a `sql.query` through the wired endpoint — with every step documented on
the website.
