---
id: C-200
title: "Config reference completeness — [wakeup], theme, and ~14 undocumented FLUX_* variables"
pillar: Core
status: done
priority: 12
epic: website-truth-and-identity
design: docs/designs/website-truth-and-identity.md
note: "agent/cli.md documents `flux wakeups` while the [wakeup] section that enables it is absent from config.md; the FLUX_EMBEDDINGS_* trio gates datasource embeddings and is undocumented entirely"
---

# Config reference completeness — `[wakeup]`, `theme`, and ~14 undocumented `FLUX_*` variables

## Goal
`website/docs/reference/config.md` (411 lines) is the strongest reference page on the site and has
two holes that leave shipped features unreachable: a reader can see `flux wakeups list | cancel`
documented at `agent/cli.md:44` and find no documented way to enable the feature, and an entire
capability — datasource embeddings — is gated behind three environment variables that appear
nowhere.

## Acceptance
- [x] `[wakeup]` is documented with its own section — the three keys, an example, the `flux wakeups`
      cross-link, and the fact that enabling the table is necessary but **not sufficient**
      (registering also needs approval-gated `host.write` authority; the table bounds an approved
      registration, it does not grant one).
- [x] The top-level `theme` key is documented in the top-level settings table.
- [x] The undocumented `FLUX_*` variables are documented — **26** of them, not the ~14 the story
      estimated (see Progress). The flat prose paragraph became six grouped tables.
- [x] `FLUX_STORE_DIR` is documented in `reference/storage.md`, along with a correction about
      `FLUX_HOME` that the story had backwards.
- [x] Failing-first: two new tests — `config_reference_documents_every_public_env_var` and
      `config_reference_documents_every_public_section`.
- [x] The exclusion list is explicit, commented by category, and named in the failure message.

## Progress
- **The env-var gap was nearly double the estimate.** Enumerating `"FLUX_*"` literals across
  `crates/*/src` and subtracting the classified set leaves **26** undocumented at HEAD, against the
  audit's ~14. The extra ones were mostly whole clusters the audit sampled rather than enumerated:
  the four A2A knobs, the three server knobs, both Vault knobs, and the diagnostics trio.
- **Rewrote the section rather than extending the sentence.** `## Environment overrides` was one
  prose paragraph listing 13 variables inline. Thirty-nine entries do not fit that shape, so it is
  now six tables by purpose — paths/workspace, safety/permissions, server/A2A, model/context/cost,
  datasource embeddings, interface/diagnostics. `FLUX_ALLOW_ALL` and `FLUX_ALLOW_SOURCE_BUILD` are
  called out as envelope-widening, with the reasoning left to the `security/` pages.
- **A claim I got wrong, caught by reading the code.** The first draft of the storage section said
  `--store` beats `FLUX_STORE_DIR`, which beats a `FLUX_HOME`-derived default. All three parts were
  wrong. `execution.rs:495-512`: `--store` **is exported as** `FLUX_STORE_DIR` (they are one
  mechanism, not two layers), and the fallback is `$HOME/.flux` — `FLUX_HOME` is not consulted at
  all. `FLUX_HOME`'s only reader is `usage.rs:2391`, and the same doc-comment records the real
  subtlety: **`flux usage` reads the global store and deliberately ignores `--store`**. That is a
  genuine footgun (a `--store` session's spend does not show up where you would look for it), so it
  is now an admonition on the storage page instead of a wrong precedence table.
- **The guard forced a classification I had missed.** First run failed on `FLUX_D116_PGPASS_` — not
  a variable at all but a `format!` prefix in `main.rs:1169`, minting a per-process credential key
  for the D-116 endpoint e2e. Added to `NON_PUBLIC_ENV` with that reason. This is the test behaving
  exactly as intended: a new `FLUX_*` literal must be documented or explicitly classified, and the
  failure message names the list to add to.
- **Exclusion strategy.** `FLUX_TEST_` is excluded wholesale by naming convention — no public
  variable would ever carry that prefix. Everything else is named individually in `NON_PUBLIC_ENV`,
  grouped under three commented reasons (test doubles, child-process markers, internal dev toggles),
  so an undocumented public variable cannot hide behind a broad substring rule. A `> 40` floor on
  the recovered set guards against the extractor silently returning nothing.
- **Second guard for sections.** `config_reference_documents_every_public_section` asserts each of
  the ten public tables both still exists on the schema (`pub <name>:`) and is mentioned in the
  reference — so a renamed table fails loudly rather than leaving a stale doc heading. `[wakeup]`
  failed this before the fix.
- Gate: `cargo test -p flux-cli --test website_contract` — 17 green. `npm run build` clean.

## Notes
- `FLUX_ALLOW_ALL` and `FLUX_ALLOW_SOURCE_BUILD` widen the safety envelope — document what they
  disable, in the same register the security pages use. `security/` keeps ownership of the
  reasoning; the reference states the effect.
- The exclusion list is the part that will rot. Prefer excluding by an explicit named set over a
  prefix/substring rule, so adding a test var doesn't silently exempt a public one.
