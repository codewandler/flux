---
title: 2. Write a reusable flow
description: Turn the handbook task into a parameterized Flux-Lang flow with explicit data, context, and model boundaries.
---

# Write a reusable flow

An adaptive request is useful when the route to the result is open-ended. When you already know the
steps, write them as a Flux-Lang flow. Flux parses and executes that authored structure without
asking a model to invent it.

## Create `brief.flux`

Create a file named `brief.flux` at the root of `flux-tutorial`:

```flux
flow answer-handbook(question: String) -> String
  $product = read("docs/product.md")
  $policies = read("docs/policies.md")

  ctx $handbook
    purpose "answer a question from the Northstar handbook"
    budget 5000
    include $product, $policies

  $answer = ai.reason({ask: "Question: {question}\nAnswer only from the handbook. If the handbook does not say, say so.", ctx: $handbook})
  return $answer
```

Flux-Lang uses two-space indentation. The flow declares one required `String` input and a `String`
result. Its body then makes every important boundary explicit:

- `read(...)` is an operation; its result is stored in an immutable symbol such as `$product`.
- `ctx $handbook` selects exactly which values the reasoning model may see and caps their combined
  materialized size. The pack retains the selected values themselves, labelled by symbol name — not
  merely the names `$product` and `$policies`.
- `ai.reason(...)` is the one model-backed step. The model receives the question and the context
  pack, not ambient access to your filesystem.
- `return` makes the flow's result explicit.

## Run the flow

Pass the declared input and choose the same model as before:

```bash
flux flow run brief.flux --arg "question=How long can a deleted workspace be recovered?" -m sonnet
```

`ai.reason` is an explicit model/network effect, so flux asks you to approve that operation before
it sends the bounded context to the configured provider. Review the operation and provider, then
approve it. The two local `read` operations remain pre-authorized.

The answer should say **30 days**. Try the other policy fact:

```bash
flux flow run brief.flux --arg "question=When can a new customer request a refund?" -m sonnet
```

The answer should say **within 14 days of the first payment**.

## Authored structure, bounded judgment

The two model uses in this tutorial have different jobs:

| Command | What the model decides | What the runtime controls |
|---|---|---|
| `flux run "…"` | Intent, semantic operation choices, and wording | The authored outer loop, schemas, batches, approvals, and every effect |
| `flux flow run brief.flux` | Only the answer inside `ai.reason` | The authored flow, context budget, dispatch, and every effect |

The reads still cross the safety envelope. Writing them into a flow does not grant extra authority;
it only makes their order and data flow repeatable. The `ai.reason` approval is the same envelope
governing the one intentional model call.

If you omit `--arg question=...`, provide an unknown input, or pass a value of the wrong declared
type, flux rejects the invocation before the flow runs.

## Checkpoint

You now have a reusable typed workflow. The final lesson will keep the same handbook but replace the
one-shot command with a long-running application that receives questions as events.

Continue to [Make the docs assistant reliable](./first-app.md).
