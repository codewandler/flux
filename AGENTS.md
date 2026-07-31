# AGENTS.md — operating contract for agents in flux

For **coding agents and automation**. Human product entry point: [README.md](README.md).
**Read this before making any change.** When in doubt, this file and the docs it links are the
tie-breaker.

This file holds only what you must know *before* acting. Anything a test, script, or CI job already
catches is a pointer, not an essay.

---

## Agent mandate

- **Serve the newest user request first.** If the user named a task, story, file, or command, that scopes the work. If not, open the board and take the top `ready` story by priority.
- **Protect the user's worktree.** Start with `git status --short --branch`; assume uncommitted changes are user-owned unless you made them. Never reset, discard, rebase, rewrite history, or force-push unless explicitly asked.
- **Keep the architecture honest.** The LLM is not the runtime; all real effects flow through authorization → approval → guarded IO. **There are no bypass paths. Don't add one.**
- **Make changes auditable.** Non-trivial behavior needs a story or design trail, a failing-first test, and a CHANGELOG entry.
- **Finish the loop.** Implement, verify with the gate, report any command you could not run, and **only commit when explicitly instructed**.

---

## Start here (every session)

1. **Orient** — read the request, run `git status --short --branch`. Resuming? Read the relevant plan in [`.flux/plans/`](.flux/plans/).
2. **What to work on** — the user's named work, else the board: **[docs/stories/README.md](docs/stories/README.md)**, top `ready` story by priority.
3. **The contract** — for story work read `docs/stories/<id>-*.md`; its **Goal + Acceptance** define "done".
4. **Do the work** — non-trivial design goes in [docs/designs/](docs/designs/); satisfy Acceptance with a **failing-first test**; run the gate until green.
5. **On done** — set `status: done`, remove the board row, add a CHANGELOG entry (+ `WHATS-NEW.md` if user-visible), keep design/plan docs in sync.
6. **New or unscoped work?** Create a story from [docs/stories/_TEMPLATE.md](docs/stories/_TEMPLATE.md) first.

---

## What flux is

A Rust **agent SDK, harness, and coding agent** — one Cargo workspace of small, strictly-layered
crates. **The LLM is not the runtime:** typed model stages detect intent, gather evidence, and
propose literal calls; authored Flux-Lang owns control flow; a deterministic runtime freezes effects
into action batches and executes them through one safety envelope — authorization → approval →
guarded IO. The model never authors executable Flux. Every operation traverses that envelope.

Why: [docs/vision.md](docs/vision.md) · design: [docs/architecture.md](docs/architecture.md) ·
status: [docs/roadmap.md](docs/roadmap.md).

**The principle that governs review: quality over quantity.** Every behavioral change ships with a
test, and the gate stays green.

---

