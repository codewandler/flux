---
title: Build your first Flux app
description: A beginner tutorial from one adaptive CLI turn to an authored flow and reliable documentation-assistant journey.
---

# Build your first Flux app

This tutorial takes you from a prompt at the command line to a small application written in
Flux-Lang. You will watch typed intent and evidence gathering, write a reusable flow, then discover
why an important application rule belongs in a deterministic journey rather than a prompt.

Allow about **35–45 minutes**. You need basic terminal skills and a text editor, but you do not need
to know Rust, Flux-Lang, or any agent framework.

## What you will build

The finished app is a local assistant for a fictional product handbook. It starts in your terminal,
indexes two Markdown files, and requires every model-written answer to pass through an authored,
scoped retrieval journey.

Along the way you will use three levels of control:

1. **An adaptive request** — typed stages infer intent, gather evidence through exact native schemas,
   and capture effects in an approval batch.
2. **A flow** — you author repeatable computation directly in Flux-Lang.
3. **An app journey** — declarations connect permissions, agents, channels, datasources, triggers,
   decisions, and delivery.

All three use the same runtime and safety envelope:

```text
authorization → approval → guarded IO
```

## Before you start

Install flux and confirm that the binary runs:

```bash
flux --version
```

If that command is missing, follow [Getting started](./getting-started.md#install) first.

This tutorial uses a **real model**. The commands use the `sonnet` alias, which requires
`ANTHROPIC_API_KEY`. Check that flux can see your credentials:

```bash
flux auth status
```

You can substitute another configured model in every `-m sonnet` argument—for example
`claude/sonnet`, `codex/gpt-5.5`, or an OpenRouter model. See
[Providers and models](./agent/providers.md).

:::note
The offline `mock` provider checks installation and runtime wiring, but its canned replies cannot
complete the grounded question-answering exercises.
:::

## Create the tutorial workspace

```bash
mkdir flux-tutorial
cd flux-tutorial
mkdir docs
```

Create `docs/product.md`:

```markdown
# Northstar Notes

Northstar Notes is a shared note-taking service for small teams. Workspaces can export every note
as Markdown. Offline edits synchronize automatically when a device reconnects.
```

Create `docs/policies.md`:

```markdown
# Northstar policies

Support is available Monday through Friday, 09:00–17:00 Central European Time.

Customers can request a refund within 14 days of their first payment. A deleted workspace can be
recovered for 30 days; after that, deletion is permanent.
```

Your directory should now look like this:

```text
flux-tutorial/
└── docs/
    ├── policies.md
    └── product.md
```

Keep your terminal in `flux-tutorial` for the rest of the series.

## Next

Continue to [Run an adaptive agent safely](./tutorial/first-agent.md).
