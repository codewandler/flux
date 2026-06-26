# AGENTS.md — working on flux

Guidance for agents (and humans) contributing to this repository. Read this before making changes.

## What flux is

A Rust agent SDK / harness / coding agent built as a Cargo **workspace of small, strictly-layered
crates**. The defining idea: **the LLM is not the runtime** — the model is a compiler front-end that
emits a typed Flux-Lang plan, and a deterministic runtime executes it. The core invariant that buys:
every operation — built-in, plugin, or sub-agent — goes through one mandatory safety envelope
(authorization → approval → guarded IO). Don't add code paths that bypass it.

For the *why* and the direction, read [docs/vision.md](docs/vision.md); for the full design,
[docs/architecture.md](docs/architecture.md); for status and what's next, [docs/roadmap.md](docs/roadmap.md).
Active execution plans live in [`.flux/plans/`](.flux/plans/) (local, gitignored) — e.g.
[`markdown-rendering-and-m2-compliance.md`](.flux/plans/markdown-rendering-and-m2-compliance.md)
(CLI markdown rendering, shipped in 0.2.4, + the markdown-library compliance push) and the separate
`flux-flow-implementation.md` effort; read the relevant plan before continuing that work.
The headline principle that governs review: **the LLM is not the runtime — with non-bypassable safety
as the invariant that buys — and quality over quantity: flux is deliberately not a sprawling,
bug-ridden codebase. Every behavioral change ships with a test, and the gate stays green.**

