---
id: C-48
title: Customer-centric changelog (WHATS-NEW.md) + `flux changelog`
pillar: Agent
status: done
priority:
design:
epic:
note: "plain-language 'what has changed' shipped in the binary; CHANGELOG.md stays the engineering log"
---

# Customer-centric changelog (WHATS-NEW.md) + `flux changelog`

## Goal
Give users a plain-language "what has changed" view: a `WHATS-NEW.md` at the repo root
(customer voice — no story IDs, no crate names; `### New/Improved/Fixed/Action needed`
sections per release), embedded into the `flux` binary and displayed via a new
`flux changelog` subcommand (current version by default; `--all`, `<version>`,
`--unreleased`). The release script rolls it exactly like CHANGELOG.md.

## Acceptance
- [x] `WHATS-NEW.md` exists with a voice-rules header, `## [Unreleased]`, and backfilled
      sections for the 0.11.x releases plus a condensed 0.9–0.10 summary.
- [x] `flux changelog` renders the running binary's version section (fallback to the most
      recent non-empty release with a note); `--all`, `<version>`, and `--unreleased` work.
- [x] `scripts/cut-release.sh` rolls WHATS-NEW.md's `[Unreleased]` and stages the file with
      the release commit; an empty `[Unreleased]` warns loudly but does not fail the cut.
- [x] AGENTS.md documents the dual-changelog rule (user-visible change ⇒ WHATS-NEW entry).
- [x] Tests: section splitting, version selection + fallback, `--all` ordering,
      unknown-version error, and the real embedded file parses with ≥1 release section.

## Progress
- 2026-07-09 DONE. WHATS-NEW.md seeded (voice-rules header, [Unreleased] incl. this feature +
  the agent-speed work in customer language, backfill 0.11.4-0.11.6 + condensed 0.9-0.10);
  crates/flux-cli/src/changelog.rs (embed via include_str repo-root — flux-cli is binary-only,
  never on crates.io; split on `## [` headings; render via flux_markdown::render_ansi);
  Commands::Changelog wired; cut-release.sh 3b rolls WHATS-NEW + empty-section warning + stages
  it; AGENTS.md release-mechanics + dual-changelog rule. Tests: split/empty-detect/real-embed.
  Smoke: default (falls to own version), specific, unknown-version error all verified.

## Notes
- Follow-ups (not this story): website "What's new" page (mirror the llms-txt build
  plugin); remote "newer version available" check (offline-first for now).
