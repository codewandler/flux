# Design: embedded docs and the structural Flux-Lang playground

## Decision

`flux docs` serves a build-time snapshot of the existing Docusaurus site from the distributed
binary. The site remains authored and built under `website/`; one deterministic script turns the
build output into a compressed checked-in bundle consumed by the non-published `flux-server` crate.
The binary therefore needs neither a checkout nor Node at runtime, and the content cannot silently
advance beyond the binary that serves it.

The server uses `/flux/` for the Docusaurus tree because that is the public site's existing base URL,
and adds convenient top-level `/console/` and `/version` routes. The console is also emitted beneath
the public site's `/flux/console/` path, so one frontend artifact covers the hosted tour and the local
editable experience.

## Playground boundary

The only dynamic playground operation is source projection. A bounded request enters
`flux_lang::editor::project_source`; its result is the versioned `EditorFlow` or an honest source-only
diagnostic. No operation host is installed, no `Executor` is assembled, and no filesystem, process,
network, model, credential, or approval path is reachable.

Play/pause/rewind/previous/next move a cursor through the projected structural nodes. This is a
**trace preview**, not execution: it contains no values and cannot pause, repeat, or undo an effect.
The console and documentation keep that distinction visible. Live inspection and mutation retain
the stronger design in `interactive-debugger.md`, including redaction and intervention evidence.

## Routing and exposure

- `GET /version` is small public build metadata: the CLI package version only.
- `POST /api/playground/project` accepts a capped JSON source body and returns only parser/editor
  output. A short request timeout and the language's own depth ceilings bound hostile input.
- Unknown `/api` paths return 404 and never receive the documentation fallback.
- Static assets carry explicit content types and immutable cache headers for hashed files; HTML and
  version responses do not.
- This surface may bind publicly because it exposes no agent and no effects. It is deliberately not
  merged into the authenticated agent server, whose route/auth invariant remains unchanged.

## Coherence

`scripts/build-embedded-docs.sh` runs the ordinary website build and writes the compressed bundle in
a stable file order with normalized metadata. `--check` rebuilds into a temporary file and compares
bytes. The website CI lane runs the check after its build, while Rust builds consume only the tracked
artifact and therefore remain Node-free.
