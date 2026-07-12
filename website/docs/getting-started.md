---
sidebar_position: 2
title: Getting started
description: "Fast install and first-run path with mock/offline mode, safety behavior, and key CLI entry points."
---

# Getting started

flux ships as a single `flux` binary. This page gets you from install to a real turn, with an
offline smoke test first so you can verify the runtime without provider credentials.

## Install

**Prebuilt binary** — installs `flux` into `~/.cargo/bin` (Linux, macOS, Windows; x86_64 + aarch64):

```bash
# Linux / macOS
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.sh | sh
```

```powershell
# Windows (PowerShell)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.ps1 | iex"
```

**From source** — requires Rust 1.85+ (`rustup update stable`):

```bash
cargo install --git https://github.com/codewandler/flux --package flux-cli
```

Prebuilt binaries, installers, and checksums are attached to every
[tagged release](https://github.com/codewandler/flux/releases/latest).

Verify which executable and release you are using:

```bash
command -v flux
flux --version
flux changelog
```

To update a prebuilt installation, rerun the installer—it resolves the latest published release and
replaces the existing binary. For a source installation, rerun the Cargo command with `--force`:

```bash
cargo install --force --git https://github.com/codewandler/flux --package flux-cli
```

## Try it without an API key

`-m mock` is an offline provider that drives the full plan/execute pipeline with canned output. It is
a zero-config runtime check: flux plans, approves, executes, writes `flux-mock.txt`, and prints
`Finished.` regardless of the prompt.

Use it to verify wiring. Use a real provider for representative agent behavior.

```bash
flux run --yes -m mock "write a quick note"
```

## Run a real agent turn

Point flux at a provider, then run a turn. The full provider matrix and credential paths are in
[Providers and models](./agent/providers.md).

```bash
# Plan + run. Risky steps prompt for approval; --yes auto-approves.
flux run "add a test for the parser"

# Preview the plan before anything runs
flux plan "summarize README.md into SUMMARY.txt"

# Interactive REPL (session auto-saved); /help for slash commands
flux

# ratatui chat UI with live streaming + an in-UI approval modal
flux tui

# Which providers/credentials are configured
flux auth status
```

Every operation crosses the same [safety envelope](./agent/safety.md). Reads are pre-allowed; writes
and commands prompt; destructive steps always re-fire the approval gate. `--yes` auto-approves every
step, including destructive ones, so reserve it for trusted automation.

## Run a stored Flux-Lang flow

Flux-Lang text can be parsed and executed without asking a model to compile a new plan:

```flux
flow hello -> String
  $when = now()
  $utc  = $when.utc
  $greeting = fmt("hello — the time is {utc}")
  return $greeting
```

```bash
flux flow run hello.flux
```

A flow that never reaches a model op runs without any API credentials. Input values are data —
they do not grant capabilities. Any operation that touches files, processes, network, models, or
plugins still crosses the runtime safety envelope. Take the
[ten-minute language tour](./language/tour.md) to go deeper, or run a whole app from one `.flux`
file with [multi-agent programs](./agent/programs.md).

## Set up your editor

Hand-editing `.flux` files is much nicer with syntax highlighting plus live diagnostics,
completion, hover, and formatting from the `flux-lsp` language server. The
[Editor setup](./language/editors.md) page has the recipes — Helix is the reference setup, with
Neovim, Zed, and IntelliJ/TextMate covered too.

## Learn by building

For a guided path through the agent, Flux-Lang, and Flux apps, follow
[Build your first Flux app](./tutorial.md). The tutorial assumes only basic terminal skills and
ends with a real model-backed assistant that answers from local Markdown documentation.

## Contributor setup

Building from a checkout, the full repository gate is:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p flux-codegate
```

This public site is intentionally lighter than the contributor docs. For implementation work, use the
repository's internal `docs/` map and `AGENTS.md`.

## Related docs

- [Beginner tutorial](./tutorial.md) — build a guarded agent task, reusable flow, and local app.
- [Concepts](./concepts.md) — the plan-first execution model.
- [CLI](./agent/cli.md) — the command surface after the first run.
- [Safety and approvals](./agent/safety.md) — what prompts and why.
- [What's new](./whats-new.md) — customer-facing release notes.
