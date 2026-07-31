# Cross-harness session history as a datasource

**Epic:** C-212 · **Stories:** C-213 → C-216 · **Status:** designed, none started

## The ask

Let an agent search what was already said — in *any* local coding harness, not only flux:

```
search(query: "why did we drop the retry wrapper", harness: "opencode")
```

`harness` selects among `flux | codex | claude-code | opencode`; omitting it searches all of them.
The result is conversation content — the message that answers the question, with enough address
(harness, session, workspace, timestamp) to go read the rest.

## Why this is small, and why that is the interesting part

flux already does the hard half. `flux usage` (`crates/flux-cli/src/usage.rs`, 2919 lines) locates,
opens, and parses the local state of all four harnesses today:

| harness | root | shape | discovery |
| --- | --- | --- | --- |
| flux | the flux event store | `flux-events` | `EventStore` |
| codex | `$CODEX_HOME` → `~/.codex/sessions` | JSONL | `harness_root("CODEX_HOME", ".codex")` |
| claude-code | `$CLAUDE_CONFIG_DIR` → `~/.claude/projects` | JSONL | `env_path(…).or_else(…)` |
| opencode | `$OPENCODE_DATA_DIR` → `~/.local/share/opencode/opencode.db` | SQLite | `db.exists()` |

Those parsers already walk **exactly the records that carry the message text** and then throw it
away. `parse_claude_projects` reads `v["message"]` and takes only `usage` and `model`
(`usage.rs:963-969`). `parse_codex_sessions` filters lines on `"user_message"` / `"agent_message"`
and then descends to `/payload/message` for `usage` alone (`:1058-1125`). `parse_opencode_db`
selects `data` out of the `message` table and reads token counts from it (`:1214-1220`). The content
is in hand at every one of those sites and discarded one field short.

So this epic is not a data-acquisition problem. It is three narrower ones: **the parsers are trapped
in a binary crate**, **the model is token-shaped rather than message-shaped**, and — the part that
deserves the design attention — **the data is a category of input flux has never ingested before.**

## The part that is genuinely new: this is untrusted, out-of-jail, secret-bearing text

Every existing datasource ingests something the operator pointed at deliberately: a markdown tree, an
OpenAPI file, a page the agent just fetched. Harness history is different on three axes at once, and
all three land on the same `<knowledge-base>` block in the system prompt.

1. **It is outside the workspace jail.** `~/.claude/projects` holds every project the user has ever
   run the harness in — other repositories, other employers, other clients. A flux session in
   `~/work/repo-a` searching harness history can surface content from `~/work/repo-b`. That is
   sometimes exactly the point ("how did I solve this last month?") and sometimes a leak. It must be
   a decision the operator makes, not a default they discover.

2. **It is secret-bearing by construction.** Conversation logs are where credentials go to be pasted.
   flux's own redactor exists for text crossing to a log or a model, and A-21 escaped
   `<knowledge-base>` bodies precisely because injected text reaches the model. Both apply here with
   more force than to any current source: a `.env` dump in a year-old transcript is *more* likely
   than in a fetched web page.

3. **It is adversarial-input-shaped.** A transcript contains, verbatim, whatever any prior model or
   any prior *user* typed — including text that reads as instructions. Ingesting it into the prompt
   is a prompt-injection surface, and it is one an attacker can pre-load: get a string into any
   harness's log on that machine once, and it is retrievable forever after.

**Design consequence:** the datasource ships **off by default**, behind explicit opt-in; every
ingested body is escaped exactly as A-21 escapes knowledge-base bodies; every record is passed
through the shared redactor at ingest rather than at render; and the op declares a permission subject
naming the harness so policy can allow `flux` and deny the rest. None of that is a nice-to-have that
a later hardening story adds — it is C-215's acceptance, because the story that exposes the data is
the story that must contain it.

## Shape

**One new module tree in `flux-capabilities` (L5), no new crate.** It already owns the datasource,
already depends on `rusqlite`, and sits above `flux-events` (L2), which the flux-native adapter
needs. The repo's standing preference is modules over new crates; nothing here argues for an
exception.

```
flux-capabilities/src/harness/
  mod.rs        HarnessKind, discovery (roots + env overrides), the scan budget
  codex.rs      JSONL  → HarnessMessage
  claude.rs     JSONL  → HarnessMessage
  opencode.rs   SQLite → HarnessMessage
  flux.rs       EventStore → HarnessMessage
```

`flux usage` keeps its own token-shaped projection and moves onto the shared discovery + iteration
layer, so there is exactly one place that knows where `~/.codex` lives.

**The record projection**, onto the existing `Record { entity, id, source, title, body, links, meta }`:

- `source` — `harness`
- `entity` — `harness.message` (a single message) and `harness.session` (the session envelope)
- `id` — `<harness>/<session-id>/<message-index>`, stable across re-scans
- `title` — harness, workspace, and timestamp: enough to judge a hit without opening it
- `body` — the message text, **redacted and escaped**
- `meta` — `{harness, session_id, role, model, workspace, ts_ms, path}`
- `links` — message → its session

`harness` is therefore expressible as an ordinary `search` filter, but a bare `source: "harness"`
cannot select *within* it, so the op gains an explicit `harness` field that lowers onto an entity/meta
filter. Making it explicit is deliberate: `harness=opencode` is the query users will actually type,
and it should not require knowing the record schema.

## The scan budget is a correctness property, not a performance one

`flux usage` already carries `MAX_JSONL_FILES = 20_000` and `MAX_JSONL_FILE_BYTES = 200 MiB`, and
degrades by *skipping and counting* rather than failing. Message-level extraction multiplies the
output per file by one to three orders of magnitude — a token record is per-turn, a message record is
per-message and carries its full text. Ingesting a decade of transcripts into a datasource index must
not be the way a user discovers there is no bound. The budget is inherited, tightened for the body
case, and the skip count is reported rather than swallowed.

