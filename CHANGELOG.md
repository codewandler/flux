# Changelog

All notable changes to this project are documented in this file. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.15.1] - 2026-07-11

### Changed

- **Docs caught up with the 0.15.0 flows-arc.** `ai_segment` joins the reflexive-op documentation
  everywhere the family is enumerated (`crates/flux-flow/docs/ops-reference.md` — full
  signature/return/scoping row — plus `docs/agent-loop.md` and `docs/architecture.md`);
  the agent-loop and architecture docs now describe the inverted, flow-driven session mode
  (`FlowEngine::start_flow_turn`, text + voice) with pointers to both designs; the architecture
  crate map credits `flux-flow`'s voice driver; `docs/roadmap.md`'s status header is current
  (0.15.0, crates.io publish shipped); and `docs/usage.md` documents `flux auth login codex`
  as the expired-login recovery path the new auth error points to.
- **Website docs caught up too.** `agent/realtime.md` no longer claims the model always owns the
  turn — it documents the flow-driven voice mode (`run_flow_turns`/`EngineVoiceHandler`) and the
  0.15.0 `VoiceReply` breaking change; `language/durability.md` gains a "Flow-driven sessions"
  section (the `await`-as-conversation model + the `ai_segment` reflexive op); `agent/agent-loop.md`
  introduces `ai_segment` as the third reflexive op; `language/types-and-effects.md` and
  `sdk/flow-client.md` document `annotate_effects`; `troubleshooting.md` gains an
  expired-subscription-login entry.

## [0.15.0] - 2026-07-11

### Added

- **D-131: Flow-driven session primitive — run an authored flow as the conversation driver.** New
  public `FlowEngine::start_flow_turn(session_id, flow, sink)` executes an authored flow **fresh** to
  its first top-level `await`, persists the suspension, and surfaces the flow's own **authored
  prompt** (its last emitted view) as the assistant turn — no planner is invoked for the
  deterministic skeleton (a two-`await` flow driven turn-by-turn makes zero provider calls). Every
  later `run_turn` routes through the existing suspension-first branch, so resume is unchanged;
  `resume_suspended` and `start_flow_turn` now surface `outcome.result` on suspension (the fixed
  "awaiting your input" hint remains only as the empty-emit fallback) and the flow's result on
  completion. The full authorization → approval → guarded-IO envelope applies inside a flow-driven
  session exactly as on the planner path (a `RiskApprover` denies a flow-dispatched destructive op
  identically). **Bounded model-segment delegation:** a new reflexive `ai_segment(goal, tools,
  max_rounds, until?)` op (a third `LoopHost` method, routed like `plan`/`run_plan`) hands a bounded
  run of model turns to the loop under a capability scope + explicit exit condition, then returns
  control to the deterministic flow — "a deterministic skeleton with visibly-bounded
  non-deterministic segments." The delegated leaf ops are confined to `tools` (dispatch-floor + a
  restricted advertised catalog, so an out-of-scope op is refused and never runs); the run is capped
  at the required `max_rounds`; and it exits early on natural completion or the optional `until`
  symbol becoming bound to a non-empty value. Both flow-driven paths arm the reflexive loop host and
  fold a segment's planner spend into the turn's usage/telemetry. Design:
  `docs/designs/flow-driven-session.md`.

- **D-132: Flow-driven voice — the realtime driver defers to flow suspensions.** Extends D-131 to the
  realtime/voice channel: an authored flow drives the call, the driver **speaks the flow's authored
  prompts** (TTS) and **resumes the suspension on caller input**, with model cognition only where the
  flow calls it (an `ai_segment`). The voice driver's Phase-2 `VoiceTurnHandler` seam is now real —
  **BREAKING:** `VoiceTurnHandler::turn` changes its return type from `String` to `VoiceReply`
  (`Continue`/`Complete`); external implementors return `VoiceReply::Continue(text)` for the old
  behavior. The trait also gains `start()` (the flow speaks first) and `VoiceSink` gains
  `session_ended` — both additive, with defaults.
  `VoiceSessionDriver::run_flow_turns` speaks the opening prompt at `SessionReady` and, on completion,
  fires the new `VoiceSink::session_ended` hangup/handoff hook before ending the session loop. The
  bridge is `EngineVoiceHandler` — a `FlowEngine`-backed handler that runs `start_flow_turn`/`run_turn`
  through a `PromptCapture` sink and classifies continue-vs-complete via the new non-consuming
  `FlowStore::has_suspension`. Barge-in and the authorization → approval → guarded-IO envelope are
  untouched (ops dispatch through the engine's shared executor; the driver's own executor is unused in
  flow mode), and turns/usage record exactly as for a text turn. No `realtime` feature needed for the
  bridge or its test. Design: `docs/designs/flow-driven-voice.md`.

- **D-133: `annotate_effects` — per-node effect/risk annotation over an analyzed flow.** New
  `flux_lang::analyze::annotate_effects(ast, ops)` walks a flow and returns, per `call` node, an
  `EffectAnnotation` (`{effects, risk, idempotency}`) keyed by the same JSON-pointer-style node path
  diagnostics already render (`body[3].then[1]`) — the per-node, attributed sibling of
  `HirFlow::effects`'s deduped flow-level union (right for the approval envelope, lossy for "which
  node did this"). `effects` folds in both of `gather_effects`'s contribution sources — the op's own
  host effects mapped onto `FlowEffect`, plus the semantic tag declared on an immediately enclosing
  `bind`/`memo` (e.g. `$charge = call(charge_card, {…}) effect: money`) — so a consumer (e.g. a
  visual editor pinning `Money`/`High`-risk nodes) can tell exactly which call moves money instead of
  only that something in the flow does. An unregistered op annotates honestly as `None` rather than
  being silently skipped, matching the analyzer's own "unknown operation" diagnostic.

### Changed

- **README identity and first-screen refresh.** Replaced the gradient orbit/lambda logo with a flat,
  dark-mode-aware execution-gate mark and path-based wordmark, then rebuilt the repository header as
  a theme-safe technical hero around flux's actual promise: models produce typed plans while the
  deterministic runtime owns effects. Added standalone vector/raster mark assets and documented the
  production brand system.

### Fixed

- **OAuth token-refresh failures no longer surface as a cryptic decode crash.** A failed
  `codex`/`claude` token refresh (e.g. an expired refresh token) returns an OAuth *error* body, and
  OpenAI (codex) wraps it in a **nested** envelope (`{"error":{"message":…,"type":…}}`). The old
  `parse_token_resp` decoded *every* response into the success-shaped `TokenResp` (whose `error` is
  `Option<String>`), so a nested envelope died with `auth error: decode refresh response (status 401
  Unauthorized): invalid type: map, expected a string` — hiding the real reason. Non-2xx bodies are
  now read leniently (`refreshed_from_body` + `oauth_error_detail`, tolerant of both the RFC-6749
  flat form and the nested envelope) and surfaced as the actual reason plus an actionable
  `Re-authenticate with \`flux auth login <provider>\`` hint for refresh failures. Regression tests
  cover the nested envelope, the flat form, a non-JSON body, and the success path.

## [0.14.9] - 2026-07-11

### Added

