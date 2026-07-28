---
id: D-186
title: Discover command files from .flux/.claude trees and dispatch them at the REPL/TUI
pillar: Agent
status: done
priority:
epic: claude-interop
design: docs/designs/claude-interop.md
note: "Claude-compatible slash-command files with $ARGUMENTS substitution; today commands are hardcoded built-ins"
---

# Discover command files from .flux/.claude trees and dispatch them at the REPL/TUI

## Goal
Load Markdown slash-command files from project `.flux/commands` + `.claude/commands` and user
`~/.flux/commands` + `~/.claude/commands` (same first-wins precedence and symlink jail as skill
discovery in `flux-runtime::metadata`), and let a human run them as `/name args…` in the CLI REPL
and TUI. A user arriving with an existing `.claude/commands` directory gets their commands working
in flux unchanged.

## Acceptance
- [x] Discovery: `*.md` files (flat, name = file stem) found across the four dirs, first-wins dedup
      by name, project dirs symlink-jailed like skills (`crates/flux-runtime/src/metadata.rs`);
      failing-first test alongside the existing skill-discovery tests.
- [x] Frontmatter `description` and `argument-hint` parse; unknown fields warn once at load (shared
      lint stance with D-189).
- [x] Substitution: `$ARGUMENTS` (full tail) and `$1..$9` (positional) replaced in the command body
      before it enters the turn as the user prompt; missing positionals substitute empty. Test with
      both forms.
- [x] REPL (`crates/flux-cli/src/session.rs`) and TUI (`crates/flux-tui/src/lib.rs`) dispatch
      `/name` to file commands; `/help` and the TUI slash menu list them with description +
      argument-hint. Built-ins always win on name clash; the clash warns at load.
- [x] Claude-specific body features not yet supported (`!`-inline bash, `@file` refs) pass through
      as literal text and are called out in docs — no silent execution.
- [x] Docs: `website/docs/agent/claude-compat.md` + `website/docs/agent/cli.md` updated to describe
      file-based commands (locations, precedence, substitution).

## Progress

**Implemented 2026-07-28.**

### flux-runtime (L2) — `crates/flux-runtime/src/metadata.rs`
- Added `CommandFile { name, description, argument_hint, body, source }`, `CommandDiscovery
  { commands, warnings }`, and `discover_commands(cwd) -> Result<CommandDiscovery>`: walks project
  `.flux/commands` → `.claude/commands` → `~/.flux/commands` → `~/.claude/commands` in that
  precedence, first-wins dedup by name. Project dirs go through the same guarded `System` +
  symlink-jail as skills; user-global dirs are trusted (`Workspace::set_unconfined`), mirroring
  `trusted_skill_files`. Commands are flat (`read_dir_text_files`, not the nested skill reader —
  no `<name>/SKILL.md` shape for commands).
- Frontmatter parsed via `flux_markdown::parse_frontmatter` into a `description` +
  `argument-hint` struct with `#[serde(flatten)] rest: BTreeMap<..>`; any other recognized key
  produces a `CommandDiscovery::warnings` entry naming the file and field(s) — a trivial shared
  warn hook, not a frontmatter lint system (D-189's scope).
- Added `expand_command_arguments(body, raw_args) -> String`: one left-to-right pass substituting
  `$ARGUMENTS` (trimmed full tail) and `$1..$9` (whitespace-split positionals; a missing
  positional substitutes empty). A substituted value is never rescanned (no reentrant expansion).
  Claude's `!`-inline-bash / `@file` syntax is untouched — verified by a dedicated test.
- Added `flux-markdown` + `serde_norway` to `flux-runtime`'s `Cargo.toml` (both already L0/shared
  deps elsewhere in the workspace).
- Tests (in `metadata::tests`, alongside the existing skill-discovery tests):
  `discover_commands_across_four_dirs_with_precedence_and_dedup`,
  `discover_commands_rejects_project_symlink_escape`,
  `command_frontmatter_parses_known_fields_and_warns_on_unknown_ones`,
  `expand_command_arguments_substitutes_full_tail_and_positionals`,
  `expand_command_arguments_missing_positional_substitutes_empty`,
  `expand_command_arguments_leaves_claude_inline_bash_and_file_refs_literal`.

### flux-cli (L3) — `crates/flux-cli/src/execution.rs`, `session.rs`, `app_cmd.rs`
- `load_command_files(cwd, builtin_names)`: calls `discover_commands`, prints frontmatter
  warnings, and drops (with a load-time warning naming the file and the built-in) any command
  whose name clashes with a built-in — built-ins always win, per acceptance. Two builtin-name
  lists live where the dispatch match arms live: `REPL_BUILTIN_COMMANDS` in `session.rs`,
  `TUI_BUILTIN_COMMANDS` in `app_cmd.rs`.
