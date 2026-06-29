# AGENTS.md — contributing to flux

This file is the contributor contract for both human developers and AI agents. **Read it before making any change.** It is the authoritative reference for architecture, conventions, safety invariants, and the dev loop. When in doubt, this file (and the docs it links) is the tie-breaker.

---

## Start here (every session)

1. **Product** — flux is a deterministic agent platform on one thesis (*the LLM is not the runtime*) with three co-equal pillars: the **Agent**, the **Language** (Flux-Lang), and the **Improvement** loop. If you don't already hold this, skim [docs/README.md](docs/README.md) and [docs/vision.md](docs/vision.md).
2. **What to work on** — open the board: **[docs/stories/README.md](docs/stories/README.md)**. Take the top `ready` story by priority, unless the user named a specific story.
3. **The contract** — read that story file (`docs/stories/<id>-*.md`); its **Goal + Acceptance** define what "done" means.
4. **Do the work** — non-trivial design goes in [docs/designs/](docs/designs/); implement; satisfy Acceptance with a **failing-first test**; run the dev loop below until the gate is green.
5. **On done** — set the story's `status: done`, remove its row from the board, add a CHANGELOG entry, and keep design/plan docs in sync.
6. **New or unscoped work?** Create a story first from [docs/stories/_TEMPLATE.md](docs/stories/_TEMPLATE.md) so the next agent inherits the context.

---

## What flux is

A Rust **agent SDK, harness, and coding agent** built as one Cargo workspace of small, strictly-layered crates. The defining idea: **the LLM is not the runtime.** The model is a compiler front-end that emits a typed Flux-Lang plan (a graph); a deterministic runtime executes that plan through one mandatory safety envelope — authorization → approval → guarded IO. Every operation, whether a file read, a shell command, a sub-agent call, or a plugin operation, traverses that envelope. **There are no bypass paths. Don't add one.**

For the *why*, read [docs/vision.md](docs/vision.md). For the full design, [docs/architecture.md](docs/architecture.md). For status and next steps, [docs/roadmap.md](docs/roadmap.md). Active work-in-progress plans live in [`.flux/plans/`](.flux/plans/) (local, gitignored) — read the relevant plan before continuing that work.

**The headline principle that governs review: quality over quantity. flux is deliberately not a sprawling, bug-ridden codebase. Every behavioral change ships with a test, and the gate stays green.**

---

