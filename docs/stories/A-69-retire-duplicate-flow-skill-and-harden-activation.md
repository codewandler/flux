---
id: A-69
title: Retire duplicate planner skill and make activation explicit
pillar: Agent
status: done
note: "C-54 live trace: a trivial flow sentence activated a redundant 19.4 KB flux-flow skill; after removal, generic 'agent' activated the unrelated self-improvement skill."
---

# Retire duplicate planner skill and make activation explicit

## Goal

Stop unproven skills from silently inflating turns. The planner already receives its live Flux-Lang
grammar and operation catalog; it must not also auto-inject a stale project skill that duplicates
them. Discovered skills stay inactive until the operator or embedding agent spec names them.

## Acceptance

- [x] Remove the project-default `.flux/skills/flux-flow` mirror and its duplicate sync test; keep
      the explicitly installable language skill/reference and website SSOT guards.
- [x] CLI `--skill <name>` explicitly selects repeatable skills from the discovered catalog; unknown
      names fail before a model call.
- [x] User text, names, descriptions, and `triggers` never activate a production skill implicitly.
- [x] SDK/AgentSpec skills remain an explicit programmatic allowlist; an empty spec stays empty.
- [x] Live same-prompt trace shows no unrelated skill activation and records the context/token delta.
- [x] No eval claim is made for automatic routing: it stays off unless a future controlled comparison
      demonstrates a quality gain worth its prompt and behavioral cost.
