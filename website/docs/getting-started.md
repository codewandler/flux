---
sidebar_position: 2
title: Getting started
description: "Fast install and first-run path with mock/offline mode, safety behavior, and key CLI entry points."
---

# Getting started

flux ships as a single `flux` binary. This page gets you from install to a real turn, with an
offline smoke test first so you can verify the runtime without provider credentials.

## Install

### Convenience installer

The release page publishes installers for Linux, macOS, and Windows. Download the script first so
you can inspect the code you are about to run.

On Linux or macOS:

```bash
curl --proto '=https' --tlsv1.2 -LsSf -o flux-installer.sh \
  https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.sh
sh flux-installer.sh
```

The installer writes to `${CARGO_HOME:-$HOME/.cargo}/bin`. Open a new shell after it finishes. If
that directory is not already on `PATH`, add it to your shell startup file; for the current shell:

```bash
export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
```

On Windows PowerShell:

```powershell
Invoke-WebRequest `
  https://github.com/codewandler/flux/releases/latest/download/flux-cli-installer.ps1 `
  -OutFile flux-installer.ps1
powershell -ExecutionPolicy Bypass -File .\flux-installer.ps1
```

Open a new PowerShell window after installation. For the current window, if needed:

```powershell
$env:Path = "$HOME\.cargo\bin;$env:Path"
```

These moving `latest` URLs trust the GitHub release origin. For a version- and workflow-bound
installation, use the manual path below.

### Attestation-verified manual install

The following Linux/macOS commands select a real target from `uname`, verify the archive against the
official release workflow and exact tag commit, and install the binary into `~/.local/bin`. Set
`FLUX_RELEASE` to the tag you reviewed (the first command resolves the latest published tag; replace
it with an explicit tag when your deployment pins one):

```bash
set -euo pipefail
export FLUX_RELEASE="$(gh release view --repo codewandler/flux --json tagName --jq .tagName)"
case "$(uname -s)/$(uname -m)" in
  Linux/x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux/aarch64|Linux/arm64) target=aarch64-unknown-linux-gnu ;;
  Darwin/x86_64) target=x86_64-apple-darwin ;;
  Darwin/arm64) target=aarch64-apple-darwin ;;
  *) printf 'No prebuilt flux archive for %s/%s\n' "$(uname -s)" "$(uname -m)" >&2; exit 1 ;;
esac
archive="flux-cli-$target.tar.xz"
work_dir="$(mktemp -d)"
trap 'rm -r "$work_dir"' EXIT
gh release download "$FLUX_RELEASE" --repo codewandler/flux \
  --pattern "$archive" --dir "$work_dir"
source_digest="$(gh api "repos/codewandler/flux/commits/$FLUX_RELEASE" --jq .sha)"
gh attestation verify "$work_dir/$archive" --repo codewandler/flux \
  --signer-workflow codewandler/flux/.github/workflows/release.yml \
  --source-ref "refs/tags/$FLUX_RELEASE" --source-digest "$source_digest" \
  --deny-self-hosted-runners
