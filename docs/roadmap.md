# flux — roadmap & status

Status as of **0.48.0 (2026-08-01)**: public + installable at
[codewandler/flux](https://github.com/codewandler/flux) and published to crates.io
(`codewandler-flux-*`); 37 root-workspace crates plus the `plugins/` pack, **2700+ tests** across
both workspaces, a permanently green
gate (tests, clippy `-D warnings`, fmt, the `flux-codegate` layering lint). See
[CHANGELOG.md](../CHANGELOG.md) for the released history and [architecture.md](architecture.md) for the
design.

## Delivered

The build proceeded breadth-first (every surface exists as a crate) and was then hardened in depth.

**Foundations & breadth (M0–M5)** — the workspace + layering lint; the content/message/streaming
model; the provider layer (wire codec × credential; five providers; credential store with PKCE login
and CLI-credential import; `provider/model` routing); the guarded IO boundary and the mandatory safety
envelope; built-in tools; SQLite sessions; the context projector; skills; markdown roles; multi-agent
orchestration; JS hooks; subprocess plugins; the SDK, HTTP server, integrations, browser/web egress,
datasource/RAG, evidence, and the OIDC identity seam.

**Hardening (M6–M9)** — provider retry/backoff; config loading + persistence; the authorization
policy wired into the envelope (default-deny + a usable local default); real secret redaction;
evidence + destructive-op escalation; capability & integration depth (`glob`/`grep`, `web_fetch`,
`search`, plugins-as-tools with host-capability callbacks, plugin lifecycle, skill activation,
policy-bounded sub-agents); streaming everywhere (CLI/TUI tokens, server SSE, in-TUI approval modal);
cancellation; autopilot (`/pd` dependency waves, `/goal`, `/loop`); context compaction; the layering
lint; CI; Anthropic prompt caching; the OIDC claims→identity seam.

**Review remediation** — two adversarial review passes were run against the hardened code and every
confirmed finding fixed with a regression test:
- *Post-M8/M9 review (R1–R8)* — session-shape breakers (empty-assistant-on-cancel, compaction
  splitting a tool_use/tool_result pair), uninterruptible autopilot, and CI/cache nits.
- *Full-tree security review (0.1.1)* — sandbox-escape, plugin-capability, server-auth, env-leak,
  policy-approval, SSRF, redaction, OAuth-state, and a batch of panic/DoS/correctness fixes. See the
  `[0.1.1]` CHANGELOG entry for the itemized list.

**Daily-driver readiness (0.2.0)** — repo-aware context (git working-tree + project-shape context
providers), a real reedline REPL (line editing, persistent history, reverse-search, visible thinking),
a whitespace-tolerant `edit` tool, `flux sessions` + `/resume`, mid-session `/model` switching, and a
live-provider smoke gate (`scripts/smoke-live.sh`). Validated end-to-end against a real provider.

**Public release (0.2.1)** — flux is open-source (MIT OR Apache-2.0) and installable at
`codewandler/flux`: dual-license files + CONTRIBUTING/SECURITY + issue/PR templates; a cargo-dist
release pipeline producing prebuilt binaries for all five targets + shell/PowerShell installers on every
tagged release; CI running the full gate on every push.

## Standing pre-release gate (do this before every release)

A **live-provider smoke test** is the manual gate that the offline mock can't replace (the mock
doesn't enforce provider message-shape rules — which is exactly how the session-shape breakers
slipped through). With a real key (e.g. `anthropic/opus`), exercise:
- a one-shot (`flux run -p`),
- an agentic file edit under the envelope (`flux run --yes`, scratch workspace),
- a multi-turn `--continue` that replays tool-call history,
- a compaction-then-continue past a tiny `FLUX_COMPACT_CHARS` (validates no 400 on the rewritten log),
- (semi-manual) a Ctrl-C mid-turn in the REPL, then a follow-up turn in the same session.

This is scripted as `scripts/smoke-live.sh` (model overridable via `FLUX_SMOKE_MODEL`) — run it
before every release. It also carries **subscription-provider legs** (C-19): one tiny `claude` and
one tiny `codex` turn, each SKIPped when the credential is absent — and the codex leg runs under
`FLUX_TRANSPORT_DEBUG=1` so a WebSocket-contract regression **fails loudly** instead of hiding
behind the transparent HTTP fallback (the C-07 lesson: live wire-contract drift is invisible to
hermetic stubs *and* to a fallback that works).

Because the manual gate only runs before a release, a CLI-surface change (a renamed subcommand, a
dropped flag) could otherwise rot it silently between runs (C-39). CI therefore also runs
`scripts/smoke-live.sh --shapes`: the same steps 1-5 invocation shapes replayed against the offline
`mock` provider in scratch dirs — no credentials, no live spend — failing fast on a clap parse error
instead of waiting for the next live run to notice.

A second, **integration-plugin** smoke (`scripts/smoke-plugins.sh`) exercises the D-08 plugin pack against
real vendor APIs: for each integration whose credential is in the environment it builds the plugin,
registers it in an isolated registry, and drives one op via `flux plugin call`, asserting a non-error
result; plugins whose key is absent are **skipped** (not failed). Run it (with whatever keys you have —
`TAVILY_API_KEY`, `GITLAB_PERSONAL_TOKEN`, `SLACK_BOT_TOKEN`, …) before releasing anything touching the
plugins. The semantic/embeddings path (`--features embeddings`) is validated manually with a feature build
(`FLUX_EMBEDDINGS_API_KEY`); its rerank logic is covered by the default-build unit test.

## Next

> The entries below are the epic log, newest first, each stamped with its status. Everything through
> **v0.38.0** is released and `[Unreleased]` is empty. That cut carried the **adversarial review
> remediation** and **Zendesk automation** epics below, plus the earlier tail (C-217, C-218, C-226,
> C-233, C-234, C-240, C-246, C-247, C-251 partial, C-252). See [CHANGELOG.md](../CHANGELOG.md) for
> the itemized history.

### Flux syntax simplification — one way to write each thing (epic) — 🔄 **PROPOSED (L-102; L-103…L-112 filed, none started)**

The canonical dialect the formatter emits is already the language we want — but it is one of
several dialects the parser accepts, the docs teach both, and the flagship corpus (agent-loop.flux,
every example) is written in the legacy one. Nine doubled spelling dimensions tax the parser, four
editor grammars, and every model prior; the pinned tree-sitter grammar cannot even parse the
canonical spellings today. This epic simplifies by subtraction: ship the missing `fluxlang fmt`
migration tool (L-103), move the corpus and spec to one dialect (L-104/L-105), then deprecate and
remove the legacy grammar (L-106/L-107), with targeted unifications behind it (one `key: value`
vocabulary, a closed pure-builtin namespace, lit/template unification, match/when ergonomics —
L-108…L-112). Done means: one way to write each construct, `fmt --check` in the gate, and a
smaller grammar in every mirror. It also re-scopes the notation workbench — Tape (L-98) and S-Flux
(L-99) are deprioritized in its favour. Design:
[flux-syntax-simplification](designs/flux-syntax-simplification.md).

### flux-lang hardening — remediate the 2026-08-01 subsystem review (epic) — 🔄 **PROPOSED (L-113; L-114…L-120 filed, none started)**

An adversarial subsystem review of flux-lang
([2026-08-01](reviews/single/2026-08-01-flux-lang-subsystem-review.md), 6/10) falsified the
crate's two headline totality claims: the parser SIGABRTs on ~200–900 levels of statement nesting
(the L-81 guard covers only expressions/types), and `each` string-splits its header on `->`,
rejecting legal programs and breaking round-trip totality — both reproduced, both the fourth
instance of the "guard tested against its own assumptions" pattern. Behind them: `repeat` lacks
the loop budget/yield discipline its own doc-comments promise, `confirm` approvals carry an empty
`IntentSet`, and the language surface has no raw-text fuzzing. One story per finding cluster
(L-114…L-120), severest first, each fix required to ship its test on the previously untested
axis. Done means the review's triage flips to `handled` with every finding owned. Design:
[flux-lang-hardening](designs/flux-lang-hardening.md).

### The execution substrate — `flux-system` for a second consumer (epic) — 🔄 **PROPOSED (C-394; C-395…C-399 filed, none started)**

`flux-system` has had exactly one consumer since it was written. That is about to stop being true:
[flux-exchange](designs/ecosystem.md) is a service that runs operations for many callers, and the
primitives it needs — a guarded request, a guarded dial, an argv-only spawn, an OS sandbox — are the
ones `flux-system` already holds.

**The response is not a new crate.** The seam already exists and was built for this: `port.rs` states
the guarded operations as capability traits so *"a WebAssembly embedder…, **a remote executor**, or a
test double"* can serve them, and it is explicit that the traits are unsealed. What is missing is
narrower than it looks.

The distinction the epic exists to protect, now stated publicly in
[`docs/concepts.md`](concepts.md): **`flux-runtime` decides whether something may happen;
`flux-system` is where it happens.** They are peers at L2, not stacked. Fusing them would force every
consumer of the substrate to take flux's approval model too — and a consumer with no human at a
terminal would reimplement guarded IO instead, which is the exact failure the substrate prevents.

Three gaps are real work. **C-395** makes the workspace-confined file surface a port; C-269 deferred
it on the stated grounds that *"a trait with no call sites would be indirection without a seam"*, and
a second consumer is precisely the condition that expires. **C-396** adds UDP and ICMP dial targets,
with the sharp requirement that an unheld `CAP_NET_RAW` refuses at *construction* — a capability check
that happens on the wire has already contacted the destination. **C-398** is the one most likely to be
got wrong quietly: `AGENTS.md` says *"Every tool runs through `Executor::dispatch`"*, which is true of
**flux** and reads as a claim about every consumer; nothing states which guarantees travel with
`flux-system` alone (path confinement, argv-only, egress guarding, sandbox, env clearing, output
capping) and which are `flux-runtime`'s and do not (default-deny authorization, approval, redaction,
evidence).

**C-397** (container backend) and **C-399** (remote port backend) are members but sit in `backlog`
with **ownership undecided** — the port is unsealed, so an out-of-repo consumer can implement either
without flux changing, an in-repo backend costs a reviewed codegate allowance, and flux's own CLI has
no use for either. Deciding that is their first acceptance criterion, deliberately.

The epic changes no layer, adds no IO path, weakens no default, and does not build flux-exchange.
Design: [execution-substrate.md](designs/execution-substrate.md).

### Explore, then freeze — ad-hoc browser testing that becomes a deterministic script (epic) — 🔄 **PROPOSED (C-430…C-434 filed, none started)**

Ask an agent to log into a site and test a module's happy path. It explores — misreads a label,
backtracks, finds the right button, succeeds — and today that exploration is thrown away. **The
valuable artifact is not the answer; it is the path that worked.** This epic makes flux able to keep
it: drop the trial and error, emit a script that runs in CI. It is the thesis in its most legible form
— the model explores, the runtime keeps the result — and an agent whose contract is its transcript can
only re-narrate what it did, because it has no artifact to hand you.

**Most of the machinery is already built.** `crates/flux-web/src/browser.rs` drives headless Chromium
over an in-repo CDP client (`browser.open · goto · act · snapshot · close`), spawned through the
guarded `spawn_debug_pipe` seam, with **every subrequest routed through the `web` egress guard via CDP
`Fetch` interception** and the ops evidence-gated behind a Chromium-discoverable signal. The page model
is semantic, not pixel: the digest is built purely from `Accessibility.getFullAXTree` — roles, names,
states — in document order, which its own header calls *"replay/`flux diff` friendly"*, and it is
testable against a scripted fake with **no Chrome in CI**. Since L-38 every accepted plan already
records parseable `plan_source`. What is missing is narrow: nothing turns a recorded *session* into a
saved *flow* — `flux flow` has `list` and `run`, no `save`. That is
[C-430](stories/C-430-distil-an-exploration-into-a-flow.md).

⚠ **Two stories are not polish; they are the two ways this fails.**
[C-431](stories/C-431-durable-locators.md): `RefMap` (`crates/flux-web/src/digest.rs:53-72`) keys on
`backendDOMNodeId` and assigns `next += 1` in first-encounter order **within one live session** — so
`e17` is stable while the agent explores and **means nothing in a fresh session**. A distiller that
emits refs produces scripts that break on the next deploy, which is exactly what made a generation of
record-replay tools disposable; freezing must re-anchor to the AX role and name the digest already
carries. [C-432](stories/C-432-browser-credentials-never-come-from-the-prompt.md): the motivating
phrasing — *"log in as X with password Y"* — is a leak. The `Redactor` redacts values it has been
**told about**; a password typed into a prompt was never registered, so nothing redacts it before it
reaches the model context, the event log and `plan_source`. A compelling recipe that teaches users to
paste production passwords into prompts would do more harm than the epic does good.

Then [C-433](stories/C-433-a-frozen-script-asserts.md) — a frozen click sequence proves only that the
clicks did not error, and a green suite that asserts nothing is worse than no suite because it is
trusted — and [C-434](stories/C-434-the-e2e-recipe.md), the worked recipe, filed last precisely because
it is the most demo-able thing here: a polished page over a distiller that emits brittle scripts would
be actively harmful. Design: [explore-then-freeze.md](designs/explore-then-freeze.md).

### flux recipes — real programs that make the difference click (epic) — 🔄 **PROPOSED (C-425…C-429 filed, none started)**

Someone evaluating flux sees a feature list and a folder of examples, and has no way to tell it is a
*different kind of thing*. The vision states the thesis — **the LLM is not the runtime** — but a thesis
is not a demonstration. This epic builds the demonstrations: real programs doing real work, each making
one guarantee visible enough that a reader can check it rather than believe it.

⚠ **The corpus we have is language samples, not recipes, and the gap is measured.** A keyword sweep of
the 16 files in `examples/` (2026-08-01) found **zero** examples using `agent_loop`, `await`,
`datasource`, `agent`, `checkpoint`, `memo`, `throttle`, `debounce`, `risk`, `try`/`catch`/`finally`,
`race` or `match`; `journey`, `trigger` and `channel` appear once each, all in `channels-app.flux`.
The durability and resilience vocabulary — the part that separates flux from a chat loop, and the part
you cannot appreciate from a grammar table — is almost entirely undemonstrated. A durable journey that
suspends on an event and resumes days later with no model re-spend needs a program that does it.
[C-428](stories/C-428-the-example-coverage-census.md) turns that into a repeatable census (and must
confirm the figures against the grammar rather than a grep — C-406's audit regex produced 319 phantom
findings, and this table is a lead, not evidence).

**The flagship is the tracking framework itself** ([C-425](stories/C-425-the-flagship-recipe-tracking-as-a-flux-app.md)).
`track` keeps exactly one deterministic component and puts every other invariant in markdown a model is
asked to honour — and we have first-person evidence of the drift rather than a hypothesis: C-406 found
epics carrying open work with no tracker, a story citing a `C-330` that was never filed, 185 stale
`priority` fields, and nine priority values shared by two or more `ready` stories so the rank does not
rank. Rebuilt as a flux app the split is the thesis made concrete — **the runtime owns the mechanical
half** (frontmatter validation, board regeneration, the epic audit, CHANGELOG sync) as authored flow
with declared bounds, **the model owns the semantic half** (writing the story, judging a duplicate).
Then [C-426](stories/C-426-the-determinism-proof.md) makes the claim checkable: run it twice, replay it
offline, diff it — separate from the flagship because an unverified determinism claim on a page arguing
*for* determinism is the worst available failure, and folded in it becomes a README sentence nobody
runs. ⚠ It must state which layer the claim covers: model-authored stages are not deterministic, the
*shape* of the run is.

[C-427](stories/C-427-the-recipe-contract.md) generalizes the contract **from** the flagship rather than
before it (a contract written with no recipe in hand is a guess), preserving the property that makes the
current corpus honest — `examples_validate.rs` sweeps the whole directory with no hand-picked list, and
a list is how a corpus rots. [C-429](stories/C-429-the-recipes-surface-and-positioning.md) is the page
the ask is really about, filed last because it is worthless without recipes underneath it. ⚠ **It names
no competitor** — claims about another system's internals cannot be verified from here, age into
misrepresentations, and are *weaker* than a command the reader can run. Every claim on it must be backed
by such a command; one unbacked claim discredits the true ones. Design:
[flux-recipes.md](designs/flux-recipes.md).

### Session screencast — render a recorded run as a terminal cast (epic) — 🔄 **PROPOSED (C-421…C-424 filed, none started)**

Demos, docs and blog posts need to *show* flux working, and today the only way is to point a screen
recorder at a live run and hope. Every asset is then a one-take performance: unreproducible when the
theme changes, impossible to make in CI. This epic makes a screencast a **render of a recording** —
run the task once, re-render as often as the layout or the narration needs.

