# Evidence-pinned memory — cross-session memory with provenance

Story: [A-92](../stories/A-92-evidence-pinned-memory-epic.md) · Pillar: Agent · Status: design

## The gap, and the shape of the flux answer

Cross-session memory is table stakes elsewhere. flux has **none** — verified: nothing in `crates/`
implements it (the `Memory*` hits in `flux-capabilities` are the in-memory vector store, a different
thing entirely).

The obvious implementation — a markdown scratchpad the model appends to and reads back — is exactly
what flux should not build. It reproduces the failure mode the whole product exists to reject: an
unfalsifiable claim, asserted by a model, that later readers cannot check and that silently rots as
the code moves underneath it. "The auth middleware is in `src/mw/auth.rs`" is a fact until someone
moves the file, and then it is a confident lie with no expiry.

The flux version pins every memory entry to **evidence**: the event-store receipt it was learned
from, and the git SHA the workspace was at. When the cited evidence changes, the entry does not
vanish and it does not keep asserting — it becomes **stale-visible**, carrying its own doubt into
the prompt.

## The load-bearing invariant

> **The model supplies the claim. The host supplies the citation.**

This is the same invariant that makes `ActionBatch` trustworthy ("the host, never the model,
constructs this value after validating the provider-native call against the live tool schema",
`staged.rs:203`), applied to memory. A model that could write its own `receipt` field could
manufacture provenance for a hallucination, and the citation would be decoration. So the
memory-writing op takes **only** a claim and a scope; the host stamps the receipt from the live turn
context and the git SHA from the workspace. There is no API through which a citation can be
supplied, forged, or omitted.

## Design

### 1. Entry schema

```rust
pub struct MemoryEntry {
    pub id: String,            // ULID, stable across edits
    pub claim: String,         // the model's contribution — the only model-authored field
    pub scope: Scope,          // Project { key } | Global
    // --- everything below is host-stamped ---
    pub receipt: Receipt,      // { stream: String, event_id: String, turn_id: Option<i64> }
    pub git: Option<GitPin>,   // { sha: String, paths: Vec<String> }  — None outside a repo
    pub learned_at_ms: i64,
}
```

`Receipt` cites the **stable event id** (a ULID), not `global_seq` — `global_seq` is a backend rowid
and would not survive a store migration or mean the same thing across the SQLite and Postgres
backends, whereas the id is caller-stable and already the basis of C-125's cross-process idempotency
proof.

`GitPin.paths` is the set of workspace paths the citing turn actually read or wrote, taken from the
turn's evidence trail — not from the model. That is what makes staleness computable: a claim learned
while reading `src/mw/auth.rs` is pinned to *that path* at *that SHA*.

### 2. Storage: its own stream in the same event store

Memory is cross-session, so it cannot live on a session stream. It gets its own stream —
`memory:<scope-key>` — in the **same** `events.db`, following the store's canon (append-only, one
log, projections as the read model, `EventKind::Custom` for app facts).

Consequences that come for free and are worth stating because they are the reason not to invent a
side table: multi-process safety (C-25/C-125), WAL hygiene (C-126), the Postgres backend, redaction
at the flush seam, and `flux export`-style read tooling all already work.

Edits and forgets are appends, not mutations — the projection takes the latest state per `id`, and a
forgotten entry appends a tombstone. History of what the agent *believed* is preserved, which is
exactly what you want when debugging a bad decision six sessions later.

### 3. Injection: memory entries are `ContextBlock`s

The injection seam already exists and is already hardened. `ContextBlock` →
`render_knowledge_blocks` (`flux-core/src/context.rs`) produces
`<knowledge-base id="…" title="…">` sections, is byte-budget-bounded with a **visible** truncation
marker (A-24), and neutralizes `</knowledge-base>` sequences in untrusted bodies so a block cannot
break out into top-level system content (A-21).

Memory rides that path unchanged. `ContextBlock.meta` carries the citation as tag attributes, so the
provenance is visible to the model, not just to `flux memory show`:

```xml
<knowledge-base id="mem_01J…" title="auth middleware location"
                source="memory" learned="s_42#01J…" sha="a1b2c3d" stale="true"
                stale-reason="src/mw/auth.rs changed since a1b2c3d">
The auth middleware lives in src/mw/auth.rs and is wired in main.rs.
</knowledge-base>
```

