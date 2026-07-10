---
id: D-137
title: Sandbox docs truth pass + plugin coverage proof
pillar: Core
status: done
priority: 4
epic: process-sandboxing
design: docs/designs/process-sandboxing.md
note: "flip the 'not OS-sandboxed' promise to 'not OS-sandboxed by default'; new security/os-sandbox page; rewrite the website_contract drift guard"
---

# Sandbox docs truth pass + plugin coverage proof

## Goal
Make the docs true again — plugins are now sandboxable — without overstating v1: update every
"not OS-sandboxed" page to the "by default; enable `[sandbox]`" phrasing, add the OS-sandbox
security page (including the honesty list of what v1 does NOT stop), and rewrite the contract
test so the new promise is drift-guarded like the old one.

## Acceptance
- [x] New `website/docs/security/os-sandbox.md`: what the sandbox confines per platform, the
      `[sandbox]` config + `--sandbox`/`--no-sandbox` + `FLUX_SANDBOX` reference, posture matrix
      (off/on/require × backend available/missing), the browser (`spawn_debug_pipe`) exemption and
      why, and the verbatim "v1 does not defend against" list from the design doc; added to the
      Security sidebar in `website/sidebars.js`.
- [x] Every page asserting "not OS-sandboxed" (the five in the contract test — using-plugins,
      authoring, plugin-sandbox, safety, infrastructure — plus any others `rg` finds) updated to
      the truthful "not OS-sandboxed **by default**" phrasing, cross-linking the new page.
- [x] `plugin_security_copy_keeps_the_native_code_trust_boundary_explicit`
      (crates/flux-cli/tests/website_contract.rs) rewritten failing-first: asserts the new
      phrase + the os-sandbox page's key claims, and rejects the old unqualified disclaimer.
- [x] `website/docs/reference/config.md` documents `[sandbox]`; troubleshooting page gains the
      NamespacesDenied / bwrap-missing entries.
- [x] Website sync tests green (`website_in_sync`, `website_contract`); gate green; CHANGELOG
      entry (docs + the epic's user-visible summary).

## Progress
- Rewrote `plugin_security_copy_keeps_the_native_code_trust_boundary_explicit`
  (`crates/flux-cli/tests/website_contract.rs`) failing-first: replaced the flat
  `docs.contains("not OS-sandboxed")` assertion with
  `assert_every_os_sandboxed_disclaimer_is_qualified`, which scans **every** occurrence of the
  substring "not OS-sandboxed" in a page and requires it be immediately followed by " by default"
  — robust to each page's own old trailing punctuation (colon/semicolon/period/"processes.")
  without needing to enumerate and ban each one individually, and structurally incapable of
  colliding with the new phrasing since it *is* the new phrasing. Kept the two existing negative
  assertions (`Plugins do **no privileged IO of their own**`, `A plugin never opens a socket`).
  Added a second test, `os_sandbox_page_exists_and_states_its_key_claims`, asserting the new page
  exists and states its honesty-list heading, both backend names, the require/fail-closed promise,
  the `[sandbox]` config reference, and the browser exemption. Ran both against the pre-edit repo
  first and confirmed they failed for the right reason (unqualified disclaimer text; missing
  page), then again after the docs edits below to confirm green.
- New `website/docs/security/os-sandbox.md`: per-platform table (Linux/bubblewrap **verified**
  live; macOS/Seatbelt **code-complete, pending hardware verification** — stated honestly, matching
  D-136's own status justification; Windows — no real backend, degrade-with-warning or fail-closed
  only), a "two boundaries" framing distinguishing the capability sandbox (what a plugin may *ask*
  the host) from the OS sandbox (what its raw syscalls can reach), the full `[sandbox]`/CLI-flag/
  env-var reference table, the off/on/require × available/degraded posture matrix (explicitly
  naming `NamespacesDenied` as the expected Docker/hardened-kernel state), the browser
  (`spawn_debug_pipe`) exemption and its Chrome-sandbox-nesting rationale, and the "What v1 does
  not defend against" honesty list covering all five design-doc items (secret reads anywhere,
  exfiltration while network=on, shared-`/tmp` interference, cargo/rustup cache poisoning,
  anything on Windows) each with one added sentence of concrete explanation. Added
  `security/os-sandbox` to `website/sidebars.js`'s Security category, next to `plugin-sandbox`.
