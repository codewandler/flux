---
title: Build your first Flux app
description: A beginner tutorial that starts with one guarded agent run and ends with a local, model-backed documentation assistant.
---

# Build your first Flux app

This tutorial takes you from a prompt at the command line to a small application written in
Flux-Lang. You will first let a model propose a plan, then write a reusable plan yourself, and
finally connect an agent to a local documentation collection and a terminal channel.

Allow about **35–45 minutes**. You need basic terminal skills and a text editor, but you do not need
to know Rust, Flux-Lang, or any agent framework.

## What you will build

The finished app is a local assistant for a fictional product handbook. It starts in your terminal,
indexes two Markdown files, and uses a real model to answer questions from those files.

Along the way you will learn the three ways work enters flux:

1. **A request** — the model compiles your words into a typed plan.
2. **A flow** — you write the plan directly in Flux-Lang.
3. **An app** — declarations connect agents, channels, datasources, triggers, and flows.

All three use the same runtime and the same safety envelope:

```text
authorization -> approval -> guarded IO
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

You can use another provider instead. Replace `sonnet` in every `-m sonnet` argument with your model
spec, such as `claude/sonnet` after `flux auth login claude` or `openai/gpt-5` with
`OPENAI_API_KEY`. See [Providers and models](./agent/providers.md) for every supported route.

:::note
The offline `mock` provider is useful for checking an installation, but its replies are canned. It
cannot complete this tutorial's grounded question-answering exercises.
:::

## Create the tutorial workspace

Make a clean directory so every file operation stays inside an obvious, disposable workspace:

```bash
mkdir flux-tutorial
cd flux-tutorial
mkdir docs
```

Create `docs/product.md` in your editor:

```markdown
# Northstar Notes

Northstar Notes is a shared note-taking service for small teams. Workspaces can export every note
as Markdown. Offline edits synchronize automatically when a device reconnects.
```

Create `docs/policies.md` beside it:

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

Continue to [Run an agent safely](./tutorial/first-agent.md) to turn a plain-language request into an
inspectable plan.