## Dev loop — run this before calling a change done

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
cargo fmt --all                                          # then commit the result
cargo test -p flux-codegate                              # architecture layering lint
```

CI enforces all of these. A change is not finished until every command above is green.

---

## Architecture & the layering rule

Crates are stratified into layers (0 = innermost contracts, 6 = outermost surfaces). **A crate may depend only on its own layer or lower ones.** This is enforced by a test in `flux-codegate` (`crates/flux-codegate/src/lib.rs`) — it is not a convention, it is a gate.

| Layer | Crates | Role |
|---|---|---|
| **L0 contracts** (pure, no IO¹) | `flux-core` `flux-policy` `flux-secret` `flux-spec` `flux-config` `flux-evidence` `flux-skill` `flux-markdown` `flux-lang` | types, authorization, secrets, tool specs, config, evidence, skills, markdown/frontmatter, the Flux-Lang language + reference interpreter |
| **L1 providers** | `flux-provider` `flux-providers` `flux-credentials` | the `Provider` abstraction + the concrete clients (`flux-providers` modules: `messages` core, `anthropic`, `openai`, `openrouter`, `ollama`) + credential store |
| **L2 runtime** | `flux-system` `flux-runtime` `flux-tools` `flux-events` `flux-context` | guarded IO, the safety envelope, built-in tools, the event store, context |
| **L3 agent** | `flux-agent` `flux-orchestrate` `flux-flow` `flux-eval` `flux-cognition` | agent definitions (`AgentSpec`/`Role`) + multi-agent orchestration + the Flux-Lang engine (the one turn loop) + the eval harness + the model-op cognition pack |
| **L4 extensibility** | `flux-hooks` `flux-plugin` | JS hooks + subprocess plugins |
| **L5 capabilities** | `flux-browser` `flux-datasource` `flux-auth` | web, datasource/RAG, caller identity |
| **L6 surfaces** | `flux-sdk` `flux-server` `flux-integrations` `flux-tui` `flux-cli` `flux-app` | SDK, HTTP server, integrations, TUI, the `flux` binary, the multi-agent program runtime host (`flux run app.flux`) |

Key rules:
- **`flux-runtime` (L2) must not depend on `flux-auth` (L5).** Surfaces resolve identity (`LocalIdentity` / `OidcIdentity`) into a `(Caller, Trust)` and inject it via `Executor::with_identity`.
- `flux-evidence`, `flux-skill`, `flux-config`, `flux-markdown`, and `flux-lang` are L0 leaves — no flux deps beyond other L0 — so runtime/agent crates may depend on them without a layering violation. `flux-skill` builds on `flux-markdown` (frontmatter); both stay L0. `flux-markdown`'s render wrappers (over `codewandler/markdown`) are behind off-by-default `ratatui`/`terminal` features, so its default build pulls no UI deps. `flux-flow` (L3, the Flux-Lang engine) builds on `flux-lang` and re-exports it as a facade.
- **If you add a crate, classify it in `flux-codegate`'s `layer()` map** or the lint fails.
- **The turn loop is itself Flux-Lang, and it is the *one* loop everywhere** — `crates/flux-flow::FlowEngine` runs `crates/flux-flow/assets/agent-loop.flux`, driven by the reflexive `plan`/`run_plan` ops and the evidence ops (`observe`/`evidence`/`grade`/`metrics`). `FlowEngine::run_turn_cancellable` is a thin bootstrap, not the loop. Those ops are documented in `crates/flux-flow/docs/ops-reference.md`. The SDK's `flux_sdk::Client` and the sub-agent spawner (`flux-orchestrate`) assemble a `FlowEngine` via `flux_agent::AgentSpec` too — the classic `flux-agent::Agent` loop is gone. `flux-agent` is now the **agent-definition** crate (`AgentSpec` + markdown `Role`); the `AgentSink` streaming trait lives in `flux-flow`. (`flux_sdk::FlowClient` is the declarative flow door.)

> ¹ `flux-lang` is the language **and its reference interpreter**, so it uses `tokio`/async; its L0-purity means "no L1+ flux deps; all effects (op dispatch, value store, observation sink) injected via traits", not "no async". Every other L0 crate is genuinely IO-free.

---

## Non-negotiable conventions

- **All real filesystem / process / network IO goes through `flux-system`** (`System` / `Workspace`). Tools never touch `std::fs` or `std::process::Command` directly. The guarded surface enforces workspace confinement, symlink/escape rejection, and **argv-only** execution — never build a shell string from model input.
- **Every tool runs through `Executor::dispatch`** (`flux-runtime`). Don't call a tool's `execute` directly outside tests; the dispatcher is the policy/approval/redaction gate.
- **Secrets never appear in logs or model-visible output as raw values.** Register them with the `Redactor` (`flux-secret`) and let `dispatch` scrub results. Use `secret:env/KEY` refs, not literals.
- **Errors:** library crates return `flux_core::Result<T>` / `flux_core::Error` (`thiserror`); the `flux` binary uses `anyhow`. Don't `unwrap()` in non-test code on fallible IO.
- **Async** is `tokio`. Long-running agent work must stay cancellable — thread the `tokio_util::sync::CancellationToken` through the agent loop, `Spawner::spawn`, and the orchestration functions.
- **Match the surrounding code** — comment density, naming, module layout. Keep doc comments on public items.

---

## Safety invariants — never regress these

Each invariant below was established (and several re-learned the hard way) during security review. Each is covered by a test. **A regression here is a release blocker, not a nit.**

- **Session shape is always a valid provider history.** Every turn-termination path (normal stop, cancel, compaction, *max-iterations*) must leave the log free of: an empty assistant message, a split tool_use/tool_result pair, or a user-after-user sequence. This bug class has recurred three times (cancel, compaction, iteration cap) — treat any new termination path as suspect. The mock provider does **not** catch it; only a live provider 400 does (see the pre-release gate in [docs/roadmap.md](docs/roadmap.md)).
- **`permission_subjects` must be accurate.** A tool that declares a `Write` effect but reports no subjects is forced to approval — an unscoped write would otherwise match a `*` path grant. Don't return empty subjects to dodge gating.
- **Plugin host capabilities are deny-by-default and manifest-scoped.** A plugin may only run programs / read secret keys / reach the network that its manifest's `capabilities` declares; `SystemHostCaps` checks every callback. Never widen this to "all plugins get everything."
- **All web egress goes through `flux_system::net::guard_url`.** It resolves hostnames to IPs and blocks private/loopback/link-local/unique-local/CGNAT/IPv4-mapped ranges + internal hostnames. Don't hand-roll a second URL guard.
- **The HTTP server is authenticated.** `flux-server` requires a bearer token on every route except `/health`; a non-loopback bind without `FLUX_SERVER_TOKEN` is refused. The daemon auto-approves tools, so an open listener is RCE.
- **Subprocesses don't inherit the agent's environment.** `flux-system` clears it and passes a minimal allow-list; captured output is byte-capped. Untrusted bytes (HTTP bodies, plugin frames) are truncated on char boundaries — never `String::truncate` at a byte offset.

---

## Where to make a change

- **Add a built-in tool:** implement `flux_runtime::Tool` (spec + `permission_subjects` + `intents` + `execute`) in `flux-tools`, do IO via `ctx.system`, register it in `register_builtins`. Declare accurate `effects` so the policy layer gates it correctly. Tools with a `group` field are only surfaced when that group's signal is detected (e.g. `"rust"` tools appear only in Rust workspaces) — add the op to the group's `tools` list in `groups.rs` and to the `builtins_register` test's expected name list. Keep the catalog docs in sync: `crates/flux-flow/docs/ops-reference.md` (and `crates/flux-lang/docs/reference.md` for language nodes).
- **The generic `bash` op is opt-in.** It lives in the off-by-default `shell` group, so it is *not* advertised unless the workspace opts in — config `enable_shell = true`, env `FLUX_ENABLE_BASH=1`, or the `/shell` REPL toggle (each injects the `shell` signal via `detect_signals`). Prefer adding a dedicated, accurately-gated op over widening reliance on `bash`; the dedicated ops (`now`/`cwd`/`sys_info`, `git_*`, the `cargo_*`/`go_*`/`python`/`node`/`make` toolchains, the pure `expr`/`jq`/`fmt` + cognition list ops) exist to keep `bash` unnecessary.
- **Add a provider:** a provider = `WireCodec` × `Credential` composed by `NativeProvider` (`flux-provider`). Add the codec/credential in the relevant `flux-providers` module (`anthropic`/`openai`/`openrouter`/`ollama`, or a new one) — Messages-protocol providers reuse `crate::messages`; wire model routing in `flux-cli`'s `build_provider`.
- **Define an agent:** an agent = a `flux_agent::AgentSpec` (model, persona/system prompt, skills, tool selection, permissions, settings) assembled onto a `FlowEngine` (`AgentSpec::assemble`/`into_engine`). The markdown **`Role`** format (`flux_agent::Role`, parsed from `.flux/agents/<role>.md`) is the file-defined form — `Role::to_spec` turns it into an `AgentSpec`.
- **Add a sub-agent role:** drop a markdown file in `.flux/agents/<role>.md` (frontmatter `description`/`model`/`tools`, body = system prompt), or add to the CLI defaults.
- **Add a skill:** drop a `.md` (or a dir with `SKILL.md`) in `.flux/skills` (project) or a user-global dir (`~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`; project wins on a name clash). Both the flux-native (`triggers:` frontmatter) and Agent-Skills/Claude (`name` + `description`, no triggers) formats are read by `flux-skill` (which parses frontmatter via `flux-markdown`); trigger-less skills activate on `name`/`description` keywords. `flux_skill::active_for` ranks + caps activation; the engine injects the matched skills into the turn's system prompt in `flux-flow` (`base_system_with_skills`), and an agent's skill set comes from `flux_agent::AgentSpec.skills`.
- **Write a plugin:** any executable speaking the framed NDJSON protocol in `flux-plugin`; the Rust SDK (`serve` + `PluginHandler` + `GuestHost`) is the reference. Operations are projected as policy-gated tools; privileged IO is requested back from the host via declared capability callbacks. The host grants nothing you don't ask for.

---

## Testing

- **Offline-first.** The built-in `mock` provider (`flux run -m mock`) drives the full agent loop without network. CLI test hooks via env vars (`FLUX_MOCK_TOOL`, `FLUX_MOCK_TOOL_INPUT`, `FLUX_MOCK_BASH`, `FLUX_MOCK_HANG`) exercise specific tools and cancellation end-to-end.
- **Pure crates** (`flux-policy`, `flux-spec`, `flux-secret`, …) get exhaustive unit tests.
- **The safety envelope** has no-bypass tests (default-deny, destructive escalation under permissive rules, secret redaction, hook-deny short-circuit) — keep them passing and add to them when you touch the dispatcher.
- **A new behavior ships with a test that fails before the change.**

---

## Commits

- **Never commit without an explicit instruction to do so.**
- **Stay on the current branch.** Don't create feature branches or git worktrees as a matter of course — do the work in place on the checked-out branch. Only create a branch or worktree when the user explicitly asks for one.
- Use **semantic commit** titles: `type(scope): short imperative description` where type is one of `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `style`. Scope is the primary crate or surface (e.g. `cli`, `tools`, `runtime`, `agent`, `flow`). Example: `feat(cli): expose /compact slash command in the REPL`.
- A blank line after the title; then a bulleted body explaining **what** changed and **why** — title-only commits are not acceptable.
- Ticket references go in a trailing `Refs:` line, not the title.
- Don't discard uncommitted changes or run destructive `git` operations on files you didn't change.

