# Design: data pipelines — a pipeline is a Flux-Lang Program

**Status:** proposed (exploration) · **Pillar:** Agent · **Story:**
[D-254](../stories/D-254-data-pipelines-exploration.md) · **Related:**
[async-live-datasource-seam.md](async-live-datasource-seam.md) (the read seam pipelines consume) ·
[datasource-rag.md](datasource-rag.md) (the index a sync pipeline would eventually feed) ·
`../flux-roadmap/decisions/0006-datasources-are-declared-read-surfaces.md` (the deciding document)

> **This is a seed, not a design.** Decision 0006 names data pipelines and deliberately does not
> design them; this document records the decided boundary so the D-254 exploration starts from it
> instead of re-litigating it. Everything below the constraints section is open.

## The decided shape

A **pipeline is a Flux-Lang Program** — datasource reads, transforms, and a declared sink,
triggered by a channel, event or schedule, and installable as an App. Apps are installed Programs,
not a second workflow model, and **pipelines do not get first-class syntax** for the same reason.
A pipeline is not a new runtime concept; it is a Program whose body happens to move records.

Named for later milestones and not designed here, all gated on the Milestone 3 stream and lease
vocabulary:

- Exchange-governed incremental sync into an index — indexing remains a separate later binding;
- streaming change-feed datasources (a declared datasource-member capability, per 0006 rule 10);
- a declared sink/write-surface contract.

## The invariant that binds immediately

**Credential-marked material never enters a `Record`.** Anything a connector declares as producing
or carrying a credential is refused by ingest paths, and ingest runs the redactor. This is not a
pipeline feature to be designed later — it holds for every ingest path from the moment records
move, and the exploration's job is to name the exact seams (ingest helpers, the sink contract)
where the refusal is enforced and how a test observes it. The existing two-layer defense from the
live seam applies: shape first (no field can hold a secret), redactor as backstop.

## Explicit non-goals

Unchanged from the accepted seam designs, restated by 0006:

- per-record ACLs — tenant isolation is by construction;
- a query DSL;
- cross-source joins;
- caching live rows into the index;
- any Exchange-side index before Milestone 3;
- first-class pipeline syntax — rejected above, permanently.

## Open questions for the exploration (D-254)

1. **The sink contract.** What does a Program-declared write surface look like, given that a
   datasource is read-only by definition — a sink is not a datasource, so what is it? How does it
   relate to the board (the existing write-capable declared surface) and to the connector
   declared-surface pattern (connector-declared member, Exchange tenant binding, writes as admitted
   operations)?
2. **Trigger composition.** Channels, events and schedules already wake Programs; what, if
   anything, does a sync pipeline need beyond what `trigger` provides today?
3. **Incremental sync bookkeeping.** Where does a pipeline keep cursors/watermarks — Program state,
   the index, or an Exchange-side lease — without violating the no-local-vendor-adapter rule
   (0006 rule 8)?
4. **App packaging.** What does "installable as an App" require of a pipeline Program beyond what
   any Program requires — reviewed datasource access lists already exist on the App concept.
