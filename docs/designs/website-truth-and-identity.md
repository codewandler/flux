# Website truth and identity — the public site tells the truth and looks like the product

Story: [C-196](../stories/C-196-website-truth-and-identity-epic.md) · Pillar: Core · Status: design

Source: a 2026-07-29 audit of every page under `website/docs/` against the tree at `0.33.1` — the
clap surface in `crates/flux-cli/src/args.rs`, the routers in `crates/flux-server/src/lib.rs`, the
config schema in `crates/flux-config/src/lib.rs`, the op registries, and the lexer/parser. Every
story under this epic cites a `path:line`. The brand half is audited against
[`assets/README.md`](../../assets/README.md), which is the project's own written brand spec.

## Where the site actually stands

It is worth being precise, because the headline is not "the docs are bad." Sixty-four pages,
eleven thousand lines, and the structural coverage is genuinely strong: **all 26 CLI subcommands,
all 43 node kinds, all 21 expr builtins** are documented, three regions are generated from the Rust
source behind `<!-- BEGIN generated: -->` markers, and `crates/flux-cli/tests/website_contract.rs`
is 732 lines of executable contract over the hand-maintained mirrors. D-117 and L-42 did real work
and it held.

What drifted is narrower and, precisely because the rest is trustworthy, more damaging: **a reader
has no way to tell which parts are the audited ones.**

## The two gaps

### 1. Content — four claims that are actively wrong, then a tail

The worst is not an omission but a false statement. `website/docs/language/flows-and-syntax.md:118`
tells authors *"Strings are single-line; embed newlines with `\n` escapes."* Triple-quoted verbatim
strings have shipped since L-39 — `crates/flux-lang/src/lexer.rs:179-186`,
`crates/flux-lang/src/parse.rs:1482`, specified at `crates/flux-lang/docs/syntax.md:316-352`. The
string `"""` appears **zero times** anywhere under `website/docs/language/`. The recommended
spelling for multi-KB prompts is documented as not existing, on the page that owns text syntax.

Second: the **entire HTTP session API is absent**. `crates/flux-server/src/lib.rs:585-607` registers
twelve routes; `website/docs/agent/a2a.md:69-73` documents three of them. `POST /sessions`, the SSE
stream, `/webhook`, and both usage endpoints — the reason `flux app run --serve` exists — appear
nowhere on the site. Two of them aren't even in the README.

Then a tail that is ordinary staleness: ops registered but uncatalogued (`schedule_wakeup`,
`ai_segment`, the nine-op eval family), the `[wakeup]` and `theme` config keys, ~14 undocumented
`FLUX_*` variables including the whole `FLUX_EMBEDDINGS_*` trio, the `agent_loop` program
declaration, a TUI surface documented in `docs/usage.md` but reduced to one table row on the site,
two stub pages, and 17 of 19 plugins without a setup guide.

And a subtler class, worth separating because the fix points the other way: **in four places the
website is right and the in-repo docs are wrong.** `docs/usage.md` is missing five shipped
subcommands and calls `read_many` a legacy alias when `crates/flux-tools/src/lib.rs:1669-1671`
registers it saying *"prefer this over sequential `read` calls."* `README.md` documents a
`flux run --program` flag that does not exist in `args.rs`. Those get fixed in the repo, not on the
site.

### 2. Identity — the site is the only surface that ignores the brand spec

`assets/README.md` is not a mood board. It states one visual idea — *plans cross an explicit
execution gate* — names the production assets, fixes the palette (signal `#0bbf83` light /
`#2be6a5` dark, ink `#141a18`), and enumerates what is forbidden: no gradients, shadows, glow,
mascots, or orbital decoration on the core identity. The README hero and its badges follow it.

The website follows none of it. No `favicon` key in `docusaurus.config.js` at all. No navbar logo.
No social card, so every shared link renders blank. The accent is `#275a68`, a petrol teal that
appears in no brand asset. `custom.css:9` names `Inter` with no `@font-face` and no stylesheet
link, so it has silently fallen back to the system stack since the day it was written. No
`prism.theme` is set, so Docusaurus's `palenight` default applies to **both** modes and code blocks
render dark on the light theme.

The neutrals are the tell that this was drift rather than a decision: `--flux-hero-heading: #18201d`
and the dark `#141a18` are already brand ink. Someone reached for the right greys and the wrong
accent.

## Approach

Eight stories, split 1 visual / 7 prose, each independently landable.

**The organizing rule, inherited from D-117 and L-42: anything that can drift gets a test.** Two
harnesses already exist and both get extended rather than duplicated —
`crates/flux-cli/tests/website_contract.rs` for hand-maintained mirrors, and
`crates/flux-lang/tests/website_in_sync.rs` for marker-delimited generated blocks. Three of the
content stories add a coverage assertion whose failure mode is *"you added a route/op/config key and
did not document it"*, which is the only durable answer to a truth pass. A one-time correction that
ships without a guard is a story we will file again in three months.

