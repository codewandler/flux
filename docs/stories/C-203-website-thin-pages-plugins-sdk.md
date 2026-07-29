---
id: C-203
title: "Thin pages, two more plugin guides, and the SDK surfaces with zero site coverage"
pillar: Core
status: done
priority: 15
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "plugins/authoring.md is 40 lines of bullets and a GitHub link; whatif/subagents/observe/Fork are exported from flux-sdk with zero hits under website/docs/sdk/"
---

# Thin pages, two more plugin guides, and the SDK surfaces with zero site coverage

## Goal
Four categories on the site have a page that undercuts the category. The Plugins section's
authoring page is a 40-line stub; the Improvement section's only page is a 50-line stub for a
pillar the site calls co-equal; 17 of 19 plugins have no setup guide; and four exported SDK modules
have no site presence at all. Raise the floor without overselling.

## Acceptance
- [x] `website/docs/plugins/authoring.md` rewritten, 40 → 318 lines: the host-does-all-privileged-IO
      rule, a real manifest example, the lifecycle in two parts, a capability→gate→host-behaviour
      table, and a typed SDK snippet with a `MockHost` test.
- [x] `website/docs/agent/improvement.md` states the pillar's actual status in a `:::note` and
      documents what genuinely works, linking C-201's op tables rather than duplicating them.
- [x] `website/docs/plugins/kubernetes.md` (239 lines) and `plugins/sql.md` (230 lines) landed and
      are wired into `sidebars.js`.
- [x] The SDK's `subagents`, `observe` and `Fork` / `ReplayReport` are documented. `whatif` turned
      out **not** to be undocumented — see Progress.
- [x] "Related docs" footers added to all five pages that lacked one.
- [x] Rust snippets derive from the real API; the existing SDK contract assertions still pass.
- [x] `npm run build` clean.

## Progress
Run as four parallel agents over disjoint file sets, with `sidebars.js` and
`crates/flux-cli/tests/website_contract.rs` held centrally so concurrent writers could not collide.

- **The `whatif` premise was wrong.** The story listed it among the four zero-coverage SDK surfaces.
  `website/docs/sdk/agent-lab.md` already documents `Session::what_if()`, the builder,
  `Counterfactual`, `WhatIfSpec` and `Client::what_if_over` in depth. Cross-linked instead of
  duplicated. The genuinely-absent three were `observe`, `subagents` and `Fork`/`ReplayReport`.
- **`intro.md` had to give too.** The improvement page now states the pillar is on hold, which
  directly contradicted `intro.md`'s "three pillars" list calling it co-equal. Rather than leave the
  site arguing with itself, `intro.md` now marks the pillar on hold in the list itself.
- **A stale version pin nothing guarded.** `sdk/datasources.md` pinned
  `codewandler-flux-sdk = "0.25"` while the workspace is at 0.33.1. Replaced with `cargo add`, which
  cannot go stale.
- **Two real doc bugs found in pages outside the story's scope, both verified and fixed:**
  - `security/plugin-sandbox.md` described the `process` capability as an *exact `argv[0]`*
    allow-list. That is the pre-C-90 model. `process_grant_allows`
    (`crates/flux-plugin-protocol/src/lib.rs:581`) matches a whitespace-separated **token prefix** —
    which is the whole reason a plugin can be granted `kubectl get` without `kubectl delete`. The
    page now says argv-prefix and explains the distinction, since it is the security property.
  - `plugins/using-plugins.md` described `sql` as "PostgreSQL read-only". MySQL/MariaDB shipped in
    D-196…D-198 (`plugins/sql/src/main.rs:23-29`). Corrected.
- **Two code bugs found and filed rather than papered over:**
  - **C-206** — guidance-fragment discovery calls the *recursive* `walk_files` while
    `context.rs:22-24` and the public page both promise a flat directory "auditable with one `ls`".
    A fragment at `.flux/context.d/sub/x.md` does load. Deliberately did **not** reword the page:
    the documented contract is the right one, so the defect is in the code.
  - **C-207** — `KUBECONFIG` decides the kubernetes group is surfaced
    (`flux-runtime/src/lib.rs:1787`) but is absent from `SAFE_ENV`
    (`flux-system/src/lib.rs:1865-1888`), so it is never forwarded to the `kubectl` subprocess. A
    user with a non-default kubeconfig gets ~24 ops offered and every one fails. `kubernetes.md`
    carries a `:::caution` with the workaround until that lands.
- **Also reported, not acted on:** `flux plugin status sql` offers `flux auth set sql password` and
  would print `✓ stored token`, but the static-endpoint handshake reads only `std::env` — a stored
  token shows green and then fails at call time. The page documents env-only; the CLI hint is a
  genuine bug and wants its own story if it recurs.
- Gate: `cargo test -p flux-cli --test website_contract` — 18 green. `npm run build` clean.

## Notes
- `kubernetes` and `sql` are chosen because they are the two highest-leverage remaining plugins and
  because `sql` is the one the endpoints documentation already uses in its worked example. Examples
  stay generic — a Postgres database, never a named downstream consumer (repo policy, restated in
  D-117).
- The Improvement page is the one place in this epic where the fix is to claim *less*. Resist
  writing around the hold; a reader who acts on an aspirational page loses more than one who reads
  a short honest one.
