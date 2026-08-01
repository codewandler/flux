---
title: Concept-docs review — the new Concepts rewrite, Ecosystem page, sync script and CI gate
date: 2026-08-01
kind: internal-review
lens: diff-review-of-uncommitted-work
method: >
  multi-agent /code-review of the uncommitted concept-docs work (new docs/concepts.md,
  docs/ecosystem.md, docs/designs/ecosystem.md, scripts/sync-website-docs.sh, the ci.yml job, the
  website/docs/ mirrors and the sidebars.js/docs/README.md wiring). Eight finder angles produced
  ~40 candidates; after dedup, each survivor was verified against the tree — the website_contract
  test suite was actually run, struct definitions, AGENTS.md invariants and crate/package existence
  were checked directly. 10 findings survive, all CONFIRMED. This is a review record only, not a
  change request — nothing was fixed.
reviewer: agent
subject:
  repo: codewandler/flux
  commit: 5995f350 (v0.45.0) + uncommitted working tree
  scope: >
    tracked modifications (ci.yml, website/docs/concepts.md, sidebars.js, docs/README.md) and
    untracked files (docs/concepts.md, docs/ecosystem.md, docs/designs/ecosystem.md,
    scripts/sync-website-docs.sh, website/docs/ecosystem.md)
verdict: >
  Prose-strong, but the work lands with a red gate — `cargo test -p flux-cli --test
  website_contract` fails 3 tests today — and the vocabulary pages misstate the language they
  define. The sync script duplicates the existing website_in_sync.rs golden mechanism and violates
  the repo's own C-326 "regenerating run is RED" invariant. No CHANGELOG entry, no story, and a
  tracked/untracked split that makes a partial commit break CI or the site build.
triage:
  kind: single
  status: open
  owner_stories: []
  aggregated_into: null
---

# Concept-docs review — 2026-08-01

Review of the new concept documentation work: the `docs/concepts.md` vocabulary rewrite, the new
`docs/ecosystem.md` Fundamentals page (with `docs/designs/ecosystem.md`), the
`scripts/sync-website-docs.sh` sync script and its CI job, and the website mirrors/sidebar wiring.

**Headline:** the gate is red *right now* — 3 `website_contract` test failures verified by direct
run — and the pages that define the project's vocabulary teach syntax and fields the implementation
rejects. Findings are ordered most-severe first.

## Findings

### 1. Ecosystem `flux` fence uses syntax that does not parse — 2 CI test failures

`docs/ecosystem.md:168` (mirrored to `website/docs/ecosystem.md`)