Two consequences of that rule shape the work:

- New prose examples go in **complete ` ```flux ` fences**, because
  `complete_flux_fences_parse_and_legacy_syntax_stays_out` already parses every one of them. Story 2
  is drift-guarded essentially for free.
- Story 5 does **not** add a parallel op check. `operations_reference_covers_the_registered_public_catalog`
  exists and currently passes while three op families are missing — so its scope is wrong, and the
  work is to tighten it.

On the visual side the constraint is contrast, not taste. `#0bbf83` is roughly 2.2:1 on white and
**fails WCAG AA for link text**, so the brand green cannot be dropped into `--ifm-color-primary`
unmodified. Light mode gets a darkened ramp verified ≥4.5:1; `#0bbf83` stays for non-text accents;
dark mode uses `#2be6a5` verbatim, where it passes comfortably.

## Alternatives considered

- **One "website truth pass" story, as D-117 was.** D-117 worked, but it was a single large diff
  whose acceptance ran to nine checkboxes, and re-reading it now it is hard to tell which guarantees
  are still guarded. Eight stories each naming one failing-first test are auditable individually.
- **Keep the petrol teal and only add the missing assets.** Cheaper and lower-risk. Rejected because
  it leaves the site the one surface that disagrees with the project's own written brand spec — the
  favicon and logo would then *introduce* the mismatch rather than resolve it.
- **Expand `agent/improvement.md` into a proper pillar page.** Rejected. `docs/vision.md` calls the
  Improvement loop aspirational and the board records the pillar ON HOLD since 2026-07-06 with the
  headline gain unproven. The honest fix is to say so on the page, not to write around it.
- **Document all 19 plugins.** Rejected as scope. The catalog table at `using-plugins.md:44-51` is
  already complete and correct; two more worked guides (`kubernetes`, `sql`) establish the pattern.

## Risks & open questions

- **The social card cannot be an SVG.** Scrapers won't render it, so a 1200×630 PNG must be derived
  from `readme-hero.svg` and committed. `rsvg-convert` and `magick` are both present locally; the
  raster is a build artifact checked into `static/img/`, which is the same trade already made for
  `architecture_v0.png`.
- **`Inter` is a fork in the road, not a bug fix.** Self-hosting it under `static/fonts/` is the
  brand-faithful option but adds ~100KB and a `@font-face` block; dropping it commits to the system
  stack. An external CDN is out — the project is local-first and chose offline search over Algolia
  for exactly this reason. Decide in the story, don't leave it declared-but-absent either way.
- **The Prism grammar at `website/src/theme/prism-include-languages.js` has no drift guard**
  (`AGENTS.md:159`). Story 2 documents a token the grammar may not highlight. Fixing that is in
  scope; *guarding* it is a bigger question that belongs to the editor-tooling mirrors as a whole,
  and is deliberately not opened here.
- **Nothing here is publicly visible until the next release.** `.github/workflows/website.yml`
  deploys on `release: published` only; `main` and PRs build for validation. That is D-117's design
  and stays — it just means the epic's payoff is release-gated.
- **The `read_many` conflict must be resolved, not papered over.** `docs/usage.md` and the registry
  description contradict each other; the story decides which is true before editing either.

## Acceptance / done

The union of the eight stories' acceptance, plus:

- `cd website && npm run build` is clean with `onBrokenLinks: 'throw'`.
- Every new page is reachable from `sidebars.js` (which is fully explicit, and which the llms.txt
  plugin reads directly for ordering).
- Three new or tightened coverage assertions exist such that adding a server route, an op, or a
  public config key without documenting it **fails the gate**.
- The site's favicon, navbar logo, social card, and accent derive from `assets/`, and the palette
  passes WCAG AA for text in both themes.
- No `<!-- BEGIN generated: -->` region was hand-edited.

## Stories

- [C-196](../stories/C-196-website-truth-and-identity-epic.md) — the epic
- [C-197](../stories/C-197-website-brand-identity.md) — brand identity on the public site
- [C-198](../stories/C-198-website-multiline-strings-memo.md) — P0: multi-line strings and `memo`
- [C-199](../stories/C-199-website-http-api-reference.md) — P0: HTTP API reference page
- [C-200](../stories/C-200-website-config-and-env-completeness.md) — P1: config keys and env vars
- [C-201](../stories/C-201-website-ops-catalog-and-agent-loop.md) — P1: ops catalog and `agent_loop`
- [C-202](../stories/C-202-website-tui-page-and-global-flags.md) — P1: TUI page and global flags
- [C-203](../stories/C-203-website-thin-pages-plugins-sdk.md) — P2: thin pages, plugin guides, SDK
- [C-204](../stories/C-204-repo-docs-reverse-drift.md) — P3: reverse drift in the in-repo docs