---

## Keeping the Flux-Lang language and its docs in sync

The Flux-Lang **language + reference interpreter** lives in **`flux-lang`** (L0: `crates/flux-lang/src/` — `ast.rs`, `render.rs`, `analyze.rs` (`lower` → typed HIR), `opspec.rs`, `schema.rs`, `runtime.rs` behind injected `host`/`store`/`sink` traits, plus `prelude.rs` (artifact types), `program.rs` (multi-agent Program), `parse.rs`/`format.rs` (text syntax), `optimize.rs` (optimizer/`PhysicalPlan`), the `fluxlang` CLI and `skill.rs`). `flux-flow` is the L3 **engine** (compile/engine/state + the `Executor`/`FlowStore`/`AgentSink` adapters and the thin `execute_flow`/`plan_risk` wrappers) and re-exports `flux-lang` as a facade, so `flux_flow::{ast, render, analyze, host, store, runtime, …}` still resolve. The language's own docs live in `crates/flux-lang/docs/` (`reference.md`, `syntax.md`, `PRD.md`, `STATUS.md`, `evolution-impl-plan.md`, `design-review.md`) + `crates/flux-lang/{README,AGENTS}.md` + the forward design `docs/designs/flux-lang-evolution.md`; the engine's ops live in `crates/flux-flow/docs/ops-reference.md`. See [`crates/flux-lang/AGENTS.md`](crates/flux-lang/AGENTS.md) for the full flux-lang design/plan docs map.

