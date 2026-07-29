---
id: C-185
title: A leading diff/list marker must not hide a credential from the redactor
pillar: Core
status: ready
priority: 1
epic:
design:
note: "`redact_patterns`' is_boundary set omits `+`/`-`/`*`/`#`, and the shape matcher requires token.starts_with(prefix) — so `+sk-ant-…` on an added diff line tokenizes with the marker glued on and is NOT redacted; every surface that renders a diff inherits the gap (found while building C-132, worked around locally there)"
---

# A leading diff/list marker must not hide a credential from the redactor

## Goal
`flux_secret::redact_patterns` splits input into tokens on a fixed boundary set
(`crates/flux-secret/src/lib.rs:223-242`) and only redacts a token that **starts with** a known
credential prefix (`:244`). That set contains quotes, brackets and punctuation but **not** `+`,
`-`, `*` or `#` — so on a unified-diff line the marker is glued to the front of the token and
`+sk-ant-abc…` never matches `sk-ant-`. The credential renders in full. This was found while
building the run export (C-132), which worked around it locally by stripping the marker before
redacting; the defect is in the shared redactor, so every surface that renders a diff — the
approval sheet's hunk preview (C-115), tool-card detail, the export, any future diff view —
inherits it. Registered-value redaction is unaffected (that path is a literal replace); this is
the shape-based matcher only.

## Acceptance
- [ ] Failing-first test in `flux-secret`: a credential-shaped value preceded by `+`, `-`, `*` or
      `#` (with no intervening space) is redacted — asserted before the fix and green after.
- [ ] The fix is in `redact_patterns`, not at call sites, and C-132's local workaround in
      `render_diff` (`crates/flux-cli/src/export_cmd.rs`) is removed in the same change so there
      is exactly one place that knows this rule.
- [ ] Redaction stays conservative in the other direction: a token that legitimately contains a
      hyphen *inside* it (`sk-ant-…` itself) must still redact as one unit — a test pins that
      widening the boundary set did not split credentials into unredacted fragments.
- [ ] The approval sheet's diff preview is covered: a seeded secret on an added hunk line does not
      reach the rendered sheet.
- [ ] Standard gate green in both workspaces (`cargo build/test/clippy -D warnings/fmt`,
      `cargo test -p flux-codegate`, plus `cargo fmt --check` in `plugins/`).

## Progress
- 2026-07-29 filed from a finding in C-132: the export's golden test seeded a secret into a diff
  and it survived redaction until the renderer stripped the marker by hand.
- 2026-07-29 fixed in `redact_patterns`. The markers were **not** added to the boundary set — `-`
  occurs inside `sk-ant-…`/`xoxb-…`, so making it a boundary would split a key into fragments that
  match no prefix and render in the clear (the opposite of the fix). Instead `flush` strips a
  leading run of `LINE_MARKERS` (`+ - * #`) off the token, matches the prefix against the
  remainder, and re-emits the markers verbatim: `+sk-ant-…` → `+[redacted]`. C-132's marker-split
  workaround in `render_diff` (`crates/flux-cli/src/export_cmd.rs`) is gone; the renderer now hands
  the whole line to the redactor and knows nothing about the rule.
- **Open — acceptance item 4 (approval sheet) not done, blocked on scope.** The premise does not
  hold: `flux-tui` performs **no redaction at all**. It has no `flux-secret` dependency
  (`crates/flux-tui/Cargo.toml`), no `Redactor` anywhere in `crates/flux-tui/src/`, and the sheet's
  preview reads raw tool input via `pending_approval_input` → `toolview::format_diff`
  (`crates/flux-tui/src/rendering.rs:570-572`), which never sees a redactor. Covering it is not a
  boundary-set change: it needs a new dependency edge plus a `Redactor` threaded into `ChatState`,
  and it should first settle whether the approval sheet — a local, human-eyes decision surface
  whose entire job is showing the user what is about to be written — *should* redact at all, or
  whether redaction belongs only on log/model-facing paths. Worth its own story.

## Notes
- Deliberately scoped to the boundary set. The broader question C-132 also raised — that
  conversation `Message` text and `TurnSummary.user_input`/`answer` are never redacted at write
  time anywhere in the engine, so every reader's own `Redactor` is the only control — is a
  separate and much larger decision; file it on its own if it is worth taking.
- Seams: `crates/flux-secret/src/lib.rs:222-264`, `crates/flux-cli/src/export_cmd.rs`
  (`render_diff`), the C-115 hunk preview in `crates/flux-tui/src/rendering.rs`.
