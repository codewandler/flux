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

- 2026-07-29 — BLOCKED on the ratatui 0.29 hold; no lockfile change made. The Notes' "if a direct
  dependency caps the major" case is what actually obtains, and the widening does not stop at one
  dependency — it leaves this repo entirely.
  - `lru` has exactly one path into the graph: `lru 0.12.5` ← `ratatui 0.29.0`, which requires
    `lru = "^0.12.0"`. Nothing in the workspace depends on `lru` directly, so there is no manifest
    of ours to bump. `cargo update -p lru` reports `Locking 0 packages`; `--precise 0.16.3` fails
    with `failed to select a version for the requirement 'lru = "^0.12.0"' ... required by package
    'ratatui v0.29.0'`.
  - `ratatui` is deliberately held at `">=0.29, <0.30"` in the root `Cargo.toml`, and that hold is
    load-bearing: the git-pinned `markdown-ratatui` 0.1.2 (codewandler/markdown @35c6db5, used by
    flux-markdown) declares `ratatui = { version = "0.29" }`. Lifting ratatui here would resolve two
    incompatible ratatui versions at the flux-markdown seam. The repo's own comment adds that
    `crossterm` (0.28) and `ansi-to-tui` (7) must lift in the same move.
  - So clearing this advisory is gated on an **external repository** adopting ratatui 0.30 — not on
    anything editable from this story's fence, and not achievable as a lru-scoped lockfile bump.
  - Confirmed the payoff is real once that unblocks: a scratch resolve of `ratatui 0.30.2` pulls
    `lru 0.18.1` via `ratatui-core 0.1.2`, which is `>= 0.16.3` and clears RUSTSEC-2026-0002.
  - `plugins/Cargo.lock` contains no `lru` at all and is already advisory-clean, so the plugins half
    of acceptance item 3 needs no work.
  - The two ignore sites were therefore left in place: removing them makes the gate red, not green
    (`cargo audit` without the lru ignore ⇒ `error: 1 denied warning found!`). `cargo deny check` is
    green as-is.
  - Suggested re-scope: make this story depend on a ratatui 0.29→0.30 upgrade story (markdown-ratatui
    pin move + crossterm + ansi-to-tui), and keep the RUSTSEC-2026-0002 ignore until then.

## Notes
- Verify which crate pulls `lru` (`cargo tree -i lru`) before bumping — if a direct dependency caps
  the major, the bump may need that dependency updated too, which widens the change.
- Source: [C-188](C-188-dependency-advisory-scanning.md) ADJACENT finding; advisory RUSTSEC-2026-0002.

- 2026-08-01 — **UNBLOCKED: the premise of the 2026-07-29 block is stale.** That note said the hold
  lifts "once codewandler/markdown moves to ratatui 0.30". **It has.** Measured:

  | | rev | `crates/markdown-ratatui` requires |
  |---|---|---|
  | what flux pins (`crates/flux-markdown/Cargo.toml:28`) | `35c6db54` | `ratatui = "0.29"` |
  | that repo's `main` | `ad16fe5` | **`ratatui = "0.30"`** |

  Four commits apart, including two releases (0.2.0, 0.2.1). We own that repository
  (`~/projects/markdown`), so nothing here waits on a third party.

  **Status changed `blocked` → `ready`.** It is no longer blocked; it is *unstarted*, and the two are
  not the same thing on a board.

  The chain to actually land it, in order — this is a dependency-upgrade story, not a lockfile nudge:

  1. Move `markdown-stream`'s git pin `35c6db54` → `ad16fe5` (`crates/flux-markdown/Cargo.toml:28`).
  2. Lift the hold at root `Cargo.toml:149-150`: `ratatui = ">=0.29, <0.30"` → `0.30`. **Read the
     comment above it first** — it names all three crates that must move together.
  3. `crossterm` must match ratatui's backend (`>=0.28, <0.29` today).
  4. `flux-tui`'s `ansi-to-tui 7` tracks ratatui 0.29 and needs the version that tracks 0.30.
  5. Only then does `lru` become bumpable — it enters solely via ratatui, so nothing in this
     workspace names it directly and `cargo update -p lru --precise` cannot work until ratatui moves.

  ⚠ **Runs solo.** It changes manifests and both lockfiles, so by the disjointness rule it cannot
  share a wave. ⚠ And it touches the TUI's rendering stack across a ratatui major — expect real
  breakage in `flux-tui`, not just a version bump. A concurrent session was editing
  `crates/flux-tui/**` on 2026-08-01; check for dirty paths there before starting.