## Dev loop — run before calling a change done

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
cargo fmt --all                                          # then commit the result
cargo test -p flux-codegate                              # architecture + test-posture lints
```

CI enforces all of these. Docs-only changes may use a narrower check — say explicitly in the final
report what was and was not run. `cargo fmt --check` must also be clean in the **nested `plugins/`
workspace** if you touched it.

**Your machine is not a CI runner in one specific way: it has `bwrap`.** C-262 fails auto-approved
and serving surfaces closed without an OS sandbox backend, so a test that spawns one and never
declares its posture passes here and reds CI (three times over, during the 0.38.0 cut). If you added
or changed such a spawn, run the posture CI runs in:

```bash
FLUX_BWRAP_BIN=/nonexistent/bwrap cargo test --workspace   # the no-backend side, as CI sees it
FLUX_TEST_SANDBOX_BACKEND=1 cargo test -p flux-cli --test sandbox_backend  # the with-backend side
```

`cargo test -p flux-codegate` catches the common case statically; `docs/stories/C-266-*.md` records
exactly what that lint does and does not cover.

---

## Architecture — the layering rule

Crates are stratified L0 (innermost contracts) → L6 (outermost surfaces): **L0** contracts ·
**L1** providers · **L2** runtime · **L3** agent · **L4** extensibility · **L5** capabilities ·
**L6** surfaces. **A crate may depend only on its own layer or lower.** This is a gate, not a
convention — enforced by `cargo test -p flux-codegate`; the authoritative map is that crate's
`layer()` function. Full topology: [docs/architecture.md](docs/architecture.md).

- **If you add a crate, classify it in `flux-codegate`'s `layer()` map** or the lint fails.
- **`flux-runtime` (L2) must not depend on `flux-auth` (L5).** Surfaces resolve identity into a `(Caller, Trust)`, pair it with policy in an atomic `ExecutionAuthorization`, and pass that through `ExecutionEnvironment`.
- **The conversational text-agent loop is itself Flux-Lang, and it is the one loop on every text surface** — `flux-flow::FlowEngine` runs `crates/flux-flow/assets/agent-loop.flux`. `run_turn_cancellable` is a thin bootstrap, not the loop. The SDK and sub-agent spawner assemble the same engine via `flux_agent::AgentSpec`.

---

## Non-negotiable conventions

- **All real filesystem / process / network IO goes through `flux-system`** (`System` / `Workspace`). Tools never touch `std::fs` or `std::process::Command` directly. **argv-only** execution — never build a shell string from model input.
- **Every tool runs through `Executor::dispatch`** (`flux-runtime`). Don't call a tool's `execute` directly outside tests; the dispatcher is the policy/approval/redaction gate.
- **Secrets never appear in logs or model-visible output as raw values.** Register them with the `Redactor` (`flux-secret`). Use `secret:env/KEY` refs, not literals.
- **Errors:** library crates return `flux_core::Result<T>`/`Error` (`thiserror`); the `flux` binary uses `anyhow`. No `unwrap()` in non-test code on fallible IO. *Wire-seam exception:* where the error crosses a protocol boundary as the payload itself (plugin frame `err`, A2A JSON-RPC error, host-capability callback), `Result<_, String>` is correct.
- **Async is `tokio`, and long-running agent work stays cancellable** — thread `CancellationToken` through the agent loop, `Spawner::spawn`, and orchestration.
- **Match the surrounding code** — comment density, naming, module layout. Doc comments on public items.

---

## Safety invariants — never regress these

Each was established during security review and is covered by a test. **A regression here is a
release blocker, not a nit.** Read these before adding a turn-termination path, a process launch, a
network call, or a plugin capability.

- **Session shape is always a valid provider history.** Every turn-termination path (normal stop, cancel, compaction, *max-iterations*) must leave the log free of: an empty assistant message, a split tool_use/tool_result pair, or a user-after-user sequence. This class has recurred three times — treat any new termination path as suspect. **The mock provider does not catch it**; only a live provider 400 does.
- **Caller identity is immutable for a live turn.** Multi-principal surfaces pass a request-owned `TurnIdentity` through `run_turn_as`/`run_turn_cancellable_as`. Never reintroduce a mutable executor identity cell or an outer-surface swap.
- **`permission_subjects` must be accurate.** A tool declaring a `Write` effect but reporting no subjects is forced to approval — an unscoped write would otherwise match a `*` path grant. Don't return empty subjects to dodge gating.
- **Plugin host capabilities are deny-by-default and manifest-scoped.** A plugin may only run programs / read secret keys / reach HTTP hosts / dial targets its manifest declares. Private/loopback egress additionally needs an operator config grant. Never widen to "all plugins get everything".
- **All web egress goes through `flux_system::net::guard_url_scoped`/`guard_url`** — resolves hostnames to IPs and blocks private/loopback/link-local/ULA/CGNAT/IPv4-mapped ranges unless the caller holds a scoped private-net grant. Don't hand-roll a second URL guard.
- **The HTTP server is authenticated.** `flux-server` requires a bearer token on every route except `/health` and the A2A discovery card; a non-loopback bind without `FLUX_SERVER_TOKEN` is refused. The daemon auto-approves tools — an open listener is RCE.
- **One guarded path starts every OS process.** All process creation — including launching a plugin binary — goes through `flux_system::System` (one `build_command`): argv-only, workspace-pinned cwd, env **cleared** to a minimal non-secret allow-list, output byte-capped. Don't add a second `Command::new`. Because the plugin process is env-cleared, a plugin cannot read host secrets via `std::env`. Truncate untrusted bytes on **char** boundaries, never `String::truncate` at a byte offset.
- **Provider bytes never error a chunk stream.** Codecs skip + count an unparseable SSE/frame envelope (surfacing `Chunk::StreamDiagnostic` at stream end) instead of `?`-propagating; only *declared* provider failures stay fatal. Enforced structurally: `crates/flux-providers/clippy.toml` bans bare `serde_json::from_*`. See [docs/designs/stream-resilience.md](docs/designs/stream-resilience.md).

---

## Where to make a change

- **Add a built-in tool:** implement `flux_runtime::Tool` (spec + `permission_subjects` + `intents` + `execute`) in `flux-tools`, IO via `ctx.system`, register in `register_builtins`. Declare accurate `effects`. Tools with a `group` are surfaced only when that group's signal is detected — add the op to `groups.rs` and to the `builtins_register` test's expected names. Mirror the catalog in **both** `crates/flux-flow/docs/ops-reference.md` **and** `website/docs/language/ops.md` — a registered *public* op missing from either reds the gate, but via a **different** test per file: the website file by `operations_reference_covers_the_registered_public_catalog` (`crates/flux-cli/tests/website_contract.rs:330`), the in-repo file by `the_in_repo_reference_covers_the_whole_production_catalog` (`crates/flux-cli/src/catalog_coherence.rs`, C-248 — it wants a table **row**, not a prose mention, and any row in a table with a Risk column is additionally held to the declared tier). Verify the finished `ToolSpec` with `flux_spec::metadata_violations` rather than by eye; since C-210 that check reads `semantic_effects` too, so declare those honestly.
- **The generic `bash` op is opt-in** (off-by-default `shell` group; `enable_shell = true`, `FLUX_ENABLE_BASH=1`, or `/shell`). Prefer a dedicated, accurately-gated op over widening reliance on `bash`.
- **Add a provider:** a provider = `WireCodec` × `Credential` composed by `NativeProvider`. Add the codec/credential in the relevant `flux-providers` module — Messages-protocol providers reuse `crate::messages` — and wire routing in `flux-cli`'s `build_provider`.
- **Define an agent:** an `AgentSpec` (model, persona, skills, tools, permissions) assembled onto a `FlowEngine`. The markdown `Role` (`.flux/agents/<role>.md`) is the file-defined form.
- **Add a sub-agent role:** markdown in `.flux/agents/<role>.md`. `model:` takes the same spec form as `-m`, but the provider prefix **must be the parent's own** — sub-agents inherit the parent's provider and a foreign prefix fails fast at spawn.
- **Add a skill:** `.md` (or dir with `SKILL.md`) in `.flux/skills` or a user-global dir (`~/.flux/skills`, `~/.agents/skills`, `~/.claude/skills`; project wins). Both flux-native (`triggers:`) and Claude/Agent-Skills formats are read. **Discovery does not activate a skill** — the CLI requires `--skill <name>`; embedded agents populate `AgentSpec.skills` explicitly.
- **Write a plugin:** **read [`plugins/AUTHORING.md`](plugins/AUTHORING.md) first.** Any executable speaking the framed NDJSON protocol in `flux-plugin`. Operations project as policy-gated tools; privileged IO is requested back via declared capability callbacks. Plugin binaries are trusted dependencies, not OS-sandboxed code.
- **Rebuild/install the plugin pack:** `task plugins:install` (or `plugins:build`). `plugins/` is a **nested workspace excluded from the root** — build it with `--manifest-path plugins/Cargo.toml`.
- **Binaries:** the repo ships **one** product binary, `flux`. Everything else is dev tooling (`fluxlang`), protocol-mandated (`flux-lsp`, plugin binaries), or a test fixture. Don't add a binary without that kind of justification.

---

## Testing

- **Offline-first.** The `mock` provider (`flux run -m mock`) drives the full loop without network. Env hooks: `FLUX_MOCK_TOOL`, `FLUX_MOCK_TOOL_INPUT`, `FLUX_MOCK_BASH`, `FLUX_MOCK_HANG`.
- **Pure crates** get exhaustive unit tests. **The safety envelope** has no-bypass tests — keep them passing and add to them when you touch the dispatcher.
- **A new behavior ships with a test that fails before the change.**

---

## Commits

- **Never commit without an explicit instruction to do so.**
- **Stay on the current branch.** Don't create feature branches or git worktrees as a matter of course — work in place. Only branch when the user asks.
- **Semantic titles:** `type(scope): short imperative description` — `feat` `fix` `refactor` `perf` `test` `docs` `chore` `style`; scope is the primary crate/surface. Breaking: `type(scope)!:`.
- Blank line, then a **bulleted body explaining what and why** — title-only commits are not acceptable. Ticket refs go in a trailing `Refs:` line, not the title.
- Don't discard uncommitted changes or run destructive `git` operations on files you didn't change.
- Ignored build output (`target/`, `plugins/target/`, `website/node_modules/`) is disposable — never add it to Git.

---

## Releases

`scripts/cut-release.sh` does the mechanics and is **transactional** (a red gate restores the tree,
so a failed cut is safe to re-run). Full runbook: `crates/flux-sdk/PUBLISHING.md`. What you must
decide *before* running it:

- **The bump.** Cargo pre-1.0 SemVer: for `0.y.z` the **minor** position is the breaking signal. Scan `[Unreleased]` and the commits since the last tag for `!` titles / "BREAKING": **any breaking change → minor**; additive/fixes only → patch. Never use patch as a rolling counter.
- **Two changelogs, two audiences.** `CHANGELOG.md` is the engineering log (story IDs, crates). **`WHATS-NEW.md` is the CUSTOMER changelog** — every user-visible change adds a plain-language entry (no story IDs, no crate names). Internal-only changes skip it. An empty customer section is legal only for internal-only releases. *A bare `WHATS-NEW.md` edit reds the gate* until you regenerate the tracked website mirror in the same commit.
- **The plugin PROTOCOL LINE is the one exception to single-version** (C-143): the crates a plugin compiles against carry an independent `1.x`, so `cut-release.sh` **never touches anything under `plugins/`**, and a pack release is cut separately by hand. A wire change implies a pack release is owed. CI enforces this (`check-crate-versions.sh`, `check-plugin-compat.sh`, `check-host-kit-protocol-drift.sh`). Design: `docs/designs/plugin-protocol-decoupling.md`.

---

## Flux-Lang & its docs

`flux-lang` (L0) is the **language + reference interpreter**; `flux-flow` (L3) is the **engine** and
re-exports it as a facade. Docs map: [`crates/flux-lang/AGENTS.md`](crates/flux-lang/AGENTS.md).

- **Node-kind and prelude tables are auto-generated — never hand-edit.** They flow from `Node`/`prelude` doc-comments through `schema::node_kind_catalog()`/`prelude_type_catalog()` into the reference, the flux-lang skill, and the website. Regenerate: `UPDATE=1 cargo test -p codewandler-flux-lang --test skill_in_sync` and `--test website_in_sync`.
- **Still manual in the same commit:** a **new node kind** needs a hand-written section in `crates/flux-lang/docs/reference.md`; **changed semantics** need the prose + Key invariants updated.
- ⚠ **Editor-tooling mirrors are manual, and only TWO of the four are guarded** — a new keyword or spelling change must be propagated by hand to the website Prism grammar (`website/src/theme/prism-include-languages.js`), [`codewandler/flux-tree-sitter`](https://github.com/codewandler/flux-tree-sitter) (Helix/Neovim/Zed; also `.helix/languages.toml`), and the TextMate/IntelliJ grammars in [`codewandler/flux-editors`](https://github.com/codewandler/flux-editors). Since C-300, `crates/flux-lang/tests/named_option_headers.rs` fails when a **canonical header-option label** (`risk`, `max`, `wait`, …) is missing from the *Prism* grammar — narrow, and Prism is the one mirror that can only mis-colour. Since C-334, `scripts/check-tree-sitter-corpus.sh` (nightly, `tree-sitter-corpus.yml`) fetches the rev **pinned in `.helix/languages.toml`** and fails on any `ERROR`/`MISSING` node in `examples/*.flux` — so both "nobody mirrored the change" and "the pin does not reflect the mirror" are now caught for the tree-sitter grammar. **Grep the TextMate and IntelliJ grammars after syntax work** — nothing else will tell you.

---

## Don't

- Don't bypass the safety envelope or the guarded IO boundary.
- Don't introduce an inner→outer crate dependency (the layering lint will fail).
- Don't log or surface secret values; don't build shell command strings from model input.
- Don't leave `clippy -D warnings` or `fmt` dirty.
- Don't create new branches or git worktrees unless the user explicitly asks.
- Don't commit without an explicit instruction.
