# flux — roadmap & status

Status as of **0.29.0 (2026-07-28)**: public + installable at
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
> **v0.28.0** is released — that MINOR carried the pub-surface breaks from C-97/C-103/C-104/C-117
> and the D-192 flux-skill removals. See [CHANGELOG.md](../CHANGELOG.md) for the itemized history.

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

### LLM cache review — prompt-cache correctness for `claude` and `codex` (epic) — ✅ **IMPLEMENTED 2026-07-28 (C-133…C-140 + A-95, all nine stories; unreleased)**

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

### flux-lsp round 2 — from "capability present" to "capability correct" (epic) — 🔜 **PLANNED 2026-07-29 (L-85…L-91, seven stories)**

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
capability without a handler" a test failure. Design:
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