tar -xJf "$work_dir/$archive" -C "$work_dir"
install -d "$HOME/.local/bin"
install -m 0755 "$work_dir/flux-cli-$target/flux" "$HOME/.local/bin/flux"
trap - EXIT
rm -r "$work_dir"
export PATH="$HOME/.local/bin:$PATH"
```

Windows releases currently target x64. This PowerShell equivalent installs to your local programs
directory and adds it to your user `PATH`:

```powershell
$ErrorActionPreference = "Stop"
function Assert-NativeSuccess([string] $step) {
  if ($LASTEXITCODE -ne 0) { throw "$step failed with exit code $LASTEXITCODE" }
}
$release = gh release view --repo codewandler/flux --json tagName --jq .tagName
Assert-NativeSuccess "Resolve release"
$target = "x86_64-pc-windows-msvc"
$archive = "flux-cli-$target.zip"
$workDir = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $workDir | Out-Null
try {
gh release download $release --repo codewandler/flux --pattern $archive --dir $workDir
Assert-NativeSuccess "Download release"
$sourceDigest = gh api "repos/codewandler/flux/commits/$release" --jq .sha
Assert-NativeSuccess "Resolve source commit"
gh attestation verify (Join-Path $workDir $archive) --repo codewandler/flux `
  --signer-workflow codewandler/flux/.github/workflows/release.yml `
  --source-ref "refs/tags/$release" --source-digest $sourceDigest `
  --deny-self-hosted-runners
Assert-NativeSuccess "Verify attestation"
Expand-Archive (Join-Path $workDir $archive) -DestinationPath $workDir
$installDir = Join-Path $env:LOCALAPPDATA "Programs\flux\bin"
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item (Join-Path $workDir "flux.exe") `
  (Join-Path $installDir "flux.exe") -Force
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (($userPath -split ';') -notcontains $installDir) {
  [Environment]::SetEnvironmentVariable("Path", "$installDir;$userPath", "User")
}
$env:Path = "$installDir;$env:Path"
} finally {
  Remove-Item -Recurse -Force $workDir
}
```

The current target names and downloadable archives are listed on the
[release page](https://github.com/codewandler/flux/releases/latest).

### Install from source

This requires Rust 1.87 or newer (`rustup update stable`):

```bash
cargo install --git https://github.com/codewandler/flux --package flux-cli
```

From a Flux checkout, `task install` verifies the workspace and installs both `flux` and
`flux-lsp`. It also requires Python 3.10+ as a pre-Cargo build-ownership helper. The default launcher
is selected automatically on Linux, macOS and Windows; set `PYTHON=<executable>` only to override
it. An operator-selected `CARGO_TARGET_DIR` stays reusable, and concurrent `task clean` refuses
while an install is building.

### Verify and update

On Linux or macOS, verify which executable and release you are using:

```bash
command -v flux
flux --version
flux changelog
```

On Windows, use `Get-Command flux`, followed by the same `flux --version` and `flux changelog`
commands.

To update a convenience installation, download and run the installer again. To update a manually
verified installation, repeat the matching manual block with the newer reviewed release tag. For a
source installation, rerun Cargo with `--force`:

```bash
cargo install --force --git https://github.com/codewandler/flux --package flux-cli
```

## Try it without an API key

`-m mock` is an offline provider that drives the full adaptive loop with canned native calls. It is
a zero-config runtime check: flux detects intent, captures and approves a batch, writes `flux-mock.txt`, and prints
`Finished.` regardless of the prompt.

Use it to verify wiring. Use a real provider for representative agent behavior.

```bash
flux run --yes -m mock "write a quick note"
```

## Run a real agent turn

Point flux at a provider, then run a turn. The full provider matrix and credential paths are in
[Providers and models](./agent/providers.md).

```bash
# Adaptive turn. Risky batches prompt; --yes approves admitted actions within active ceilings.
flux run "add a test for the parser"

# Reveal intent, scoped exploration, and batch machinery
flux run --show-loop "summarize README.md into SUMMARY.txt"

# Interactive REPL (session auto-saved); /help for slash commands
flux

# ratatui chat UI with live streaming + an in-UI approval modal
flux tui

# Which providers/credentials are configured
flux auth status
```

Every operation crosses the same [safety envelope](./agent/safety.md). Evidence reads are pre-allowed;
writes and commands are captured into an action batch and prompt; destructive effects remain forced
through approval. `--yes` auto-approves every admitted action, including destructive ones, but never
widens policy, app, or agent ceilings. Reserve it for trusted automation.

## Run a stored Flux-Lang flow

Flux-Lang text is authored, parsed, and executed without asking a model to generate the program. Save this
as `hello.flux`:

```flux
flow hello -> String
  clock = now()
  utc = clock.utc
  greeting = fmt("hello — the time is {utc}")
  return greeting
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
- [Concepts](./concepts.md) — typed stages, authored flows, and guarded execution.
- [CLI](./agent/cli.md) — the command surface after the first run.
- [Safety and approvals](./agent/safety.md) — what prompts and why.
- [What's new](./whats-new.md) — customer-facing release notes.