Reusing this seam rather than adding a memory-shaped prompt section is deliberate: A-21's escape
hardening and A-24's budget accounting are non-obvious, already paid for, and a second injection
path would have to re-earn both.

### 4. Staleness: computed at injection, never cached

At turn assembly, each candidate entry with a `GitPin` is checked:

- `git rev-parse HEAD` — already available via the context provider's helper
  (`flux-runtime/src/context.rs:133`).
- If `HEAD == pin.sha`, the entry is fresh.
- Otherwise `git diff --name-only <pin.sha>..HEAD -- <pin.paths>`; a non-empty result marks the
  entry **stale** with the changed paths as the reason.

One `rev-parse` plus at most one `diff` per turn (paths batched across entries), both cheap and both
already inside the guarded process boundary.

**Stale entries are still injected**, marked. This is the central behavioural decision and it cuts
against the obvious alternative of dropping them. A stale memory is not *false* — the file may have
changed for unrelated reasons — it is *unverified*. Dropping it silently loses real knowledge and
teaches nobody anything; injecting it with `stale="true"` and its reason lets the model do the one
correct thing: re-check before relying on it. Silence and false confidence are the two failure modes
worth engineering against, and dropping picks the first while a bare scratchpad picks the second.

Entries with no `GitPin` (learned outside a repo) are never stale and never claim to be fresh — the
attribute is simply absent.

### 5. Writing: one op, no citation parameter

```
memory_note(claim: string, scope: "project" | "global") -> memory id
```

Declared with `Effect::Write` so it passes the ordinary envelope (policy, approval, audit) — writing
durable cross-session state is a real effect and is gated like one. It takes no receipt, no SHA, and
no paths: the host reads those from the live `RuntimeTurnContext` and the workspace. A model cannot
express a forged provenance because the parameter does not exist.

Compensation (A-91): `Compensation::Inverse { op: "memory_forget" }` — a memory write is cleanly
reversible, which makes it a good early consumer of that contract.

### 6. Inspect and prune

```
flux memory list [--scope project|global] [--stale]
flux memory show <id>          # claim + full citation + freshness, and what changed
flux memory forget <id>        # appends a tombstone
```

`flux memory list --stale` is the maintenance loop: it is the review queue for knowledge whose
evidence moved. Pruning is a user verb; flux never silently forgets on the agent's behalf.

### 7. Retention: memory is not a timer's business (C-231)

Putting memory on its own stream in the shared event store buys everything in §2 for free, and
inherits one hazard with it. `EventStore::prune_adhoc_older_than` (D-77) deletes every stream that
carries no `streams` registry row and whose newest event predates a cutoff. A `memory:<scope-key>`
stream is exactly that shape. Left alone, the first scheduled retention job flux ever ships would
delete cross-session memory — silently, with no error, and with the deleted evidence being the only
thing that could have reconstructed what was lost. There is no caller today, which is why this is
cheap to settle now and expensive to settle later.

**The position: memory is not prunable by age, and the ad-hoc sweep must skip it entirely.** Three
reasons, in the order they actually decide it:

1. **Age is not disuse for a memory.** For a session stream, "no event in 90 days" means the
   conversation is over. For a memory stream it means the knowledge *settled* — a claim nobody has
   had to amend is the best case for a memory, not a sign it is unwanted. A time-based horizon reads
   the healthiest entries as the most disposable, which is the wrong way round.
2. **There is no second copy.** The whole point of §1's `Receipt` is that an entry can be checked
   later. Delete the stream and both the claim and its provenance are gone; unlike a pruned session
   (whose cost rollups survive, and which was a transcript anyway), nothing downstream retains a
   shadow of it. Silent, unrecoverable, unattributable — the three properties that make a data-loss
   bug impossible to diagnose after the fact.
3. **A deliberate path already exists.** `flux memory forget` (A-110) appends a tombstone: the entry
   leaves the read model and its history stays, which is the auditable form of "stop believing this".
   Forgetting is already a user verb with a receipt. A sweep would be a second, worse path to the
   same outcome — no tombstone, no record, no actor.

