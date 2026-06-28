# Design: make the generic `bash` op opt-in

## Why

`bash` is the one tool that breaks flux's defining principle — *the LLM is not the runtime; every
operation is a typed, auditable node traversing one safety envelope*. An arbitrary `sh -c` string is
opaque to the policy/approval layer (best-effort `bash_subjects` parsing aside), Turing-complete, and
the easiest path for a model to smuggle control flow out of the auditable plan graph. The goal is to
make `bash` **opt-in (off by default)** so the agent works through dedicated, gated ops, reaching for
the shell only as a deliberate escape hatch.

The precondition: before turning `bash` off by default, the op/primitive catalog must cover what
`bash` was actually used for — otherwise default-off cripples the agent.

## What the session data shows

Mined from the local event/flow stores (`~/.flux/events.db` `run`/`step_started` events,
`~/.flux/flow.db` `symbols`, and the REPL history `~/.flux/history.txt`):

- `bash` was already a **small** fraction of activity: ~14 invocations vs `read` 195, `glob` 49,
  `grep` 20, `git_*`/`edit` a handful.
- Reconstructed `bash` use-cases: `date`/time, `echo`×N (print/repeat), `sensors` (cpu temp), `pwd`,
  arithmetic on a fetched value, reversing/shaping strings, invoking `python`, and many `git`
  operations.
- The `.flux/improve/*.md` beta-feedback notes repeatedly asked for exactly these replacements
  (`bash_safe`/`read_cmd`, regex `grep`, `git_diff`, `append`, `read_many`).

## Coverage analysis

**Already covered (pre-existing):** regex `grep` (`literal:true` to opt out), `append`, `patch`,
`git_status`/`git_diff`/`git_log`/`git_stage`/`git_commit`/`git_push`/`git_checkout`/`git_unstage`,
`read_many`, the pure cognition list ops `sort`/`dedupe`/`top`/`merge`/`compare`, and the language
itself (`when`/`unless`/`match`/`route`, `repeat`/`each`/`loop`, `parallel`/`race`, `try`/`retry`,
`throttle`/`debounce`/`timeout`/`budget`, `pipe`/`seq`, `confirm`, `verify`, `ctx`, plus `expr`/`fmt`/
`jq`/`parse`).

**Residual gaps this change closes:**

| Gap (forced a `bash` call) | Replacement |
|---|---|
| Boolean / comparison logic in conditions | `expr` extended: `== != < <= > >=`, `&& \|\| !`, `true`/`false` |
| String shaping (`split`/`reverse`/`case`/`repeat`/…) | `expr` string fns: `len`/`lower`/`upper`/`trim`/`replace`/`repeat`/`reverse`/`contains`/`concat` |
| `date` | `now` (unix seconds + UTC) |
| `pwd` | `cwd` |
| `uname`/`sensors` | `sys_info` (os/arch/family/hostname) |
| list `length`/`first`/`last`/`filter` | `len`, `first`, `last`, `filter` (pure cognition ops) |
| `python`/`pytest`/`npm`/`node`/`go`/`make` | toolchain ops gated by language signal |

The general-subprocess long tail (anything without a dedicated op) is served by **opt-in `bash`**.

## The opt-in gate

`bash` joins a new **`shell` tool group** (`crates/flux-tools/src/groups.rs`), off by default —
`BashTool::spec().group = Some("shell")`. Groups surface only when their signal fires
(`flux_runtime::is_advertised` / `resolve_active_groups`), so by default `bash` is **not advertised
to the model at all**.

The opt-in is an explicit **`shell` signal**, injected (not detected from the filesystem) by
`detect_signals` when `FLUX_ENABLE_BASH` is truthy (`flux_runtime::shell_opt_in`). Three front doors
all funnel to that env signal:

- **config** — `enable_shell = true` in `.flux/config.toml`; the CLI exports `FLUX_ENABLE_BASH=1`.
- **env** — `FLUX_ENABLE_BASH=1` directly.
- **REPL** — `/shell` toggles it for the session (effective next turn; the advertised catalog is
  recomputed per turn).

Defense-in-depth: `bash` is also removed from the committed default `permissions.allow`, so even if a
plan emits it while the group is off it hits the normal approval gate rather than being auto-allowed.
(`FLUX_SURFACE_ALL=1` still forces everything on, as before — an intentional debug escape hatch.)

## Key files

- `crates/flux-lang/src/runtime.rs` — typed `expr` evaluator (`ExprVal`); `eval_cond` accepts `expr`.
- `crates/flux-tools/src/extra.rs` — `now`, `cwd`, `sys_info`.
- `crates/flux-tools/src/cognition.rs` — `len`, `first`, `last`, `filter`.
- `crates/flux-tools/src/toolchains.rs` — `python_run`/`pytest`/`npm`/`node_run`/`go_*`/`make`.
- `crates/flux-tools/src/groups.rs` — populated `go`/`node`/`python` groups + new `make`/`shell`.
- `crates/flux-tools/src/lib.rs` — `BashTool` group; `register_builtins`; `builtins_register` test.
- `crates/flux-runtime/src/lib.rs` — `make`/`shell` signals; `shell_opt_in`.
- `crates/flux-config/src/lib.rs` — `Config::enable_shell`.
- `crates/flux-cli/src/main.rs` — config→env bridge; `/shell` toggle.
- Docs/prompts: `crates/flux-lang/docs/reference.md`, `crates/flux-flow/docs/ops-reference.md`,
  `crates/flux-agent` `DEFAULT_SYSTEM_PROMPT`, `crates/flux-flow/src/compile.rs`,
  `.flux/skills/flux-flow/SKILL.md`, `AGENTS.md`.

## Verification

`cargo build/test/clippy/fmt --workspace` + `cargo test -p flux-codegate`. Targeted: the `expr`
operator/function tests, the new-op tests, `builtins_register`, and
`bash_is_off_by_default_and_opts_in_via_shell_signal`. Offline (mock provider): confirm `bash` is
absent from the catalog by default and present under `FLUX_ENABLE_BASH=1`.
