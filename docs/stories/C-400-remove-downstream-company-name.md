---
id: C-400
title: "Remove the downstream company name from the tree"
pillar: Core
status: ready
priority: 5
note: "36 occurrences across 10 files, 14 of them in PUBLISHED crate source (all inside #[cfg(test)]). Audited 2026-08-01: nothing is load-bearing, but three tests carry paired fixture/assertion literals that must change together"
---

# Remove the downstream company name from the tree

## Goal

No flux-family repository names a downstream company. A product's name is true only of that product,
so it belongs in that product's repository — the same boundary rule the ecosystem design applies to
code, applied to the documentation and fixtures.

## Acceptance

- [ ] `git grep -i babelforce` returns nothing in the tracked tree.
- [ ] The three crate-source files are anonymised **as pairs where the literals are coupled**:
      - `crates/flux-providers/src/bedrock.rs` (10 occurrences, all inside the `#[cfg(test)]` module):
        an AWS config fixture and its assertions, a `profile_section` assertion, and
        `sso_cache_path_is_sha1_of_session_under_home` — the last computes its expected SHA-1 from the
        same literal it passes in, so the `&str` and the `b"..."` byte literal **must change together**.
      - `crates/flux-plugin/src/host.rs` (2) and `crates/flux-plugin/src/pack.rs` (2) — a plugin repo
        slug used as input and expected output in the same assertion.
- [ ] `docs/designs/connector-backed-storage-facade.md` (12) uses the vocabulary the document already
      established for itself — "the hosted platform", "platform-managed", "platform-owned" — and the
      proposed `` `babelforce` `` binding identifier is renamed. The design is `proposed` and nothing
      implements it, so the identifier is free to move.
- [ ] `docs/designs/grounded-knowledge.md` (2) drops the company adjective; the load-bearing part
      ("a downstream ai-agents service", "a customer-facing knowledge feature") is retained.
- [ ] Stories corrected: D-87 (3, the closest to a real citation — the *shape* is what carries the
      reason, not the private artifact's name), C-37 (1), D-130 (1, a machine-local absolute path that
      resolves on exactly one machine), D-221 (2, one of them in frontmatter).
- [ ] `docs/stories/README.md` is **regenerated with `/track:board`**, not hand-edited — its
      occurrence is a verbatim echo of D-221's frontmatter inside the generated region.
- [ ] Gate green in both workspaces. No behaviour, test-name or public-API change is expected; if one
      appears, stop and report rather than adjusting an assertion to match.

## Progress
- (not started)

## Notes
- Audited 2026-08-01. All 14 crate-source occurrences are inside `#[cfg(test)]` modules — they ship in
  the `.crate` tarball and are browsable on docs.rs, but no production code, public item, test
  function name, or doc comment carries the name.
- Suggested substitutions: crate tests → `acme`; design/story prose → `platform*`; D-87 →
  `<vendor>` placeholders; D-130 → drop the absolute path.
- **Out of scope, deliberately:** `.flux/config.toml` carries a real internal hostname under
  `[private_net.plugins]`. It is gitignored and has never shipped, and rewriting it would break a
  working local setup. The lesson it carries is to avoid pasting `private_net` examples from a live
  config into docs.
