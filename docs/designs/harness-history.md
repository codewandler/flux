# Cross-harness session history as a datasource

**Epic:** C-212 · **Stories:** C-213 → C-216, C-316 · **Status:** C-213 → C-216 landed; C-316 (the
envelope bound the corpus asked for) implemented

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

## What C-216 measured, and what C-315 closed (the redactor's under-match, and the recourse)

The redactor is a lossy heuristic by design. C-216 measured how lossy, over the corpus in
`crates/flux-capabilities/tests/harness_redaction_corpus.rs`, rather than assuming it away; C-315
closed the six shapes that measurement named. Every entry below is asserted in both directions by
`the_measured_under_match_is_exactly_the_list_the_design_records`, so **this table cannot rot in
either direction** — a widened redactor fails the test just as a narrowed one does.

### The mechanisms (C-315)

C-216 measured a *prefix list*. That is now one of four mechanisms, because the shapes it missed do
not have a common prefix to add:

1. **Registered values**, matched by substring, longest-first. The operator's recourse.
2. **PEM private-key blocks** — the body between `-----BEGIN … PRIVATE KEY-----` and its `-----END`
   collapses to one `[redacted]` line. The **delimiters are kept**: they are not secret, and they
   are the only thing that makes the redaction legible. Scoped to `PRIVATE KEY`, so a certificate or
   a public key is untouched. An **unterminated** block is redacted to the end of the input — that
   is the shape of a key truncated by `flux-system`'s output byte cap, and there is no reading of
   its remaining bytes under which they are safe to show.
3. **URL credentials** — the password in a `scheme://user:password@host` authority, bounded by the
   *last* `@`. This is structural, not heuristic: userinfo containing a colon **is** a credential by
   the URL grammar, so the false-positive rate is zero by construction. Only the password goes;
   which database, as whom, is what an operator reads a connection string for.
4. **The token pass** — a maximal run of non-boundary characters is redacted when either
   - it starts with a known credential prefix at or above **that prefix's own length floor**, or
   - it is the **value of an assignment whose name declares it a secret** and its own shape is
     opaque material.

**Prefixes and their floors.** `sk-ant-` 8, `sk-` 8, `sk_live_` 20, `xoxb/xoxp/xoxe-` 8, `ghp_` 8,
`gho_` 8, `github_pat_` 12, `glpat-` 20, `hf_` 30, `AKIA` 8, `AIza` 8, `ya29.` 8, `eyJ` 8 — the last
meaning a JWT *and* any base64 blob whose decoded content is JSON. The floors are per-prefix because
the prefixes are not equally distinctive: `hf_` is three characters and `hf_hub_download` is an
ordinary identifier, so its floor sits just under a real 37-character Hugging Face token.
Deliberately **absent**: `sk_test_` — a Stripe *test* key is not production credential material and
C-216 did not measure it.

**The contextual rule, and its guard rails.** The name (lower-cased, ≤ 64 chars) must contain one of
`secret token password passwd apikey api_key access_key private_key credential`; the value must be
≥ 16 characters, drawn only from `[A-Za-z0-9+/_-]`, and contain **both** a letter and a digit. Each
condition buys back a class of false positive: excluding `.` and `:` keeps hostnames, URLs, versions
and paths-with-extensions out; requiring a digit keeps `TOKEN_PATH=/etc/flux/credentials` out;
requiring a letter keeps `secret_ttl=3600` out; the length floor keeps `SECRET_NAME=my-app-config`
out. The rule reads `=` only — `key: value` is out, because `:` introduces far more prose than it
does credentials.

**Entropy scoring was considered and rejected.** It is the only mechanism that would catch a bare
40-character AWS secret with nothing naming it, and it cannot distinguish that from a git SHA, a
checksum, a UUID, a base64 PNG or a minified asset — all of which this corpus contains and asserts
must survive verbatim. `Redactor` is the shared redaction path for the stream-json writer, the
whatif cassette, the approval sheet, the evidence flush and harness ingest, so a false positive
silently destroys information on every one of them at once. Context is cheaper than entropy and its
failures are false *negatives*, which is the direction to fail in when the alternative is censoring
the operator's own diff.

