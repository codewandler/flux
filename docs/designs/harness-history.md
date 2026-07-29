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
