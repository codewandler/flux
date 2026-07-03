---
sidebar_position: 2
title: Getting started
---

# Getting started

flux ships as a single `flux` binary. The fastest way to try it needs no API key at all.

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
cargo install --git https://github.com/codewandler/flux flux-cli
```

Prebuilt binaries, installers, and checksums are attached to every
[tagged release](https://github.com/codewandler/flux/releases/latest).

## Try it with no API key

`-m mock` runs an offline provider through the full pipeline — a zero-config way to see how a turn
plans and executes:

```bash
flux run --yes -m mock "summarise this repo"
```

## Run a real agent turn

Point flux at a provider (see [Providers and models](./agent/providers.md) for the full matrix and
`flux auth login`), then:

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

Every operation crosses the same [safety envelope](./agent/safety.md): reads are pre-allowed, writes
and commands prompt for approval, and destructive steps always re-confirm.

## Run a stored Flux-Lang flow

Flux-Lang text can be parsed and executed without asking a model to compile a new plan:

```flux
flow hello -> String
  $when = now()
  $message = fmt("hello — the time is {when}")
  return $message
```

```bash
flux flow run hello.flux
```

A flow that never reaches a model op runs without any API credentials. Input values are data —
they do not grant capabilities. Any operation that touches files, processes, network, models, or
plugins still crosses the runtime safety envelope. Take the
[ten-minute language tour](./language/tour.md) to go deeper, or run a whole app from one `.flux`
file with [multi-agent programs](./agent/programs.md).

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
