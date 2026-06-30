---
title: Overview
---

# Flux-Lang overview

Flux-Lang is the language pillar of flux: the typed plan format a model emits and a deterministic
runtime executes.

The model writes a readable, analyzer-validated plan over symbols. The runtime resolves those symbols
to immutable values and dispatches every real-world effect through policy, approval, and guarded IO.

## What Flux-Lang is

- An executable AST with a human-readable text form.
- A structured workflow language for agent work.
- A way to audit and replay plans.
- A boundary between model planning and runtime execution.

## What Flux-Lang is not

- Not a shell script.
- Not a ReAct transcript where the model schedules each tool live.
- Not a general-purpose language.
- Not a behavior tree or actor runtime.

## The two forms

- **Text form**: `.flux`, the public human-readable syntax.
- **JSON AST**: the wire/storage form used by planners, SDKs, and tests.

Both forms describe the same `DraftAst`. The text form is what most humans should read first.
