---
id: D-217
title: "`EventDecl::when` const-equality matching — the narrowed event a connector arm currently refuses"
pillar: Agent
status: ready
priority: 11
epic: connector-channels
areas: [flux-channels]
note: "filed from D-216's implementation, not from planning — `when` was named as v1 scope in D-216's Notes and the epic design's item 5, but D-216 shipped a load REFUSAL instead of matching, deliberately and fail-closed; this is the story that decides whether matching arrives or the refusal becomes the permanent answer"
---

# `EventDecl::when` const-equality matching

## Goal

D-216 landed the `connector` channel arm. It models `ManifestEvent::when` **in order to refuse it**,
not to match it — a manifest that narrows a coarse vendor event (GitHub's `issues` narrowed to
`issues.opened`) is a load error naming the cost.

That was the right call at the time and the reasoning should not be lost: before D-216, such a
manifest **loaded and then silently no-opped on every delivery**. The discriminator carries the
coarse value (`issues`), which is not a member of the closed event set once the manifest has narrowed
it — so the channel bound, the port opened, deliveries arrived, and nothing ever fired. A refusal
that names the cost is strictly better than a channel that looks healthy and does nothing.

But refusing is not what the epic design asked for. The design's "what flux must add" item 5 and
D-216's own `## Notes` both name const-equality matching as v1 scope. D-216's implementor deferred it
on two grounds, both of which this story must re-test rather than inherit:

1. **No shipped manifest emits a `when`** — and, for GitHub, no `[[events]]` at all. If that is still
   true, the refusal costs nothing today and this story is cheap to defer again.
2. **The correspondence is underspecified.** Given a discriminator value and a narrowed event name,
   the design does not say how one maps to the other. Implementing it would have been speculation,
   and speculation in a load path that decides whether a webhook fires is the wrong place for it.

## What the D-216 review already settled

Both deferral grounds were checked against the producing repository during D-216's review, and the
answer is stronger than "no manifest happens to emit a `when`":

> **Two event fields deliberately stop at the catalogue.** `schema` and `when` are vendor JSON
> Schemas, and TOML has no `null`.
> — `flux-connectors/crates/connector-cli/src/seam.rs:364`

`when` is **never emitted into a manifest by design**, and a grep over all 96 shipped manifests
returns zero `when` keys. So the epic design's item 5 — "`EventDecl::when` matched by `const`
equality" — is not implementable from the manifest at all: there is nothing on the wire to read.
D-216's load refusal is therefore the correct permanent answer, not a deferral, and this story is
mostly about recording that and closing the design's stale promise.

What remains genuinely open is upstream: if narrowed events are wanted, `connector-cli` has to emit
something for flux to match on. That is a flux-connectors change, not a flux one.

## Acceptance

- [ ] Confirm the above still holds at the time of pickup (`seam.rs:364`, and zero `when` keys across
      the pack). If `connector-cli` has started emitting `when`, everything below changes and this
      story's priority is wrong.
- [ ] **Specify the correspondence before implementing it.** Write down, in the epic design, how a
      discriminator value maps to a narrowed event name — including the coarse-value case that
      produced the silent no-op. A story that implements this without the design saying what it means
      has just moved the speculation into code.
- [ ] Const-equality matching is implemented, and a manifest narrowing a coarse event **fires for the
      narrowed case and does not fire for the others** — proven by a delivery-level test, not by a
      load-time one. The bug class here is a channel that loads and does nothing, so a test that only
      proves the manifest loads would reproduce exactly the defect D-216 refused.
- [ ] **Failing-first**: the delivery test reds before the matching implementation lands.
- [ ] D-216's refusal is removed only in the cases now genuinely handled. Anything still
      unmatchable stays a load error — never a silent no-op, at any point in this story.
- [ ] Full gate green in both workspaces.

## Notes

- Filed from D-216's implementation report (2026-07-31), where the implementor wrote that this
  "deserves a follow-up story, not something I consider done."
- Related: [D-216](D-216-connector-channel-arm.md) built the arm and the refusal.
- A legitimate outcome of this story is **"the refusal is the permanent answer"** — if the pack never
  narrows events, deleting the design's item 5 and keeping the load error is a real closure, not a
  cop-out. Say so explicitly if that is what the evidence supports.
- Separate but adjacent, and worth knowing before you touch this arm: `hmac` bindings are also
  refused today, pending C-291/C-292, so the arm currently serves only
  `verification.kind = "none"` webhooks. That is a different gap with a different owner — don't
  conflate the two in one change.