### Caught, in this corpus

`sk-ant-…`, `xoxb…`, `ghp_…`, `AKIA…`, `AIza…`, `eyJ…` (C-216), plus the six C-216 measured as
missed and C-315 closed:

| shape | what catches it now |
| --- | --- |
| an AWS **secret** access key (`wJalr…`, 40 chars) | the assignment that names it (`AWS_SECRET_ACCESS_KEY=…`) |
| a password inside a connection URL (`postgres://user:pw@host`) | the URL pass — structural, not heuristic |
| a Stripe secret key (`sk_live_…`) | prefix, floor 20 |
| a Hugging Face token (`hf_…`) | prefix, floor 30 |
| a GitLab PAT (`glpat-…`) | prefix, floor 20 |
| PEM private-key material | the block pass; body redacted, delimiters kept |

### Not caught, in this corpus — the residual gaps, by decision

| shape | why it is left | 
| --- | --- |
| a secret-named assignment below the opaque-material floor (`REDIS_PASSWORD=c216corpusPw`) | ≥ 16 chars is what keeps `SECRET_NAME=my-app-config` intact; a short password is the price |
| a bare high-entropy token with nothing naming it (`wJalr…` in prose) | only entropy would reach it, and entropy flags hashes, diffs and image blobs |
| a secret-named binding in `key: value` form | `:` introduces prose far more often than credentials |
| an all-digit credential (`ACCOUNT_SECRET_ID=216216216216216218`) | no prefix can mark it and the contextual rule requires a letter, so `secret_ttl=3600` survives |

### The operator's recourse, in order

1. **Register the value** — `Redactor::try_add_secret(value)` catches every shape in the residual
   table above (asserted). This is the right answer for a credential the host already holds.
2. **Leave the datasource off.** It is off by default and off means off; see the opt-out audit.

The limit of recourse 1 is still measured, and since C-315 it is no longer silent: values under
`MIN_REGISTERED_SECRET_LEN` (6) are **declined with an error the caller can see**
(`Unregistered::TooShort`), because a security-registration call that reports success for something
it did not do is its own defect. The floor itself stays — registered values are matched by plain
substring, so registering `"abc"` would turn every `abc` in every diff into `[redacted]`.
`add_secret` remains as the infallible form for callers that have already established the length
(chiefly tests); `codewandler-flux-secret` is a published 1.x protocol-line crate, so the fallible
form was added beside it rather than replacing it.

**The all-digit shape is where recourse 1 is load-bearing**, and it is the reason to treat
"registration is total" as an invariant: it is the *only* mechanism that reaches a numeric
credential. Any redaction path that narrows where a registered value is matched — for example one
that walks a JSON document and skips `Value::Number` on the reasoning that a number cannot be a
secret — is a hole in that guarantee rather than an optimization. C-315 measured the shape but did
not audit the JSON walkers; that is filed separately.

### What the corpus also found, and did not fix

- **Only claude-code surfaces tool output.** codex files it as a `function_call_output` response
  item that carries no `role`, so the adapter's prefilter never parses the line; opencode files it
  as a `tool` part whose output sits under `state`, which the flattener renders as a bare
  `[tool_use: …]` marker. Containment is unaffected — dropping is containment — but *coverage* is
  asymmetric, and a reader would otherwise assume all three behave alike. Pinned by
  `no_adapter_but_claude_code_surfaces_tool_output`.
- **No adapter surfaces a tool call's input**, so a credential passed as a tool argument is never
  indexed. Same shape of finding: containment by omission.
- **Session-envelope retention was bounded by the schema, not by the code** — *fixed in C-316,* see
  below. `ingest_harness_history` held one `SessionEnvelope` per session for the whole scan on the
  reasoning that sessions are three to five orders of magnitude rarer than messages. That ratio is a
  property of the *harness schema*: an opencode database with no `session_id` column and no
  `sessionID` in `message.data` falls back to the message's own id, and envelopes then scaled
  one-for-one with messages. The scan budget did not bound this retention.
