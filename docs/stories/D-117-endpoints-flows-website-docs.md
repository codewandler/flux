---
id: D-117
title: "Website: endpoints concept page, `flux endpoint` CLI reference, saved-flows page"
pillar: Core
status: ready
priority: 23
design: docs/designs/datasource-discoverability.md
epic: datasource-discoverability
note: "the endpoint subsystem (D-25..D-32) has design/story/roadmap coverage but ZERO public docs — no concept page, `flux endpoint` absent from the CLI reference, one incidental line in plugins/gitlab.md; ~/.flux/flows + flow_list/flow_run + op.register have one paragraph in modules-and-programs.md"
---

# Website: endpoints concept page, `flux endpoint` CLI reference, saved-flows page

## Goal
The three undocumented capability clusters this epic touches become public documentation: how
endpoints work (and how the agent + operator register/enumerate/use them), the `flux endpoint` CLI,
and user-defined deterministic actions (saved flows + composite ops). A reader can go from "I have
a Postgres database" to "the agent queries it" using only the website.

## Acceptance
- [ ] New endpoints concept page (Agent section, sibling of `datasources.md`): the weak-reference
      model (`EndpointRef`, credential *location* never value, host-side resolution + injection),
      the five ops (`endpoint.discover/list/info/select/import`) and when they surface, the
      operator lifecycle (`~/.flux/endpoints.toml`, `flux endpoint list/show/resolve/import` — plus
      `add` once D-101 lands), and the sql-plugin end-to-end example (discover → select → `sql.query
      {endpoint_ref}` with host-terminated SCRAM).
- [ ] `flux endpoint` added to the website CLI reference (`website/docs/agent/cli.md`).
- [ ] New saved-flows page: `.flux/flows` + `~/.flux/flows` (precedence, legacy ops dirs),
      `flow_list`/`flow_run` from the agent side, `flux flow run` from the CLI side, agent-side
      `op.register` scopes (turn|session|project|global) and composite `expose` semantics.
- [ ] `datasources.md` cross-links: "which sources exist" (points at the D-114 `sources` op once it
      ships — coordinate; a records-vs-endpoints disambiguation box distinguishing the knowledge
      index from live endpoints, naming D-62 as the future bridge).
- [ ] Sidebar entries wired; any `.flux` snippets parser-validated per website conventions; no
      dead links (site build green).

## Progress
- 2026-07-09 filed from the datasource-discoverability grounding pass (see design doc).

## Notes
- Docs-coverage audit lives in the design doc (`docs/designs/datasource-discoverability.md`).
- Sequenced last in the epic (priority 23) so it can document D-114/D-115/D-116 outcomes — but the
  endpoints/flows material documents *shipped* behavior and can start any time.
- No consumer internals: examples stay generic (a Postgres database), per repo policy.
