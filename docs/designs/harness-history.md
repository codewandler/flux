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
- **The redactor's under-match is measured — see the section below.**

## What C-216 measured (the redactor's under-match, and the recourse)

The redactor is a lossy heuristic by design: a fixed prefix list plus registered values matched by
substring, with a 6-character registration floor. On a log line that is an honest trade. On years of
conversation it under-matches, and the corpus in
`crates/flux-capabilities/tests/harness_redaction_corpus.rs` measures by how much rather than
assuming it away. Every entry below is asserted in both directions by
`the_measured_under_match_is_exactly_the_list_the_design_records`, so **this table cannot rot in
either direction** — a widened redactor fails the test just as a narrowed one does.

**Caught** (the prefix list, tokenized on whitespace *and* `" ' \` ( ) [ ] { } , ; = : < >`, with a
leading `+ - * #` set aside): `sk-ant-…`, `sk-…`, `xoxb/xoxp/xoxe-…`, `ghp_…`, `gho_…`,
`github_pat_…`, `AKIA…`, `AIza…`, `ya29.…`, and `eyJ…` — which means a JWT *and* any base64 blob
whose decoded content is JSON, since that is what `eyJ` is. A credential inside a `tool_result`, a
heredoc or a diff hunk is caught exactly as one in prose is.

**Not caught, in this corpus:**

| shape | why the prefix list misses it |
| --- | --- |
| an AWS **secret** access key (`wJalr…`, 40 chars) | no prefix at all — only the *access key id* (`AKIA…`) has one |
| a password inside a connection URL (`postgres://user:pw@host`) | `:` is a boundary, so the password is its own unprefixed token |
| a Stripe secret key (`sk_live_…`) | the list has `sk-`, with a hyphen; `sk_` does not match |
| a Hugging Face token (`hf_…`) | not on the list |
| a GitLab PAT (`glpat-…`) | not on the list |
| PEM private-key material | the `-----BEGIN …-----` delimiters are prose and the body is unprefixed base64 |

Two of these are worth naming as the sharp edge: **`sk_live_…` and PEM material**, because a
transcript in which an agent *writes* a production config is exactly where they appear, and the
heredoc corpus case is that transcript.

**The operator's recourse, in order.**

1. **Register the value** — `Redactor::add_secret(value)` catches every shape in the table above
   (asserted). This is the right answer for a credential the host already holds.
2. **Leave the datasource off.** It is off by default and off means off; see the opt-out audit.

The limit of recourse 1 is also measured: `add_secret` **silently ignores values under 6
characters**, so a short credential has no recourse but recourse 2.

**Widening the redactor is deliberately out of C-216's scope.** `flux-secret` is shared by the
stream-json writer, the whatif cassette, the approval sheet and the evidence flush; widening its
matching changes all of them at once, so it needs its own story and its own blast radius.

### What the corpus also found, and did not fix

- **Only claude-code surfaces tool output.** codex files it as a `function_call_output` response
  item that carries no `role`, so the adapter's prefilter never parses the line; opencode files it
  as a `tool` part whose output sits under `state`, which the flattener renders as a bare
  `[tool_use: …]` marker. Containment is unaffected — dropping is containment — but *coverage* is
  asymmetric, and a reader would otherwise assume all three behave alike. Pinned by
  `no_adapter_but_claude_code_surfaces_tool_output`.
- **No adapter surfaces a tool call's input**, so a credential passed as a tool argument is never
  indexed. Same shape of finding: containment by omission.
- **Session-envelope retention is bounded by the schema, not by the code.** `ingest_harness_history`
  holds one `SessionEnvelope` per session for the whole scan on the reasoning that sessions are three
  to five orders of magnitude rarer than messages. That ratio is a property of the *harness schema*:
  an opencode database with no `session_id` column and no `sessionID` in `message.data` falls back to
  the message's own id, and envelopes then scale one-for-one with messages —
  `session_envelope_retention_is_bounded_by_sessions_only_when_the_schema_has_them` states the ratio.
  The scan budget does not bound this retention. Left as a finding rather than fixed: the fix is a
  bound inside ingest, which is C-215's blast radius, not C-216's.
- **`meta`'s string values are redacted but not escaped** (`workspace`, `model`, `path`), where the
  body, title and id are both. Nothing model-visible renders record `meta` today —
  `records_to_context_blocks` writes only `source`/`entity` as tag attributes, and
  `render_match`/`render_record` print id, title and body — so this is latent rather than live. A
  future renderer that prints `meta` would need `contain` applied there too.
