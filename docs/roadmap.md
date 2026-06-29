# flux — roadmap & status

Status as of **0.2.4 (2026-06-25)**: public + installable at
[codewandler/flux](https://github.com/codewandler/flux); 31 crates, **450+ tests**, a permanently green
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
before every release.

## Next

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
  - **The flux-lang agent loop is now observable.** The self-hosted loop (`agent-loop.flux`) shipped
    transparent (zero surface change); these make it visible: `flux run --show-loop` reveals the
    `plan → run_plan → observe` machinery live, the REPL `/evidence` prints the audit trail, and
    `flux loop show`/`eject` reads or scaffolds the loop (`.flux/agent-loop.flux` override). See
    [agent-loop.md](agent-loop.md).
- **SDK + crates.io** (tier 2) — **P7 landed the bulk:** a **Rust eDSL** (`flux_lang::dsl`, re-exported
  as `flux_sdk::dsl`) whose builder primitives compile to the Flux-Lang AST — loops
  (`each`/`repeat`/`loop_for`/`race`) and control-flow (`match`/`route`/`fallback`/`timeout`/`budget`)
  first-class, all 36 node kinds covered (drift-guarded by `dsl_covers_every_node_kind`), authored in
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
  42 node kinds. Remaining (optional): native `{k:expr}`/`[expr]` text spelling + a strict-JSON-schema
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

- **Two turn loops.** The CLI/TUI/server run the pure-DAG `FlowEngine`, but the SDK's
  `flux_sdk::Client` still drives the classic `flux-agent::Agent` loop. Unify onto
  `FlowEngine`/`FlowClient` and retire `flux-agent::Agent` (ref
  [designs/flux-flow.md](designs/flux-flow.md) §11). → [A-01](stories/A-01-unify-flowengine.md).
- ~~**Crate consolidation phases 2–4**~~ ✅ done (35 → 31). → [C-01](stories/C-01-crate-consolidation.md).
- **crates.io publish** blocked on the `flux-core` name (needs a vanity prefix); deferred.
- **Self-improvement headline gain** still lacks a trials ≥ 3, grader-confirmed result.
  → [I-01](stories/I-01-headline-gain.md).

## Backlog (product improvements)

- **Load skills from a user/global dir** (e.g. `~/.flux/skills`) in addition to the project
  `.flux/skills`, so global skills needn't be copied per-project. → [L-01](stories/L-01-global-skills.md).

## Direction

The through-line is **the LLM is not the runtime**: the model is a compiler front-end that emits a
Flux-Lang plan, and the deterministic engine runs it — **non-bypassable safety** is the hard
invariant that buys. Priority is **personal coding agent → reusable SDK → multi-user platform**. See
[vision.md](vision.md). The annotated original design & planning document (with full
milestone-by-milestone detail) is retained outside the repo by the author; this roadmap is the
in-repo canonical summary.