- **D-134: OS-level process sandbox — abstraction, config, and threading (no backend yet).**
  New `crates/flux-system/src/sandbox.rs` module: `SandboxSettings`/`SandboxMode` (Off/On/Require,
  `from_env` reading `FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/`FLUX_SANDBOX_WRITABLE`), a `Backend` enum
  declaring all three eventual variants (`Bubblewrap`/`Seatbelt`/`Unsupported`) though `resolve()`
  only ever yields `Unsupported` in this story, `Sandbox` (`disabled`/`resolve`/`is_active`/
  `ensure_available`/`preflight`/`configure`/`wrap_argv`), `SpawnPolicy::for_workspace`, and
  `Confinement::{Sandboxed, Exempt}`. `System::build_command` — the one process choke point — now
  takes an explicit `Confinement`; all five spawn modes pass it (`spawn_debug_pipe` is `Exempt`,
  documented as a deliberate v1 exception for Chrome's own sandbox; the rest are `Sandboxed`), and
  wraps at the top so `current_dir`/`kill_on_drop`/`process_group`/`apply_safe_env` apply to the
  future wrapper unchanged. A `Require`-mode sandbox with no usable backend refuses to spawn (the
  fail-closed backstop); a genuinely-active sandboxed spawn injects `FLUX_SANDBOXED=1`, now part of
  `SAFE_ENV` so the marker survives descendants' env-clear and `Sandbox::resolve` treats a nested
  flux invocation as `Backend::AlreadyConfined` (confined by the parent; see the hardening pass
  below). `flux-config` gains
  `SandboxConfig { enabled, require, network, writable }` on `Config` with a security-directional
  merge (`enabled`/`require` OR, `network` strictest-wins, `writable` concatenates). `flux-cli`
  gains global `--sandbox`/`--no-sandbox` (conflicting) flags and `apply_sandbox_env`, resolving
  the tightest posture across flag/env/config (with `--no-sandbox`/`FLUX_SANDBOX=off` as the kill
  switch; see the hardening pass below) and exporting `FLUX_SANDBOX`/`FLUX_SANDBOX_NET`/
  `FLUX_SANDBOX_WRITABLE` so child flux invocations inherit the posture; startup runs the preflight
  (a hard error under `require`+unavailable, otherwise one styled warning). `System::from_env(cwd)`
  (`Workspace::from_env` + `Sandbox::resolve`) replaces the production
  `System::new(Workspace::from_env(..))` call sites; `with_sandbox` is the builder for
  custom-workspace sites; `System::new` stays env-free and infallible. Opt-in and default off — on
  every platform today this story's behavior (settings plumbing, warnings, fail-closed `require`)
  IS the shipped behavior, since no real backend exists yet (bubblewrap lands in D-135, Seatbelt in
  D-136).

- **D-135/D-136: real bubblewrap (Linux) and Seatbelt (macOS) sandbox backends.** The D-134
  `bubblewrap_argv`/`seatbelt_argv` stubs are now real, filling in the abstraction's two argv
  builders. **Linux (bubblewrap)**: the verified baseline template —
  `--die-with-parent --unshare-pid --unshare-ipc --unshare-uts --unshare-cgroup-try` (+
  `--unshare-net` iff network is off), `--ro-bind / / --dev /dev --proc /proc --tmpfs /run` (+
  narrow resolver-file rebinds when network is on), `--bind /tmp /tmp` + the workspace root (real
  `--bind`, must exist) + automatic writable roots (`--bind-try`) and required configured roots
  (`--bind`, created before launch), `--chdir <root> --`; `--allow-all-paths` collapses the fs binds
  to a single `--bind / /` while keeping lifecycle/network/`/run`-masking flags. **macOS
  (Seatbelt)**: `sandbox-exec -D WS_ROOT=<canon> -D TMP=<canon> -D W0=<canon>… -p <profile>
  <argv>` — no `--` separator (`sandbox-exec`'s CLI grammar has none); the generated SBPL profile
  is `(version 1)(allow default)(deny file-write*)` widened back open under `WS_ROOT`/`TMP`/the
  fixed `/private/tmp`+`/private/var/tmp` roots/extras, with device carve-outs
  (`/dev/null`/`/dev/zero`/ttys/`/dev/fd/`) and `(deny network*)` when network is off; writable
  paths are canonicalized before emission, and any path containing a `"` or a control character is
  rejected (`Sandbox::wrap_argv`, before the profile is built — a bwrap bind is a separate execv
  argv entry with nothing to escape, so that backend has no equivalent check). Both backends share
  one discovery shape (env override → PATH, always resolved to an **absolute** path — `bwrap`:
  `FLUX_BWRAP_BIN`; `sandbox-exec`: `FLUX_SANDBOX_EXEC_BIN` → the fixed `/usr/bin/sandbox-exec`)
  and one preflight probe (`bwrap`'s baseline flags / `sandbox-exec`'s minimal allow-all profile,
  against `true`, ~2s timeout, cached by binary path): `Missing` (spawn failure),
  `NamespacesDenied` (Linux-only — stderr matches known unprivileged-userns-refusal patterns;
  expected inside default-seccomp Docker and hardened kernels), or `Broken(stderr)`. The probe now
  runs *inside* `Sandbox::resolve()` itself (a deliberate departure from treating discovery as
  presence-only): a backend that exists but doesn't functionally work resolves `Unsupported` with
  the classified reason, so an `on`-mode sandbox auto-degrades to unconfined *before* any real
  spawn is attempted, rather than genuinely trying and failing every subsequent command. `resolve()`
  skips discovery entirely when the sandbox mode is `off`. `SpawnPolicy` gained an `unconfined`
  field mirroring `Workspace::is_unconfined`. Verified live on this Linux dev machine against real
  `bwrap 0.11.2`: sandboxed writes inside the workspace succeed, writes elsewhere under `$HOME`
  fail, a network-off sandbox cannot reach a loopback listener the test itself opened, sandboxed
  `spawn_interactive` round-trips stdin/stdout unchanged, killing a sandboxed background child
  leaves no orphan (the `--die-with-parent`/`--unshare-pid` guarantee), and exit codes propagate.
  The Seatbelt discovery/preflight code (macOS-only by `#[cfg]`) cross-checks clean — zero
  warnings, `cargo check` and `cargo clippy -D warnings` — against a real `x86_64-apple-darwin`
  target from this Linux box, but real-hardware verification is still outstanding (tracked as an
  explicit unchecked "verify on macOS" checklist in D-136; no macOS CI exists yet).

- **D-137: sandbox docs truth pass — shipping the process-sandboxing epic.** This closes
  D-134…D-137: **opt-in OS-level process sandboxing** (bubblewrap on Linux, Seatbelt on macOS) now
  confines shell/exec ops and plugin subprocesses at their spawn boundary — with documented
  browser and trusted-host exemptions — as
  defense-in-depth *underneath* the safety envelope and the plugin capability sandbox. Off by
  default; turn it on with `--sandbox` / `[sandbox] enabled = true` / `FLUX_SANDBOX=on`, or fail
  closed instead of silently degrading with `--sandbox`+`require = true` / `FLUX_SANDBOX=require`.
  New `website/docs/security/os-sandbox.md` documents per-platform coverage (Linux verified live;
  macOS code-complete, pending hardware verification; Windows degrades with a warning — no real
  backend yet), the full `[sandbox]`/CLI-flag/env-var reference, the off/on/require × available/
  degraded posture matrix, the browser (`spawn_debug_pipe`) exemption, and — stated plainly rather
  than overclaimed — what v1 does not defend against (secret reads anywhere on the fs,
  network-on exfiltration, shared-`/tmp` interference, cargo/rustup cache poisoning, anything on
  Windows). The five website pages that promised plugins are "not OS-sandboxed" (`using-plugins`,
  `authoring`, `plugin-sandbox`, `safety`, `infrastructure`), plus `security/overview`,
  `security/plugin-trust`, and `docs/architecture.md`, now say "not OS-sandboxed **by default**"
  and link the new page — the claim was true before this epic and is now only true absent
  `[sandbox]`. `website/docs/reference/config.md` documents the `[sandbox]` table (fields,
  defaults, security-directional merge); `troubleshooting.md` gains entries for a missing
  `bwrap` binary and for `NamespacesDenied` (the expected auto-degrade inside default-seccomp
  Docker, Debian ≤11, and Ubuntu 23.10+'s AppArmor userns restriction). The
  `plugin_security_copy_keeps_the_native_code_trust_boundary_explicit` contract test now
  drift-guards the qualified phrasing on every occurrence, not just a flat substring check, and a
  new `os_sandbox_page_exists_and_states_its_key_claims` test locks in the new page's key claims.

- **Process-sandboxing hardening pass (xhigh code review remediation).** A recall-mode review of
  the epic surfaced and this closes a batch of correctness gaps before merge:
  - **Posture no longer bypassed on major surfaces.** The env-resolved sandbox is now attached at
    the `flux app run` (journey + sub-agent) and served-agent (`app run --serve`) systems, the SDK
    (`Client`/`FlowClient` builders — default resolve-from-env plus an explicit `with_sandbox`),
    and the runtime git-context spawns — all previously built via `System::new` and therefore
    silently unconfined even under `--sandbox`/`require`. The terminal-bench eval harness stays
    deliberately unsandboxed (it drives Docker; the task container is the boundary) and now says so
    in code and docs.
  - **Posture resolution is tightest-wins, not strict precedence.** `--sandbox` layered over
    `[sandbox] require` stays `require`; an empty/unrecognized `FLUX_SANDBOX` no longer downgrades
    a configured posture (a non-empty garbage value warns); `--no-sandbox`/`FLUX_SANDBOX=off` stay
    the outright kill switch. A config file that fails to parse is now a hard startup error for
    every command, so a typo beside `require = true` cannot reach plugin status/skill generation
    with a default-off posture; clap still serves `--help`/`--version` before config loading.
  - **Nested runs under `require` no longer brick.** A flux invocation genuinely confined by an
    outer flux sandbox resolves the new `Backend::AlreadyConfined` (satisfies `require`, skips
    re-wrapping) instead of failing closed; the `FLUX_SANDBOXED` marker is matched with truthy
    semantics so a spoofed/stale value can't disable confinement.
  - **Backend robustness and no second process path.** The macOS probe degrades instead of
    `panic!`-ing on `Operation not permitted`; backend probes now use `System::build_command`'s
    synchronous guarded mode (safe env, process group, bounded stderr, timeout, descendant cleanup)
    instead of a raw `std::process::Command`. Bubblewrap's inner `true` is resolved to an absolute
    executable from the caller's PATH before scrubbing, so NixOS/Guix layouts work.
  - **Mount/write confinement closed.** Network-on DNS restores only resolver files after the
    `/run` tmpfs, keeping D-Bus, NetworkManager, and systemd-resolved IPC sockets hidden. A writable
    `/` from `TMPDIR`, toolchain env, a named root, or config is rejected unless the workspace is
    explicitly unconfined (whose root bind is safely ordered before special mounts). Configured
    writable directories are created and use required binds rather than silent `--bind-try` skips.
    Linked Git worktrees add their reciprocally validated administrative and common directories to
    the write set, restoring `git add`/ref/object updates without trusting an arbitrary `.git` pointer.
  - **Online local evals keep provider access.** The child `flux` host is launched through an
    explicit trusted-host exemption and receives the resolved sandbox variables; with
    `network = false`, provider HTTP stays outside the child namespace while that host's shell and
    plugin descendants are confined at `System::build_command`.
  - `FLUX_SANDBOX_NET` truthiness, empty `CARGO_HOME`/`RUSTUP_HOME`, and unknown `[sandbox]` keys are
    handled explicitly rather than silently widening or corrupting the profile.

### Changed

- **D-90: gitlab pagination & truncation tell the truth.** Every bound-output gitlab op now
  reports exactly what it returned. `file.show max_bytes` caps the DECODED bytes and re-encodes,
  so `content` is always valid for its `encoding` instead of an undecodable base64 fragment
  (GL-013). All byte caps (`max_diff_bytes`, `max_data_bytes`) are inclusive of their truncation
  marker — the returned string never exceeds the requested maximum (GL-035). `compare` caps
  commits (`max_commits`, default 50) and its top-level `truncated` is now true whenever ANYTHING
  was cut — dropped files, a capped per-file diff, or capped commits — with per-cause
  `files_truncated`/`commits_truncated` flags alongside (GL-014, GL-045). `mr.changes` paginates
  the diff list and applies the `file` filter BEFORE the file cap, so a targeted file beyond the
  first page can no longer come back empty, and reports `files_truncated` distinct from per-file
  `diff_truncated` (GL-042, GL-044). `mr.diff.lines` and `mr.discussion.create` resolve files by
  paginating ALL changed files instead of a hard-coded first 200 (GL-043), and `mr.diff.lines`
  can anchor on a deleted line via `old_line` (GL-047). `repository.archive` refuses archives
  over `max_bytes` (default 50 MiB) instead of staging an unbounded blob (GL-023). List ops take
  an explicit 1-based `page` input and reject over-cap limits via the schema maximum instead of
  silently clamping (GL-019).

- **D-89: gitlab read defaults are honest — no silent scope-broadening or -narrowing.** `per_page`
  is now a documented alias of `limit` on every paginating gitlab op (previously accepted-but-ignored,
  GL-009), and non-positive `limit`/`per_page`/`max_bytes`/`max_files`/`max_diff_bytes`/
  `max_data_bytes` values are rejected by the shared D-88 preflight instead of silently expanding to
  the default page size / no cap (GL-010, GL-024). `search.blobs` rejects the ambiguous
  `project`+`group` combination and a group-scoped `ref` (previously project silently won and `ref`
  was dropped, GL-032/GL-041), and its description documents that instance-global search needs
  GitLab advanced/exact code search (GL-007). `job.list scope` entries are typed against the CI
  status set, so a non-string or unknown status is a validation error, not a silently-skipped entry
  (GL-033). `index.build` with an unknown selector (a typo like `porjects`) is a validation error in
  both dry-run and the live call instead of an empty `indexed: 0` success — mixed lists with a typo
  are rejected too (GL-034). `mr.list` gains a typed state filter (opened|closed|locked|merged|all)
  and, with `issue.list` and `project.list`, documents its default scope: state defaults to `opened`
  (index.build indexes all states) and project listing is membership-only unless `membership=false`
  (GL-018, GL-038).

## [0.14.8] - 2026-07-10

### Added

- **D-130: Kubernetes-authenticated Vault credential stores.** Host applications can now construct
  `VaultCredentialStore` with a Vault Kubernetes role and projected service-account JWT instead of
  mounting a long-lived `VAULT_TOKEN`. Construction logs in eagerly; the store serializes lease
  renewal, re-reads kubelet's rotated JWT when re-authenticating, and retries one KV-v2 request after
  a 401/403. The existing static-token constructor, `from_env`, KV-v2 wire shape, and local
  `FileCredentialStore` default remain compatible. Loopback tests cover login, read/write, renewal,
  rotated-JWT re-login, and the static-token regression. Downstream ai-agent-platform C-27 is the
  first deployment consumer.

- **D-88: shared dry-run/runtime validation layer for plugins.** host-kit plugins now run ONE
  preflight in both the CLI's `--dry-run` and runtime dispatch, closing the gap where `--dry-run`
  trusted only the generated JSON schema while handlers enforced more (a beta pass found ~14
  gitlab ops answering `valid: true` for inputs guaranteed to fail before any HTTP). host-kit
  gains a flex-aware schema validator (required + blank-string rejection, enum membership,
  positive-id ranges, non-empty arrays, typed nested payload elements, `$defs` refs; unknown
  fields *warn* on open schemas and hard-fail under `additionalProperties: false`), a
  `PluginBuilder::preflight(op, rule)` hook for constraints schemas cannot express, and an
  auto-registered internal `plugin.validate` op (never projected as a model tool). `flux plugin
  call --dry-run` feature-detects that op and merges its verdict (plus a new `warnings` array)
  into the printed result; runtime dispatch rejects the same problems before the handler runs, so
  the two paths cannot disagree. The gitlab plugin adopts the layer end-to-end: enum-validated
  `state`/`visibility`/`link_type`/`variable_type`/archive `format`, non-empty + element-typed
  commit `actions`/snippet `files`/pipeline `variables`, typed `labels`/`assignee_ids`, positive
  id/iid bounds, conditional-target rules (`ref` OR `project`+`iid`, `snippet_id`/`id`), regex
  compilation for `mr.diff.lines search`, consistent empty-update guards on
  `mr.update`/`issue.update`/`release.update`, and schema-surfaced aliases (`tag`/`name`→
  `tag_name`, `name`→`branch`, `id`→`snippet_id`). The release ops' undocumented `name`→`tag_name`
  fallback is dropped — `name` is the release/link display-name field there, and the old chain
  could silently treat a display name as the tag. `mr.discussion.create` now documents its
  `dry_run` field as a server-side anchor preview, distinct from the CLI's local `--dry-run`
  (GL-004/008/011/012/020/021/022/024/025/027/028/029/030/036).

### Fixed

- **D-127: slack mrkdwn→Markdown renderer no longer mangles or panics on multi-byte chars.** The
  converter's fallthrough copied one *byte* per iteration (`bytes[i] as char`), turning every
  multi-byte UTF-8 char into mojibake and leaving the index mid-sequence, so the next slice
  panicked (`byte index … is not a char boundary; it is inside '—'`) and killed the plugin process
  — `slack.message.list`/`slack.thread`/`slack.mentions` default to Markdown conversion, so
  reading any channel containing an em-dash, umlaut, or emoji crashed. The fallthrough now
  advances char-wise. (Ships with the next plugins pack cut; until then `text_format: "mrkdwn"`
  is the workaround.)
- **D-128: `slack.file.upload` completes against the real Slack API.** The pre-signed-URL bytes
  leg used PUT — `files.slack.com` answers with a 302 redirect and the upload never landed — and
  `alt_text` was sent inside the `files.completeUploadExternal` files entry, which accepts only
  `id`/`title` (`invalid_arguments`). The bytes leg now POSTs per the `files.getUploadURLExternal`
  contract (the upload unit test asserts the verb via the MockHost call log) and alt text rides
  the reserve call as `alt_txt`. (Ships with the next plugins pack cut.)

### Changed

- **D-129: plugin manifests inherit the pack workspace version.** All 19 integration plugins now
  report `env!("CARGO_PKG_VERSION")` as their manifest version instead of hand-maintained string
  literals (kubernetes had already drifted to a self-reported `0.2.0` against its `0.1.0`
  descriptor — the exact mismatch `flux plugin status` warns about). The plugins workspace is
  bumped to **0.1.1** and released as pack `plugins-v0.1.1`, delivering the D-127/D-128 slack
  fixes; descriptor, index, and manifest versions now agree by construction on every future cut.

### Added

- **Website: worked Slack plugin setup page** (`plugins/slack`) — install, bot/user tokens via env
  or `flux auth set` (with the 0.14.7 version gate called out), `slack.test` verification, and real
  operations; the first page of the per-plugin setup series alongside `plugins/gitlab`. The
  *Credentials & secrets* page documents stored plain plugin tokens (`flux auth set`, stored-wins /
  env-fallback precedence, `--clear`), and *Using plugins* points plain-bearer plugins at it.

## [0.14.7] - 2026-07-10

### Added

- **D-126: `flux auth set <plugin> [<purpose>]` — stored bearer tokens for plugin auth purposes.**
  Plain (non-OAuth2) auth methods resolved ONLY from the host process environment; there was no way
  to configure a plugin credential in advance for a session whose env can't carry the secret.
  `flux auth set` validates the plugin + purpose against the live manifest (purpose optional when
  exactly one is declared), prompts hidden for the token (reads one stdin line when piped, so it
  scripts), and stores it under `plugin:<name>:<purpose>` in `~/.flux/credentials.toml` (0600) —
  the same key/store plugin OAuth logins (D-82) use. `SystemHostCaps::resolve_purpose` now consults
  the credential store first for plain methods (a stored token wins over env, matching the OAuth2
  store-first rule; the resolved value is registered with the secret sink), falling back to the
  declared env keys. `--clear` removes a stored token (new `flux_credentials::delete_token`).
  `flux plugin status` shows a stored plain bearer as the active resolution and points
  `not configured` purposes at the right configure command (`auth set` vs `auth login`).

### Changed

- **`codewandler-flux-host-kit` now versions in LOCKSTEP with the flux workspace.** The plugin
  SDK's independent version line (0.2.x → flux 0.13; a short-lived unpublished 0.3.0) silently
  froze whenever nothing remembered to bump it, leaving consumers on stale flux dep lines.
  `scripts/cut-release.sh` now bumps `plugins/host-kit/Cargo.toml` (and stages it) on every cut, so
  each flux release publishes a host-kit whose version and flux dep line match the flux crates
  exactly — host-kit X.Y.Z is compatible with flux X.Y.Z by construction. External consumers
  should repin `codewandler-flux-host-kit` from `^0.2` to the current flux minor (e.g. `^0.14`).

### Fixed

- **D-125: ref-based `http.do` no longer drops a path-bearing endpoint base.** `compose_url` joined
  the plugin-supplied op path onto the resolved endpoint base with RFC-3986 `Url::join`, so a
  leading-slash path *replaced* the base's path — `slack.endpoint`'s default `https://slack.com/api`
  + `/auth.test` composed to `https://slack.com/auth.test`, 404ing **every** op of any plugin whose
  endpoint base carries a path segment (host-kit's `MockHost` joins by concatenation, so plugin
  tests never caught the divergence). The join is now slash-normalized concatenation — the same
  semantic as `MockHost::join_url` and the OAuth `token_path` join — which also means a full-URL
  `path` can no longer swap out the pinned endpoint base (it fails the egress guard's URL parse
  instead). Host-only bases (the gitlab/jira shape) compose identically before and after.

## [0.14.6] - 2026-07-10

### Added

- **D-116: static endpoint wiring — `flux endpoint add` + config bindings that resolve.** A new
  `flux endpoint add <id> --url <url> [--product] [--protocol] [--credential-ref <ref>] [--label
  k=v]…` persists a weak, credential-free config-bound `EndpointRef` to `~/.flux/endpoints.toml`,
  rejecting a credential-bearing URL (points to `--credential-ref`), an `@endpoint/…` id (reserved
  for discovered refs), and an unparseable credential ref. A declarative `[[endpoint.static]]`
  array-of-tables under the config `[endpoint]` table is the config-as-code alternative, merged into
  the session registry at startup (invalid entries warned-and-skipped). Both surfaces now populate
  the `StaticResolver` binding map — previously constructed with an empty `HashMap`, so only
  discovered `@endpoint/*` refs resolved — via the new `EndpointRegistry::config_bindings()`, so a
  named/config-bound `endpoint_ref` resolves through the broker's `ReferenceResolver` chain at
  connect time. This closes the "wire a service with an endpoint, then the agent can use it" loop
  without a Kubernetes provider (the canonical case: a Postgres database reached by host-terminated
  SCRAM). `flux_config::EndpointConfig` gains `static_endpoints` (`StaticEndpoint`); `run_endpoint`
  is refactored to a path-parameterized `run_endpoint_in` for testing.

### Changed

- **D-117: public website truth pass and drift guards.** Corrected stale SDK installation/package
  selectors, private-network configuration, plugin dry-run behavior, GitLab surface counts,
  Flux-Lang examples, native-web operations, and native-plugin trust-boundary claims. Added public
  endpoint, saved-flow, skills/roles, Improvement-loop, complete CLI/config, and generated customer
  changelog entry points. New executable docs contracts cover public CLI commands, registered
  operations, TOML configuration, complete Flux fences, plugin-pack membership, SDK commands, and
  security wording; the website workflow now builds previews from main/PRs but deploys stable Pages
  only for a published release or an explicit dispatch.
- The release cutter now regenerates and stages the website's customer-changelog mirror after
  rolling `WHATS-NEW.md`, keeping the generated drift guard green during the release gate.

## [0.14.5] - 2026-07-10

### Added

- **A-65 — dense daily-driver TUI makeover.** `flux-tui` now uses a borderless, viewport-cached
  transcript and background-only multiline composer; async terminal input adds atomic bracketed
  paste without idle polling, a visible editable FIFO follow-up queue, contextual scroll/follow
  status, a session picker, real `/new`/`/clear`, durable `/resume` reconstruction, and in-place
  `/model`, `/plan`/`/run`, `/shell`, `/tools`, `/evidence`, and `/compact` controls. CLI and TUI now
  share `FlowEngine`'s cancellable reviewed-plan execution and atomic model-switch seams. Durable
  `RunEvent::OpRecorded` cells carry an optional redacted/capped display input (serde-defaulted for
  old logs and kept separate from replay hashes/result truncation), so resumed tool cards remain
  useful without re-executing effects. Terminal setup unwinds raw/alternate/mouse/paste/cursor state
  in reverse order, late action events are rejected by generation id, and `always allow` choices are
  persisted even when a later TUI error occurs. Review hardening keeps queue edits in their original
  FIFO slot, protects the active empty session from pruning, routes `/model mock` to the offline
  provider, preserves the active engine model during projection, reconstructs reduced historical
  cards when cassette capture was disabled, and redacts decoded JSON string leaves so escaped
  secrets cannot survive in durable input views.

## [0.14.4] - 2026-07-10

### Fixed

- **gitlab plugin: CI job-token allowlist write ops 400'd on a `namespace/path` project.**
  `gitlab.ci.job_token.allowlist.{add,remove}` and `.groups_allowlist.{add,remove}` sent the
  URL-encoded `namespace%2Fproject` path form to GitLab's `job_token_scope/allowlist` and
  `groups_allowlist` POST/DELETE endpoints, which reject it with `400 {"error":"id is invalid"}` —
  unlike the matching GET, which accepts it. These four ops now resolve `project` to its numeric id
  via `/projects/:id` first and use that in the write path.

## [0.14.2] - 2026-07-10

### Changed

- Flux-Lang editor onboarding now uses the tested `flux-tree-sitter` Helix installer as its single
  setup/update path. The public guide covers immutable grammar revisions, matched parser/query
  updates, Flux-only builds, health-vs-version checks, semantic capture inspection, theme-controlled
  colours, and expected optional-query gaps; the repository-local Helix config pins the verified
  grammar revision and links back to that guide.

## [0.14.1] - 2026-07-10

### Added

- **L-79 — run saved flows directly from the CLI.** `flux flow list` (alias `ls`) now prints the
  same project/global flows-and-ops catalog as `flow_list` without constructing an agent session;
  `flux flow run <target>` resolves an existing path first, then a saved filename stem or declared
  flow name. Declared parameters are a strict CLI contract: `--inputs <JSON object>` and repeatable
  `--arg key=value` merge with typed coercion and last-arg-wins precedence, rejecting malformed JSON,
  unknown/missing keys, and deterministic type mismatches before execution. Optional
  `--map-inputs <text>` lowers missing parameters into recorded `ai.extract` → JSON parse →
  cardinality assertion → strict field-bind nodes, uses the same `-m` as authored model ops, and is
  skipped when deterministic inputs are complete. Discovery/resolution is now one System-backed
  `flux-tools` catalog shared by `flow_list`, `flow_run`, `flow_render`, and the CLI. Explicit
  module-local composites shadow same-named auto-loaded definitions before validation, fixing direct
  runs of mixed flow/op files inside a flows home.

### Fixed

- **v0.14.0 regression: `flux app run --serve <addr>` (space form, no program) was silently
  misparsed.** The CLI review wave's `require_equals = true` on `--serve` was meant to stop a
  *preceding* bare `--serve` from swallowing a following `<program>` positional, but it also broke
  the far more common documented form (`flux app run --serve <addr> --yes`, no program) used
  throughout README.md, the public website docs, `docs/a2a.md`, and the flux-markdown test
  corpora — the address silently fell back to the default and got misread as `<program>` instead.
  Reverted `--serve` to its pre-review-wave optional-value behavior; `plugin install --dir` keeps
  `require_equals` (every documented use there is the bare, no-value form, so there's no
  equivalent breakage). Disambiguate the original footgun by argument order
  (`flux app run prog.flux --serve`) or `--serve=addr`.
- `plugins/{alertmanager,aws}`: both plugins' leap-year checks used `u64::is_multiple_of`, stable
  only since Rust 1.87 while the plugins workspace MSRV is pinned to 1.85 — pre-existing,
  unrelated to the CLI review wave, caught by CI (`cut-release.sh`'s local gate checks
  plugins-workspace `fmt` but not `clippy`). Rewritten as plain modulo checks.

## [0.14.0] - 2026-07-10

### Changed

- **CLI review wave — the flag surface is now honest.** (Some of these are CLI-surface breaking;
  per the project SemVer rule the next release should be a MINOR bump.)
  - `flux run <file>` program detection keys on the `.flux` extension ONLY — any other existing
    file as the first prompt word no longer hijacks the turn into program mode (`flux run
    Cargo.toml explain the workspace deps` is a turn about Cargo.toml again), and trailing words
    after a program path are a loud error instead of silently dropped.
  - `flux review` accepts only the flags it honors (`-m/--model`, `--max-tokens`). The turn/session
    flags it silently ignored (`--yes`, `--continue`, `--resume`, `--skill-dir`, `--dev`,
    `--turn-budget`, `-v`) are now parse errors — review always auto-approves its fixed read-only
    flow. Likewise `flux fork` rejects `--continue`/`--resume`, and `flux app run <program>`
    rejects `--continue`/`--resume`/`--dev`/`--skill-dir`/`--turn-budget` at runtime.
  - Optional-value flags take `=` now: `flux app run --serve=ADDR`, `flux plugin install
    --dir=PATH`. The space form swallowed a following positional (`--serve prog.flux` silently
    served the built-in agent instead of the program).
  - `--color auto` requires BOTH stdout and stderr to be terminals, so `flux usage > report.txt`
    no longer embeds ANSI escapes in the file.

### Fixed

- Flux-Lang editor diagnostics now use the stable CLI cognition, datasource, and native-web
  operation catalog, eliminating false `unknown operation` warnings for calls such as `ai.extract`,
  `search`, and `web_fetch`. Multi-declaration modules are analyzed declaration-by-declaration with
  module-local composite-op resolution and precise body ranges; module formatting stays disabled
  until declaration order can be preserved safely.
- **Boolean `FLUX_*` env signals are value-parsed, not presence-checked** (one owner:
  `flux_system::env_truthy`). `FLUX_ALLOW_PRIVATE_NET=0` previously ENABLED private-network egress
  for plugins and `web_fetch`; now only `1|true|yes|on` counts — same for `FLUX_VERBOSE`,
  `FLUX_SHOW_LOOP`, `FLUX_TRACE_LOOP`. The private-net warning also fires when the grant arrives
  via the env var, naming the actual source (ditto the sandbox-off warning, which no longer blames
  `--allow-all-paths` for a config/env grant).
- **Offline paths stay offline:** the lazy engine paths (`flux replay`, `flow run`) no longer
  eagerly build the cognition pack's sibling provider — with an `aws/…` model that ran the
  SSO/IMDS credential chain over the network during a "no live IO" replay. The sibling is now
  deferred exactly like the engine's own provider.
- **`flux plugin status` PATH-searches bare program names.** `Path::parent()` is `Some("")` for a
  one-component name, so the PATH-search branch was unreachable and every bare-name plugin
  rendered a red `missing` while `plugin call` spawned it fine.
- **Config/limit errors are loud:** a malformed `FLUX_TURN_TOKEN_BUDGET` is a hard error instead
  of silently removing the spend ceiling (`FLUX_COMPACT_CHARS` warns); a malformed
  `.flux/config.toml` fails `flux plugin call` / `flux auth status` with the parse error instead
  of silently dropping `[private_net]` grants / showing the wrong default model.
- **Validation moved to clap where clap can own it:** `flux completion <shell>` is a `ValueEnum`
  (unknown shell exits 2 instead of writing an empty script and exiting 0); fork `--prompt`
  conflicts with `--inject`/`--edit`; `--resume-value` requires `--resume`; `changelog`
  `<version>`/`--all`/`--unreleased` are mutually exclusive; `plugin install` modes are exclusive;
  `plugin call --dry-run` conflicts with `--no-validate`; `skill --global` requires `--install`
  on both surfaces and `plugin skill --out` conflicts with `--install`/`--global`; `auth login
  claude|codex --password` is rejected; `eval` takes a typed adapter and `--members` must pair
  with `multi`; zero is rejected for `--max-tokens`, `--turn-budget`, `--trials`, and the 1-based
  `replay --turn`.
- **`flux tui -v` works:** verbose now reaches the TUI — tool cards start expanded and the
  30-line detail cap lifts. The flag was silently dead there.
- **REPL/turn polish:** a failed `/run` ends the turn properly (cost line printed, spinner
  stopped, instead of the spinner redrawing over the prompt); "always allow" choices persist even
  when the turn later fails or `/clear` hits a store error; a `/model` switch reports a failed
  persistence instead of silently mis-attributing usage; `flux plan --yes` in print-only mode
  notes that `--yes` has no effect; a cassette-recording failure on `flow run` warns at record
  time ("this run won't be replayable") instead of surfacing turns later in `flux replay`; a flow
  store read error on resume propagates instead of masquerading as a bogus `unbound symbol`.
- **OAuth logins are bounded and honest:** `flux auth login codex` times out after 300s like the
  generic plugin flow (was: waited forever on an abandoned browser), callback socket read errors
  are reported instead of masquerading as empty requests, and the interactive prompts run off the
  runtime threads.
- **Process-env hygiene:** every startup env export now happens before the tokio runtime spawns
  worker threads, and the REPL `/shell` toggle + config `enable_shell` flip an in-process runtime
  override instead of calling `set_var` on a live multi-threaded runtime (a data race with any
  concurrent `getenv`; UB on glibc). `flux a2a --token`'s `FLUX_A2A_TOKEN` fallback is declared on
  the flag (shows in `--help`).
- **Smaller honesty fixes:** `plugin status` no longer claims an endpoint/auth is configured when
  its env var is set but empty (matching resolution); `plugin call --dry-run` help states the
  plugin process is spawned for its manifest (the op is never invoked); the workspace doc indexer
  size-checks via metadata instead of reading oversized files fully on every agent construction;
  `flux usage` survives an unreadable JSONL file (skip-counted, scan continues) and counts covered
  days on calendar days; token counts hand off units cleanly (`1.0M`, never `1000.0k`);
  `flux preset` rejects unknown keys and GNU-style flags instead of swallowing them, and errors on
  `-o` with `--run` / `-m` without `--run`; the generated plugin-skill page emits the footnote its
  `Medium*` risk marker references.

## [0.13.3] - 2026-07-10

### Documentation

- Added a public Infrastructure page with an architecture diagram and a concise guide to flux's
  planner/runtime split, mandatory safety envelope, three pillars, and L0-L6 crate layering.

### Fixed

- **C-50 audit hardening wave.** Native web and plugin HTTP now re-authorize every redirect hop,
  strip credentials across origins, reject HTTPS-to-HTTP downgrades, and stream bounded response
  bodies. Guarded subprocesses drain output while running and kill/reap their process group on
  timeout or cancellation. Flow compaction keeps provider-valid sessions and usage accounting,
  configured iteration limits control the real loop, and invalid eval candidates cannot win through
  zeroed telemetry. Filesystem permission subjects now follow physical symlink identity, plugin
  protocol cancellation poisons and restarts an ambiguous child, Flux-Lang rejects unsafe cleanup
  scopes in cancellable branches, retries honor bounded `Retry-After` delays, eval trials support
  deterministic bounded concurrency, and the plugin workspace inherits the declared MSRV.

## [0.13.2] - 2026-07-09

### Added

- **L-77 — `flux render` CLI subcommand: `.flux` file → One-Dark SVG, and the doc-image
  generator.** `flux render <file.flux> [--view source|tree] [-o out.svg]` — the non-model entry
  point to L-76's `flow_render` renderer: the highlighted source (default) or the execution-path
  plan tree, as a self-contained SVG. Without `-o` the SVG prints to stdout; `-o` writes through
  the workspace-confined `System` (parents created). A hard parse error in `tree` view exits
  non-zero with the parser's message; `source` view is total (malformed input still renders).
  This replaces the `codewandler/flux-tree-sitter` repo's `scripts/render-example.mjs` Node
  script as the way flux regenerates its own doc images (that repo's README/AGENTS now point at
  `flux render`; companion change there). No new deps.

### Fixed

- **D-115 — endpoint ops surface from the endpoints store.** A registered endpoint
  (`~/.flux/endpoints.toml`) now surfaces the `endpoint` op group without a kubeconfig: the CLI
  injects a session-ambient `endpoint` signal when its startup-loaded registry is non-empty
  (computed once — never a per-turn re-read; sticky-monotonic surfacing makes startup-static
  sufficient), carried by a new ambient-signals seam on the engine (`AgentSpec.ambient_signals` →
  `FlowEngine::with_ambient_signals`, appended to every turn's workspace-probed signals). A
  corrupt endpoints store is now reported at startup ("fix or remove it") instead of silently
  ignored, and the startup `project.signals` audit observation records the ambient signals
  alongside the probed ones. Housekeeping while there: `endpoint.import` is listed explicitly in
  the group manifest (membership already held via the spec's own group tag — `effective_group`
  falls back to it — so this is explicitness, not a behavior change; a test now pins the
  manifest against `endpoint_tools()`), and its spec is constructed honestly (LocalSystem
  effect, not the read-only constructor). SDK note: `AgentSpec` gained a public
  `ambient_signals` field — literal constructors add the field (or use `..Default::default()`).
- Stale plugin registrations (a descriptor whose recorded binary no longer exists — e.g. a
  deleted checkout) no longer print one "failed to load" warning per plugin on every command.
  They are skipped up front and reported as a single aggregated line naming the plugins and the
  remedy (`flux plugin status <name>` for the recorded path; rebuild/reinstall, or `flux plugin
  uninstall <name>`). Only an ABSOLUTE recorded path positively confirmed absent counts as
  stale — relative paths (cwd-dependent), bare PATH names, stat errors, and Windows programs
  recorded without `.exe` all defer to the spawn, whose real failure still gets its own detailed
  line. The same aggregation now also covers the `flux skill plugin` catalog path.
- Agent-startup plugin loading now goes through the hash-verified spawn path
  (`PluginHost::spawn_verified`): a descriptor with a recorded sha256 whose binary has drifted is
  refused at session start exactly like `flux plugin call`/`status` — previously the agent-startup
  loops spawned without the D-48 check. Hashless local/dev descriptors spawn as before. API note:
  `flux_plugin::load_plugin_tools` now takes the plugin name + descriptor instead of
  program/args.

## [0.13.0] - 2026-07-09

### Changed

- **Dependency refresh (BREAKING): every dependency upgraded to latest across BOTH workspaces
  (root + `plugins/`), plus a new `task deps:upgrade` step.** **BREAKING for `flux-pg` consumers**:
  its `pub use sqlx` re-export moves 0.8 → 0.9 — sqlx 0.9 requires dynamic SQL strings to be
  wrapped in `sqlx::AssertSqlSafe` (an injection-audit gate), so any downstream code building
  `sqlx::query`/`raw_sql` from a non-literal string against `flux_pg::sqlx` types needs the same
  wrap. All other bumps are internal (no other published crate's public API changed) but are
  listed here for completeness: reqwest 0.13 (the `rustls-tls` alias split into `rustls` +
  `webpki-roots`, `form` became a feature), tokio-tungstenite 0.29 (`Message::Text` now carries
  `Utf8Bytes`), sha1/sha2 0.11 + hmac 0.13 (digest 0.11: hex output hand-encoded — byte-identical,
  so cassette/checkpoint keys are unchanged; `new_from_slice` via `KeyInit`), rand 0.10
  (`OsRng`→`SysRng`, `thread_rng`/`gen_range`→`rng`/`random_range`), axum 0.8 (`/:param`→`/{param}`
  routes, `keep_alive` wraps the SSE stream type), rusqlite 0.40, toml 1.1, similar 3, rquickjs
  0.12, cron 0.17, scraper 0.27, ureq 3, fastembed 5, reedline 0.49, slack-morphism 2.22 (the
  `<2.18` cap is obsolete — its missing `signal-hook-tokio 0.4` now exists on crates.io).
  **Deliberately held**: ratatui 0.29 / crossterm 0.28 / ansi-to-tui 7 — markdown-ratatui
  (codewandler/markdown) requires ratatui ^0.29, and a bump would split ratatui into two
  incompatible versions at the flux-markdown seam; the holds are expressed as range requirements
  (`">=0.29, <0.30"`) which `cargo upgrade` skips, with pointers at the declarations. The new
  Taskfile task runs `cargo upgrade --incompatible allow` + `cargo update` in both workspaces.

### Added

- **L-74 — `flux_lang::highlight`: CST-classified highlight spans (L0 substrate).** New pure
  module `flux_lang::highlight` — `highlight(src) -> Vec<(TextRange, HighlightClass)>` classifies
  every token of a `.flux` source by walking the lossless CST (`parse_cst`), keyed on token kind
  *and parent node kind* (the leading ident of a `when`/`flow`/`do` statement is a `Keyword`, a
  `NAME` in type position is a `Type`, `$x` → `Var`, `@effect` → `Annotation`) — strictly more
  accurate than keyword-list matching, and total: malformed/incomplete source still yields spans
  (never panics), with a `"""triple"""` string as one span covering all its lines. The shared
  substrate for the `flow_render` SVG source view (L-76) and, later, flux-lsp semantic tokens
  (L-69). No new deps.
- **L-75 — `flux_lang::render`: span form of the plan-tree renderer.** New public `Role` enum
  (the eight `Palette` roles + `Text` for structural glue) and
  `render_styled_spans(ast) -> Vec<Vec<(String, Role)>>` — the same tree walk emitting lines of
  `(text, role)` spans, with every tree glyph (`├─`/`└─`/`│`, including indent-only runs)
  carrying `Role::Connector`. `render_styled` is now the ANSI-palette stringifier over those
  spans — byte-identical output, pinned by a marker-palette snapshot test written against the
  pre-refactor renderer — so `flux-tui` (ANSI) and the `flow_render` SVG tree view (L-76) share
  one walk instead of a "render to ANSI then parse it back" round-trip. No new deps.
- **L-76 — `flow_render`: Flux-Lang source/plan → syntax-highlighted SVG.** New model-facing
  built-in tool beside `flow_list`/`flow_run` (new `flux_tools::render`, registered in the CLI):
  pass inline `source` or the `name` of a stored flow (resolved through the same
  `.flux/flows`/`~/.flux/flows` dirs as `flow_run`), and `view: "source"` (default) or `"tree"`.
  The source view colours the code via `flux_lang::highlight` (L-74) with a char-indexed grid —
  multi-line `"""strings"""` colour on every line, multi-byte glyphs don't smear — and is total
  (malformed source still renders); the tree view colours `render_styled_spans` (L-75) roles,
  renders `Program.flows` as trees and composite ops as best-effort statement-head lists, and
  returns an error result on unparseable source. Theme ported verbatim from the tree-sitter
  README script (One Dark) so images match; SVG returned inline via `ok_view` with a compact
  `rendered <name> (<view> view) → SVG WxH, N lines` summary — read-only, SVG-only (file
  output/PNG are L-77/L-78). Pure core `render_flux_svg` is directly reusable by the `flux
  render` CLI subcommand (L-77). No new deps.
- **D-114 — `sources` op: enumerate the knowledge datasources the agent can query.** New
  read-only, ungrouped datasource op beside `search`/`get`/`list`/`relation`/`batch_get`: no
  arguments in, per source out its entity types and record count (`DatasourceBackend::sources`,
  implemented for the in-memory, SQLite, and Postgres backends). Closes the discoverability gap —
  `search`/`get`/`list`/`relation`/`batch_get` all take a `source` argument the agent previously
  had to guess (`local`, a program-declared name, a contributing plugin's) or infer from a prior
  `search` hit; now it can just ask. Registered in the same catalog as the other retrieval ops,
  evidence-gated identically (surfaces whenever the datasource group does).

## [0.12.0] - 2026-07-09

### Fixed

- **C-49 — Claude provider hardening: every documented model spec now works or fails
  client-side.** An e2e sweep found `claude/haiku` (and every route to Haiku 4.5 — `anthropic`,
  `aws`, `openrouter-anthropic`) failing with HTTP 400 "adaptive thinking is not supported on
  this model": the Messages quirks profiles ignored the model and always sent
  `thinking: {"type": "adaptive"}`. A shared per-model capability helper
  (`messages::quirks::anthropic_model_caps`) now gates adaptive thinking, `output_config.effort`,
  and the sampling params (`temperature`/`top_p` — rejected outright by Fable 5, Opus ≥ 4.7,
  Sonnet ≥ 5) per model id, across the Anthropic-direct, Bedrock (inference-profile ids), and
  OpenRouter (`anthropic/…` slugs) profiles; unknown/future ids default to the newest shape.
  `claude/` (empty model) is now rejected client-side with a hint instead of round-tripping a
  confusing API 400.

### Changed

- **C-49 — Anthropic model aliases refreshed; `fable` and bare `claude` added.** `sonnet` now
  resolves to `claude-sonnet-5` (the current Sonnet — also flux's default model; previously
  `claude-sonnet-4-6`), `haiku` to the undated `claude-haiku-4-5`, and a new `fable` alias
  resolves to `claude-fable-5` (`claude/fable` previously 404ed). Bare `claude` is now shorthand
  for `claude/sonnet`, mirroring bare `codex`. The pricing mirror, docs (`docs/model.md`), and a
  new website page (`agent/claude-code` — the Claude Code subscription provider, its models, and
  the per-model request invariants) moved in lock-step. Pin `anthropic/claude-sonnet-4-6` to keep
  the previous default.

### Added

- **D-121…D-124 — native non-visual browser (tier 3).** The agent can now drive a real headless
  Chromium without ever receiving HTML source or screenshots. New ops (evidence-gated behind a
  `browser` group that surfaces only when a Chromium binary is discoverable): `browser.open {url?}`,
  `browser.goto`, `browser.snapshot {view}`, `browser.act {action, ref?, value?}`, `browser.close`.
  - **CDP-on-a-pipe (D-121):** a hand-rolled, transport-agnostic CDP client (`flux-web::cdp`, id-
    correlated calls + event stream over `\0`-framed JSON — no WebSocket, no debug port). Chrome is
    spawned through a **new guarded seam** `flux_system::System::spawn_debug_pipe` — a full-duplex
    socketpair mapped onto the child's fd 3/4 in an async-signal-safe `pre_exec` hook, keeping the one
    `build_command` envelope (argv-only, env-cleared, cwd-pinned). Discovery `FLUX_BROWSER_BIN` →
    config `browser_bin` → PATH; **no auto-download**. `Arc<Mutex<…>>` session registry with an idle
    TTL; ephemeral profile per session; `Effect::Process` disclosed.
  - **The digest (D-122):** `browser.snapshot` builds a byte-budgeted digest from the accessibility
    tree joined with DOM identity — a `url · title` header, condensed `## content`, and a `## actions`
    table of `e<N> role "name" (state)` with refs stable across observations (dead nodes marked, never
    renumbered) + a DOM-heuristic fallback for div-soup clickables.
  - **Act + delta (D-123):** `browser.act` clicks/types/fills/selects/presses/scrolls/navigates by
    ref and returns a **delta** — what changed (nav, added/removed refs, dialogs, console errors) — not
    the whole page (`full: true` to override), with a navigation-aware bounded auto-wait.
  - **Egress interception (D-124):** every browser subrequest (navigation, subresource, redirect hop,
    JS `fetch()`) is routed through `guard_url_scoped` via CDP `Fetch` interception — the SSRF chokepoint
    a navigation-only check can't be. Violations fail the request and surface in the digest; a private
    host admitted under the `web` grant audits `PrivateNetAdmit { caller: "web:browser" }`. No off switch.
  - Hermetically tested against a scripted-fake CDP endpoint (no Chrome in CI); an env-gated live smoke
    drives real headless Chrome end-to-end when one is installed.
- **D-120 — `web_fetch` reads pages as condensed markdown; unified `web` egress.** `web_fetch` moved
  into `flux-web` and now returns readable **markdown** for `text/html` responses (navigation/scripts/
  boilerplate stripped, the main content region preferred, cap applied *after* condensation so the
  budget buys content) — non-HTML stays raw, `raw: true` forces the raw body. A new pure op
  `html_to_markdown` condenses an HTML string with no egress, so `http.request → html_to_markdown`
  reads any fetched HTML. The condenser lives in `flux-web::condense` (the html5ever family lands
  here, not in flux-markdown, which stays a pure markdown engine consumed for its AST + writer).
  Fetched pages contribute `web.page` datasource records (title/url/content) so read content is
  groundable. **BREAKING — clean cutover** to the family-wide `web` egress scope: the per-tool
  `effective_web_fetch_private_hosts` path and the `[private_net] web_fetch` config key are deleted
  (a legacy `web_fetch = …` entry is now silently ignored — migrate it to `web`); admissions audit as
  `PrivateNetAdmit { caller: "web:web_fetch" }`. `flux-capabilities::browser` retired.
- **D-98 — `flux-web` crate + `http.request` (native web capabilities, tier 1).** New L5 library
  crate `crates/flux-web` (package `codewandler-flux-web`, path-only/unpublished — the `flux-eval`
  precedent) founds the native web family. Its first op, `http.request`, gives the model raw HTTP
  protocol access — any method/headers/body → status + response headers + a char-boundary-capped
  body; a non-2xx response is a *result*, not an op failure. Header values may be secret references
  (`{"$secret": "ENV"}`) resolved from the environment and seeded into the redactor so a token never
  surfaces in output or persisted events. All flux-web ops answer to one family-wide egress scope:
  the new `[private_net] web` config key (public-only by default; `--allow-private-net` widens it),
  guarded by `flux_system::net::guard_url_scoped` on every request, with private-host admissions
  audited as `PrivateNetAdmit { caller: "web:http.request" }`. Registered via `flux_web::register_web`
  alongside the eval ops. First story of the web-capabilities epic (design:
  `docs/designs/web-capabilities.md`).
- **`flux changelog` + `WHATS-NEW.md` — a customer-centric changelog (C-48).** New repo-root
  `WHATS-NEW.md` holds plain-language, per-release "what has changed" notes (no story IDs, no
  crate names; `### New/Improved/Fixed/Action needed`), embedded into the binary and shown by the
  new `flux changelog` subcommand (own version by default, `--all`, `<version>`, `--unreleased`).
  `scripts/cut-release.sh` now rolls and stages BOTH changelogs and warns loudly when the customer
  section is empty at cut time; AGENTS.md documents the dual-changelog rule.
- **L-73** — Public editor-setup docs page (Helix flagship) + LSP docs pass: new
  `/docs/language/editors` page (tree-sitter highlighting first, `flux-lsp` on top, verified
  Helix recipe with `hx --health flux`; Neovim, Zed, IntelliJ/TextMate coverage; shipped-only
  LSP capability table), the tooling page's editor section slimmed to a pointer, cross-links
  from getting-started/tour/overview/flows-and-syntax, and a `crates/flux-lsp` README.
- **L-54 — content-addressed cache for deterministic read-only ops.** `Executor::dispatch` now
  replays a deterministic read's result instead of re-running its guarded IO when the same op is
  dispatched again with the same input within a turn. The cache sits strictly AFTER the whole
  authorization → approval envelope (a hit is served only when the op is admissible right now),
  and only ops that are read-only (`Read` effects), `Idempotent`, low-risk, approval-insensitive,
  and non-destructive qualify — model calls, writes, and unknown ops never enter it. Keys hash op
  identity + canonical input + input-schema fingerprint + an invalidation-domain generation; any
  non-read dispatch starts a new generation, and the engine resets the cache at every turn start
  (external edits between turns can't be tracked, so the reuse window is repair rounds, retries,
  and nested plans within one turn). Hits are audit-distinguishable via a new `op_cache_hit`
  observation. Kill switch: `FLUX_OP_CACHE=off`.
- **L-53 — whole-flow dependency scheduler: independent read-only work runs in parallel.** The
  optimizer now summarizes every top-level statement across its whole subtree (nested blocks,
  `when` conditions, templates, call arguments) into symbol reads/writes and an effect class,
  then schedules whole-node read-only statements into dependency levels — a `when` block, a
  template bind, and a plain read that are mutually independent share one parallel stage, where
  the old scheduler only batched consecutive `bind`-of-`call` statements. Hard fences preserve
  the safety envelope: any write/network/process effect, any **unknown** op, or an
  approval/durability construct (`confirm`, `await`, `checkpoint`, `once`, `saga`, `thing`)
  pins its statement in program order and nothing is scheduled across it in either direction.
  An order floor keeps the emitted schedule in exact program order, so the optimized run is
  observationally equivalent to sequential execution — proven by a new trace-equivalence test
  (same bound values, same user-visible op event order) alongside six failing-first scheduler
  tests. Docs: the execution-model page now spells out what parallelizes and what fences.
- **L-59 — the CST is now the flow front-end, and analyzer diagnostics carry real source ranges.**
  New `flux_lang::lower_cst`: `cst_to_draft` (strict lowering — every lexer/parser error reported
  with its `TextRange`) and `parse_with_ranges`, which pairs the exact legacy `DraftAst` (the
  proven line machinery stays the semantic authority, so behavior and error texts are unchanged)
  with a node-path→range side-map keyed by the analyzer's own locator paths (`body[3].then[1]`,
  longest-prefix resolution). `parse`/`parse_program` are re-pointed through the front-end with a
  debug-mode acceptance-agreement gate, and flux-lsp now publishes analyzer findings (unknown
  ops, unbound `$vars`, arity) as warnings with resolved spans. Hardening the agreement guards
  (examples corpus + a CST assertion inside the 1000-seed round-trip property test) fixed nine
  tolerant-parser gaps: kebab-case flow names, `ctx` sub-lines, blank/comment lines at block
  boundaries, dotted op names, empty `+=` appends, full `thing` selector forms, scientific-
  notation numbers, single-quoted strings, and column-0 `goal` lines.
- **L-55 — plan-delta emission for cheap safe repairs.** A repair round can now patch just the
  invalid node(s) of the previous rejected plan instead of re-emitting it whole: a new
  `emit_plan_delta` tool (advertised only after a plan this turn was decoded and then rejected)
  takes a versioned `{version, base, ops[]}` patch — `base` pins it to the previous AST's content
  hash (a stale `base` is refused without touching the previous plan); `ops` replace/insert/delete
  nodes by the SAME path vocabulary analyzer diagnostics already use (`body[3].then[1]`, …, at any
  nesting depth). The materialized result runs through the identical model-ingress normalization,
  hidden-ops surfacing, and analyzer/lower gates a full `emit_plan` does before it can ever be
  accepted — an emission optimization, not a new execution path; the runtime still only ever sees
  a complete, analyzed plan. Audit (`PlanAttempted.delta_source`) records the raw delta alongside
  the materialized `plan_source` so either can be reconstructed.
- **L-56 — automatic context slicing for planner and model ops.** New `flux_lang::context_slice`:
  a pure, deterministic engine that derives the minimum model-visible context for one decision
  from HIR symbol reads, `jq` field-access-path narrowing, an op's declared param schema, and
  planner repair diagnostics, then gates the result (`Private`/`Hidden`/secret-derived/
  policy-denied symbols are never included unless explicitly referenced *and* permitted) and
  trims it to a token budget — exact when a host `TokenCounter` is supplied, a deterministic
  ~4-chars/token fallback otherwise — returning a full audit record of what was kept or dropped
  and why. Wired into two real default paths: `ctx`/`ctx_append` context packs
  (`flux_lang::runtime::build_ctx`, which feeds `ai.reason` and future `Ctx`-typed model-op params)
  now exclude `Private`/`Hidden` members by default and emit a `context.sliced` audit observation;
  a rejected `emit_plan`/`emit_plan_delta` now appends a sliced, budgeted "relevant session
  symbols" block to its repair feedback, scoped to what the rejected plan actually read plus what
  its diagnostics named, instead of relying on the model to re-derive relevance from the full
  session view.

### Changed

- **Docs truth pass across the release.** The flows docs cover `flow_list`/`flow_run` + the
  unified `~/.flux/flows` home; a new website datasources concept page explains how datasources
  relate to operations; `examples/advanced-code-review.flux` and the SDK doc fixtures use the
  native spellings instead of stale `@json` escapes; AGENTS.md documents the repo's binary
  inventory and the dual-changelog rule.
- **`task install` now also installs `flux-lsp`.** The install task puts both the `flux` CLI and
  the `flux-lsp` language server on `~/.cargo/bin`, and the install docs (README, the website
  tooling page, and the repo-local `.helix/languages.toml`) now show the `cargo install` route to
  the server for editor setup.

### Fixed

- **`flux eval` A/B harness accepts the CLI's provider prefix in the emission model spec** — a
  `FLUX_EMISSION_AB_MODEL=codex/gpt-5.5`-style spec no longer has to be spelled differently from
  the `-m` flag's form.
- **Review-hardening pass over the agent-speed epic (xhigh code review, 2026-07-09).** Op cache:
  `now`, `evidence`, and `metrics` are now declared non-idempotent (a cached clock froze time
  within a turn; cached metrics froze the agent's own progress signals), sub-agent `task`
  dispatches carry `Effect::Process` so they invalidate the parent's cache, cache hits are
  re-redacted against the CURRENT secret set, the invalidation generation now bumps BEFORE a
  write's IO (closing a concurrent-read staleness window), the hit path no longer holds the cache
  lock across the evidence lock, and `plan_turn` gets the same per-turn cache boundary as
  `run_turn`. Scheduler: `throttle`/`debounce` are hard fences again (their name-keyed durable
  buckets are invisible to the symbol hazard model), the binder-write collector is exhaustive (a
  future binder variant can no longer silently vanish from the write set), and node summaries are
  gathered in one visitor pass. Plan deltas: rejection feedback now carries the rejected plan's
  content hash — the `base` that `emit_plan_delta` requires (previously the schema promised it
  but nothing supplied it, so a model's first delta always failed) — the verdict bookkeeping is
  shared between full emissions and deltas, and the hash derivation reuses the one shared
  `sha256_hex`. Front-end: `parse`/`parse_program` are single-pass again (the CST + range map is
  built only by `parse_with_ranges` for callers that consume ranges, i.e. the LSP), the
  per-parse acceptance debug-asserts are gone (agreement is enforced by the dedicated test
  guards; the asserts aborted debug builds on untrusted model-emitted text), the LSP parses once
  per keystroke instead of three times, and analyzer warnings whose node path cannot be resolved
  say "(unlocated)" instead of silently piling at the document start.

## [0.11.6] - 2026-07-09

### Added

- **`flow_list` / `flow_run` tools — the agent can discover and run stored flows.** Reusable
  `.flux` definitions now live under `.flux/flows` (project) and `~/.flux/flows` (global, the new
  `@global_flows` root), with the legacy `.flux/ops` / `@global_ops` dirs still read during the
  ops→flows unification. `flow_list` enumerates every flow and composite op with its description
  and params; `flow_run(name, inputs?)` seeds `inputs` as literal binds and runs the flow in the
  current session through the same depth-guarded `run_plan` reentry — inheriting the provider,
  session, and approval/IO envelope. Composite-op loading also reads the flows dirs leniently, so
  an `op` dropped in `~/.flux/flows` auto-loads as a callable op regardless of file shape.

### Changed

- **The merged Node schema is now the planner's default emission arm (L-71 cutover).** The L-71
  A/B was run live — two runs on codex/gpt-5.5, 15 tasks × 3 arms each: pooled first-emission
  acceptance 28/30 for both `json` and `merged` (run 2: merged 15/15 with zero repair rounds,
  the fastest arm) at −26% uncached input tokens and −23% estimated cost. That clears the
  pre-registered parity rule, so `FLUX_EMISSION` unset now selects `merged`; `json` (the L-20
  winner) and `text` stay opt-in via `FLUX_EMISSION` as the measurement scaffold. The A/B
  harness now also takes `FLUX_EMISSION_AB_MODEL=codex/<model>` to run arms against the
  ChatGPT/Codex subscription provider. Tables + decision in
  `docs/designs/flux-lang-emission-ab.md`.

### Fixed

- **A bare `@name` now resolves to its named workspace root.** `Workspace::base_for` only mapped
  `@name/subpath`; a bare `@name` fell through to `<primary_root>/@name`, so directory reads of a
  named root (e.g. enumerating `@global_ops`) silently resolved to a non-existent path and
  returned nothing — which is why global composite-op loading from `~/.flux/ops` had quietly never
  loaded anything.

## [0.11.5] - 2026-07-09

### Added

- **Wired the `codewandler/flux-tree-sitter` grammar into editor support.** The new sibling repo
  ships a tree-sitter grammar + highlight queries for Helix, Neovim, and Zed. The repo-local
  `.helix/languages.toml` now declares the grammar (Helix colours via tree-sitter only — it does
  not render LSP semantic tokens), the website tooling page gained an Editor support section
  covering flux-lsp + the grammar + flux-editors, and the flux-lsp design/L-69 story are re-scoped
  accordingly (semantic tokens are for clients that render them, not a Helix path). README now
  surfaces editor support on the Flux-Lang pillar, and AGENTS.md classifies `flux-lsp` in the L6
  surfaces row and documents the hand-maintained highlighting-grammar mirrors (Prism,
  flux-tree-sitter, flux-editors) that syntax changes must propagate to.

### Fixed

- **`cut-release.sh` now creates an annotated tag.** The lightweight tag it previously created was
  not pushed by the `git push --follow-tags` command the script printed, so the tag-triggered
  Release + crates.io workflows silently never fired (hit on the 0.11.4 cut); the printed push
  command now names the tag explicitly and suggests an `ls-remote` verification.

## [0.11.4] - 2026-07-09

### Added

- **Flux-Lang lossless CST front-end (foundation).** New `flux_lang::{syntax, lexer, parser}`
  modules: a layout-aware lossless lexer and an error-tolerant parser producing a rowan concrete
  syntax tree. Every token and node carries a byte range, token text reproduces the source
  byte-for-byte, and parsing never aborts — errors become `ERROR` nodes with resync, so a broken
  buffer still yields a complete tree. This powers editor tooling; the classic parser remains the
  semantic front-end until the `cst_to_draft` unification story (L-59).
- **Native text syntax for all 16 formerly `@json`-only node kinds.** Durability and cross-turn
  state (`memo`, `once`, `checkpoint`, `await`), guard rails (`confirm`, `throttle`, `debounce`,
  `verify`), expression sugar (`peek $x`, `parse(…, as: "…")`), control flow (`try`/`catch`,
  `race`, `scope`/`finally`, `saga` with `step`/`undo`, `pipe`), and `thing` references. Every node
  kind now has a native spelling — round-trip- and property-tested — and `@json` remains only as
  the escape for shapes the grammar cannot express (non-identifier symbol names, non-invertible
  `expr` formulas, bracket-path `jq`, all-literal templates).
- **`flux-lsp` language server with Helix wiring.** New `crates/flux-lsp` (tower-lsp over stdio):
  positioned diagnostics from the tolerant CST parser, completion (ops, node-kind keywords,
  prelude types, in-scope `$vars`), hover (op signatures with effects/risk, node-kind and
  prelude-type docs), and whole-document formatting via the invertible formatter. A committed
  `.helix/languages.toml` wires it into `hx` config-only. Built from source for now
  (`cargo build -p flux-lsp`); shipping binaries is part of the packaging story (L-70).
- **Merged model-facing Node schema — a third emission arm (L-71).** `FLUX_EMISSION=merged`
  advertises `emit_plan` with the `Node` definition collapsed to ONE object schema (`kind` enum +
  the union of every kind's properties, each optional) instead of the 43-variant `oneOf` —
  29.9 kB → 10.2 kB (−66%, ~7.5k → ~2.6k tokens). Same wire format, prompt, parse path, and
  repair loop as the `json` arm; per-kind semantics stay in the node-kind catalog. New
  `flux_lang::schema::{merge_node_schema, model_schema}`, `fluxlang schema --merged`, and a
  three-arm emission-A/B live harness. `json` remains the default pending the measured A/B
  (decision rule pre-registered in `docs/designs/flux-lang-emission-ab.md`).

### Changed

- **schemars 0.8 → 1.2.1 in the root workspace.** Op parameter schemas and `param_signature()`
  strings now list required parameters in declared order (schemars 1.x behavior) instead of
  alphabetical order.
- **Language docs truth pass for the native syntax.** The website language pages, the Flux-Lang
  syntax spec (`crates/flux-lang/docs/syntax.md`), and `STATUS.md` now document the native
  spellings instead of describing the 16 nodes as `@json`-only — every rewritten snippet is
  parser-validated — and the spec gained previously missing sections for `parse`, `checkpoint`,
  `once`, `scope`/`finally`, and `saga`.

## [0.11.3] - 2026-07-09

### Fixed

- **Stopped rejecting valid `expr` predicates that use list built-ins.** The `expr` formula
  validator evaluated formulas against scalar dummy values, so a predicate applying a list built-in
  (`sum`, `any`, `all`, `has`, `join`, `first`, `last`) to a variable or dotted field — e.g.
  `filter(where: "has(it.labels, 'bug')")` or `map(expr: "sum(it.scores)")` — was wrongly reported
  "malformed" and rejected at both plan-analysis time and op runtime. Validation now checks grammar
  in a type-tolerant mode, so only genuinely malformed formulas and undeclared variables are flagged.
- **Restored the efficiency summary in `flux usage`.** The per-session efficiency line
  (turns, calls/turn, iterations/turn, plans/turn, cache-read %, uncached-in/turn, out/turn)
  reappears for the flux harness on the unbounded `all time` view.
- **Made `flux usage` metrics consistent under `--since`/`--until`/`--last`.** Call counts are now
  derived from the window-filtered records so they match the token and cost totals, and whole-session
  message counts are only attributed to sessions that fall entirely inside the window.
- **Fixed cross-harness double-counting in the usage summary.** The combined/summary totals now union
  active days and workspaces across harnesses instead of summing the per-harness counts, so a day or
  workspace active in two harnesses is counted once.

## [0.11.2] - 2026-07-09

### Fixed

- **Allowed intentional cargo-dist workflow overrides.** The release workflow now passes
  `--allow-dirty` to cargo-dist so the scoped `RELEASE_TOKEN` GitHub Release hardening does not
  make tag builds fail as an out-of-date generated workflow.

## [0.11.1] - 2026-07-09

### Added

- **Tracked the Flux-Lang agent-speed epic.** New epic design and L-53..L-56 stories cover the
  four KF items: whole-flow dependency scheduling, content-addressed op caching, plan-delta
  emission, and automatic context slicing.
- **Extracted the Flux-Lang expression evaluator.** `flux_lang::expr` is now a public module with
  the shared tokenizer, validator, evaluator, and typed `ExprVal` surface. Expressions now preserve
  objects, support lenient dotted access like `it.author.name`, and add list-aware built-ins:
  `sum`, `any`, `all`, `has`, `join`, `split`, `first`, `last`, plus list overloads for `min` and
  `max`. Note: object values no longer stringify inside `expr`, so `len(obj)` now returns key count
  instead of the compact-JSON character count.
- **Added deterministic core transform ops.** The cognition pack now exposes `map`, `flatten`,
  `skip`, `join`, and `split`; `filter` accepts `where` predicates evaluated by the shared
  `flux_lang::expr` engine; and `filter`/`sort`/`dedupe` `by` fields accept dotted paths.
- **Added aggregation and predicate ops.** `sum`, `count_by`, `group_by`, `any`, `all`, and `has`
  provide deterministic reductions and boolean emitters that can be used directly in
  `when`/`until`/`assert` call positions.
- **Added object and null-kit ops.** `pick`, `omit`, `merge_obj`, `coalesce`, `keys`, and `values`
  provide deterministic payload trimming, shallow object merge, fallback selection, and object
  introspection in pure Flux-Lang flows.
- **Added native expression syntax for conditions and bind RHS.** Flux-Lang text now accepts
  invertible formulas such as `when $count > 3`, `until len($queue) == 0`, and
  `$ok = $score >= 0.8`, lowering them to pure `expr` nodes with an automatically built `vars` map.
- **Completed the Flux-Lang data-transforms epic.** [L-46](docs/stories/L-46-expr-engine-module-and-list-builtins.md)
  extracted the shared expression engine, [L-47](docs/stories/L-47-core-transform-ops.md)
  shipped core list transforms, [L-48](docs/stories/L-48-aggregation-predicate-ops.md)
  added reductions and predicates, [L-49](docs/stories/L-49-object-null-kit-ops.md)
  added object/null helpers, [L-50](docs/stories/L-50-regex-ops.md) covered bounded regex,
  [L-51](docs/stories/L-51-native-expr-conditions.md) added native expression syntax, and
  [L-52](docs/stories/L-52-transforms-docs-examples-pass.md) closed the docs/examples pass with a
  hermetic `examples/data-transforms.flux` gate.

### Changed

- **`flux usage` now reports time-window metrics and unpriced reasons.** The dashboard can be
  narrowed with `--last`, `--since`, and `--until`; it renders period/session/wall-time/cache/cost
  metrics above each table, starts with a per-harness plus absolute total summary, explains `$?` rows
  as unknown-model or missing-usage cases, and shows TTY-only scan progress while reading large Codex,
  Claude Code, and opencode histories. The built-in pricing table now covers the current high-volume
  Claude/Codex/OpenRouter model ids seen in those histories, including Claude Fable/Sonnet 5,
  GPT-5.4, GPT-5.3-Codex, and route-suffixed OpenRouter slugs. JSON output now carries the same
  normalized period, summary, metric, row, and cost-status data without progress noise.
- **`filter` truthiness is aligned with Flux-Lang expressions.** In bare/`by` mode the strings
  `""`, `"false"`, and `"0"` are now falsey, matching `when`/`until`/`expr`; a list item previously
  kept solely because the inspected value was the literal string `"false"` or `"0"` will now be
  dropped.
- **Regex ops are included in the engine ops reference.** The previously shipped `regex_match` and
  `regex_extract` cognition ops now appear in the planner-facing `flux-flow` registered-ops
  reference alongside the other data-transform ops.
- **Malformed literal transform predicates fail during analysis.** Op parameters marked with
  JSON-Schema `format: "flux-expr"` are now validated before dispatch when their value is a literal
  string, so bad `filter.where`/`map.expr`/`any.where`/`all.where` formulas get ordinary compile
  diagnostics instead of a mid-run tool error.

### Fixed

- **Release publication now fails fast and verifies assets.** The tag-triggered binary Release workflow
  requires the repo-scoped `RELEASE_TOKEN` secret instead of falling back to a `GITHUB_TOKEN` path that
  returned `HTTP 403`, retries/idempotently refreshes release creation, and runs
  `scripts/verify-github-release.sh` to prove the tag has installer/checksum/platform assets.
- **Planner field-access ingress is consistently lenient.** JSON `emit_plan` ASTs are now
  normalized like native-text plans so a model-provided `jq.optional = false` cannot make agent
  field access strict; strict `$x.field` remains the native text authoring behavior.
- **Flux-Lang docs use the published Cargo package ids.** Active docs, sync tests, and website
  examples now use `codewandler-flux-lang` / `codewandler-flux-flow`; generated public tables no
  longer leak an internal ticket label, and the website overview reflects that planners may emit
  JSON AST or native text before normalization.

## [0.11.0] - 2026-07-09

### Changed

- **BREAKING — Flux-Lang field access is now strict, with an opt-out.** Native `$x.field` / `$x.0`
  raises a loud error on a missing object key, an out-of-range index, or a field access on a
  non-object, so a typo fails fast instead of silently binding empty. Mark an access **optional**
  with a trailing `?` (`$x.field?`, `$list.0?`) to read `null` when a field may legitimately be
  absent; a present-but-`null` field is never an error. Dotted numeric access now indexes lists
  (`$nums.0` is the first element). Strictness is a **reference-position** rule (bind value,
  condition, value template, `match`/`route` subject); field access *inside an `expr` computation*
  (`$x.field == y`, `filter`/`map` predicates) stays lenient. A model- or host-emitted `jq` keeps
  the lenient "absent means `null`" traversal, so agent turns are unaffected. Note: a session
  recorded before 0.11 that read a missing field may diverge on `flux replay`/`fork`/`diff` — an
  inherent consequence of the semantics change; divergence fails loudly (never silent).
- **`merge` tolerates an absent list.** A `null` or empty-string element of `lists` (what an optional
  `$x.field?` read of a missing list binds to) is treated as `[]` and contributes nothing, so a
  fan-out where some branches produced no list still merges the rest instead of hard-erroring. A
  genuine non-array element is still a type error.
- **Full expressions in every value position.** `return`, `each` sources, `route`/`match` subjects
  and `case` values, call arguments, and `{…}`/`[…]` template values now accept a full `expr`
  (`return $a + 1`, `op($x * 2)`, `[ $n - 1 ]`), not just a single leaf — consistent with `bind`.
  This also restores Flux-Lang round-trip totality (`parse(format(ast)) == ast` for every AST).
- **`flux auth status` regrouped by state.** Providers are split into *Available* / *Not
  configured* with a `N of M configured` summary, an active-default-provider marker, the resolved
  source (`ANTHROPIC_API_KEY (env)`, `flux store`, imported file), and a per-provider setup hint
  (`flux auth login …` / `set $VAR`).
- **Overhauled the public documentation.** Reworked the root README and every public website
  markdown page with clearer opening guidance, consistent metadata, normalized related-doc links,
  and tighter navigation across the agent, Flux-Lang, SDK, plugin, reference, and security sections.

### Added

- **Unified `flux usage` dashboard.** The command now renders an aligned, color-aware usage table
  and, by default, folds in detected local Codex, Claude Code, and opencode state alongside flux's
  own event store. Use `--no-external`, `--harness flux,codex,claude,opencode`, or `--json` to
  narrow or script the report.
- **Zero-arg ambient reads are pre-allowed by default.** `now`, `cwd`, `home_dir`, and `sys_info`
  (read-only, no IO, no permission subjects) no longer trip the approval gate, so a `now()` in a
  stored flow runs without a prompt — and no longer auto-denies on a non-TTY. A configured
  `[permissions].allow` still replaces the default entirely.
- **`flux plugin list`** is now accepted as an alias of the terse `flux plugin ls`.
- **`flux --allow-private-net` — an ephemeral private-network egress grant.** A global flag that, for
  the current invocation only, allows egress to private/internal/loopback addresses without editing
  `config.toml`. It widens only the *operator-grant* side of the scoped model: plugins still only reach
  the private hosts their manifest declares, every admission is still audited (with a distinct
  `cli:--allow-private-net` grant source), and nothing is persisted. `web_fetch` is opened for the run
  too — its guard has no manifest safeguard, so prefer a scoped `[private_net.plugins]` grant for
  recurring use.
- **GitLab plugin: CI/CD job-token, protected-tag, and deploy-token management.** 15 new operations —
  `gitlab.ci.job_token.scope.{show,set}`, `gitlab.ci.job_token.allowlist.{list,add,remove}` plus the
  `groups_allowlist.*` counterpart, `gitlab.repository.protected_tag.{list,show,protect,unprotect}`,
  and `gitlab.deploy_token.{list,create,revoke}`. The job-token allowlist is what lets one project's CI
  use its `CI_JOB_TOKEN` to clone/access another project. Destructive removes/unprotect/revoke and the
  credential-minting `deploy_token.create` carry High risk metadata; the destructive ops accept an
  optional `confirm_*` field that must echo the target.

### Fixed

- **`flux run -m mock` writes `flux-mock.txt` again.** The offline mock's canned plan emitted the
  `write` op with two positional arguments, which current plan validation rejects (multi-parameter
  ops take a single named object); the rejected plan was silently repaired into a prose `Finished.`
  with a zero exit, so the documented smoke test looked successful but did nothing. The fixture now
  emits `{path, content}` as one object, and a gate-level end-to-end test guards it. Also fixes
  `flux plan -o json -m mock`, which now emits the plan.
- **Getting-started / tour `hello` example prints a readable time.** `now()` returns an object, so
  `fmt("… {when}")` spliced the whole `{"unix":…,"utc":…}` blob; the example now binds
  `$utc = $when.utc` and interpolates `{utc}`.

## [0.10.1] - 2026-07-08

## [0.10.0] - 2026-07-08

### Added

- **`regex_match` and `regex_extract` cognition ops.** Two new pure operations bring
  deterministic, ReDoS-free regex matching and extraction into Flux-Lang flows. `regex_match`
  returns `"true"`/`"false"` for boolean predicates (drop-in for `when` conditions), and
  `regex_extract` returns the first match or all matches of a capture group (with `all: true`).
  Both use Rust's `regex` crate (Thompson NFA, linear-time guarantee) with pattern length and
  size limits, so malicious patterns cannot exhaust resources. Use for log classification, SemVer
  extraction, and other structured-text parsing where regex is the natural tool.

## [0.9.3] - 2026-07-08

### Changed

- **Packaged the `flux-sdk` + `flux-providers` closure for crates.io and automated the publish.** The
  20-crate publish closure is now published under a `codewandler-` vanity prefix (the bare `flux-*` names
  are squatted on crates.io), while every import path stays unprefixed (`use flux_sdk::…`,
  `use flux_providers::…`) via a package-name/lib-name split — no source or downstream-manifest change,
  and the shipped `flux` binary is unaffected. Crates that had joined the closure via later refactors
  (`flux-markdown`, `flux-orchestrate`, and `flux-pg` — pulled in by `flux-events`' optional `postgres`
  backend, which crates.io requires be published) gained the version metadata they were missing, and the
  non-closure `flux-datasource` dropped a stray version. A `vX.Y.Z` tag now publishes the closure via
  `.github/workflows/crates-io.yml` (`scripts/publish-crates-io.sh`, idempotent/resumable). See
  `crates/flux-sdk/PUBLISHING.md`.

### Added

- **`scripts/cut-release.sh`** — one command to cut a release: bump every version, re-lock both
  workspaces, roll the CHANGELOG, run the full gate, then commit + tag (staging only the release files,
  so concurrent work is never swept in).

## [0.9.2] - 2026-07-08

### Fixed

- **Clearer planner repair diagnostics.** When a plan is rejected for a missing required parameter,
  the feedback now names the parameter's expected type and the operation's full accepted-parameter
  shape (not just "add a key") — so the model can correct the call on the next attempt instead of
  re-emitting the same broken node.
- **Typed scalar binds everywhere.** Binding a scalar out of a field access (`$n = $obj.count`), an
  expression (`$ok = expr($a > $b)`), or a `parse` (`$n = parse($s, i64)`) now preserves its JSON
  number/boolean type instead of stringifying it — so a rebuilt object stays typed and
  numeric/boolean `match` arms fire. This closes the residual gaps left by the earlier
  typed-scalar-literal fix, making every pure-node bind consistent.
- **`flux plugin install --dir` prunes stale local registrations.** A local re-scan now removes
  descriptors for plugins whose binary is no longer in the scanned directory (e.g. after a partial
  build), so the CLI stops warning about a missing binary on every later command. Verified pack
  installs and plugins registered from elsewhere are never touched, and an empty scan prunes nothing.
- **Agent guidance for line-numbered reads.** The default system prompt now notes that `read`'s
  line-number prefixes are a citing/editing aid, not part of the file content — so an agent asked to
  return a line verbatim strips the prefix instead of echoing it.

## [0.9.1] - 2026-07-08

### Fixed

- **Plugin workspace CI.** Updated the Kubernetes plugin's typed operation helper for the new
  `OperationSpec::group` field so the full native plugin workspace clippy gate stays green.

## [0.9.0] - 2026-07-08

**Minor bump** (pre-1.0 SemVer, where the minor position is the breaking-change signal):
`OperationSpec` gained an optional public `group` field and `PluginManifest` gained a public
`groups` field. Additive for serialized manifests (serde defaults preserve legacy manifests), but
breaking for Rust callers that construct either struct by listing every field exhaustively.

### Added

- **Secret-management plugins (D-84..D-86).** Plugin manifests can now declare operation groups, and
  the plugin pack adds native Vault (`vault.admin` + `vault.kv`) and 1Password Connect
  (`onepassword.server` / `vaults` / `items` / `files`) plugins. Both plugins use host-managed HTTP
  and auth injection; datasource contributions are metadata-only and never include secret values or
  file bytes.

## [0.8.0] - 2026-07-08

**Minor bump** (pre-1.0 SemVer, where the minor position is the breaking-change signal): `AuthMethod`
gained a public `oauth2` field — a breaking change for code that constructs it by listing every field
exhaustively (see Changed); additive for manifests and for constructor / `..Default::default()` callers.

**Host-side OAuth for plugins** (the `plugin-oauth` epic, D-80..D-83) — a plugin declares its OAuth2
endpoints and stays a pure bearer consumer, while the host runs every token grant, refresh, and the
token store.

### Added

- **Plugin OAuth2 (the `plugin-oauth` epic, D-80..D-83).** A plugin can declare an OAuth2-backed auth
  purpose and stay a pure bearer consumer — the host runs every token grant and refresh, so the
  plugin never touches `/oauth/token`:
  - **D-80** — an `oauth2` block on a manifest `AuthMethod` (authorize/token paths, client_id,
    scopes, grants, loopback redirect); backward-compatible — legacy manifests round-trip unchanged.
  - **D-81** — the host resolves an OAuth2 purpose from the credential store (keyed
    `plugin:<name>:<purpose>`) and auto-refreshes a stale access token via the declared token endpoint
    (SSRF- + host-allow-list-guarded), handing the plugin only a fresh bearer; falls back to a
    declared env secret before login.
  - **D-82** — `flux auth login <plugin>` (and the `flux plugin login` alias) runs the browser PKCE
    flow (or `--password`) from the plugin's manifest and stores the tokens, so a later
    `flux plugin call` needs no env token.
  - **D-83** — a pluggable async `CredentialStore` (the 0600 file backend by default; a
    host-injectable Vault KV-v2 backend for deployment). Provider logins (`claude`/`codex`) keep
    working through the file backend.

### Changed

- **`AuthMethod` gained an optional `oauth2` field** (plugin-oauth D-80). Additive for manifests
  (serde default — legacy manifests round-trip unchanged) and for code that builds an `AuthMethod`
  via a constructor or with `..Default::default()`. It is, however, a breaking change for any code
  that constructs `AuthMethod { … }` by listing every field exhaustively: add `..Default::default()`
  or switch to a constructor (`AuthMethod::bearer`/`basic`/`oauth2`/…).

## [0.7.0] - 2026-07-08

**Minor bump** (pre-1.0 SemVer, where the minor position is the breaking-change signal): the
beta-hardening pass includes a breaking runtime behavior change — scalar text binds now preserve
number/bool type and integral numbers render as `5` not `5.0` (L-43 / F-008), so a flow relying on the
old stringified form changes output.

**v0.6.0 beta hardening** (the `beta-hardening` epic — triaged from the first external beta test of a
shipped release). Targeted fixes and docs alignment across the CLI, Flux-Lang, and the
served/program surfaces; no redesign.

### Added

- **`flux flow run --resume-value <json>`** (beta A-58 / F-015). A resumable flow halted on a
  top-level `await` (`$reply = await …`) can now be resumed with a payload: it is coerced to the
  await's type and bound to the awaited symbol before the fast-forward, so post-await statements run
  with it bound. Resuming a value-await with no `--resume-value` refuses with a clear error naming the
  symbol instead of failing later on `unbound symbol`.
- **`peek` is now bindable** (beta F-009). `$prev = peek(name)` reads a symbol softly (its value when
  bound, empty when not) and binds the result — previously `peek` was statement-only.
- **Context-pack shrinkage is surfaced** (beta A-63 / F-011). When a `ctx` pack drops members to fit
  its budget, the CLI prints `⊙ context: dropped N of M members` (once, from existing metadata) so a
  plain run can tell context was evicted.

### Fixed

- **Scalar text binds keep their JSON type** (beta L-43 / F-008). `$n = 1` / `$ok = false` bind the
  number/boolean, not the strings `"1"`/`"false"`, so `match` arms and structured output see the typed
  value; integral numbers also render as `5`, not `5.0`.
- **`parse` composes like other pure nodes** (beta L-44 / F-012) — accepted as an object-template leaf
  (`{ data: parse($x) }`) and as a direct return, not only in bind position.
- **`fluxlang compile` accepts a module with a leading `op`** (beta L-45 / F-013) — it now shares the
  module parse entry with `flux flow run`.
- **Direct `flow run` sub-agents correlate to the parent** (beta A-59 / F-016). A `task(...)` child
  under `flux flow run` records `correlation_id = parent session`, so `replay --sub-agents` recurses
  (parity with normal `flux run` turns).
- **`app run --serve -m mock` uses the mock provider** (beta A-60 / F-014) instead of silently taking
  the Anthropic path.
- **No panic on a broken pipe** (beta A-61 / F-006). Piping a streaming subcommand into `head`/`less`
  ends the process quietly (default SIGPIPE disposition restored) instead of a Rust panic + backtrace.
- **Accurate validation diagnostic headers** (beta A-62 / F-010). The "references unknown operations"
  header/refusal appears only when every diagnostic is genuinely an unknown-op error; other validation
  failures are headed "failed validation".
- **Weak-model repeat-read stall guard** (beta A-64 / F-004). A repeated, already-succeeded network
  read (`websearch.search`) is now counted by the loop's stall guards, which escalate then stop with
  an honest "could not make progress" instead of looping.

### Documentation

- **Beta docs-truth pass** (beta C-45 / C-46 — F-001/F-003/F-005/F-007). Reconciled the `--yes`
  destructive-op contract (it is a documented allow-all; the safety docs no longer claim destructive
  ops "always re-confirm" under `--yes`); reframed `-m mock` as a canned offline smoke test (writes
  `flux-mock.txt`, prints `Finished.`), not a representative agent run; aligned every A2A
  protocol-version mention to the card's `0.3.0`; fixed the Flux-Lang top-level-shape `TypeRef` JSON
  examples; and documented a planner **model capability floor** (weak models can fail the `emit_plan`
  contract).
- **Public docs coverage for shipped features.** Added seven pages to the public documentation
  site (`codewandler.github.io/flux`) that previously had no user-facing coverage: the Time Machine
  (`flux replay`/`fork`/`diff`, cassettes, resumable flows), storage & persistence (the append-only
  event log; the opt-in Postgres backend), using plugins (install/verify/pin/call the signed pack),
  an expanded SDK overview (the `Client`/`FlowClient`/`dsl` front doors + recipes), usage & cost
  (`flux usage`, `pricing.toml` overrides), experimental realtime voice, and a troubleshooting/FAQ
  page. All are wired into the sidebar and the "Start here" funnel; the README now links the docs
  site and adds time-travel to the thesis pitch. An `examples/` index README documents each runnable
  `.flux` file and how to run it.
- **Accuracy fixes.** Removed the incorrect claim that `--think`/`--effort` are wired into the plan
  engine (they only affect the raw `-p` path); replaced the flux-sdk README's dead docs.rs links
  with in-repo links (crates.io publication is still pending); refreshed the roadmap status header to
  0.6.0; genericized a downstream-consumer reference and an SSO profile name in the internal docs.

## [0.6.0] - 2026-07-08

**A2A conformance Tier 3 — the stateful task model** (the `a2a-conformance` epic, A-54..A-57,
implementing the A-53 design). An A2A task is now first-class and addressable: task id = the flux
session id, a `Task` is a projection over the session's own turn-lifecycle events (no second
store), and live runs are held in an in-process registry. Minor bump: one wire-behavior change
(the non-blocking spec default).

### Added

- **A-54** — addressable tasks. `message/send` honors `configuration.blocking`: the spec default
  (absent/`false`) returns a `submitted` task immediately and runs the turn in the background;
  `blocking: true` keeps the synchronous completed-Task fast path bit-for-bit. Server-side
  `tasks/get` resolves a live or retained task to its current state (realm-scoped;
  unknown/cross-realm/non-A2A ids are one constant `-32001 TaskNotFound`), on both the
  single-agent and multi-agent mounts. A finished task stays readable for as long as its session
  is retained — across restarts, since terminal state folds from the event log.
- **A-55** — `tasks/cancel`: fires a live run's `CancellationToken` out-of-band (the same token an
  SSE disconnect fires); the run stops between plan rounds and records the durable `cancelled`
  outcome. Terminal or not-in-process tasks → `-32002 TaskNotCancelable`. Blocking and streaming
  runs are cancelable too — every A2A run now registers while in flight.
- **A-56** — `tasks/resubscribe`: re-attach an SSE stream to a task — a live task replays its
  current state and follows the run's frames (deltas included) to the final frame; a finished
  task replays its terminal state and closes. Resubscribers are observers: disconnecting one
  cancels nothing.
- **A-57** — push notifications: `tasks/pushNotificationConfig/{set,get,list,delete}` register
  per-task webhooks; status transitions POST to the URL (config `token` echoed as
  `X-A2A-Notification-Token`; one attempt, 10s timeout, no retry — the durable task state is the
  source of truth). Only public `http(s)` endpoints are accepted (`-32003` otherwise;
  `FLUX_A2A_PUSH_ALLOW_LOCAL=1` for local development). `capabilities.pushNotifications` is now
  advertised `true`.
- `EventStore::prune_inactive_excluding(agent_id, cutoff, keep)` — the tag-scoped sweep with a
  keep-list, so the A2A TTL sweep can never prune a session whose task is queued or running
  in-process (the C-29 protection, generalized for the async era).

### Changed

- **A2A wire behavior:** a `message/send` that omits `blocking: true` now returns a `submitted`
  task instead of blocking to completion (the A2A spec default) — poll `tasks/get` or use
  `tasks/resubscribe`; flux's own client/CLI always sent `blocking: true` and is unaffected.

## [0.5.0] - 2026-07-08

**Postgres backend post-ship hardening** (the `pg-backend` epic's review follow-ups, D-76..D-79).
One breaking signature (`PostgresBackend::new`) → a minor bump.

### Added

- **D-77** — retention for ad-hoc (unregistered) event streams: new
  `EventStore::prune_adhoc_older_than(cutoff_ms)` deletes every stream that has no session-registry
  row and whose *newest* event predates the cutoff (per-stream horizon — a still-active ad-hoc
  stream keeps its full history), on both backends. Previously all three prune primitives
  enumerated only the registry, so ad-hoc fact streams grew without bound; `prune_older_than`'s
  docs now state its registry-only coverage explicitly.
- **D-78** — cross-namespace entity scan for the Postgres datasource backend:
  `PostgresBackend::scan(handle, ns_prefix, entity)` answers a global lookup over per-scope
  namespaces in **one** query instead of the 1+N serial round trips of `namespaces()` + a per-scope
  backend + `list()` each. Like `namespaces()`, it is an associated fn on the Postgres impl — the
  `DatasourceBackend` trait stays per-scope by design.
- **D-79** — `PgHandle::redacted_dsn()`: the one safe-to-print DSN form, rebuilt from the *parsed*
  components (userinfo → `…`; `password`-class query params masked — sqlx honors `password` as an
  authenticating query param, which naive split-at-`@` redaction prints verbatim; flux-owned params
  stay visible). `PgHandle`'s `Debug` shows exactly this form, and a DSN parse error no longer
  echoes the raw (credential-bearing) string.

### Changed

- **D-76** — Postgres bootstrap DDL is now safe under concurrent cold boots: every bootstrap path
  (the `schema` `CREATE SCHEMA` hook, the event-store schema, the datasource schema) runs in a
  transaction under one global advisory lock (`flux_pg::ddl_lock`), so concurrent first-boots
  serialize instead of flaking on Postgres's non-atomic `IF NOT EXISTS` (duplicate-key errors on
  the catalog indexes). `namespaces()` on a never-bootstrapped database now returns `Ok([])`
  instead of `undefined_table`.
- **D-76 (BREAKING)** — `PostgresBackend::new(handle, ns)` is now I/O-free and returns `Self` (not
  `Result<Self>`): it binds the namespace only. The shared-table DDL moved to the new
  `PostgresBackend::ensure_schema(handle)` — call it once from wherever a deployment opens its
  stores. The signature change is deliberate: adopters get a compile error pointing at
  `ensure_schema` instead of a silent runtime `undefined_table`.

## [0.4.3] - 2026-07-08

**A2A protocol conformance, Tier 2 — I/O fidelity within the synchronous-turn model** (the
`a2a-conformance` epic). Inbound `data` parts now drive a real turn, inbound `file` parts are refused
cleanly, and a returned `Task` carries conversation `history` and (for runners with structured output)
`artifacts`. Additive/non-breaking. Docs: `docs/a2a-conformance.md`.

### Added

- **A-51** — inbound multimodal parts, decided per part kind in one shared boundary
  (`flux_a2a::server::extract_input`, used by the reusable dispatcher and both HTTP handlers, so the
  decision cannot drift). A `data` part is **surfaced** into the turn input as structured JSON — a
  message whose only part is a `data` part now runs a real turn instead of an empty one — while a
  `file` part is **refused** with `-32005 ContentTypeNotSupported` (flux's turn is text-only, so a
  file is never silently dropped, even alongside text). `Part` gained first-class `as_data`/`as_file`
  accessors.
- **A-52** — outbound `Task` fidelity. A blocking `message/send` now returns `Task.history` from the
  conversation projection, capped to the new `configuration.historyLength` when the client sets it; a
  runner's structured (non-text) reply parts become `Task.artifacts` (the spec-faithful home — clients
  read artifacts first), and a reusable `artifact_update_value` frame shaper is added for streaming
  `TaskArtifactUpdateEvent`s. flux's built-in text agent produces no structured output, so its tasks
  carry history but empty artifacts.

### Changed

- A runner's structured reply parts (`A2aReply.extra_parts`) now surface as `Task.artifacts` rather
  than riding on `status.message` — a fidelity correction (the text answer stays in `status.message`;
  `Task.final_text()` is unaffected since data parts carry no text).

## [0.4.2] - 2026-07-07

**A2A protocol conformance, Tier 1 — an honest, spec-conformant discovery card and the A2A-specific
JSON-RPC error codes** (the `a2a-conformance` epic). Additive and non-breaking: the new card fields
serialize only when set, and the new error codes only replace today's generic codes on
already-failing paths. Docs: `docs/a2a-conformance.md`.

### Added

- **A-49** — A2A AgentCard conformance fields. The discovery card now emits the spec-required
  `protocolVersion` (single-source `flux_a2a::PROTOCOL_VERSION`), declares the JSON-RPC transport it
  actually serves (`preferredTransport: "JSONRPC"` plus one `interfaces` entry keyed to the card
  `url`, instead of an empty `interfaces`), advertises `supportsAuthenticatedExtendedCard: false`
  (honest — no extended-card method yet), and carries optional `provider` / `documentationUrl` /
  `iconUrl` when the served agent configures them. Additive: the new fields serialize only when set,
  so a card that configures none stays byte-stable.
- **A-50** — A2A-specific JSON-RPC error codes (the `-32001..-32007` binding set, as named constants
  in `flux_a2a::error`). A defined-but-unsupported A2A method (`tasks/cancel`, `tasks/resubscribe`,
  `tasks/pushNotificationConfig/*`, `agent/getAuthenticatedExtendedCard`) now returns
  `-32004 UnsupportedOperation` rather than a generic `-32601`, while a genuinely-unknown method name
  keeps `-32601`; an inbound message that carries parts but no usable text part returns
  `-32005 ContentTypeNotSupported` instead of silently running an empty turn. One shared classifier
  governs every dispatch site (the reusable dispatcher and both HTTP handlers) so the codes cannot
  drift between them.

## [0.4.1] - 2026-07-07

**Postgres storage backend (opt-in): a shared, durable, multi-writer-safe home for the two
persistence primitives server deployments run against — the unified event log and the datasource
record store.** Embedded SQLite stays the default and the default build is entirely DB-free; a
`postgres` cargo feature adds a Postgres backend behind each, so a multi-replica / load-balanced
deployment gets what an embedded file structurally cannot provide (cross-process/replica append
serialization, managed backups and failover). Additive — no public API change, and the SQLite path
stays byte-identical.

The `pg-backend` epic (D-71…D-75). Design: `docs/designs/pg-backend.md`.

### Added

- **D-71** — `flux-pg`, a new L1 crate that owns the sole `sqlx` dependency, the connection pool, the
  DSN contract (`pool_max` / `acquire_timeout_ms` / `schema` params — the last doubling as a
  search-path test-isolation knob), and a **panic-safe sync↔async bridge**: `PgHandle::block_on`
  runs a query future to completion from any calling context — a plain thread, a multi-thread tokio
  worker, or a current-thread runtime — by spawning onto the handle's own dedicated runtime and
  blocking on a `std::sync::mpsc` channel (the naive bridges each panic somewhere in that matrix).
  `PgHandle`'s `Drop` shuts the runtime down non-blockingly, so it is safe to release from inside
  another runtime.
- **D-72** — `flux-events::EventStore` restructured onto an internal backend seam (`enum Backend` +
  a private `EventBackend` trait): the ~20 SQL primitives delegate per-backend while every
  projection, wrapper, and serde decode stays shared over the `RawEvent` row tuple. Pure refactor —
  the public API is byte-identical and no consumer changed.
- **D-73** — a Postgres `EventStore` backend behind `flux-events`'s `postgres` feature
  (`EventStore::open_postgres(Arc<PgHandle>)`). `BIGSERIAL` + `INSERT … RETURNING` preserve the
  `s_<n>` session-id and turn-id contracts; `payload` stays `TEXT` for a byte-exact serde
  round-trip; appends serialize per-stream via a transaction-scoped
  `pg_advisory_xact_lock(hashtextextended(stream, 0))` — strictly stronger than the SQLite in-process
  lock because it also serializes appends across processes and replicas. A shared conformance suite
  runs the store's tests against both backends, plus a Postgres-only test proving N concurrent
  appends to one stream stay contiguous.
- **D-74** — a Postgres `DatasourceBackend` behind `flux-capabilities`'s `postgres` feature
  (`PostgresBackend`). Isolation is structural — a `namespace` bound once at construction is part of
  the primary key, the exact analog of one SQLite file per scope. Keyword search reaches
  FTS5/bm25 parity through a stored generated `tsvector` column + GIN index with `websearch_to_tsquery`
  + `ts_rank`; snippet / matched-field shaping is shared with the SQLite backend, so a `Match` is
  shape-identical across backends.
- **D-75** — `EventStore::prune_older_than(cutoff_ms)`, a whole-store retention primitive (deletes
  every stream whose `updated_at` predates the cutoff) on both backends — the tag-agnostic sibling
  of `prune_inactive`, for scheduled retention in long-running deployments.
- **CI** — a `postgres` job (`services: postgres:16`) runs the feature-gated crates against a live
  database; the default `check` job stays DB-free.

## [0.4.0] - 2026-07-07

**Multi-tenant A2A: per-request principal auth, per-principal isolation, and a resolver-keyed
multi-agent mount.** A flux server can now authenticate *callers* (not just the deployment):
per-request bearer→principal resolution via RFC 7662 introspection, sessions tagged with and scoped
to the caller's tenant realm, and one server serving N agents by path — each with flux's A2A session
machinery. Breaking: `flux_server::{serve, serve_on, router}` take `ServerAuth` and
`flux_a2a::server::dispatch` takes an authenticated realm.

Per-request auth + per-principal isolation (D-64 design → D-68 + D-69) + the multi-agent A2A mount
(D-63) + channel principal-auth parity (D-70), grounded against the A2A spec, the reference
downstream shape, and an adversarial security review (15 design-level fixes folded in before a line
of code). Design: `docs/designs/request-auth-seam.md`, `docs/designs/multi-agent-a2a-mount.md`.

A final pre-release code review (8-angle, high-effort) then hardened the implementation: realm keys
are `acct:`/`user:`-namespaced so an account value can't collide with a principal-derived realm
(cross-tenant break); realm-scoped `/usage` reuses the store's canonical pricing fold
(`cost_summary_for_account`) instead of a hand-rolled merge that could split model keys or drop a
priced total; shared-secret mode gained `external_url` so its public card isn't Host-phishable;
`serve_multi`/`serve_multi_on` refuse an unauthenticated non-loopback bind; the duplicate-header
rejection and the refresh-token guard were broadened; and secret-without-client-id / account-claim-
without-require-account are now hard errors / warnings.

### Added

- **D-68** — `flux-auth` gains the per-request bearer→principal seam its docs always deferred:
  `flux_auth::request` with `RequestAuthenticator::authenticate(bearer) -> AuthContext { account,
  caller, trust }` (object-safe; one claims→identity projection point — `AuthContext` carries the
  already-projected `(Caller, Trust)`, and the envelope stays the sole authorization source of
  truth), `AuthError { Unauthorized, Unavailable }` (payload log-only; byte-constant
  `WWW_AUTHENTICATE` challenge), and `bearer_from_header` (RFC 6750 parsing: case-insensitive
  scheme, b64token charset, 8 KiB cap — all before any hashing or network). Behind the new
  `introspect` feature: an RFC 7662 `Introspector` (optional `client_secret_basic`; redirects
  refused outright — a 307 would forward the token body; https required unless `allow_http`;
  256 KiB response cap; non-access tokens rejected so a leaked refresh token is not a bearer
  credential; account claim literal-key-first then dot-path; roles as JSON array or
  space-separated string with the reserved `account:` prefix stripped — the authenticator is that
  mirror group's sole writer; principal chain `sub`→`username`→`client_id` with a namespaced
  account fallback; trust clamped ≤ `Verified`) and a `CachedAuthenticator` decorator (SHA-256
  keys, segregated positive/negative stores so garbage floods cannot evict legitimate entries,
  `exp`-bounded saturating TTLs, `Unavailable` never cached). 42 new hermetic tests against a
  live mock endpoint.
- **D-69** — flux-server per-principal isolation over that seam, closing the A-48 single-realm
  caveat. Three explicit `ServerAuth` modes (open / shared-secret unchanged / principal); in
  principal mode every request resolves to a principal, sessions are tagged with the caller's
  realm (`account`, else `user:<principal>` — never a shared "no account" pool), one structural
  guard wraps every `/sessions/:id/*` route **including the write path** with 404s byte-identical
  to nonexistent ids (A2A §13.1), `/usage` is realm-scoped, A-48 `contextId` continuity is
  realm-keyed (`find_correlated_in_realm`; `contextId` is a grouping key, not a security
  boundary), and every turn — sub-agents included, via the shared `IdentityCell` threaded from
  `Executor` through the spawner — runs the safety envelope under the request principal's
  `(Caller, Trust)`, enforced by a gate-witnessed `enter_turn` (the realm is only obtainable from
  the function that swaps the identity). The agent card declares `securitySchemes`/`security`
  whenever auth is enabled, and in principal mode its `url` comes from the configured
  `[server] external_url`, never the Host header. CLI wiring: `[server] introspect_url` (+
  claim/client/allow-http knobs; the client secret arrives via a NAMED env var, never config).
  Auth failures are constant-shape (401 with the RFC 6750 challenge on every cause; 503 with
  backend detail logged server-side only; duplicate `Authorization` headers rejected).
- **D-63** — resolver-keyed **multi-agent A2A mount**: `flux_server::router_multi` serves N agents
  under `/:agent_id/.well-known/agent-card.json` + `/:agent_id/a2a`, each with flux's full A2A
  session machinery (TTL, `contextId` continuity, SSE) — the surface a multi-tenant host otherwise
  hand-rolls while forgoing that machinery. Agents are resolved per request by an `AgentResolver`
  (`StaticResolver` built in; dynamic per-tenant resolvers implement the trait and may key on the
  authenticated principal). Auth stays **one outer layer** — the resolver consumes the
  `AuthContext`, never verifies tokens (answering D-63's and D-64's shared open question); each
  agent's card advertises its own `/:agent_id/a2a`; unknown agent → constant 404; agents are
  isolated by construction (own engine + store). Design: `docs/designs/multi-agent-a2a-mount.md`.
- **D-70** — per-request principal auth **parity for a program's `a2a` channel** (`flux app run`):
  the channel adapter gains the same introspection knobs as `--serve` (client secret as a
  host-resolved `secret "ENV"`), routed through one shared construction point
  (`PrincipalAuth::from_introspection`) so the claim mapping never diverges between surfaces.

### Changed

- **BREAKING (D-69)** — `flux_server::{serve, serve_on, router}` take `ServerAuth` instead of
  `Option<String>` (`ServerAuth::from_token` is the drop-in mapping), and
  `flux_a2a::server::dispatch` gains a required authenticated-realm parameter
  (`dispatch(runner, realm, body)`) with `A2aTurnContext.realm` carrying it to the turn — the
  realm comes from the mount's own request authentication, never from message content. One-line
  changes at existing call sites.

## [0.3.3] - 2026-07-07

### Fixed

- **A-48** — A2A stateful mode: **one session per `contextId`**. Multi-turn agents (slot-filling
  presets) looped forever over the A2A text channel because both A2A surfaces were stateless per
  turn — each answer wiped the last. Now: (1) `flux-server`'s mount reuses the live session whose
  correlation id matches the request's `contextId` (the C-18 tagging already persisted the key;
  sweep-before-lookup means an expired conversation is never resumed; no-`contextId` requests keep
  per-task isolation), so the engine's conversation projection provides memory with **no client
  change** — the `flux a2a` chat client already sends one `contextId` per session; and (2) the
  reusable `flux_a2a::server` seam gains `A2aTurn::run_in_context(&A2aTurnContext, …)` (additive,
  default delegates to `run_rich`) so downstream A2A mounts can key their own continuity —
  previously the seam structurally could not carry a conversation id. Pinned by a memory-probe
  integration test (same `contextId` → same task id + the second answer proves it saw turn one;
  different/absent ids stay isolated), a stateful-runner dispatch test, and a store-level
  `find_correlated` test. Reported live by a downstream consumer.

## [0.3.2] - 2026-07-07

The **Time Machine** (epic, phases 0–3: C-43 → A-45 → A-46 → C-44): hermetic replay,
fork-at-any-decision, and run-diff of agent runs — the capstone of *the LLM is not the runtime*.
Because every accepted plan already persists as re-parseable Flux-Lang (`plan_source`) and the
runtime is deterministic, the one missing piece was durable op outputs; with the cassette closing
it, a flux run is a fully reproducible artifact. Design: `docs/designs/time-machine.md`.

### Added

- **C-43** — The op-output cassette: every leaf-op dispatch records a REDACTED
  `RunEvent::OpRecorded` cell (op, dual input hashes, content/view, error/denied flags) on the
  session's event stream — riding `EventKind::Run`, no new table, decodable by every existing log.
  Captured at the one dispatch chokepoint (`ExecutorHost`, self-wired from the store like the A-20
  read ledger; the outer agent-loop machinery is never cassetted), scrubbed through the same
  redactor as `plan_source` (C-22), per-op cap `FLUX_CASSETTE_MAX_BYTES` (1 MiB; an over-cap cell
  keeps a `truncated` head that replay refuses loudly), kill-switch `FLUX_CASSETTE=0`. On by
  default — measured cost: ~442 bytes/cell on representative ops, 0.01% of a heavily-used
  `events.db`. Also armed on the `flux flow run` path (which now persists its executed plan as an
  accepted `plan_source` attempt, so authored runs replay too).
- **A-45** — `flux replay <session|last>`: hermetically re-execute a recorded run — plans re-parse
  from `plan_source` (no model; the lazy provider is never constructed), op outputs served from
  the cassette (no live IO, side effects never re-fire), `confirm` gates auto-allow (nothing can
  execute from tape). The driver derives the execution list from the trace itself, reproducing the
  loop host's dispositions (A-05 identical-skip, halted prefixes, revision fast-forwards); the
  matcher is out-of-order-tolerant (absorbs nondeterministic `parallel` interleavings) and
  dual-hash (redaction-shifted inputs match); any miss is a loud `ReplayDiverged` + exit 1, never
  silent continuation. `--turn N`, `--sub-agents` (recurses the A-08 child streams via the new
  `EventStore::children_of`), `--json`. Live-verified: a recorded write-turn replays
  transcript-identical in ~400µs with the deleted artifact NOT recreated.
- **A-46** — `flux fork <session> --at N`: branch a recorded run at a top-level statement of its
  final plan. The prefix replays from tape into a NEW session (`correlation_id` = source, parent
  conversation copied); at the divergence point the cassette scope swaps Replay→Record — THE
  cassette-vs-live boundary — so the tail runs through the REAL approval envelope (pinned by a
  deny-approver test) and the forked run records its own cassette + `plan_source` rows, making
  forks first-class replayable/diffable sessions. Three modes: `--inject <json>` (a synthetic
  bind-plan, D-67 lit-canonicalization parity), `--edit <file>` (unchanged statements fast-forward
  via the content-hash ledger, edits run live), `--replan` (default: a live model turn from the
  forked state).
- **C-44** — `flux diff <A> <B>`: align two runs' executed statements (positional — the natural
  shape for run-vs-fork) and classify each row: the PLAN changed (`stmt_hash16` differs) vs the
  same plan hit a DIFFERENT WORLD (recorded op output differs) vs identical. Statement hashes are
  re-humanized through each session's stored `plan_source`; `--json`; exit 1 on divergence,
  diff-style. Pure L2 read-model (`flux_events::run_diff`/`stmt_rows`).

### Fixed

- **D-67** — `FlowStore::seed` literal-canonicalization parity: seeded values are stored via the
  new public `flux_lang::runtime::lit_value` (JSON string → the raw string, null → `""`, everything
  else → compact JSON text) — exactly how the interpreter's `Node::Lit` bind arm stores literals —
  so a seeded `$var` is indistinguishable from a literal-bound one everywhere downstream, including
  `map_args_to_input`'s lone-argument string-wrap (before, an `execute_with`-seeded object reached
  an op as the bare object while the equivalent `Bind{Lit}` delivered `{"<param>": "<json text>"}`;
  the structural path was also f64-lossy). Parity pinned by flux-sdk's
  `a_seeded_object_marshals_exactly_like_a_literal_bound_one`. Found adopting D-56 downstream
  (ai-agents C-14). Rider: `TranscriptAccumulator` + `UsageRecording` are re-exported at the
  `flux-flow` crate root alongside the other voice types.

## [0.3.1] - 2026-07-07

The hardening/docs/cleanup push: nine stories from the 2026-07-07 repo survey, closing the
release-gate rot, docs drift, and drift-guard holes the survey verified. No API breakage
(the one new surface, `App::with_events`, is additive) — a patch release.

### Fixed

- **C-39** — The live smoke gate works again: `scripts/smoke-live.sh` steps 1–5 invoked the
  retired flag-style CLI (`flux -p -m`, `--agent`, `--serve`) and died on clap parse errors before
  testing anything; rewritten onto the subcommand CLI (`flux run [-c] [--yes]`,
  `flux app run --serve`) via shared wrapper functions, plus a hermetic `--shapes` guard run in CI
  against the mock provider so CLI drift fails fast instead of rotting until the next release.
  Same pass: step 5's JSON-RPC payloads moved off the stale `tasks/send`/`tasks/sendSubscribe`
  method names onto `message/send`/`message/stream` (shapes pinned by the new C-41 tests), and
  step 6's ollama leg was moved off the same retired flags. Live-verified end to end
  (steps 1–4 + 7 + 8 green over openrouter-anthropic + subscription credentials).
- **C-40** — Docs truth pass after v0.3.0: roadmap header (0.2.15/"33 crates" → 0.3.0/34 crates/
  1900+ tests re-counted), the board's hand-written Status block (was still "v0.2.4"),
  `docs/architecture.md`'s false "merged flux-datasource" claim (it is a live standalone L0 crate)
  and its missing `flux-audio` rows (AGENTS.md's L0 table had the same gap), README's under-listed
  providers/L6 rows, the stale plugin-distribution "live next stories" epic heading (complete
  since 2026-07-05), and a dangling `examples/strict-review-app.flux` doc-comment reference.
- **L-41** — Every checked-in `examples/*.flux` now validates in CI
  (`crates/flux-eval/tests/examples_validate.rs`, directory-enumerating — a new example is guarded
  by default; JSON via serde, native text via the real parser, programs via the Program gate; the
  fullest registry buildable from flux-eval incl. the cognition pack). The sweep immediately
  caught real drift: `advanced-code-review.flux` was written in a fictional dialect and never
  parsed (rewritten in the real grammar), `improve-multi.flux` was missing `improve_log`'s
  required `record` param at all three call sites (fixed), and the unreferenced `loop-poc.flux`
  failed `lower` against the registry (deleted; superseded by the real `agent-loop.flux`).
- **D-65** — App-path redaction + audit parity: on `flux app run`, a cross-plugin-resolved
  credential is now seeded into the executor's redactor (closing a narrow breach of the
  "secrets never appear raw in model-visible output" invariant — the CLI agent path already had
  this via C-13), and the three dormant audit hooks now record on the run's stream:
  `PrivateNetAdmit` (D-20), `CrossPluginResolve` (D-27), `EndpointDiscovered` (D-30). One
  root-cause seam: additive `App::with_events(...)` hands the host's `EventStore` to the wiring
  sites; the redaction test was verified to catch the regression (fails with the sink removed).

### Added

- **C-41** — flux-server integration suite (`crates/flux-server/tests/`, 10 tests): `message/send`
  completes, `message/stream` SSE framing asserted structurally (working → completed, final flag),
  discovery-card/health auth-exemption vs 401 on guarded routes, malformed JSON-RPC handling, and
  C-18 TTL pruning driven through the real HTTP surface. One defect documented (garbage bodies get
  axum's generic 400, not a JSON-RPC `-32700` envelope) with an `#[ignore]`d failing-first test
  pinning the ideal shape for a follow-up.
- **L-42** — The website's language tables are now generated + drift-guarded like every other
  catalog consumer: `website/docs/language/{node-reference,types-and-effects}.md` carry
  `generated:node-kinds`/`generated:prelude-types` marker blocks synced by
  `crates/flux-lang/tests/website_in_sync.rs` (`UPDATE=1` to regenerate). The failing-first run
  caught real published drift (a wrong `peek` description and a hand-stripped `Blocked` link);
  new public `node_kind_rows()`/`prelude_type_rows()` accessors feed per-consumer rendering
  (Docusaurus needs `\|` cell escaping).

### Changed

- **C-42** — The schema-migration drift ledger moved from repo-root `DRIFT.md` to
  `docs/archive/drift-reports.md` (living ledger header added; all references repointed; repo
  root keeps only the canonical top-level files).
- **D-66** — Schema-SSoT tranche 1: 14 flux-tools ops (+1 cleanup) now parse their schemars
  struct via `parse_params` instead of ad-hoc `&Value` extraction (`edit`, `glob`, `append`,
  `read_many`, the seven `git_*` ops, `flux_reload`, `evidence`, `metrics`; `observe` was already
  converted), retiring their `#[allow(dead_code)]`. Derived schemas byte-identical; two
  strictness alignments now enforce what the schema always published (unknown keys and non-string
  `paths[]` elements hard-error), each pinned by a failing-first test; the seven git ops gained
  their first execute-level tests. Numeric-field ops (`read`/`grep`/`bash`/…) deliberately
  deferred to tranche 2 to preserve `u64_arg`'s stringly-number tolerance.
- **A-44** — flux-flow unwrap/expect triage: all 101 non-test sites audited and classified
  (94 mutex-lock poisoned-panic policy + 7 invariant-backed `expect`s) — zero fallible sites,
  zero code changes; the classification table lives in the story, and a crate-wide clippy
  unwrap ban was evaluated and rejected as pure noise (recorded with reasoning).

## [0.3.0] - 2026-07-07

The consumer-gaps release: a review of flux's first production SDK consumer surfaced the places it
had to re-implement or work around flux — this release closes them upstream.

> Re-cut of v0.2.24 with the correct version signal: the C-38 realtime-seam change is breaking, and
> pre-1.0 SemVer puts breaking changes in the minor position. Content is identical to v0.2.24,
> which remains published but superseded.

### Changed (BREAKING)

- **C-38** — Realtime usage/cost capture end-to-end: `RealtimeEvent::ResponseDone` gains
  `{ usage: Option<Usage> }` and `VoiceSink::response_done` now receives `Option<&Usage>` (default
  no-op retained) — a **breaking change on the realtime seam** for sinks that override it. The
  OpenAI Realtime codec parses `response.usage` tolerantly (malformed/missing → `None`, never a
  stream error); `Usage` gains `audio_input_tokens`/`audio_output_tokens` subset fields and
  `Rates`/`RateOverride` gain audio **surcharge** tiers with builtin `gpt-realtime`/`gpt-realtime-2`
  rows, so voice responses price correctly; the new optional
  `VoiceSessionDriver::with_usage_recording(UsageRecording)` appends one `EventKind::CallUsage` row
  per usage-bearing response — voice sessions reach cost parity with the text engine. Plus:
  `TranscriptAccumulator` (voice delta buffers → whole per-turn `Message`s, hangup-safe close
  flush), `RealtimeConfig::{with_voice, with_temperature}`, and a `default_model` re-export.
  Design: `docs/designs/realtime-usage-capture.md`.

### Added

- **D-55** — `EventKind::Custom { name, payload }`: one open variant in the otherwise-closed event
  enum so applications can append their own facts (audit trails, domain events) to the unified
  `events.db` with account scoping — flux never interprets the payload; all flux projections
  verified to skip it.
- **D-56** — `FlowClient::analyze_seeded` / `optimize_seeded`: analyze a stored flow with
  intended `execute_with` seed names counted as bound — no more flow-param declarations or
  hand-prepended Bind nodes for per-invocation values (completes the D-01 seam).
- **D-57** — flux-a2a schema derives behind an off-by-default `utoipa` feature (v5) on the full
  wire-type closure, so consumers derive OpenAPI docs from flux types instead of maintaining
  mirrors; shared `flux_a2a::server::card_url` helper (x-forwarded-proto aware), now also used by
  flux-server.
- **D-58** — `RiskApprover`: the middle ground between Allow and Deny — reads free, writes gated by
  declared `Risk` tier behind a consent marker in the permission subjects (default
  `"user-confirmed"`, threshold `High`), unknown tools pass through, plan gate fails closed.
- **D-59** — Closure-backed tools: `tool_fn(spec, handler)` / `FnTool` (handler errors fold to soft
  `ToolResult::error`) + a runtime object-schema builder in flux-spec
  (`req`/`opt`/`object_schema`/`empty_schema`) — dynamic tool families without bespoke structs.
- **D-60** — Provider/lang ergonomics: `NullProvider` + `StaticProvider` exported for
  deterministic/key-free flows and tests; Bedrock haiku alias profile overridable via
  `FLUX_BEDROCK_HAIKU_PROFILE`; `openai_from_env` accepts the `OPENAI_KEY` alias;
  `DraftAst::scoped(tools)` wraps a parsed flow in a capability scope.
- **D-61** — New `flux-audio` L0 crate: PCM16 LE/BE codecs, stateless resampling, a streaming
  `Resampler` that carries phase across packets, and a `Framer` re-chunker — the sample-math layer
  every realtime-voice consumer previously had to write itself.
- **D-62/D-63/D-64** — design-first stories filed: async paged live-backend datasource seam,
  multi-agent A2A mount, per-request bearer→principal auth seam.

## [0.2.23] - 2026-07-06

### Added

- **A-43** — OpenAI-wire plan skeletons: `map_chat_stream` now yields the same additive
  `Chunk::ToolInputDelta` the Messages codec has surfaced since L-23, one per `tool_calls[].
  function.arguments` fragment (name carried forward from the first delta of that call index),
  so `emit_plan` plan skeletons stream live on plain `openrouter`/`openai` too, not just
  Anthropic-family wires. Purely additive — the existing accumulation and A-32/A-33 malformed-args
  tolerance are unchanged. The codex path (re-enveloped through the Responses codec) is scoped
  out: `map_responses_stream` has no per-fragment tool-args accumulator at all today (only the
  fully-formed `response.output_item.done`), so wiring it would be new state, not an additive
  one-liner — left for a future story.

## [0.2.22] - 2026-07-06

### Added

- **L-25** — Pre-authored flow-run resumable mode: `flux flow run <file> --resumable` reports a
  structured halt on failure/await (✓/✗ statement tree + machine-readable failure + session id)
  and persists the same halt latch + statement ledger the loop uses; `--resume <session|last>`
  re-parses the corrected flow, folds the ledger, and fast-forwards the hash-matching completed
  prefix — the denial guard enforced on the authored path too (an unchanged denied statement
  never re-dispatches). `once` never re-fires across a fast-forward and `saga` recompensates
  consistently (both test-pinned); checkpoint deliberately coexists (cross-run caching) rather
  than being subsumed — documented in the language reference. Live-smoked end-to-end.
- **C-37** — Bedrock credential lifecycle: expiry re-resolution + lazy chain constructor.
- **L-23** — Streaming plan-emission render: plan skeletons appear while `emit_plan` streams.
  New additive `Chunk::ToolInputDelta` surfaces the Messages codec's already-accumulated
  `input_json_delta` fragments; a tolerant, resumable `PlanSkeletonScanner` (depth/string
  tracker, never a parser on the decode path) extracts completed top-level statements as
  headlines; a default-no-op `AgentSink::plan_delta` relays them and the CLI spinner shows
  `planning… · 2 read /app/server.py` live. Final tree render byte-identical; the A-40
  truncation/repair machinery proven unaffected. OpenAI-wire skeletons filed as **A-43**.
- **L-24** — Reified-await ledger fold: a top-level `await` inside a loop plan now opens the same
  halt latch + statement ledger as a failure halt (`FailureKind::Awaiting`, non-fatal, reified
  directly in `run_top_level_resumable`), so the model's post-await re-emission fast-forwards the
  hash-matching completed prefix instead of re-running it — effectful pre-await statements
  dispatch exactly once (pinned by `post_await_reemission_keeps_completed_prefix`). The engine's
  pre-authored await-suspension path (journeys/reply-parking) is untouched.
- **C-37** — Bedrock credential lifecycle: `BedrockCreds` carries the source-reported expiration
  (IRSA/Pod-Identity ISO stamps, SSO epoch-millis), `BedrockCredential::apply()` re-resolves
  through the stored resolver when creds are absent or within a 5-minute expiry window (the
  resolve-once C-09 seam went dark at the first STS rotation in a long-running process), the
  request region is pinned at construction and coerced onto every resolve so URL host and SigV4
  scope always agree, and `bedrock_with_chain(model_id)` builds the chain-backed provider
  **sync + lazily** (first request resolves — no `materialize_chain_into_env` snapshot needed).

## [0.2.21] - 2026-07-06

### Added

- **I-05** — Sharpened improve round (then put ON HOLD with I-01, user call): the scored eval set
  becomes fibonacci-server × 5 trials (chess-best-move excluded from scoring with recorded
  evidence — 28↔42% cross-round noise), the planner consumes candidates in weight order, and
  `bench/run-tbench-loop.sh` gains `FLUX_IMPROVE_EVAL_MODEL` for operator provider routing.
  Round 3 on this setup surfaced the next chain defect (planner prose → 0 tasks → null candidate
  leg) — two fixes are queued in the story for resumption. Full state: I-01/I-05 stories +
  `docs/self-improvement/STATUS.md` journey entry 6.
- **A-42** — Plan-mode gather rounds stream live: `compile_with_gather` takes an owned
  `Arc<Mutex<dyn AgentSink>>` and hands each round a fresh `SharedSink` (the loop host's
  ChannelSink/drain shape — the NLL wall A-18 hit never arises because nothing is reborrowed),
  so `flux plan` and REPL `/plan` show gather ops/results as they run, with per-round planning
  spinner + `loop.phase` labels (A-15 parity). Rendering only — same read-only gate, same shared
  budget, settled plan still never runs.

## [0.2.20] - 2026-07-06

### Added

- **A-18** — Multi-pass plan mode: `flux plan` and the REPL `/plan` toggle now run the same
  orient → bounded read-only gather → settle contract as normal mode (shared
  `GATHER_ROUND_BUDGET = 3`, drift-tested against `agent-loop.flux`'s `repeat 3`). Gather plans
  auto-execute (compile-enforced read-only via the A-13 gate — verified through the new seam with
  a real `Write` tool that is never dispatched), the settled final plan is shown and never run,
  and piped/`-o json|yaml` stays print-and-exit. docs/usage.md's "single-shot" caveat retired.
  Deferred (filed **A-42**): gather rounds render through a null sink for now — live streaming
  needs the loop host's ChannelSink shape.

### Fixed

- **I-01 (round infra)** — The self-improvement flows work again end-to-end: both improve flows
  predated the explicit `obj`/`list` value-template nodes and carried node-maps inside `lit` args
  (no longer implicitly resolved) — `task` calls crashed, and `improvements_aggregate`/
  `change_implement`/`score_compare` silently operated on unresolved maps (a whole round's review
  signal vanished as "0 candidates"). All call sites converted to obj templates;
  `improvements_aggregate` now names non-empty-but-unparseable input in its view instead of a
  silent zero (failing-first test). The funded round then ran the complete pipeline with real
  payloads for the first time and produced a correct strict revert (278 vs 278) — machinery
  proven; the trials≥3 headline gain remains open (full record in
  `docs/self-improvement/STATUS.md` journey entry 6).

- **I-04** — Terminal-bench containers now run flux with the `shell` group enabled: the tb custom
  agent (`flux_agent.py`) sets `FLUX_ENABLE_BASH=1` for the in-container run (disposable task
  sandbox; the envelope still gates every call), pinned by a flux-eval test. Live verify on
  fibonacci-server: checks 0% → 83% — the agent finally *starts* the server it writes (only a
  genuine behavior edge case remains). Every historical containerized number was equally depressed
  by this; decision recorded in the story: corrected harness carries forward, no I-03 re-baseline.

## [0.2.19] - 2026-07-06

### Added

- **C-33** — Cost-surface follow-ups: app-run/journey/agent-target turns now surface cost — the
  app's `RecordingSink` captures `turn_end` usage, `JourneyRun` carries `usage` + the canonical
  model spec, and the `flux app run` operator console (stdin channel loop) prints a dim stderr
  cost line per completed run under the same contract as CLI turn ends (priced / `$? (unpriced)`
  metered-cloud miss / silent local). The TUI stops silently under-reporting on pricing-table
  misses: its cumulative header switches to `$X.XXXX+? (unpriced)` (or bare `$?`) once any turn is
  unpriced, sharing the new `flux_core::is_metered_cloud_spec` predicate with the CLI's marker.
  GoalSink verified already-correct (spec re-derived per goal iteration; `/model` unreachable
  mid-goal) — no change needed.

### Fixed

- **A-40** — Oversized plan emission dies at max_tokens — split, don't retry the whole plan: a
  `max_tokens`-truncated `emit_plan` is now its own in-loop repair class in `compile_turn` instead
  of a turn-killing error. The repair instructs a *split* (smaller plan now, omit `complete`, one
  large file write per plan; the text arm additionally cites L-39's `"""` verbatim spelling),
  installs shape-safely (no empty assistant message, no user-after-user — empty-preamble repairs
  append to the trailing user message), and is bounded by `TRUNCATION_REPAIRS` (2) before a legible
  failure naming the ceiling. Live re-run (fibonacci-server × 3): the I-03 signature is gone —
  0 truncation deaths, sampled trial 4 steps/$0.35 vs 31 steps/$0.76 never completing. Validation
  also uncovered that tb containers never enable the `shell` group (depresses every historical
  tbench number equally) → filed **I-04**.
- **C-35** — Prompt caching on the openrouter-anthropic wire: `OpenRouterProfile::quirks_for` now
  keys on the model and enables `cache_control` for `anthropic/…` slugs (vendor-prefix match; all
  other gateway upstreams stay conservative — the original A-03 rationale for `false` still holds
  for them). Live-verified: a prefix-sharing second call reads 27.2k cached (99% hit) at $0.0093
  vs $0.1033 uncached — the I-03-measured +35% gather-round overhead on openrouter is gone;
  `flux usage` prices the cached reads (C-30 intact).

## [0.2.18] - 2026-07-06

### Added

- **I-03** — Measure the multi-pass cutover: the epic's acceptance gate ran on funded keys
  (baseline `b528772` vs post v0.2.17, `openrouter-anthropic/anthropic/claude-sonnet-4.6`; raw
  recordings + both legs' reports under `bench/*/results/i03-go/`). Time-to-first-feedback: post
  wins all 5 corpus prompts, −0.4s (trivial chat) to −5.1s (explore-complex), 30/30 trials clean,
  first planning indicator ~71ms vs baseline's silent planning. Rounds/tokens: no tiny-plan
  dribble, no call inflation on trivial turns, revise 0.0/turn; honest regression — gather rounds
  re-pay the ~20k prefix uncached on the caching-disabled openrouter wire (corpus spend +35%,
  filed C-35). Terminal-bench (2 tasks × 3 trials): pass-all ties 0/6 vs 0/6, but baseline keeps
  14% partial checks where post keeps 0% and pays ~4× to fail — execute-phase plans truncate at
  the 16384 emission ceiling and retry whole (filed A-40). Full tables + verdicts in
  `docs/designs/multipass-agent-loop.md` "I-03 measurement results"; the multipass-agent-loop
  epic's cutover is now judged on evidence. New backlog stories: **C-35** (prompt caching on the
  openrouter-anthropic wire) and **A-40** (split oversized plan emission instead of whole-plan
  retries).
- **SDK** — `FlowClientBuilder::approver`: inject a custom per-op `Approver` policy into
  declarative flow runs — the seam between blanket `auto_approve` and the headless default deny
  (e.g. a risk-aware confirm gate), mirroring `LocalSpawner::with_approver` on the sub-agent path
  so both paths take the same policy.

### Fixed

- **A-41** — Role `model:` override goes through provider-spec parsing, not verbatim to the wire:
  a sub-agent role's `model:` frontmatter now resolves via the new `flux_core::resolve_role_model`
  at the spawn call site (`flux-orchestrate`). A prefix naming the parent's own provider is
  stripped to the provider-local slug; a prefix naming a *different* known provider fails fast at
  spawn with a diagnostic naming both providers and the inherit-parent-provider constraint —
  instead of an opaque HTTP 400 mid-turn. Bare slugs and unknown leading segments (openrouter's
  `vendor/model` ids) pass through unchanged; `openrouter` vs `openrouter-anthropic` never
  prefix-match each other. Docs: accepted forms in `docs/usage.md` + AGENTS.md's sub-agent-role
  bullet. (Hit live 2026-07-06 running `examples/god-review.flux`.)
- **D-54** — Guest SDK `serve()` must not silently skip malformed host frames: the plugin-side
  protocol loop (`flux_plugin::serve`) now writes a one-line stderr diagnostic per unparseable
  frame (byte length + parse error only — never frame content) and exits after
  `MAX_CONSECUTIVE_MALFORMED_FRAMES` (5) consecutive failures, so the host surfaces its existing
  "plugin closed the connection" error instead of hanging on a response that will never come. A
  parsed frame resets the counter. `serve()` now delegates to a testable `serve_io` seam; its
  public signature is unchanged. (The one validated kernel of god-review finding #4 — the host
  side was already hardened.)

### Changed

- **C-36** — Error/Result convention adherence: AGENTS.md's Errors bullet now codifies the
  wire-seam exception (plugin frame `err`, A2A JSON-RPC error objects, host-capability callback
  results legitimately carry `String` errors); every other bare `Result<_, String>` in `crates/`
  was either converted to the convention (`flux-capabilities` `EndpointRegistry`, the `fluxlang`
  CLI bin, `flux-cli` strays → `anyhow`, `flux-tools` `sqlite_query` closure) or classified
  wire-seam in the story's Progress table. Also: `flux-a2a` re-exports `new_id` on its own
  `pub use` line instead of hiding a function inside the type re-export block. Pure refactor, no
  behavior change.

## [0.2.17] - 2026-07-05

### Added

- **Enforced pin/rollback — spawn-time hash verification over the versioned store (D-48).**
  `flux plugin pin`/`rollback` stop being advisory labels and become supply-chain statements
  (trust-ladder step 5, `docs/designs/plugin-distribution.md`). `pin <name> <version>` is a
  verified version switch: an already-stored version repoints **offline** (no download), anything
  else rides the same signed-index + checksum path as `install`; the descriptor records the
  binary's `sha256` + `version` and remembers the replaced version in the new `previous` field.
  `rollback <name>` flips to `previous` — offline and instant by construction (`pack::rollback`
  takes no fetcher), with current/previous swapping so a second rollback returns; its old
  clear-the-advisory-pin semantics (and `set_pinned`) are gone, a clean cutover. Enforcement:
  every descriptor-based spawn (`agent-startup discovery`, `flux plugin call`, `status`) goes
  through the new `PluginHost::spawn_verified`, which re-hashes a `sha256`-carrying descriptor's
  binary (sub-millisecond) and **refuses drift** naming plugin + expected + actual hash — an
  unreadable binary under a recorded hash is drift too, never a silent pass; hashless dev
  descriptors spawn as before, labeled `unverified (local)`. The offline paths are anchored by a
  hash **sidecar** written beside each stored binary at verified-unpack time — a store entry
  without one (pre-D-48) is a clean refusal rather than a re-hash that would bless tampered bytes.
  `flux plugin status` (and `ls`, which previously trusted the descriptor field and could say
  `verified` over tampered bytes) now show re-hashed verification — `verified` / `hash drift` /
  `unverified (local)` — plus a loud-but-nonfatal manifest-vs-descriptor version-agreement
  warning; `uninstall --purge` also removes the plugin's versioned store (traversal-guarded,
  and it cleans orphaned store dirs).
- **Multi-line string literals in Flux-Lang — `"""…"""` (L-39).** The fine-tune's dominant failure
  mode (and a human-authoring pain) was representational: a multi-KB edit payload had to be ONE
  escaped single-line JSON string, and models (the 3B fine-tune always, Sonnet sometimes) break such
  strings with literal newlines. The text surface now accepts a triple-quoted spelling anywhere a
  JSON string literal can appear — bind values, call args, string leaves at any depth inside
  pure-JSON `Lit` objects/arrays, value-template leaves, `fmt` templates, `assert` messages, `ctx`
  purposes, `route` case labels — implemented as a lexer-level desugar in `parse::preprocess`
  (content is **fully verbatim** to the next literal `"""`: no escapes, no comment stripping, no
  dedent), so every downstream parser is unchanged and the Lit-vs-template disjointness rule is
  untouched. `format::format` emits the spelling for any string containing a newline unless one of
  three narrow safety guards forces the escaped fallback — embedded `"""`, trailing `"`, or an
  embedded `\r` (the `\r` guard closes a real `parse(format(A)) != A` violation against
  `preprocess`'s CRLF normalization, caught in cross-session review of the in-progress diff and
  fixed failing-first). `format_compact` deliberately never emits it (the single-line display
  variant stays single-line). The planner text grammar teaches the spelling with a fourth worked
  example; the L-18 roundtrip property suite (1000 seeds × 43 node kinds) now exercises it incl.
  the unsafe-shape fallbacks; the grammar is specified in `crates/flux-lang/docs/syntax.md`
  § "Multi-line strings"; and a redaction test proves a secret scrubbed mid-block leaves the text
  parseable (the L-38/C-22 invariant).
- **`flux corpus export` — mine NL→Flux-Lang corpus rows from events.db (D-53).** The L-38 hedge
  cashed out: every accepted plan since v0.2.15 records a parseable `plan_source`, and the exporter
  pairs each one with the user turn that produced it, emitting corpus-shaped JSONL
  (`{id, nl_goal, source, provenance{session, turn}, flux_rev}`) compatible with flux-model's
  validation ladder — zero LLM cost per row, compounding with real flux usage. A new pure
  `flux_events::corpus_rows` projection does the pairing exactly (a `PlanAttempted` is recorded
  scoped to its turn's `TurnStarted.global_seq`, so no conversation-walk heuristic),
  `EventStore::corpus_rows_all()` folds it over every stream (a sub-agent child's accepted plans
  are independent training examples, deliberately not the cost rollups' double-count-guarded
  aggregation), and skip counters (`no_plan_source`, `ambiguous_pairing`, `unparseable_at_head`)
  go to stderr while rows go to stdout or `--out <file>`. `plan_source` is already redacted at
  record time; `nl_goal` (raw user input, which record time never redacts) gets an export-time
  credential-shaped-token scrub (`AKIA…`/`sk-…`/`ghp_…`/JWT patterns) independent of any
  per-session secret registry. `flux_rev` is the exporting binary's crate version — the anchor
  honestly available wherever an installed binary runs.
- **D-49** — Plugin naming + docs truth pass — the crate / the pack / the CLI: the canonical trio
  vocabulary (the plugin protocol crate `flux-plugin` / a pack binary `flux-plugin-<name>` / the
  plugin CLI `flux plugin …`) is now documented once in `plugins/README.md` (rule of thumb:
  hyphen-no-suffix = crate, hyphen-with-name = pack binary, space = CLI) and applied across
  `README.md`, `docs/usage.md`, `docs/architecture.md`, and the `flux plugin` help strings.
  `README.md` Install gained an "Install plugins" subsection (verified remote one-liner + source
  fallback); `plugins/AUTHORING.md` gained "Releasing: where your binary ends up" (the
  `plugins-v*` pack channel + the never-hand-push-a-`plugins-v*`-tag warning); pin/rollback docs
  updated from advisory to D-48's enforced semantics. Docs fix along the way: the source-fallback
  examples now use `(cd plugins && cargo build --release) && flux plugin install --dir` — the
  chained `cd` form would have scanned the wrong directory, since bare `--dir`'s default resolves
  against the cwd. New help test `plugin_help_documents_install_modes_and_pin_rollback` asserts
  the CLI help stays truthful. Docs + help strings only; no behavioral code paths touched.

## [0.2.16] - 2026-07-04

### Added

- **Breadth guard — freshness-independent convergence pressure on read-only turns (A-29).** The
  fourth (and general) member of the read-loop guard family. The shipped guards all detect
  *redundancy* — identical transcripts, repeated failure keys, renamed re-reads (A-20), slid
  windows over covered lines (A-28) — so a model on a novelty treadmill (s_356: 22 rounds, a NEW
  grep pattern or a fresh window over new lines every round) looked legitimately fresh to every
  one of them and the loop's only exit was the model choosing prose. `guard_breadth` now counts
  **consecutive read-only rounds regardless of freshness**: at 6 it injects the "ANSWER NOW from
  the session symbols, or name precisely which fact is missing" directive carrying the full
  evidence inventory (rounds, distinct resources bound, A-28 coverage spans); at 10 it ends the
  turn honestly via the existing force-stop seam. Any effectful dispatch resets the counter
  (read→fix iteration is never punished), a no-read round leaves it unchanged (a no-op round
  can't launder the count), and a redundancy stop armed in the same round suppresses the breadth
  banner. Thresholds are named constants (`READONLY_ROUNDS_ESCALATE`/`READONLY_ROUNDS_STOP`,
  ordering pinned at compile time) overridable for legitimately read-heavy workflows via the new
  `[limits] readonly_rounds_escalate` / `readonly_rounds_stop` config keys (0 disables a rung,
  project-over-user scalar merge). Recorded decision: the per-turn token budget **stays
  default-OFF** — the pathological read case is now bounded in rounds, which is model- and
  pricing-independent. New "Convergence guards" section in `docs/agent-loop.md` documents the
  whole ladder family.

### Fixed

- **Docs accuracy sweep — every claim re-verified against the code.** A six-way audit of the
  committed docs (canon, operational, language, designs, plugins, self-improvement) surfaced ~80
  verified inaccuracies; all fixed. Highlights: `docs/usage.md` no longer claims destructive ops
  "always re-confirm / can't be bypassed" (false under `--yes`, which installs an allow-all
  approver, and for steps already disclosed in an approved plan); `plugins/README.md` +
  `AUTHORING.md` no longer teach the bare `flux plugin install` scan (now an error) or the deleted
  `endpoint` URL-handback capability — they document the verified remote install + trust ladder,
  the full capability set (`conn.authenticate`/`credential`/`fs.read`/`discover`/`config`), and
  the `*_ref` reference-addressed host methods; `docs/roadmap.md` caught up from v0.2.4 to
  v0.2.15 reality (eight shipped epics marked shipped, new plugin-distribution section with
  D-48/D-49 as next, node-kind counts corrected to 43); `docs/model.md` records Bedrock streaming
  as the wire, marks `--think`/`--effort` as unwired no-ops, and routes GLM guidance at
  `openrouter-anthropic/z-ai/glm-4.6`; `docs/a2a.md` documents the C-18 session TTL sweep and
  rich `data` reply parts; the self-improvement docs demote the saturated synthetic suite to
  regression floor and fix `flow.db`→`events.db`; the flux-lang docs fix the `peek` contract
  (empty string, not null — corrected at the `ast.rs` source and regenerated into the guarded
  blocks), the retry `delay` default (500 ms), the missing `each.flat` row, and gain a
  module-declarations section covering `secret "ENV"`; `docs/architecture.md`/`AGENTS.md` crate
  tables now cover all 33 crates and the envelope chain includes the capability-scope floor;
  design docs got truthful Status lines (stream-resilience and openrouter-reported-cost →
  implemented v0.2.15; plugin-distribution → core shipped v0.2.14) and two fully-realized designs
  (`plugin-skill-generation.md`, `session-s251-postmortem.md`) moved to `docs/archive/designs/`
  with every inbound link repointed.

## [0.2.15] - 2026-07-04

### Added

- **`--trace-loop` — flag-gated structural trace of the outer agent loop (A-39).** Under
  `--trace-loop` (or `FLUX_TRACE_LOOP=1`), the CLI prints one dim line per outer-loop round
  (`⟳ round 3/25`) and per structural AST node the agent-loop program executes — op calls with
  their bind names (`· plan → $plan`), `when`/`unless`/`match` branches taken, `parallel` branch
  entry, `return`, and until-exit — closing the gap where the loop's position was invisible (the
  only live signal was the `loop.phase` spinner label, and the transcript's `[N/25]` counts inner
  tool calls against the outer `repeat 25` cap). Observation-based and live-only: the interpreter
  emits `loop.round`/`loop.node` observations through the existing sink path, gated once at
  emission by a default-false `FlowSink::trace_structural()` method — never `executor.observe`,
  so nothing lands in the evidence log or events.db. Outer-loop scoping is by sink instance
  (`SinkBridge.trace`, set only by the engine's new `execute_flow_traced` call under the flag);
  inner `run_plan`, `flow run`, and resume paths stay untraced and are one boolean away from
  opting in later. Zero default-output change (pinned by a negative test); pure computation nodes
  (fmt/jq/expr/parse/lit and pure binds) are never traced. Documented in `docs/agent-loop.md`
  beside `--show-loop`.

- **Multi-perspective example — parallel 3-lens scout fan-out to a cited Answer (L-37).** Second
  checked-in, test-guarded native-text showcase after `examples/strict_review.flux`:
  `examples/multi-perspective.flux` runs one query through three sub-agent lenses in `parallel`
  (tech / product / risk scouts resolved from the new `.flux/agents/{tech,product,risk}-scout.md`
  role files via the `task` op), extracts each scout's `.evidence`, `merge`s the claim lists, and
  `synth`esizes a cited prelude `Answer` — fan-out orchestration, role-file sub-agents, and the
  cognition ops composing in the language, not in host code. Hermetic
  `crates/flux-sdk/tests/multi_perspective.rs` (no API key): one mock provider serves both the
  top-level provider (`synth` is CognitionPack provider-injected) and the sub-agent factory,
  disambiguated on the request's system text, and pins exactly-once spawning per scout, the
  end-to-end evidence flow into the synth prompt, an Answer-shaped return, and cross-run
  determinism. Grammar fact that fell out of implementation: flow names DO allow `-`
  (`is_name_char`), so the header keeps the literal `flow multi-perspective(query: String) -> Answer`;
  `.gitignore` gained a `!.flux/agents/*-scout.md` negation so the role files actually check in.

- **Structural enforcement for the stream-resilience invariant (A-37 — closes the stream-resilience
  epic).** "Provider bytes never error a chunk stream" (A-33/A-34/A-35/A-36) is now self-enforcing,
  not just a convention. A crate-local `crates/flux-providers/clippy.toml` bans
  `serde_json::from_str/from_slice/from_value/from_reader` via `disallowed-methods`; **verified to
  fire under `cargo clippy --workspace --all-targets -- -D warnings`** (a scratch call added
  without an allow failed the gate; removing it restored green) — the speculative per-crate
  resolution the design flagged as the one open risk does hold under `--workspace`. The dozen
  legitimate remaining call sites (the tolerant skip+count SSE/frame parses in `openai.rs`,
  `messages/mod.rs`, and `bedrock.rs`'s `frame_to_sse`; bedrock's out-of-scope SSO/STS/EKS
  credential-resolution parses; codex's WS terminal-event/kind sniffs and its test fixtures) each
  carry a targeted `#[allow(clippy::disallowed_methods)]` at the function or module scope, with a
  reason pointing at why. New `crates/flux-providers/src/envelope_corpus.rs`
  (`#[cfg(test)] mod envelope_corpus`) systematically corrupts one valid fixture turn per codec —
  truncation at every byte offset, junk-frame injection at every frame boundary, single-frame
  corruption — and asserts no `Err` chunk ever escapes the three SSE codecs (chat/responses/
  messages), and that any bedrock `Err` classifies `Error::StreamDecode`, never `Error::Provider`.
  Confirmed the corpus is a real regression guard, not just green-by-construction: reverting the
  messages codec's tolerant match back to a bare `?` turned 2 of its 3 corpus tests red (junk
  injection and single-frame corruption) before the fix was restored. `map_chat_stream`/
  `map_responses_stream` are now `pub(crate)` so the corpus can drive them directly. AGENTS.md's
  safety-invariants list gains a matching bullet linking
  [docs/designs/stream-resilience.md](docs/designs/stream-resilience.md).

- **`FLUX_PLANNER_TRACE=1` env-gated planner trace (A-38 — the stream-resilience epic's parse-
  resilience residual).** The A-33 backstop made planner failures *quieter* (retries instead of
  crashes), which made the next s_360/s_368-class forensic *harder*: a session can now silently
  retry a few times and still land a plan, burning steps and tokens with nothing on stderr to show
  why. Setting `FLUX_PLANNER_TRACE=1` writes one greppable line per planner step to stderr — step
  index, stop reason, tool names called, reject/decode text (A-31's `last_reject`, only when
  *this* step set it — never a stale echo of an earlier step's rejection), and any dropped-frame
  diagnostic (A-33's `Chunk::StreamDiagnostic`) — covering every way a step can end (decode-error
  retry, hidden-op/gather/validate rejection, the text-fallback plan/chat returns, a max_tokens
  truncation, and plan acceptance). Test-observable per the C-19 `fallback_note_sink` precedent:
  since the planner loop (`crates/flux-flow/src/compile.rs`) is free functions with no long-lived
  object to hang a field off, the injection point is a `#[cfg(test)]` thread-local sink instead of
  a struct field — a test asserts on structured `TraceRecord`s instead of scraping stderr text.
  Off by default and zero-cost when disabled: every call site checks the flag first and returns
  before any clone/allocation.

- **Canonical plan source on the plan-lifecycle record (L-38 — plan-corpus initiative).** Every
  ACCEPTED plan's `PlanAttempted` event now carries `plan_source` — the canonical, parseable
  Flux-Lang projection (`flux_lang::format::format`) of the accepted AST — alongside the
  display-only `plan_text` (`render_pretty`). Invariant: a present `plan_source` always parses —
  it is dropped (`None`) when over its 32k cap rather than truncated, and it is scrubbed through
  the same C-22 redactor as `plan_text` (redaction rewrites inside string literals, so the result
  still parses; pinned by `redacted_plan_source_still_parses`). Old events.db rows decode with
  `None` (serde-default, the `phase`-field precedent). End-to-end roundtrip pinned at the event
  boundary by `crates/flux-sdk/tests/plan_source.rs`: `parse(plan_source) == accepted ast` (L-18
  totality). This is the "projection, not emission" half of
  `docs/designs/plan-corpus-and-small-model.md` — plans become minable as text without touching
  the L-20 keep-json emission decision (the `FLUX_EMISSION` scaffold stays).

### Fixed

- **Bedrock frame decode resilience (A-36 — stream-resilience epic).** The AWS event-stream
  deframer (`flux-providers/src/bedrock.rs`) treated every decode failure the same way: a bare
  `?` inside the `try_stream!` loop killed the whole stream, whether the failure was one junk AWS
  chunk or the framing itself breaking. Now split into two classes. A `chunk` event's *payload*
  being garbage — not valid JSON, the `{"bytes":…}` wrapper's base64 not decoding, the decoded
  bytes not being UTF-8 — is tolerated: the frame is skipped, counted, and `tracing::warn!`'d, and
  deframing continues with the next frame (this layer emits raw bytes, not `Chunk`s, so there's no
  `StreamDiagnostic` to surface — the downstream Messages codec's own A-35 diagnostics cover
  content accounting). Genuine framing-**integrity** failures — a message or prelude CRC mismatch,
  an implausible frame length, a header block overrunning the frame or failing to parse, a frame
  missing `:message-type`, a truncated tail — stay fatal but are now classified
  `flux_core::Error::StreamDecode` instead of `Error::Provider`, so the A-33 backstop retries the
  call as one planner step instead of killing the turn. `exception`/`error` frames (a *declared*
  failure reported by AWS/the model) are unchanged and stay fatal via `Error::Provider` — that's a
  real failure, not garbage bytes, pinned by the existing `event_stream_surfaces_exception_frames`
  test.

- **Messages-wire envelope tolerance (A-35 — stream-resilience epic).** The shared Anthropic
  Messages SSE codec (`crates/flux-providers/src/messages/mod.rs`) — used by anthropic-direct,
  bedrock (re-enveloped), openrouter-anthropic, and ollama-anthropic — had one bare-fatal envelope
  parse (`serde_json::from_str::<StreamEvent>(data).map_err(...)?`) that killed the whole stream on
  either a syntactically-broken `data:` frame *or* a well-formed frame carrying a `type` the
  `StreamEvent` enum didn't recognize (a new vendor extension, a keep-alive-ish event) — the latter
  a latent kill found during design review, since the internally-tagged enum has no catch-all arm
  and rejects an unknown tag with the same `serde_json::Error` as malformed JSON. Both shapes now
  **skip + count** at that one call site instead of `?`-propagating (with a `tracing::warn!` per
  drop), and if any frames were dropped, yield exactly one
  `Chunk::StreamDiagnostic { dropped_frames, detail }` right before the stream's normal terminal
  chunks. No `wire.rs` catch-all variant was needed — catching the deserialize error generically
  covers both failure shapes. Declared provider errors (`StreamEvent::Error`, i.e. a real mid-stream
  outage) are unaffected: that variant still parses successfully and its match arm still propagates
  `Err`, pinned by a guardrail test (`messages_declared_error_event_stays_fatal`) so tolerance can
  never mask a real outage as a silent empty turn. Uses A-33's `StreamDecode`/`StreamDiagnostic`
  seams.

- **OpenAI-wire envelope tolerance (A-34 — stream-resilience epic).** The chat-completions and
  Responses-API SSE codecs (`crates/flux-providers/src/openai.rs`) each had one bare-fatal envelope
  parse (`let chunk: ChatChunk = serde_json::from_str(data)?;` and
  `let v: Value = serde_json::from_str(data)?;`) — the exact source of user-witnessed
  `runtime error: step plan failed: serialization error: …` turn deaths whenever the wire delivered
  one syntactically-broken `data:` frame (vendor keep-alive junk, a truncated proxy write). Both
  sites now **skip + count** an unparseable frame instead of failing the stream (with a
  `tracing::warn!` per drop), and if any frames were dropped, yield exactly one
  `Chunk::StreamDiagnostic { dropped_frames, detail }` right before the stream's normal terminal
  chunks — content that arrived in frames after the junk is never truncated. Declared provider
  errors (`response.failed`, the Responses `"error"` event type) are unaffected: those run on the
  successfully-parsed value and stay fatal exactly as before, pinned by a guardrail test
  (`responses_declared_error_events_stay_fatal`) so a real outage can never be masked as a silent
  empty turn. Uses A-33's `StreamDecode`/`StreamDiagnostic` seams; `flux-providers` gained a direct
  `tracing` dependency (previously only transitive) to emit the drop warning.

- **Stream-decode backstop (A-33 — the stream-resilience epic's first story).** A mid-stream
  provider decode error (malformed/truncated JSON from a weak model) used to `?`-propagate out of
  the planner's chunk-stream reader, discarding every accumulated block *and* the call's usage and
  killing the whole turn with `runtime error: step plan failed: serialization error: …` — the
  A-31 reject/retry loop never saw these errors because they escaped before reaching it. New
  `flux_core::Error::StreamDecode` classifies provider/model-bytes decode failures distinctly from
  transport errors, and a new `flux_core::Chunk::StreamDiagnostic { dropped_frames, detail }` lets
  a tolerant codec report skipped frames without failing the stream. `stream_blocks`
  (`flux-flow`) now returns its usage unconditionally alongside the `Result` (mirroring C-31's
  `compile_turn` shape one level down), so a decode error's already-streamed usage is never lost;
  the planner loop classifies `StreamDecode`/in-context `Serde` errors as retryable, costing one
  step of the existing `max_steps` budget (with `last_reject` naming the cause) instead of the
  whole turn, and an empty turn that nonetheless saw a `StreamDiagnostic` names that as the
  rejection cause too. Non-decode errors (API/HTTP/transport) still propagate and kill the turn as
  before. This story only adds the classification seams, exercised via mocks — the three SSE/frame
  codecs (OpenAI-wire, Messages, Bedrock) get their own envelope-tolerance stories next
  (A-34/A-35/A-36).

- **OpenAI-wire tool-args resilience (A-32 — the s_368 deepseek failure class).** The plain
  `openrouter`/`openai`/`ollama` chat-completions codec (and the Responses-API path) parsed
  accumulated tool-call arguments with a bare `serde_json::from_str(&args)?` — one malformed or
  truncated `emit_plan` blob killed the whole provider stream and with it the turn
  (`runtime error: step plan failed: serialization error: …`; s_368 lost one turn after seven
  accepted multipass rounds and another instantly to a 19KB blob the endpoint cut mid-list). Both
  sites now run the Messages wire's repair (`parse_tool_input`: tolerate trailing junk,
  balance-close truncation — the shapes deepseek-v4-flash and glm-5.2 actually emit), and when
  even repair fails the codec yields the tool_use block with a parse-error *sentinel* input
  (`__args_parse_error` + `__raw_prefix`, keys shared via `flux-core`) instead of a stream error.
  The planner turns the sentinel into A-31 repair feedback ("your `emit_plan` arguments were not
  valid JSON (…) — re-emit …") so the model retries in-turn and an exhausted budget names the real
  cause; without the gate a sentinel would even have decoded as an *empty accepted plan*. The
  Messages wire gets the same sentinel-instead-of-error treatment on repair failure. Pinned by
  failing-first tests on both wires plus planner sentinel-rejection/turn-survival tests.

- **OpenRouter models price from the provider's own reported cost, not just the static table
  (C-34).** Untabled OpenRouter models (deepseek, glm, qwen, …) always rendered ` · $? (unpriced)` —
  the pricing table can't keep up with OpenRouter's catalog. Both wires already carry a `cost` field
  (USD, from the final usage frame) on every response with no opt-in flag required; `Usage` now
  carries it (`reported_cost_usd`, summed across a turn's calls) and `PricingTable::cost` prefers it
  over the table at the single choke point every sink (REPL turn line, `flux usage`, TUI, `/goal`)
  reads from — `$?` disappears for any OpenRouter model the moment it reports a cost, with zero
  table maintenance. BYOK responses correctly add `cost_details.upstream_inference_cost`; non-BYOK
  responses don't double it (the field duplicates `cost` there). `flux usage`'s all-sessions rollup
  prices **per call** (reported where present, table where not) so a model's history mixing old
  unreported calls with new reported ones sums correctly instead of one reported call silently
  overriding the whole row's total. Pre-existing `events.db` rows decode unaffected
  (`reported_cost_usd` is `#[serde(default)]`); non-reporting providers are untouched. Live-verified
  against real OpenRouter traffic on both wires and against the real `~/.flux/events.db` history.

## [0.2.14] - 2026-07-03

### Added

- **Verified remote plugin install (D-47 — the demand side of plugin distribution).**
  `flux plugin install <name>[@<version>] …` (multiple names; `--all`) resolves the newest
  `plugins-v*` release (or the exact tag), fetches `plugins-index.json` + `.minisig`, verifies the
  minisign signature against a public key embedded in the binary (fail-closed, **no bypass flag**),
  refuses a protocol mismatch, verifies each archive's sha256 against the index *before* unpacking,
  installs into the versioned store `~/.flux/plugins/bin/<name>/<version>/`, and writes the
  descriptor with new serde-defaulted `version`/`sha256`/`source` fields. Download URLs are built
  only from `(repo, tag, asset-name)` — a URL-shaped index asset is rejected. The old local scan
  moved behind `flux plugin install --dir [path]` (now `.exe`-aware on Windows); bare
  `flux plugin install` errors naming both modes. `ls`/`status` show the version plus a
  `verified` / `unverified (local)` marker (spawn-time hash enforcement lands with D-48). All
  verification paths pinned by hermetic failing-first tests (injectable fetcher, real minisign
  fixtures — no network in the gate).

### Fixed

- **Planner parse resilience (A-30/A-31/C-31 — the s_360 qwen failure class).**
  OpenAI-wire-trained models habitually double-encode `emit_plan`'s nested `ast` argument — a JSON
  **string** containing a perfectly valid plan instead of an object (qwen3.7-max/plus, confirmed
  live) — and flux's strict decode rejected every repair step, killing the turn after 8 steps with
  the bare "planner did not produce a plan within 8 steps". Three fixes: **(A-30)** the JSON arm
  unwraps a string-encoded `ast` (`from_str` first; a non-JSON string keeps the strict error, and
  the decoded plan traverses the unchanged hidden-op/gather/validate gates) — live-verified: the
  exact s_360 model+prompt now compiles a clean 15-branch gather plan on the first emission;
  **(A-31)** the decode-failure and hallucinated-tool branches now record `last_reject`, so an
  exhausted budget reports "the last attempt was rejected: <cause>" instead of masking it;
  **(C-31)** `compile_turn` returns its accumulated `Usage` outside the `Result` and the loop host
  accounts it *before* branching on the outcome — a failed consultation now lands in the per-turn
  tally and `call_usage` events (s_360 burned ~8 × 37k input tokens invisibly to `flux usage`),
  and the emission A/B harness reports real spend on failed tasks instead of zeros.
- **Three weak-model landmines from the s_362 forensics (L-36, L-35, C-32).** *jq missing-data →
  null:* `jq` paths (and the `$a.b` field-access sugar) now yield `null` when traversing missing
  data instead of a fatal, turn-killing error — s_362's one substantive turn gathered everything
  then died at answer synthesis on an absent `.transcript` key; malformed path syntax still errors
  loudly. *`len()` counts elements:* the expression evaluator gained a real list type
  (`ExprVal::List`) — `len(glob("**/*.rs"))` returns the number of paths (was: the character count
  of the stringified array, which the model then confabulated an explanation around); string-stored
  op results re-parse to their native JSON shape before typing. *`read()` on a directory:* returns
  actionable, repairable guidance ("…is a directory — list it with glob(...) first") through a new
  guarded `System::is_dir`, instead of halting the plan with a raw `Is a directory` io error.
- **Per-turn dollar costs finally render where you work (C-30).** The interactive REPL (and
  `/plan`, `/compact`, `/run`, `/loop`, `/goal`, `flux plan`, `flux flow run`) never attached the
  pricing seam — the after-turn dollar suffix was structurally impossible outside one-shot
  `flux run` and the TUI, which is why costs "never appeared anywhere". Every CLI sink now derives
  its model spec from the live engine at turn start (so `/model` switches are priced correctly)
  and attaches the loaded pricing table. Three companions: a **visible ` · $? (unpriced)` marker**
  when a metered cloud model has no pricing-table row (silent omission hid real spend; a
  once-per-run note points at the `~/.flux/pricing.toml` override — local/mock specs stay silent),
  `/run` and `flow run` now report their real model usage at turn end instead of `None` (and `/run`
  scopes the loop host, so nested plan ops stop streaming onto the stale prior turn), and
  `canonical_model_spec` keeps the **serving** provider for passthrough ids — OpenRouter spend is
  no longer silently attributed to `anthropic` in `flux usage` (old mislabeled rows stay separate;
  the merge never guesses across providers).
- **CI: the plugins release workflow's Intel-mac leg** targeted the retired `macos-13` runner label
  (jobs starved waiting for a runner); switched to `macos-15-intel`, matching the core dist release.

## [0.2.13] - 2026-07-03

### Fixed

- **A-28 — window-sliding reads no longer defeat the resource stall guard.** The A-20 `ReadTracker`
  keyed freshness on `op + resolved args`, so a model sliding a `read` window a few lines per round
  over the same file registered "new evidence" every round and the escalate/stop ladder never armed
  (the `s_355` runaway: 25 rounds, one file, offsets 2180→2990, no answer). Freshness for
  `read`-shaped dispatches (`path` + optional `offset`/`limit`) is now coverage-based — a per-path
  covered-line interval set; a window is fresh only if it adds unread lines — while a first pass
  paging through new regions never trips the guard, grep/glob keep exact-key freshness, and
  write-invalidation clears coverage with the cache. The stall feedback now names the covered files
  and line spans so the model (and the user) see why more reading cannot help.

## [0.2.12] - 2026-07-03

### Added

- **Plugin pack release pipeline (D-46 — the supply side of plugin distribution).** A dedicated
  `plugins-v<version>` release channel for the 17-plugin integration pack, fully separate from the
  core cargo-dist series: the hand-written `release-plugins.yml` (`workflow_dispatch`, deliberately
  not a tag push) builds the pack on 5 native runners, packages one
  `flux-plugin-<name>-<version>-<target>.tar.xz`/`.zip` per plugin per target (bare binary at the
  archive root), and assembles a minisign-signed `plugins-index.json` generated by the new
  unit-tested `pack-index` tool (`plugins/pack-index`): `schema: 1`, deterministic output, asset
  values are bare file names — URL/path-shaped names are rejected so a compromised index can never
  redirect a download — plus the asset-count sanity gate. Releases are created `--latest=false` so
  the core installer URL keeps resolving to core releases; `publish: false` is a dry run. The
  verified remote `flux plugin install` lands next (D-47).

### Fixed

- **Review hardening — the 0.2.11 diff-review residuals.** An xhigh workflow-backed review of the
  0.2.11 diff produced 15 findings; each was grounded against flux's invariants before filing
  (design: `docs/designs/review-hardening.md` — the grounding outcomes, incl. one withdrawal and
  three downgrades, are the point of that doc). All 12 filed stories fixed, each with a
  failing-first test:
  - *C-27 — nested destructive re-fire (security).* `destructive_scope` is now a per-scope
    disclosure stack and the undisclosed-destructive gate keys on the **innermost** approved scope's
    own flag — a nested `run_plan` approved `destructive:false` no longer rides an outer plan's
    destructive disclosure, so a runtime-assembled destructive op inside the nested plan re-fires
    per-op approval instead of dispatching silently.
  - *L-32 — structural denial classification.* Envelope denials are flagged structurally at the
    executor's own deny sites (new `DispatchOutcome::denied`: cap-scope, policy floor,
    permission-rule deny, approval deny) instead of prefix-matching tool output — an op that ran and
    merely relayed `` `op` denied by … `` text is a repairable failure again, while genuine envelope
    denials stay fatal and hook denials stay retryable.
  - *C-28 — codex WS fail-fast contract.* Three fallback-defeating defects fixed: oversized pre-data
    error payloads are truncated on a char boundary (was a panic on multibyte text), the first-frame
    wait has a bounded connect timeout (default 30s) so a blackholing proxy fails over to HTTP-SSE,
    and a Close before the terminal event surfaces as a stream error instead of a silently truncated
    turn. (`tokio` becomes a hard dep of `flux-providers`.)
  - *L-33 — tilde-fence sizing.* The markdown writer sizes a tilde fence from the literal's tilde
    runs (was: backtick runs), restoring the `parse(to_markdown(parse(src))) == parse(src)`
    round-trip for code blocks whose info string forces a tilde fence.
  - *D-52 — SCRAM iteration bound.* The host-terminated PG handshake rejects a server-supplied
    `i=` above `MAX_SCRAM_ITERATIONS` (1,000,000) before any PBKDF2 work — a hostile/MITM'd endpoint
    can no longer peg a CPU core for minutes.
  - *A-26 — cumulative turn budget.* The per-turn token budget gates on cumulative billed tokens
    summed across the turn's calls (was: the replace-style last-call snapshot) — a runaway
    multi-call loop now trips the ceiling it exists to enforce.
  - *A-27 — identical-plan skip stall guard.* The skip transcript routes through `guard_transcript`,
    so a model re-emitting a byte-identical already-succeeded plan force-stops after the stall
    threshold instead of spinning the full 25-round repeat budget.
  - *A-25 — transitive delegation cap-scope.* Under opt-in `with_max_depth ≥ 2`, the depth-next
    spawner is built over the ancestor-narrowed registry subset and `task` must survive the
    role∩cap-scope intersection to delegate further — an ancestor `with_tools` ceiling now holds N
    hops down. Behavior note: a delegating role must now declare `task` in its tool set.
  - *L-30 — transitive surfacing enforcement.* `hidden_ops_in` expands composite bodies
    (cycle-guarded), so a turn-registered composite naming a non-advertised op is rejected by
    `compile_turn` like a direct hidden-op call. Legibility-gate completeness; the approval envelope
    always held.
  - *C-29 — queued a2a session retention.* Sessions are minted **inside** the single-turn gate
    (both `send` and `subscribe`), so a request queued behind a long turn can no longer age past the
    TTL and be swept mid-flight — no more orphaned event rows or spend missing from usage rollups.
  - *L-31 — cap-scope in concurrent position.* The analyzer statically rejects
    `with_tools`/`CapScope` inside a `parallel`/`race` branch (`check_cap_scope_position`, mirroring
    the await/checkpoint positional guards) — the shared cap-scope stack doesn't compose with
    concurrent branches.
  - *L-34 — spaced thematic break after a list.* `parse_list` terminates the list when the next line
    is a thematic break (`- - -`), matching block-start precedence, instead of consuming it as a
    nested empty list.
- **A-20 — the agent loop converges on read-heavy analysis turns (the `s_346` runaway).** The
  loop's stall guard hashed byte-exact `run_plan` transcripts, so re-reading the same files under
  renamed symbols / reordered statements never tripped it — `s_346` burned 22 read-only rounds and
  51.8k output tokens re-reading 6 files ~10× each with no answer. Fixed with a per-turn,
  dispatch-time read-resource ledger (`ReadTracker`, threaded into the loop's `run_plan` execution
  only — pre-authored `flow run`/journeys are untouched):
  - *Resource-aware convergence guard.* Reads are keyed on `op + resolved args` — symbol-name- and
    statement-order-insensitive by construction (the dispatch input is post-var-resolution). A
    clean round of pure reads that were ALL already gathered this turn is a no-new-evidence stall:
    the feedback escalates at 2 consecutive stalled rounds ("answer now") and the turn force-stops
    honestly at 3. Any effectful dispatch, any genuinely new read, or a read-free round resets the
    counter — incremental gathering and read→fix iteration are never punished.
  - *Redundant-read short-circuit.* An exact-repeat filesystem read (`read`/`glob`/`grep`/
    `read_many`/`sqlite_query` — classified from effect + access metadata, never an op-name list)
    is served from the turn's cache with a legible note (`already read as $X — reusing`) instead of
    re-fetching, so ignoring the session symbols is costless and the fed-back transcript stays
    small. Live session state (`evidence`/`metrics`) is stall-tracked but never cache-served; ANY
    local-state-mutating dispatch invalidates the cache (a post-write re-read is real IO again,
    pinned by test); hits are never served while a `with_tools` cap scope is open.
  - The `s_346` shape is pinned as a regression fixture: the full built-in agent loop, driven by a
    scripted planner that re-reads the same 3 files under fresh symbol names forever, converges in
    ≤ 6 planner rounds with an honest stop and exactly 3 real read dispatches (previously it spun
    toward the 25-round cap).

## [0.2.11] - 2026-07-03

### Changed

- **Efficiency + LLM prompt-cache hardening across the per-turn hot path.** A review of the agent loop
  surfaced several redundant-work and cache-invalidation issues; all fixed:
  - *Incremental conversation cache.* The reflexive `plan` op reloaded and re-decoded the entire
    session event log on every planner round (orient/gather/execute + repairs) — an effective O(N²)
    per session. The loop host now maintains the conversation incrementally, fetching only the
    message/compacted events appended since the last round (new `FlowStore::conversation_delta` /
    `EventStore::conversation_delta`, kind-filtered so the bulky plan/run/usage payloads are no longer
    decoded and discarded), and folding them in (a `Compacted` resets the fold). The cache is bounded:
    switching sessions evicts the previous session's entry, so a shared engine (a2a server) holds at
    most the active conversation.
  - *Monotonic (sticky) group surfacing.* Evidence-gated op surfacing was recomputed statelessly each
    turn, so a workspace marker appearing/disappearing rewrote the cached op catalog and missed the
    provider prompt cache on the whole `tools + system` prefix. Surfacing now accumulates within a
    session — once a group surfaces it stays advertised — keeping the cached prefix stable. Behavior
    note: advertising is not granting; the approval/policy envelope still gates every op. Only affects
    workspaces that opt into gating via `.flux/groups.toml` (the default advertises all ops, already
    stable).
  - *Schema memoization.* The op catalog re-derived every registered tool's JSON schema via
    `schema_for!` (~40 per turn) and each op resolution rebuilt a signature, plus the node-kind catalog
    and both plan grammars were rebuilt on every planner call — all compile-time constants. Now
    memoized: `tool_input_schema` caches per `TypeId` (a bare `static` in a generic fn is shared across
    monomorphizations, so it must be type-keyed), and `ast_schema` / `node_kind_catalog` / `ast_grammar`
    / `text_grammar` via `OnceLock`. `tool_input_schema` / host-kit `op_input_schema` (+ the `*_typed`
    spec helpers) gain a `T: 'static` bound — satisfied by every real input struct.
  - *Event-append statements* now use `prepare_cached` (many appends per turn).
  - *`grep`* bounds its per-file line-scan (`GREP_FILE_BYTE_CAP`) so one huge matched file can't
    dominate a search; *windowed `read`* streams only the requested line window instead of
    materializing every line of the file first, and applies the byte cap to the windowed path too.
  - *`detect_signals`* probes workspace markers in a single upward walk instead of one full cwd→root
    walk per marker.

  Deferred for a follow-up (noted, not done): the compaction size-probe still serializes to measure
  (a cheaper estimate would shift trigger semantics, and it's once-per-turn, not the O(N²) path), and
  the per-repair-step `Request` re-clone (a clean fix would widen the `Provider` trait for a cold
  path).

## [0.2.10] - 2026-07-03

### Changed

- **Website: complete makeover of the Flux-Lang docs.** The language section grows from 5 stub pages
  (~280 lines) to a 16-page language documentation set (~2,400 lines) sourced from the authoritative
  in-repo references (`crates/flux-lang/docs/{syntax,reference}.md`,
  `crates/flux-flow/docs/ops-reference.md`): an example-first overview + ten-minute tour, an
  eight-page guide (flows & syntax, control flow, pure data, context packs, concurrency, reliability,
  durability, execution model, modules/composite ops/programs), a four-page reference (full node
  catalog with JSON shapes + field tables, types & effects incl. the prelude artifacts, the registered
  ops catalog, tooling), and a rewritten examples cookbook. Every one of the 86 `flux` code snippets
  on the site is validated against the real parser (`fluxlang compile`); the docs strictly separate
  native text spellings from `@json`-only nodes and drop all aspirational syntax. Site chrome:
  a custom Prism grammar highlights `flux` code blocks, the landing page gains a hero flow +
  dark-mode support, the sidebar gets Guide/Reference sub-categories, and `flux flow run` is now shown
  in getting-started. The old `text-syntax`/`execution-semantics`/`ast-reference` pages are replaced.
  Fixed en route: an `@effect` example (inherited from syntax.md) that annotated a bare call, which
  the parser rejects — `@effect` only annotates binds.

### Fixed

- **Two invalid examples in the flux-lang syntax spec** (`crates/flux-lang/docs/syntax.md`), caught by
  running every website snippet through the real parser: the `@effect` section annotated a bare
  `bash(...)` call (the parser only accepts `@effect` on a bind), and the `each -> flat` example used
  the aspirational comma-kwarg call form `glob("*.rs", path: $dir)` instead of the implemented
  single-object form. Both corrected and parser-verified.

## [0.2.9] - 2026-07-03

### Fixed

- **Release pipeline builds again (all platforms) — first green release build since 0.2.4.** Two
  unguarded unix-only paths broke the `x86_64-pc-windows-msvc` build: flux-tools' dev-mode `ReloadTool`
  called `execv` (`std::os::unix::process::CommandExt`) unconditionally, and the AWS Bedrock SSO-cache
  writer set unix `0600` permissions unguarded. Both are now `#[cfg(unix)]`-gated with Windows
  fallbacks (spawn + wait + exit; default ACLs). Separately, cargo-dist failed on every platform with
  "failed to find bin fluxlang": `flux-lang`'s `fluxlang` bin is a feature-gated dev tool the default
  dist profile never builds, so the crate is now marked `dist = false` — only the `flux` binary
  (flux-cli) ships.
- **CI green on rust 1.96 clippy.** The GitHub `ci` workflow tracks stable, which moved to 1.96 and
  flagged three `-D warnings` lints: `unnecessary_sort_by` (the flux-skill activation ranking and the
  homer plugin's call-summary sort → `sort_by_key(Reverse(..))`), `collapsible_if`/match-guard in the
  sql plugin's read-only detector, and a test-only `result_large_err` on the codex WebSocket stub
  (allowed — the callback's `Err` type is fixed by the tungstenite API). `cargo clippy --workspace
  --all-targets -- -D warnings` is green on both the root and `plugins/` workspaces; `plugins/Cargo.lock`
  synced to the 0.2.8 workspace versions.

## [0.2.8] - 2026-07-03

### Documentation

- **Docs consolidation + public docs site refresh.** Reviewed and consolidated `docs/`: merged three
  genuinely-redundant design clusters into their story-linked survivor (plugin ×4 →
  `integration-plugins.md`, channel ×2 → `event-trigger-channels.md`, provider ×2 →
  `subscription-providers-and-cost.md`; `docs/designs/` 31 → 26), repointed the affected story
  `design:` links + roadmap/cross-design links (0 broken links), truth-passed stale "Status: proposed"
  lines on already-shipped designs, and corrected stale Anthropic model ids in `docs/model.md` to the
  real aliases (`opus`→`claude-opus-4-8`, `sonnet`→`claude-sonnet-4-6`, `haiku`→`claude-haiku-4-5-20251001`).
  Refreshed and expanded the public Docusaurus site (`website/`, deployed to
  `codewandler.github.io/flux`): new pages for the multi-pass agent loop, agent-to-agent (A2A),
  multi-agent `.flux` programs, and the safety & approval model, plus install-first getting-started,
  a full provider matrix, a concrete config schema, and CLI subcommand/slash-command tables. Site
  build green (`onBrokenLinks: throw`).

### Fixed

- **Library hardening (epic).** Three adversarial subsystem audits (context, evidence,
  flux-lang/flow) surfaced 15 code-confirmed residual defects inside already-shipped stories; all
  are fixed with the failing-first test named in each story, full gate green (1218 tests).
  - *flux-lang / flow.* **L-26** — the CSE/batch optimizer's `collect_var_reads` dropped `Obj`/
    `List`/`Fmt`/`Expr` under a `_ => {}`, so a `$var` read inside a named-argument object was
    invisible to the batch/CSE hazard check (a reader could parallelize with its writer; CSE could
    reuse a stale value) — now routed through the exhaustive `for_each_node`, with a property test.
    **L-27** — the `route` selector, `verify` `expect`, and `expr` formula were analyzer positions
    the runtime rejected but the analyzer accepted; now diagnosed at analysis time (bind-it-first
    hint / formula validated against its own `vars`). **L-29** — the gather-phase effect gate only
    blocked `Write`/`Destructive`, letting an advertised `Network`/`Process+LocalSystem` op (http,
    `run_plan`, cargo, bash) execute in a "read-only orientation" round; it now flags any
    `Write`/`Network`/`Browser`/`LocalSystem` effect or `Destructive` risk while keeping
    `Read`/`Filesystem`/bare-`Process` (`git_status`) gather-safe. **L-28** — a resumable-mode
    ledger fast-forward silently dropped a binding when a ledgered value couldn't be rehydrated;
    now a hard resume error naming the lost statement, plus a `parallel` arm in `top_level_bind`.
  - *Context management.* **A-21** — `<knowledge-base>` block bodies were emitted verbatim, so
    untrusted RAG/datasource text containing `</knowledge-base>` could close the containment tag and
    inject top-level system content; the body is now neutralized (case-insensitive,
    whitespace-tolerant) in both render paths, before truncation. **A-22** — served/agentic/SDK
    agents built with `AgentSpec::default()` never compacted (`compact_threshold_chars = 0`), so a
    persistent-session channel target grew unbounded; a non-zero default + `with_compaction` +
    per-agent/env/default precedence now bounds it (CLI and one-shot `flux run` unchanged). **A-23**
    — the Anthropic 4-`cache_control` ceiling was unguarded (subscription-claude sat at exactly 4);
    `segmented_system_field` now caps breakpoints at 4, keeping the largest (stable prefix
    preserved). **A-24** — `render_knowledge_blocks` and `symbols_block_bounded` overshot their byte
    budgets (marker/header uncounted); both now reserve it and pin `len <= cap`
    (`SYMBOLS_CHAR_CAP`→`SYMBOLS_BYTE_CAP`, honest doc).
  - *Evidence.* **C-22** — the durable evidence trail (tool-call observations + rendered plan text)
    was persisted to `events.db` unredacted; both are now scrubbed through the executor's `Redactor`
    before persistence, so a secret in a plan/bash arg no longer lands in the clear. **C-23** —
    sub-agent spend was double-counted in the all-sessions cost/efficiency rollups (parent synthetic
    `CallUsage` + child's own stream); correlated child streams are now excluded from the
    all-streams folds (per-session reporting unchanged). **C-24** — `flush_observations` advanced
    the watermark past failed writes (silent permanent loss); it now advances only past successful
    writes and retries the rest. **C-25** — the shared `events.db` set no `busy_timeout`, so a
    serve-daemon + CLI collided on `SQLITE_BUSY`; `open` now sets `busy_timeout`/`synchronous=NORMAL`
    and writes use `BEGIN IMMEDIATE`. **C-26** — await/resume (reply-parking) continuations recorded
    no turn telemetry (`turn_id = -1`); they now run inside a real `begin_turn`/`end_turn` with
    sub-agent `CallUsage`.

### Changed

- **Docs truth pass (C-16).** The claims are a product surface, so they now say exactly what the
  code does: README's "re-running costs zero extra model calls" is scoped to stored-plan replay
  (a live turn costs the *fewest* calls, matching vision.md); "symbols not re-sent" became the
  precise "bounded digest of summaries" (A-07); the safety-model section now shows the
  capability-scope floor as step 0 of the chain, the plan-intents disclosure re-fire (C-12), and
  the documented `bash` `sh -c` exception to argv-only with its actual defenses (per-token
  permission subjects, the `<shell-expansion>` sentinel subject, destructive-command escalation);
  the evidence and sub-agent bullets reflect the durable trail (C-14) and correlated child audit
  (A-08). roadmap.md's "Known divergences" strikes "Two turn loops" (A-01 done) and "No cost
  tracking" (C-05/C-06/C-15 done). Every fix in this round was also live-verified against a real
  model and events.db — a completion-carrying turn makes exactly 2 model calls ($0.0032 vs the
  prior $0.0137 baseline), `--turn-budget 1` stops honestly, plan attempts persist with rendered
  text + fingerprint, sub-agent streams land correlated, and `flux usage` reports the efficiency
  line over merged keys.

### Added

- **Multi-pass cutover measurement plumbing (I-03, in progress).** The efficiency rollup now
  reports the phased loop's rounds: `EfficiencySummary` folds `orient/gather/revise` rounds and
  accepted plans from `PlanAttempt.phase` — gather counts every gather-phase attempt (a repair
  round is a paid round), revise only execute-phase attempts that accepted a further plan (the
  terminal chat round is not a revision), and plans/turn is phase-blind so pre-A-14 logs still
  report the tiny-plan-dribble watch. `flux usage` prints `plans/turn` always and gather/revise
  per turn only when the log carries phase data (on an older log the figures are unrecorded, not
  zero). Alongside, two dry-run-by-default measurement harnesses land in `bench/`:
  `run-ttff.sh` (fixed 5-prompt corpus + PTY recorder keeping raw timestamped chunks; `report.py`
  derives spawn→first-rendered-artifact medians, spinner/banner excluded, failed-turns flagged)
  and `run-tbench-compare.sh` (pre-cutover `b528772` vs current, same tasks/trials/model, each
  leg's own prebuilt musl binary with `rebuild: false`). The paid comparison legs are the
  remaining I-03 step.

- **Multi-pass agent loop: orient → gather → execute/revise, with patch-and-continue (A-12–A-17,
  L-22).** The turn loop (`agent-loop.flux`) is now three visible passes: the first planner call
  *orients* (answer as chat, emit the full execution plan, or emit a small read-only gather plan
  plus a `brief` grounding artifact rendered immediately as `◆ goal: …`), then up to three
  compile-enforced gather rounds (effect-clean ops only — composites checked transitively — capped
  at 12 call nodes, validated as repair feedback like hidden-ops, approval-free), then the standard
  execute loop with unchanged guards. Trivial and simple turns make exactly as many provider calls
  as before. The planning wait is never silent anymore: normal-mode turns stream planning state and
  thinking deltas, and the CLI/TUI label the spinner by phase (orienting… / gathering… / planning… /
  revising…). Mid-plan failure no longer discards the plan: `run_plan` executes in a resumable mode
  where a failing top-level statement is *reified* — the result carries
  `failure{node, stmt, op, kind, fatal, message, completed[]}` plus the prefix transcript and a
  ✓/✗/·-marked plan — and every completed statement is ledgered as append-only run-events
  (`StatementCompleted`/`PlanHalted`/`PlanResumed`, fold-derived latch, no new table). The model's
  corrected full re-emission passes the unchanged `emit_plan` gates (C-17 intact), and the runtime
  fast-forwards the hash-matching completed prefix with value rehydration, continuing from the
  failure point; the loop routes revision rounds on `$ran.failure` and the surface shows
  `✗ step N/M — revising…`. Policy: denied/confirm-denied statements are never re-dispatched
  unchanged; assert failures demand re-planning the remainder; approval is scoped to the suffix
  that will actually run; repeated identical halts escalate through the existing loop guard.
  `PlanAttempt` gains a `phase` field so usage metrics can report gather/revise rounds per turn.
  Epic design: `docs/designs/multipass-agent-loop.md`; the measured cutover verdict (I-03:
  time-to-first-feedback, rounds, tokens, terminal-bench pass-rate vs baseline) is still open.

- **flux-markdown is a real engine, and skills load progressively (L-02).** The markdown crate is
  no longer a wrapper: a two-pass own engine (goldmark-style AST — recursive block pass +
  CommonMark delimiter-stack inlines, GFM tables/strikethrough) with a shared width-aware layout
  core feeding both the terminal and ratatui renderers, whose public APIs are preserved exactly.
  The old wrapper crates survive only as dev-dependency *parity oracles*: exact per-line
  ANSI/ratatui output parity is pinned over a snippet suite and nine committed repo-doc fixtures,
  plus a parse/write round-trip law. Skill activation is now standards-aligned progressive
  disclosure: startup reads only a frontmatter head-scan (name + description), selection/capping
  happens on metadata alone, and a skill's body is read lazily at the moment of injection —
  unselected bodies never touch the disk. `[skills] dirs` in config (CLI > project > user >
  defaults, with a repeatable `--skill-dir`) adds custom skill directories, and the agent/SDK now
  populate skills from the same well-known dirs as the CLI. Documented subset: no setext
  headings, indented code, general HTML, reference links, or footnotes; terminal code blocks lose
  per-token syntax colors (uniform code color).

- **The endpoint epic is complete — the host now terminates raw-socket auth (D-31).** The last
  place a (trusted) plugin held a secret is gone: the new `conn.authenticate` host capability
  performs the PostgreSQL v3 startup + auth handshake host-side (full RFC 5802/7677
  SCRAM-SHA-256 including server-signature verification, plus MD5/cleartext) over an
  already-dialed connection, resolving the credential by reference (broker path — cross-plugin
  gate + audit unchanged — or declared auth purpose) and handing `sql` a *post-auth* connection
  with only `server_version`/parameters/backend-key. The `sql` plugin's own
  startup/SCRAM/MD5/crypto code is deleted and its manifest grants **no `credential` and no
  `secrets`**; MockHost call-log tests prove no password ever crosses a plugin frame. The
  references-only invariant now holds absolutely, not "except for in-band-auth raw sockets";
  mysql/AMI termination are seamed follow-ons behind the same capability.

- **Plugin-pack distribution is scoped and decided (D-21).** How a non-source user gets the ~20
  integration plugins: **fetch-on-install from a signed, first-party pack channel** — pack
  releases as their own `plugins-v*` GitHub release series (one prebuilt archive per plugin per
  target + a minisign-signed `plugins-index.json`), with `flux plugin install <name>[@version]`
  verifying the index signature against a pubkey embedded in flux and the artifact sha256 before
  anything becomes executable, into a versioned store that gives the existing `pin`/`rollback`
  real teeth (spawn-time hash re-verification). Bundling into the core release was rejected on
  coupling (not size); no marketplace service — the index is the marketplace seed. Design:
  `docs/designs/plugin-distribution.md`; implementation slices filed as D-46 → D-47 → {D-48,
  D-49}. Found along the way: Windows `--dir` install is broken today (`plugin_binaries_in` skips
  `.exe`) — fix scheduled in D-47.

- **The planner-emission A/B has an answer: strict JSON stays (L-20).** The open question from
  the emission design doc is now measured, not guessed: a `FLUX_EMISSION=json|text` selector (json
  default byte-identical to the shipped surface; both arms behind the same hidden-op +
  analyze/lower gates), a 15-task committed corpus, and one live run per arm on OpenRouter Sonnet.
  Strict-schema JSON wins where it matters — 93% vs 60% first-emission acceptance, 15/15 vs 13/15
  accepted within one retry — while native text wins base-prompt size (−47%) and cost (~15%) but
  loses wall time. Decision recorded in `docs/designs/flux-lang-emission-ab.md` with the full
  table; the text arm stays behind the flag for future re-measurement.

- **Live smoke gate covers the subscription providers — and WS regressions fail loudly (C-19).**
  `scripts/smoke-live.sh` gains a `claude` and a `codex` leg (one tiny turn each, SKIP when the
  credential is absent). The codex leg runs under the new `FLUX_TRANSPORT_DEBUG=1` switch: the
  transport→HTTP fallback in `NativeProvider::stream` now emits a stable, env-gated stderr marker,
  and the smoke greps for it — so the next codex wire-contract drift fails the gate with the
  fallback reason instead of silently completing over HTTP (the C-07 lesson). Both legs validated
  live (codex confirmed over the WebSocket). Known issue recorded in the story: the script's older
  steps 1–5 still use the pre-subcommand CLI forms and need modernizing.

- **`flux auth login codex` — flux-native PKCE login, claude parity (C-08).** The last open story
  of the subscription epic: codex authentication no longer requires logging into the Codex CLI
  first. The flow uses upstream-verified constants (`auth.openai.com/oauth/authorize`, the
  registered `localhost:1455/auth/callback` redirect, `id_token_add_organizations=true` so the
  ChatGPT account id rides the id_token claims) with a deliberately narrower scope than upstream
  (connectors scopes dropped — least privilege, documented). CSRF state-binding is factored into
  one helper shared with the claude flow and checked before any network I/O; the exchange
  persists under the `codex` provider exactly like the import path, which remains the default.
  All hermetic (stub token endpoint, injected callback); a manual live login smoke is the
  recorded residual.

- **flux-lang v1 residual burn-down (L-21).** The four residuals the hardening epic recorded at
  close are gone: (1) suspensions persist the flow name (guarded migration), so a *named* flow
  resumed through the engine derives the same name+hash checkpoint key as its run — edited flows
  no longer fast-forward wrongly on the resume path either; (2) a policy/permission/scope/user
  denial is now structurally marked (`OpOutcome.denied` set by the host, the executor's canonical
  op-anchored denial shape pinned by a contract test) and surfaces as the fatal
  `FlowError::Denied` — a denied op inside `loop`/`retry` dispatches exactly once, and a denied
  condition op propagates instead of reading `false` (hook denials deliberately stay retryable);
  (3) `each` source / `jq` input / `parse` value reject `call` nodes at analysis time like the
  runtime does at eval time; (4) type-checker diagnostics carry the same JSON-pointer node paths
  as structural ones.

- **A2A task sessions now expire — TTL-scoped retention (C-18).** Every A2A request minted a
  session that lived in `events.db` forever. A2A sessions are now created tagged (D-02 envelope:
  `agent_id "a2a"`, the request's `contextId` as `correlation_id`) and a lazy sweep at each mint
  deletes whole expired A2A streams — age measured from last activity, TTL from
  `[server] a2a_session_ttl_secs` (default 3600, `0` = never prune, project-over-user merge).
  Deletion is stream-scoped (`EventStore::prune_inactive`, one txn) — append-only holds *within*
  a stream; retention is a stream-level decision, same as the existing `prune_empty`. CLI/TUI and
  other untagged sessions are never touched, and the sweep covers both the standalone server and
  the flux-channels `a2a` mount with no caller changes.

- **`ask` now really waits — journey reply-parking on the suspension seam (A-11).** flux-app's
  `ask { channel, message }` was documented as expecting a reply but behaved like `send` (the
  reply was never awaited). A top-level `$reply = ask(...)` in a journey is now lowered at run
  time into the same `ask` call plus an `await` (source `ask.reply`), so the flow suspends on the
  existing seam; the App parks the run keyed by the asked channel and a correlated inbound
  message — the channel's own name, or `user_input` for CLI-rendered channels — is consumed to
  resume the oldest matching park with the reply text bound as the ask's result. Uncorrelated
  messages route normally. Resume re-enters through `resume_flow` over a full-envelope executor
  (no side-channel execution). Nested asks keep fire-and-forget (`await` is top-level-only), and
  park timeout/expiry is a recorded follow-up.

- **Pricing table verified against vendor sheets — codex costs were understated ~4× (C-20).**
  Every `builtin()` rate is now verified against the vendor's current public pricing page
  (2026-07-02, source URLs in the doc comment) instead of shipping "plausible" figures behind a
  `TODO verify` hedge. Headline fix: `gpt-5.5` had shipped sharing gpt-5's launch rates
  ($1.25/$10) but actually bills **$5/$30** (cache write $5, read $0.50) — the exact row the live
  `codex` provider resolves to, so all codex cost reporting to date was ~4× low. The llama
  OpenRouter row was also adjusted, unverifiable rows (`gpt-5-codex`, routed llama) are marked
  ESTIMATED inline, known unmodelled premiums are documented (Bedrock cross-region ~10%, OpenAI
  long-context 2×/1.5×, Anthropic 1h cache-write tier), and a pin test locks the headline rates.

- **The `host.endpoint` URL-handback is gone — references-only plugin IO is now compile-enforced
  (D-32).** No plugin op receives a URL string anymore: the `endpoint` host-capability arm in
  `SystemHostCaps`, host-kit's `Host::endpoint`, and `MockHost`'s endpoint shim are deleted, so any
  straggler fails to build (the epic's clean-cutover proof). The residual uses were covered first:
  a gated non-secret **`config` capability** (declared `ConfigSpec`s only, refuses
  secret-classified env keys *and* credential-bearing values like a password-embedding DSN);
  **template endpoints** (`EndpointSpec.template`) that compose dynamic bases host-side from
  declared config — the Atlassian gateway (`api.atlassian.com/ex/…/{cloud_id}`) is now a named ref
  the plugin addresses without ever holding the URL; **`http_bytes_ref`** for attachment byte-IO by
  reference (jira/confluence migrated, gitlab archive downloads now byte-exact); and `sql` dialing
  by named ref (`SqlTarget` lost its `host`/`port` — the plugin *cannot* dial a parsed address;
  the mock-only bare `@endpoint/<id>` string input was removed in favor of the full `endpoint`
  object from `endpoint.select`). Scope grew from the story's 5–6 residual call sites to 13
  consumer plugins, all migrated (`EndpointSpec.default` absorbed plugin-side URL fallbacks
  behavior-preservingly; ops that surfaced a resolved URL now surface the ref name). Both
  workspaces green; the plugins workspace builds with zero references to the old capability.

- **Codex WebSocket transport — default, with transparent HTTP-SSE fallback (C-07).** The `codex`
  provider now dials `wss://chatgpt.com/backend-api/codex/responses` first, mirroring the upstream
  codex client. A new provider-level seam (`flux_provider::StreamTransport`, tried by
  `NativeProvider` before the reqwest path) keeps every other provider byte-for-byte on HTTP+SSE.
  The tungstenite handshake carries the codex auth headers (Bearer, `chatgpt-account-id`,
  `OpenAI-Beta`, `originator`) from the one `codex_headers()` source that also feeds the HTTP
  credential, so the two paths cannot drift; response frames are re-enveloped as SSE bytes into
  the existing Responses codec, making the chunk stream identical by construction. Any
  connect-time failure — refused connection, handshake rejection, a policy close (1008), or an
  `error` event before the first response event — falls back transparently to HTTP-SSE, which
  also owns 401 recovery (C-04). The wire contract was **live-verified the same day and
  corrected**: the backend requires the request as an inline `response.create` event (a bare body
  is rejected with an error *event*, which the first implementation would have committed to,
  silently killing the WS leg) and prefixes responses with a WS-only `codex.rate_limits`
  preamble, and it resets the socket after the terminal event instead of a close handshake; the
  transport now sends the envelope, skips the preamble pre-commit, treats an error-type first
  frame as fallback, stops reading at the terminal event (a reset before it still surfaces as
  real truncation), and the hermetic stubs pin the observed live contract — a live
  `flux run -m codex` turn completes green over the WS path end-to-end.

- **flux-lang v1 hardening (epic: C-17, L-15..L-19).** A full review of the language pillar
  (27 findings, `docs/designs/flux-lang-v1-hardening.md`) fixed in one pass:
  - *Compile-path plan gates (C-17).* The plain-text plan fallback now runs the same
    `hidden_ops_in` gate as `emit_plan` (a prose-JSON plan can no longer execute a
    registered-but-unsurfaced op); plans are never "accepted with diagnostics" (always repair
    feedback, with a `plan_turn` backstop); duplicate `emit_plan` calls in one message are
    rejected; the planner loop's `unsafe` sink reborrow is gone (`#![deny(unsafe_code)]`).
  - *Analyzer contract (L-15, L-16).* `analyze_flow`/`lower` now take the session-symbol set and
    enforce symbol definedness (unbound `$var` is a diagnostic, order-insensitive — zero false
    positives), required-param presence on named input maps, expression-position legality
    mirroring the runtime's `eval_arg`/`eval_cond`, declared-name validity (dotted/spaced names
    rejected — the silent round-trip corruption class), `repeat` bounds sanity, `parallel`
    cross-branch bind disjointness, and JSON-pointer node paths (`body[3].then[1]`) on every
    diagnostic. **`lower` (typed gate) now runs on the production path** — engine compile,
    composite registration, and the CLI flow gate — not just the SDK opt-in.
  - *Runtime semantics (L-17).* One shared `eval_pure_node` path for pure nodes in bind and
    statement position (fixes statement-`jq` divergence); structural error fatality
    (`FlowError::is_fatal()`) surviving `loop`/composite wrapping so a denied `confirm` is never
    retried; `parallel` merges completed branches' audit in declaration order even when a sibling
    fails; `race` distinguishes all-failed (joined error) from timeout and keeps losers' dispatch
    audit; one checkpoint `flow_key` (name + body hash) for run and resume — edited flows never
    fast-forward wrongly; `throttle` counts real op dispatches atomically; `debounce` is real
    keyed cross-turn coalescing; `StepId` includes the op name; `each` restores shadowed outer
    bindings; `pipe`/`memo` transcript views respect the output cap.
  - *Round-trip totality (L-18).* `parse(format(ast)) == ast` now holds for every `DraftAst`:
    the formatter falls back to `@json` for unspellable names in every position (op/symbol/
    branch/collect/bind/type), pinned by a seeded 1000-iteration property test over all 43 node
    kinds; parse errors carry `line N:` locators; the parser rejects dotted declared names
    (`$a.b = …`) with a field-access hint.
  - *Docs truth pass (L-19).* syntax.md/reference.md/STATUS.md/emission-ab.md now describe only
    what is implemented (multi-line strings, comma-form named args, `watch`/`block`, `memo`
    keyword et al. explicitly marked aspirational or `@json`-only; race/throttle/debounce match
    the new runtime); the plan-approval render shows `obj`/`list` template contents and
    `children()` is exhaustive; skill examples carry a parse-as-`DraftAst` drift test.
  - Design policy adopted: **node-catalog freeze** — no new `Node` kinds until definedness
    analysis and diagnostic locators had shipped (both landed with this epic).

- **Per-turn efficiency metrics + canonical usage keys (C-15).** Tokens-per-task was queryable only
  from raw `CallUsage` rows. `TurnSummary` now folds each turn's provider-call count and per-call
  usage sum, and the new `efficiency_summary()` projection (surfaced as one line per `flux usage`
  section) reports turns, calls/turn, iterations/turn, the cache-read share of the prompt side, and
  uncached-input/output per turn — the Improve pillar's efficiency trend at a glance. Usage
  attribution keys are now stamped **canonically at write time** via the new
  `flux_core::pricing::canonical_model_spec` (`provider/model`, aliases resolved, Bedrock regional
  routing prefixes stripped) at all three write sites — the loop host's planner and completion-render
  calls, the engine's `begin_turn`, and sub-agent spawns. **Migration caveat:** the append-only log
  is never rewritten, so older events keep their inconsistent keys (`gpt-5.5` vs `openai/gpt-5.5`,
  `aws/us.anthropic.…`); `cost_summary` now merges those legacy variants on the read side —
  same-provider canonical duplicates always collapse, and a bare key folds into a prefixed sibling
  only when exactly one candidate provider exists (an ambiguous bare key stays its own row rather
  than guessing between providers that bill differently).

- **Opt-in per-turn token budget — the loop now has an enforced spend ceiling (A-10).** Usage was
  accounted per call but nothing enforced a bound: a pathological turn was limited only by the
  25-iteration cap times up-to-8 compile steps. The loop host now checks the turn's accumulated
  planner usage at the top of every `plan()` (the same stall-stop pattern as the retry-breaker):
  once it crosses the installed budget, the turn ends honestly with a budget-exceeded answer naming
  the budget and the tokens used — no further model call is paid, and a `turn.budget_exceeded`
  observation lands durably on the evidence trail. Resolution: `--turn-budget <tokens>` >
  `FLUX_TURN_TOKEN_BUDGET` > the new `.flux/config.toml` `[limits] turn_token_budget` (project
  overrides user). **Default OFF** — the iteration cap and stall guards already bound normal turns.
  The ceiling counts the turn's planner/render calls; sub-agent spend still rolls up only at turn
  end (documented limitation).

- **Sub-agent audit is ON by default — child runs land correlated in the shared event store
  (A-08).** The D-05 `with_audit` seam existed but the primary paths never wired it: every `task`
  spawn fell back to a throwaway in-memory store, so a sub-agent's tool activity vanished (only its
  token usage bubbled up). The CLI now opens the session event store BEFORE building the spawner
  and audits children into it by default; flux-app's journey spawner does the same with the app's
  own store. Each child gets its OWN session stream, correlated on the D-02 context envelope —
  `agent_id: "subagent:<role>"`, `correlation_id: <parent session>` — so the store answers "what
  did the sub-agents of turn X do" with one indexed read; the parent session id reaches the spawner
  via the new `ToolContext::set_session/session_id` (installed per turn beside `set_cancel`, and
  per journey run in flux-app). The `Spawner` trait collapsed to a single
  `spawn(SpawnRequest { role, task, cap_scope, parent_session }, cancel)` (breaking, clean
  cutover), and `SpawnOutcome` now reports the child's `session_id` + `tool_calls`, which feed a
  new compact `subagent.trace` observation on the parent's evidence trail (a pointer — the full
  child trail already flushes durably under its own correlated stream via C-14, never a copy).
  Spawners built without an audit store keep the ephemeral in-memory behavior — the documented
  mode for storeless hosts (SDK consumers, tests), not a fallback.

- **The evidence trail is durable — observations, plan attempts, and signal provenance land in
  `events.db` (C-14).** The audit story used to be two unrelated systems: the in-memory
  `EvidenceLog` that `/evidence` reads (lost on exit) and the durable event store — and the
  compiled plan graph reached NEITHER (`flow.plan` went to the display sink only;
  `record_plan_attempt` had zero production callers). Now: (1) a new
  `EventKind::Observation(flux_evidence::Observation)` and a per-turn watermark flush in the
  engine persist every observation (`tool_call` markers, `turn.iteration`, `groups.active`,
  `skill.activated`, flow-emitted `observe(…)`) to the session stream at both turn-termination
  paths — batched per turn, so a crash mid-turn loses at most that turn's batch; (2) the loop host
  records every planning attempt durably — `accepted` with the plan's AST fingerprint and its
  human-readable rendered graph (capped 8k; "a turn is a readable graph", persisted), `chat`,
  `compile_error` (with the planner error), and `rejected` (user declined) — via the struct-shaped
  `record_plan_attempt`, with `#[serde(default)]` fields so old logs decode; (3) the `groups.active`
  observation now carries the workspace **signals** that justified the surfaced groups, closing the
  signal→group provenance gap. `/evidence` keeps reading the live in-memory log; the new
  `flux_events::projection::observations()` (and `EventStore::observations`) serves offline reads.

- **The `complete` fast-path is wired — a finishing turn no longer pays a full second planner round
  (A-06).** The planner prompt has always instructed models to attach a `complete` directive
  (instructions for the final message) to a plan that finishes the request, and `render_completion`
  (a grounded, toolless, catalog-less render) was fully implemented — but nothing ever called it:
  the loop host surfaced `complete` as a bare boolean the shipped loop never read, so every working
  turn ended with planner round 2 (full catalog + conversation + uncached symbols) just to produce
  closing prose. Now `run_plan` arms the directive after a plan runs to success (a rejection,
  execute error, suspension, or identical-plan skip never arms it — the model keeps seeing feedback
  and re-plans), and the next `plan()` renders the final message from the actual results in one
  cheap toolless call, ending the turn through the loop's existing `case "chat"` — no change to the
  loop contract, so ejected/custom loops keep working. The render call's tokens land in the turn
  totals and per-call attribution; a render failure emits a `completion.render_error` observation
  and falls back to a normal planner round rather than aborting a turn whose work already
  succeeded. The plan value's `complete` field is now the directive object (or `null`) instead of a
  boolean.

- **The session-symbols digest is bounded (A-07).** The symbols block — the uncached trailing
  system segment re-sent on every planner call — rendered one line per visible/pinned session
  symbol with no cap, so a long session's per-call context grew without eviction (conversation
  compaction never touched it). The renderer now caps it at 64 lines with a 10k-char backstop:
  pinned symbols always rank ahead of visible ones, the store's newest-updated-first order is kept
  within each tier, an oversized summary is dropped-and-continued (L-08 precedent) instead of
  evicting everything after it, and a trailing marker counts the omissions — omitted symbols stay
  fully referencable by `$name`; only the digest is capped, `FlowStore::view` (symbol resolution,
  context budgeting) stays complete. `FLUX_SYMBOLS_CAP` overrides the line cap (`0` disables).
  Both the loop planner's segment C and the one-shot compiler go through the same bounded renderer.

- **Planner teaches `parallel` for independent reads; one-shot compile repairs hit the prompt cache
  (A-09).** The grammar's canonical worked example for "read a.rs, b.rs and c.rs and summarise
  each" emitted a serial `each` — which executes strictly in order — right after a hint saying to
  prefer `parallel`, so every model learned the slow shape. The grammar now carries three examples
  (sequential dependency chain, `parallel` with named branches for independent reads, and `each`
  scoped to dynamic lists, where `parallel`'s static branches can't apply), and a test parses every
  worked example against the real `DraftAst` schema so the prompt can never drift from the AST
  again. The one-shot `compile()` repair loop used to re-send the entire flat prompt (instructions
  + catalog + grammar + previous output) uncached on every attempt; it now mirrors `compile_turn`'s
  cache-first layout — instructions+catalog+grammar as ONE byte-stable cached system segment,
  session symbols uncached, and the repair exchange riding as ordinary messages — so attempt 2+
  re-reads the bulk from the provider prompt cache instead of re-paying it.

- **AWS Bedrock LLM provider — `flux run -m aws`, streaming, full credential chain, priced (C-09).**
  New `aws` provider (`flux_providers::bedrock`, L1) drives Bedrock-provisioned Claude through the
  same agent harness: the wire is the Anthropic Messages shape flux already speaks, wrapped in a
  thin codec that moves `anthropic_version` into the body and signs every request with hand-rolled
  **SigV4** (pinned by known-answer tests; no AWS SDK crates anywhere). Turns stream over
  `invoke-with-response-stream`: `map_bedrock_event_stream` deframes AWS's binary event-stream
  (CRC-32-validated frames, split-safe buffering across HTTP chunk boundaries, exception frames
  surfaced as errors) into SSE for the shared `map_messages_stream` mapper — the interim
  non-streaming `invoke` path was deleted, streaming is the only wire. Credentials resolve through
  a hand-rolled AWS default chain — static env → **SSO** (reads `~/.aws/config` +
  `~/.aws/sso/cache`, refreshes expired access tokens via SSO-OIDC `CreateToken` and persists them
  back 0600-atomic) → **IRSA** (`sts:AssumeRoleWithWebIdentity`) → **EKS Pod Identity** — so dev
  (`aws sso login` + `AWS_PROFILE`) and k8s-injected prod both work with no `aws` CLI and no
  manual `export-credentials` step; the `BedrockCredentialsResolver` trait is the swappable seam,
  and the resolved chain is materialized into env once so every sync path (REPL `/model`,
  sub-agent factory, server) builds the provider. Model aliases resolve region-aware
  (`aws`/`aws/sonnet` → `us.`/`eu.` cross-region inference profiles by `AWS_REGION`; `aws/haiku`
  → the `global.` profile), and pricing keys the **region-less** Bedrock id with a routing-prefix
  strip in `rates_for`, so every regional profile prices identically (metered, `$` suffix).
  Live-verified e2e (dev SSO, eu-central-1): streaming turn, tool-use turn, and the haiku
  `global.` profile, each with the cost suffix; expired-token OIDC refresh confirmed against the
  live portal. README provider table + `-m` CLI help document the auth modes.

- **Usage & cost accounting — attribution, sub-agent rollup, `cost_summary`, `flux usage`, server
  endpoint, cache-aware surfacing (C-06).** Turns C-05's captured tokens into the full user-facing
  surface. New `EventKind::CallUsage { model, usage }` (`flux-events`) records EVERY provider call
  individually, stamped with the model active at call time, so a mid-turn model switch attributes
  tokens/cost correctly (`EngineLoopHost::turn_calls`, appended by `FlowEngine` right before
  `end_turn`); `TurnEnded.usage` stays as the turn-total back-compat field. A spawned sub-agent's
  token spend now reaches the parent turn's total: `flux_runtime::Spawner::spawn`/`spawn_scoped`
  return a `SpawnOutcome { text, model, usage }` (was a bare `String`); `TaskTool` records the
  child's usage as a `subagent.usage` observation on the shared evidence log (the same
  snapshot-then-diff side-channel `turn.iteration` already uses), and the engine folds it into the
  parent's total AND emits a `CallUsage` attributed to the sub-agent's own model. New
  `flux_events::cost_summary` projection (+ `EventStore::cost_summary`/`cost_summary_all`) rolls up
  tokens + cost by model — every tier, cache-aware, reasoning priced — preferring per-call
  `CallUsage` attribution and falling back to per-turn totals for logs written before this shipped.
  New `flux usage` command reports per-model tokens + cost for the latest session and an
  all-sessions total. New server routes `GET /sessions/:id/usage` + `GET /usage`; `POST
  /sessions/:id/messages`'s usage JSON no longer drops the cache/reasoning tiers. CLI
  `usage_annotation` and the TUI header now surface cache-write + reasoning tokens (TUI also gained
  a running dollar-cost figure via a new `ChatState::with_cost`).

- **Strict review: app journey + `flux review` CLI + CI output modes (L-13, Phase 4).** Strict review
  is now a product surface, with ONE shared `strict_review` definition consumed identically by both:
  `crates/flux-app/src/review.rs` embeds the checked-in `examples/strict_review.flux` via
  `include_str!` and wraps its parsed `DraftAst`, unmodified, as a `strict_review` composite op —
  never a second hand-maintained copy. `flux_app::App` grew `with_sub_agents` (a `TaskTool` +
  `SubAgents`-built spawner installed on every journey run's executor, mirroring
  `FlowClient::with_sub_agents`), so the new `review_code` journey (pure plumbing: `return
  strict_review(files: $files)`) can delegate to the bounded 3-role reviewer fan-out via `task`.
  `flux app run strict-review` (a built-in program name) runs it. The new `flux review --files
  <path>… [--format md|json] [--fail-on <severity>]` CLI command wires roles + sub-agents exactly like
  `build_agent` (shared `build_review_sub_agents` helper) and runs the same embedded flow text through
  `flux_sdk::FlowClient::run_flow` — printing a markdown findings summary (default) or raw
  `ReviewReport` JSON, and exiting 1 when a finding's severity meets `--fail-on` (`info|low|medium|
  high|critical`; an unrecognized severity string fails safe as `critical` rather than silently
  bypassing the gate). Self-contained: the built-in reviewer roles + the embedded flow text ship in
  the binary, so `flux review` works in any repo (a project's own `.flux/agents/review-*.md` still
  overrides). The strict-review core stays read-only — no write/network/publishing effect was added.
  New headline test `crates/flux-app/tests/strict_review_journey.rs` asserts the journey path and the
  direct `FlowClient` path produce the byte-identical `ReviewReport` for the same inputs (added RED,
  made GREEN); 10 new `flux-cli` unit tests cover the `should_fail` exit-code decision and markdown
  rendering.

- **Strict review: typed artifacts + deterministic aggregator — `review.normalize`/`review.aggregate`
  (L-12, Phase 3).** `examples/strict_review.flux`'s aggregation tail (`merge` → `filter` → `dedupe` →
  `sort` over a **model-emitted** `rank`) is replaced by a single deterministic native call,
  `review.aggregate({ findings, files, reviewers })`, returning a typed `ReviewReport`. Two new ops in
  `crates/flux-tools/src/cognition.rs` (registered in `register_cognition` + the `cognition` tool
  group), with `schemars`-derived embedded schemas (`ReviewFinding`/`ReviewReport` — prelude-type
  promotion deferred to when a second surface consumes them): `review.normalize({ findings })` parses
  raw reviewer entries into well-formed findings, computing a stable fingerprint
  (`category+file+line+normalized-title`, hashed with a fixed-key `DefaultHasher` so it never depends
  on `HashMap` iteration order) and quarantining malformed entries as human-readable `gaps` strings —
  never silently dropped, never surfaced as findings. `review.aggregate` runs normalize, dedupes by
  fingerprint (counting distinct reviewers as `agreement`, keeping the max `confidence` across the
  group), and ranks severity desc → confidence desc → agreement desc → fingerprint asc (a stable
  tiebreak, so ordering is byte-identical across runs). The three reviewer roles
  (`.flux/agents/review-{security,correctness,maintainability}.md`) no longer emit
  `fingerprint`/`rank` — the aggregator computes both now, not the model. The L-10 integration test
  (`crates/flux-sdk/tests/strict_review.rs`) is updated: a cross-reviewer duplicate now asserts
  `agreement == 2`, the malformed mock entry asserts a `gaps` entry instead of a silently-dropped
  finding, and ranking is asserted severity → confidence → agreement. 3 new failing-first unit tests
  in `flux-tools` (stable-ordering + malformed→gap, fingerprint stability/distinctness +
  duplicate-collapse, ranking-order disambiguation).

- **Runtime-enforced capability scoping — `with_tools` (L-11, Phase 2).** Per-block tool restriction
  is now enforced, not advisory: a new `Node::CapScope` AST node (native text
  `with_tools ["a", "b"] { … }`) narrows the tool-name allowlist for its body, checked as the FIRST
  gate in `Executor::dispatch` (before pre-tool hooks and the policy/permission layers) — a call
  outside the active scope fails closed even when the outer session policy would allow it. The gate
  runs on every dispatch, so a composite op's inner calls and a sub-agent's own dispatches are caught
  identically to a direct call (non-bypassable). Capabilities only ever narrow on descent: nesting
  intersects with the enclosing scope, and `Spawner::spawn_scoped` intersects a `task` role's own
  `tools` with the caller's active block scope (`role.tools ∩ active_block_scope`), wired through
  `TaskTool` reading `ToolContext::active_cap_scope()`. Scope entry/exit and every denial are recorded
  in the evidence log (`cap_scope_enter`/`cap_scope_denied`/`cap_scope_exit`). The analyzer also flags
  a literal-op `call` statically provable to be outside its enclosing `with_tools` scope. Resolves the
  design's two Phase 2 open questions: scopes narrow by tool **name** (not effects), and sub-agent
  restriction is a **surrounding block scope**, not a `task(tools:)` parameter.

- **Strict code review as a checked-in Flux-Lang flow (L-10, Phase 1).** `examples/strict_review.flux`
  gathers context read-only (`git_status`/`git_diff`/`read_many`), packs it into a budgeted `ctx` for
  the audit trail, fans out via `parallel` to a FIXED set of three restricted reviewer roles
  (`.flux/agents/review-{security,correctness,maintainability}.md`, each `tools: []` — no filesystem/
  shell access, JSON-only output contract), then aggregates deterministically: `merge` → `filter`
  (quarantines malformed entries) → `dedupe` (by `fingerprint`) → `sort` (by `rank` desc) → a
  structured report. Proves the strict-review protocol shape (design
  `docs/designs/strict-review-flows.md`) using only existing primitives — no language/runtime
  changes. New integration test `crates/flux-sdk/tests/strict_review.rs` drives the real flow + role
  files through a mock sub-agent provider, asserting bounded fan-out (exactly 3 `task` calls),
  cross-reviewer dedup, malformed-entry quarantine, and stable ordering across runs.
  - Fixed two narrow upstream bugs surfaced while building this flow: the `flux-lang` analyzer now
    accepts a lone `obj` **template** argument (a dynamic field, e.g. `task({role: "x", task:
    $prompt})`) as the named-input map, matching how the runtime already treats it — previously only
    a lone literal object was exempt from the "ambiguous bare value" arity check. The `flux-flow`
    planner's prose-embedded-AST fallback now requires a non-empty plan `body`, so a sub-agent's
    JSON-array reply (as this protocol requires reviewers to emit) is no longer misdetected as an
    empty no-op plan merely because it contains a balanced `{…}` substring.

- **sql/asterisk per-call read timeout is now wire-enforced (D-45).** The `timeout` input (parsed
  since D-40/D-41 but previously discarded) is now plumbed through the host `conn.read`
  (`timeout_ms`) and `host-kit`'s `ConnStream::set_read_deadline`, surfacing
  `std::io::ErrorKind::TimedOut` instead of a silent hang when a PostgreSQL or AMI server stops
  responding. The connection stays open on timeout (the plugin decides to retry or close). 5
  failing-first tests (1 live-loopback host test + 2 sql + 2 asterisk).

- **`flux plugin call/run --arg` — schema-coerced plugin op invocation (Track A1).** `flux plugin
  call <name> <op> [json-input]` (now aliased `run`) accepts repeatable `--arg key=value` flags,
  coercing each value to the op's declared `input_schema` type (string/integer/boolean/array/
  object/enum — resolving schemars' `$ref`/`anyOf` nullable forms) and merging over the
  `<json-input>` base. `--dry-run` validates locally against the schema and prints the coerced
  input + problems without calling the op (mirrors the fluxplane `operation invoke` ergonomics);
  `--no-validate` skips coercion and passes args through as strings. Live-smoke-verified against
  a migrated plugin's schemars-derived schema. 7 flux-cli unit tests.

- **Plugin protocol: host-only ops + path-scoped `fs.read` capability (C-09a).** Two
  deny-by-default L4 surfaces for the `aws-bedrock` plugin (C-09b) to resolve the AWS credential
  chain without an `aws` CLI. (1) `OperationSpec::internal` — an op marked `internal: true` is NOT
  advertised to the LLM as a callable tool (the aws-bedrock `auth` op returning raw keys is the
  canonical case; the model must never call it). It stays host-dispatchable via the shared
  `PluginHost` handle; `visible_ops(manifest)` is the single projection filter. Defaults `false`
  (no behavior change for existing manifests). host-kit gains `internal_op`/`internal_op_typed`.
  (2) `PluginCapabilities.fs: Vec<FsReadScope>` — path-scoped deny-by-default reads of HOST files
  outside the workspace jail (`~/.aws/config` + `~/.aws/sso/cache`): globs (`/**` nested, `/*`
  children, exact), `~`→`$HOME`, `..` rejected, 256KB cap, `secret: true` scopes
  Redactor-registered (the SSO refresh-token privilege boundary). Binary → `body_b64`, text →
  `body`. Each ships with failing-first tests (internal op absent from the tool catalog; out-of-scope
  `fs.read` denied; `..` rejected; secret scopes redactor-registered).

- **AWS Bedrock: region-aware model resolution (C-09b fix).** `resolve_model` now picks the
  cross-region inference-profile prefix (`us.`/`eu.`) from `AWS_REGION` — `us.anthropic.*` is
  invalid in `eu-central-1` (Bedrock 400 "The provided model identifier is invalid"), and the
  SSO chain resolves the region from `~/.aws/config`. Haiku stays `global.` (a global profile).
  Fixed `flux run -m aws` failing on real agentic turns in non-us regions (the `say ok` smoke
  had masked it by setting `AWS_REGION=us-east-1` explicitly).

- **AWS Bedrock: full credential chain (no `aws` CLI) (C-09b).** `flux run -m aws` now works
  with SSO / IRSA / EKS Pod Identity — no `aws configure export-credentials` dance, no `aws` binary.
  The chain (env → SSO → IRSA → EKS Pod Identity) is hand-rolled in `flux-providers::bedrock` over
  direct `std::fs` + `reqwest` (the flux-credentials precedent — credential bootstrap is a separate
  trust boundary from agent-tool IO). SSO: reads `~/.aws/config` + `~/.aws/sso/cache/<sha1(session)>.json`,
  refreshs expired tokens via SSO-OIDC `CreateToken` (JSON/camelCase, persisted back 0600 atomic),
  calls `GetRoleCredentials` (access token in the `x-amz-sso_bearer_token` header). IRSA:
  `AssumeRoleWithWebIdentity`. EKS Pod Identity: `AWS_CONTAINER_CREDENTIALS_FULL_URI`. The CLI
  resolves once in `build_agent` (async) into `AWS_*` env so every sync path (REPL `/model`,
  sub-agent, server) reads the resolved creds.

- **AWS Bedrock LLM provider — L1 core + CLI routing (C-09, in-progress).** `flux run -m aws` now
  drives Bedrock-provisioned Claude through the same harness. The load-bearing reuse: Bedrock's
  `invoke-model` on an Anthropic model returns **native Anthropic Messages JSON** — the exact shape
  flux's `messages` codec already speaks — so the wire codec is a thin wrapper. The new
  `flux_providers::bedrock` module (L1, no AWS SDK deps) hand-rolls **SigV4 signing** (~150 lines,
  pinned by two known-answer tests cross-verified against an independent Python `hmac` impl) over an
  injected `BedrockCredentialsResolver` trait — the seam that lets the credential source swap without
  touching L1. The shipped stand-in is `EnvStaticResolver` (reads `AWS_*` env); per the design's
  **Option C decision**, an `aws-bedrock` plugin embedding `aws-config` (full SSO / IRSA / EKS Pod
  Identity chain, no `aws` CLI in prod) replaces it at this trait in a follow-up (C-09a/b). Non-streaming
  `invoke-model` ships first (one Messages JSON object → `Chunk`s); event-stream streaming (C-09d) and
  the plugin (C-09a/b) follow. Live-verified against the dev account: `say ok`→"ok" ($0.1522 metered),
  the `opus` alias, and a real read-file tool-use turn. Three live-caught bugs (unit tests couldn't):
  Bedrock rejects `model`+`stream` in the body; the model id was empty in the URL (resolution ran
  after credential construction); the L0 `known_provider` pricing mirror didn't include `aws`.
  Pricing: `aws/anthropic.*` entries (match direct Anthropic rates, metered not subscription).

- **homer `call.analyze` parity port (D-37).** `homer.call.analyze` now does the full
  multi-leg correlation analysis ported from the fluxplane reference: seed by `call_id` **or**
  `from_user`+`to_user`, fan out by the seed caller + extra `numbers`, confirm legs by a shared
  `correlation_header` value + temporal overlap, and additionally by involving an extra number.
  New params: `from_user`, `to_user`, `numbers`, `headers`, `limit` (`render` advertised for
  parity, SVG deferred). Result matches fluxplane's `CallAnalyzeResult` (legs with `matched_by` /
  `correlation` / `headers` / `duration`, `leg_count`, merged `events` + `ladder`). Failing-first
  MockHost tests cover the from/to-seed + correlation path and the number-matching path.
  `MockHost::with_http_seq` added (host-kit) for repeated-URL test scenarios.

- **gitlab schemars op schemas (D-36).** All 64 gitlab op `input_schema`s are now schemars-derived
  via `host-kit::read_op_typed::<T>` / `write_op_typed::<T>` instead of hand-written
  `so(json!{...}, json![...])` literals; the local `so` helper is deleted. Handlers unchanged
  (schema-only structs, D-34 precedent). A contract test asserts all 64 derived schemas match the
  legacy `so(...)` contract (fields / required / types); `gitlab` added to the
  `no_manual_plugin_schema` guard. Fluxplane parity re-audit deferred (D-14 ported the surface).

- **slack schemars op schemas (D-36).** All 30 slack op `input_schema`s are now schemars-derived
  (slack used inlined `json!({"type":"object",...})`, not a `so()` helper). The
  `no_manual_plugin_schema` guard now flags **both** hand-written shapes (`so(json!{...})` and
  inline `json!({"type":"object",...})`), verified failing-first for both. Contract test locks
  all 30 schemas. Fluxplane parity re-audit deferred (D-14 ported the surface).

- **gitlab fluxplane parity ports (D-38).** Closed the gaps the D-36 re-audit surfaced: list-op
  pagination/filter (`project.list`/`mr.list`/`issue.list`/`pipeline.list` got `limit`/`query`/
  `order_by`/`sort` + per-op filters), `index.build` selector surface (index just `projects` /
  `issues` / `merge_requests`), `repository.file.show` `max_bytes` + `search.blobs`
  `max_data_bytes` byte caps, and the `mr.merge` `remove_source_branch` drift. Failing-first
  tests; 44 gitlab tests.

- **slack fluxplane parity ports (D-39).** Closed the re-audit gaps: **Block Kit messaging**
  (`message.send`/`message.edit` gained `markdown`/`blocks`/`unfurl_links`/`unfurl_media`/`parse`,
  `text` relaxed to optional — the model can now send Block Kit messages), `message.list`/`thread`
  `text_format`, `search`/`mentions` ticket extraction + `mentions` `bot`, `file.upload`
  `content_bytes` (base64 inline) + `alt_text` (was dead) + `blob_ref` relaxed, `file.download`
  `blob_ref` seed, and `query`/`limit` filters on the list ops (`emoji.list` also `mode`/
  `include_aliases`). `schema_contract` gained `Kind::ArrayStr`. Failing-first tests; 65 slack tests.

- **sql schemars migration + timeout parity (D-40).** All 7 sql op schemas schemars-derived via
  `read_op_typed::<T>`; shared connection fields factored into a `ConnProps` struct embedded with
  `#[serde(flatten)]`/`#[schemars(flatten)]`, `Driver` as a derived enum; `so()`/`merge()`/
  `conn_props()` helpers deleted. Fluxplane re-audit ported the missing `timeout` param (default
  10s, Go-duration) to all 7 ops, parsed/validated in `resolve_target`. Honest limitation: the host
  `conn.*` capability exposes no per-call timeout, so it's validated but not enforced as a
  deadline. 17 sql tests.

- **asterisk schemars migration + AMI parity ports (D-41).** All 8 asterisk op schemas
  schemars-derived (shared `AMIConn` via flatten; `Risk::Destructive`/`High` preserved). Fluxplane
  re-audit ported: `timeout` (all ops, validated), `call.originate` `early_media`/
  `channel_id`/`other_channel_id` AMI fields, `peer.list` output `comment`, `ami.ping` output
  `duration_ms`. 12 asterisk tests.

- **Observability cluster schemars migration + parity (D-42).** grafana (21 ops), prometheus
  (16), loki (9), alertmanager (7) all schemars-derived in parallel; `so()`/inline helpers
  deleted; `schema_contract` tests lock all contracts. Fluxplane re-audit per plugin;
  prometheus `.test` now checks `/-/ready` status (`ready`/`error`/`latency_ms`). MockHost
  gained `with_http_status_body` (host-kit) for error-path tests. 21/16/19/7 tests.

- **Medium cluster schemars migration + parity (D-43).** huggingface (9 ops), opsgenie (8),
  docker (33, largest of the batch) all schemars-derived in parallel; `so()` helpers deleted;
  `schema_contract` tests. Fluxplane parity ports: huggingface `chat.stop`/`embed.input` enforced
  as `[]string`; opsgenie `401`/`403` auth-rejection message + `Accept: application/json` header;
  docker `system.df` `types` filter, `container.top` `args` array, `container.restart` `signal`,
  `container.create`/`run` `mounts`/`open_stdin`/port `protocol`, `network.create`
  `scope`/`ingress`/`enable_ipv4`/`enable_ipv6`, list-op `limit` caps. Docker's
  streaming/hijack/tar ops (`container.exec`/`stats`/`copy_*`, `image.push`/`build`, `events`,
  `context.*`) flagged as a residual scope boundary. 19/11/42 tests.

- **Final cluster schemars migration + parity (D-44) — D-36 COMPLETE.** websearch (2 ops), jira
  (21), confluence (15), kubernetes (24), aws (11) all schemars-derived in parallel. **All 17
  in-repo plugins now derive every op `input_schema` via `host-kit::read_op_typed`/
  `write_op_typed`; the `no_manual_plugin_schema` guard (all 17) enforces it.** Fluxplane parity
  ports: websearch `limit`/`queries`/`NormalizeMax`; jira `body_format` (ADF→Markdown) +
  `fields`/raw `fields`/`update` + `content_bytes`; confluence `content_bytes` + pagination
  tokens + error extraction; kubernetes `query`/`limit` inventory filters + `pod.logs` `until` +
  `deployment.scale`/`restart` + `portforward.start` `duration_seconds`; aws
  integer→RFC3339 timestamp formatting + `latency_ms`. Residuals (docker streaming ops,
  confluence `index.build` paging, aws `inspect` env lookup, jira ADF-image) flagged for future
  passes. 8/34/38/42/22 tests.

- **Plugin-side schemars op schemas — D-36 (in-progress).** `host-kit` now derives a plugin
  op's `input_schema` from a typed struct via `read_op_typed::<T>` / `write_op_typed::<T>`
  (+ `op_input_schema::<T>()`, a `schemars` re-export), the plugin-side counterpart of D-34.
  `homer` is the first migrated plugin (8 ops): its hand-written `so(json!{...}, json![...])`
  op schemas are gone, replaced by `#[derive(Deserialize, schemars::JsonSchema)]` structs
  (handlers unchanged — schema-only, the D-34 precedent). A contract test asserts each derived
  schema's fields/required/types/enums match the legacy `so(...)` contract, and a workspace
  guard (`plugins/host-kit/tests/no_manual_plugin_schema.rs`, scoped to `MIGRATED_PLUGINS`) fails
  on a reintroduced `so(json!{...})`. Drift the migration surfaced is recorded in `docs/archive/drift-reports.md`
  (two `homer` drifts, preserved as-is — pure schema-source change, not a contract change).
  16 plugins remain under D-36.

- **Codex provider hardening + provider-owned model resolution (C-03).** The ChatGPT-subscription
  `codex` provider is now correct against the live backend's quirks, verified by a real
  `~/.codex/auth.json` smoke test (no token values committed). A live turn surfaced one bug the unit
  tests couldn't: `-m codex/gpt-5-codex` died with HTTP 400 ("`gpt-5-codex` is not supported when
  using Codex with a ChatGPT account") — the backend serves the `gpt-5.5` family; the `*-codex` ids are
  legacy. `codex` now has its own provider module (`flux_providers::codex`) owning `DEFAULT_MODEL`
  (`gpt-5.5`) and `resolve_model` (empty/`*-codex` → `gpt-5.5`, else pass-through), so bare `codex`,
  legacy `codex/gpt-5-codex`, and `codex/gpt-5.5` all resolve and complete. The `anthropic`/`claude`
  alias resolution moved out of the CLI into `flux_providers::anthropic::resolve_model` so every
  surface (CLI/SDK/server/TUI/L3 sub-agent spawner) shares one owner instead of each carrying its own
  alias table — the CLI keeps only the bare-`codex` shorthand policy. (The cache + reasoning token
  capture in the Responses codec was already present and is now live-verified: `cache 5.1k (15% hit)`.)

- **Claude/codex force-refresh-on-401 (C-04).** OAuth-backed subscription providers refreshed
  tokens on expiry time only — a stale or wrong expiry re-sent the same dead token and failed the
  turn. `NativeProvider::stream` now reacts to a 401 on an OAuth credential by forcing exactly one
  token refresh (`Credential::token_source()` / `TokenSource::refresh()` seams) and retrying once;
  a second 401 surfaces the error, and 5xx/429 keep the ordinary backoff without spending the
  refresh grant. `RefreshingToken`'s forced path ignores the expiry buffer, persists the freshened
  token, and coalesces a concurrent burst of 401s into one refresh. Landed with C-03/C-05 in
  `c2cd360`; this entry and the story's `done` status were back-filled after a tracking audit
  found the frontmatter stale.

- **Live per-turn cost in the CLI annotation + codex pricing fix (C-05).** The cross-provider cost
  model (`flux_core::pricing`: per-model per-tier rates, `cost(&Usage, model)`, `~/.flux/pricing.toml`
  overlay, subscription labelling) was shipped but never *wired* — `cost()` was dead code and the CLI
  showed tokens (`ctx`/`out`/`cache`) but no dollar cost. It is now live: `flux run` loads the pricing
  table and appends a per-turn cost suffix to the turn-end rule — `· $0.0023` for metered spend
  (`anthropic`/`openai`/…), `· ~$0.0359 (sub)` for subscription spend (`claude`/`codex`, shown as
  *equivalent metered cost* since it bills against a flat sub, not the API). To wire it, `build_agent`
  now returns the canonical resolved `provider/model` spec (the raw input may be a bare alias like
  `codex` that neither `is_subscription` nor `rates_for` can decode) and `CliSink` carries the spec +
  table. The live smoke also caught a real regression from C-03: the pricing table keyed codex on the
  legacy `gpt-5-codex` id, but the C-03 resolver emits `gpt-5.5` — so codex spend priced as `None`
  (zero). Fixed by keying `gpt-5.5` (keeping `gpt-5`/`gpt-5-codex` as defence-in-depth aliases). A live
  codex turn now shows `· ~$0.0359 (sub)`. The fuller reporting surface (per-call attribution,
  `flux usage`, server endpoint) is C-06.

- **Complete `flux plugin` lifecycle — `uninstall` + `status` (D-19).** Plugin management is now
  fully first-class from the CLI. `flux plugin uninstall <name>` removes the descriptor at
  `~/.flux/plugins/<name>.toml` (a missing name is a clean `no such plugin — nothing to uninstall`, never
  a panic). `flux plugin status [<name>]` reports, per plugin, the resolved binary path, a **liveness**
  probe — `ok` / `missing` / `unloadable` — the version, the pin state, and the declared surface from
  the manifest (op count, auth purposes, endpoints, datasources, `discovers`, requested capabilities).
  A missing binary is detected **without spawning a process** (a path/`PATH` existence check); a present
  binary is spawned and its manifest loaded through the same guarded `PluginHost::spawn` boundary `call`
  uses, so a bad-but-present binary is `unloadable`, never a crash. With no argument it summarizes every
  installed plugin; `ls` stays the terse default. New `flux_plugin::remove_descriptor` helper backs it.

- **App-runner ergonomics for declarative bots (D-11).** `flux app run` is now a viable host for a
  real declarative bot, not just a demo. An agent's persona can live in files: an `agent` declaration's
  `settings.system_prompt_files` (a list of workspace-relative paths) is read through the guarded,
  workspace-confined `System` and concatenated after the base persona — so a bot keeps a long
  `bot/PERSONA.md` in a file instead of inlining it into `bot.flux` (a non-string entry or an unreadable
  path is a clean, attributed error). Reading the persona files made the agent spec/engine-build chain
  async; the per-agent engine cache is built off-lock so no lock is held across the file read. This
  completes the two ingest/event gaps that had landed earlier without tests — the program-declared
  `datasource` ingest already covers markdown **and** OpenAPI JSON (`build_datasources` → the existing
  `ingest_openapi`), and an event-woken agent already receives synthesized event context (the firing
  trigger's label + payload, e.g. a schedule tick's `at`) as its turn input when the event carries no
  user text — both now locked by tests.

### Fixed

- **Program-declared secrets now seed the redactor at resolution (C-13).** `flux_app::resolve_secrets`
  resolved `secret "NAME"` markers to plaintext without ever registering the values, so a Slack bot
  token (or any declared secret) leaking into tool output passed unredacted unless it happened to
  match a credential-shape heuristic — despite README's "secrets are scrubbed from all tool output
  and logs". `resolve_secrets(program, &redactor)` now `add_secret`s every resolved value at the
  moment of resolution, and `App::with_sub_agents` threads that ONE redactor into both executor-build
  paths (journey runs and agent-target engines) via `ToolContext::with_redactor` — redactor clones
  share the value store, so the CLI, the app host, and the plugin secret sink all scrub with the same
  set. Provider env-key seeding is unified behind the new `flux_credentials::provider_env_keys()`
  (replacing the CLI's hardcoded 4-key list), which also covers `AWS_SECRET_ACCESS_KEY`/
  `AWS_SESSION_TOKEN` — the Bedrock credential chain materializes those into the process env, so an
  `env` dump in tool output is scrubbed now too. The `flux app run` path previously seeded nothing.

- **Plan approval sees the plan's real intents — the sub-agent destructive bypass is closed (C-12).**
  `Approver::request_plan` used to forward an **empty** `IntentSet`, and an approved plan scope
  suppressed every per-op gate — so a sub-agent's destructive op, arriving via its only tool
  (`emit_plan` → `run_plan` → `approve_plan`), sailed past `SubAgentApprover`'s destructive-deny
  blind, breaking README's "sub-agents cannot approve destructive operations". `request_plan` now
  takes a `PlanApprovalRequest { summary, ops, destructive, mutating, intents }` (breaking trait
  change, clean cutover) built by `PlanRisk::approval_request()`; `plan_risk` folds each call node's
  `tool.intents(...)` into the aggregate. `SubAgentApprover` denies any plan that is destructive or
  carries a destructive intent — proven on the real emit_plan path by a new orchestrate test. The
  dynamic-arg hole is closed too: a `destructive_scope` disclosure bit rides beside the approved-plan
  scope, and a destructive op that was **not** statically visible at plan-approval time re-fires the
  approval gate even inside an approved scope (interactive approvers prompt, `--yes` allows,
  sub-agents deny) — **this deliberately also holds under "always"/trust_all**, a behavior change:
  previously a trusted session never re-prompted; now a plan that smuggles a destructive op through
  dynamic args gets one more explicit prompt. Disclosed destructive plans run without any per-op
  re-prompt (regression-tested), and the REPL's `/run` computes the reviewed AST's risk so its
  approval discloses correctly. `AllowApprover` keeps allowing destructive plans at top level (human
  opt-in via `--yes`/server); its docs now state it must never be installed for sub-agents.

- **Plugin descriptor names are sanitized against path traversal (D-35).** `flux plugin
  uninstall` (and `add` / `pin` / `rollback` / `status`) fed the user-typed plugin name straight into
  `Path::join` for `<dir>/<name>.toml`, and `remove_descriptor` handed the result to
  `std::fs::remove_file`. `Path::join` treats `..` and absolute components literally, so a name like
  `flux plugin uninstall ../../config` resolved to `<dir>/../../config.toml` and an absolute name
  replaced the base entirely — a destructive delete of a file that was never a plugin descriptor.
  The single `descriptor_path` seam now validates the name first (no path separators, no `..`/`.`
  component, no absolute/Windows-prefix component, not empty) and returns a clean `Err` before any
  filesystem op, covering `add_descriptor` / `load_descriptor` / `set_pinned` / `remove_descriptor`
  from one guard. Legitimate names (alphanumeric, `-`, `_`, `.`) are unaffected. Surfaced by an xhigh
  review of D-19 (`27b1c10`), which shipped the unsanitized `uninstall`.

- **Endpoint discovery resolves cluster aliases and relays structured `cluster`/`namespace` (D-33).**
  The `endpoint.discover` spine no longer requires the agent to hand-recover a wrong cluster. A short
  `cluster` alias (e.g. `dev`) is resolved against kubeconfig context names (case-insensitive
  substring; exact match wins; an ambiguous >1 or unknown 0 match is a loud error, never a silent
  empty result) — fixing the s_251 failure where `"dev"` was not a real context (the real ones are
  long ARN-like names) and was either passed literally to `kubectl --context` (→ kubectl error) or,
  through the broker, never set `context` at all. The agent-facing `endpoint.discover` op and the
  broker now carry structured `cluster`/`namespace` fields, and the broker parses `cluster=<x>`/
  `namespace=<y>` tokens out of free-text `query` (explicit params win; tokens stripped from the
  forwarded query) so the structured path is reachable without hand-parsing. The `namespace=latest`
  ambiguity is also fixed: the free-text `query`-substring "newest namespace" heuristic is retired —
  only `latest_namespace: true` triggers it, and a literal namespace named `latest` is just
  `namespace: "latest"`. See [docs/archive/designs/session-s251-postmortem.md](docs/archive/designs/session-s251-postmortem.md).

- **`ctx` context packs no longer drop the working set on a single oversized member (L-08).** The
  `ctx`/`ctx_append` packer in `flux-lang` packed members greedily with a hard `break` on the first
  overflow, so one oversized early member (e.g. a 493k-char session-evidence bind) evicted every
  smaller member after it — even ones that would have fit in the leftover budget — starving the
  `ai.reason` step of the very evidence the flow had just gathered (the session `s_251` reasoning
  death spiral). It now **drops the oversized member and continues packing** the rest, while keeping
  the visibility-tier priority (`Pinned` > `Visible` > …, no rank inversion). Prompt caching is
  unaffected: the `Ctx` value is a per-iteration tool result, never the cached system prompt. See
  [docs/archive/designs/session-s251-postmortem.md](docs/archive/designs/session-s251-postmortem.md).

### Changed

- **Board is now generated from story frontmatter (`track` plugin).** `docs/stories/README.md`'s
  status lists (Now / Next / Blocked / Backlog / Done) are regenerated from each story's
  frontmatter by the [`track` plugin](https://github.com/codewandler/agentplugins)
  (`/track:board`), delimited by `<!-- BEGIN/END track:board -->` markers; the hand-written intro
  and `## Status` summary stay outside the markers. This retires flux's manual "keep this board in
  sync" upkeep. Story frontmatter gained two optional fields — `epic:` (design-doc slug; the board
  groups backlog/ready rows under the epic, with a one-line blurb from the design doc's `## Why`)
  and `note:` (a one-line row annotation) — and the prior per-story board one-liners were moved into
  `note:` so the source of truth is the story file. Frontmatter drift surfaced by the migration was
  reconciled: D-02/D-03/D-12 set to `done` (board + commits already showed them shipped), D-11 set
  to `ready` (the board's "Next" pick), I-01's `priority: P1` normalized to `1`, and D-18 — which
  was missing from the hand-written board — now correctly appears. Epic narrative prose that lived
  on the board is summarized by the generator's blurb; the full narratives remain in each epic's
  design doc.

### Added

- **Endpoint lifecycle — refresh runner, operator CLI & audit (D-30).** The final step of the
  endpoint-discovery epic. `EndpointBroker::refresh` re-runs the cross-plugin fan-out per product and
  reconciles each discovering provider's set via `EndpointRegistry::replace_owned` — stale entries
  dropped, fresh ones inserted, other owners' records left untouched — driven on demand by an
  `EndpointRunner::tick` (no always-on ticker: it would contend with the agent's own plugin-host
  locks). A new `flux endpoint` CLI (`list`/`show`/`resolve`/`import`) renders the persisted
  `~/.flux/endpoints.toml` store as weak references + health + the credential *location*, **never** a
  secret value (pinned by `cli::endpoint_list_redacts`); the agent-facing `endpoint.import` op
  persists a weak ref across sessions. A new `EndpointDiscovered` audit event fires per provider on
  both `discover` and `refresh` (product/provider/count only — no URL, no secret). The epic core
  (D-25 → D-30 + D-20) is complete; D-31 (host-terminated raw-socket auth) and D-32 (retire the
  `host.endpoint` URL-handback) are filed as backlog hardenings.
- **Reference-based plugin IO migration (D-29).** Plugin operations now do their primary IO through
  references rather than holding URLs: `SystemHostCaps` resolves a named manifest endpoint locally (env
  binding stays a host-side resolver default) and a discovered `@endpoint/<id>` through the broker;
  host-kit gained `http_ref`/`get_json_ref`/`send_json_ref`/`conn_dial_ref`/`credential` helpers; and the
  six URL-handback callers (alertmanager, grafana, huggingface, slack, confluence, jira) were migrated.
  The `sql` plugin connects to a discovered Postgres endpoint — host:port/db/user from the secret-free
  weak reference, the password materialized via the gated `credential` capability (never in a URL, never
  to the model), with multi-instance selection. Full removal of the `host.endpoint` URL-handback is a
  tracked follow-up (it remains for attachment byte-IO, the Atlassian gateway, and a couple of config
  reads).
- **Kubernetes endpoint provider + agent-facing discovery ops (D-28).** The kubernetes plugin is now a
  discovery provider: `kubernetes.endpoint.discover` declares the products it can find and returns weak
  `EndpointCandidate`s — kubeconfig contexts → cluster endpoints, in-cluster Services/Ingresses → product
  endpoints (prometheus/loki/grafana/alertmanager), and crossplane/RDS Secrets → `postgres`/`mysql`
  endpoints carrying a `kubernetes/<ns>/<secret>/<key>` `credential_ref` (a location, never a value), with
  "latest namespace" selection. The broker now resolves a provider's real (namespaced) op name. New
  agent ops `endpoint.discover`/`select`/`info`/`list` (read-only, in an `endpoint` group surfaced when a
  `kubernetes` signal — `KUBECONFIG`/`~/.kube/config` — is present) let the model discover and select an
  endpoint as a weak reference; the agent never sees a secret. This wires the "connect to my latest
  namespace backend RDS" path.
- **Reference-based plugin IO + host-injected connect (D-27).** Plugin `http.do`/`conn.dial` now accept
  an `endpoint_ref` (named or discovered `@endpoint/<id>`); the host resolves it, composes the URL
  through the existing egress guard, and injects credentials host-side — the plugin and the model never
  see a URL with credentials. A discovered endpoint's `credential_ref` is materialized via the owning
  plugin's `secret.read` (e.g. a Kubernetes-scheme ref → the kubernetes plugin), gated **deny-by-default**
  by a `[endpoint] cross_plugin_credentials` operator grant + a first-use-approval seam + a
  `CrossPluginResolve` audit event, on both the HTTP-injection and raw-socket paths (gated by the real
  consumer). Raw-socket protocols that must speak auth in-band (Postgres SCRAM) receive the credential via
  a new gated `credential` capability — trusted plugin only, registered with the redactor, never the
  model. Inline `user:pass@host` URLs are split into an injected header. The `Redactor` now shares its
  value store across clones so a mid-run materialized secret is scrubbed everywhere.
- **Cross-plugin endpoint discovery broker (D-26).** Plugin manifests can declare `discovers: [products]`
  (and a `discover` capability); a new L5 `flux_capabilities::endpoint` broker fans a consumer plugin's
  `endpoint.discover` host call out to every provider plugin that declares the product, aggregates and
  ranks their weak-reference candidates, and commits them to the session `EndpointRegistry` — with a
  re-entrancy guard and a `ProviderInvoker` seam. `EndpointBrokerHostCaps` wraps the existing host caps
  (deny-by-default `endpoint.discover`), and the broker is wired into both `flux run` and `flux app run`.
  Discovery results are weak references only — never a resolved URL or a secret.
- **Scoped private-network egress, finished (D-20).** The 0.2.7 scoped model gained **per-endpoint**
  grant granularity (`PrivateNetConfig.endpoints`, keyed `"<plugin>:<endpoint>"`, merged with the
  plugin-level grant) and a **private-network-admit audit event**: a new `EventKind::PrivateNetAdmit`
  is recorded whenever the host admits a private/internal address under a scoped grant, via a new
  `flux_plugin::EgressAudit` seam (no flux-plugin→flux-events dependency) with the event-store-backed
  impl wired at the `flux` CLI. Pulled in as the prerequisite for the endpoint-discovery epic.
- **Endpoint reference model & registry (D-25).** The references-only spine of the endpoint-discovery
  epic: a new L0 `flux_secret::endpoint` schema (`EndpointRef`/`EndpointCandidate`/`EndpointRecord` weak
  references that carry a `credential_ref` location, never a secret; and a host-only `ResolvedEndpoint`
  with no serializer), a `flux_plugin::ReferenceResolver` trait seam, and
  `flux_capabilities::endpoint::{EndpointRegistry, StaticResolver}` — a session registry with
  `put`/`resolve`/`list`/`replace_owned` and `~/.flux/endpoints.toml` persistence (weak refs only), plus
  a static config-binding resolver. No discovery wiring yet (that lands with D-26/D-27).
- **Endpoint discovery & brokerage epic (planning).** Filed the design for cross-plugin endpoint discovery
  ([docs/designs/endpoint-discovery.md](docs/designs/endpoint-discovery.md)) and stories D-25–D-30: a
  references-only plugin-IO model (a plugin operation deals only in host-managed endpoint/credential
  references — never env vars, raw secrets, or credential-bearing URLs), a host fan-out discovery broker, a
  kubernetes endpoint provider, and an endpoint-lifecycle CLI. Reverses the `.dex`-style endpoint-registry
  deferral from D-10/D-12; D-20 (scoped private-net egress) is pulled in as a hard dependency. Design +
  backlog only — no code yet.

### Changed

- **Named-argument calls (L-09).** Flux-Lang `call` parameter order is no longer
  load-bearing: multi-param ops are called with a single named object argument
  (e.g. `write({path, content})`); a sole-required-param op keeps bare-value sugar
  (e.g. `read("README.md")`). The deprecated positional form (2+ bare args) is rejected
  by the analyzer with a repair diagnostic, and the runtime keeps a fallback so legacy
  stored plans still execute. `x-param-order` is gone — `required` is now a *set*, not
  an order; `schema_params` returns sorted optional params for display only. The planner
  prompt + catalog signature (`name({params})`) + skills/reference docs now teach the
  named-args form. This unblocks the schemars schema migration (D-34), since `schemars`
  does not emit `x-param-order`.
- **schemars-derived op input schemas (D-34).** Every in-process `ToolSpec` operation's
  `input_schema` is now derived from a typed Rust struct via
  `flux_spec::tool_input_schema::<T>()` (schemars), killing all hand-written
  `json!({...})` schemas across `flux-tools`, `flux-eval`, and `flux-orchestrate` — so an
  op's schema and its runtime parsing can no longer drift. A regression guard
  (`crates/flux-tools/tests/no_manual_schema.rs`) fails on a reintroduced hand-written
  schema; `docs/archive/drift-reports.md` records the mismatches found + fixed (notably broken `$ref`s in the
  eval comparison ops and the dropped `x-param-order` cargo extension). Plugin
  `OperationSpec` ops (~275) remain deferred.

### Fixed

- Preserved kubeconfig access in `scripts/smoke-plugins.sh` when the Kubernetes plugin smoke uses an
  isolated `HOME`, so `kubectl` sees the same configured cluster as the caller.

## [0.2.7] - 2026-06-30

### Changed

- Clarified the root docs split: `AGENTS.md` is now explicitly the operating contract for coding agents,
  while `README.md` gives humans a faster product overview, common entry points, and contributor map.
- Hardened pre-push security edges: plugin HTTP callbacks now require declared host allow-lists, private
  network access is scoped per caller and per plugin config grant, server turns are serialized on shared
  engines, unauthenticated non-loopback server binds are refused, and persisted composite-op loading now
  goes through guarded `flux-system` paths.

## [0.2.6] - 2026-06-30

### Changed

- **Agent-loop retry efficiency (I-02).** Cargo wrapper ops now normalize model-supplied duplicate scope
  and warning flags before invoking Cargo, preventing failures like duplicate `--workspace` or
  `--all-targets`. The loop retry breaker also fingerprints deterministic cargo duplicate-argument and
  stale `edit` anchor failures, so semantically repeated failures are escalated even when the full
  transcript changes.

## [0.2.5] - 2026-06-30

### Added

- **Generated Flux skills (L-07).** Added `flux skill [cli|lang|plugin|ops]` to render Claude-format
  skills for Flux itself, plus `flux skill --install` / `flux skill <type> --install` to write a root
  routing skill and focused section skills (`flux-cli`, `flux-lang`, `flux-plugin`, `flux-ops`) into
  project `.flux/skills` or user-global `~/.claude/skills` with `--global`. The renderers are grounded
  in live sources of truth: Clap for CLI commands, `flux_lang::skill::render()` for Flux-Lang,
  `ToolRegistry`/`OpRegistry` plus group metadata for operations, and installed plugin manifests for
  plugin ops. Project-local `.claude/skills` is now loaded by default after `.flux/skills`; the legacy
  `flux plugin skill` command remains as a plugin-section alias.
- **Public Docusaurus docs site (L-05).** Added a standalone `website/` Docusaurus project for the public
  docs at `https://codewandler.github.io/flux/`, distinct from the repository's internal contributor and
  design docs. The initial public docs cover getting started, core concepts, CLI/provider basics,
  Flux-Lang text syntax, execution semantics, AST reference pointers, examples, SDK `FlowClient`, plugin
  authoring, and configuration defaults. A GitHub Pages workflow builds the site on PRs and deploys `main`.
- **Flux-Lang composite ops (L-04).** Native `.flux` modules can now declare reusable `op` definitions:
  typed, module-local composite operations implemented as ordinary Flux-Lang bodies. Composite calls are
  catalog-visible, analyze like normal ops, execute in a scoped symbol frame (params/locals do not leak),
  and every inner real op still dispatches through the existing authorization/approval/redaction/guarded-IO
  envelope. SDK `FlowClient`, `flux flow run`, and `flux-app` install module composites; validation rejects
  recursion, `await` in composites, duplicate/conflicting names, and understated transitive risk/effects.
  Added the shell-group-gated `proc.run` op for argv-only process execution through `flux_system::System`.
- **Agent-registered composite ops (L-06).** Added the model-facing root op `op.register`, letting an agent
  register exactly one validated Flux-Lang composite op into `turn`, `session`, `project`, or `global` scope.
  Session definitions persist in the flow store; project/global definitions are normalized `.flux` source
  written through guarded `System` paths (`.flux/ops/<name>.flux` and `@global_ops/<name>.flux`). Registered
  ops are folded into later planner/execution catalogs and still run as scoped composites, so every inner real
  op continues through `Executor::dispatch`.
- **Single guarded process-spawn path + plugin authoring guide (D-22).** All OS-process creation now funnels
  through one `flux_system::System` constructor (`build_command`: argv-only, workspace-pinned cwd, env
  **cleared** to a minimal non-secret allow-list) — `run_with_env`, the streamed runner, `spawn_background`,
  and a new **`spawn_interactive`** (piped stdin/stdout, inherited stderr, `kill_on_drop`) all layer only
  their own stdio on top. `PluginHost::spawn` now launches plugins through `spawn_interactive`, so the
  **plugin process is env-cleared**: a plugin can no longer read the host's secrets via `std::env`, closing a
  bypass of the deny-by-default `secret` gating (regression test `plugin_cannot_read_host_env`).
  `flux-runtime`'s git-context call is routed through `System::run` too (gaining a wall-clock timeout). New
  **`plugins/AUTHORING.md`** — the canonical plugin guide (lifecycle, the host-does-all-IO invariant, the
  capability set, the rules) — linked from `AGENTS.md` and `plugins/README.md`.
- **One daemon command for served agents (D-23).** The standalone `flux serve` command is removed; use
  `flux app run --serve <addr> --yes` to expose the built-in coding agent over the same REST/SSE/A2A HTTP
  surface. A `.flux` program can declare an `a2a` channel, and `flux app run <program.flux> --serve <addr>`
  injects an ad-hoc A2A channel for a sole-agent program. The HTTP implementation is shared through
  `flux-server`, including bearer-token enforcement for non-loopback binds.
- **Provider schema + CLI daemon hardening (D-24).** Added `flux_spec::tool_input_schema` for
  schemars-derived tool input contracts and switched the planner's synthetic `emit_plan`/`ask_user` tools
  to typed schemas; `emit_plan` now advertises the full `DraftAst`/`Node` JSON Schema instead of a bare
  object placeholder. `flux plugin call <plugin> <op>` now accepts short op names by resolving them against
  the plugin manifest's fully qualified operation names, while still preserving explicit full names.
  `flux-server` and `flux app run` channel hosts now honor SIGTERM as well as Ctrl-C, and `flux tui` fails
  early with a clear error when stdin/stdout are not real terminals.
- **Plugin host protocol — managed background processes + binary HTTP body (D-14 enabler).** Two additive
  capabilities on `flux.plugin.v1`, extending the host the way D-12 added auth/conn/blob:
  - **Managed background processes** — `process.spawn`/`read`/`status`/`kill`, a per-session registry in
    `SystemHostCaps` beside `conns`/`blobs` (so a process started in one op call is stopped/queried in a
    later one — one host instance is shared across a plugin's tool calls). Backed by a new
    `flux_system::System::spawn_background` returning a `ManagedChild` (piped stdout/stderr drained into
    capped buffers, `kill_on_drop`). Same safety envelope as `run_with_env`: argv-only, env **cleared** +
    minimal allow-list + caller overrides, workspace-pinned cwd; `process.spawn` is gated by the manifest's
    `process` allow-list exactly like `process.run` (deny-by-default). This is what lets a plugin host a
    long-lived `kubectl port-forward`.
  - **Binary HTTP body** — `http.do` accepts a base64 `body_b64` request body and, with `response_binary:
    true`, returns the raw response bytes as `body_b64` (16 MiB cap, no char-truncation). host-kit exposes
    `Host::process_*` and `Host::http_bytes`. Byte-exact file upload **and** download (was lossy through the
    UTF-8 `String` body before).
- **fluxplane-plugins parity — the 8 native plugins at full op + behavioural parity (D-14).** Brought every
  plugin in the in-repo `plugins/` pack to its fluxplane counterpart's operation set (**+~160 ops**) *and*
  to faithful behaviour (not just op names):
  - **gitlab 6 → 64** — full MR review/diff/discussion workflow, branches, repo files/tree/commits/tags,
    CI/CD, releases + links + changelog, issues, snippets, `repository.archive` → host blob; `mr.diff.lines`
    uses real **regex** matching (matching the reference), `mr.merge` sends the modern `auto_merge` (not the
    deprecated field), `pipeline.create` validates its `variables`.
  - **slack 5 → 30** — edit/delete, threads, search, reactions, bookmarks, presence, emoji; **`mentions`**
    does the reference's replied/acked/pending thread classification and **`unreads`** uses real `last_read`
    cursor math; files upload/download **byte-exact** via `http_bytes`.
  - **kubernetes 5 → 24** — renamed `k8s.*` → `kubernetes.*`; full inventory, scale/restart/history,
    logs/events, secret.read, endpoint.discover, one-shot `pod.exec`; **port-forward start/stop/list run on
    the host managed-process capability** (spawns `kubectl port-forward`, parses the readiness line for the
    real local port, kills on stop).
  - **jira 3 → 21** / **confluence 3 → 15** — full issue/page CRUD + transitions/comments/attachments/
    links/user-search; **attachments byte-exact** via `http_bytes`; jira ports the markdown→ADF renderer and
    the transition-selection scorer faithfully; confluence renders storage↔markdown with `body_format`.
  - **prometheus 4 → 8** (series/targets/rules/alerts; rejects empty `query`), **loki 3 → 5** (metric,
    recent_logs; Basic + `X-Scope-OrgID` tenant header, auth purposes named per the reference), **websearch**
    `provider.list` + provider selector. Each HTTP plugin has an `index.build` op driving its datasource
    contribution exhaustively (`{indexed: n}`).
  - **Auth re-port:** jira/confluence drop the hand-rolled Basic-auth base64 — primary is **Bearer
    `api_token` via the `cloud_id` gateway** (`api.atlassian.com/ex/jira|confluence/{cloud_id}`, the
    fluxplane reference), with **Basic (email:token) retained as a configurable fallback**, selected per
    request from the configured env. The host injects both schemes (D-12 `AuthScheme`); no base64 in-plugin.

  Op shapes + behaviour were ported from the fluxplane manifests/clients; every op keeps a MockHost unit
  test (incl. non-UTF-8 byte round-trips and managed-process lifecycle), and the nested `plugins/` workspace
  gate stays green (203 plugin tests). Deeper fidelity closed too: confluence full storage↔markdown
  conversion, prometheus typed `query`/`query_range` results, loki SHA1 entry ids + RFC3339Nano timestamps,
  slack `mentions`/`unreads` `since`/`unhandled`/`tickets`. New **plugin-local** dependencies: `regex`
  (gitlab `diff.lines`), `pulldown-cmark` + `quick-xml` (confluence storage↔markdown), `sha1` + `time`
  (loki). See [docs/designs/fluxplane-plugins-parity.md](docs/designs/fluxplane-plugins-parity.md).
- **fluxplane-plugins parity — the 9 missing portable plugins are native (D-15/D-16/D-17).** Added the
  remaining single-vendor plugins from the fluxplane pack: **alertmanager** (5 ops), **grafana** (20),
  **opsgenie** (8), **huggingface** (9), **aws** (11 read-only ops via the host-managed `aws` CLI),
  **docker** (33 core Docker Engine REST ops over the guarded Unix `conn.*` stream), **sql** (6 PostgreSQL
  read/introspection ops over `host-kit::ConnStream`), **asterisk** (8 AMI ops over guarded TCP), and
  **homer** (8 Homer SIP-capture ops with JWT login + blob-backed PCAP export). The plugin smoke script now
  has skip-safe entries for the new pack. Honest residuals are documented: Docker's streaming/hijack ops need
  a later stream design, SQL live Postgres interop still needs an env-gated smoke, MySQL is a clear unsupported
  error, and SQLite is unsupported by design because plugins have no host file capability.
- **Reusable A2A server protocol — `flux_a2a::server` (D-03).** Lifted the duplicated A2A server-side
  logic out of `flux-server/src/a2a.rs` into a reusable, **axum-free** module on the L1 `flux-a2a` crate:
  the `A2aTurn` runner seam, `dispatch` (`message/send` → a completed `Task`; JSON-RPC errors),
  `agent_card`, `extract_text`/`extract_context_id`, `rpc_ok`/`rpc_err`, `now_rfc3339`, and
  `status_update_value` (the `message/stream` frame `result`). `flux-server` now consumes it (keeping its
  axum routes + SSE + engine wiring); downstream services can mount the same module instead of
  re-implementing the protocol. Current spec only (no `tasks/send`); `flux-codegate` confirms `flux-a2a`
  stays L1 (only new dep: `async-trait`). Serves downstream A2A consumers.
- **`.flux` does all of it — native-text module declarations (L-03).** A `.flux` app is now written
  entirely in native flux-lang: `agent` / `channel` / `datasource` / `trigger` / `journey` declarations
  (each with an indented `key value` settings block) plus the journey flows, replacing the JSON program
  manifest. Settings are flux-lang values (strings/numbers/bools/lists/records, bare identifiers coerce to
  strings); `channel`/`datasource` default `kind` to the decl name. **Secrets are references, never
  inline** — `secret "ENV_NAME"` lowers to a `{"$secret":…}` marker in the pure parser and is resolved
  from the environment at load by the host (`flux_app::resolve_secrets`); a missing var errors naming the
  var, not the value. This is the **single** secret mechanism — the channel adapters' former
  `"secret:env/KEY"` string convention was removed (token fields now read the host-resolved value), with
  the marker shape owned by L0 (`flux_lang::program::{SECRET_KEY, secret_marker, as_secret_ref}`) and
  `build_channels` guarding against an unresolved marker so the resolve-before-consume order can't be
  skipped silently. The host builds the knowledge backend from the declared `datasource`s
  (markdown/openapi ingesters). **Clean cutover:** `flux_lang::program::Module::parse_str` now parses
  native text (`from_json`/`PROGRAM_KEYS` deleted); `flux app run` and `flux flow run` load native-text
  `.flux` (the latter still sniffs a leading `{` for checked-in JSON `DraftAst` loops). The bundled
  examples (`crates/flux-app/examples/{hello,support-bot}.flux`, `examples/channels-app.flux`) are
  rewritten in native text. No new node kinds; `flux-codegate` layering unchanged. See
  [docs/designs/native-text-modules.md](docs/designs/native-text-modules.md).
- **Tenant/agent context envelope on the event log (D-02).** `flux-events` runs can now carry an optional,
  stream-level `EventContext { account, agent_id, agent_version, correlation_id }`, set once at creation via
  `EventStore::create_session_with_context` (the 1-arg `create_session` delegates with an empty envelope, so
  the single-tenant path and every existing call site are unchanged). The context is surfaced on
  `StoredEvent` / `SessionInfo` / `SessionSummary`, and new account-scoped reads `list_for_account` /
  `account_streams` return only one tenant's runs — so a downstream multi-tenant service replays per-account
  transcripts as *projections over the same log* (via the unchanged `conversation`/`turns` projections), not a parallel store. Additive, idempotent
  column migration; the `events` table and all projections are untouched. See
  [docs/designs/tenant-event-substrate.md](docs/designs/tenant-event-substrate.md).
- **Integration-stack hardening (C-02).** Three follow-ups over the shipped D-07/D-08/D-09/D-10 stack:
  - **`flux plugin call <name> <op> [json]`** — invoke one declared op of an installed plugin directly
    (spawns the binary via `PluginHost`, drives it through the `DatasourceHostCaps` bridge), plus
    **`flux plugin install [dir]`** to register every built `flux-plugin-*` binary in one shot. A new
    `plugins` CI job now builds/tests/clippy/fmt the nested `plugins/` workspace (previously untested
    because it's excluded from the root workspace).
  - **Semantic / embeddings retrieval** behind the D-07 `Embedder` seam, feature-gated (`embeddings`, off
    by default): an `OpenAiEmbedder` over an OpenAI-compatible `/v1/embeddings` (config from env, via the
    runtime-free `ureq` client + the `guard_url` SSRF check) and a `SemanticIndex` decorator over any
    `DatasourceBackend` that reranks keyword candidates by a blend of keyword score + query/record cosine
    similarity. The default build, keyword path, and gate are unchanged; the rerank logic has a hermetic
    stub-embedder test.
  - **`scripts/smoke-plugins.sh`** — a live, env-gated plugin smoke (skip-not-fail) driving `flux plugin
    call` against real vendor APIs for whichever keys are present; documented in the roadmap's standing
    pre-release gate.

- **Integration plugin pack — 8 native plugins (D-08).** A new in-repo `plugins/` cargo workspace
  (excluded from the root flux gate so vendor surface stays out of it) with eight subprocess plugins on a
  shared **`host-kit`** SDK: `websearch` (Tavily + DuckDuckGo), `gitlab` (projects/MRs/issues/pipelines),
  `jira` (issue search/show, projects), `confluence` (search/page/spaces), `kubernetes`
  (namespaces/pods/deployments/logs/events via `kubectl`), `loki` (LogQL), `prometheus` (PromQL/alerts/
  targets), `slack` (post/history/channels/users/thread). Plugins do **no privileged IO of their own** —
  every side effect is a host-capability callback (http with bearer-injection / process / secret-by-purpose
  / datasource-record contribution); list/search ops contribute `flux-datasource` records that reach the
  D-07 index via the L5 `DatasourceHostCaps` bridge. Hermetic `MockHost` tests throughout. See
  [`plugins/README.md`](plugins/README.md) and [`docs/designs/integration-plugins.md`](docs/designs/integration-plugins.md).
- **Process-plugin protocol — manifest + host-capability enrichment (D-10).** `flux-plugin`'s manifest
  is now the single host-introspected source of truth: it gains `auth` (auth-by-purpose), `datasources`
  (shared `flux-datasource` `Declaration`s a plugin contributes), and `endpoints` (env-resolved base URLs);
  `OperationSpec` gains `idempotency` + `secret_purposes` (reusing flux's own `Effect`/`Risk` vocabulary,
  not a ported access enum). `SystemHostCaps` grows `with_manifest`, secret-by-purpose resolution, full
  HTTP (method/headers/body + bearer injection), and `endpoint` resolution. The transport was already a
  single unified Request/Response frame (no `target` field), so this is an additive enrichment, not a
  cutover. A new L5 **`DatasourceHostCaps`** (in flux-capabilities) services a plugin's
  `datasource.records`/`search`/`get` against the D-07 index. See
  [`docs/designs/process-plugin-protocol.md`](docs/designs/process-plugin-protocol.md).
- **Knowledge datasource — a real RAG layer (D-07).** A new L0 **`flux-datasource`** crate holds the
  shared record/retrieval schema (`Record` addressable by `(source, entity, id)`, `Declaration`/
  `EntitySchema`, and the `Search`/`Get`/`List`/`Relation`/`BatchGet` I/O types) — so the knowledge index
  and (future) integration plugins agree on one shape. `flux-capabilities::datasource` is rebuilt onto it:
  a **`DatasourceBackend`** trait with two impls — the in-memory `MemoryBackend` (default, keyword/TF) and
  a persistent **`SqliteBackend`** (a `records` table + an FTS5 virtual table over title+body, ranked by
  the built-in `bm25()`, WAL) — five agent-facing retrieval ops (`search`/`get`/`list`/`relation`/
  `batch_get`, registered via `register_datasource_ops`), markdown + OpenAPI ingesters
  (`ingest_markdown`/`ingest_openapi`), `reindex`/`freshness`, and an unwired `Embedder` (semantic) seam.
  The CLI's `search` is unchanged for users; the model also gains the four new verbs. See
  [`docs/designs/datasource-rag.md`](docs/designs/datasource-rag.md).
- **Agentic channel target — `trigger.agent` (D-09, mechanism).** A channel trigger naming an `agent`
  now wakes a `FlowEngine` agent turn (the model drives RAG + granted tools) instead of a journey, with
  per-thread `(agent, conversation) → EventStore` session memory and grants from the `AgentDecl`'s `tools`
  under a headless `DenyApprover`. Reuses the existing `TriggerDecl.agent` field; the journey route is
  unchanged. **Registry wiring (completing D-09):** a non-breaking `App::with_tools` seam + the CLI's
  `flux app run` now index workspace docs into a shared `DatasourceBackend` and register the D-07
  datasource ops + every discovered D-08 plugin's tools into the host registry (plugin-contributed records
  land in the same index via `DatasourceHostCaps`) — so the agent target drives RAG `search` + the granted
  integration ops over one knowledge index. See [`docs/designs/agentic-channel-target.md`](docs/designs/agentic-channel-target.md).
- **Parameterized flow execution — the behaviour-runner seam (D-01).** Run a *stored, validated* Flux-Lang
  flow **per invocation** with input values injected at call time, instead of re-compiling from natural
  language or baking inputs into the AST. Two thin `flux-sdk` additions over a new `flux-flow` store
  primitive — modules, zero new crates:
  - `FlowStore::seed(session_id, name, value)` (`flux-flow`) — pre-bind a named input so a flow's `$name`
    resolves to it before the run (`put_value` via `Value::from_json` + `bind` as `Hidden`, so a seed
    resolves for the interpreter but stays out of the model-facing `view`).
  - `FlowClient::parse(text)` — deterministic text → AST (wraps `flux_lang`'s parser; **no** provider
    round-trip, the non-NL partner of `compile`).
  - `FlowClient::execute_with(ast, inputs)` + `run_flow(text, inputs)` — execute a flow with `inputs`
    seeded as `$vars`, through the **same `Executor` safety envelope** (seeding injects *data*, never a
    capability). Each call runs against a **fresh store** (per-run isolation); a flow-local `bind` shadows
    a seed. One-shot — genuine cross-turn `await` flows stay on `FlowEngine`. Serves downstream
    behaviour-runner and preset-framework consumers. Hermetic example: `examples/parameterized_flow.rs`.
- **Realtime voice-to-voice as a first-class provider (D-06).** A **sibling, session-oriented** model seam
  beside the half-duplex `Provider`, so a full-duplex speech-to-speech model (OpenAI Realtime) is a flux
  provider whose tool calls run through the **same `Executor` safety envelope** as a text turn — declared
  **once** from the live `ToolRegistry` (no more model-facing-vs-runtime double declaration). Built as
  modules (zero new crates):
  - `flux_core::audio` (L0) — `AudioFormat`/`AudioEncoding`.
  - `flux_provider::realtime` (L1) — `RealtimeProvider`/`RealtimeSession`/`RealtimeEvent`/`RealtimeConfig`/
    `TurnDetection`; events carry decoded bytes and plain strings only (the seam never names a runtime type).
  - `flux_providers::realtime` (L1, behind the **`realtime`** Cargo feature) — the OpenAI-Realtime WebSocket
    impl ported from a downstream realtime client (GA shape; one `openai_realtime(...)` constructor;
    idempotent barge-in cancel).
  - `flux_flow::voice` (L3) — `VoiceSessionDriver` (routes `ToolCall` → `Executor::dispatch` off the audio
    loop; debounced `create_response`; idempotent barge-in), `VoiceSink`, `tool_defs_from_registry`, plus a
    Phase-2 *engine-owned-turns* spike (`run_flow_turns` + a `VoiceTurnHandler` seam — a flux-lang flow
    drives turns; per-turn `run_turn`, not yet cross-turn `await`).
  - `flux_sdk::flow::FlowClient::run_voice_session(...)` (L6) — the one-call consumer seam (mirrors
    `with_sub_agents`). Audio resampling stays in the consumer/channel (model-native format only). The
    downstream consumer rewiring lands outside this repo as a follow-up.
- **Event-trigger channels — background agents woken by events (D-04).** A new `flux-channels` (L6) crate
  lets a `.flux` **program** be woken by external events: a cron schedule, an inbound webhook, or a Slack
  mention. Channels are declared in the program as ordinary `ChannelDecl`s and run by the **app runner** —
  `flux app run <program.flux>` (a new explicit subcommand; `flux run <app.flux>` routes through the same
  path). Each channel fires a bus event **under its own name**; a `trigger { on: "<channel name>", run:
  "<journey>" }` routes it to a journey via the existing `App::deliver` → triggers → journeys path (the
  event payload is seeded into the journey's flow store). flux-app is unchanged — the heavy adapter deps
  (`axum`, `cron`/`chrono`, feature-gated `slack-morphism`) live only in `flux-channels`, which depends on
  flux-app.
  - **schedule** (`kind = "schedule"`): full cron (5-field crontab **or** 6/7-field seconds-first) +
    `on:"startup"`; UTC, fire-and-forget.
  - **webhook** (`kind = "webhook"`): an axum server per channel; `POST` delivers the JSON body and
    replies with the journeys' results, or `202` when `async = true`; optional bearer token, **required**
    for a non-loopback bind (mirrors flux-server).
  - **slack** (`kind = "slack"`, feature `slack`): socket-mode mentions/messages → delivery; posts the
    journeys' result back to the thread; `allow_users`/`allow_channels` policy; tokens via `secret:env/…`.
  - Deliveries are **serialized** (`App::deliver` drains the broadcast bus's cascades, so concurrent
    deliveries would double-process via fan-out); journeys themselves run on independent per-run stores.
    10 hermetic tests + 3 feature-gated Slack unit tests; `examples/channels-app.flux`. See
    [`docs/designs/event-trigger-channels.md`](docs/designs/event-trigger-channels.md).
- **Sub-agents are production-hardened for multi-tenant consumption (D-05).** The `flux-orchestrate`
  sub-agent primitive — single-tenant and wired only in the CLI — now has the seams downstream
  multi-tenant SDK consumers need:
  - **SDK seam.** `FlowClient::with_sub_agents(SubAgents { … })` registers the `task` tool and installs
    the spawner into every run's context, so a consumer drives sub-agents without re-assembling the
    executor/registry/context by hand. `SubAgents::into_spawner` is the single construction path; the CLI
    refactors onto it (unchanged behaviour). Hermetic `flux-sdk` example `sub_agent.rs` (mock, no API key).
  - **Lifecycle limits.** Configurable `SpawnLimits { max_iterations, max_tokens, wall_clock }`; the
    wall-clock deadline **fires the child's cancel token** (cooperative, valid-history termination) rather
    than dropping the future mid-turn. The `task` tool now threads a child of the parent turn's cancel
    token (installed on `ToolContext` per turn by the engine) into the sub-agent — cancelling the parent
    cancels the child, fixing the old orphan-token behaviour.
  - **Pluggable approver.** `LocalSpawner::with_approver` lets a consumer approval-gate a sub-agent's
    mutations instead of the hardcoded auto-approve-non-destructive default.
  - **Audit threading.** `LocalSpawner::with_audit(EventStore)` persists a child's run (and its inner tool
    calls) into a shared tenant event store — the flow store now shares it — instead of a throwaway
    in-memory one. (The account/agent tag + explicit parent-session link land with D-02.)
  - **Ergonomics.** In-memory roles (`RoleRegistry::from_roles` / `FromIterator<Role>`) for programmatic
    consumers, and a depth-aware recursion guard (`with_max_depth`, default `1` keeps children leaves;
    `> 1` is a bounded opt-in). 8 new failing-first tests in `flux-orchestrate`. Isolation stays
    composition over the existing envelope — no new sandbox. See
    [`docs/designs/sub-agent-hardening.md`](docs/designs/sub-agent-hardening.md).
- **Per-turn token usage flows through the unified loop and renders in the CLI.** The planner's token
  counts are now captured from the provider stream (`compile_turn` returns them), accumulated across a
  turn's planner calls by the loop host (output summed; input/cache reflect the final, largest prompt
  so re-sent context isn't multiply-counted), and handed to `sink.turn_end` by the engine. The CLI's
  turn-end rule now shows **context-window occupancy, generated tokens, and — under prompt caching —
  cached tokens with the hit-rate** (e.g. `1 step · 90ms · ctx 1.4k · out 60 · cache 1.2k (87% hit)`);
  it stays clean (no all-zero noise) on offline `-m mock` turns. The SDK `Client` now also populates
  `TurnOutput.usage`. (Previously usage was dropped through the flux-lang loop — `turn_end(None)`.)
  Per-turn usage is now also **persisted** to the unified event store on the `TurnEnded` event
  (serde-default, so older logs still decode) and summed back by the eval runner (`load_usage` →
  `RunResult.tokens`), so `mean_tokens` becomes a real keep/revert tiebreaker for the self-improvement
  loop instead of always reading 0.
- **Stable-baseline self-improvement loop on the synthetic suite.** A new no-Docker loop —
  `examples/improve-synthetic.flux` (adapter `synthetic`, **trials = 5**, strict `score_compare`) with
  its runner `bench/run-synthetic-loop.sh` — drives the keep/revert loop against the 16 deterministic,
  objectively-graded coding riddles. The candidate's edits are measured via `gate_check`'s
  `target/debug/flux` rebuild (no musl), so a round is cheap enough to run trials ≥ 5 for a
  statistically clean gain. The flow is added to the loop's `PROTECTED` paths and the flow-validation
  test.
- **A2A client — `flux a2a <URL>`.** flux can now *consume* a remote A2A agent, not just expose one:
  `flux a2a <URL>` connects to any spec-conformant Agent-to-Agent agent and drives it from the CLI
  like a local agent — an interactive REPL, or a one-shot turn from command-line prompt words or
  piped stdin. Streamed replies render live (`message/stream`); Ctrl-C cancels a turn. A2A is an
  *agent* protocol, not a model protocol, so the client is thin: one user turn maps to one remote
  task (the remote runs its own loop), carrying the A2A `contextId`/`messageId`/`taskId` so a
  stateful remote keeps memory. A new leaf crate **`flux-a2a`** (L1) owns the spec wire types and
  the `A2aClient` (`fetch_agent_card` / `message/send` / `message/stream` / `tasks/get`), shared with
  the server.
- **Global, multi-format skills.** Skills are now discovered from the project's `.flux/skills` **and**
  the user-global dirs `~/.flux/skills`, `~/.agents/skills`, and `~/.claude/skills` (project wins on a
  name clash), so skills kept for other agents work in flux without per-project copies. Beyond the
  flux-native `triggers:` format, flux reads the cross-agent [Agent Skills](https://agentskills.io) /
  Claude format (`name` + `description`, no triggers); trigger-less skills activate on
  `name`/`description` keywords. A new **`flux-markdown`** crate (L0) owns frontmatter parsing
  (`serde_norway`) shared by `flux-skill` and `flux-orchestrate`, and wraps the `codewandler/markdown`
  crates for the TUI/CLI render paths behind off-by-default features. Activation is centralized in
  `flux_skill::active_for` (ranked + capped) and used by both the `flux-flow` and `flux-agent` loops.
- **Native tool calling for OpenRouter and local Ollama via the Anthropic Messages protocol.** Two new
  providers — `openrouter-anthropic` and `ollama-anthropic` — route through each gateway's Messages
  endpoint (`/api/v1/messages`, `/v1/messages`), so tool calls return as structured `tool_use` content
  blocks that can't leak as inline `<tool_call>` text the way some models do on the OpenAI Chat path.
  Both are built on a new shared **`flux-messages`** crate (wire schema + body/stream helpers + a
  per-`(provider, model)` quirks profile); `flux-anthropic` now composes the same core.

### Changed

- **Crate consolidation, phases 2–4 — workspace 35 → 31 crates.** Continuing the within-layer-merge
  pattern from phase 1 (the providers collapse), four thin single-consumer crates were folded into
  their same-layer neighbours (the `flux-codegate` layering lint stayed green throughout, one commit
  per phase): `flux-hooks` → a `hooks` module of **`flux-plugin`** (L4); `flux-browser` +
  `flux-datasource` → a new **`flux-capabilities`** crate with `browser`/`datasource` modules (L5);
  `flux-context` → a `context` module of **`flux-runtime`** (L2, additive to the published surface).
  **`flux-auth` was kept standalone** — caller identity is a distinct concern from tool capabilities
  (and `flux-runtime` must not depend on it). The orphan **`flux-integrations`** crate (Slack
  webhook/notify helpers, no consumers — never wired in) was **removed**; its code remains in git
  history for a future flux-server-native rebuild. No behavior change; all public entry points
  (`flux-plugin::hooks`, `flux_capabilities::{browser,datasource}`, `flux_runtime::context`) keep
  working.
- **One agent loop everywhere; the classic `Agent` loop is gone.** The SDK `Client` and the sub-agent
  spawner (`flux-orchestrate`) now run on the same `FlowEngine` flux-lang loop as the CLI/TUI/server —
  the legacy provider-native `flux-agent::Agent::run_turn` loop has been **deleted** (no fallback, no
  bridge). The `AgentSink` streaming trait moved to `flux-flow` (the engine crate). `flux-agent` is
  repurposed into the **Agent-pillar** crate: it owns **`AgentSpec`** (model, persona, skills, tool
  selection, permissions, settings) + `assemble`/`into_engine` (→ `FlowEngine`), keeps
  `DEFAULT_SYSTEM_PROMPT`, and absorbs the markdown `Role` agent-definition format (moved from
  `flux-orchestrate`). The SDK `Client` keeps its `TurnOutput` API.
- **A2A server speaks the current spec (breaking for A2A callers).** `flux serve`'s A2A endpoint
  moved from the early-draft `tasks/send` / `tasks/sendSubscribe` methods to the current spec's
  `message/send` / `message/stream`, with message parts keyed by `kind` (was `type`), a `Task` /
  `TaskStatusUpdateEvent` result shape built from the shared `flux-a2a` types, and SSE frames as
  plain JSON-RPC responses. The discovery card is now also served at `/.well-known/agent-card.json`
  (the `…/agent.json` path remains as an alias). The old draft methods are gone (clean cutover).
- **CLI: every entry point is now a subcommand (breaking).** The implicit top-level "run a turn"
  behavior and the top-level mode flags are gone, so `flux --help` shows only the command list plus the
  global `--color`. Migrate: `flux --serve <addr>` → `flux serve <addr>`, `flux --tui` → `flux tui`,
  `flux --plan "…"` → `flux plan "…"`, and a flag-led one-shot like `flux -m X "…"` / `flux --yes "…"`
  → `flux run -m X "…"` / `flux run --yes "…"`. `flux` with no arguments still opens the REPL; an
  unrecognized first word is now a clap "unrecognized subcommand" error instead of a bespoke refusal.
  The agent/turn flags (`-m`, `--yes`, `--max-tokens`, `-c`, …) live on the agent-path subcommands
  (`run`/`plan`/`tui`/`serve`) and no longer leak onto `sessions`/`loop`/`eval`/… help.

### Fixed

- **Self-improvement tag scalar is partial-credit-aware.** `SuiteScore::scalar()` now returns
  `round(mean_check_pass_rate * 1000)` instead of `round(pass_rate * 1000)`, so a candidate that
  improves only on sub-checks (partial credit) tags meaningfully (e.g. `improve-tbench-833`) instead of
  the misleading `improve-tbench-0`. Unchanged for binary adapters where `mean_check_pass_rate ==
  pass_rate` (e.g. the synthetic suite).
- **OpenRouter / local-model wire robustness (Messages path).** The shared parser tolerates the
  malformations real gateways and models emit: `null` usage counters, the OpenAI-style `[DONE]` stream
  sentinel, and tool-input JSON with trailing junk or an unterminated tail (off-by-one braces / open
  strings are repaired best-effort). Each has a regression test.
- **Inline tool-call recovery on the OpenAI Chat path.** When a model emits tool calls as text
  (`<tool_call>…</tool_call>` or `<function=…><parameter=…>`) instead of structured `tool_calls` —
  seen with GLM via OpenRouter and local models on multi-call turns — flux recovers them into
  `tool_use` blocks instead of stalling the turn on what looks like prose.

## [0.2.4] — 2026-06-25

Markdown rendering in the CLI — the highest-frequency dogfood readability gap (F2, [#1](https://github.com/codewandler/flux/issues/1)).

### Added

- **Assistant output now renders Markdown to the terminal.** The REPL, agentic mode, and `-p` one-shot
  feed streaming model text through the [`codewandler/markdown`](https://github.com/codewandler/markdown)
  renderer: on a TTY it redraws the reply in place as tokens arrive (headings, bold/italic, inline and
  fenced code with syntax highlighting, lists, links, GFM tables); piped (`flux … | cat`) it stays clean
  plain text with no escape sequences. Closes #1.

## [0.2.3] — 2026-06-25

Dogfood-driven fixes — surfaced by driving flux's own agentic mode on real coding tasks (see
[docs/archive/notes/dogfood-notes.md](docs/archive/notes/dogfood-notes.md)). flux completed every task; the friction was in the
tooling/UX layer.

### Fixed

- **`grep`/`glob` scoped to a file `path` now searches that file** instead of silently returning "no
  matches". The shared workspace walk (`System::walk_files`) only ever listed directories, so a file
  path produced an empty result — wasting agent turns and risking false "symbol not found" conclusions.

### Changed

- **The CLI shows a multi-line preview of tool output** (up to 12 lines, indented, with a `… (+N more
  lines)` note) instead of collapsing each result to a single 200-character line — so test output, grep
  matches, and file reads are actually visible. Display only; the model still receives the full result.

## [0.2.2] — 2026-06-25

Post-publish adoption — making the published release discoverable and installable from the front door.
No functional code changes.

### Added

- **README install section** — prebuilt-binary one-liners (shell + PowerShell, via the auto-tracking
  `releases/latest/download/…` URL) and a from-source fallback, plus CI / release / license status
  badges.

### Changed

- `docs/roadmap.md` refreshed to 0.2.1 status (cargo-dist binaries moved to *Delivered*; the
  0.2.0 daily-driver and 0.2.1 publish milestones recorded; dogfooding and SDK/crates.io noted as the
  next candidate phases).

## [0.2.1] — 2026-06-25

First publicly installable release — no functional changes from 0.2.0.

### Added

- **Prebuilt binaries + installer** — `flux` for Linux, macOS (x86_64 + aarch64), and Windows,
  with a `curl … | sh` / PowerShell installer, produced on each tagged release (cargo-dist).
- Dual-license files (MIT + Apache-2.0), contributor and security policies, and GitHub issue/PR
  templates.

## [0.2.0] — 2026-06-25

Daily-driver readiness: make flux a coding agent you actually reach for. Validated end-to-end against
a live provider (see `scripts/smoke-live.sh`).

### Added

- **Repo-aware context** — each turn's system prompt now includes the git working-tree state (branch,
  short status, recent commits, diff stat) and the project's shape (detected stack + top-level
  listing), so the agent no longer starts each turn blind.
- **A real REPL** — line editing, persistent history, reverse-search, and multiline input (reedline);
  a prompt-level Ctrl-C now clears the line instead of being swallowed.
- **Mid-session controls** — `/model <spec>` switches model/provider without restarting; `flux
  sessions` (and the REPL `/sessions`) list recent sessions with message counts, and `/resume <id>`
  reattaches to one.
- **A live-provider smoke gate** (`scripts/smoke-live.sh`) — exercises the real-provider
  message-shape paths the offline mock can't, as a standing pre-release check.
- Extended thinking is now visible in the REPL, and the usage line shows cache tokens when prompt
  caching is active.

### Changed

- **Stronger coding-agent system prompt** — an explicit inspect → smallest change → verify → summarize
  contract that honors `AGENTS.md`/`CLAUDE.md` conventions.
- **The `edit` tool is resilient to whitespace mismatches** — when the exact text isn't found it
  falls back to a whitespace-tolerant, line-aligned match (leading indentation must still match, and
  CRLF endings are preserved), and its errors now report occurrence line numbers / indentation hints
  instead of just failing.

## [0.1.1] — 2026-06-25

Security and robustness hardening from a full source-tree review. No API additions; existing
behavior is preserved except where it was unsafe.

### Security

- **Sandbox escape closed** — the workspace path guard now resolves symlinks component-by-component
  (including *dangling* symlinks, which `Path::exists()` skips), so a write through an in-workspace
  symlink pointing outside the root is rejected.
- **Subprocess isolation** — model-run commands no longer inherit flux's environment; only a minimal
  non-secret allow-list (`PATH`, `HOME`, …) is passed, so secrets like `ANTHROPIC_API_KEY` can't be
  read by a spawned command.
- **Plugin capability model** — host capabilities (`process.run`, `secret`, `http.do`) are now granted
  per-plugin from the manifest and checked on every call: a plugin can only run allow-listed programs,
  read allow-listed env keys, and reach the network if it declared `http`. Plugin operations also
  declare effects, so they pass through the authorization floor like built-in tools.
- **HTTP daemon authentication** — `flux --serve` now requires a bearer token (`FLUX_SERVER_TOKEN`) on
  every route except `/health`, and refuses a non-loopback bind without one (it auto-approves tools, so
  an open listener was remote code execution).
- **Authorization floor** — a policy grant marked `requires_approval` now forces the approval prompt
  even under a permissive permission rule (previously the `ApprovalRequired` decision was discarded).
- **Sub-agent scoping** — a role with `tools: []` now grants *zero* tools (an explicit empty allowlist),
  instead of inheriting the parent's full toolset.
- **SSRF guard** — web egress (`web_fetch` and plugin `http.do`) share one guard that resolves hostnames
  to IPs and blocks private/loopback/link-local/unique-local/CGNAT and IPv4-mapped ranges, plus internal
  hostnames — closing hostname- and IPv6-based metadata-endpoint access.
- **Secret redaction** — registered secrets are stored trimmed (so a trailing newline no longer defeats
  redaction) and punctuation-glued credential shapes (`api_key=sk-…`) are now scrubbed.
- **OAuth login** — the Claude PKCE login validates the callback `state` against the locally generated
  value (CSRF / code-injection guard).
- **Credential store** — written atomically with `0600` from creation (no world-readable window); a
  corrupt store is now an error instead of being silently overwritten (which wiped other providers'
  tokens).
- **Defense-in-depth** — policy path globs are normalized before matching (a `..` traversal can't widen a
  grant), subject-scoped deny rules fail safe to a prompt when no subjects are reported, unscoped writes
  force approval, user+project policy grants concatenate (a project policy no longer drops user grants),
  and `bash` permission parsing surfaces programs hidden behind `VAR=`/`$(…)`/backticks and flags
  unresolved shell expansion for approval.

### Fixed

- **Session shape** — reaching the per-turn iteration cap while still calling tools now appends a final
  assistant message, so the next turn isn't poisoned by an invalid user-after-user sequence (the third
  of the cancel/compaction/iteration-cap family).
- **Panics & DoS** — char-boundary-safe truncation of fetched/plugin bodies; `saturating_add` in the
  `read` tool's line range; byte→char offset in search snippets; caps on captured process output, framed
  plugin reads, and the OpenAI tool-call accumulator; and a wall-clock interrupt on JS hooks.
- **Provider accounting** — Anthropic input/cache token counts from `message_start` are preserved into the
  final usage chunk instead of being zeroed by the `message_delta`; OpenAI Responses truncation now maps
  to a `MaxTokens` stop reason.
- **Resilience** — `--continue` surfaces real SQLite errors instead of silently starting fresh; a failed
  worker in a parallel `/pd` wave no longer discards its completed siblings.

## [0.1.0] — 2026-06-24

First release.

### Added

- **CLI** — zero-config interactive REPL; `-p` one-shot; `--tui` (ratatui); `--agent` agentic mode under
  the safety envelope; `-c/--continue` to resume; `--serve` HTTP daemon; slash commands
  (`/help` `/tools` `/session` `/clear` `/pd` `/goal` `/loop`).
- **Providers** — `anthropic`, `claude`, `openai`, `codex`, `openrouter`, modeled as wire codec ×
  credential. `provider/model` routing, `flux auth status|login`, import of existing CLI credentials,
  PKCE login, JWT-exp token refresh, adaptive thinking + `--effort`, and Anthropic prompt caching.
- **Safety envelope** — default-deny authorization policy (grants over subjects × resources × actions
  with trust + scopes), layered permission rules with inline approval, destructive-operation escalation
  even under permissive rules, and secret redaction of tool output.
- **Guarded IO** — workspace-confined filesystem with symlink/escape rejection, argv-only process
  execution (no shell), and an SSRF-guarded web fetch.
- **Built-in tools** — `read`, `write`, `edit`, `bash`, `glob`, `grep`, `web_fetch`, `search`, `task`.
- **Sessions** — SQLite-backed, resumable, with automatic context compaction for long sessions.
- **Multi-agent orchestration** — sub-agent roles bounded by the inherited policy; `/pd` dependency-wave
  plan-and-dispatch (parallel workers); `/goal` (evaluator-driven autopilot); `/loop`.
- **Streaming & control** — token-by-token rendering in the CLI, TUI, and over server-sent events;
  in-TUI approval modal; Ctrl-C cancellation of an in-flight turn or command.
- **Extensibility** — JavaScript pre-tool hooks (observe/modify/deny); any-language subprocess plugins
  over a framed protocol with host-capability callbacks, projected as policy-gated tools, plus
  `flux plugin add|ls|pin|rollback`.
- **Skills** — markdown skills with triggers, activated and injected per turn.
- **Surfaces** — a high-level library SDK, an HTTP API + SSE server, and Slack/webhook integrations.
- **Identity** — local single-user default with an OIDC seam for multi-user deployments.
- **Tooling** — an architecture layering lint that fails on inner→outer crate dependencies, and CI
  running build/test/clippy/fmt.

[0.2.3]: https://github.com/codewandler/flux/releases/tag/v0.2.3
[0.2.2]: https://github.com/codewandler/flux/releases/tag/v0.2.2
[0.2.1]: https://github.com/codewandler/flux/releases/tag/v0.2.1
[0.2.0]: https://github.com/codewandler/flux/releases/tag/v0.2.0
[0.1.1]: https://github.com/codewandler/flux/releases/tag/v0.1.1
[0.1.0]: https://github.com/codewandler/flux/releases/tag/v0.1.0