- **`meta`'s string values are redacted but not escaped** (`workspace`, `model`, `path`), where the
  body, title and id are both — *closed in C-316:* every **transcript-derived** string in `meta` now
  goes through `contain`. `harness` and `role` stay exempt, and the reason is recorded at the
  definition rather than left to be rediscovered — see below.

## C-316 — the bound, and what it does not bound

The retention above is now capped inside ingest by `MAX_LIVE_SESSION_ENVELOPES` (4096), so no schema
— degenerate, drifted or hostile — can make the live envelope set scale with message count.

**At the cap an envelope is flushed, not dropped and not refused.** The oldest live envelope is
projected, handed to the backend and let go. Refusing (erroring the scan) would let one unusual
database deny the whole index, and partial recall is this datasource's value; dropping would leave a
session unsearchable and dangle the message→session link every one of its messages carries, silently.
Flushing keeps every session addressable and costs one thing, stated rather than hidden: a session
whose messages straddle an eviction is projected twice and the later projection wins, so its
`messages` count becomes a lower bound. `HarnessIngestReport::sessions_evicted` reports that this
could have happened. Two alternatives were rejected — reading the flushed record back to resume its
count (one backend round trip per new session, i.e. per *message* in the degenerate schema the cap
exists for) and LRU instead of FIFO eviction (in the schema that actually evicts every session holds
one message, so arrival order *is* completion order).

Message records and evicted envelopes now share one outgoing buffer, so peak *record* retention stays
one `UPSERT_BATCH` rather than two summed, and an envelope leaves memory on the same flush as the
messages around it.

**Two things the cap deliberately does not bound, both stated by tests rather than assumed away:**

- **The bytes in one envelope.** `session_id`, `workspace` and `path` are transcript-derived and
  bounded only by the adapter's `max_line_bytes` (8 MiB). The cap is on the *number* of envelopes.
  The same observation applies to record ids, which are model-visible via `render_match` and are not
  length-bounded either; that is a separate story.
- **The adapters' own `session id -> ordinal` maps** (`harness/opencode.rs` and the two JSONL
  adapters), which are keyed by exactly the identifier this schema degenerates and so still grow one
  entry per message on it. Bounding them trades a memory bound for silently colliding record ids — an
  evicted ordinal restarts at 0 — so it belongs in its own story with C-214's blast radius. Stated by
  `the_adapter_ordinal_map_is_a_retention_this_cap_does_not_cover`.

Proofs: `session_envelope_retention_does_not_scale_with_message_count` measures peak live retention
**from outside**, by replaying the upsert stream, and states the property without naming the constant
(doubling the messages must not move the peak);
`session_envelope_retention_is_bounded_by_ingest_not_by_the_harness_schema` is C-216's ratio test,
rewritten to assert the bound and to overflow it by 900 sessions.

On `meta`: the attribute surface was never the hazard — `flux_core`'s `open_tag` `attr_escape`s every
value it writes, and `escape_knowledge_base_body` is not an attribute escaper — so what `contain` on
`meta` buys is the *body* surface, for a future renderer that prints a meta value as text. The comment
in `message_meta` that claimed `records_to_context_blocks` renders record `meta` as tag attributes was
wrong (it builds its own `{source, entity}`) and is corrected.

**`harness` and `role` are exempt, deliberately, and the exemption is narrower than the tidier rule.**
Containing every meta string uniformly is the rule that reads better and it is wrong here. Both are
`HarnessKind`/`MessageRole` ids — this crate's own closed enums, never a byte of transcript — so there
is nothing in them to contain; and `harness` is the key the selector lowers onto (`record_is_from`
compares it to `HarnessKind::id`). Running it through the redactor would make a filter's correctness
depend on the operator's secret list: register a value that occurs inside a harness id and every
record of that harness gets `meta.harness = "[redacted]"`, after which `search(harness: …)` answers
"no matches" over an index that holds the rows. The failure direction is under-return rather than
leakage, so nothing else would have caught it —
`the_harness_id_in_meta_is_exempt_from_containment_because_it_is_the_filters_key` pins both halves:
the harness id survives a redactor holding it, and a transcript-derived meta value carrying the same
substring does not.
