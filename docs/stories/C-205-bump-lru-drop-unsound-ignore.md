---
id: C-205
title: "Bump lru to >= 0.16.3 and drop the RUSTSEC-2026-0002 advisory ignore"
pillar: Core
status: ready
priority: 5
epic: security-assurance
design: docs/designs/security-assurance.md
note: "SURFACED BY C-188 — lru 0.12.5 carries an *unsound* (not vulnerable) advisory reachable only via LruCache::iter_mut; the clean fix is a Cargo.lock bump, which was out of C-188's fence"
---

# Bump lru to >= 0.16.3 and drop the RUSTSEC-2026-0002 advisory ignore

## Goal
C-188 added `cargo-deny`/`cargo-audit` advisory scanning and had to ignore four *informational*
advisories to reach green. Three are unmaintained transitive crates with no clean fix; the fourth,
**RUSTSEC-2026-0002** (`lru` 0.12.5 `IterMut` unsound — a Stacked-Borrows soundness violation
reachable only by code calling `LruCache::iter_mut`), has a real fix: a version bump to `>= 0.16.3`.
That touches `Cargo.lock`, which was out of C-188's fence, so it was ignored with this standing
follow-up. Do the bump and remove the ignore, so the advisory gate carries one fewer standing
exception.

## Acceptance
- [ ] `cargo update -p lru --precise <>=0.16.3>` (or a manifest bump if a direct dependent pins a
      lower major) moves `lru` to `>= 0.16.3` in `Cargo.lock`; confirm nothing else regresses in the
      resolve.
- [ ] The `RUSTSEC-2026-0002` entry is removed from `deny.toml`'s `[advisories].ignore` **and** from
      the `cargo audit --ignore …` list in `.github/workflows/security-audit.yml` — the two must stay
      in sync (C-188 made them mirror each other).
- [ ] `cargo deny check` and `cargo audit --deny warnings` are green on both the root and `plugins/`
      lockfiles with the ignore gone — proving the bump actually cleared the advisory rather than
      moving it.
- [ ] Full Rust gate green (`cargo build/test/clippy -D warnings/fmt`, `cargo test -p flux-codegate`)
      — a transitive bump can change behavior; the workspace tests must still pass.
- [ ] `cargo-deny`'s `unused-ignored-advisory` no longer has anything to warn about for this id.

## Progress
- 2026-07-29 — filed from C-188's adjacent finding during impl-coord integration. The other three
  C-188 ignores (paste / ttf-parser / rustybuzz, all unmaintained proc-macro or trusted-font render
  path) have no clean drop-in fix and are correctly left ignored; this one does.

## Notes
- Verify which crate pulls `lru` (`cargo tree -i lru`) before bumping — if a direct dependency caps
  the major, the bump may need that dependency updated too, which widens the change.
- Source: [C-188](C-188-dependency-advisory-scanning.md) ADJACENT finding; advisory RUSTSEC-2026-0002.