**The cost is acknowledged, not waved away.** "Never delete" is a choice with consequences, and
unbounded growth is real. It is also, here, small and bounded by a human: entries are written only by
deliberate `memory_note` calls (§5, *not* automatic formation — see Non-goals), each is a short claim
plus a citation, and injection is byte-budget-capped by A-24 regardless of how many exist, so growth
costs disk rather than prompt. A store where that becomes the dominant cost has a memory *hygiene*
problem — thousands of unreviewed claims — and the fix for that is `--stale` review, not a timer that
deletes the reviewed ones too.

**If a memory retention policy is wanted later**, these are its terms, so that a future author does
not have to re-derive them:

- **Scope-aware and explicit.** Its own entry point (`prune_memory_*`), not an ad-hoc-stream sweep
  that happens to reach memory. Memory retention that arrives as a side effect of a generic prune is
  the exact failure C-231 closed, and re-opening it by widening the generic prune is not an option.
- **Argued per scope.** `Global` and `Project { key }` are different bets: a project scope can be
  retired with its project, a global one usually cannot.
- **Entry-shaped, not stream-shaped.** The unit is a `MemoryEntry`, and the mechanism is A-110's
  tombstone append — the append-only log stays append-only, so what was believed and when it stopped
  being believed remain answerable. Deleting stream rows would make the retention itself unauditable.
- **Staleness is not a delete criterion on its own.** §4 already settled that a stale entry is
  *unverified*, not false. A policy built on `--stale` would delete exactly the knowledge that most
  needs a human look.

Mechanically, the decision lives in `crates/flux-events/src/retention.rs`:
`ADHOC_STREAM_FAMILIES` lists each ad-hoc stream family flux names with a `Retained`/`Prunable`
verdict and its reason, and all three store backends filter candidates through the one
`is_retained_from_adhoc_prune` classifier. The table is a decision record rather than a lookup table
on purpose — the next ad-hoc stream family has to answer the same question in the same place, and a
`STREAM_PREFIX` declared in `flux-events` without a row fails the gate.

## Non-goals

- **Not semantic retrieval.** v1 injects by scope + recency under the existing byte budget. Ranking
  memory by embedding similarity is a real question, but it is a *selection* problem layered on top
  of a provenance model that must exist first, and `flux-capabilities` already has the vector
  substrate when it is time.
- **Not automatic memory formation.** The agent calls `memory_note` deliberately. Mining memories
  out of session history is [L-84](../stories/L-84-habit-compiler-epic.md)'s shape of problem, not
  this one's.
- **Not shared/team memory.** Single-principal, local store. Multi-principal memory inherits the
  whole per-principal isolation question and is its own epic.
- **Not a replacement for `AGENTS.md`/skills.** Authored, committed guidance stays authoritative;
  memory is *learned* and therefore always cites where it came from.

## Verification

- Failing-first headline: a memory written in session A is injected into session B with its citation
  intact — impossible today (no memory exists).
- A memory pinned to a file, then that file changed and committed, renders `stale="true"` with the
  changed path named — and is still injected.
- The forgery test: no public API path allows supplying a `receipt` or `git` field on
  `memory_note`; a test asserts the op's input schema has exactly `claim` and `scope`.
- A memory body containing a literal `</knowledge-base>` cannot break out of its block (inherits and
  re-pins the A-21 property at this new call site).
- Budget: memory blocks respect the A-24 accounting, with the omission marker counted.
- Redaction: a claim containing a credential shape is redacted before it reaches the store.
- Retention (§7, C-231): an aged `memory:*` stream survives `prune_adhoc_older_than` while an
  ordinary aged ad-hoc stream in the same sweep is still deleted — on every backend — and a
  `STREAM_PREFIX` declared in `flux-events` with no `ADHOC_STREAM_FAMILIES` row fails the gate.
- Full gate in both workspaces.

## Stories

| ID | Title |
|---|---|
| A-107 | The memory stream + `MemoryEntry` projection (append, edit, tombstone) in `flux-events` |
| A-108 | `memory_note` op with host-stamped citation — the no-forged-provenance seam |
| A-109 | Injection as `ContextBlock`s + git-pin staleness computed at turn assembly |
| A-110 | `flux memory list/show/forget`, including the `--stale` review queue |
| C-231 | The ad-hoc prune must not evaporate memory — the retained-family table and §7's position |
