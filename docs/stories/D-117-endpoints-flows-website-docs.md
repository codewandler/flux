---
id: D-117
title: "Website truth pass: endpoints, saved flows, and drift guards"
pillar: Core
status: done
priority: 23
design: docs/designs/datasource-discoverability.md
epic: datasource-discoverability
note: "public-doc audit found stale SDK/config/plugin instructions, over-broad security claims, invalid Flux examples, missing endpoint/improvement/skills coverage, and no drift guards for those mirrors"
---

# Website truth pass: endpoints, saved flows, and drift guards

## Goal
The public website becomes a release-aligned, executable source of truth: current install/config/
security/plugin/operation instructions are correct, endpoints and deterministic reuse are discoverable,
all three product pillars have an onboarding path, and CI catches future drift in copied commands,
configuration, Flux-Lang examples, operation catalogs, plugin summaries, and release notes.

## Acceptance
- [x] New endpoints concept page (Agent section, sibling of `datasources.md`): the weak-reference
      model (`EndpointRef`, credential *location* never value, host-side resolution + injection),
      the five ops (`endpoint.discover/list/info/select/import`) and when they surface, the
      operator lifecycle (`~/.flux/endpoints.toml`, `flux endpoint add/list/show/resolve/import`,
      and `[[endpoint.static]]`), and the sql-plugin end-to-end example (discover → select →
      `sql.query {endpoint}` with host-terminated SCRAM).
- [x] `flux endpoint` added to the website CLI reference (`website/docs/agent/cli.md`).
- [x] New saved-flows page: `.flux/flows` + `~/.flux/flows` (precedence, legacy ops dirs),
      `flow_list`/`flow_run` from the agent side, `flux flow run` from the CLI side, agent-side
      `op.register` scopes (turn|session|project|global) and composite `expose` semantics.
- [x] `datasources.md` cross-links: "which sources exist" (points at the D-114 `sources` op once it
      ships — coordinate; a records-vs-endpoints disambiguation box distinguishing the knowledge
      index from live endpoints, naming D-62 as the future bridge).
- [x] Sidebar entries wired; any `.flux` snippets parser-validated per website conventions; no
      dead links (site build green).
- [x] Correct the audited truth gaps: crates.io SDK install/package selectors, `[private_net] web`,
      plugin `--dry-run`, current GitLab manifest surface, current Flux-Lang syntax, native-web ops,
      and the trusted-native/plugin-vs-host-capability security boundary (including the opt-in shell
      exception to argv-only host launches).
- [x] Add concise skills/roles and Improvement-loop entry points, make the CLI/config references
      cover every stable public surface, and expose the customer changelog on the site.
- [x] Generate or structurally check release notes, stable operation names, plugin-pack membership,
      CLI command coverage, config examples, SDK examples, and complete Flux code fences.
- [x] Public Pages deployment follows a published release; main/PR changes continue to build as
      previews without silently replacing the stable documentation.
      **Reversed 2026-07-30** (this criterion was met as written, then the policy itself was
      changed): the site now deploys on every push to `main`. PRs still only build. The trade this
      accepts is the inverse of the one above — the site may document an unreleased change, rather
      than lag the tree until the next release.

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).
- 2026-07-10 expanded from the post-v0.14.4 public-doc audit and started by explicit user request.
- 2026-07-10 completed: public IA and audited pages corrected; endpoint, saved-flow, skills/roles,
  Improvement, and customer-changelog entry points added; executable drift guards and release-bound
  Pages deployment added; Docusaurus production build and focused contract tests green.

## Notes
- Docs-coverage audit lives in the design doc (`docs/designs/datasource-discoverability.md`).
- Sequenced last in the epic (priority 23) so it can document D-114/D-115/D-116 outcomes — but the
  endpoints/flows material documents *shipped* behavior and can start any time.
- No consumer internals: examples stay generic (a Postgres database), per repo policy.
