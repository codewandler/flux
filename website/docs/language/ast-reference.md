---
title: AST reference
---

# AST reference

The JSON AST is the wire and storage form of Flux-Lang. It is useful for SDK consumers, tests, and
planner integrations. Humans should usually start with the text syntax.

## Top-level shape

```json
{
  "name": "optional-name",
  "params": [{"name": "ticket", "ty": "Ticket"}],
  "returns": "Result",
  "body": []
}
```

## Node categories

- **Primitive values**: `lit`, `var`, `thing`, `obj`, `list`.
- **Operations**: `call`, `bind`, `memo`, `pipe`.
- **Branching**: `when`, `unless`, `match`, `route`.
- **Iteration**: `repeat`, `each`, `loop`.
- **Concurrency**: `parallel`, `race`.
- **Failure and reliability**: `assert`, `try`, `retry`, `fallback`, `timeout`, `budget`, `confirm`.
- **Durability and side effects**: `scope`, `saga`, `once`, `checkpoint`.
- **Context and pure computation**: `ctx`, `ctx_append`, `fmt`, `jq`, `expr`, `parse`.
- **Cross-turn flow**: `await`.

The internal reference is generated from the `Node` enum doc comments and checked by repository tests.
For now, use it as the exhaustive node catalog:

https://github.com/codewandler/flux/blob/main/crates/flux-lang/docs/reference.md

Future versions of this site should generate a public node table from the same source to avoid drift.
