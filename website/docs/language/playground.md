---
title: Flux-Lang workbench
description: "Edit with Flux LSP support and run declared documentation examples through guarded scratch execution."
---

# Flux-Lang workbench

Every Flux code block can expand into the same workbench used by `/console/`: a Monaco editor with
Flux syntax, live diagnostics, completion, hover, formatting, structural graph projection, input
editing, run output, cancellation, and effect-bound approval prompts.

The complete site and runtime ship inside the CLI:

```bash
flux docs
flux docs --model openai/gpt-5.6   # model-backed examples resolve this lazily
# docs:      http://127.0.0.1:8788/flux/
# workbench: http://127.0.0.1:8788/console/
```

Open the exact URL printed by the command. Its fragment contains a one-use launch secret; the page
exchanges that locally for an HttpOnly, SameSite cookie and clears the fragment. Runtime and LSP
routes reject other origins and sessions.

## What can run

Run appears only on examples with a server-known fixture. The examples page currently enables
`summarize-readme`, `latest-release`, `cached-page`, `wait-for-artifact`, and `rust-files`. The
first-app tutorial also exposes Part A and Part B as persistent app sessions with the handbook files
from the earlier lessons. App messages reuse the same session; editing the program or switching
variants starts a fresh one.

Other Flux blocks still get the editor and checker. They remain non-runnable when they rely on an
undefined operation, a real Git checkout, an intentionally abridged production flow, or an external
integration the tutorial cannot safely synthesize.

## Safety boundary

Each runnable block gets a private scratch project retained for its browser session. Its manifest
limits both fixture files and reachable operations; the runtime performs a graph/risk preflight and
then dispatches every operation through ordinary authorization, approval, redaction, and guarded IO.
Approval decisions include the pending effect fingerprint, so an edit or different call cannot reuse
one. Shell, plugins, the directory where you launched Flux, sub-agents, host secrets, and private
network access are absent.

`flux docs --bind 0.0.0.0:8788` deliberately changes posture. A non-loopback listener does not
construct or mount the executor, scratch, approval, app, or LSP services; it serves static docs and
structural projection only. The hosted public website similarly provides syntax-aware editing but
has no backend and cannot execute.