The ```flux channel/trigger example uses brace-and-equals syntax that Flux-Lang does not parse,
turning the CI gate red and publishing a flagship example that cannot run.

Verified by running `cargo test -p flux-cli --test website_contract`:
`complete_flux_fences_parse_and_legacy_syntax_stays_out` and
`public_flux_examples_are_canonical_formatter_fixed_points` both FAIL on
"website/docs/ecosystem.md Flux block 2: parse error". Real syntax is bareword + indented
attributes (`channel support` / `  kind "connector"` — see `examples/channels-app.flux` and
`website/docs/agent/programs.md:51`), never `{ k = v, ... }`. A user copying the block into
`flux app run` gets a parse error on the site's introductory Fundamentals page.

### 2. Symbol rewrite breaks a pinned contract test and reintroduces the `$` sigil

`docs/concepts.md:96` (mirrored to `website/docs/concepts.md`)

The Symbol rewrite deletes the exact phrase a shipped contract test pins and reintroduces the
legacy `$` sigil (`$resp`) that the guard exists to keep off this page — the third CI test failure.

`crates/flux-cli/tests/website_contract.rs:1033-1036` asserts `website/docs/concepts.md` contains
the literal "symbols such as `src` or `tests`"; verified by test run:
`homepage_flux_example_is_canonical_and_analyzes_against_the_live_catalog` FAILS with "the
Concepts symbol examples must use the formatter's canonical bare spelling". The new text
"(`src`, `tests`, `$resp`)" shows bare and `$`-prefixed spellings side by side, teaching the exact
non-canonical form the assertion was written to police.

### 3. Connector-channel sample uses fields that do not exist

`docs/ecosystem.md:171`

The sample uses `mode` and `exchange` — neither exists — omits the required `connector` id, and
misuses `binding`. Because `ConnectorSettings` is deliberately not `deny_unknown_fields`, the
fictitious fields would be silently ignored, not rejected.

`crates/flux-channels/src/config.rs:171-205` defines `ConnectorSettings` as {connector (required),
binding (required), service, manifest, credentials, addr, path, token} — no `mode`, no `exchange` —
and adapters only ever construct the local `ConnectorChannel`. An operator writing the documented
channel either fails on the missing `connector` field or silently starts a local listener with no
exchange involved — the opposite of what the page promises. `docs/designs/ecosystem.md` compounds
it by asserting in present tense that "a `mode = "remote"` setting opens a stream instead of
binding a listener".

### 4. Concepts teaches the wrong trigger syntax twice in prose

`docs/concepts.md:209` (and again at line 225)

The vocabulary page shows `trigger { on = "<channel>.<event>", run = "<journey>" }` — braces, `=`,
comma, and no name, none of which parse.

`crates/flux-lang` `TriggerDecl` requires a name plus indented `on "…"` / `run "…"` attributes
(`examples/channels-app.flux:16-18`, `website/docs/agent/programs.md:51`). This is the page every
other doc says to read first, so a reader (or a model grounded on Concepts) emits the brace form
and gets "expected a `flow` header or a top-level declaration", with nothing on the page showing
the real form.

### 5. `workspace.write` listed as an effect, but it is a capability

`docs/concepts.md:82`

The Effect entry lists `workspace.write` as an example effect; the effect parser rejects it — the
disambiguation page conflates the two axes it exists to separate.

`crates/flux-lang/src/cst_decode.rs` `parse_effects` accepts only
read/write/network/model/process/browser/filesystem/file_system/local_system;
`effects ["workspace.write"]` on a composite op returns "unknown effect". `workspace.write` is
asserted as a *capability* in `crates/flux-cli/tests/mock_smoke.rs:103` — the very term Concepts
defines two entries later, so a reader is taught that a capability is an effect on the page whose
stated job is keeping such pairs apart.

### 6. flux-exchange documented as shipped; it does not exist

`docs/ecosystem.md:127`

flux-exchange is described in the present indicative with a copyable `cargo run -p flux-exchange`
command, but no such package, repo, or feature exists anywhere — the design doc itself calls this
work "the charter for codewandler/flux-exchange" (proposed).

Verified: no `flux-exchange` member in the workspace (`cargo run -p flux-exchange` yields "package
ID specification did not match any packages"), and the "Where things live" table renders its GitHub
URL as bare text because the repo does not exist. A user on the Fundamentals page (`sidebars.js`
places Ecosystem second) runs the documented command, it errors, and nothing on the page
distinguishes the two shipped projects from the unshipped one; `subscribe`, per-tenant grants, and
the remote binding likewise do not exist in any tree.

### 7. Sync script: unknown args silently select destructive write mode, then report green

`scripts/sync-website-docs.sh:40`

Any argument other than the exact literal `--check` silently selects WRITE mode, and write mode
overwrites both website pages then exits 0 printing green `ok` — the exact
regenerate-then-report-green anti-pattern the repo pinned shut as C-326.

`[[ "${1:-}" == "--check" ]] && check_only=1` has no unknown-arg rejection: `--dry-run`, `--help`,
or a typo'd `--chekc` overwrites `website/docs/{concepts,ecosystem}.md` (destroying any local
edits) and reports success. `AGENTS.md:178` states the hard invariant: "A regenerating run is RED
on purpose … a run that wrote a golden verified nothing and must not be reportable as a passing
check" — every existing golden guard writes then FAILS with "REGENERATED <path>"; this script
writes and exits green.

### 8. The script duplicates the existing `website_in_sync.rs` mechanism

`scripts/sync-website-docs.sh:1`

The whole script re-implements a load-bearing mechanism:
`crates/flux-lang/tests/website_in_sync.rs` already mirrors root-authored markdown into
`website/docs/` with drift-fail semantics, and the new parallel path is invisible to the release
cut and to the existing doc contract.

The repo now has two answers to "how does a website page stay synced", with different arming
(`FLUX_UPDATE_GOLDEN=1` vs bare re-run), different banners (BEGIN/END generated: fences naming the
regenerate command vs a one-line banner naming none), and different CI shape —
`scripts/cut-release.sh:143` regenerates only the `FLUX_UPDATE_GOLDEN` path, so these pages are
never refreshed at release cut; `website_contract.rs` assertions now target the generated copy
that the script overwrites; and the new job is the only checkout-only gate in ci.yml without a
`--self-test` leg (crate-versions, action-pins, no-direct-io all prove their checker can fail)
while spending a dedicated ubuntu runner on a sub-second diff that could be a step in the existing
action-pins job. Extending `website_in_sync.rs` (~15 lines) deletes the script, the job, and the
divergence.

### 9. Eight-plus relative links 404 in the docs/ tree, and no linter covers it

`docs/concepts.md:29` (and throughout both new docs/ pages)

Every relative link in the two new `docs/` pages is authored for the website tree, so they 404
where the files actually live — and the same commit promotes them to the top two rows of the
`docs/README.md` contributor map, with no linter covering that side.

`docs/` has no `agent/`, `language/`, `security/`, `plugins/` directories and no
`infrastructure.md`, so `./agent/saved-flows.md`, `./agent/agent-loop.md` (x2),
`./security/plugin-trust.md`, `./language/overview.md`, `./infrastructure.md` (concepts.md) and
`./agent/agent-loop.md`, `./infrastructure.md`, `./plugins/using-plugins.md` (ecosystem.md) all
404 on GitHub. A contributor following the new `docs/README.md` rows ("the shared vocabulary —
every term the docs use") hits dead ends on nearly every onward link, and `docs-links.yml` globs
only `README.md website/docs/**/*.md` so nothing ever catches it.

### 10. No CHANGELOG entry, no story, and a partial-commit landing hazard

`CHANGELOG.md:7`

The change ships a new public site page, a full Concepts rewrite, a new script and a new CI gate
with no CHANGELOG entry and no story — and the tracked/untracked file split makes a partial commit
that breaks CI or the site build easy.

`AGENTS.md:17` ("Non-trivial behavior needs a story or design trail, a failing-first test, and a
CHANGELOG entry") and step 6 ("New or unscoped work? Create a story … first") are both unmet:
`## [Unreleased]` is empty and no story references this work (`docs/designs/ecosystem.md` is the
only design in `docs/designs/` whose header carries no Stories: link). Landing risk: ci.yml,
`website/docs/concepts.md` and `sidebars.js` are tracked modifications while the script,
`docs/concepts.md`, `docs/ecosystem.md` and `website/docs/ecosystem.md` are untracked —
`git commit -a` commits the CI job but not the script it runs (every PR fails with "No such file
or directory") and commits a `sidebars.js` `ecosystem` id with no doc file, which throws in the
llms-txt sidebar-corpus validation and breaks the site build.