## Non-goals

- **No embeddings / semantic search in this epic.** Keyword search over the existing backend first;
  the datasource already has a semantic path, and turning it on is a separate decision with its own
  cost profile.
- **No writing to another harness's state, ever.** Every adapter opens read-only — the opencode
  adapter already passes `SQLITE_OPEN_READ_ONLY` and that is the standing rule, not an accident.
- **No live tailing.** Scan on demand; a watch/incremental mode is a later story if the scan cost
  turns out to justify it.
- **No cross-machine sync.** Local state only.

## Sequence

**C-213 → C-214 → C-215 → C-216.** Strictly ordered — each consumes the previous one's surface.

- **C-213** extracts discovery + iteration into `flux-capabilities`, leaving `flux usage` behaviourally
  identical. Pure refactor; its 12 existing tests are the pin.
- **C-214** adds the message-shaped model and per-harness extraction, with the scan budget.
- **C-215** projects onto `Record`, registers the source, adds the `harness` selector — and carries
  the whole safety envelope: opt-in, permission subject, escaping, redaction.
- **C-216** hardens what C-215 established: a redaction corpus over real transcript shapes, and proof
  that an opted-out flux never touches another harness's files.

Done looks like: `search(query: …, harness: "opencode")` returning a redacted, escaped, addressable
message from a real opencode database, with a test that a disabled datasource performs **zero** reads
outside the workspace.

## What C-215 settled (the seams C-216 tests)

**The datasource seam, not a new builtin.** `search` already exists as a datasource-pack op
(`flux-capabilities/src/datasource/ops.rs`) and the ask is a *filter on it*, so the change is a field
on that op rather than a second retrieval verb the model has to learn. `LiveDatasource` was the other
candidate and is the wrong shape: it is the async system-of-record contract for a backend flux queries
live and never snapshots, whereas harness history is exactly the indexed, ingest-once case
`DatasourceBackend` is for — and ingest is where redaction has to happen. Nothing was registered in
`register_builtins`, so the builtin catalog is unchanged; the two ops-reference mirrors are updated
because `search`'s signature gained a field.

**Off is a different declaration, not a runtime branch.** `HarnessHistory::disabled()` is the default
and `datasource_tools` *is* the disabled case, so a host that never opted in gets a `search` whose
schema and permission subjects are byte-identical to the pre-C-215 op. There is no code path where an
un-opted-in flux advertises a `harness` field or demands a harness subject.

**The opt-out is observable, not inferred.** `ingest_harness_history` returns before resolving a
single path when disabled, and reports `HarnessIngestReport::roots_opened()` when enabled. Recording a
root and opening it are the same call (`open_root`), which is what makes "no candidate root was
opened" an assertion about behaviour rather than about an empty result set. **This is the seam C-216's
opt-out audit extends** to every discovery branch, the env-override paths included.

**Containment is one function.** `contain()` is the only place harness text becomes stored text:
`Redactor::redact` first, then `flux_core::escape_knowledge_base_body` — A-21's own escaper, exported
rather than reimplemented, so the two cannot drift. The order matters: the redactor tokenizes on
delimiters that include `<` and `>`, so it must see the text as written. Containment is idempotent, so
re-rendering an already-contained body through `render_knowledge_blocks` is a no-op.

**Subjects: omitted means all.** An omitted `harness` demands *every* enabled harness's subject, so a
policy denying `datasource:harness.opencode` cannot be bypassed by leaving the field out. Only
`HarnessKind::id` values ever become subjects, so no `*` can be injected through the field; an
unresolvable value errors rather than widening to an all-harness search.

**Known gaps, deliberately left to their own stories.**

- **The flux-native adapter is not here** — it is C-302. Enabling `HarnessKind::Flux` opens no root and
  is reported in `HarnessIngestReport::unsupported()` rather than looking like an empty history.
- **The `harness` filter is a post-filter, and its over-fetch is a heuristic with a known failure
  mode — not a bound.** The index backends filter natively on `source`/`entity` only, and `harness`
  is a within-source distinction by construction (a `source` cannot select within itself — the reason
  the field exists). A filtered search pins `source: "harness"` natively, resolves the caller's
  `limit` (defaulting to 5 *before* widening, or the common no-`limit` call widens by nothing), and
  over-fetches 8×.

  That covers hits spread roughly evenly across the ≤4 enabled harnesses. **It does not cover rank
  skew**: if one harness holds more than `8 × limit` better-scoring hits than the selected one, the
  selected harness's rows are ranked out before the filter sees them and the op under-returns
  silently. Nothing enforces an even distribution. Removing the failure mode means pushing a `meta`
  predicate down into `DatasourceBackend`, which touches all four backends; C-215 recorded that as
  deliberately out of its blast radius, not as solved.

- **Ingest and advertisement are separately configured.** `ingest_harness_history` and
  `datasource_tools_with_history` each take their own `&HarnessHistory`. A host that ingests enabled
  but registers the pack disabled puts harness records in an index whose `search`/`list`/`get` demand
  only `datasource:*/*`, bypassing the per-harness subject. The op cannot detect this — it never sees
  the index's provenance — so **pairing them is a host-wiring obligation**, pinned by
  `the_pack_must_be_registered_with_the_same_history_that_was_ingested`. There is no in-tree host
  wiring yet; the first one should take a single `HarnessHistory` and do both.
- **The redactor's under-match is not yet measured.** That is C-216's third acceptance item, and its
  answer belongs in this document.
