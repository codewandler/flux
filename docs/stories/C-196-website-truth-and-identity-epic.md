---
id: C-196
title: "Website truth and identity — the public site tells the truth and looks like the product (epic)"
pillar: Core
status: done
priority: 8
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "AUDIT EPIC — every child cites a path:line from the 2026-07-29 audit against 0.33.1; the site documents multi-line strings as nonexistent and omits the entire HTTP session API, while ignoring the project's own brand spec in assets/README.md"
---

# Website truth and identity — the public site tells the truth and looks like the product (epic)

## Goal
A 2026-07-29 audit of all 64 pages under `website/docs/` against the tree at `0.33.1` found the
site structurally strong — all 26 CLI subcommands, all 43 node kinds, all 21 expr builtins covered,
three regions generated and drift-guarded — but carrying four actively wrong claims and a tail of
staleness, and ignoring the project's own written brand spec entirely. Close both gaps, and leave
coverage assertions behind so that adding a route, an op, or a config key without documenting it
fails the gate rather than surfacing in the next audit.

## Acceptance
- [x] C-197 … C-204 are all done, each with the failing-first test or verification its story names.
- [x] **Six** new or tightened coverage assertions in `crates/flux-cli/tests/website_contract.rs`
      (the epic promised three): server routes (C-199), registered ops (C-201), public config
      sections and `FLUX_*` vars (C-200, two tests), multi-line-string syntax (C-198), and TUI
      keybindings + themes (C-202). The suite went 13 → 18 tests.
- [x] `npm run build` clean under `onBrokenLinks: 'throw'` — and now `onBrokenAnchors: 'throw'`
      too. Every new page is reachable from `sidebars.js`.
- [x] Favicon, navbar logo, social card and accent all derive from `assets/`; the palette measures
      4.65:1 (light) and 10.62:1 (dark) for link text.
- [x] No generated region was hand-edited.

## Progress
- 2026-07-29 — epic opened from the audit, then completed the same day. Design:
  [website-truth-and-identity.md](../designs/website-truth-and-identity.md). C-197…C-201 were run
  sequentially; C-202, C-203 and C-204 were run as five parallel agents over disjoint file sets,
  with `sidebars.js` and `website_contract.rs` held centrally so concurrent writers could not
  collide.

### Every count in the audit was low
Not marginally — systematically, and always in the same direction. The audit sampled where it should
have enumerated:

| Surface | Audit said | Actually |
|---|---|---|
| Undocumented server routes | 12 total, 3 documented | **14** total (two multi-agent card routes were missed because `rustfmt` wraps those `.route(` calls) |
| Undocumented `FLUX_*` vars | ~14 | **26** |
| Missing ops in `ops.md` | 11 | **27** (6 `reflect`-group agent-loop stages + 21 core) |
| Missing subcommands in `docs/usage.md` | 5 | 5 ✓ (the one count that held) |

This is the argument for the epic's central rule. A hand-audited list is a snapshot that starts
decaying immediately; the six assertions enumerate from the registry, the router, the config schema
and the TUI's own help table, so the next gap fails `cargo test` instead of waiting for an audit.

### Four claims corrected before they shipped
Each was caught by checking source rather than trusting a plausible-sounding statement:
- `--store` beats `FLUX_STORE_DIR` beats a `FLUX_HOME` default — **all three parts false**.
  `--store` *is* `FLUX_STORE_DIR` (one mechanism), the fallback is `$HOME/.flux`, and `FLUX_HOME`'s
  only reader is `flux usage`, which deliberately ignores `--store`. That last part is a real
  footgun and is now documented.
- Braces in a `fmt` template swallow a following placeholder — false; the unbound branch re-scans
  (`runtime.rs:3818-3821`).
- `--max-tokens` is a global flag — false; it lives on `AgentFlags`, flattened only into the
  agent-path subcommands. Only seven arms are `global = true`.
- `eval_sessions` returns a `flow_db` key — false; it *reads* `flow_db` and *emits* `db`
  (`flux-eval/src/ops.rs:352-355`).

### Two code bugs found and filed, not papered over
- **C-206** — guidance-fragment discovery uses the recursive `walk_files` while both the code's own
  contract and the public page promise a flat, `ls`-auditable directory.
- **C-207** — `KUBECONFIG` gates whether the kubernetes ops are surfaced but is absent from
  `SAFE_ENV`, so it never reaches `kubectl`.

Neither was fixed here: both need a decision (constrain the walk vs. change the contract; forward
the var vs. stop honouring it), and this epic's remit is the docs. In both cases the documented
contract is the correct one, so the pages were deliberately left stating it.

### Three doc bugs fixed outside the original scope
`security/plugin-sandbox.md` described the `process` capability as an exact `argv[0]` allow-list
(it is an argv-**prefix** matcher since C-90 — the property that lets `kubectl get` be granted
without `kubectl delete`); `plugins/using-plugins.md` called `sql` PostgreSQL-only (MySQL/MariaDB
shipped in D-196…D-198); and `sdk/datasources.md` pinned `codewandler-flux-sdk = "0.25"` against a
0.33.1 workspace. Plus one genuinely broken anchor in `sdk/flow-client.md`, which is why
`onBrokenAnchors` is now `'throw'`.

## Notes
- **Why the false claim leads.** `website/docs/language/flows-and-syntax.md:118` states *"Strings
  are single-line"*, on the page that owns text syntax, while triple-quoted verbatim strings have
  shipped since L-39 (`crates/flux-lang/src/lexer.rs:179-186`, `parse.rs:1482`,
  `crates/flux-lang/docs/syntax.md:316-352`). `"""` has **zero** occurrences under
  `website/docs/language/`. An omission costs a reader a search; a false statement costs them the
  belief that the workaround is necessary.
- **The HTTP API gap is the largest by surface.** `crates/flux-server/src/lib.rs:585-607` registers
  twelve routes; `agent/a2a.md:69-73` documents three. `/sessions/{id}/usage` and `/usage` are not
  even in the README.
- **Contrast is the binding constraint on the palette.** `#0bbf83` is ~2.2:1 on white and fails
  WCAG AA for text, so light mode needs a darkened ramp; `#2be6a5` is used verbatim in dark mode.
  The neutrals already match the brand — `--flux-hero-heading` and the dark background are brand
  ink `#141a18` — which is the evidence that the teal accent was drift, not a decision.
- **Ordering is by damage, not by page count.** The two P0 stories (C-198, C-199) correct claims a
  reader would act on. C-197 is placed with them because the site currently has no favicon and no
  social card at all, so every shared link renders blank — the cheapest visible win in the epic.
- Scope deliberately excluded: documenting all 19 plugins (the catalog table is already complete —
  two worked guides establish the pattern), expanding `agent/improvement.md` into a pillar page
  (the pillar is ON HOLD since 2026-07-06 and the page should say so), and adding a drift guard for
  the editor-tooling mirrors (`AGENTS.md:159`) — that is a question about all four mirrors, not
  just the website's.
- ~~Nothing here is publicly visible until the next release: `.github/workflows/website.yml` deploys
  on `release: published` only. That is D-117's design and is not being changed.~~
  **Superseded 2026-07-30:** the site now deploys on **every push to `main`** (plus `release` and
  `workflow_dispatch`), and the push trigger carries no `paths` filter, so any commit that can change
  a rendered page republishes it. D-117's release-pinned policy was reversed deliberately: a stale
  public page that contradicts the tree costs more than a page describing an unreleased change.
  `website/docs/intro.md` states the new policy for readers.