- `run_repl` (`session.rs`) discovers command files once at startup, lists them under `/help`
  (name, description, argument-hint), and the catch-all `other` match arm looks the name up,
  expands arguments, and runs the substituted body via the same `run_interruptible` +
  `run_turn_cancellable` path as ordinary typed input — dispatching a file command behaves
  exactly like typing the substituted text.
- `run_tui` (`app_cmd.rs`) discovers command files and passes them into the new
  `TuiRunOptions::file_commands` field.
- Tests: `execution::command_file_tests::a_builtin_named_file_command_is_dropped_not_dispatched`,
  `execution::command_file_tests::no_command_dirs_yields_an_empty_list`.

### flux-tui (L3) — `crates/flux-tui/src/lib.rs`, `state.rs`, `rendering.rs`
- `ChatState.file_commands: Vec<CommandFile>` (new field, `Default`/`for_session` initialize
  empty) + `ChatState::with_file_commands` builder (mirrors `with_verbose`/`with_cost`).
- `SlashCmd` changed from a `&'static str` pair to owned `String`s so built-ins and discovered
  file commands can share one `Vec` (`BUILTIN_COMMANDS: &[(&str, &str)]` replaces the old
  `COMMANDS` const; `all_slash_commands`/`slash_matches` now take `file_commands` and merge).
  `rendering.rs` and the key-handling event loop updated to pass `&state.file_commands` through.
- Extracted two pure helpers used by `handle_command`'s `"" | "help"` and fallback `other` arms:
  `help_text(file_commands) -> String` (built-in `HELP_TEXT` + a `command files:` block) and
  `file_command_prompt(name, args, file_commands) -> Option<String>` (lookup + substitution). The
  `other` arm calls `start_turn` with the resolved prompt on a hit, else reports "unknown command"
  — unchanged behavior for a non-file-command name.
- `TuiRunOptions.file_commands: Vec<CommandFile>` (new field, defaults empty in `::new`), wired
  into `ChatState` in `run_with_options`.
- Tests: `slash_menu_lists_discovered_command_files_with_hint` (render-level, TestBackend),
  `help_text_lists_discovered_command_files`, `help_text_without_command_files_is_unchanged`,
  `file_command_prompt_substitutes_arguments`, `file_command_prompt_is_none_for_an_unknown_name`.
  Existing `slash_menu_filters_and_renders` updated for the new `slash_matches` signature.

### Docs
- `website/docs/agent/claude-compat.md`: rewrote the "Slash commands" section (was "Not supported
  yet") to describe the four discovery dirs + precedence, frontmatter handling, substitution
  semantics with a worked example, and the explicit `!`/`@file` non-interpretation + human-only
  scope.
- `website/docs/agent/cli.md`: added a "Command files" section under "REPL slash commands" with
  the same worked example and a cross-link to the compat page.

### Verification
- `CARGO_TARGET_DIR=/home/timo/projects/flux/target cargo test -p codewandler-flux-runtime` — 13
  `metadata::` tests pass (6 new + 7 pre-existing, including the concurrently-landed skill-nesting
  tests from another in-flight story).
- `CARGO_TARGET_DIR=/home/timo/projects/flux/target cargo test -p flux-tui --lib` — 62 pass (5 new).
- `CARGO_TARGET_DIR=/home/timo/projects/flux/target cargo test -p flux-cli --bin flux` — 167 pass
  (2 new).
- `CARGO_TARGET_DIR=/home/timo/projects/flux/target cargo test -p flux-cli --test website_contract`
  — 13 pass (green after the doc edits).
- `CARGO_TARGET_DIR=/home/timo/projects/flux/target cargo clippy -p codewandler-flux-runtime -p
  flux-cli -p flux-tui --all-targets -- -D warnings` — clean.
- `cargo fmt --all -- --check` — clean.

### Known gap
- No subprocess/pty-level test exercises the REPL's interactive `/name` keystroke path end-to-end.
  `reedline` requires a real terminal (raw mode) to read lines; piping stdin to the built binary
  causes an immediate `Signal` read error and the REPL exits after the startup banner (verified
  manually) — there is no pty-harness in this repo, and no existing REPL test does this either
  (`flux-cli/tests/mock_smoke.rs` only exercises the one-shot `flux run <prompt>` path). Discovery,
  substitution, and built-in-shadowing are covered at the `flux-runtime`/`load_command_files`
  level; the REPL dispatch arm itself is a thin wire (same shape as the untested `pd`/`goal` arms
  already in `session.rs`) reusing the already-tested `expand_command_arguments` and the same
  `run_interruptible` path every other REPL command uses.

## Notes
- Reuse `flux-system`'s `read_dir_text_files_*` readers and the guarded-root pattern from
  `discover_skills_from` rather than inventing a second discovery mechanism.
- Epic: [designs/claude-interop.md](../designs/claude-interop.md). Agent-side invocation is D-187,
  not this story.
