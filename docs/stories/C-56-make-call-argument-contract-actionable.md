---
id: C-56
title: Make the model call-argument contract actionable
pillar: Core
status: done
note: "Matched Codex E2E emitted the same invalid four-position grep call in four consecutive runs; each repair cost 6–10 seconds and about 17k input tokens."
---

# Make the model call-argument contract actionable

## Goal

Put Flux-Lang's named-object convention where a model constructing `Node::Call.args` can see it,
and make rejection feedback show the actual AST repair shape plus the operation's accepted names and
types.

## Acceptance

- [x] The model-facing JSON schema describes the one-object convention directly on `Call.args`.
- [x] A positional-call diagnostic shows the `args: [{"kind":"obj", ...}]` AST shape.
- [x] The same diagnostic lists the actual operation's required/optional parameters and known types.
- [x] Existing named-object enforcement and runtime behavior remain unchanged.
- [x] The previously failing live `grep` prompt is rerun and its repair count recorded honestly.