**The node-kind tables are a single source of truth and are auto-generated — do not hand-edit them.** The `Node` enum's doc-comments in `crates/flux-lang/src/ast.rs` flow through `flux_lang::schema::node_kind_catalog()` into (a) the `emit_plan` planner prompt, (b) the "Node kinds at a glance" table in `crates/flux-lang/docs/reference.md`, (c) the `## Node kinds` table in the **flux-lang** language skill (`crates/flux-lang/skill/SKILL.md`), and (d) the same table in the **flux-flow** engine skill (`.flux/skills/flux-flow/SKILL.md`). The **artifact-type prelude** has a parallel SSOT: the `flux_lang::prelude` struct doc-comments flow through `prelude_type_catalog()` into the `<!-- BEGIN/END generated:prelude-types -->` block in `reference.md` and the skill — regenerated by the same `skill_in_sync` test, so add a `Named` prelude type then `UPDATE=1 cargo test -p flux-lang --test skill_in_sync`. The generated blocks are fenced by `<!-- BEGIN/END generated:node-kinds -->`; two tests fail on drift: `cargo test -p flux-lang --test skill_in_sync` (the language skill + reference) and `cargo test -p flux-flow --test skill_docs_in_sync` (the engine skill). After adding/renaming a node kind or editing a variant doc-comment, regenerate with `UPDATE=1` on both: `UPDATE=1 cargo test -p flux-lang --test skill_in_sync` and `UPDATE=1 cargo test -p flux-flow --test skill_docs_in_sync`.

What still needs manual updates in the same commit:

- **New node kind** → write its doc-comment on the `Node` variant (the summary tables regenerate), and add a detailed hand-written section under the appropriate group in `crates/flux-lang/docs/reference.md` (primitive, control-flow, …) plus an example in the skill if helpful.
- **Changed semantics** → update the relevant prose section and the Key invariants list in `crates/flux-lang/docs/reference.md`.
- **New built-in tool in `flux-tools`** → the op catalog the model sees is built dynamically from the live `ToolRegistry`, so the prompt needs nothing; but update the hand-written tables in `crates/flux-flow/docs/ops-reference.md` and the engine skill's "Registered ops" table.

---

## Don't

- Don't bypass the safety envelope or the guarded IO boundary.
- Don't introduce an inner→outer crate dependency (the layering lint will fail).
- Don't log or surface secret values; don't build shell command strings from model input.
- Don't leave `clippy -D warnings` or `fmt` dirty.
- Don't create new branches or git worktrees unless the user explicitly asks — work on the current branch.
