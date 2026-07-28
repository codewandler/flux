# Amp — manual read and feature extraction

**Source:** [ampcode.com/manual](https://ampcode.com/manual) · **Observed:** 2026-07-28 ·
**Kind:** vendor manual.

A vendor manual documents *intent* as well as shipped behavior, and says nothing about how well any
of it works. Nothing here is a benchmark or an endorsement — this is a **mining record**: read a
mature competitor's whole surface, decide feature by feature whether flux should want it, and leave
the reasoning behind so the next pass starts from the verdicts instead of the manual.

Amp is the closest competitor to flux by *shape* (terminal-first agent, plugin system, skills,
review, AGENTS.md) and the furthest by *thesis* — see [Positioning](#positioning) at the end.

Supersedes the Amp profile in [`../archive/research/profiles-cli.md`](../archive/research/profiles-cli.md)
(observed 2026-06-25).

---

## Method — and where it is weak

Stated so the verdicts can be weighed, and so a later pass knows what to redo rather than repeat.

**What was done:** the manual was read once through a summarizing fetch (not the raw page); every
extracted feature was bucketed `mine` / `have` / `reject`; each `mine` was then grounded against the
flux tree with `file:line` evidence before being filed.

**Three biases, found on review, partly corrected here:**

1. **The evidence was asymmetric.** Every `mine` carries verified `file:line` evidence that the gap
   is real. The `have` and `reject` verdicts were largely asserted. That is backwards: a wrong
   `mine` costs one story that gets closed, while a wrong `have` kills a candidate silently and
   permanently. Re-checking the `have` column found three errors — session search is **not**
   covered by `flux sessions` (which has exactly one flag, `--prune`), git commit trailers are a
   *policy* reject rather than a `have`, and custom keybindings were labelled `partial` while the
   note said "fixed". All three are corrected below. Treat any remaining `have` as
   lower-confidence than any `mine`.
2. **Selection favoured what is cheap to build in flux.** The original write-ups justify inclusion
   with "adds zero authority", "the two hard halves already exist", "small by design" — that is
   *architectural fit*, not *user value*. Fit is a real input, but it was doing most of the work.
   The [value axis](#value-axis) below re-ranks the same candidates by how often a flux user would
   actually feel the absence, which produces a materially different order.
3. **The rejects were rejected as a bloc, on one axis.** Everything cloud-shaped was dismissed as
   "contradicts local-first" — which conflates the *hosted mechanism* (correctly rejected: flux has
   no backend) with the *user need underneath* (often real, and sometimes answerable locally). The
   [rejects section](#deliberate-rejects) was restructured to separate the two, and doing so
   surfaced two candidates the bloc rejection had buried.

**Known limitations of this record:** the manual was read via one summarizing pass, so a feature
described only in a sub-page or in passing may be missing entirely; nothing here was verified by
*using* Amp; and a vendor manual describes intent, not quality of execution.

---

## What changed since the 2026-06-25 snapshot

The archived profile describes Amp as *"curated multi-model, no BYO key; MCP + TS plugins; hosted
approval model; first-class subagents"*. Those facts hold, but the surface around them has grown
substantially. Materially new or newly-documented:

- **A TypeScript plugin system with event hooks** — `.amp/plugins/*.ts` handling `session.start`,
  `agent.start`/`agent.end`, `tool.call`, `tool.result`; registering custom tools *and* custom
  commands; driving UI (notify / confirm / input / select); and creating custom agent modes.
- **User-defined review checks** — `.agents/checks/` criteria files with frontmatter carrying
  severity and per-check tool access, project-over-global precedence, driven by `amp review`.
- **Agent skills** — `.agents/skills/` and `~/.config/agents/skills/`, with precedence rules and
  the ability to bundle an `mcp.json` per skill.
- **AGENTS.md as the guidance format** — with relative `@`-mentions for inclusion and **YAML
  frontmatter `globs`** on mentioned files for path-scoped guidance; explicit migration from
  `CLAUDE.md` / `.cursorrules`.
- **A `--stream-json` line protocol** — NDJSON out, `--stream-json-input` for multi-message drive,
  `--stream-json-thinking`, and in-band steering via a `{"steer": true}` attribute.
- **Named specialist subagents** — Oracle (a separate, higher-reasoning consulting model),
  Librarian (cross-repo GitHub search), Painter (image generation), Code Review.
- **Speed/effort modes** — `low` / `medium` / `high` / `ultra`, plus a fast-mode toggle.
- **Cloud execution** — orbs (remote machines), runners (headless remote thread creation),
  multiplayer collaboration, remote control from web/mobile, workspace-shared threads.
- **Agent-set schedules** — the agent can schedule its own wake-up with prompt and context preserved.
- **MCP with OAuth** — automatic browser flow, workspace-approval requirement, `includeTools`
  filtering, per-server permissions.
- **Permissions posture stated plainly** — *no approval required by default* for tool calls;
  `amp.permissions` is described as legacy; `amp.dangerouslyAllowAll` exists; guarded-file
  allowlists and plugin-authored policies are the customization path.

---

## Feature inventory and verdicts

`mine` = worth having, filed · `have` = flux already ships an equivalent · `reject` = deliberately
not wanted, reason given.

| Amp feature | Verdict | flux |
|---|---|---|
| `--stream-json` / `--stream-json-input` / in-band steering | **mine** | [C-160](../stories/C-160-ndjson-agent-protocol.md) |
| Review **checks** (`.agents/checks/`, severity + tool scope) | **mine** | [C-161](../stories/C-161-user-defined-review-checks.md) |
| **Oracle** — consult a stronger model for advice | **mine** | [A-96](../stories/A-96-second-opinion-op.md) |
| Glob-scoped guidance in AGENTS.md-mentioned files | **mine** | [A-97](../stories/A-97-path-scoped-guidance-fragments.md) |
| `amp.tools.disable` — tool-name blocklist | **mine** | [C-162](../stories/C-162-tool-disable-list.md) |
| Plugin-registered commands + host UI prompts | **mine** | [C-163](../stories/C-163-plugin-commands-and-host-ui.md) |
| Agent-set self-waking schedules | **mine** | [A-98](../stories/A-98-agent-set-wakeup.md) |
| Thread finding — keyword / file / repo / date filters | **mine** ⟵ *was wrongly `partial`* | [C-164](../stories/C-164-session-search.md) |
| Managed settings — an enforced, non-overridable baseline | **mine** ⟵ *was wrongly bloc-rejected* | [C-165](../stories/C-165-managed-config-tier.md) |
| Command palette (Ctrl+O) over all commands | **mine** (folded) | scope note added to [C-153](../stories/C-153-overlay-fuzzy-filter-and-overflow.md) |
| MCP client + server, OAuth, tool filtering | **mine** (already filed) | [D-193](../stories/D-193-mcp-interop-epic.md) |
| Skills — project + user-global, precedence, frontmatter | **have** | `.flux/skills`, `.claude/skills`, `~/.flux/skills`; D-186…D-192 |
| AGENTS.md / CLAUDE.md as project guidance | **have** | `ProjectFiles` (`crates/flux-runtime/src/context.rs:29-73`) |
| Speed / effort modes (`low`…`ultra`) | **have** | `--effort Low\|Medium\|High\|Xhigh\|Max` (`crates/flux-cli/src/args.rs:103-106`) |
| Subagents with isolated context | **have** | policy-bounded sub-agents, `task`, `.flux/agents/*.md` roles |
| Code review as a first-class command | **have** | `flux review` (L-13 strict-review protocol) |
| Cost display + `amp usage` | **have** | turn cost line, `flux usage` |
| Session continuity + time travel | **have** | `/resume`, `flux replay` / `fork` / `diff` |
| Git commit trailers (thread / co-author) | **reject** | deliberate: flux does not stamp commits |
| Custom keybindings | **reject** (low value) | TUI keybindings are fixed; nobody has asked |
| Config env interpolation (`${VAR}`) | **deferred** | real gap, but touches the C-76 secret-consent surface — see below |
| Plugin-invoked model calls (`amp.ai.ask`) | **deferred** | grants a plugin *spend*, not just capability — needs C-130 first |
| Voice input, image paste | **not mined** | `flux-audio` exists; out of scope for this pass |
| Threads, visibility levels, workspace sharing | **reject** | cloud-coupled, account-bound |
| Orbs, runners, multiplayer, remote control | **reject** | hosted execution; see below |
| Slack "Puck" personal assistant | **reject** | flux's Slack surface is a channel adapter, not a hosted bot |
| Painter (image generation) | **reject** | plugin territory, not core |
| Librarian (cross-repo GitHub search) | **reject** | plugin territory; the gitlab/github plugins own this |
| Curated models, no BYO key | **reject** | inverse of provider neutrality |
| No-approval-by-default, `dangerouslyAllowAll` | **reject** | inverse of the default-deny envelope |
| Enterprise SSO, zero-retention, IP allowlists, analytics API | **out of scope** | no hosted plane to attach them to |

---

## The mined candidates

Each subsection states what Amp does, what flux has **today** with `file:line` evidence, and why the
delta is worth closing. Evidence anchors were read on 2026-07-28; re-verify before acting on them.

### 1. An NDJSON agent protocol — [C-160](../stories/C-160-ndjson-agent-protocol.md)

**Amp:** `--stream-json` emits one JSON object per line; `--stream-json-input` accepts the same
framing on stdin for a multi-message conversation; `--stream-json-thinking` includes reasoning
blocks; and an input line carrying `"steer": true` injects guidance into the running turn. The
manual demonstrates composing input with `jq`.

**flux today:** `AgentFlags` (`crates/flux-cli/src/args.rs:77-250`) has **no output-format flag at
all**. `--format json` exists on exactly one subcommand, `flux review`
(`crates/flux-cli/src/args.rs:377-380`). A non-Rust caller wanting to observe a turn must scrape
human-formatted prose or reach for `flux-sdk` (Rust), the HTTP server, or A2A.

**Why it matters:** this is the cheapest available increase in flux's addressable surface. The two
hard halves already exist — the durable event stream (`flux-events`, the "replayable record") and
mid-turn steering (**A-94**, merged) — so the work is a *projection* and a schema, not new
machinery. It also makes flux embeddable by other agents and by CI without a Rust dependency, which
is the same interop argument that motivated **D-193** (MCP) and the Claude-interop epic.

**Constraint flux must not lose:** every emitted line crosses a trust boundary, so redaction is
enforced *on the protocol*, not assumed from the renderer.

### 2. User-defined review checks — [C-161](../stories/C-161-user-defined-review-checks.md)

**Amp:** `.agents/checks/*.md`, each one criterion, with frontmatter for severity and the tools that
check may use; resolved from project, API-subdirectory, and global locations with project winning.

**flux today:** `flux review` runs the L-13 strict-review protocol with its reviewer roles and the
`strict_review` flow text **embedded in the binary**, so it works in any repo
(`crates/flux-cli/src/review.rs:100-108`). A project can override the *roles* wholesale via
`.flux/agents/review-*.md`, but there is no way to add a single project-specific criterion with its
own severity and path scope. `--fail-on <severity>` already exists
(`crates/flux-cli/src/args.rs:381-384`), so severity is already plumbed through to the exit code.

**Why it matters:** the override-the-whole-role escape hatch is too coarse for the common case
("also check that every new op declares its effects"). And the pieces to build it are already in the
tree — frontmatter parsing, glob scoping, and name-collision precedence all shipped with the skills
work (D-186…D-192).

### 3. A second-opinion op — [A-96](../stories/A-96-second-opinion-op.md)

**Amp:** the **Oracle** — a separate, higher-reasoning model the agent consults on hard problems,
deliberately distinct from the working model. The manual recommends invoking it explicitly and
treats it as one of the product's highest-value tools.

**flux today:** provider/model routing across five providers with credential reuse
(`crates/flux-cli/src/args.rs:82-97`) and policy-bounded sub-agents. But every escalation path flux
has is an *actor* — a sub-agent has tools, a workspace, and a policy scope.

**Why it matters, and why it fits flux specifically:** a consult op is **pure**. Its only outbound
path is a model call; it reads nothing, writes nothing, spawns nothing. It therefore adds *zero new
authority* to the safety envelope — which makes it the single cheapest capability addition available
under flux's constraints. It is also where provider neutrality converts into a user-visible feature:
the second opinion can come from a different vendor than the working model, which a vendor-locked
agent structurally cannot offer.

**The trap:** the answer is model output from elsewhere and enters context as untrusted content. It
must not be able to close a containment tag — the **A-21** lesson, which cost flux a security fix.

### 4. Path-scoped guidance fragments — [A-97](../stories/A-97-path-scoped-guidance-fragments.md)

**Amp:** AGENTS.md files include other files by relative `@`-mention, and a mentioned file may carry
YAML frontmatter `globs` so it loads only for matching paths.

**flux today:** `ProjectFiles` reads a fixed three-file list — `CLAUDE.md`, `AGENTS.md`,
`.flux/context.md` — **whole and unconditionally, every turn**
(`crates/flux-runtime/src/context.rs:29-73`). Skills carry `triggers`, `allowed_ops`, and a `model`
override (`crates/flux-skill/src/lib.rs:53-78`), but nothing carries a path scope.

**Why it matters:** in a repo the size of flux's own, guidance is a forced choice between thin
(the agent misses subsystem rules) and complete (the relevant rule is buried, and every turn pays
for all of it). Path scoping removes the tradeoff.

**The trap, and it is a real one:** guidance sits in the *stable prompt prefix*. A fragment that
loads or unloads mid-session is a cache invalidation, and the C-133…C-140 work just finished
proving how expensive that is — A-95 exists because one no-op capability signal was churning the
prefix for nothing. Scope resolution must happen **once per turn**, before the prefix is built.

### 5. A tool blocklist — [C-162](../stories/C-162-tool-disable-list.md)

**Amp:** `amp.tools.disable`, a glob-supporting list of tool names to turn off.

**flux today:** tool groups (`crates/flux-evidence/src/lib.rs:185-193`, manifested in
`.flux/groups.toml` per `crates/flux-config/src/lib.rs:1028`) are **purely additive** — `tools` plus
`surface_when` signals decide when an op family *appears*. Nothing subtracts. To keep an op away
from the model you write authorization policy.

**Why it matters:** "I never want `browser.*` in this repo" is a question about *surface*, not
*authority* — prompt size and prompt-injection target area, not permission. Making users learn the
policy language to express it is a usability failure, and policy is the wrong instrument besides.

**The line to hold:** this is defense-in-depth and prompt hygiene, **not** a security boundary. The
policy stays the security control, the docs must say so plainly, and if the two ever disagree the
policy wins.

### 6. Plugin commands and host UI — [C-163](../stories/C-163-plugin-commands-and-host-ui.md)

**Amp:** plugins register custom tools *and* custom commands, and can drive UI — notify, confirm,
input, select — from inside a running operation.

**flux today:** a plugin's only expression is an **op** projected to the model.
`PluginCapabilities` (`crates/flux-plugin-protocol/src/lib.rs:515-578`) enumerates process, secrets,
http, http_hosts, private_hosts, conn, blob, discover, credential, and fs scopes — there is no
command or UI verb — and the host-callback surface is `host.read` / `host.write`.

**Why it matters:** a plugin that needs a human decision currently has to fail and explain. And the
seam is already built for this: `Frame` is command-keyed
(`crates/flux-plugin-protocol/src/lib.rs:44-59`) and the protocol crate sits on its own additively-
versioned `1.x` line — which is precisely the affordance the decoupling epic (C-141…C-147) created.

**The security question to settle in the design, not in code:** a plugin that can pop a dialog
inside a trusted surface is a plugin that can phish. Constrain the rendering (plugin name always
shown, no styling control, text never interpreted) rather than trusting plugin behavior. And a UI
confirm must never satisfy the approval gate — a plugin cannot self-approve.

### 7. Agent-set wake-ups — [A-98](../stories/A-98-agent-set-wakeup.md)

**Amp:** the agent can set a schedule and wake itself later with its prompt and context preserved,
optionally notifying via Slack.

**flux today:** `crates/flux-channels/src/adapters/schedule.rs` is a declared cron channel driven by
`flux app run` — **author**-initiated, not agent-initiated.

**Why it matters:** flux has the durable half already (`await`/suspension, journeys, the event log),
so this is a verb over shipped machinery. "The deploy is running, check it in ten minutes" is a
real, common shape that currently forces the agent to block or to drop the thread.

**Why it is sequenced last of the seven:** the timer is trivial; the *authority* is not. An agent
that can schedule itself can spend budget unattended. This wants **C-130** (monetary budgets and
quotas) as a genuine dependency, not a nice-to-have.

### 8. A command palette — folded into [C-153](../stories/C-153-overlay-fuzzy-filter-and-overflow.md)

**Amp:** one fuzzy command palette (Ctrl+O) over 100+ commands.

**flux today:** C-153 is already filed to give the TUI pickers a shared fuzzy ranker (`fuzzy_rank`
exists and serves `@`-path completion; slash matching is a separate prefix/substring path). A
palette is a *superset* of that story — a third caller plus a keybinding once the ranker is shared.
Deliberately not filed separately; C-153 gained a scope note instead.

---

## Deliberate rejects

Recorded so the next mining pass does not re-litigate them.

### Cloud features: reject the mechanism, not the need

The first pass dismissed everything cloud-shaped in one line ("contradicts local-first"). That was
too fast. Amp's hosted features exist because users have needs; the *mechanism* is rejectable
without the *need* being rejectable. Separating them:

| Amp mechanism (rejected) | Underlying need | flux's local-first answer |
|---|---|---|
| Orbs, runners (remote machines) | Run the agent somewhere that isn't my interactive terminal | Partial — `flux app run`, the HTTP server, A2A. Detach/reattach of a *local* turn has no answer; not filed, because the adjacent surfaces muddy the gap and it needs its own grounding pass. |
| Remote control from web/mobile | Approve a pending call when I'm away from the keyboard | **C-127** — approvals over Slack / signed webhook. Already filed; the first pass rejected "remote control" without noticing flux had independently identified the same need. |
| Multiplayer, thread sharing, visibility levels | Show a colleague what the agent did | **C-132** — `flux export <run>` to one self-contained HTML file, no server. |
| Thread finding (keyword / file / repo / author / date) | Find the session where I worked on X | **None** — `flux sessions` takes one flag (`--prune`) and the store's only query is `list(limit)`. Now filed as **C-164**. |
| Managed settings (system-wide enforcement) | A team or auditor pins a baseline a developer can't override | **None** — config is a user→project merge (`crates/flux-config/src/lib.rs:968-973`), both writable by the same user. Now filed as **C-165**. Needs no backend, and serves the regulated-buyer lane the landscape doc names as flux's whitespace. |
| Workspace pooled billing, credits, cost entitlements | Cap what this agent may spend | **C-130** — monetary budgets and quotas. |
| Slack "Puck" personal assistant | Drive the agent from chat | Partial — flux's Slack surface is a channel adapter (`flux app run`), not a hosted bot. |
| Amp-hosted repos (`amp clone`), auto-PR workflows | Ship the change | Out of scope by choice — git worktrees (C-97…C-100) get the agent to a mergeable branch; PR creation is plugin territory. |
| Enterprise SSO, directory sync, zero-retention, IP allowlists, analytics API | Institutional control | Genuinely needs a control plane. Rejected on mechanism *and* need. |

The lesson worth carrying: **"it's cloud" is not a reason, it's a category.** Two real candidates
(C-164, C-165) were buried by treating it as one.

### Rejected outright

- **No-approval-by-default, `amp.permissions` as legacy, `amp.dangerouslyAllowAll`, guarded-file
  allowlists as the primary control.** The exact inverse of flux's default-deny envelope. Not a gap
  — a fork in the road, taken deliberately. Useful head-to-head material for Matrix D in
  [`../archive/research/landscape.md`](../archive/research/landscape.md).
- **No-approval-by-default, `amp.permissions` as legacy, `amp.dangerouslyAllowAll`, guarded-file
  allowlists as the primary control.** This is the exact inverse of flux's default-deny envelope.
  Not a gap — a fork in the road, taken deliberately. Useful head-to-head material for Matrix D in
  [`../archive/research/landscape.md`](../archive/research/landscape.md).
- **Curated multi-model with no BYO key.** Inverse of provider neutrality.
- **Painter (image generation), Librarian (cross-repo GitHub search).** Genuine capabilities, wrong
  layer: plugin territory. The gitlab/github plugins already own the search shape.
- **Speed modes (`low` / `medium` / `high` / `ultra`).** flux ships `--effort`
  (`Low|Medium|High|Xhigh|Max`, `crates/flux-cli/src/args.rs:103-106`). Different spelling, same
  control.
- **Git commit trailers (thread id, co-author).** A deliberate flux position, not a missing feature.

### Deferred with a reason (not rejected, not filed)

- **Config env interpolation (`${VAR_NAME}` in settings).** Verified absent — no `${` handling in
  `crates/flux-config/src/lib.rs`. Looks like a trivial nicety, and isn't: flux gates secret
  resolution behind explicit consent (**C-76**, a shipped security fix), so a config file that
  silently expands environment variables re-opens a surface that was deliberately closed. Worth
  doing, worth designing — not worth filing as a one-liner.
- **Plugin-invoked model calls (Amp's `amp.ai.ask`).** flux plugins have no model access
  (`PluginCapabilities`, `crates/flux-plugin-protocol/src/lib.rs:515-578`). Useful — a plugin could
  classify without carrying its own API key — but it grants a plugin the ability to *spend*, which
  is a different class of authority than every existing capability. Wants **C-130** (budgets)
  underneath it first. Related to but distinct from **C-163**.
- **Detach and reattach a running local turn.** The need is real (orbs/runners answer it for Amp),
  but flux has three adjacent surfaces (`flux app run`, the HTTP server, A2A) and no clean read on
  where the gap actually is. Needs its own grounding pass rather than a speculative story.

- **MCP, and skills bundling `mcp.json`.** Not rejected — already filed as
  [D-193](../stories/D-193-mcp-interop-epic.md). Worth noting that Amp's manual reinforces the
  archived landscape's finding that MCP is now table stakes, and that Amp routes MCP through
  workspace approval and `includeTools` filtering — the same "gate it, don't just mount it"
  instinct D-193 describes.

---

## Value axis

The candidates above were originally ordered by architectural fit — how cleanly each slots into
flux. That is a build-cost ranking wearing a value ranking's clothes. Re-ranked by **how often a
flux user would feel the absence**, weighted by how much it hurts when they do:

| Candidate | Felt how often | Hurts how much | Notes |
|---|---|---|---|
| **A-97** path-scoped guidance | Every turn, every repo | Moderate | The only candidate on the hot path of ordinary use. Highest daily-user value; also the one with a real cache-invalidation trap. |
| **C-164** session search | Weekly | Low, but sharply annoying | Cheapest real win here. Pure query over `events.db`. |
| **C-160** NDJSON protocol | Rarely, by *users* | High, by *integrators* | A platform bet, not a UX fix. Value is unlocking a class of caller flux currently cannot serve at all. |
| **C-161** review checks | Scales with `flux review` adoption | Moderate | Value is contingent on a usage number nobody has measured. |
| **A-96** consult op | Unknown | Potentially high on hard tasks | The most speculative. Amp asserts high value; flux has no evidence either way. A cheap experiment, not a confident bet. |
| **C-165** managed config tier | Never, for solo users | Potentially decisive for one buyer segment | Strategic, not ergonomic. Worth only as much as the regulated-buyer lane is worth. |
| **C-163** plugin commands + UI | Rarely today | Unlocks plugin authors | Value is second-order — it raises the ceiling for other people's work. |
| **C-162** tool blocklist | Occasionally | Low | Trivial cost, trivial benefit. Filed because it is nearly free, not because it matters. |
| **A-98** agent-set wake-up | Rarely | High authority risk | Ranked last deliberately: the timer is easy, the unattended-spend problem is not. Gated on C-130. |

Two honest observations from re-ranking: **C-162 survived on cheapness alone** and would not have
been filed on merit, and **A-96 is a guess** — it is in the list because a competitor vouches for
it, which is the weakest evidence in this document.

## Positioning

Amp's manual states its design principles explicitly: **"unconstrained token usage"**, **"always
uses the best models"**, **"raw model power without unnecessary abstractions"**, **"built to evolve
with new models"** (with no backcompat constraint), and **no approval required by default**.

That is a coherent, well-executed thesis. It is also the precise anti-thesis of flux's:
*the LLM is not the runtime* — typed model stages interpret intent, an authored Flux-Lang loop
freezes effects into a plan, and a deterministic Rust runtime executes it through one mandatory
envelope. Amp maximizes what the model may do; flux constrains what any actor may do and makes the
constraint auditable.

Two consequences worth carrying into flux's own positioning:

1. **The mineable features are almost entirely surface, not substance.** Seven of the eight
   candidates above are ergonomics, interop, and authoring affordances — none of them require
   loosening the envelope, and several (the consult op, the tool blocklist) *fit* precisely because
   flux's constraints make them cheap. A competitor's whole feature surface yielding no pressure on
   the core thesis is itself a useful signal.
2. **The rejects cluster on one axis: hosted.** Threads, orbs, runners, multiplayer, remote control,
   workspaces, enterprise controls — Amp's most distinctive investments all presuppose a backend.
   flux's local-first stance forgoes them wholesale. That is the real strategic choice this read
   surfaced, and it is worth making deliberately rather than by default.