- Updated the **five contract-test pages** (`using-plugins.md`, `authoring.md`,
  `plugin-sandbox.md`, `safety.md` — two occurrences (the intro paragraph and the closing
  invariant blockquote), `infrastructure.md`) to "not OS-sandboxed by default", each now
  cross-linking `os-sandbox.md` and naming `[sandbox]` as the opt-in closer. Also updated the two
  other website pages `rg` found making the same unqualified claim outside the contract test's
  five —`security/overview.md` (the "honest posture" bullet + a new pillars-table row) and
  `security/plugin-trust.md` (the "does not guarantee" bullet) — and the repo-internal
  `docs/architecture.md` (the Extensibility → Plugins bullet), since leaving those unqualified
  would have defeated the truth-pass while the drift guard only watches five pages. Left
  `crates/flux-markdown/tests/corpus/{architecture,agents}.md` untouched: they are frozen
  markdown-parser round-trip fixtures, already substantially drifted from their `docs/` /
  `AGENTS.md` sources (confirmed via `diff` — no sync test ties them together), so editing them
  would be scope creep with no test benefit. Left root `AGENTS.md` untouched too — outside the
  `website/ docs/ crates/` scope the story's own `rg` command specified, and not a "docs page".
  Left `docs/roadmap.md` and `docs/designs/process-sandboxing.md`'s "Why"/"Next" narrative
  sentences alone (they describe the pre-epic motivation in past tense, not a current-state claim).
- `website/docs/reference/config.md`: new "## OS-level process sandbox" section (table + example
  TOML + the security-directional merge rule), linked from "Related docs"; `FLUX_SANDBOX*`/
  `FLUX_BWRAP_BIN`/`FLUX_SANDBOX_EXEC_BIN` added to the "Environment overrides" list. The new TOML
  fence is covered by the existing `public_config_examples_deserialize_and_have_effect` contract
  test (parses every `toml` fence in `config.md` against `flux_config::Config`) — confirmed it
  deserializes cleanly under `deny_unknown_fields`.
- `website/docs/troubleshooting.md`: two new entries — "sandbox unavailable: bubblewrap not found"
  (install `bwrap` or set `FLUX_BWRAP_BIN`) and "sandbox auto-degrades: unprivileged user
  namespaces are refused (NamespacesDenied)" (names Docker's default seccomp profile,
  Debian ≤11's `kernel.unprivileged_userns_clone` sysctl, and Ubuntu 23.10+'s AppArmor userns
  restriction as the expected auto-degrade causes, with the `require`-mode alternative), linking
  `os-sandbox.md#posture-matrix`. Added a Related-docs bullet too.
- Gate: `cargo build --workspace`; `cargo test -p flux-cli -p codewandler-flux-lang -p
  codewandler-flux-system -p codewandler-flux-markdown` (all green — includes `website_in_sync`,
  `website_contract`, flux-markdown's `parity`/`roundtrip` corpus suites, and every
  `flux-system::sandbox` unit/live-smoke test, none of which this story touched); `cargo clippy
  --workspace --all-targets -- -D warnings` clean; `cargo fmt --all` (reformatted the new test
  code once) then `--check` clean in both the root and `plugins/` workspaces.

## Notes
- Design: [process-sandboxing](../designs/process-sandboxing.md).
- Keep the two negative assertions from the old test (plugins do privileged IO via the host;
  "A plugin never opens a socket" stays banned copy).
