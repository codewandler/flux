---
sidebar_position: 2
title: Getting started
---

# Getting started

flux is currently developed from source. The CLI is the reference application.

## Build

```bash
cargo build --workspace
```

## Run a local agent turn

```bash
cargo run -p flux-cli -- run "read the README and summarize the project"
```

The exact provider and model setup depends on your local configuration. flux also includes a mock
provider path used by tests and offline development.

## Run a stored Flux-Lang flow

Flux-Lang text can be parsed and executed without asking a model to compile a new plan:

```flux
flow hello(name: String) -> String
  $message = fmt("hello {name}")
  return $message
```

The important distinction is that input values are data. They do not grant capabilities. Any operation
that touches files, processes, network, models, or plugins still crosses the runtime safety envelope.

## Contributor setup

The full repository gate is:

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test -p flux-codegate
```

This public site is intentionally lighter than the contributor docs. For implementation work, use the
repository's internal `docs/` map and `AGENTS.md`.