⚠ **"Everything is recorded anyway" is half-true, and the half that isn't is the epic.** What is
there: `flux-events` rows carry `ts` at **millisecond** resolution, and `EventKind` holds the messages,
turns, plans and (since C-43) durably-redacted op output — content and timing are on disk, so nothing
new needs capturing. What is not: the TUI's on-screen surface is `UiEvent`, `pub(super)` and
**ephemeral**, and the existing durable→screen path (`crates/flux-tui/src/projection.rs`) is **100
lines handling five observation kinds against 26 live variants**. The data is largely there; the
projection is not. That 5-of-26 ratio is why [C-422](stories/C-422-the-render-projection.md) — the
render projection, and a committed *fidelity table* saying per variant whether a cast shows truth, an
approximation, or nothing — gates the renderer instead of being folded into it. Some variants are
genuinely unrecoverable (C-158's live tool tail, spinner frames, retry countdowns); a cast must not
invent them.

The other three: [C-421](stories/C-421-tui-takes-a-task-from-the-cli.md) gives `flux tui` a positional
prompt — it is the one verb you cannot hand a task, and a run that starts from a command line is a run
that can be re-recorded unattended; [C-423](stories/C-423-flux-cast.md) paints the timeline headless to
**asciicast v2** (text, so it diffs in review, and no image dependency — GIF/SVG stays `agg`'s job)
using the live TUI's own widgets, so a cast shows the product rather than a mock of it; and
[C-424](stories/C-424-a-cast-is-a-publishing-act.md) is the safety story, deliberately not folded into
the renderer. ⚠ **A cast's whole purpose is to leave the machine**, unlike every other Time Machine
verb, and redaction has failed *open* here before (C-339). Rendering is a second chance to leak — a
secret can be absent from a payload and present in a wrapped, ANSI-styled frame reassembled from it —
so the test must attack the rendered frames, and an unavailable redactor must refuse the cast rather
than write it.

This is the visual sibling of **Time Machine** (A-45 replay, A-46 fork, C-44 diff re-*run* a session;
this re-*shows* one, needing no execution at all) and follows **flux-render**'s precedent (L-74…L-78).
Design: [session-screencast.md](designs/session-screencast.md).

### Guarded network primitives — DNS · TCP · UDP · ICMP behind one egress decision (epic) — 🔄 **PROPOSED (C-418; C-284…C-288 filed, none started)**

`guard_url_scoped` resolves a hostname and decides on the **resolved address**, refusing private,
loopback, link-local, ULA, CGNAT and IPv4-mapped ranges unless the caller holds a scoped grant.
`AGENTS.md` names it a safety invariant and forbids hand-rolling a second URL guard. That invariant is
about **web egress**; this epic is what it means for everything else — name resolution, stream,
datagram and raw. C-284 fixes the shape the other four inherit, precisely so a second guard never
appears.

⚠ **It overlaps the execution substrate above, and that boundary was undocumented until now.** C-396
("UDP and ICMP dial targets") and C-287/C-288 ("a guarded UDP/ICMP operation") were filed into
different epics describing adjacent ground. The intended layering: **C-396 is the substrate
primitive** — the dial target and its guard integration in `flux-system`; **C-287/C-288 are the op
surface** on top, with intent declaration and per-reply checking. C-396 is the floor, and neither op
re-derives the guard. If an implementer finds that layering does not hold, that is a backlog problem
to raise rather than something to settle inside a diff. Tracker:
[C-418](stories/C-418-guarded-network-primitives-epic.md).

### The verified webhook channel — a delivery flux can prove the origin of (epic) — 🔄 **PROPOSED (C-419; C-292…C-295 filed, none started)**

The inbound counterpart to the epic above. Today an inbound webhook is a JSON body flux trusts
because it arrived: signature verification is per-vendor or absent, the envelope carries no id, no
source and no `verified`, and endpoint-verification challenges cost a whole agent turn. Four stories
make a delivery something flux can prove the origin of (C-292, one parameterized constant-time
replay-bounded HMAC — not one per vendor), route by (C-294, by event discriminator, without the agent
parsing the body to decide whether it cares), and hand on with provenance intact (C-295).

⚠ **Two neighbours arrived after those four were written**, and all three meet on one request path:
[C-409](stories/C-409-channel-served-http-has-no-resource-limits.md) — the webhook adapter binds its
own listener with **none** of `flux-server`'s body caps, timeouts, rate limits or concurrency
admission, since C-189 gave those to the server only; and
[C-416](stories/C-416-a-channel-adapter-should-declare-its-principal.md) — a webhook body's principal
is authenticated by nothing, and the adapter is the only component that knows it. The useful ordering
is **prove it (C-292) → carry it (C-295) → decide with it (C-416)**; implementing the three as
unrelated answers is the failure mode. Tracker:
[C-419](stories/C-419-verified-webhook-channel-epic.md).

### The connectors seam — a vendor credential flux is structurally unable to hold (epic) — ✅ **CORE SHIPPED (C-420; 7/8 done, C-405 open)**

Let an operator hand flux a *platform* rather than a *secret*: flux calls a connector, the connector
holds the vendor credential, and flux is structurally unable to receive one back. The invariant is
that **flux holds exactly one secret on this path — the deployment session bearer** — and a response
carrying credential-shaped material is *refused*, not merely redacted (C-312). Around it: an op set
that changes when the operator authenticates a provider, without restarting flux (C-310), and
vendor-host disclosure at approval (C-311).

⚠ **C-311 is a compensating control, not a fix, and this is the seam's one honest trade-off**: when
the platform dials the vendor, `guard_url_scoped` only ever sees `localhost:8000`, so flux's
per-vendor egress allowlist stops constraining which vendor is reached.

**Four of the eight stories were not planned scope** — C-403, C-404, C-410 and C-411 were each found
by a review *of the previous one*, and each found the same defect class: **a guard or a comment that
agrees with its own assumption.** C-312 stated its scope; C-403 found a live call site the scope did
not cover. C-404's carve-out was described as dormant; it was excusing a real dispatched op. C-410
found the surface that prints plugin-authored strings running outside the envelope C-404 presumed.
C-411 closed silent grant widening. A new ingest surface here does not inherit the boundary by being
near it — only by being routed through the check. C-405 (twelve private percent-encoders in the pack,
one already drifted) is the remaining open story and is a **protocol-line change owing a version
decision**. Tracker: [C-420](stories/C-420-connector-platform-epic.md).

### The road to stable — what must be true before flux is measured rather than built (epic) — 🔄 **PROPOSED (C-342; ~16 of 110 open stories block it, 9 owned + 6 cross-referenced)**

flux has 663 done stories and ~110 open, and the open set is not a queue of equal work: roughly 16
stories block a credible claim that flux is stable, and the other ~94 are capability that stability
does not depend on. This epic names those 16 so the distinction stops being re-derived, and so the
switch to **harness-driven** development (`flux-bench` runs driving improvement, regressions read off
benchmarks rather than found by reading code) happens on evidence rather than on feel.

The 2026-07-31 backlog analysis found the moment is close but not here. The backlog has flipped from
planning-driven to discovery-driven — 54% of stories C-301…340 originate from a review or an
implementor report, against ~1% for C-1…200, and **zero of the 20 newest stories are new capability**.
Defects are tractable and clustered: 17 of 85 non-epic open stories describe something a user can hit,
grouped in webhook delivery, the Flux-Lang grammar and its editor mirrors, and the redaction path —
where two stories fail *open*.

**"Done" is not a bug count.** The architecture is settled ([C-337](../designs/architectural-simplification.md)
says "preserve, do not redesign"), but the published API surface is not: C-337 records a scheduled
breaking window for `AgentSpec`, compatibility doors slated for deletion, and 37 crates with no
ownership audit — while carrying zero implementation stories. Benchmarking against an API with a
deliberate break still queued makes regressions indistinguishable from intended churn. So this epic
closes when the defect clusters close, C-337 is decomposed and its window scheduled, **and**
[C-255](../designs/adversarial-review-remediation-2026-07-30.md)'s final bullet is ticked — three
fresh independent reviews finding no reproducible High-severity containment defect. That bullet is the
repo's own definition of stable, and its first closure pass found twelve defects after every child was
marked done.

Design: [`docs/designs/road-to-stable.md`](designs/road-to-stable.md).

### Architectural simplification — fewer assembly paths, smaller modules, less compatibility debt (epic) — 🔄 **PROPOSED (C-337; implementation stories not yet filed)**

The L0–L6 architecture and guarded execution envelope are the part to preserve; the review found
complexity accumulating *inside* them. `ExecutionEnvironment` centralizes executor construction, but
surface assembly still grows positional inputs and optional invariant-bearing chains; deprecated
agent/runtime/app compatibility doors remain long after their stated removal release; and eight
implementation files now range from 3,014 to 9,789 lines. The workspace also grew from the 31 crates
recorded after the last consolidation to 37 members, without a current ownership/consumer audit.

The sequence is deliberately conservative: make execution-environment assembly typed and omission-
resistant; remove the expired compatibility paths in one planned minor release; split
`flux-runtime` and `flux-codegate` into internal modules before tackling the remaining large files;
re-audit crate ownership under the existing same-layer/published-boundary rules; design an `AgentSpec`
settings migration only for a deliberate breaking window; then archive delivered roadmap history.
No new crate is introduced to make a file look smaller, no published or deliberate L0 boundary is
merged merely to lower the count, and no module move changes behavior. Design:
[architectural-simplification.md](designs/architectural-simplification.md); tracker:
[C-337](stories/C-337-architectural-simplification-epic.md).

### Meeting rooms — a multi-party channel where humans and agents meet (epic) — 🔄 **PROPOSED (D-203; D-204…D-213 filed, none started; feasibility PROVEN live)**

Every channel flux has is 1:1 or fire-and-forget — `schedule`/`webhook`/`slack`/`a2a` wake a journey and
return, and the voice path assumed exactly one caller (before D-204, `VoiceTurnHandler::turn` had no
speaker parameter, because on a phone line there is only one candidate; it now carries a `Speaker`).
Until D-204 there was no channel where flux is **one participant
among several**: humans and agents co-present, the agent hearing everything but addressed by only some of
it, able to **show** something instead of only saying it. A meeting room is that shape, and — the framing
that motivated the epic — it is the *root* substrate in which **agents can meet**, which makes it fleet
infrastructure (A-111, A-119/A-120) rather than a voice curiosity.

**This epic is unusual in that its feasibility was measured before it was designed.** On 2026-07-30 a
spike joined a real Brave Talk room from a plain HTTP/WebSocket client and, over the session, reached
presence, bidirectional text, and audible speech — with the human in the call confirming each. Brave Talk
is 8x8's Jitsi-as-a-Service with a Brave token service in front, and its client is open source, which is
how the handshake was derived: `PUT /api/v1/rooms/<room>` hands an anonymous caller a **3-hour,
room-scoped JaaS JWT** (creating a room needs Premium; joining one needs *nothing*), then
`conference-request/v1` allocates focus, then XMPP-over-WebSocket with SASL `ANONYMOUS` joins the MUC.

The load-bearing finding is that **presence and text are pure XMPP** — no WebRTC, no browser — while audio
and screenshare need a browser-grade media stack. So the design splits the port: a native `Room` with
`XmppMucRoom` (portable, what CI runs) and `JaasRoom` (vendor token acquisition) backends, plus an
**optional feature-gated media sidecar**. Audio out is proven but every documented recipe for it failed —
Chrome 150 ignores `--use-fake-device-for-media-capture` entirely and Jitsi's `setAudioInputDevice` does
not stick; what worked was a private PipeWire null sink with the browser's *own* capture stream moved onto
it per-stream, leaving the human's microphone untouched. Screenshare remains **unproven** (headless has no
display for `getDisplayMedia`; the Xvfb fallback never loaded), and D-211 records why the next attempt
should drive `lib-jitsi-meet` with a canvas-sourced track instead of chasing desktop capture.

The hard part was never the plumbing. It is that a room has N speakers and the agent is the addressee of
almost none of it (D-207: an address rule, or the agent answers every sentence two humans say to each
other — observed within three messages in the spike), and that **a room link is effectively a credential**
(D-213: anyone holding it can put a listener in the call without an account, as the spike itself
demonstrated). D-213 owns the invariants — co-presence grants no authority, self-announcement is not
optional, and publishing audio or a screenshare into a room full of people is an approved, redacted act.

### A portable Flux runtime — WebAssembly as a second execution substrate (epic) — 🔄 **PROPOSED (C-268; C-269…C-273 filed, none started)**

Flux executes `.flux` on one substrate: a native process holding the OS's ambient authority, confined
*after the fact* by an OS sandbox and by the authorization → approval → guarded-IO envelope. That is
right for a flux the operator installed. It does not answer the case this epic exists for — **someone
else's `.flux`, submitted to us, executed by us** — where the only honest answers today are "a container
we manage" or "no".

A Wasm module has **no ambient authority at all**: no syscalls, no filesystem, no network, no clock,
unless the embedder hands it an import. That is the posture the plugin host constructs by policy, except
the runtime enforces it. The secondary prize is reach — the same module runs in a browser or an edge
worker, so running a program stops requiring installing flux.

**The decision is to port the interpreter, not to write a Flux-to-Wasm code generator.** Codegen means a
second implementation of Flux semantics — `retry`, `parallel`, budgets, approval gating — that must agree
with the first one forever, for no user-visible gain; and it pushes decisions into generated code, when
the point of flux is that the *runtime* decides.

**The load-bearing invariant is that the guard runs OUTSIDE the sandbox.** If the host exports
`fetch(url)` and the module is merely *expected* to guard it first, a submitted program declines to —
it controls its own control flow. So imports are narrow, already-decided operations (host resolves the
endpoint, applies `guard_url_scoped`, pins the vetted address, injects credentials the module never
sees), never raw primitives. **C-272** carries that as its central test: a module that does not call a
guard must still not escape one.

Two blockers were measured rather than assumed. **[C-269](stories/C-269-system-trait-seam.md)**:
`flux-system::System` is a concrete struct (`lib.rs:1077`), so nothing can substitute a non-syscall
backend — wide but shallow, since the method set already dictates the trait.
**[C-270](stories/C-270-engine-state-store-port.md)**: `flux-flow` binds `rusqlite`, which cannot build
for `wasm32` — but only in `src/state.rs`, **1 of 17 files**, so it is an extraction. `flux-lang` is
already **L0, no IO**, so the parser and AST are portable today.
Then **[C-271](stories/C-271-portable-core-wasm-parity.md)** proves parity against the *native* engine on
the same source, and **[C-273](stories/C-273-embedder-resource-limits.md)** bounds a run — because Wasm
constrains authority and **not** resources: an unbounded loop or allocation takes the embedder down
unless the host caps it.

This is defence in depth, not a replacement for C-262's fail-closed OS sandbox on the flux we run. The
design is deliberately published while still cheap to argue with: repo record
[portable-wasm-runtime.md](designs/portable-wasm-runtime.md), public page
`website/docs/direction/portable-wasm-runtime.md`.

### Adversarial review remediation (epic) — 🔄 **IN PROGRESS (C-255; C-256…C-265 all done, closure reviews pending)**

Three independent adversarial passes were run against `cb3bb057` on 2026-07-30 and recorded under
`docs/reviews/`. They rated the tree 5.5/10, 6/10 and 7/10, and — importantly — **all three rejected
flux as a standalone unattended boundary**. The spread is the work: the architecture was not the
finding, the *adapter-level exceptions to it* were.

Every actionable finding became one child story with a one-to-one evidence trail. The HIGH ones are
all the same shape — a guard produces an answer and the caller then discards it.
**[C-256](stories/C-256-pin-fleet-a2a-egress.md)** and
**[C-257](stories/C-257-pin-plugin-callback-egress.md)** pin fleet A2A, plugin HTTP/OAuth and plugin
TCP to the exact DNS answers `guard_url_scoped` admitted, disable ambient proxies and automatic
redirects, and re-authorize every supported redirect hop.
**[C-258](stories/C-258-make-eval-run-host-selected.md)** stops `eval_run` accepting a
model-controlled `flux_bin` that was then exempted from sandboxing and handed raw provider keys.
**[C-259](stories/C-259-authenticate-core-release-tooling-and-artifacts.md)** removes pipe-to-shell
bootstrap from privileged release jobs and gives core artifacts consumer-verifiable provenance.

The MEDIUMs bound the daemon and the default posture:
**[C-260](stories/C-260-bound-rest-sse-lifecycle.md)** (SSE disconnect cancels its turn, events are
bounded), **[C-261](stories/C-261-add-daemon-resource-budgets.md)** (principal-aware admission plus
completed-usage circuit breakers), and **[C-262](stories/C-262-fail-closed-unattended-sandbox-profile.md)**
— the one breaking change in the release: unattended and serving surfaces now **fail closed** on
sandbox posture rather than silently starting unconfined.
**[C-263](stories/C-263-strengthen-direct-io-enforcement.md)** and
**[C-264](stories/C-264-add-adversarial-assurance-lanes.md)** turn assurance structural: the
direct-I/O gate parses Rust rather than grepping it and covers every model-facing pack, and CI gains
adversarial parser, memory-safety and static-analysis lanes.
**[C-265](stories/C-265-contain-built-in-strict-review.md)** came out of the *first closure review* —
project role files could shadow the built-in `flux review` reviewers and turn a promised read-only
auto-approved command into workspace-write authority.

The epic stays open on its last acceptance bullet, deliberately: three fresh independent reviews must
run against the integrated tree and find no reproducible High-severity containment defect. Ratings
are evidence snapshots, not acceptance criteria. Bus factor and an external commissioned audit are
recorded as residual **governance** risks rather than fictional code stories.
Design: [adversarial-review-remediation-2026-07-30.md](designs/adversarial-review-remediation-2026-07-30.md).

### Zendesk automation — deterministic support workflows with bounded AI (epic) — 🔄 **IN PROGRESS (D-199; L-92 + A-136 + D-214 shipped, plugin WITHDRAWN, D-200/D-201/D-202 CLOSED as superseded — one bullet open, owned by another repository)**

The first complete reference for deterministic third-party automation in Flux-Lang, and a deliberate
demonstration of the vision's hard line: **the model reads evidence and never writes**.

**[L-92](stories/L-92-flux-run-named-entrypoints.md)** adds `flux run <module.flux> --entry <flow>`
with `--inputs`/`--arg`, so one `.flux` file can carry several named workflows and a caller selects
one — reusing the strict authored-flow input and safety path rather than growing a second one.
**[D-200](stories/D-200-zendesk-plugin-read-api.md)** and
**[D-201](stories/D-201-safe-zendesk-triage-writes.md)** build the typed `flux-plugin-zendesk`:
endpoint resolution and Basic API-token injection stay in the **host**, reads are bounded to one
page and contribute `zendesk.ticket` records, and writes are typed, additively-scoped and
concurrency-safe (mandatory `updated_stamp`, `safe_update=true`, internal comments by default).
**[A-136](stories/A-136-zendesk-triage-reference-flow.md)** is the runnable artifact —
`examples/zendesk.triage.flux` with `setup`, `triage`, `brief` and `eod` entrypoints exercising
retry, parallel reads, bounded contexts, model timeouts and deterministic evidence fallback. It
contains **no Zendesk write operation at all**; the plugin's writes exist and are separately
callable, which is the point.
**[D-202](stories/D-202-zendesk-docs-and-release-proof.md)** makes it reproducible for a user holding
only a Zendesk URL, an email and one API token, and is honest about the exposure of internal notes to
the configured model.

**The plugin half is withdrawn before it ever shipped.** `flux-plugin-zendesk` is removed from the
tree rather than released: a flux-connectors interop layer is to supersede it, and publishing a signed
pack now would ship a binary already scheduled for replacement. Nothing is withdrawn from users — no
pack was ever published, so the plugin existed only in a source checkout. D-200, D-201 and D-202
therefore revert from `done` to `blocked` on that interop.

**What survives is the half that was not integration-specific.** L-92's `--entry` is shipped and
unaffected. A-136's flow is kept deliberately, marked plainly as unrunnable, because its coverage is
provider-free — the flow tests drive all four entrypoints against stubbed operations, so the authored
shape stays enforced while no integration exists to serve it. That shape, plus the data-exposure and
write boundary in [zendesk-triage.md](zendesk-triage.md), is the contract the replacement inherits;
the `zendesk.*` operation names are the only part expected to move.
Design: [zendesk-automation.md](designs/zendesk-automation.md).

### The fleet runs the track / impl-coord loop (epic) — 🔄 **IN PROGRESS (C-239; F1 C-236 + F3 C-238 in flight, C-240…C-246 filed)**

0.36.0 shipped a fleet *coordinator* — a Program declares a board and hands items to remote agents
over A2A with `runner`/`task_id` written back — but not the loop the `track` plugin actually runs:
select a wave of independent items, give each an isolated worker that implements and gates and commits
on a scratch branch, review the returned diff **as evidence**, two rework rounds to the *same* worker,
park after that, integrate serially with a full gate after **every** merge and revert on red, then
write the ledger. The load-bearing decision is where that contract lives: **the model reasons, the
host enforces.** A `WaveCoordinator` owns the irreversible, order-sensitive actions — isolation, gate,
merge, revert, ledger — and the model owns only wave selection and diff review. The point is not
tidiness: it means fenced ledger, gate-after-every-merge, never-implement, revert-on-red and
park-after-two hold *even when the model is wrong or lazy*, because they are host behaviour rather
than instructions a prompt can lose. `fleet.integrate` is the sharpest instance — it gates and merges
or does neither, so the most-violated rule in the loop becomes unskippable; and gating per merge is
what attributes the failure of two stories that each compile alone but not together, which is exactly
the case that produces no git conflict. Coordination *prose* deliberately stays out of flux: the
wave-selection and review heuristics are content, and they live in a reference `coordinator.flux` and
its guidance. Sequenced so the data path lands before anything reasons over it — **F1** makes the
board readable (`board.query` with a real `output_schema`; today `render_compact` drops `runner`,
`task_id`, `depends_on`, `repo` and `evidence`, so `each`/`match` has nothing typed to iterate, and
C-235's JSON-quoted strings break even scraping the prose), **F2** makes it correct (a `Failed→Ready`
retry currently keeps a dead `runner`/`task_id` and the next sweep chases a corpse), then F3–F5 make
integration possible, F6–F8 make the worker real, F9 is the product and F10 makes a running fleet
visible. **A code-read moved the scope boundary before any code landed**, and it is the most useful
thing the design records: per-worker filesystem isolation does not exist for remote workers and is
*designed out* (`git_worktree_enter` is caller-local by construction), and a worker cannot return a
branch or a diff at all — `SpawnOutcome` has no artifact field and `flux-server` never populates
`Task.artifacts` — so "review the diff as evidence" has no channel to a remote worker, and a branch
*name* from another filesystem is useless anyway. **The full implementation loop is therefore a
local-worker loop for now**, which is sound because local children get real isolation via C-100; the
distributed half (Docker isolation, artifact return over A2A, discovery, worker auth) is the
`agent-fleet-runtime` epic and is explicitly later. Two corrections went the other way: A2A session
continuity on `contextId` **is** implemented, so F8's rework genuinely resumes the same worker; and
`ProcessRuntime` is not an optimization but a **prerequisite for any wave larger than one**, because
`FlowEngine`'s `turn_gate` means one worker serves one concurrent turn. Done looks like F9's offline
end-to-end journey — a stub A2A worker, a `MemoryBoard`, two items, one integrating and one parking,
no network and no real model — with every loop invariant pinned by a test instead of asserted in
prose. Design: [designs/fleet-loop.md](designs/fleet-loop.md).

### Unattended run integrity — survive provider transport failure, and be honest when you don't (epic) — 🔄 **DESIGNED (C-229; C-226…C-228 filed, none started)**

Three stories filed separately turned out to be one failure at three depths, and grouping them said
something none of them said alone. **C-228** is the symptom: `openrouter/google/gemini-3.x` dies with
`stream closed before completion`, reproducibly, part-way through exploration at 12–21k ctx — short
turns survive, which is why no smoke test catches it. **C-227** is the missing capability: a closed
socket is not a decision the agent made, yet it ends the turn outright, so a run that has executed
dozens of ops and written real files loses the rest of its work to one dropped TCP stream. **C-226**
is why nobody has quantified any of it: that dead turn exits **0**, emits no NDJSON `error` line, and
reports the failure as prose inside `turn_end.answer` — `loop_host.rs` literally converts
`Err(error)` into `Ok(json!({"kind": "error", …}))`, at two sites. So flux is not currently safe to
run unattended against a real provider for a long task, and the failures are invisible to exactly the
automated consumers — CI, editor extensions, a coordinator fanning work to sub-agents — that would
have counted them. The load-bearing decision is an **ordering** one: **C-228 must be diagnosed before
C-227's retry is designed.** If the stream close is a genuine transport event, bounded retry is the
right fix; if flux's own Messages-path codec ends the stream on an unhandled reasoning envelope, then
retrying re-runs a *deterministic* bug — every attempt failing identically at the same context depth,
burning budget and real money, and converting a reproducible defect into what looks like a flaky
network, which is strictly worse than today's honest hard failure. The evidence leans that way
already: `gemini-2.5-flash`, which has **no reasoning stream**, survives the workload that kills
3.5 and 3.6, and a vendor-wide transport problem would not discriminate on whether a model emits
reasoning deltas. There is even an invariant to judge it against — A-33…A-37 established that codecs
*skip and count* an unparseable envelope via `Chunk::StreamDiagnostic` rather than `?`-propagating, so
a hard `api_error` that kills a turn is the exact shape that rule exists to prevent, and C-228 would
be an **invariant regression rather than a feature request**. Sequenced C-226 ∥ C-228 (file-disjoint:
`flux-flow`/`flux-cli` vs `flux-providers`), then C-227, which needs C-226's typed outcome before its
own "visible, never silent" retry telemetry has anywhere to go. Done looks like a long run that either
completes with a bounded, visible, *accounted* retry, or exits non-zero with an outcome a subprocess
driver can branch on without parsing prose. Design:
[designs/unattended-run-integrity.md](designs/unattended-run-integrity.md).

### The agent-authored surface — panes the model opens, config it can safely change (epic) — 🔄 **DESIGNED (C-219; C-220…C-225 filed, none started)**

The ask was "tools to directly modify the harness, the UI … so it would be completely free". Reading
the tree said most of the plumbing is already built, and that the interesting question is not
capability but trust. **The tool→surface seam exists twice already** — `ToolProgressSink` and
`SpawnActivitySink` (`flux-runtime/src/lib.rs:188-262`) are the same shape: a send-only trait at L2,
installed by the L6 surface, reachable only through `ToolContext`, with redaction applied at the
reporter so a tool structurally cannot put raw bytes on a screen. **And "the agent extends the
harness" already ships one layer down**: `op.register` (`flux-tools/src/reflect.rs:459`) lets the
model author a Flux-Lang composite at runtime with `scope: turn|session|project|global`, where the
engine owns all state mutation and every inner call still traverses the envelope. What is missing is
only the surface layer. A third finding decided where to start: **A-79's correlated sub-agent
activity stream has no consumer in the TUI at all** — the tree's only `SpawnActivitySink`
implementation is `flux-cli`'s `IgnoredSpawnActivity` (`main.rs:114`), so a designed, redacted,
tested stream is discarded on the daily driver. The epic's weight, though, sits somewhere else
entirely: **a model that can draw a styled region inside a trusted terminal is a model that can
imitate the approval sheet.** C-163 already wrote that rule for plugins — *constrain the rendering
rather than relying on good behaviour* — and it holds harder for the model, which is the thing the
approval sheet exists to gate. So both halves are closed structurally rather than by policy: panes
carry `kind` and `data` and **no field that reaches a `Style`**, making trust chrome unforgeable
because there is nothing to forge it with; and the agent-writable config key set is asserted
**disjoint from `PinnableKey::ALL`** by unit test, so `[permissions]`, `[sandbox]`,
`workspace.allow_all` and `private_net.web` are unknown keys rather than denied ones. Two scope
decisions were taken deliberately against the more powerful option: a **typed pane vocabulary**
(`rows|kv|log|progress|tree|markdown`) rather than raw layout control, and **no process re-exec** —
`/resume`'s existing `project_session` gives in-process reload without opening a new
turn-termination path, the bug class that has recurred three times. C-220 → C-221 → C-222 land the
contract, the rendering and the trust invariant *before* C-223 makes any of it reachable by the
model; C-224 (the fleet pane) and C-225 (config) are separable. Done looks like a live pane the agent
opened on `flux tui`, and two tests: one where an approval-sheet impersonation payload still renders
inside the marked agent region with the real sheet drawn over it, and one where the writable key set
provably cannot name a security-relevant key. Design:
[designs/agent-authored-surface.md](designs/agent-authored-surface.md).

### Cross-harness session history — search what was already said, in any local harness (epic) — 🔄 **DESIGNED (C-212; C-213…C-216 filed, none started)**

The ask was a datasource: `search(query: "why did we drop the retry wrapper", harness: "opencode")`
over `flux | codex | claude-code | opencode`. Reading the tree said the acquisition half is already
built and shipping. **`flux usage` locates, opens and parses all four harnesses today** —
`$CODEX_HOME`/`~/.codex/sessions` and `$CLAUDE_CONFIG_DIR`/`~/.claude/projects` as JSONL,
`~/.local/share/opencode/opencode.db` as SQLite, flux's own event store — and its parsers walk
**exactly the objects that hold the message text** before taking only `usage` and `model` out of them
(`usage.rs:963-969`, `:1058-1125`, `:1214-1220`). The content is in hand at every site and dropped one
field short. So the extraction is one field wide, and the epic's weight sits somewhere else entirely.
Harness history is a **category of input flux has never ingested**. Every existing datasource reads
something the operator deliberately pointed at — a markdown tree, an OpenAPI file, a page just
fetched. This one reads *every project the user has ever worked in*, from outside the workspace jail;
it is secret-bearing by construction, because a conversation log is precisely where credentials get
pasted; and it is verbatim adversarial text that an attacker can pre-load once and have retrieved
forever after. All three land on the same `<knowledge-base>` block A-21 had to escape. The design
consequence is that **C-215 carries the whole containment envelope rather than deferring it** — off
unless explicitly enabled, escaped at ingest, redacted at ingest rather than at render (the C-195
lesson mirrored: that surface has nothing downstream, this one has everything downstream), and a
per-harness permission subject so a policy can allow `flux` and deny the rest. Splitting "ship it"
from "make it safe" would make the unsafe version the shipped version, and the epic's entire risk
lives in that window. The other genuinely hard part is a budget: a usage record is per-turn and eight
integers, a message record is per-message and carries full text, so the same scan produces one to
three orders of magnitude more output against directories holding years of history — the inherited
`MAX_JSONL_FILES`/`MAX_JSONL_FILE_BYTES` caps are necessary and not sufficient. Strictly sequential
(C-213 → C-216), no fan-out; no new crate (`flux-capabilities`, L5, already owns the datasource and
`rusqlite`); read-only against every foreign harness, always. Done looks like a redacted, escaped,
addressable message returned from a real opencode database — and a test proving a disabled datasource
performs **zero** reads outside the workspace. Design:
[designs/harness-history.md](designs/harness-history.md).

### Agent fleet runtime — starting, stopping and finding agents (epic) — 🔄 **DESIGNED (A-119; A-120…A-128 filed, none started)**

The fleet coordinator assumes workers exist at known URLs. Two exhaustive sweeps of the tree
established that both halves of that assumption are unbacked, and the answers are blunter than
expected. **Who starts an A2A-reachable agent? A human, in a shell.** Every one is an
`Arc<FlowEngine>` inside a single foreground `flux` process; `flux-channels/src/host.rs:63-78` is the
entire supervision story and a fatal channel error kills the process with no restart; there is no
Dockerfile, no unit file, no manifest, no `--daemon`, and flux never spawns `flux`. `GET /health`
exists and nothing consumes it; D-63's multi-agent mount is implemented and has no production
caller. **How does an agent learn a peer exists? It doesn't** — the A2A card answers "what is at
this URL", never "which agents exist", there is no index route, and roles (`.flux/agents/*.md`) are
a local persona catalog in a namespace disjoint from A2A entirely. The design's leverage is that the
missing mechanism is only missing *for agents*: the endpoint broker (D-25…D-32) already fans a
"which endpoints exist for product X" query out to provider plugins and returns weak refs with
labels and a `credential_ref` and never a secret — so agents become a **product** on it, and the
kubernetes plugin can enumerate live pods as fleet members with no config edit and no second
discovery path. The rest is a new L5 crate, **`flux-fleet`**, built on two axes the design refuses to
conflate: the **runtime** owns the process (`external` · `proc` · `docker` · `k8s`) and the
**transport** owns the conversation (`a2a` over HTTP · `ndjson` over stdio), carried in one URI whose
scheme picks the runtime — `k8s://prod/deploy/flux-worker`, `proc://flux?program=w.flux`,
`proc://claude?proto=ndjson`. Readiness is `status()`, never `start()` returning, because a
scheduled pod is not an agent that can take a turn. Two things carry the review weight: `fleet.start`
on a `proc://` address is **`bash`-class power** and is gated as such, and unifying roles with the
fleet (a role gains an optional `address`) means `cap_scope` — enforced today by constructing the
child's registry in-process — becomes a *request* across a trust boundary, which the design surfaces
as a divergence rather than papering over. Done looks like a coordinator discovering a worker,
starting it, dispatching to it, watching it through `Ready → Busy → Exited`, and reclaiming its work
— offline, in CI. Design: [designs/agent-fleet-runtime.md](designs/agent-fleet-runtime.md).

### Fleet coordinator — flux orchestrating flux across repos (epic) — 🔄 **DESIGNED (A-111; A-112…A-118 filed, none started)**

The ask was a first-level orchestration harness: cross-repo work, Jira, a global board, remote
agents dispatched and monitored, status reported back — and the assumption was that it needed a
second app beside the coding agent. Reading the tree said otherwise. **`flux-app` is already that
harness**: it runs a `.flux` Program of agents/channels/datasources/triggers/journeys over a bus and
a delivery supervisor, `flux-channels` already supplies cron/webhook/slack/a2a adapters, and
`plugins/jira` already has issue CRUD, transitions and search. The coordinator is a *Program*, not a
binary. What is actually missing is narrower and more interesting. The state source flux would want
already exists in shape — `LiveDatasource` (`datasource/live.rs:60`) has a backend declare its
entities, filters and external authority, validates it once, and then *generates* uniform ops with
stable permission subjects, a tool group and an ambient signal — but it is **read-only**, and a board
needs create/transition/claim/comment. So the centre of this epic is **A-113**, a write-capable
`WorkBoard` sibling carrying a typed state machine (`Ready → Claimed → InProgress → Review → Done`,
plus `Blocked`/`Failed`) whose `transition` rejects an illegal edge *without writing* — purpose-built
rather than generic precisely so the coordinator can reason over dependency waves and stuck items
instead of shuffling opaque rows, with markdown, Jira, in-memory and GitLab backends behind one
contract suite. Two findings sharpened the rest. The A2A **server** task surface is already complete
(A-53…A-57, `flux-server/src/a2a.rs`); the gap is the **client** — `A2aClient` has no `cancel` and
its only caller is the `flux a2a` REPL, so no journey can dispatch anywhere (**A-116**). And run
state dissolves entirely: `fleet.dispatch` writes the `task_id` back onto the board item, so the
board *is* the run registry and crash recovery is "restart, sweep, re-derive". The load-bearing
blocker is none of that: `flux-channels` documents that deliveries are serialized by the shared
`App` and that "cross-channel parallelism needs per-delivery bus isolation" (`lib.rs:20`), so a
coordinator whose nightly sweep blocks webhook intake is single-threaded by construction — **A-112**
lands first, and it is likely a MINOR. Done looks like `flux run coordinator.flux --serve` driving an
intake → dispatch → sweep → done cycle offline in CI against `MemoryBoard` and a stub worker. Design:
[designs/fleet-coordinator.md](designs/fleet-coordinator.md).

### Website truth and identity — the public site tells the truth and looks like the product (epic) — 🔄 **DESIGNED (C-196; C-197…C-204 filed, none started)**

A 2026-07-29 audit of all 64 pages under `website/docs/` against the tree at `0.33.1` found the
site structurally strong — all 26 CLI subcommands, all 43 node kinds and all 21 expr builtins are
covered, and three regions are generated from the Rust source behind drift-guarded markers. D-117
and L-42 held. What drifted is narrower and, because the rest is trustworthy, harder for a reader
to detect. `language/flows-and-syntax.md:118` states that strings are single-line, on the page that
owns text syntax, while triple-quoted verbatim strings have shipped since L-39 — `"""` has zero
occurrences anywhere under `website/docs/language/`. The **entire HTTP session API is absent**:
`flux-server` registers twelve routes and the site documents the three A2A ones, so `POST
/sessions`, the SSE stream and the webhook — the reason `flux app run --serve` exists — are
publicly undocumented. Behind those sit uncatalogued ops, the `[wakeup]` and `theme` config keys,
~14 undocumented `FLUX_*` variables, a TUI surface reduced to one table row, and four places where
the **website is right and the in-repo docs are wrong** (README documents a `flux run --program`
flag that does not exist). Separately, the site is the only surface that ignores
[`assets/README.md`](../assets/README.md), the project's own brand spec: no favicon key at all, no
navbar logo, no social card, and a petrol-teal accent that appears in no brand asset. Done looks
like eight landed stories and, more durably, three coverage assertions in `website_contract.rs`
that make an undocumented route, op or config key fail `cargo test` — a truth pass without a guard
is a story we file again in three months. Design:
[designs/website-truth-and-identity.md](designs/website-truth-and-identity.md).

### Typed session log — session-shape validity by construction (epic) — 🔄 **IN PROGRESS (A-93; A-99 + A-100 done, A-101…A-102 filed)**

The "session shape is always a valid provider history" invariant has broken three times — cancel,
compaction, the iteration cap — each time on a newly added turn-termination path, and each time it
was caught by a provider 400 rather than by the code. It held by *discipline*: every writer funnels
through one `finish_turn`, and compaction snaps its own boundary with a local helper. **A-99** (in
`[Unreleased]`) makes the three rules types instead — `AssistantMessage`, `ValidHistory` and
`ShapeError` in `flux-events`, neither constructible except through a checking constructor nor
mutable afterwards, with rejections naming the failed invariant and the offending index. The design
also turned up the predicted fourth path already in the tree: `resurrect.rs` closes a turn outside
`finish_turn` and is correct only because its ordering was copied by hand. **A-100** (also in
`[Unreleased]`) puts the turn lifecycle itself at the write seam: `SessionLog` carries the log's
`Tail` and offers only transitions that preserve the invariant, re-derived from the store on every
open and appended *conditional on that derivation* — so two writers racing to open a turn leave one
user message, not two. **A-101** (next, and the board's top `ready` story) migrates `flux-flow`
onto it and deletes the unguarded write API; **A-102** migrates the SDK/CLI history rewriters.
Design: [designs/typed-session-log.md](designs/typed-session-log.md).

### Transactional turns — a compensating undo for the world, not just the session (epic) — 🔄 **DESIGNED (A-91; A-103…A-106 filed, none started)**

The Time Machine and the Lab replay and fork the *session*; nothing undoes effects in the *world*.
Since every effect is a frozen `ActionBatch` of literal calls, each mutating op can declare its
compensator and the runtime can synthesize a reverse batch, making `flux undo --turn 14` one command
and "no compensator declared" a policy-visible risk signal. The design corrects the epic's original
premise rather than inheriting it: reverse-batch synthesis **at approval time is not implementable**
for the dominant case, because the prior bytes a `write` must restore are not knowable until
immediately before the write — capturing early would confidently restore the wrong content. So
declaration (static, on `ToolSpec`, which is what powers the risk signal) splits from materialization
(execution time, inside the guarded boundary). Undo runs LIFO through the ordinary approval envelope,
so it is itself undoable, and stops at the first failure rather than half-applying. `EventKind` is a
closed set, so the new `Compensated` variant is breaking ⇒ MINOR. Design:
[designs/transactional-turns.md](designs/transactional-turns.md).

### Evidence-pinned memory — cross-session memory with provenance (epic) — 🔄 **DESIGNED (A-92; A-107…A-110 filed, none started)**

flux has no memory of any kind today (confirmed by code read — the `Memory*` hits in
`flux-capabilities` are the unrelated in-memory vector store). The flux-native version is not a
scratchpad: every entry cites the event-store receipt and git SHA it was learned from, and goes
stale-visible when that evidence changes. The load-bearing invariant is that **the model supplies the
claim, the host supplies the citation** — `memory_note` takes only `(claim, scope)`, so there is no
parameter through which provenance can be forged, the same property that makes `ActionBatch`
trustworthy. Storage is a `memory:<scope>` stream in the existing `events.db`, inheriting C-25/C-125
multi-process safety, C-126 WAL hygiene and flush-seam redaction instead of re-earning them;
injection reuses the `<knowledge-base>` `ContextBlock` seam already hardened against breakout (A-21)
and budget-bounded (A-24). Stale entries are still injected, marked — stale means *unverified*, not
false. Design: [designs/evidence-pinned-memory.md](designs/evidence-pinned-memory.md).

### TUI polish round 2 — legibility, discoverability, one overlay language (epic) — ✅ **COMPLETE (C-149…C-158, all ten stories; nine shipped v0.33.0, C-158 in v0.34.0)**

The first wave (C-101…C-116) delivered the TUI's *capabilities* — dense transcript, themes, approval
sheet with diffs, focus/yank, search, live tool cards. A read of `crates/flux-tui/src` said the
residual was not capability but *presentation*: every entry flush-left plain text, three list
overlays each hand-rolling their own chrome, pickers that could not filter, an approval sheet that
encoded no risk, and two states — empty transcript, blank-line Ctrl-C — that said nothing at all.
Ten bounded stories, no new capability surface. **C-151** and **C-153** shipped early in v0.32.0 (one
fuzzy ranker behind every picker, relative session ages); this release closes the rest. **C-149**
gives the transcript a per-kind gutter rail — an ordinary leading span, so wrap math, the C-111 focus
paint and the C-109 running-badge pairing needed no changes at all. **C-150** adds `dracula`, `nord`
and a `high-contrast` accessibility palette, with the MONO fallback now derived from `Theme::names()`
so a future palette cannot forget it. **C-152** collapses the queue, session and help overlays onto
one panel helper with the scroll-window math in a single unit-tested place, absorbing C-153's query
row and counter — and, as a side effect of exact-fit sizing, deleting a permanently reserved blank
row. **C-154** tints the approval sheet by effect tier, and *that* turned up the epic's one real
defect: the per-op approval path received its `IntentSet` and discarded it, so a single-op
destructive call had never disclosed as destructive — only whole-plan approvals did. **C-155** makes
per-card expansion discoverable, **C-156** requires a second Ctrl-C to quit, **C-157** greets an
empty session with a card naming model, workspace and the three affordances worth knowing.
**C-158** (streaming partial tool output onto running cards) was first *retired with cause*: an
investigation scoped to the TUI/CLI/runtime crates found no seam there that can observe in-flight
content, since `bash` and `task` live elsewhere and are awaited as one opaque unit. It was then
reopened with the wider crate set and **implemented in `[Unreleased]`**, and the blocking decision
went against the option that investigation had framed: `SpawnActivityEvent` is **not** widened to
carry content — that boundary keeps a spawned child's raw output from reaching the parent's surface
unredacted-by-construction, and loosening it for every sub-agent consumer to serve a local `bash`
card was the wrong trade. A narrower turn-scoped `ToolProgressSink` was added instead, whose only
route to a sink binds the context's `Redactor`, so redaction is structural. The honest consequence,
recorded rather than papered over: **only `bash` streams; `task` cards still show no live content.**
The breaking change in the shipped part is `ApprovalRequest` gaining `mutating` ⇒ MINOR per the
pre-1.0 rule.

### Event-store concurrent use — visibility, proof, hygiene (epic) — ✅ **SHIPPED v0.33.0 (2026-07-29; C-124…C-126, all three stories)**

Multiple flux processes sharing one `~/.flux/events.db` was supported by design — WAL, `busy_timeout`,
`BEGIN IMMEDIATE`, idempotent appends, PG advisory locks — but the envelope's edges were invisible and
the SQLite side was only ever tested inside one process. **C-124** shipped in v0.32.0: the one
`begin_write` seam now times its wait and warns past a threshold, so contention is observable before
it becomes a 5s failure. **C-125** replaces the two-connection in-process test with real operating
system processes — four writers × 25 appends across shared streams, asserting gapless per-stream
sequence and an exact total, plus a second test racing one stable event id from three processes and
asserting exactly one stored event. Its failing-first proof is the useful part: reverting C-25's
`BEGIN IMMEDIATE` makes it fail reliably, so the test genuinely guards the fix. **C-126** bounds the
WAL sidecar for long-lived daemons — but only after verifying the premise rather than assuming it, by
pinning a reader and watching `events.db-wal` grow unreclaimed. The hook runs
`wal_checkpoint(TRUNCATE)` on a dedicated zero-busy-timeout connection every five minutes from the
served coding-agent loop, and swallows `SQLITE_BUSY`: a checkpoint can never surface as a turn-visible
failure. Design:
[designs/event-store-concurrent-use.md](designs/event-store-concurrent-use.md).

### Turn latency visibility — where the wall clock actually went (epic) — ✅ **SHIPPED v0.31.0 (2026-07-28; C-180…C-182, all three stories)**

The TUI attributed execution time to operations and nothing else, so the largest component of almost
every turn — waiting on the model — was invisible, and a 30s backoff storm looked identical to a
model thinking hard. C-180 surfaces per-call model time and TTFT on each round's transcript badge,
live in the footer, and as an `llm` split in the closing summary; C-181 adds a `RetryObserver` seam
in `flux-provider` so connect-phase retries, OAuth refreshes and transport fallbacks are announced
*before* their backoff sleep (plus counted on `model.call`); C-182 makes the plan-approval sheet
list the operations it is asking to authorize instead of a bare count. The retry types were sealed
`#[non_exhaustive]` before their first crates.io publication in the same release. Known follow-ups
from the shared code review remain open: the footer badge outlives the recovered call, and the
observer is wired at the staged model stages but not yet compaction/cognition. Design:
[designs/turn-latency-visibility.md](designs/turn-latency-visibility.md).

### Unify the Anthropic Messages provider — gateways become config (epic) — ✅ **SHIPPED v0.31.0 (2026-07-28; C-168…C-173, all six stories incl. the review follow-up)**

The [LLM cache review](designs/llm-cache-review.md) closed with one finding it deliberately left
unfiled, because the epic was scoped to `claude` and `codex`: `openrouter/anthropic/*` runs at
**literally 0% cached**. Ninety days of `flux usage` sharpens it — `openrouter/anthropic/claude-fable-5`
($13.76) and `openrouter/anthropic/claude-opus-4.6` ($11.10), **$24.86 across 3.1M tokens in 32 days**,
and unlike the subscription-billed `claude/*` and `codex/*` rows this is metered cash: ~82% of real
spend. The same vendor one row down, via `openrouter-anthropic`, hits **69%**. So the fix already
exists — it just lives behind a *second provider name* that nobody types, while the obvious spelling
lands on the chat codec, which consults no `ProviderProfile` and therefore emits no `cache_control`
(and, per `spec.rs:174`, leaks tool calls as `<tool_call>` text instead of returning structured
blocks). The root cause is structural: a `WireCodec` **hardcodes** its quirks profile, so "the same
protocol over a different gateway" can only be said by writing another codec — and `OpenRouterMessages`
and `OllamaMessages` differ from `AnthropicMessages` by exactly one line each. The epic collapses
them: **C-168** makes the profile a constructor argument and deletes both duplicates (`BedrockAnthropic`
stays — version-in-body, field stripping and binary event-stream decoding are real wire behaviour, not
config); **C-169** makes OpenRouter's Messages endpoint the *only* wire flux uses for that gateway and
deletes `OpenRouterChat`, retiring `openrouter-anthropic` outright (BREAKING — a public const and a
user-facing name ⇒ next MINOR). That last decision changed mid-implementation: routing by vendor
segment would have stranded the GLM/qwen/deepseek-over-Messages route the docs already recommend,
since the endpoint is model-agnostic and returns structured `tool_use` for every vendor — one wire is
both simpler and strictly better. The retired name stays recognised by the *pricing parser* (the event
store is append-only, so historical rows must keep splitting the same way) while leaving the set of
selectable providers; **C-170** verifies then enables the 1h stable-prefix
TTL through the gateways, currently off and commented "Unverified"; **C-171** decodes the cache-write
tokens the chat wire drops for every OpenRouter row; **C-172** repairs the A/B harness, whose
`openrouter*` glob picks a kill switch that does nothing on the chat wire, so both arms run identical
bodies while reporting "no difference". Done means the spelling users type is the one that caches, one
codec implements the protocol for every non-Bedrock transport, and the win carries a measured
before/after. **Result:** on the identical prompt back to back, `openrouter/anthropic/claude-opus-4.6`
goes 0% → **62%** cached and **$0.0473 → $0.0194** — and that understates it, because the control is
not a flag but 32 days of history in which the old path wrote *zero* cache tokens across 91 calls, so
there was never anything to read. The 1h TTL was verified before being enabled (a probe put 7725
tokens in `ephemeral_1h_input_tokens` where the plain form put them in the 5m tier, even with
OpenRouter routing to Bedrock upstream); Bedrock-direct stays off, deliberately unverified. Two
follow-ups are recorded rather than assumed: Bedrock's own 1h probe, and why DeepSeek reports no cache
split on the Messages wire. Design:
[designs/messages-provider-unification.md](designs/messages-provider-unification.md).

### MariaDB / MySQL in the `sql` plugin (epic) — ✅ **SHIPPED v0.30.1 (2026-07-28; D-196…D-198, all three stories)**

An external user pointed a MariaDB endpoint at the `sql` plugin and got
`mysql is not yet supported by the flux sql plugin (residual)`. The message is accurate — `mariadb`
normalizes to `Dialect::MySql` (`plugins/sql/src/main.rs:339`) and all six ops call
`require_postgres()` before doing anything (`:932,967,1003,1062,1134,1249`) — and it is the residual
[D-31](stories/D-31-host-terminated-rawsocket-auth.md) recorded when it host-terminated the Postgres
handshake: *"mysql + Asterisk AMI host-termination (seam in place, clear error, credential cap
retained for them)"*. A user has now walked into that seam. The design's first job is answering why
this needs writing at all, because the obvious objection is strong: a SQL connection is a TCP
connection, Go injects one with `RegisterDialContext`, and flux already has the transport —
`ConnStream` is `std::io::Read + Write` (`plugins/host-kit/src/lib.rs:818+`). Three things block the
shortcut, and only the last decides it. No Rust MySQL crate exposes a dialer seam (`mysql_async::Opts`
and `sqlx::MySqlConnectOptions` offer host/port or a Unix socket *path*, never a caller's stream);
`ConnStream` is blocking where those crates want tokio; and — decisively — **host-terminated auth
makes a driver crate unusable in principle**, since a driver insists on running its own handshake,
which needs the password inside the plugin, which is exactly what the reference invariant forbids. So
the epic mirrors the Postgres split rather than working around it: **D-196** puts handshake v10 +
`mysql_native_password` host-side in `crates/flux-plugin/src/mysql.rs` (MariaDB's default auth
plugin, and *simpler* than the SCRAM already shipped — no PBKDF2, no server-signature check),
**D-197** gives the plugin a `COM_QUERY` client, and **D-198** replaces `require_postgres()` with
per-dialect SQL. D-198 is the one that is easy to under-scope: `table.list`/`index.list` read
`pg_class`/`pg_index` with no MySQL equivalent, foreign keys need a different join shape, and
`database.list` is a semantic trap — the same `information_schema.schemata` query parses on both
engines but means *schemas in the current database* on Postgres and *actual databases* on MySQL. A
"trusted plugin" tier that dials directly with credentials was considered and **rejected**: it would
make an invariant that is currently absolute and testable into a conditional one. Non-goals: SQLite
(still needs a host file capability), writes, AMI, and `caching_sha2_password`/`ed25519`/`parsec`
(follow-ons). Two things the plan did not foresee, both caught while implementing: `pg_lit()` escapes
only `'`, but MySQL treats `\` as an escape character, so a name containing `\'` could have broken out
of a literal (new `my_lit()`); and MySQL names *every* primary key `PRIMARY`, so porting the Postgres
PK join would have matched every table's PK in the schema at once. The `CLIENT_DEPRECATE_EOF`
plumbing the design called for was also dropped — the two result-set shapes are separable by the
spec's own fixed sizes (a classic EOF payload is exactly 5 bytes, every OK packet at least 7), which
avoids widening `HandshakeInfo`, a published 1.0.0 protocol-line type. Live interop against a real
All six ops were then verified live against a real **MySQL 5.7.44** (dev cluster, `latest`), which
advertises exactly the `mysql_native_password` handshake D-196 implements — though a MariaDB-specific
server remains untested. The plugin half lives in `plugins/`, which release cutting never touches —
it needs its own pack cut to reach users, as the smoke test demonstrated when the registered pack
v0.1.0 plugin had to be replaced by a local build. Design:
[designs/mariadb-support.md](designs/mariadb-support.md).

### Plugin protocol decoupling — a release that leaves the plugin pack alone (epic) — ✅ **SHIPPED 2026-07-28 (C-141…C-147, all seven stories)**

Cutting 0.28.0 made the plugin pack's release tax visible. `scripts/cut-release.sh` rewrites
`plugins/Cargo.toml`'s pins, bumps `plugins/host-kit/Cargo.toml` in lockstep, and re-locks the
nested workspace on **every** flux cut — `plugins/Cargo.lock` changed in five of the last eight
commits that touched it, all release cuts — while the wire contract those plugins speak has changed
twice in its history, additively both times. Reading the seam end to end found the reason the
lockstep exists and why it is the wrong instrument: **nothing enforces compatibility at all.**
`protocol.rs:10` defines `PROTOCOL = "flux.plugin.v1"` and stamps it into every frame, but no host
code reads it back and the pack index records nothing — matching version numbers are a ritual
standing in for a contract nobody wrote down. Worse, **every plugin compiles `flux-lang`**: the
guest wire surface names exactly one type from it (`FlowEffect`, `protocol.rs:7`/`:140`, a *tag
vocabulary*), and that one edge drags a 75-crate subtree through
`flux-lang → flux-plugin → host-kit → all 21 plugins`. The epic extracts the wire contract into
`codewandler-flux-plugin-protocol` on its own `1.x` semver line, moves the serde-only leaves it
needs onto that line, and takes `plugins/` out of the cut entirely — with the guards that make the
split honest: the host validates the protocol marker, golden JSON fixtures pin the wire rather
than the Rust signatures, a snapshot guard forces a deliberate version bump, and CI runs the
*previously released* plugin binary against the current host. Publishing then only touches what
moved, and the cut becomes transactional (0.28.0's gate failure left both changelogs rolled and had
to be finished by hand). C-141 landed first and alone was worth shipping: it deleted the 75-crate
subtree with no version-line change (a plugin's build graph went from **74 to 30** crates).

**As shipped:** the wire contract is `codewandler-flux-plugin-protocol` at `1.0.0`, the `guest`
feature on `flux-plugin` is gone, and `scripts/cut-release.sh` touches nothing under `plugins/`.
What the lockstep was implicitly guaranteeing is now checked explicitly, in CI:
`scripts/check-plugin-compat.sh` runs the *previously released* plugin binaries against a host
built from the tree, and `scripts/check-crate-versions.sh` fails a crate that changed content
without moving its version. The host rejects a foreign protocol marker by name, golden JSON pins
the wire, `host-kit` publishes with the pack instead of the flux closure, and a failed cut restores
the tree instead of leaving it half-rolled. Design:
[designs/plugin-protocol-decoupling.md](designs/plugin-protocol-decoupling.md) (see "As built").

### LLM cache review — prompt-cache correctness for `claude` and `codex` (epic) — ✅ **SHIPPED v0.30.0 (2026-07-28; C-133…C-140 + A-95, all nine stories)**

A-03 made the planner *prefix* cache-stable and live-verified a 99% cross-process hit. That fix
still works — and it is also the entire extent of prompt caching in flux. Reading the request path
end to end turned up the gap: **no cache breakpoint is ever placed in `messages`**. Every
`cache_control` is stamped in the `system` array, and `ContentBlock` has no field that could carry
one, so the cacheable prefix stops where the system prompt ends and the whole growing transcript is
re-priced at full input rate every round. Four aggravators sit on top: tool-set churn mid-loop
cold-writes the prefix (tools render *before* system, so one `capability_signal` invalidates
everything), only the 5-minute TTL is ever used (an interactive pause outlives the cache), the
`% hit` we render is the turn's *last* round rather than the turn, and subscription-claude already
sits at Anthropic's hard maximum of four breakpoints so the headline fix cannot land until the cap
becomes a union budget. On the codex side the Responses path sends no cache-routing key and hoists
the deliberately-volatile trailing system segment into the front of `instructions` — the A-03 layout
helps Anthropic and actively hurts codex. A first measurement (`flux usage`, 2026-07-28) bounded the
problem and corrected the premise: 32% cached across 813 calls, with `claude/*` at 35% and
`codex/*` at 29% — claude is mid-pack, not the outlier — while `openrouter/anthropic/claude-opus-4.6`
sits at literally 0% for 2.1M tokens and $11.10 (the `openrouter` chat path still pins
`prompt_caching: false`; C-35 covered `openrouter-anthropic` only). Seeing it clearly comes first
(C-133 turn-level accounting + trace fields + a repeatable harness, C-139 a TUI header that splits
the cache tiers and stops summing last rounds, C-140 an in-TUI `/usage` overlay showing per-round
hit rate live), then claude (C-134 tail breakpoint, C-135 1h TTL on the stable prefix, A-95 freeze
the tool set per turn), then codex (C-136, C-137), then close-out (C-138). Done means every fix
carries a live before/after number from the same harness, a regression test pins where breakpoints
land, and the cache-layout invariant is documented so the next segment change can't halve the cache
in silence. **Result:** on a long-transcript turn the conversation-tail breakpoint takes the hit rate
47% → **71%** and equivalent cost ~$0.106 → **~$0.042**, with the tail arm running first so ordering
favoured the control; on short turns it is neutral, and it ships with a `FLUX_CACHE_TAIL=off` kill
switch. `prompt_cache_key` was live-verified accepted by the ChatGPT/codex backend. Two stories
landed narrower than filed on evidence — `flux usage` was already correct (only the live displays
were biased to a turn's last round), and the adaptive tool set was already byte-stable except for a
no-op capability signal that churned the prompt for nothing. Design:
[designs/llm-cache-review.md](designs/llm-cache-review.md).

### TUI polish — 5 UX + 5 UI (epic) — ✅ **SHIPPED v0.28.0 (2026-07-28; wave 1 C-102…C-110 + wave 2 C-111…C-116, all fifteen stories)**

The TUI became a daily driver (A-65) and just gained its boot splash + spinners (C-101); what
remains are well-defined rough edges. Five UX fixes — a mouse-capture toggle so terminal-native
select/copy works (Ctrl-T), an approval modal that no longer denies on stray keys and renders its
subjects as text, readline Ctrl-R history search, Ctrl-F transcript search with highlights, and a
real help overlay — and five UI upgrades: progressive narrow-width header/footer degradation, a
redesigned approval sheet, a theme system (truecolor dark / light / mono + `/theme` + persistence),
a scroll position indicator, and live animated running tool cards that leave the transcript layout
cache untouched. Done means all nine stories' acceptance green under the standard gate; the three
pub-surface breaks (`TuiRunOptions.theme`, `ChatState.modal` → `approval`, and wave 2's
`ApprovalChoice::DenyWithReason`) shipped with the 0.28.0 MINOR.
Design: [designs/tui-polish.md](designs/tui-polish.md).

### Claude interop — commands + skills that load from both worlds (epic) — ✅ **SHIPPED v0.28.0 (2026-07-28; D-186…D-192, all seven stories; supersedes C-93)**

A compatibility audit found flux half-speaks Claude Code's dialect: skills already load from
`.claude/skills`/`~/.claude/skills` with `name`/`description` frontmatter, but command files
(`.claude/commands/*.md`, `$ARGUMENTS`) don't exist at all, every other frontmatter field
(`allowed-tools`, `model`, `disable-model-invocation`, …) is silently dropped, supporting files
(`references/`) are unreachable, nested skill trees are invisible, and half of `flux-skill` is dead
code. The epic's stance: compatible where Claude's semantics are good, deliberately divergent where
ours are better (manual `--skill` activation stays the default; model-invoked disclosure becomes an
opt-in), and loud about the difference — no silently dropped fields, and a dedicated
`website/docs/agent/claude-compat.md` page whose claims track what actually ships. Done looks like: a
real `.claude` directory (commands + nested multi-file skills) works in place, agent invocation is
triple-gated (permitted ∧ accessible ∧ agent-triggerable, absorbing C-93), and one honest discovery
implementation remains. Design: [designs/claude-interop.md](designs/claude-interop.md).

### Context-local Git worktrees (epic) — ✅ **SHIPPED v0.28.0 (2026-07-28; C-97…C-100 + C-120/C-121; `ToolContext.system` field → accessor is the breaking change that made it a MINOR)**

> Follow-ups: C-120 (disk-backed worktree allocation — `/tmp` tmpfs hazard) and C-121
> (per-turn worktree note) landed same day; C-122 (plugin hosts follow the transition) is backlog.

Agents that mutate a repo while the user or another agent works in the same checkout step on each
other. This epic adds `git_worktree_enter {}` / `git_worktree_leave {}` as guarded Git built-ins:
`enter` moves **only the calling agent context** into a temporary worktree under a private
`/tmp/flux-worktree-*` directory (a generated `flux/worktree/…` branch off a clean `main`); `leave`
trial-merges then `--no-ff`-merges the committed work back into `main`, restores that context's
original root, and cleans up — with failure modes that never lose work or strand `main` conflicted.
The enabler is a per-agent `WorkspaceContext` (a context-owned, swappable active `System` — never
`set_current_dir`), which also makes `FlowEngine` group probing follow the active root and gives
spawned sub-agents an independent snapshot. Done means the two-context isolation test, the full
enter→commit→leave→merge round trip, and every rejection/recovery path are green under the standard
gate. Distilled from a decision-complete plan session. Design:
[designs/context-local-git-worktrees.md](designs/context-local-git-worktrees.md).

### Deterministic Agent Lab — Test · Tune · Resurrect (epic) — ✅ **COMPLETE (2026-07-28; D-174…D-180, all seven stories)**

Turns the deterministic-run substrate (canonical `plan_source` + the redacted op cassette) from
debugging verbs into a product for SDK embedders — the three doors no LLM-as-runtime SDK can open:
**Test** (`flux_sdk::test::Scenario`, feature `test-kit`: record a run once, commit it as a redacted
`Storage::dir` fixture, re-run the *real* agent offline in `cargo test` for $0 and assert on the
canonical Flux-Lang plan), **Tune** (`Session::what_if()` / `Client::what_if_over`: re-run a recorded
session under exactly one changed variable — model, prompt, policy, or a substituted tool output —
against a byte-frozen world, with an honest `hermetic()` readout), and **Resurrect**
(`Session::resurrect()`: finish a turn killed mid-execution with zero model re-spend and exactly-once
semantics for every op with a recorded cassette cell). One engine spine underneath: the
`CassetteScope` family grows `Frozen`/`Resume` arms at the single dispatch chokepoint, plus
`run_turn_pinned` (breaking → MINOR). CLI surface: `flux record` / `flux test` / `--store` /
resurrect-on-open. Design:
[designs/deterministic-agent-lab.md](designs/deterministic-agent-lab.md).

### Harness hardening — guard the untrusted-input surface (epic) — ✅ **SHIPPED 2026-07-15 (v0.26.0; C-76…C-88 + L-81…L-83, all sixteen stories)**

A full-workspace review found the codebase mature and well-gated (CI enforces `fmt`/`clippy -D
warnings`/the `flux-codegate` L0–L6 layering lint/the raw-spawn scanner; `cargo audit` clean; provider
codecs, the spawn seam, A2A isolation, and the DAG optimizer all verified solid). The residual gaps
cluster into two root causes. **(1) A model-reachable exfiltration surface** — since `network.fetch` is
a default grant, a prompt-injected model can move a secret off-box in one unapproved call: `http.request`
resolving any `$secret` env var to any URL and then *scrubbing it from the transcript* (**C-76**, verified),
the egress guard vetting a resolved IP while reqwest re-resolves at connect (DNS-rebinding → cloud metadata,
**C-77**), and `sqlite_query` reading any on-disk DB outside the jail at `Risk::Low` (**C-78**), plus
credential-leak vectors (**C-82**). **(2) No resource governor at the execution boundary** — safety caps
live only in the analyzer, so untrusted `.flux`/LLM plans reach uncatchable stack-overflow aborts (**L-81**),
CPU-pin + OOM (**L-82**), and host OOM in tool/network/plugin paths (**C-79**, **C-80**, **C-83**, **C-84**);
plus upgrade-safety (**C-81**), correctness/growth (**C-85**, **C-86**, **L-83**, **C-87**), and a
code-quality standalone (**C-88**). All sixteen stories closed and shipped in **v0.26.0**: the
exfiltration surface is gated (secret-resolution consent, connect-time IP pinning, the sqlite jail),
and execution-boundary budgets (recursion depth, CPU/step, response-size caps) now back the
analyzer's static limits. Design:
[designs/harness-hardening.md](designs/harness-hardening.md).

### Async live systems-of-record datasources — ✅ **SHIPPED 2026-07-15 (v0.26.0; D-62, D-168…D-173)**

Added a first-class seam for remote systems of record without turning the synchronous indexed
knowledge store into an async integration abstraction. A pure L0 contract describes entities,
typed filters, compact rows, opaque cursors, and weak references; guarded async backends project a
consistent per-domain `list`/`get` pair with validation before backend entry. Configured-domain
evidence controls discovery, while planning and dispatch share exact datasource, network, and
connection authority. The SDK installs the complete surface with one fallible builder call, and a
hermetic support-system example proves paging, multiple entities, get/not-found, catalog surfacing,
and denial before IO. Design: [designs/async-live-datasource-seam.md](designs/async-live-datasource-seam.md).

### flux-sdk surface — a standard agent SDK (epic) — ✅ **SHIPPED 2026-07-12 (v0.16.0–v0.17.0; D-142…D-159)**

Grew `flux-sdk` from a thin `FlowClient` into a batteries-included agent SDK **without adding a third
client**: two doors (the deterministic `FlowClient` and the model-driven `Client`) over one `Session`
handle, with feature-gated batteries (`pricing`/`providers`/`plugins`). **Wave 1** (storage + the
`Session` handle, `Client` envelope parity, `send_with`, `TurnStream`, a re-export sweep — D-142…D-146)
landed in **v0.16.0**; **waves 2–4** (flow-driven `Session::start_flow` + suspensions, `with_sub_agents`,
ClientBuilder surfacing/compaction knobs, `ExecutionResult.usage`, `Session` projections; the
`flux_providers::spec` module + the `providers`/`plugins` features + `Session::run_voice_flow`; hermetic
`Session::replay`, `Session::fork`+diff, `FlowClient` streaming, and the datasource recipe doc —
D-147…D-159) landed in **v0.17.0**. `flux-sdk` itself stays `dist = false` — excluded from the binary
release closure. Design: [designs/sdk-surface.md](designs/sdk-surface.md).

### Web capabilities II + plugin polish — crawl · PDF · embeddings · dot-naming (v0.16.0–v0.19.2) — ✅ **SHIPPED**

The additive cluster that extended the D-98/D-120 web surface and adjacent plugin/example work, none
of which had a roadmap entry:
- **Web capabilities II (D-160…D-163, D-166)** — ✅ `web.crawl` (same-host BFS with page/depth caps,
  v0.16.0; later gained an optional `max_total_bytes` caller byte budget, v0.19.1 —
  [D-166](stories/D-166-web-crawl-byte-budget.md)), PDF extraction on `web.fetch`
  ([D-161](stories/D-161-web-fetch-pdf-extraction.md)), an opt-in provider embeddings pack
  ([D-162](stories/D-162-provider-embeddings-pack.md)), and the **breaking `web_fetch`/`web_search` →
  `web.fetch`/`web.search` dot-rename** that made the whole web family uniformly dot-namespaced
  (clean cutover, no alias; [D-163](stories/D-163-web-fetch-search-dot-rename.md), v0.19.0).
- **Plugin operation output schemas (D-164, v0.18.0)** — ✅ preserve a plugin op's declared output
  schema end-to-end ([D-164](stories/D-164-plugin-operation-output-schemas.md)).
- **Runnable Slack support-bot example (D-165, v0.18.0)** — ✅ made the support-bot example genuinely
  runnable ([D-165](stories/D-165-support-bot-example-runnable.md)); a public beginner tutorial
  followed in v0.19.2.

### OS process sandboxing — bubblewrap · Seatbelt · graceful-Windows (epic) — ✅ **SHIPPED 2026-07-11 (v0.14.9; D-134…D-137)**

The safety envelope governs what the *model* may request, but the processes flux ultimately spawns
— shell commands and above all **stdio plugins** — run with the user's full OS access; five website
pages honestly promise plugins are "not OS-sandboxed". This epic flips that disclaimer into a
feature: an OS-level sandbox as defense-in-depth **underneath** the envelope, applied at flux's
single spawn choke point (`System::build_command`), so shell ops and plugin subprocesses are
confined by one seam. A concrete `Backend` enum (no trait) carries per-OS mechanics — **bubblewrap**
on Linux (whole-fs read-only, writes confined to workspace/named-roots/tmp/toolchain-caches,
network switchable via namespace), **Seatbelt** (`sandbox-exec` + generated SBPL profile) on macOS,
**graceful degradation** on Windows (warn-and-run, or fail-closed under `require`; real backend is
a follow-up). Opt-in default-off in v1 (`[sandbox]` config, `--sandbox`/`--no-sandbox`,
`FLUX_SANDBOX` inheritance channel), orthogonal to the approval gate, browser `spawn_debug_pipe`
deliberately exempt (Chrome's own sandbox is stronger than what would survive nesting). "Done" =
the abstraction + both Unix backends landed with golden-argv/profile tests and live double-gated
smokes ([D-134](stories/D-134-sandbox-abstraction-config-threading.md) ·
[D-135](stories/D-135-bubblewrap-backend.md) · [D-136](stories/D-136-seatbelt-backend.md)), and the
website security docs updated truthfully with the drift-guard test rewritten
([D-137](stories/D-137-sandbox-docs-truth-pass.md)). Design:
[designs/process-sandboxing.md](designs/process-sandboxing.md).

### Web capabilities — request · read · browse (epic) — ✅ **SHIPPED 2026-07-09 (v0.12.0; D-98 + D-120…D-124)**

Working with the web is **three fundamentally different capabilities** — distinguished by what the
model *sees* and what can go wrong — and flux ships them as three deliberately separate surfaces,
**all native, no plugins** (table-stakes capabilities don't sit behind an install step), in one new
L5 crate **`crates/flux-web`** governed by one family-wide scoped egress policy
(`[private_net] web`; public-only default, SSRF guard on every request, `PrivateNetAdmit` audit):
**tier 1, request** — [D-98](stories/D-98-flux-web-crate-and-http-request-op.md), the crate + a
native `http.request` op (arbitrary method/headers/body, status/bytes back); **tier 2, read** —
[D-120](stories/D-120-web-fetch-readable-markdown.md), pages as *documents*: an
HTML→readable-markdown condenser (`flux-web::condense`, emitting through the `flux-markdown` AST)
behind an upgraded `web.fetch` (which cuts over to the `web` scope — the per-tool special case
from the D-96 caveat dies; the op shipped as `web_fetch` here and was dot-renamed to `web.fetch`
by D-163 in v0.19.0) plus a composable pure `html_to_markdown` op; **tier 3, browse** —
non-visual browser use over headless Chromium and a minimal hand-rolled CDP-on-a-pipe client,
evidence-gated behind a Chromium-discoverable signal
([D-121](stories/D-121-browser-cdp-foundation.md)): the agent observes a byte-budgeted
**page digest** — condensed content + a resolved action space of stable element refs from the
accessibility tree, never HTML source, never screenshots
([D-122](stories/D-122-browser-page-digest.md)) — acts by ref and re-observes **deltas**, so a
browsing task costs tokens proportional to change, not page size
([D-123](stories/D-123-browser-actions-delta.md)), with every request (subresource, redirect hop,
JS-initiated) run through the scoped guard via CDP interception — required for epic-done
([D-124](stories/D-124-browser-egress-interception.md)). The rule the surface teaches:
**APIs → tier 1, documents → tier 2, applications → tier 3.** Subsumes and re-scopes the original
D-98 (first drafted as plugins; revised native the same day, user call). Design:
[designs/web-capabilities.md](designs/web-capabilities.md).

### flux-render — `flow_render`: flux source/plan → SVG (+ PNG) (epic) — ✅ **SHIPPED 2026-07-09 (v0.13.2; L-74…L-77) + L-78 PNG**

A model-facing built-in tool `flow_render` (beside `flow_list`/`flow_run`) that turns Flux-Lang into
a syntax-highlighted image — the highlighted **source** or the **execution-path tree** — rendered
entirely from flux's own view of the code: the lossless rowan CST for source, the `render_styled`
plan renderer for the tree. No tree-sitter, no external toolchain. Serves the surfaces that can't
run a grammar (GitHub READMEs, Slack, docs, chat/tool-result panels) and lets flux regenerate its
own doc images, retiring the brittle Node script in `flux-tree-sitter`. Layered for reuse: a pure
`flux_lang::highlight` substrate ([L-74](stories/L-74-flux-lang-highlight-substrate.md) — also the
base flux-lsp L-69 semantic tokens will adapt), a span form of the plan renderer
([L-75](stories/L-75-render-styled-spans.md)), the SVG tool itself
([L-76](stories/L-76-flow-render-tool-svg.md)), a `flux render` CLI subcommand that replaces the
Node doc-image script ([L-77](stories/L-77-flux-render-cli-subcommand.md)), and opt-in PNG
rasterization ([L-78](stories/L-78-flux-render-png.md) — feature-gated resvg/usvg stack with an
embedded JetBrains Mono; `flux render -o out.png`; the only story that adds deps). The model-facing
tool stays SVG-only by constraint and by design: `ToolResult` is text-only, so PNG is a CLI surface.
Design: [designs/flux-render.md](designs/flux-render.md).

### Datasource & endpoint discoverability (epic) — ✅ **SHIPPED 2026-07-09…10 (v0.13.0–v0.14.6; D-114…D-117)**

A grounding pass over "what can the agent do to enumerate its datasources and register new ones —
e.g. wire a Postgres endpoint and query it?" found the machinery **exists and is well-built but is
undiscoverable**: the five knowledge-retrieval ops require a `source` name nothing enumerates
([D-114](stories/D-114-datasource-sources-op.md) adds a `sources` op); the endpoint ops surface
only when a kubeconfig is present — an endpoint registered in `~/.flux/endpoints.toml` never
surfaces them, and `endpoint.import` is missing from the group so its gating is inverted
([D-115](stories/D-115-endpoint-group-surfacing.md)); wiring a known service without k8s discovery
means hand-writing `import --from-json`, and statically-registered refs don't resolve because the
`StaticResolver` bindings map is empty ([D-116](stories/D-116-static-endpoint-wiring.md) — `flux
endpoint add` + config bindings, proven end-to-end against the sql plugin's host-terminated SCRAM);
and the whole endpoint + saved-flows cluster has effectively zero website documentation
([D-117](stories/D-117-endpoints-flows-website-docs.md)). Done looks like the original scenario
running without a kubeconfig: one command wires a Postgres endpoint, the agent discovers the ops
unaided, enumerates its sources, and queries through the endpoint — all documented publicly.
Live systems of record remain separate from this indexed-knowledge surface. Their shipped seam
([D-62](stories/D-62-async-live-datasource-seam.md)) exposes generated per-domain `list`/`get`
operations with backend-owned paging and exact authority instead of pretending a remote database
is a local knowledge index. Design: [designs/datasource-discoverability.md](designs/datasource-discoverability.md).

### v0.6.0 beta hardening (epic) — ✅ **done 2026-07-08 (external beta test)**

The first external beta test of a shipped release (Codex, clean `/tmp` workspace vs. the published
`0.6.0` binary + source) exercised the product end-to-end and reported: *"Flux v0.6.0 is credible as
a beta — the architectural foundations are visible in real behavior, not just docs … The release
needs a focused hardening pass before broad beta use. Most issues are fixable with targeted
docs/runtime alignment and surface parity work rather than a redesign."* The core thesis held (visible
plans, real guardrails, offline replay, host-mediated plugin calls, bounded sub-agent scopes); the 16
findings cluster into docs/runtime mismatches and a few surface-specific gaps. Triaged into 12
stories under [beta-hardening](designs/beta-hardening.md) (the source report lived in an ephemeral
`/tmp` workspace, so its findings + repro essence are embedded in the design doc). **All 12 stories
are now done (2026-07-08) — implemented in the report's recommended fix order, each with a
failing-first/behavior-lock test, full gate green. Top five (fixed first):**
[C-45](stories/C-45-yes-destructive-approver-truth.md) (reconcile the `--yes` destructive-op safety
docs vs. the allow-all approver), [A-58](stories/A-58-flow-resume-await-payload.md) (`flow run
--resume` must bind the top-level `await` payload), [A-59](stories/A-59-flow-run-subagent-correlation.md)
(correlate direct `flow run` sub-agent children so `replay --sub-agents` recurses),
[A-60](stories/A-60-serve-mock-provider-parity.md) (program `--serve -m mock` provider parity), and
[A-61](stories/A-61-cli-broken-pipe-no-panic.md) (no SIGPIPE panic on a closed pipe). **Then:**
Flux-Lang fixes [L-43](stories/L-43-text-scalar-bind-types.md) (scalar bind types),
[L-44](stories/L-44-parse-node-composability.md) (`parse` composability),
[L-45](stories/L-45-fluxlang-compile-leading-op.md) (`fluxlang compile` leading-`op` parity);
diagnostics/UX [A-62](stories/A-62-validation-diagnostic-headers.md) (accurate diagnostic headers) and
[A-63](stories/A-63-context-pack-shrinkage-surface.md) (surface context-pack shrinkage); the
[C-46](stories/C-46-beta-docs-truth-pass.md) docs-truth pass (mock mode, A2A `protocolVersion`,
Flux-Lang examples, `peek`); and [A-64](stories/A-64-weak-model-planner-robustness.md) (weak-model
planner/loop robustness — guardrail, not a hard guarantee). Design + embedded findings:
[designs/beta-hardening.md](designs/beta-hardening.md).

### Data transforms (epic) — **SHIPPED 2026-07-09 (L-46…L-52)**

The missing data-shaping surface shipped: `map`, predicate-`filter`, aggregations (`sum`/`count_by`/
`group_by`/`any`/`all`/`has`), `flatten`/`skip`, `join`/`split`, object `pick`/`omit`/
`merge_obj`/`coalesce`/`keys`/`values`, and `regex_match`/`regex_extract` — all as pure ops
(per the evolution-doc precedent), powered by one shared predicate mini-language: the existing
`expr` engine extracted into `flux_lang::expr` with dotted access and list-aware builtins.
Native text can now say `when $count > 3` and `$ok = $score >= 0.8` without `@json`. Kills two
anti-patterns: (a) LLM cells prompted "Return ONLY a JSON array" as stand-ins for
deterministic map/filter, and (b) bespoke Rust boolean-emitter ops that only exist because
`expr` had no text spelling. Seven stories shipped in order: L-46 foundation → L-47/48/49/50 ops →
L-51 native conditions → L-52 docs/examples. Design:
[designs/data-transforms.md](designs/data-transforms.md).

### Flux-Lang CST front-end + LSP (epic) — ✅ **SHIPPED 2026-07-09 (L-57…L-70, L-73; closed)**

Editor-grade language support for Flux-Lang, in two coupled workstreams done as one parser pass.
**(1) Front-end:** a lossless concrete syntax tree (CST) on `rowan` (the rust-analyzer model) —
a layout-aware lossless lexer and a tolerant parser whose spans and error-recovery are
*structural*: every token/node carries a range and parsing always yields a complete tree with
`ERROR` nodes instead of aborting on the first error (L-57/L-58). In the same pass the **`@json`
syntax gap closed**: the 16 node kinds that formerly only round-tripped through the `@json` escape
(`memo`/`once`/`checkpoint`/`await`, `confirm`/`throttle`/`debounce`/`verify`, `peek`/`parse`,
`try`/`race`/`scope`/`saga`/`pipe`, `thing`) now have native text (L-60–L-63), round-trip- and
property-tested — `@json` remains only as the escape for unspellable shapes. **(2) flux-lsp:** a
standalone `flux-lsp` (tower-lsp) server wired into Helix (`hx`) config-only — diagnostics from
the CST parser, completion (ops/keywords/prelude/`$vars`), hover (op signatures + node-kind docs),
and formatting (L-64–L-67). Syntax **highlighting** ships separately as the sibling
[`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) grammar
(Helix/Neovim/Zed — Helix renders tree-sitter only, not LSP semantic tokens). The epic then closed
out in full: L-59 (`parse` re-pointed onto the CST), L-68 (symbols/go-to-def), L-69 (semantic
tokens, re-scoped to clients that render them), L-70 (incremental sync + comment-preserving format +
`dist = true`), and L-73 (the public editor-setup page). What each capability is worth *in practice*
is the subject of the follow-up epic below. Designs: [designs/flux-lang-cst.md](designs/flux-lang-cst.md),
[designs/flux-lsp.md](designs/flux-lsp.md).

### flux-lsp round 2 — from "capability present" to "capability correct" (epic) — ✅ **SHIPPED v0.30.1 (2026-07-28; L-85…L-91, all seven stories)**

The first LSP epic answered *"does `.flux` have editor support at all?"* — yes, and the binary
ships. Reading `crates/flux-lsp/src/main.rs` against what its `initialize` advertises answers the
next question less happily: several capabilities are advertised at full strength and implemented at
a fraction of it, always because a feature was built on the cheapest substrate that passed its test
while the lossless CST and the L-68 scope model sat next to it. **Completion never reads the cursor
position** (`main.rs:256-261`) — every request returns the union of every op, every node kind, every
prelude type, and every `$` byte-scanned out of the buffer, including variables from other flows and
from inside string literals, while go-to-definition on the same buffer is scope-correct. **Hover
resolves a word by scanning the raw line** (`word_at`, `main.rs:686`), so `read` inside a comment
renders the op card, and a `$var` never hovers at all. **References and rename don't exist**, though
the shadowing-aware resolution they need shipped with L-68. **Formatting is opt-out in two of three
cases**: modules return no edit (declaration order isn't recoverable from `Program`), and a
commented flow gets an indentation-only re-indent. **The catalog stops at the file edge**, so calling
a composite stored in `.flux/flows` — which the real host loads — squiggles as an unknown operation,
and every analyzer finding is a bare `WARNING` with no code. **"Incremental" is edit application,
not incremental reparse**, and each handler re-parses from text per request. Round 2 moves each
feature onto the tree that is already paid for: L-90 (parse cache + real incrementality) and L-85
(completion) first, then L-86/L-87/L-88/L-89, with L-91 splitting the 1800-line `main.rs` into the
modules the original design named and adding the in-memory-duplex harness that makes "advertised
capability without a handler" a test failure. **All seven landed.** Completion is cursor-aware and
scope-correct, hover resolves the CST token, references/rename exist and respect shadowing,
formatting is CST-driven so modules format and keep declaration order, diagnostics know workspace
composites and carry real severities and codes, and the document store caches the parse — a
`didChange` + completion + hover cycle on a 2,099-line buffer went from 3 parses (~38.4 ms) to 1
(~12.8 ms), with `parsing_is_confined_to_the_document_store` scanning the crate's own sources so
nothing can bypass the cache. `main.rs` is an 11-line bootstrap. Picking the epic back up turned up
the tail nobody had closed: the code had landed but every story was still `backlog`, and both
capability tables still described pre-round-2 behaviour — claiming modules were left unformatted and
listing neither references, rename, nor range formatting. Design:
[designs/flux-lsp-round-2.md](designs/flux-lsp-round-2.md).

### A2A protocol conformance (epic) — ✅ **SHIPPED (Tier 1 v0.4.2/0.4.3; Tier 3 v0.6.0; A-49…A-57 all done)**

After v0.4.0 (multi-tenant principal auth + multi-agent mount) the A2A wire surface was stable enough
to measure against the [spec](https://a2a-protocol.org/) (v0.3.0), so the gaps became a ranked backlog —
now **fully delivered**. The root of the early gaps was one deliberate choice: flux ran an A2A request
as one synchronous turn and returned a `completed` Task, with no retained addressable async task. Tier 3
removed that constraint. **Tier 1 (v0.4.2/0.4.3)** —
[A-49](stories/A-49-agent-card-conformance-fields.md) (the card gains `protocolVersion`, honest
`interfaces`/`preferredTransport`, optional metadata) and
[A-50](stories/A-50-a2a-error-codes.md) (A2A-specific error codes: `-32004` for unsupported methods,
`-32005` for unusable content). **Tier 2 (shipped)** —
[A-51](stories/A-51-inbound-multimodal-parts.md) (inbound file/data parts) and
[A-52](stories/A-52-outbound-task-fidelity.md) (`Task.history` + artifact emission). **Tier 3 (v0.6.0)** —
[A-53](stories/A-53-stateful-a2a-task-model.md), the stateful task model, plus the addressable-task
surface it unlocked: `tasks/get` non-blocking ([A-54](stories/A-54-addressable-tasks-get-nonblocking.md)),
`tasks/cancel` ([A-55](stories/A-55-tasks-cancel.md)), `tasks/resubscribe`
([A-56](stories/A-56-tasks-resubscribe.md)), and push notifications
([A-57](stories/A-57-a2a-push-notifications.md)). The remaining open slice is `input-required`/
`auth-required` (resume-on-`taskId`) — tracked in the living matrix. Non-goals: gRPC/REST bindings,
extensions negotiation, `tasks/list`. Living support matrix:
[a2a-conformance.md](a2a-conformance.md); design: [designs/a2a-conformance.md](designs/a2a-conformance.md).

### Postgres storage backend (epic) — **SHIPPED 2026-07-07 (v0.4.1; D-71…D-75)**

flux's durable persistence is embedded SQLite — the right default for CLI and demos, but server
deployments (multi-tenant managed-agent services, >1 replica, ephemeral or network-mounted disks)
need a shared, multi-writer-safe backend with real operational tooling. This epic adds **Postgres**
as a second backend for the two primitives deployments actually persist through — the unified event
log (`flux-events::EventStore`) and the datasource records store (`flux-capabilities::
DatasourceBackend`) — behind opt-in `postgres` features; the default build stays rusqlite-only and
never needs a database. Shape: one new L1 crate **[D-71](stories/D-71-flux-pg-bridge-crate.md)
`flux-pg`** owns sqlx, the pool, and a panic-safe sync↔async bridge (spawn onto an owned runtime +
mpsc-block — the only shape that survives plain threads, tokio workers, AND current-thread
runtimes); **[D-72](stories/D-72-eventstore-backend-seam.md)** splits `EventStore` into an internal
backend enum with the public API byte-identical (no trait — 23 consumer files hold it concretely);
**[D-73](stories/D-73-postgres-eventstore-backend.md)** implements the Postgres event log
(`BIGSERIAL` preserves the `s_<n>`/turn-id contracts, `payload` stays TEXT for byte-exact serde,
per-stream `pg_advisory_xact_lock` replaces Mutex+`BEGIN IMMEDIATE` and — new capability —
serializes appends **across replicas**) plus a run-twice conformance suite and a CI postgres job;
**[D-74](stories/D-74-postgres-datasource-backend.md)** adds the purely-additive
`PostgresBackend` (namespace-column-per-scope replaces one-file-per-scope;
`websearch_to_tsquery`+`ts_rank` for FTS5/bm25 parity); **[D-75](stories/D-75-eventstore-prune-older-than.md)**
adds the whole-store retention primitive the tag-scoped `prune_inactive` can't express. Critical
path D-71→D-72→D-73 (D-71 ∥ D-72; D-74 parallel after D-71). Non-goals: `FlowStore`/`ValueStore` on
Postgres (traits exist; on demand), pgvector `VectorStore`, MySQL, SQLite→PG data migration.
Design: [designs/pg-backend.md](designs/pg-backend.md).

### Time Machine (epic) — **SHIPPED 2026-07-07 (phases 0–3: C-43 · A-45 · A-46 · C-44; A-47 cockpit optional)**

The capstone of *the LLM is not the runtime*: because a flux run is a deterministic artifact (the
accepted plan of every turn already persists as re-parseable Flux-Lang, the execution core is
deterministic, and `RunEvent` is literally the "replayable record"), flux can do what no
LLM-as-runtime framework can — **hermetic replay, fork-at-any-decision, and run-diff of agent runs**.
The one missing piece is durability of op *outputs* (values are ephemeral today; only references
persist) — a redacted op-output "cassette" closes it. Three verbs: `flux replay <run>` (re-execute
exactly, offline, zero API spend), `flux fork <run> --at <node>` (branch and explore a different
path, live tail gated by the real approval envelope), `flux diff <A> <B>` (align two runs, show where
the plan or the world diverged). Phased C-43 (cassette capture) → A-45 (replay — the vertical slice
that proves it) → A-46 (fork) → C-44 (diff) → A-47 (optional TUI cockpit). Design:
[time-machine.md](designs/time-machine.md). Done = a `-m mock` run replays byte-identically with no
provider constructed, forks explore a divergent tail through the real envelope, and diff pinpoints
the divergence.

### Plugin distribution (epic) — **complete 2026-07-05 (D-46..D-49 all shipped)**

A flux user without the source tree had no way to obtain the integration plugin pack.
[D-21](stories/D-21-plugin-distribution.md) scoped the answer — **fetch-on-install from a signed
first-party pack channel** (bundling was rejected on coupling, not size) — and both sides shipped:
[D-46](stories/D-46-plugin-pack-release-pipeline.md) built the supply side (a `workflow_dispatch`
release pipeline packaging per-plugin per-target archives + a minisign-signed `plugins-index.json`
into `plugins-v*` GitHub releases; **plugins-v0.1.0 published 2026-07-03 with 87 signed assets**),
and [D-47](stories/D-47-remote-plugin-install.md) the demand side (released in 0.2.14): remote
`flux plugin install <name>[@version]` resolves the `plugins-v` release, verifies the signed index
(embedded pubkey, no skip flag), sha256-checks every archive before anything executes, and unpacks
into the versioned store `~/.flux/plugins/bin/<name>/<version>/` — live-verified with
`flux plugin install gitlab`. Epic design: [plugin-distribution.md](designs/plugin-distribution.md).
The final two stories completed the trust ladder (both shipped 2026-07-05):

- **[D-48](stories/D-48-enforceable-pin-rollback.md) — Enforced pin/rollback** · *Core, done.* Turn
  `flux plugin pin`/`rollback` from advisory labels into supply-chain statements: pin fetches through
  the verified D-47 path, repoints the descriptor, and records the hash; rollback is an offline flip
  to `previous`; the recorded sha256 is re-verified before **every** spawn (drift = hard refusal),
  with `status` gaining the verification column.
- **[D-49](stories/D-49-plugin-naming-docs-pass.md) — Plugin naming + docs truth pass** · *Core,
  done.* Apply the canonical trio vocabulary everywhere user-facing — the protocol *crate*
  (`flux-plugin`) vs the plugin *pack* (`flux-plugin-<name>` binaries) vs the *CLI* (`flux plugin …`)
  — and document the remote install path now that it ships (the C-16/L-19 docs-truth pattern).

### Stream resilience + provider-reported cost (epic) — **shipped 2026-07-04 (7/7, full gate green, live-verified)**

Parse resilience wave 2, filed 2026-07-04 after the user pasted a **fourth** turn-killing
`runtime error: step plan failed: serialization error: …` from an s_368-class deepseek session —
plus the permanent ` · $? (unpriced)` on every OpenRouter turn. A-32 hardened tool-**args**; the
SSE **envelope** parses stayed bare-fatal (`openai.rs:269`/`:870`, `messages/mod.rs:381`,
`bedrock.rs:236`), mid-stream errors are never retried, and `stream_blocks` discards accumulated
blocks + usage on the way out — so one malformed frame from a weak model still costs the whole
turn. The epic enforces the invariant **provider bytes never kill a turn** at three layers: a
planner backstop that turns classified decode errors into one retried step within the existing
budget ([A-33](stories/A-33-stream-decode-backstop.md)); per-codec skip+count+diagnostic envelope
tolerance with declared provider errors pinned fatal
([A-34](stories/A-34-openai-wire-envelope-tolerance.md) ·
[A-35](stories/A-35-messages-wire-envelope-tolerance.md) ·
[A-36](stories/A-36-bedrock-frame-decode.md)); and structural enforcement so the class can't
regress — a crate-local clippy ban on bare `serde_json::from_*` in flux-providers plus a
malformed-envelope corpus test ([A-37](stories/A-37-parse-enforcement.md)) — with
`FLUX_PLANNER_TRACE=1` forensics ([A-38](stories/A-38-planner-trace.md)). Riding ahead of the wave:
[C-34](stories/C-34-openrouter-reported-cost.md) prices turns from OpenRouter's own reported
`cost` (final usage frame, both wires) instead of the static table — `$? (unpriced)` disappears
for OpenRouter models with zero table maintenance. Epic design:
[stream-resilience.md](designs/stream-resilience.md) ·
[openrouter-reported-cost.md](designs/openrouter-reported-cost.md).

### Planner parse resilience (epic) — **shipped 2026-07-03 (3/3, gate green, live-verified on qwen3.7-max)**

Root-caused from session s_360 (2026-07-03): qwen3.7-max via OpenRouter **double-encodes
`emit_plan`'s `ast`** — a JSON string containing a perfectly valid plan — and flux's strict decode
rejects it on all 8 repair steps, killing the turn with the uninformative "planner did not produce a
plan within 8 steps". A live instrumented repro (s_361) confirmed the class (qwen3.7-plus too;
GLM 5.2 is a sibling; Sonnet unaffected) and surfaced three independent defects: no stringified-JSON
tolerance in the `EmissionArm::Json` decode ([A-30](stories/A-30-stringified-ast-fallback.md), the
one-line interop fix that would have made the whole turn succeed); the decode-`Err` and
hallucinated-tool branches never set `last_reject`, so the exhausted-budget error masks its own
cause ([A-31](stories/A-31-planner-reject-surfacing.md)); and `compile_turn`'s `Err` path drops the
accumulated `Usage`, so failed consultations persist **no** `call_usage` event and `flux usage`
undercounts exactly the most wasteful turns ([C-31](stories/C-31-planner-usage-on-error.md)). Done =
a qwen-shaped string-encoded plan compiles and runs like its object twin, every exhausted-budget
error names the last rejection, and failed planner turns are cost-accounted. Epic design:
[parse-resilience.md](designs/parse-resilience.md).

### Library hardening (epic) — **shipped 2026-07-03 (13/13, full gate green)**

Three adversarial subsystem audits (2026-07-03, one Opus reader each over the context-assembly,
evidence/event-store, and flux-lang/flow paths) surfaced 15 code-confirmed residual defects **inside
already-shipped stories** — every one carrying `file:line` evidence and a concrete failure scenario. The
headline three are 🔴 silent/security: an optimizer read-collector that drops `Obj`/`List` call args and so
parallelizes a reader with its writer / reuses a stale CSE value on the canonical named-arg form
([L-26](stories/L-26-optimizer-nested-arg-reads.md)); a `<knowledge-base>` body emitted verbatim so a
retrieved/poisoned RAG record can close the containment tag and inject top-level system content
([A-21](stories/A-21-knowledge-base-body-escape.md)); and the durable evidence trail persisted **unredacted**
so a `Bearer` token in a plan/bash arg lands in the clear in `events.db`
([C-22](stories/C-22-redact-durable-evidence-trail.md)). Then 🟠 enforcement/durability — the gather phase's
effect gate only blocks `Write`/`Destructive` not `Network`/`Process`
([L-29](stories/L-29-gather-effect-gate.md)), `events.db` has no `busy_timeout` so a serve-daemon + CLI
collide on `SQLITE_BUSY` ([C-25](stories/C-25-events-db-busy-timeout.md)), and the observation watermark
advances past failed writes ([C-24](stories/C-24-observation-flush-failure-watermark.md)); plus 🟡 accounting/
hygiene — sub-agent spend double-counted in the all-sessions rollups
([C-23](stories/C-23-subagent-usage-double-count.md)), served/agentic agents that never compact
([A-22](stories/A-22-served-agents-compaction.md)), the 4-breakpoint prompt-cache ceiling with no guard
([A-23](stories/A-23-cache-breakpoint-cap.md)), await/resume continuations with no turn telemetry
([C-26](stories/C-26-resume-turn-telemetry.md)), a ledger fast-forward that silently drops an
un-rehydratable binding ([L-28](stories/L-28-ledger-rehydration-guard.md)), analyzer positions the runtime
rejects ([L-27](stories/L-27-analyzer-contract-completion-r2.md)), and context byte-budgets that overshoot
their cap ([A-24](stories/A-24-context-byte-budget-overshoot.md)). Scoped to the **library core** — crate
release and the plugin platform (D-46..D-49) are explicitly out. Each ships with the failing-first test named
in its Acceptance; order: correctness/security → enforcement/durability → hygiene. Epic design:
[library-hardening.md](designs/library-hardening.md).

### Review hardening (epic) — 0.2.11 diff-review residuals — **shipped 2026-07-03 (12/12, released 0.2.12)**

An xhigh workflow-backed code review of the 0.2.11 diff (2026-07-03, 192 changed files: six finder angles
→ 38 candidates → an independent verifier per (file, line) → 15 reported) surfaced a batch of residual
defects **inside already-shipped stories**. Before filing, every finding was **grounded against flux's
stated invariants** by an independent Opus reader — and that grounding is the point of the epic: the raw
review ranked four "enforcement-boundary bypasses" as the gravest defects, and only one survived as
security. The one that did is 🔴 [C-27](stories/C-27-nested-destructive-refire.md) — the C-12
undisclosed-destructive re-fire gate keys on a bare shared depth counter, so a nested `run_plan` approved
`destructive:false` rides an outer plan's disclosure and a runtime-assembled `rm -rf` dispatches with no
prompt (a genuine approval-gate bypass, reachable via reflexive `run_plan`). The rest of the raw "gravest
four" were **corrected**: the composite hidden-op "bypass" is a legibility gap, not a security one — the
envelope holds and the gather gate is honored transitively ([L-30](stories/L-30-composite-surfacing-transitive.md),
🟡); the `parallel` cap-scope corruption is a real but **latent** soundness gap (`with_tools` is unused in
any shipped flow — [L-31](stories/L-31-cap-scope-parallel-position.md), 🟡); the nested-delegation cap-scope
escape is real but **opt-in only** (default `max_depth = 1` keeps every child a leaf —
[A-25](stories/A-25-nested-delegation-cap-scope.md), 🟠); and one candidate — the SQL_USERNAME
"regression" — was **withdrawn** entirely (a username is non-secret DSN metadata; plugins read no env; the
D-31 redesign was correct). The other confirmed 🔴/🟠 items are grounded correctness/robustness bugs against
documented contracts: `is_envelope_denial` misclassifying real tool failures as fatal denials
([L-32](stories/L-32-envelope-denial-classification.md)); the codex WS transport defeating its guaranteed
HTTP fallback three ways ([C-28](stories/C-28-codex-ws-fallback-hardening.md)); a markdown writer emitting
an early-closing fence ([L-33](stories/L-33-markdown-writer-fence-length.md)); the host-terminated SCRAM
handshake trusting an unbounded server iteration count ([D-52](stories/D-52-scram-iteration-bound.md)); the
A-10 turn budget measuring last-call occupancy instead of cumulative billed tokens
([A-26](stories/A-26-turn-budget-cumulative.md)); the A-05 identical-plan skip bypassing the stall guard
([A-27](stories/A-27-identical-plan-skip-stall-guard.md)); a queued a2a session pruned mid-flight into
orphaned, un-prunable events ([C-29](stories/C-29-a2a-queued-session-retention.md)); and a markdown list
swallowing a spaced thematic break ([L-34](stories/L-34-markdown-parser-thematic-break.md)). Each ships
with the failing-first test named in its Acceptance; order: security/correctness → robustness → hygiene.
Epic design: [review-hardening.md](designs/review-hardening.md).

### flux-lang v1 hardening (epic) — **shipped 2026-07-02 (C-17 + L-15..L-19 + the L-21 residual burn-down, gate green)**

A full review of the language pillar (2026-07-02: three scoped deep-dives plus first-hand
parser/spec reading and empirical round-trip probes) confirmed the architecture but surfaced 27
findings concentrated where a model-authored language hurts: a hidden-op bypass on the compile
path's plain-text plan fallback, an analyzer that under-delivers its documented contract (no
symbol definedness, accepts expression positions the runtime rejects, type checker unwired),
duplicated runtime eval paths that already diverged (`jq`), retry fatality defeated by error
re-wrapping, a **confirmed silent round-trip corruption** (`Var{"a.b"}` → `jq`), and spec/docs
describing behavior that doesn't exist. Done means: every finding fixed with a failing-first test
or honestly re-documented, `throttle`/`debounce` implemented fully, `lower()` type checking on
the production path, and a **node-catalog freeze** until definedness + diagnostic locators ship.
Epic design: [flux-lang-v1-hardening.md](designs/flux-lang-v1-hardening.md). Stories:
[C-17](stories/C-17-compile-path-plan-gates.md) (compile-path gates, P0) →
[L-15](stories/L-15-analyzer-unbound-vars-required-params.md) +
[L-16](stories/L-16-analyzer-contract-completion.md) (analyzer contract) ·
[L-17](stories/L-17-runtime-semantics-hardening.md) (runtime semantics) ·
[L-18](stories/L-18-roundtrip-totality-parser-locators.md) (round-trip totality) ·
[L-19](stories/L-19-flux-lang-docs-truth-pass.md) (spec truth pass) ·
[L-21](stories/L-21-flux-lang-v1-residual-burndown.md) (residual burn-down).

### Endpoint discovery & brokerage (epic) — **shipped 2026-07-02 (8/8 incl. D-31/D-32, gate green)**

flux's plugins each talk to a single, statically-configured service; the fluxplane pack they were modelled on
had **cross-plugin endpoint discovery**, which flux deferred in
[D-10](stories/D-10-process-plugin-protocol.md) (both it and the parity epic list a `.dex`-style endpoint
registry as a non-goal). This epic **reverses that deferral**. Its spine is a hard invariant — **a plugin
operation deals only in references**: it never reads, names, or receives an environment variable, never
receives a raw secret, never assembles a credential-bearing URL. Everything host-bound is an opaque,
host-managed `endpoint_ref` / `credential_ref`; the host alone resolves a reference and injects credentials,
so neither the plugin nor the LLM ever sees a secret value. Over that, the kubernetes plugin becomes an
endpoint **provider** (kubeconfig contexts → clusters; in-cluster services → prometheus/loki/grafana/
alertmanager/sql endpoints; RDS/crossplane secrets → credential *references*), and a consumer asks the host
*"which endpoints exist?"* → the host **fans out** to providers and returns weak refs. Epic design:
[endpoint-discovery.md](designs/endpoint-discovery.md). **[D-20](stories/D-20-scoped-private-net-egress.md) was
a hard prerequisite** (discovered endpoints are usually private/in-cluster hosts; ✅ shipped). Built in this
order (all six shipped, then [D-31](stories/D-31-host-terminated-rawsocket-auth.md) host-terminated SCRAM +
[D-32](stories/D-32-retire-url-handback.md) retired the URL handback):

- **[D-25](stories/D-25-endpoint-reference-model.md) — Reference model & registry** · *Core, leads.* ✅
  `EndpointRef` weak refs + `EndpointRegistry` (owner/TTL) + a static env/config resolver that moves env
  binding out of the plugin into host config (clean cutover). The spine; no discovery yet.
- **[D-26](stories/D-26-endpoint-discovery-broker.md) — Discovery provider role & fan-out broker** · *Core.*
  Manifest `discovers: [products]` + an `endpoint.discover` host capability; the broker matches a product and
  fans out to provider plugins, returning weak refs only.
- **[D-27](stories/D-27-reference-based-io.md) — Reference-based IO & host-injected connect** · *Core, needs
  D-20.* The protocol cutover that **enforces** the invariant — host IO takes an `endpoint_ref` and injects
  credentials host-side (incl. cross-plugin Kubernetes-scheme refs); cross-plugin credential use is
  deny-by-default + operator grant + first-use approval + audit.
- **[D-28](stories/D-28-kubernetes-endpoint-provider.md) — Kubernetes endpoint provider** · *Agent.* The
  reference provider; elevates the existing k8s discover/cluster/secret ops into a real provider.
- **[D-29](stories/D-29-migrate-plugins-to-references.md) — Migrate native plugins to references** · *Agent.*
  Clean-cutover every native plugin onto ref-based IO; the sql/observability consumers use discovered
  endpoints (multi-instance); `flux app run` + agent wiring.
- **[D-30](stories/D-30-endpoint-lifecycle-cli.md) — Endpoint lifecycle: refresh runner, CLI & audit** ·
  *Core.* Periodic rediscovery + `flux endpoint list/show/resolve` (weak refs + health, never secrets) + audit.

### Session `s_251` post-mortem — ctx-pack eviction & discovery aliases (epic) — **both fixes landed 2026-06-30**

A live `openai/gpt-5.5` session surfaced two compounding defects: an `endpoint.discover` "check db
connectivity" turn that returned `{"candidates": []}`, and the follow-up "analyze why it's broken" turn
that **looped 7 iterations and was cancelled**. Post-mortem design:
[session-s251-postmortem.md](archive/designs/session-s251-postmortem.md). The two fixes are independent but
both are needed for the "check db connectivity" path to be trustworthy:

- **[L-08](stories/L-08-ctx-pack-eviction.md) — Fix ctx-pack eviction** · *Language.* ✅ The `ctx` packer's
  greedy prefix-fill with a hard `break` drops every member after the first overflow, so one oversized
  early bind (a 493k session-evidence dump) starved the `ai.reason` step of the code reads the same
  flow had just gathered → the reasoning death spiral. Drop-and-continue + a value-aware keep priority.
- **[D-33](stories/D-33-endpoint-discovery-aliases.md) — Resolve cluster/namespace aliases** · *Agent.*
  ✅ (was blocked on the positional→kwargs cutover). `"dev"` isn't a kubeconfig context (it's a full
  EKS ARN) and the broker never relays structured `cluster`/`namespace`; `namespace=latest` is
  ambiguous with the newest-namespace heuristic. Provider alias resolution + broker query-parsing +
  disambiguating `latest`.

### Grounded knowledge (epic) — **shipped 2026-07-03 (3/3: A-19 + D-50 + D-51)**

flux's datasource layer (D-07) delivers knowledge to a model **only** as retrieval **tool calls** — there
is no way to hand a small KB to the model *inline*, and a bare agent (empty system prompt) is ungoverned
(the incident: a customer's empty voice agent free-associated about its operator from the base model's own
training). This epic adds the two reusable primitives a grounded-knowledge product needs, keeping retrieval
tool-based and unchanged: **[A-19](stories/A-19-context-block-injection.md)** — `add_context`, an
`AgentSpec.context` rendered into the system prompt as byte-budgeted `<knowledge-base id=… title=…>` blocks
(the greenfield inject seam); **[D-50](stories/D-50-text-file-chunking-ingester.md)** — a raw-text/file
chunking ingester so pasted text and uploaded text files become chunked `file.document` records; and
**[D-51](stories/D-51-local-embeddings-vector-store.md)** — per-KB, opt-in semantic search via an
in-process fastembed CPU embedder + a generic `VectorStore` seam backed by `sqlite-vec` co-located in the
same SQLite file (no external DB), turning on the existing `SemanticIndex`/`SqliteBackend` scaffolding.
Epic design: [grounded-knowledge.md](designs/grounded-knowledge.md). Consumer: a downstream
managed-agents service. Order: A-19 ∥ D-50 → D-51.

### Multi-pass agent loop (epic)

The turn loop one-shots a plan per iteration: the plan must be right on the first try, the user
stares at a silent wait while it composes, and a mid-plan failure **discards the whole plan** for a
from-scratch re-plan (a terminal-bench smoke functionally solved its task yet burned the
30-iteration cap stuck on one step; `s_251` above is the same shape). This epic restructures the
turn into visible passes — **orient** (the first planner call may answer, emit the full plan, or
emit a small read-only gather plan + a `brief` grounding artifact) → **bounded gather** (compile-
enforced read-only, capped) → **execute/revise** — and gives the runtime a memory of what already
ran: a failing statement is *reified* (structured halt + prefix transcript) into an append-only
**statement ledger**, so the model's corrected re-emission fast-forwards the hash-matching
completed prefix and **continues from the failure point**. The loop stays a flux-lang program
(no Rust loop returns); every re-emission re-passes the C-17 gates; denied statements are never
re-dispatched unchanged. Epic design:
[multipass-agent-loop.md](designs/multipass-agent-loop.md). Built in this order:
[A-12](stories/A-12-unsilence-planning-wait.md) (un-silence the planning wait — independent quick
win) → [A-13](stories/A-13-phase-aware-planner-protocol.md) (phase protocol) →
[A-14](stories/A-14-multipass-agent-loop.md) (the phased loop) →
[A-15](stories/A-15-phase-aware-surface.md) (surface) ∥
[L-22](stories/L-22-reified-halts-statement-ledger.md) (runtime ledger) →
[A-16](stories/A-16-loop-host-resume-policy.md) (resume policy) →
[A-17](stories/A-17-revise-wiring.md) (revise wiring, tracks join) →
[I-03](stories/I-03-multipass-cutover-measurement.md) (measured cutover gate). **Status
2026-07-02: the MVP (A-12–A-17 + L-22) is implemented, full gate green — I-03's measured verdict
is the remaining epic gate.** Later:
[A-18](stories/A-18-multipass-plan-mode.md) ·
[L-23](stories/L-23-streaming-plan-render.md) (after L-20) ·
[L-24](stories/L-24-reified-await-ledger.md) ·
[L-25](stories/L-25-flow-run-resumable-mode.md).

### Downstream enablement

A ranked track that exists to **unblock and de-risk downstream products** that consume flux by **path
dependency** (no version boundary, so flux churn breaks them directly; tightening these seams also eases
that coupling): multi-tenant managed-agent services and Slack-channel assistants. Sourced from cross-repo
audits; filed as the **D- story track** (see the [board](stories/README.md)). Slack-channel assistants
consume the shipped channel transport (D-04) and drive the **integration stack** (✅ all four
shipped) — built in this order: a knowledge/RAG datasource (**D-07**, which adds the shared
`flux-datasource` schema) → a clean
**process-plugin protocol redesign** (**D-10**) → a native integration-plugin pack (**D-08**, in an in-repo
`plugins/` workspace) → an agentic channel target (**D-09**). The app these consumers author is now a single
**native flux-lang `.flux`** file — `agent`/`channel`/`datasource`/`trigger`/`journey` module declarations
with secrets as `secret "ENV"` references, replacing the JSON manifest
([L-03](stories/L-03-native-text-program-grammar.md), [design](designs/native-text-modules.md)).

1. **[D-01](stories/D-01-flow-input-seeding.md) — Parameterized flow execution (the behaviour-runner
   seam)** · ✅ **shipped.** A deterministic `FlowClient::parse(text)` (no model round-trip) + a per-run
   input-seeding seam (`FlowStore::seed` + `FlowClient::execute_with`/`run_flow`) so a stored, validated
   Flux-Lang flow runs per invocation with effective-settings injected as `$vars` (not baked into the AST)
   and custom ops registered — fresh-store isolation, flow-local binds shadow seeds, the safety envelope
   unchanged; one-shot (genuine cross-turn `await` stays on the engine). Modules, zero new crates.
   Unblocks downstream behaviour-runner and preset-framework consumers. Design:
   [flow-input-seeding.md](designs/flow-input-seeding.md).
2. **[D-02](stories/D-02-tenant-event-substrate.md) — Tenant/context-taggable event substrate** ·
   ✅ **shipped.** Tag `flux-events` with an account/agent context + an account-scoped projection read API, so downstream
   run-persistence/transparency is a projection over the log, not a parallel store. "Build it in,
   not on" — decide while R-01 lands, or it's a retrofit.
3. **[D-03](stories/D-03-a2a-server-helpers.md) — Reusable A2A server helpers (current spec)** ·
   ✅ **shipped.** Lift flux-server's inline A2A routes (`message/send`/`message/stream`/`tasks/get`) into a reusable
   helper. Unblocks downstream A2A consumers **and** fixes drift where older consumers still serve the
   deleted `tasks/send` dialect (removed in the A-02 cutover, commit `06065f6`).
4. **[D-04](stories/D-04-event-trigger-channels.md) — Event-trigger channels (cron/webhook/Slack)** ·
   ✅ **shipped.** A `flux-channels` (L6) crate so agents **wake on external events** (schedule, webhook,
   Slack). Routes each event to a **journey** declared in the `.flux` program, run by `flux app run`
   (the App-runner route, superseding the design's `EngineTarget`; that agentic target is now **D-09**).
   Background agents woken by events; Slack-channel assistants consume the Slack adapter directly.
5. **[D-05](stories/D-05-sub-agent-hardening.md) — Harden the sub-agent primitive for multi-tenant
   production** · ✅ **shipped.** Closed the five gaps a downstream service hits: a consumable `flux-sdk`
   seam (`FlowClient::with_sub_agents` over a reusable `SubAgents` assembly — the CLI consumes the same
   helper), lifecycle limits (parent-cancellation threading + wall-clock-as-cancel + configurable
   `SpawnLimits`), a pluggable approver (`with_approver`) + a tested workspace-confinement isolation
   guarantee, and child tool calls threaded into a shared audit store (`with_audit`; the account tag +
   explicit parent-session link ride D-02). Isolation is per-scope composition, not new sandboxing.
   Unblocks multi-tenant sub-agent consumers. Design: [sub-agent-hardening.md](designs/sub-agent-hardening.md).
   Two lifecycle gaps documented (parent-turn cancel finalization; per-engine concurrent-turn cancel
   slot) — see the design's "Known limitations".
6. **[D-06](stories/D-06-realtime-voice-provider.md) — Realtime voice-to-voice as a first-class flux
   provider** · ✅ **shipped.** A **sibling, session-oriented provider seam**
   (`RealtimeProvider`/`RealtimeSession`, full-duplex) beside the half-duplex `Provider`, plus an
   OpenAI-Realtime impl ported from a downstream realtime client. Realtime tool calls route through the
   **same `Executor` envelope** with tools declared **once** from the live `ToolRegistry`, so downstream
   consumers can delete parallel voice-model stacks (bespoke WS clients, double tool-declaration, scattered keys).
   Built as **modules, zero new crates** (L0 `flux_core::audio`, L1 `flux_provider::realtime` +
   `flux_providers::realtime` behind a feature, L3 `flux_flow::voice`, SDK `FlowClient::run_voice_session`)
   + a Phase-2 engine-owned-turns spike (`run_flow_turns`/`VoiceTurnHandler`; per-turn `run_turn`, not yet
   cross-turn `await`). Downstream consumer rewiring is a separate pass outside this repo. Design:
   [realtime-voice-provider.md](designs/realtime-voice-provider.md).
7. **[D-07](stories/D-07-knowledge-datasource-rag.md) — Knowledge datasource (a real RAG layer)** ·
   *Slack assistant* · ✅ **shipped.** Turn `flux-capabilities::datasource` from an in-memory keyword index into a
   real knowledge layer: a new **L0 `flux-datasource` schema crate** (record/declaration/lookup, shared with
   the plugin layer), a persistent sqlite index, `search`/`list`/`get`/`relation`/`batch_get`, and
   reindex/freshness — keyword/BM25 behind a pluggable embeddings seam. Grounds Slack assistant answers in
   help-center + OpenAPI docs. Design: [datasource-rag.md](designs/datasource-rag.md).
8. **[D-10](stories/D-10-process-plugin-protocol.md) — Process-plugin protocol redesign** · *Slack
   assistant* · ✅ **shipped.** Redesign `flux-plugin`'s wire protocol/manifest/binding-SDK so a plugin can call ops,
   contribute & query **datasource records** (feeding D-07), and request host capabilities (HTTP with
   secret-by-purpose injection, process/env/blob/conn) over **one clean unified frame** — informed by
   fluxplane's evolved protocol but dropping its cruft (dual modes, three command families, per-call grant
   negotiation). Clean cutover of `flux.plugin.v1`. Blocks D-08. Design:
   [process-plugin-protocol.md](designs/integration-plugins.md).
9. **[D-08](stories/D-08-integration-plugin-pack.md) — Integration plugin pack** · *Slack assistant
   (epic)* · ✅ **shipped.** Native flux plugins (capability-gated, over the D-10 protocol) for the DevOps surface —
   Slack ops, websearch, GitLab, Jira, Confluence, Kubernetes, Loki, Prometheus — in an **in-repo
   `plugins/` cargo workspace** (excluded from root, so heavy deps stay out of the main gate; *reverses* the
   earlier sibling-repo plan). Each emits `flux-datasource` records reaching D-07's index via an L5
   `DatasourceHostCaps` bridge. Slice 1 (Slack ops + websearch) unblocks the assistant MVP. Design:
   [integration-plugins.md](designs/integration-plugins.md).
10. **[D-09](stories/D-09-agentic-channel-target.md) — Agentic channel target** · *Slack assistant* ·
    ✅ **shipped.** Let a channel wake an `AgentSpec` `run_turn` (model drives RAG + tools) **alongside** the
    shipped journey route, with per-conversation thread memory + declared op grants — builds the
    `EngineTarget` the D-04 design deferred, via a new `Deliverer` (the Slack adapter is unchanged). Also
    wires the `flux app run` path to **load plugins + register datasource tools** (today CLI-only). Design:
    [agentic-channel-target.md](designs/event-trigger-channels.md).

### fluxplane-plugins parity (epic) — **shipped 2026-06-30 (6/6: D-12..D-17, `plugins/` gate green)**

flux shipped **8** native plugins (D-08) over the D-10 protocol; the fluxplane pack they were modelled on has
**26 marketplace plugins**, and flux's 8 cover a fraction of their ops (gitlab 6/60+, slack 5/30, jira 3/~20,
k8s 5/24). This epic drives **full native parity**: every *portable* fluxplane plugin rewritten as a native
flux plugin at full op coverage, plus a generated plugin skill so the catalog is self-documenting. Builtin/
provider-covered plugins (clock/system/sleep/git/openai/ollama/duckduckgo/tavily) and fluxplane's
aggregator/generator surfaces (vision/websearch-aggregator/openapi) are explicit non-goals. Epic design:
[fluxplane-plugins-parity.md](designs/integration-plugins.md). Built in this order:

- **[D-12](stories/D-12-plugin-protocol-parity.md) — Plugin protocol parity extensions** · *core, leads.*
  Three additive host capabilities the missing plugins need: non-Bearer auth injection (Basic/header/query by
  purpose — Slice A), a guarded raw `conn.*` socket dialer (Slice B), and a `blob.*` store (Slice C). Clean
  extension of `flux.plugin.v1`; the dialer lives in flux-system. Gates D-15/D-16/D-17 and lets D-14 delete
  jira/confluence's hand-rolled base64. Design:
  [plugin-protocol-parity.md](designs/integration-plugins.md).
- **[D-13](stories/D-13-plugin-skill-command.md) — Generated plugin skill (`flux plugin skill`)** · *core.*
  Renders the installed plugin manifests into a Claude-format `flux-plugin` SKILL.md + `references/` (the
  flux analogue of fluxplane's `fluxplane-plugin skill`); adds a frontmatter writer to flux-markdown.
  Independent of D-12. Design: [plugin-skill-generation.md](archive/designs/plugin-skill-generation.md).
- **[D-14](stories/D-14-deepen-native-plugins.md) — Deepen the 8 native plugins** to their full fluxplane op
  sets (and drop the base64 hand-rolling). · *epic, per-plugin.*
- **[D-15](stories/D-15-observability-ai-plugins.md) — Observability & AI pack** (alertmanager, grafana,
  opsgenie, huggingface; HTTP, needs D-12 auth).
- **[D-16](stories/D-16-datastore-infra-plugins.md) — Datastore & infra pack** (sql, docker, aws; needs D-12
  conn + blob).
- **[D-17](stories/D-17-telephony-plugins.md) — Telephony pack** (asterisk, homer; serves downstream voice
  surfaces; asterisk needs D-12 conn).

### Subscription providers & cross-provider cost (epic) — **shipped 2026-07-02 (C-03..C-09 all done, C-07 live-verified)**

flux already drives the two **subscription / passthrough** model backends — `claude` (Claude Max / Claude-Code
OAuth) and `codex` (ChatGPT/Codex OAuth) — by **reusing the desktop apps' tokens** and refreshing them, with no
full interactive OAuth2 login (that was the deliberate later stage; C-08 closed it). `flux-credentials` imports from
`~/.claude/.credentials.json` / `~/.codex/auth.json`, refreshes via a 0600 store, and `-m claude|codex/...`
routes to them; the `claude` (Bearer + `oauth-2025-04-20` + Claude-Code system prefix) and `codex` (Responses
API on the ChatGPT backend) providers are wired. This epic **hardens** that against the live-backend quirks,
makes codex's **websocket** the default transport (HTTP fallback), and adds the missing cross-cutting piece:
**full usage + cost tracking across all providers**. Epic design:
[subscription-providers-and-cost.md](designs/subscription-providers-and-cost.md). Built in this order
(C-03/C-04/C-05 parallelize — mostly disjoint files):

- **[C-03](stories/C-03-codex-provider-hardening.md) — Codex provider hardening** · *core.* `account_id` from
  the `id_token` JWT claims (real `auth.json` nests it there → missing `chatgpt-account-id` rejects), cache +
  reasoning token capture in the Responses usage, and reasoning continuity under `store:false`. Foundation for
  C-07.
- **[C-04](stories/C-04-claude-401-refresh.md) — Claude verify + force-refresh-on-401** · *core.* Refresh today
  is expiry-time-only; add a single 401→refresh→retry path on the credential/`NativeProvider` seam (shared by
  both subscription providers), and a hermetic verify of the claude request shape.
- **[C-05](stories/C-05-pricing-cost-model.md) — Cross-provider pricing & cost model** · *core.* Per-model
  per-tier rates (input/output/cache-write/cache-read/reasoning) + `cost(&Usage, model)`; a **built-in table
  overlaid by `~/.flux/pricing.toml`**; normalize the OpenAI Chat/Responses codecs to populate cache fields
  (they zero them today). Subscription spend is labelled as *equivalent metered cost*.
- **[C-06](stories/C-06-usage-cost-accounting.md) — Usage & cost accounting** · *core, needs C-05.* Per-model
  attribution + sub-agent rollup + a `cost_summary` event-log projection + a `flux usage` command + a server
  endpoint + cache-aware CLI/TUI/server output. The full "usage + cost across all providers" surface.
- **[C-07](stories/C-07-codex-websocket-transport.md) — Codex WebSocket transport (default)** · *core, needs
  C-03.* WS (`wss://chatgpt.com/backend-api/codex/responses`) as the primary path with transparent HTTP-SSE
  fallback (a transport seam in `NativeProvider`; auth on the tungstenite handshake, per the realtime provider).
  Upstream WS is experimental — the fallback is non-negotiable and test-covered.
- **[C-08](stories/C-08-full-oauth2-login.md) — Full OAuth2 login (codex PKCE)** · *core.* ✅ A
  flux-native `flux auth login codex` to parity with claude's PKCE login. Initially deferred behind
  import + refresh; shipped last (2026-07-02) with a real PKCE flow — import stays the default.
- **[C-09](stories/C-09-aws-bedrock-provider.md) — AWS Bedrock LLM provider** · *core, DONE.* Drives
  Bedrock-provisioned Claude (`us.`/`eu.`/`global.` inference profiles) through the same harness:
  `flux run -m aws`. The wire is native Anthropic Messages (streaming `invoke-with-response-stream`;
  a CRC-checked event-stream deframer feeds the shared SSE mapper), SigV4 + codec +
  `BedrockCredentialsResolver` hand-rolled in L1 (`flux-providers::bedrock`). The Option-C plugin
  was **reversed in implementation**: the credential chain (env → SSO w/ OIDC refresh → IRSA → EKS
  Pod Identity) is hand-rolled in L1 over `std::fs`+`reqwest` (the flux-credentials trust-boundary
  precedent — the plugin sandbox env-clears and can't walk the chain), so flux ships **zero AWS SDK
  deps** and needs no `aws` CLI in dev or prod. Pricing keys the region-less Bedrock id (every
  regional profile prices identically, metered). The C-09a protocol knobs (`internal` op flag,
  path-scoped `fs.read`) landed for other plugins' benefit. Live-verified e2e on the dev account
  (SSO, eu-central-1) incl. tool-use turns and cost suffixes.
  Design + implementation status in [aws-bedrock-provider.md](designs/subscription-providers-and-cost.md).

### Strict review flows & journeys (epic) — **shipped 2026-07-01 (4/4, `flux review` live)**

A skill can *advise* a reviewer, but a review protocol needs guarantees — fixed step order, a bounded
tool set per phase, sub-agents on a frozen context instead of ambient workspace authority, and
deterministic aggregation. This epic expresses **strict code review as an enforced Flux-Lang flow**
rather than prompt convention, matching the project invariant that *the LLM is not the runtime*:
prompt guidance may inspire the protocol, but the executable flow and runtime policy enforce it.
"Done" is a reusable `strict_review` flow that reads only the requested context read-only, fans out
to capped reviewer sub-agents, aggregates typed findings deterministically into a `ReviewReport`, and
fails closed on any undeclared tool — reachable both directly and as a `flux-app` journey. Epic
design: [strict-review-flows.md](designs/strict-review-flows.md). Built in four phases:

- **[L-10](stories/L-10-strict-review-example-flow.md) — Example flow + reviewer roles** · *Language,
  leads.* ✅ The `strict_review` flow + role files using only existing primitives (context
  gather → capped fan-out → deterministic dedupe/rank), proving the runtime contract with no language
  change. Sub-agent tool restriction stays at the role level here.
- **[L-11](stories/L-11-strict-review-scoped-capabilities.md) — Scoped capabilities (`with_tools`)** ·
  *Language.* An analyzer-visible capability-scope node threaded into `Executor::dispatch` so a tool
  outside the active scope fails closed (session ∩ AgentSpec ∩ flow ∩ block ∩ sub-agent), with
  entry/exit and denials in the evidence log. The feature that makes this not-just-a-skill.
- **[L-12](stories/L-12-strict-review-typed-artifacts.md) — Typed artifacts + deterministic
  aggregator** · *Language.* `ReviewRequest`/`ReviewFinding`/`ReviewReport` + `review.normalize`/
  `review.aggregate` (fingerprint/dedupe/rank, malformed→gap, stable ordering); the model does prose
  synthesis only, against a fixed schema.
- **[L-13](stories/L-13-strict-review-journey-cli.md) — App journey + CLI & CI surfaces** · *Agent.* A
  `flux-app` `review_code` journey + optional `flux review` command + CI output modes (markdown/JSON/
  nonzero exit on high severity); the journey path and the direct flow path produce the same report.

**Candidate phases (vision tail, in priority order):**
- **Crate consolidation** ✅ **all phases shipped** — shrank the workspace by merging coherent
  *same-layer* siblings (layering lint stayed green throughout). Phase 1 collapsed the five L1 provider
  crates into `flux-providers` (37→33). Phases 2–4 folded `flux-hooks`→`flux-plugin`,
  `flux-browser`+`flux-datasource`→`flux-capabilities`, `flux-context`→`flux-runtime`, and removed the
  dead `flux-integrations` (the workspace had drifted to 35; landed at **31**). `flux-auth` was kept
  standalone (caller identity ≠ tool capability). See
  [designs/crate-consolidation.md](designs/crate-consolidation.md).
- **Dogfood & harden** (tier 1) — drive flux's agentic mode on real coding work, capture friction as
  issues, and fix the top biters. Validates the daily-driver claim on real tasks.
  - **Generic `bash` is now opt-in** (off-by-default `shell` group; `enable_shell`/`FLUX_ENABLE_BASH`/
    `/shell`). Session-data analysis drove the dedicated-op coverage that makes default-off viable:
    `expr` extended with comparison/boolean/string ops, `now`/`cwd`/`sys_info`, `len`/`first`/`last`/
    `filter`, and the `go`/`node`/`python`/`make` toolchain ops. See
    [archive/designs/bash-replacement.md](archive/designs/bash-replacement.md).
  - **The Flux-Lang agent loop is authored, adaptive, and observable.** The shipped
    `agent-loop.flux` owns `detect_intent → explore → approve_batch → execute_batch →
    present_results`; models use native operation schemas and never emit executable Flux.
    `flux run --show-loop` reveals those stages, the REPL `/evidence` prints the audit trail, and
    `flux loop show`/`eject` displays or scaffolds an explicit loop. A local file is selected only
    when requested with `--loop`/config; there is no magic override. See
    [agent-loop.md](agent-loop.md) and [adaptive-outer-loops.md](designs/adaptive-outer-loops.md).
- **SDK + crates.io** (tier 2) — **P7 landed the bulk:** a **Rust eDSL** (`flux_lang::dsl`, re-exported
  as `flux_sdk::dsl`) whose builder primitives compile to the Flux-Lang AST — loops
  (`each`/`repeat`/`loop_for`/`race`) and control-flow (`match`/`route`/`fallback`/`timeout`/`budget`)
  first-class, all node kinds covered (43 today, drift-guarded by `dsl_covers_every_node_kind`), authored in
  Rust then run through the existing `FlowClient` lifecycle. The public API is **stabilized**
  (`#![warn(missing_docs)]`, crate READMEs, three runnable no-API-key examples, crates.io metadata) and
  **publish-prepped** (the 16-crate closure carries versions; topo order + runbook in
  [`crates/flux-sdk/PUBLISHING.md`](../crates/flux-sdk/PUBLISHING.md); `cargo package` validated).
  A **recipe cookbook** (`flux_sdk::recipes` — routing/lookup/batch/resilience/fanout/dispatch/compose:
  reusable, parameterized flow builders) was then folded into the SDK and made **✅ reachable from the
  binary** via the **`flux preset`** subcommand (`list`/`help`, scaffold a recipe to a tree or JSON, or
  `--run` it through the envelope; op-resolution gates offline-runnability) — the DSL/recipes line is no
  longer library-only. **Blocked on a name decision before publishing:** the crate name `flux-core` is already taken on
  crates.io by an unrelated project — the namespace must be vanity-prefixed (`codewandler-flux-*`) or
  `flux-core` renamed (see the runbook §1). The real `cargo publish` is left to the maintainer (token +
  irreversible).
- **flux-lang evolution — ✅ shipped** (P0–P6 + flux-app): the agent-cognition layer landed — the
  artifact **prelude** (11 `Named` types), `ctx`/`ctx_append` context-pack nodes (36 node kinds),
  op-input JSON Schema, typed HIR with arg type-checking (`analyze::lower`), the **text parser**
  (`parse`/`format`) and **optimizer** (`optimize` + `PhysicalPlan` execution); the **`flux-cognition`**
  (L3) model-op pack and **`flux-app`** (L6) multi-agent runtime host (`flux run app.flux`,
  deny-destructive by default); and the **`flux-sdk` `FlowClient`** lifecycle. **P6** added **`await`
  cross-turn suspend/resume**, the **Tier-1 control-flow primitives** (`match`/`route`/`fallback`/
  `timeout`/`budget`), and polish (`fluxlang compile`, token-efficient `format_compact`, a deterministic
  thing resolver). See [designs/flux-lang-evolution.md](designs/flux-lang-evolution.md) and the
  [PRD status RTM](../crates/flux-lang/docs/STATUS.md). **P7** added the **Tier-2 control-flow
  primitives** — `scope` (RAII cleanup), `saga`/`compensate` (reverse-order unwind), `once`
  (at-most-once side effect), `checkpoint` (durable resume point) — on a narrow `DurableStore` seam
  (`FlowStore` folds them out of the append-only event log), plus a **dead-step optimizer pass**
  (drop read-only binds whose result is never used) and **common-subexpression elimination** (dedupe an
  identical read-only, deterministic call into a `Stage::Alias` — one dispatch, reused result).
  **P8** removed the language's top authoring friction: `bind` now accepts a `var` (`$b = $a` alias)
  or `lit` (`$x = 5`/`[1,2,3]`/`{…}`) directly, and two pure **value-template** nodes (`obj`/`list`)
  let a record/list assemble from variables (`return { ok: true, n: $count, intent: $x.intent }`) —
  43 node kinds today. Remaining (optional): native `{k:expr}`/`[expr]` text spelling + a strict-JSON-schema
  vs. native-text **emission A/B** (measure planner accuracy before switching the model's surface);
  deeper optimizer passes (predicate pushdown, batch/model-call fusion); `checkpoint`∘`await`.

**Environment-gated (need a live key or external infra):**
- **Homebrew tap** — an auto-updating `brew install codewandler/tap/flux` formula via cargo-dist
  (`publish-jobs = ["homebrew"]` + `tap`/`formula` in `dist-workspace.toml`); needs a
  `HOMEBREW_TAP_TOKEN` PAT with push access to a `codewandler/homebrew-tap` repo.
- Switch `openai`'s default wire from Chat to Responses, verified with a live round-trip.
- `web_search` server tool; live token-count endpoint.
- Wire a real OIDC IdP behind the existing `OidcIdentity` seam (the multi-user platform tier).

**Deferred behind existing seams (add on concrete demand):**
- A `deno_core` / `rustyscript` hook backend (async / TypeScript / npm) behind the `PreToolHook` seam.
- A `chromiumoxide` CDP browser tool (navigate/screenshot; needs Chrome) behind `flux-capabilities`' `browser` module.

## Known divergences / decisions pending

Drift made visible, so it stops being silent. Each maps to a story on the
[board](stories/README.md):

- ~~**Plugin ops still bind to env-var names + receive raw URLs; no cross-plugin endpoint
  discovery.**~~ ✅ done — plugin IO is references-only (opaque `endpoint_ref`/`credential_ref`,
  host-side resolution + credential injection, the URL handback deleted), with cross-plugin
  discovery fan-out and the `flux endpoint` operator CLI. → endpoint discovery & brokerage
  ([D-25](stories/D-25-endpoint-reference-model.md)..[D-32](stories/D-32-retire-url-handback.md)).
- ~~**Two turn loops.**~~ ✅ done — every surface (CLI/TUI/server/SDK) runs the pure-DAG
  `FlowEngine`; the classic Rust loop is retired. → [A-01](stories/A-01-unify-flowengine.md).
- ~~**Crate consolidation phases 2–4**~~ ✅ done (35 → 31). → [C-01](stories/C-01-crate-consolidation.md).
- ~~**crates.io publish** blocked on the `flux-core` name~~ ✅ done — the whole publish closure ships
  as vanity-prefixed `codewandler-flux-*` (import paths unchanged), published by CI on every version
  tag (`scripts/publish-crates-io.sh` is the ordered, idempotent source of truth).
- **Self-improvement headline gain** still lacks a trials ≥ 3, grader-confirmed result; the
  initiative is **ON HOLD / de-prioritized since 2026-07-06** (machinery proven, gain unproven).
  → [I-01](stories/I-01-headline-gain.md).
- ~~**No cost tracking.**~~ ✅ done — per-call usage is attributed (`CallUsage`, canonical
  provider/model keys), priced via the built-in table + `~/.flux/pricing.toml`, and reported
  (`flux usage` incl. the per-turn efficiency line, turn-end cost annotations, a server endpoint).
  → [C-05](stories/C-05-pricing-cost-model.md) / [C-06](stories/C-06-usage-cost-accounting.md) /
  [C-15](stories/C-15-efficiency-metrics-and-key-normalization.md).
- ~~**Codex transport is HTTP-SSE only**~~ ✅ done — WS is the default codex transport (live-verified
  wire contract) with transparent HTTP-SSE fallback. → [C-07](stories/C-07-codex-websocket-transport.md).
- ~~**Subscription-provider login is import-only for codex**~~ ✅ done — `flux auth login codex` runs
  a real PKCE flow to parity with claude (import + refresh stay the default path).
  → [C-08](stories/C-08-full-oauth2-login.md).

## Backlog (product improvements)

- ~~**Load skills from a user/global dir**~~ ✅ done — skills load from the project `.flux/skills`
  **and** the user-global dirs (`~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`; project wins),
  in both the flux-native and Agent-Skills/Claude formats. → [L-01](stories/L-01-global-skills.md).

## Direction

The through-line is **the LLM is not the runtime**: the model is a compiler front-end that emits a
Flux-Lang plan, and the deterministic engine runs it — **non-bypassable safety** is the hard
invariant that buys. Priority is **personal coding agent → reusable SDK → multi-user platform**. See
[vision.md](vision.md). The annotated original design & planning document (with full
milestone-by-milestone detail) is retained outside the repo by the author; this roadmap is the
in-repo canonical summary.
