---
id: D-49
title: Plugin naming + docs truth pass — the crate / the pack / the CLI
pillar: Core
status: ready
priority: 10
epic: plugin-platform-hardening
design: docs/designs/plugin-distribution.md
note: "docs-truth pass (C-16/L-19 pattern): apply the canonical trio vocabulary — the plugin protocol crate (`flux-plugin`) vs the plugin pack (`flux-plugin-<name>` binaries) vs the plugin CLI (`flux plugin …`) — and document the remote install path once it ships"
---

# Plugin naming + docs truth pass — the crate / the pack / the CLI

## Goal
User-facing docs conflate three different things that all read as "flux plugin": the protocol
*library* (`crates/flux-plugin`), the plugin *binaries* (`flux-plugin-<name>`), and the *CLI*
surface (`flux plugin …`). Apply the canonical vocabulary decided in
[plugin distribution](../designs/plugin-distribution.md) ("Naming: the trio") everywhere a user
reads, and make the install docs tell the truth once the remote path (D-47) exists.

## Acceptance
- [ ] The trio vocabulary is applied consistently: **the plugin protocol crate** (`flux-plugin`),
      **the plugin pack / a plugin binary** (`flux-plugin-<name>`), **the plugin CLI**
      (`flux plugin …`, always with the space). Rule of thumb documented once (hyphen-no-suffix =
      crate; hyphen-with-name = pack binary; space = CLI) and linked from the design doc.
- [ ] `README.md` "Install" gains an "Install plugins" subsection: the remote one-liner
      (`flux plugin install <name>`), what verification the user gets (signed index + checksums,
      one sentence), and the source fallback (`cd plugins && cargo build --release &&
      flux plugin install --dir`).
- [ ] `plugins/README.md` "Installing + invoking plugins" leads with the remote path and demotes the
      source build to the contributor/fallback path; `plugins/AUTHORING.md` states where a new
      plugin's binary ends up (pack release) and what the release channel is.
- [ ] `docs/usage.md` and `docs/architecture.md` plugin mentions audited against the trio; the
      release-process doc (wherever D-46 documents it) carries the "never hand-push a `plugins-v*`
      tag" warning.
- [ ] `flux plugin --help` and its subcommand help strings follow the trio and describe the
      *current* semantics (remote install, `--dir`, enforced pin/rollback once D-48 lands) — help
      text asserted by the existing CLI help tests where present.
- [ ] Docs-only + help-string-only change: no behavioral code paths touched; gate green.

## Progress
- (not started)

## Notes
- Depends on [D-47](D-47-remote-plugin-install.md) (docs must not promise an unshipped install
  path); pick up [D-48](D-48-enforceable-pin-rollback.md)'s pin/rollback wording if it has landed,
  otherwise keep pin documented as advisory and leave a pointer.
- Pattern precedent: C-16 and L-19 docs-truth passes — verify claims against the code, don't
  paraphrase the design doc.
- The future crates.io vanity prefix (`codewandler-flux-*`, `crates/flux-sdk/PUBLISHING.md`) renames
  only the crate leg of the trio — no doc should hardcode assumptions that break under that rename.