## The dev loop (run before you call a change done)

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings   # must be clean
cargo fmt --all                                          # then commit the formatting
cargo test -p flux-codegate                              # architecture layering lint
```

CI enforces all of these. A change isn't finished until they're green.

## Architecture & the layering rule (important)

Crates are stratified into layers (0 = innermost contracts, 6 = outermost surfaces). **A crate may
depend only on its own layer or lower ones.** This is enforced by a test in `flux-codegate`
(`crates/flux-codegate/src/lib.rs`), the project's `codegate` analog.

| Layer | Crates | Role |
|---|---|---|
| **L0 contracts** (pure, no IO) | `flux-core` `flux-policy` `flux-secret` `flux-spec` `flux-config` `flux-evidence` `flux-skill` | types, authorization, secrets, tool specs, config, evidence, skills |
| **L1 providers** | `flux-provider` `flux-credentials` `flux-anthropic` `flux-openai` | the `Provider` abstraction + clients + credential store |
| **L2 runtime** | `flux-system` `flux-runtime` `flux-tools` `flux-session` `flux-context` | guarded IO, the safety envelope, built-in tools, sessions, context |
| **L3 agent** | `flux-agent` `flux-orchestrate` | the agent loop + multi-agent orchestration |
| **L4 extensibility** | `flux-hooks` `flux-plugin` | JS hooks + subprocess plugins |
| **L5 capabilities** | `flux-browser` `flux-datasource` `flux-auth` | web, datasource/RAG, caller identity |
| **L6 surfaces** | `flux-sdk` `flux-server` `flux-integrations` `flux-tui` `flux-cli` | SDK, HTTP server, integrations, TUI, the `flux` binary |

Rules that fall out of this:
- **`flux-runtime` (L2) must not depend on `flux-auth` (L5).** Surfaces resolve identity (`LocalIdentity`
  / `OidcIdentity`) into a `(Caller, Trust)` and pass it into the `Executor` via `with_identity`.
- `flux-evidence`, `flux-skill`, and `flux-config` are pure L0 leaves on purpose (no IO, no flux deps
  beyond other L0), so runtime/agent crates may depend on them.
- **If you add a crate, classify it in `flux-codegate`'s `layer()` map** or the lint fails.

## Non-negotiable conventions

- **All real filesystem / process / network IO goes through `flux-system`** (`System` / `Workspace`).
  Tools never touch `std::fs` or `std::process::Command` directly. The guarded surface enforces
  workspace confinement, symlink/escape rejection, and **argv-only** execution (no shell — never build a
  shell string from model input).
- **Every tool runs through `Executor::dispatch`** (`flux-runtime`). Don't call a tool's `execute`
  directly outside tests; the dispatcher is the policy/approval/redaction gate.
- **Secrets never hit logs or model-visible output as raw values.** Register them with the `Redactor`
  (`flux-secret`) and let `dispatch` scrub results. Use `secret:env/KEY` refs, not literals.
- **Errors:** library crates return `flux_core::Result<T>` / `flux_core::Error` (`thiserror`); the `flux`
  binary uses `anyhow`. Don't `unwrap()` in non-test code on fallible IO.
- **Async** is `tokio`. Long-running agent work must stay cancellable: thread the
  `tokio_util::sync::CancellationToken` (the agent loop, `Spawner::spawn`, and the orchestration
  functions all take one).
- **Match the surrounding code** — comment density, naming, module layout. Keep doc comments on public
  items.

## Safety invariants — never regress these

These were established (and several re-learned the hard way) during security review. Each is covered
by a test; if you touch the relevant code, keep the test passing and add to it. **Non-bypassable
safety is the hard invariant the architecture buys** ([docs/vision.md](docs/vision.md)) — a
regression here is a release blocker, not a nit.

- **Session shape is always a valid provider history.** Every turn-termination path (normal stop,
  cancel, compaction, *max-iterations*) must leave the log free of: an empty assistant message, a
  split tool_use/tool_result pair, or a user-after-user sequence. Treat any new termination path as
  suspect — this bug class has recurred three times (cancel, compaction, iteration cap), and the mock
  provider does **not** catch it (only a live provider 400 does — see the pre-release gate in
  [docs/roadmap.md](docs/roadmap.md)).
- **`permission_subjects` must be accurate.** A tool that declares a `Write` effect but reports no
  subjects is forced to approval (an unscoped write would otherwise match a `*` path grant). Don't
  return empty subjects to dodge gating.
- **Plugin host capabilities are deny-by-default and manifest-scoped.** A plugin may only run programs
  / read secret keys / reach the network that its manifest's `capabilities` declares; `SystemHostCaps`
  checks every callback. Never widen this to "all plugins get everything."
- **All web egress goes through `flux_system::net::guard_url`.** It resolves hostnames to IPs and
  blocks private/loopback/link-local/unique-local/CGNAT/IPv4-mapped ranges + internal hostnames. Don't
  hand-roll a second URL guard; reuse this one.
- **The HTTP server is authenticated.** `flux-server` requires a bearer token on every route except
  `/health`; a non-loopback bind without `FLUX_SERVER_TOKEN` is refused. The daemon auto-approves
  tools, so an open listener is RCE.
- **Subprocesses don't inherit the agent's environment** (`flux-system` clears it and passes a minimal
  allow-list), and captured output is byte-capped. Untrusted bytes (HTTP bodies, plugin frames) are
  truncated on char boundaries and parsed with size bounds — never `String::truncate` at a byte offset.

## How things fit together (where to make a change)

- **Add a built-in tool:** implement `flux_runtime::Tool` (spec + `permission_subjects` + `intents` +
  `execute`) in `flux-tools`, do IO via `ctx.system`, and register it in `register_builtins`. Declare
  accurate `effects` so the policy layer gates it (`Effect::Write` → `workspace.write`, etc.).
- **Add a provider:** a provider = `WireCodec` × `Credential` composed by `NativeProvider`
  (`flux-provider`). Add the codec/credential in `flux-anthropic` or `flux-openai`; wire model routing in
  `flux-cli`'s `build_provider`.
- **Add a sub-agent role:** drop a markdown file in `.flux/agents/<role>.md` (frontmatter
  `description`/`model`/`tools`, body = system prompt), or add to the CLI defaults.
- **Add a skill:** `.flux/skills/*.md` with `triggers:` frontmatter; activation + injection is handled
  by the agent loop.
- **Write a plugin:** any executable speaking the framed NDJSON protocol in `flux-plugin`; the Rust SDK
  (`serve` + `PluginHandler` + `GuestHost`) is the reference. Operations are projected as policy-gated
  tools; privileged IO is requested back from the host via capability callbacks. Declare the
  operation's `effects`/`risk` and the `capabilities` (allow-listed `process` programs, `secret` keys,
  `http`) in the manifest — the host grants nothing you don't ask for.

## Testing

- **Offline-first.** A built-in `mock` provider (`flux --... -m mock`) drives the full agent loop without
  network. The CLI exposes test hooks via env vars (`FLUX_MOCK_TOOL`, `FLUX_MOCK_TOOL_INPUT`,
  `FLUX_MOCK_BASH`, `FLUX_MOCK_HANG`) to exercise specific tools / cancellation end-to-end.
- **Pure crates** (`flux-policy`, `flux-spec`, `flux-secret`, …) get exhaustive unit tests.
- **The safety envelope** has no-bypass tests (default-deny, destructive escalation under permissive
  rules, secret redaction, hook-deny short-circuit) — keep them passing and add to them when you touch
  the dispatcher.
- A new behavior ships with a test that fails before the change.

## Commits

- **Never commit without an explicit instruction to do so.**
- Use **semantic commit** titles: `type(scope): short imperative description` where type is one
  of `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`, `style`. Scope is the primary
  crate or surface affected (e.g. `cli`, `tools`, `runtime`, `agent`, `flow`). Example:
  `feat(cli): expose /compact slash command in the REPL`.
- A blank line after the title; then a bulleted body explaining **what** changed and **why**
  (title-only commits are not acceptable). Keep commits atomic.
- Ticket references go in a trailing `Refs:` line, not the title.
- Don't discard uncommitted changes or run destructive `git` operations on files you didn't change.

## Don't

- Don't bypass the safety envelope or the guarded IO boundary.
- Don't introduce an inner→outer crate dependency (the layering lint will fail).
- Don't log or surface secret values; don't build shell command strings from model input.
- Don't leave `clippy -D warnings` or `fmt` dirty.
